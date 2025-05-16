// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::{HashMap, HashSet},
    fs,
    fs::File,
    io::Write,
    net::IpAddr,
    num::NonZeroUsize,
    path::PathBuf,
    str::FromStr,
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Error};
use hickory_resolver::{
    config::{ResolverConfig, ResolverOpts},
    TokioAsyncResolver,
};
use itertools::Itertools;
use libc;
use libp2p::{
    autonat::{self, NatStatus, OutboundProbeEvent},
    dcutr,
    futures::StreamExt,
    gossipsub::{self, IdentTopic, Message, MessageAcceptance, PublishError},
    identify::{self, Info},
    identity::Keypair,
    mdns::{self, tokio::Tokio},
    multiaddr::Protocol,
    ping,
    relay,
    request_response::{self, cbor, OutboundFailure, OutboundRequestId, ResponseChannel},
    swarm::{behaviour::toggle::Toggle, dial_opts::DialOpts, DialError, NetworkBehaviour, SwarmEvent},
    Multiaddr,
    PeerId,
    Swarm,
};
use log::{debug, error, info, trace, warn};
use lru::LruCache;
use rand::{seq::SliceRandom, thread_rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, to_writer_pretty};
use tari_common::configuration::Network;
use tari_common_types::types::FixedHash;
use tari_core::proof_of_work::PowAlgorithm;
use tari_shutdown::Shutdown;
use tari_utilities::{epoch_time::EpochTime, hex::Hex};
use tokio::{
    select,
    sync::{
        mpsc::{self, UnboundedReceiver},
        oneshot,
        OwnedSemaphorePermit,
        RwLock,
        Semaphore,
    },
    time::MissedTickBehavior,
};

use super::messages::{CatchUpSyncRequest, CatchUpSyncResponse, MetaDataRequest, MetaDataResponse, NotifyNewTipBlock};
use crate::{
    anyhow_error,
    server::{
        config,
        diagnostics::{DiagnosticPeerInfo, DiagnosticsBroadcastClient, DiagnosticsReceiverClient},
        http::stats_collector::StatsBroadcastClient,
        p2p::{
            messages::{
                self,
                DirectPeerInfoRequest,
                DirectPeerInfoResponse,
                PeerInfo,
                SyncMissingBlocksRequest,
                SyncMissingBlocksResponse,
            },
            peer_store::{AddPeerStatus, PeerStore, PeerStoreRecord},
            relay_store::RelayStore,
        },
        PeerType,
        PROTOCOL_VERSION,
    },
    sharechain::{
        p2block::{P2Block, VerifiedStatus, CURRENT_CHAIN_ID},
        ShareChain,
    },
};

const PEER_INFO_TOPIC: &str = "peer_info";
const BLOCK_NOTIFY_TOPIC: &str = "block_notify";
const BLOCK_NOTIFY_RX_TOPIC: &str = "block_notify_rx";
const BLOCK_NOTIFY_SHA3X_TOPIC: &str = "block_notify_sha3x";
pub(crate) const SHARE_CHAIN_SYNC_REQ_RESP_PROTOCOL: &str = "/share_chain_sync/5";
pub(crate) const DIRECT_PEER_EXCHANGE_REQ_RESP_PROTOCOL: &str = "/tari_direct_peer_info/5";
pub(crate) const META_DATA_EXCHANGE_REQ_RESP_PROTOCOL: &str = "/tari_meta_data_info/5";
pub(crate) const CATCH_UP_SYNC_REQUEST_RESPONSE_PROTOCOL: &str = "/catch_up_sync/5";
const LOG_TARGET: &str = "tari::p2pool::server::p2p";
const SYNC_REQUEST_LOG_TARGET: &str = "sync_request";
const MESSAGE_LOGGING_LOG_TARGET: &str = "tari::p2pool::message_logging";
const PEER_INFO_LOGGING_LOG_TARGET: &str = "tari::p2pool::topics::peer_info";
const NEW_TIP_NOTIFY_LOGGING_LOG_TARGET: &str = "tari::p2pool::topics::new_tip_notify";
pub const STABLE_PRIVATE_KEY_FILE: &str = "p2pool_private.key";
// The amount of time between catch up syncs to the same peer
const MAX_ACCEPTABLE_P2P_MESSAGE_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT: Duration = Duration::from_millis(100);
const CATCH_UP_SYNC_BLOCKS_IN_I_HAVE: usize = 100;
const MAX_CATCH_UP_ATTEMPTS: u64 = 500;
const MAX_CATCH_UP_BLOCKS_TO_RETURN: usize = 5;
// Time to start up and catch up before we start processing new tip messages
const NUM_PEERS_TO_SYNC_PER_ALGO: usize = 32;
const NUM_PEERS_INITIAL_SYNC: usize = 100;
const NUM_PEERS_TO_META_DATA_EXCHANGE: usize = 8;
const NUM_PEERS_TO_PEER_INFO_EXCHANGE: usize = 8;
const MAX_OUTBOUND_SQUAD_PEERS: usize = 8;
const MAX_OUTBOUND_NON_SQUAD_PEERS: usize = 2;
const CONNECTION_CHURN_DELAY_SECS: u64 = 60 * 20; // 20 minutes

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct Config {
    pub external_addr: Option<String>,
    pub seed_peers: Vec<String>,
    pub peer_info_publish_interval: Duration,
    pub stable_peer: bool,
    pub private_key_folder: PathBuf,
    pub private_key: Option<Keypair>,
    pub mdns_enabled: bool,
    pub relay_server_disabled: bool,
    pub squad_override: Option<String>,
    pub squad_prefix: String,
    pub num_squads: usize,
    pub user_agent: String,
    pub grey_list_clear_interval: Duration,
    pub black_list_clear_interval: Duration,
    pub meta_data_exchange_interval: Duration,
    pub peer_exchange_interval: Duration,
    pub is_seed_peer: bool,
    pub debug_print_chain: bool,
    pub sync_job_enabled: bool,
    pub sha3x_enabled: bool,
    pub randomx_enabled: bool,
    pub num_concurrent_syncs: usize,
    pub num_sync_tips_to_keep: usize,
    pub max_missing_blocks_sync_depth: usize,
    pub diagnostic_mode: bool,
    pub diagnostic_mode_file_path: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            external_addr: None,
            seed_peers: vec![],
            peer_info_publish_interval: Duration::from_secs(60 * 15),
            stable_peer: true,
            private_key_folder: PathBuf::from("."),
            private_key: None,
            mdns_enabled: false,
            relay_server_disabled: false,
            squad_prefix: "default".to_string(),
            squad_override: None,
            num_squads: 1,
            user_agent: "tari-p2pool".to_string(),
            grey_list_clear_interval: Duration::from_secs(60 * 2),
            black_list_clear_interval: Duration::from_secs(60 * 60),
            meta_data_exchange_interval: Duration::from_secs(5),
            peer_exchange_interval: Duration::from_secs(60 * 60),
            is_seed_peer: false,
            debug_print_chain: false,
            sync_job_enabled: true,
            sha3x_enabled: true,
            randomx_enabled: true,
            num_concurrent_syncs: 1,
            num_sync_tips_to_keep: 10,
            max_missing_blocks_sync_depth: 8,
            diagnostic_mode: false,
            diagnostic_mode_file_path: None,
        }
    }
}

struct SyncMissingBlocks {
    pub algo: PowAlgorithm,
    pub peer: PeerId,
    pub missing_parents: Vec<(u64, FixedHash)>,
    pub is_from_new_block_notify: bool,
    pub depth: usize,
}

struct PerformCatchUpSync {
    pub algo: PowAlgorithm,
    pub peer: PeerId,
    pub last_block_from_them: Option<(u64, FixedHash)>,
    pub their_height: u64,
    // pub their_pow: u128,
    pub permit: Option<OwnedSemaphorePermit>,
}

#[derive(NetworkBehaviour)]
pub struct ServerNetworkBehaviour {
    // This must be the first behaviour otherwise you end up with a debug-assert failing in
    // req-resp
    // pub connection_limits: connection_limits::Behaviour,
    pub mdns: Toggle<mdns::Behaviour<Tokio>>,
    pub gossipsub: gossipsub::Behaviour,
    pub share_chain_sync: cbor::Behaviour<SyncMissingBlocksRequest, Result<SyncMissingBlocksResponse, String>>,
    pub direct_peer_exchange: cbor::Behaviour<DirectPeerInfoRequest, Result<DirectPeerInfoResponse, String>>,
    pub meta_data_exchange: cbor::Behaviour<MetaDataRequest, Result<MetaDataResponse, String>>,
    pub catch_up_sync: cbor::Behaviour<CatchUpSyncRequest, Result<CatchUpSyncResponse, String>>,
    pub identify: identify::Behaviour,
    pub relay_server: Toggle<relay::Behaviour>,
    pub relay_client: relay::client::Behaviour,
    pub dcutr: dcutr::Behaviour,
    pub autonat: autonat::Behaviour,
    pub ping: ping::Behaviour,
}

pub enum P2pServiceQuery {
    ConnectionInfo(oneshot::Sender<ConnectionInfo>),
    GetPeers(oneshot::Sender<(Vec<ConnectedPeerInfo>, Vec<ConnectedPeerInfo>)>),
    GetConnections(oneshot::Sender<Vec<ConnectedPeerInfo>>),
    GetChain {
        pow_algo: PowAlgorithm,
        height: u64,
        count: usize,
        response: oneshot::Sender<Vec<Arc<P2Block>>>,
    },
}

#[derive(Serialize, Clone)]
pub(crate) struct ConnectedPeerInfo {
    pub peer_id: String,
    pub peer_info: Option<PeerInfo>,
    pub last_grey_list_reason: Option<String>,
    pub last_ping: Option<EpochTime>, /* peer_addresses: Vec<Multiaddr>,
                                       * is_pending: bol, */
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConnectionInfo {
    pub listener_addresses: Vec<Multiaddr>,
    pub connected_peers: usize,
    pub network_info: NetworkInfo,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct NetworkInfo {
    /// The total number of connected peers.
    pub num_peers: usize,
    /// Counters of ongoing network connections.
    pub connection_counters: ConnectionCounters,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ConnectionCounters {
    /// The current number of incoming connections.
    pub pending_incoming: u32,
    /// The current number of outgoing connections.
    pub pending_outgoing: u32,
    /// The current number of established inbound connections.
    pub established_incoming: u32,
    /// The current number of established outbound connections.
    pub established_outgoing: u32,
}

enum InnerRequest {
    DoSyncMissingBlocks(SyncMissingBlocks),
    PerformCatchUpSync(PerformCatchUpSync),
    AddSyncedBlock {
        algo: PowAlgorithm,
        block: Arc<P2Block>,
        source_peer: PeerId,
        depth: usize,
    },
}

// Helper function to check for EAFNOSUPPORT
// This ideally should be more robust, perhaps by checking the specific TransportError variant
// or the inner std::io::Error's kind or raw_os_error.
fn is_eafnosupport(e: &libp2p::TransportError<std::io::Error>) -> bool {
    if let libp2p::TransportError::Other(io_err) = e {
        // Since 'e' is &libp2p::TransportError<std::io::Error>,
        // 'io_err' here will be a &std::io::Error.
        if let Some(raw_os_err) = io_err.raw_os_error() {
            return raw_os_err == libc::EAFNOSUPPORT;
        }
    }
    // If 'e' is libp2p::TransportError::MultiaddrNotSupported(_), or if Other(io_err)
    // doesn't contain a raw_os_error or the error code doesn't match,
    // we correctly return false.
    false
}

/// Service is the implementation that holds every peer-to-peer related logic
/// that makes sure that all the communications, syncing, broadcasting etc... are done.
// Allow type complexity for now. It is caused by the hashmap of arc of rwlock of lru cache
// it should be removed in future.
#[allow(clippy::type_complexity)]
pub struct Service<S>
where S: ShareChain
{
    pub squad: String,
    swarm: Swarm<ServerNetworkBehaviour>,
    port: u16,
    share_chain_sha3x: Arc<S>,
    share_chain_random_x: Arc<S>,
    network_peer_store: Arc<RwLock<PeerStore>>,
    config: Config,
    shutdown: Shutdown,
    query_rx: mpsc::Receiver<P2pServiceQuery>,
    client_broadcast_block_rx: UnboundedReceiver<Arc<P2Block>>,
    inner_request_tx: mpsc::UnboundedSender<InnerRequest>,
    inner_request_rx: mpsc::UnboundedReceiver<InnerRequest>,

    relay_store: Arc<RwLock<RelayStore>>,
    are_we_synced_with_randomx_p2pool: Arc<AtomicBool>,
    are_we_synced_with_sha3x_p2pool: Arc<AtomicBool>,
    stats_broadcast_client: StatsBroadcastClient,
    diagnostics_broadcast_client: Option<DiagnosticsBroadcastClient>,
    diagnostics_receiver_client: Option<DiagnosticsReceiverClient>,
    randomx_sync_semaphore: Arc<Semaphore>,
    sha3x_sync_semaphore: Arc<Semaphore>,
    randomx_last_sync_requested_block: Option<(u64, FixedHash)>,
    sha3x_last_sync_requested_block: Option<(u64, FixedHash)>,
    randomx_in_progress_syncs: HashMap<PeerId, (OutboundRequestId, OwnedSemaphorePermit)>,
    sha3x_in_progress_syncs: HashMap<PeerId, (OutboundRequestId, OwnedSemaphorePermit)>,
    last_failed_randomx_peer: Option<PeerId>,
    last_failed_sha3x_peer: Option<PeerId>,
    recent_synced_tips: HashMap<PowAlgorithm, Arc<RwLock<LruCache<PeerId, (u64, FixedHash)>>>>,
    missing_blocks_sync_request_depth: HashMap<OutboundRequestId, usize>,
    share_window: u64,
    dialed_seed_peers: HashSet<PeerId>,
    is_private_nat: bool,
}

impl<S> Service<S>
where S: ShareChain
{
    /// Constructs a new Service from the provided config.
    /// It also instantiates libp2p swarm inside.
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        config: &config::Config,
        share_chain_sha3x: Arc<S>,
        share_chain_random_x: Arc<S>,
        shutdown: Shutdown,
        are_we_synced_with_randomx_p2pool: Arc<AtomicBool>,
        are_we_synced_with_sha3x_p2pool: Arc<AtomicBool>,
        stats_broadcast_client: StatsBroadcastClient,
        diagnostics_broadcast_client: Option<DiagnosticsBroadcastClient>,
        diagnostics_receiver_client: Option<DiagnosticsReceiverClient>,
        share_window: u64,
        swarm: Swarm<ServerNetworkBehaviour>,
        squad: String,
        broadcast_block_rx: UnboundedReceiver<Arc<P2Block>>,
        query_rx: mpsc::Receiver<P2pServiceQuery>,
    ) -> Result<Self, Error> {
        let _res = stats_broadcast_client.send_info_changed(squad.clone(), *swarm.local_peer_id());

        let network_peer_store = PeerStore::new(stats_broadcast_client.clone(), squad.clone());
        // client related channels
        // let (broadcast_block_tx, broadcast_block_rx) = broadcast::channel::<NotifyNewTipBlock>(100);
        // let (_share_chain_sync_tx, _share_chain_sync_rx) = broadcast::channel::<LocalShareChainSyncRequest>(1000);
        // let (snooze_block_tx, snooze_block_rx) = mpsc::channel::<(usize, P2Block)>(1000);
        // let (query_tx, query_rx) = mpsc::channel(100);
        // This should not be unbounded but we need to find out what is using up all the permits
        let (inner_request_tx, inner_request_rx) = mpsc::unbounded_channel();

        let mut recent_synced_tips = HashMap::new();
        recent_synced_tips.insert(
            PowAlgorithm::RandomX,
            Arc::new(RwLock::new(LruCache::new(
                NonZeroUsize::new(config.p2p_service.num_sync_tips_to_keep).unwrap(),
            ))),
        );
        recent_synced_tips.insert(
            PowAlgorithm::Sha3x,
            Arc::new(RwLock::new(LruCache::new(
                NonZeroUsize::new(config.p2p_service.num_sync_tips_to_keep).unwrap(),
            ))),
        );
        Ok(Self {
            squad,
            swarm,
            port: config.p2p_port,
            share_chain_sha3x,
            share_chain_random_x,
            network_peer_store: Arc::new(RwLock::new(network_peer_store)),
            config: config.p2p_service.clone(),
            shutdown,
            // client_broadcast_block_tx: broadcast_block_tx,
            client_broadcast_block_rx: broadcast_block_rx,
            inner_request_tx,
            inner_request_rx,
            query_rx,
            relay_store: Arc::new(RwLock::new(RelayStore::default())),
            are_we_synced_with_randomx_p2pool,
            are_we_synced_with_sha3x_p2pool,
            stats_broadcast_client,
            diagnostics_broadcast_client,
            diagnostics_receiver_client,
            randomx_sync_semaphore: Arc::new(Semaphore::new(config.p2p_service.num_concurrent_syncs)),
            sha3x_sync_semaphore: Arc::new(Semaphore::new(config.p2p_service.num_concurrent_syncs)),
            randomx_last_sync_requested_block: None,
            sha3x_last_sync_requested_block: None,
            randomx_in_progress_syncs: HashMap::new(),
            sha3x_in_progress_syncs: HashMap::new(),
            last_failed_sha3x_peer: None,
            last_failed_randomx_peer: None,
            recent_synced_tips,
            missing_blocks_sync_request_depth: HashMap::new(),
            share_window,
            dialed_seed_peers: HashSet::new(),
            is_private_nat: false,
        })
    }

    /// Broadcasting current peer's information ([`PeerInfo`]) to other peers in the network
    /// by sending this data to [`PEER_INFO_TOPIC`] gossipsub topic.
    async fn broadcast_peer_info(&mut self) -> Result<(), Error> {
        let public_addresses: Vec<Multiaddr> = self.swarm.external_addresses().cloned().collect();
        if public_addresses.is_empty() {
            warn!("No public addresses found, skipping peer info broadcast");
            return Ok(());
        }
        // get peer info
        let peer_info_squad_raw: Vec<u8> = self.create_peer_info(public_addresses).await?.try_into()?;

        let topic = Self::network_topic(PEER_INFO_TOPIC);
        // broadcast peer info to squad
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(IdentTopic::new(topic), peer_info_squad_raw)?;

        Ok(())
    }

    pub fn local_peer_id(&self) -> PeerId {
        *self.swarm.local_peer_id()
    }

    async fn create_peer_info(&mut self, public_addresses: Vec<Multiaddr>) -> Result<PeerInfo, Error> {
        let share_chain_sha3x = self.share_chain_sha3x.clone();
        let share_chain_random_x = self.share_chain_random_x.clone();
        let current_height_sha3x = share_chain_sha3x.tip_height().await?;
        let current_height_random_x = share_chain_random_x.tip_height().await?;
        let current_pow_sha3x = share_chain_sha3x.chain_pow().await.as_u128();
        let current_pow_random_x = share_chain_random_x.chain_pow().await.as_u128();
        let peer_info_squad_raw = PeerInfo::new(
            *self.swarm.local_peer_id(),
            current_height_sha3x,
            current_height_random_x,
            current_pow_sha3x,
            current_pow_random_x,
            self.squad.clone(),
            public_addresses,
            Some(self.config.user_agent.clone()),
        );
        Ok(peer_info_squad_raw)
    }

    /// Broadcasting a new mined [`Block`] to the network (assume it is already validated with the network).
    async fn broadcast_block(&mut self, block: Arc<P2Block>) {
        // if self.sync_in_progress.load(Ordering::SeqCst) {
        //     return;
        // }

        let algo = block.algo();
        let version = block.version;
        let squad = if version <= 35 {
            self.squad_topic(BLOCK_NOTIFY_TOPIC)
        } else {
            match algo {
                PowAlgorithm::RandomX => self.squad_topic(BLOCK_NOTIFY_RX_TOPIC),
                PowAlgorithm::Sha3x => self.squad_topic(BLOCK_NOTIFY_SHA3X_TOPIC),
            }
        };
        let message = NotifyNewTipBlock::new(self.local_peer_id(), vec![Arc::unwrap_or_clone(block)]);
        let block_raw_result: Result<Vec<u8>, Error> = message.try_into();
        match block_raw_result {
            Ok(block_raw) => {
                match self
                    .swarm
                    .behaviour_mut()
                    .gossipsub
                    .publish(IdentTopic::new(squad), block_raw.clone())
                {
                    Ok(_) => {},
                    Err(error) => {
                        if matches!(error, PublishError::InsufficientPeers) {
                            debug!(target: LOG_TARGET, "No peers to broadcast new block");
                        } else {
                            error!(target: LOG_TARGET, "Failed to broadcast new block: {error}");
                        }
                    },
                }
            },
            Err(error) => {
                error!(target: LOG_TARGET, "Failed to convert block to bytes: {error}")
            },
        }
    }

    /// Generates a gossip sub topic name based on the current Tari network to avoid mixing up
    /// blocks and peers with different Tari networks.
    fn network_topic(topic: &str) -> String {
        let network = Network::get_current_or_user_setting_or_default().as_key_str();
        let chain_id = CURRENT_CHAIN_ID.clone();
        format!("{network}_{chain_id}_{topic}_{PROTOCOL_VERSION}")
    }

    /// Generates a gossip sub topic name based on the current Tari network to avoid mixing up
    /// blocks and peers with different Tari networks and the given squad name.
    fn squad_topic(&self, topic: &str) -> String {
        let network = Network::get_current_or_user_setting_or_default().as_key_str();
        let chain_id = CURRENT_CHAIN_ID.clone();
        let squad = &self.squad;
        format!("{network}_{chain_id}_{squad}_{topic}_{PROTOCOL_VERSION}")
    }

    /// Subscribing to a gossipsub topic.
    fn subscribe(&mut self, topic: &str, squad: bool) {
        let topic = if squad {
            self.squad_topic(topic)
        } else {
            Self::network_topic(topic)
        };
        info!(target: LOG_TARGET, "Subscribing to topic: {topic}");
        self.swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&IdentTopic::new(topic))
            .expect("must be subscribed to topic");
    }

    /// Subscribes to all topics we need.
    async fn subscribe_to_topics(&mut self) {
        self.subscribe(PEER_INFO_TOPIC, false);
        // if self.config.is_seed_peer {
        // return;
        // }
        if !self.config.diagnostic_mode {
            // Legacy. To be removed in the future.
            self.subscribe(BLOCK_NOTIFY_TOPIC, true);
            if self.config.randomx_enabled {
                self.subscribe(BLOCK_NOTIFY_RX_TOPIC, true);
            }
            if self.config.sha3x_enabled {
                self.subscribe(BLOCK_NOTIFY_SHA3X_TOPIC, true);
            }
        }
    }

    /// Main method to handle any message comes from gossipsub.
    #[allow(clippy::too_many_lines)]
    async fn handle_new_gossipsub_message(
        &mut self,
        message: Message,
        propagation_source: PeerId,
    ) -> Result<MessageAcceptance, Error> {
        debug!(target: MESSAGE_LOGGING_LOG_TARGET, "New gossipsub message: {message:?}");
        let _unused = self.stats_broadcast_client.send_gossipsub_message_received();
        let source_peer = message.source;
        if let Some(source_peer) = source_peer {
            let topic = message.topic.to_string();
            match topic {
                topic if topic == Self::network_topic(PEER_INFO_TOPIC) => {
                    match messages::PeerInfo::try_from(message) {
                        Ok(payload) => {
                            debug!(target: LOG_TARGET, "[PEER_INFO_TOPIC] New peer info: {source_peer:?} -> {payload:?}");
                            if payload.version != PROTOCOL_VERSION {
                                warn!(target: LOG_TARGET, "Peer {} has an outdated version, skipping", source_peer);
                                return Ok(MessageAcceptance::Reject);
                            }

                            // 60 seconds. TODO: make config
                            if payload.timestamp < EpochTime::now().as_u64().saturating_sub(60) {
                                warn!(
                                    target: LOG_TARGET,
                                    "Peer {} sent a peer info message that is too old, skipping",
                                    source_peer
                                );
                                // TODO: should be punish
                                return Ok(MessageAcceptance::Ignore);
                            }

                            self.add_peer(payload, source_peer).await;
                            return Ok(MessageAcceptance::Accept);
                        },
                        Err(error) => {
                            warn!(target: LOG_TARGET, "Can't deserialize peer info payload: {:?}", error);
                            return Ok(MessageAcceptance::Reject);
                        },
                    }
                },
                // TODO: send a signature that proves that the actual block was coming from this peer
                // TODO: (sender peer's wallet address should be included always in the conibases with a fixed percent
                // (like 20%))
                topic
                    if topic == self.squad_topic(BLOCK_NOTIFY_TOPIC) ||
                        topic == self.squad_topic(BLOCK_NOTIFY_RX_TOPIC) ||
                        topic == self.squad_topic(BLOCK_NOTIFY_SHA3X_TOPIC) =>
                {
                    // if self.sync_in_progress.load(Ordering::SeqCst) {
                    //     return;
                    // }
                    match NotifyNewTipBlock::try_from(message) {
                        Ok(payload) => {
                            let algo = payload.algo();
                            // info!(target: LOG_TARGET, squad = &self.config.squad; "New new tip notify: {}", payload);
                            if payload.version != PROTOCOL_VERSION {
                                warn!(target: LOG_TARGET, "Peer {} has an outdated version, skipping", source_peer);
                                return Ok(MessageAcceptance::Reject);
                            }
                            // lets check age
                            // if this timestamp is older than 60 seconds, we reject it
                            if payload.timestamp < EpochTime::now().as_u64().saturating_sub(60) {
                                warn!(
                                    target: LOG_TARGET,
                                    "Peer {} sent a notify message that is too old, skipping",
                                    source_peer
                                );
                                return Ok(MessageAcceptance::Ignore);
                            }
                            if algo == PowAlgorithm::RandomX && !self.config.randomx_enabled {
                                warn!(
                                    target: LOG_TARGET,
                                    "Peer {} sent a RandomX block but RandomX is disabled, skipping",
                                    source_peer
                                );
                                return Ok(MessageAcceptance::Ignore);
                            }
                            if algo == PowAlgorithm::Sha3x && !self.config.sha3x_enabled {
                                warn!(
                                    target: LOG_TARGET,
                                    "Peer {} sent a Sha3x block but Sha3x is disabled, skipping",
                                    source_peer
                                );
                                return Ok(MessageAcceptance::Ignore);
                            }
                            let payload = Arc::new(payload);
                            let message_peer = payload.peer_id();
                            let message_age = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_else(|_| Duration::from_secs(0))
                                .checked_sub(Duration::from_secs(payload.timestamp))
                                .unwrap_or_else(|| Duration::from_secs(0));
                            info!(
                                target: NEW_TIP_NOTIFY_LOGGING_LOG_TARGET,
                                "[SQUAD_NEW_BLOCK_TOPIC] New block from gossip: {source_peer:?} via \
                                {propagation_source}-> [{}] Blocks: {} Age:{}",
                                algo,
                                payload.new_blocks.iter().map(|b| format!("{}:{}",
                                b.height,
                                &b.hash.to_hex()[0..8])).collect::<Vec<String>>().join(","),
                                humantime::format_duration(message_age)
                            );

                            // verify payload
                            if payload.new_blocks.is_empty() {
                                info!(target: LOG_TARGET, "Peer {} sent notify new tip with no blocks.", message_peer);
                                return Ok(MessageAcceptance::Reject);
                            }

                            // if self.config.is_seed_peer {
                            //     return Ok(MessageAcceptance::Accept);
                            // }

                            info!(
                                target: LOG_TARGET,
                                "[{:?}]🆕 New block from broadcast: {:?}",
                                &algo,
                                &payload.new_blocks.iter().map(|b| b.height.to_string()).collect::<Vec<String>>()
                            );
                            // info!(target: LOG_TARGET, squad = &self.config.squad; "🆕 New blocks from broadcast:
                            // {:?}", &payload.new_blocks.iter().map(|b| b.hash.to_hex()).collect::<Vec<String>>());
                            let share_chain = match algo {
                                PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
                                PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
                            };

                            let our_tip = share_chain.tip_height().await.unwrap_or(0);
                            let our_pow = share_chain.get_total_chain_pow().await;
                            if payload.total_proof_of_work() < our_pow &&
                                payload
                                    .new_blocks
                                    .iter()
                                    .map(|payload| payload.height)
                                    .max()
                                    .unwrap_or(0) <=
                                    our_tip.saturating_sub(4)
                            {
                                info!(
                                    target: LOG_TARGET,
                                    "Peer {} sent a block that is not better than ours, skipping",
                                    message_peer
                                );
                                return Ok(MessageAcceptance::Ignore);
                            }

                            // If the tip is much higher than ours, it may be either a fake block or on a part of the
                            // network that we can't get to
                            if payload.new_blocks.iter().map(|b| b.height).max().unwrap_or(0) >
                                our_tip.saturating_add(10)
                            {
                                info!(
                                    target: LOG_TARGET,
                                    "Peer {} sent a block that is much higher than ours, skipping",
                                    message_peer
                                );
                                // Return accept so that the rest of the network can get it
                                return Ok(MessageAcceptance::Accept);
                            }

                            let max_payload_height = payload
                                .new_blocks
                                .iter()
                                .map(|payload| payload.height)
                                .max()
                                .unwrap_or(0);

                            let mut blocks: Vec<P2Block> = payload.new_blocks.to_vec();
                            for block in &mut blocks {
                                block.verified = VerifiedStatus::new();
                            }
                            let blocks = blocks.into_iter().map(Arc::new).collect::<Vec<_>>();
                            match share_chain.add_synced_blocks(blocks).await {
                                Ok(new_tip) => {
                                    info!(
                                        target: LOG_TARGET,
                                        "[{:?}]New tip notify blocks added to share chain: {}",
                                        &algo, new_tip
                                    );
                                    let missing_parents = new_tip.into_missing_parents_vec();
                                    if !missing_parents.is_empty() {
                                        if missing_parents.len() > 5 {
                                            info!(
                                                target: LOG_TARGET,
                                                "We are missing more than 5 blocks, we are missing: {}",
                                                missing_parents.len()
                                            );
                                            return Ok(MessageAcceptance::Ignore);
                                        }

                                        if our_tip < max_payload_height.saturating_sub(10) ||
                                            our_tip > max_payload_height.saturating_add(5)
                                        {
                                            info!(
                                                target: LOG_TARGET,
                                                "Our tip({}) is too far off their new block({}) waiting for sync",
                                                our_tip, max_payload_height
                                            );
                                            return Ok(MessageAcceptance::Accept);
                                        }
                                        info!(
                                            target: LOG_TARGET,
                                            "We are missing less than 5 blocks, sending sync request with missing blocks to {}",
                                            propagation_source
                                        );
                                        let sync_share_chain = SyncMissingBlocks {
                                            algo,
                                            peer: propagation_source,
                                            missing_parents,
                                            is_from_new_block_notify: true,
                                            depth: 0,
                                        };

                                        let _unused = self
                                            .inner_request_tx
                                            .send(InnerRequest::DoSyncMissingBlocks(sync_share_chain));
                                        return Ok(MessageAcceptance::Accept);
                                    }
                                },
                                Err(error) => {
                                    error!(target: LOG_TARGET, "Failed to add synced blocks to share chain: {error:?}");
                                    self.network_peer_store
                                        .write()
                                        .await
                                        .move_to_grey_list(*message_peer, format!("Block failed validation: {error}"));
                                    return Ok(MessageAcceptance::Ignore);
                                },
                            };

                            return Ok(MessageAcceptance::Accept);
                        },
                        Err(error) => {
                            // TODO: elevate to error
                            debug!(target: LOG_TARGET, "Can't deserialize broadcast block payload: {:?}", error);
                            self.network_peer_store.write().await.move_to_grey_list(
                                source_peer,
                                format!("Node sent a block that could not be deserialized: {:?}", error),
                            );
                            return Ok(MessageAcceptance::Reject);
                        },
                    }
                },
                _ => {
                    debug!(target: MESSAGE_LOGGING_LOG_TARGET, "Unknown topic {topic:?}!");

                    warn!(target: LOG_TARGET, "Unknown topic {topic:?}!");
                    return Ok(MessageAcceptance::Reject);
                },
            }
        } else {
            warn!(target: LOG_TARGET, "No peer found for message");
        }
        Ok(MessageAcceptance::Reject)
    }

    async fn add_peer(&mut self, payload: PeerInfo, peer: PeerId) -> bool {
        // Don't add ourselves
        if &peer == self.swarm.local_peer_id() {
            error!(target: LOG_TARGET, "Peer {} has same local id as us, skipping", peer);
            return false;
        }

        if payload.version != PROTOCOL_VERSION {
            debug!(target: LOG_TARGET, "Peer {} has an outdated version, skipping", peer);
            return false;
        }

        let public_addresses = payload.public_addresses();
        let add_status = self.network_peer_store.write().await.add(peer, payload).await;

        match add_status {
            AddPeerStatus::NewPeer => {
                for addr in &public_addresses {
                    self.swarm.add_peer_address(peer, addr.clone());
                    debug!(target: LOG_TARGET, "Added peer {}", peer);
                }

                return true;
            },
            AddPeerStatus::Existing => {
                trace!(target: LOG_TARGET, "Peer was already added");
                return true;
            },
            AddPeerStatus::Greylisted => {
                debug!(target: LOG_TARGET, "Added peer but it was grey listed");
                return true;
            },
            AddPeerStatus::NonSquad => {
                debug!(target: LOG_TARGET, "Added peer {} but it was not in the same squad", peer);
                return true;
            },
            AddPeerStatus::Blacklisted => {
                debug!(target: LOG_TARGET, "Added peer {} but it was black listed", peer);
            },
        }

        false
    }

    async fn initiate_direct_peer_exchange(&mut self, peer: &PeerId) {
        if let Ok(my_info) = self
            .create_peer_info(self.swarm.external_addresses().cloned().collect())
            .await
            .inspect_err(|error| {
                error!(target: LOG_TARGET, "Failed to create peer info: {error:?}");
            })
        {
            let local_peer_id = *self.swarm.local_peer_id();
            if peer == &local_peer_id {
                return;
            }
            let peer_store_read_lock = self.network_peer_store.read().await;
            let my_best_peers = vec![];
            let known_peers = peer_store_read_lock.get_known_peers();
            drop(peer_store_read_lock);
            let request = DirectPeerInfoRequest {
                my_info,
                peer_id: local_peer_id.to_base58(),
                best_peers: my_best_peers,
                known_peer_ids: known_peers.into_iter().collect(),
            };
            debug!(target: LOG_TARGET, "Initiate direct peer exchange with peer: {}, request: {}", peer, request);
            self.swarm
                .behaviour_mut()
                .direct_peer_exchange
                .send_request(peer, request);
            if let Some(client) = &self.diagnostics_broadcast_client {
                let _unused = client.send_exchange_request(*peer, self.peer_type(peer).await);
            }
        } else {
            error!(target: LOG_TARGET, "Failed to create peer info");
        }
    }

    async fn initiate_meta_data_exchange(&mut self, peer: &PeerId, my_info: PeerInfo) {
        // if let Ok(my_info) = self
        //     .create_peer_info(self.swarm.external_addresses().cloned().collect())
        //     .await
        //     .inspect_err(|error| {
        //         error!(target: LOG_TARGET, "Failed to create peer info: {error:?}");
        //     })
        // {
        let local_peer_id = *self.swarm.local_peer_id();
        if peer == &local_peer_id {
            return;
        }

        let request = MetaDataRequest {
            my_info,
            peer_id: local_peer_id.to_base58(),
        };
        debug!(target: LOG_TARGET, "Initiate meta data exchange with peer: {}, request: {:?}", peer, request);
        self.swarm
            .behaviour_mut()
            .meta_data_exchange
            .send_request(peer, request);
        // } else {
        //     error!(target: LOG_TARGET, "Failed to create peer info");
        // }
    }

    async fn handle_direct_peer_exchange_request(
        &mut self,
        channel: ResponseChannel<Result<DirectPeerInfoResponse, String>>,
        request: DirectPeerInfoRequest,
    ) {
        if request.my_info.version != PROTOCOL_VERSION {
            // debug!(target: LOG_TARGET, squad = &self.config.squad; "Peer {} has an outdated version, skipping",
            // request.peer_id);
            let _unused = self
                .swarm
                .behaviour_mut()
                .direct_peer_exchange
                .send_response(channel, Err("Peer has an outdated version".to_string()))
                .inspect_err(|e| {
                    error!(target: LOG_TARGET, "Failed to send peer info response: {e:?}");
                });

            return;
        }

        let source_peer = request.my_info.peer_id;
        let source_peer_squad = request.my_info.squad.clone();
        let num_peers = request.best_peers.len();

        info!(
            target: LOG_TARGET,
            "[DIRECT_PEER_EXCHANGE_REQ] New peer info: {source_peer:?}({source_peer_squad:?}) with {num_peers} new peers"
        );
        for peer in &request.best_peers {
            info!(
                target: LOG_TARGET,
                "[DIRECT_PEER_EXCHANGE_REQ] new peer from request received. {:?}({})",
                peer.peer_id, peer.squad
            );
        }
        let local_peer_id = *self.swarm.local_peer_id();
        if let Ok(info) = self
            .create_peer_info(self.swarm.external_addresses().cloned().collect())
            .await
            .inspect_err(|error| {
                error!(target: LOG_TARGET, "[DIRECT_PEER_EXCHANGE_REQ] Failed to create peer info: {error:?}");
            })
        {
            let num_peers = if request.best_peers.is_empty() {
                NUM_PEERS_INITIAL_SYNC
            } else {
                NUM_PEERS_TO_SYNC_PER_ALGO
            };

            let my_best_peers = self.network_peer_store.read().await.best_peers_to_share(
                num_peers,
                &request.my_info.squad,
                &request.known_peer_ids,
            );
            let my_best_peers: Vec<_> = my_best_peers.into_iter().map(|p| p.peer_info).collect();
            if let Err(e) = self.swarm.behaviour_mut().direct_peer_exchange.send_response(
                channel,
                Ok(DirectPeerInfoResponse {
                    peer_id: local_peer_id.to_base58(),
                    info,
                    best_peers: my_best_peers,
                }),
            ) {
                error!(
                    target: LOG_TARGET,
                    "[DIRECT_PEER_EXCHANGE_REQ] Failed to send peer info response to {:?}: {:?}",
                    source_peer, e
                );
            }
        } else {
            error!(target: LOG_TARGET, "[DIRECT_PEER_EXCHANGE_REQ] Failed to create peer info");
        }

        match request.peer_id.parse::<PeerId>() {
            Ok(peer_id) => {
                if self.add_peer(request.my_info, peer_id).await {
                    // self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                }
                for mut peer in request.best_peers {
                    if let Some(peer_id) = peer.peer_id {
                        debug!(
                            target: LOG_TARGET,
                            "[DIRECT_PEER_EXCHANGE_REQ] New peer info: {:?}({})  [rx {}, sha {}]received from {}",
                            peer.peer_id, peer.squad, peer.current_random_x_height, peer.current_sha3x_height, peer_id
                        );

                        // Reset the timestamp so that we can try to ping them
                        peer.timestamp = EpochTime::now().as_u64();
                        self.add_peer(peer, peer_id).await;
                    }
                }
            },
            Err(error) => {
                error!(target: LOG_TARGET, "[DIRECT_PEER_EXCHANGE_REQ] Failed to parse peer id: {error:?}");
            },
        }
    }

    async fn handle_meta_data_exchange_request(
        &mut self,
        channel: ResponseChannel<Result<MetaDataResponse, String>>,
        request: MetaDataRequest,
    ) {
        if request.my_info.version != PROTOCOL_VERSION {
            // debug!(target: LOG_TARGET, squad = &self.config.squad; "Peer {} has an outdated version, skipping",
            // request.peer_id);
            let _unused = self
                .swarm
                .behaviour_mut()
                .meta_data_exchange
                .send_response(channel, Err("Peer has an outdated version".to_string()))
                .inspect_err(|e| {
                    error!(target: LOG_TARGET, "Failed to send peer info response: {e:?}");
                });

            return;
        }

        let source_peer = request.my_info.peer_id;

        info!(target: PEER_INFO_LOGGING_LOG_TARGET, "[META_DATA_REQ] New peer info: {source_peer:?}({})", request.my_info.squad);
        let local_peer_id = *self.swarm.local_peer_id();
        if let Ok(info) = self
            .create_peer_info(self.swarm.external_addresses().cloned().collect())
            .await
            .inspect_err(|error| {
                error!(target: LOG_TARGET, "Failed to create peer info: {error:?}");
            })
        {
            if let Err(e) = self.swarm.behaviour_mut().meta_data_exchange.send_response(
                channel,
                Ok(MetaDataResponse {
                    peer_id: local_peer_id.to_base58(),
                    info,
                }),
            ) {
                error!(target: LOG_TARGET, "Failed to send meta data response to {:?}: {:?}", source_peer, e);
            }
        } else {
            error!(target: LOG_TARGET, "Failed to create peer info");
        }

        match request.peer_id.parse::<PeerId>() {
            Ok(peer_id) => {
                if self.add_peer(request.my_info, peer_id).await {
                    // self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                }
            },
            Err(error) => {
                error!(target: LOG_TARGET, "Failed to parse peer id: {error:?}");
            },
        }
    }

    async fn handle_meta_data_exchange_response(&mut self, response: MetaDataResponse) {
        info!(target: PEER_INFO_LOGGING_LOG_TARGET, "[META_DATA_EXCHANGE_RESP] New peer info: {}({})", response.peer_id, response.info.squad);
        match response.peer_id.parse::<PeerId>() {
            Ok(peer_id) => {
                // If they are talking an older version, disconnect
                if response.info.version != PROTOCOL_VERSION {
                    warn!(target: LOG_TARGET, "Peer {} has an outdated version, disconnecting", peer_id);
                    let _ = self.swarm.disconnect_peer_id(peer_id);
                    return;
                }
                self.add_peer(response.info.clone(), peer_id).await;

                // This is a seed peer, so we dont care about chain pow
                if self.network_peer_store.read().await.is_seed_peer(&peer_id) {
                    return;
                }

                if self.network_peer_store.read().await.is_blacklisted(&peer_id) {
                    debug!(target: LOG_TARGET, "Peer {} is blacklisted, skipping", peer_id);
                    return;
                }
                if self.network_peer_store.read().await.is_greylisted(&peer_id) {
                    debug!(target: LOG_TARGET, "Peer {} is greylisted, skipping", peer_id);
                    return;
                }
                if response.info.squad != self.squad {
                    // this is a non squad peer, so we should not care about their tips
                    return;
                }

                // if we are a seed peer, end here
                if self.config.is_seed_peer {
                    return;
                }
                // if should_we_make_explict_peer {
                // self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                // }
                let our_tip_sha3x = self.share_chain_sha3x.chain_pow().await;

                if self.config.sha3x_enabled && response.info.current_sha3x_pow > our_tip_sha3x.as_u128() {
                    let perform_catch_up_sync = PerformCatchUpSync {
                        algo: PowAlgorithm::Sha3x,
                        peer: peer_id,
                        last_block_from_them: None,
                        their_height: response.info.current_sha3x_height,
                        // their_pow: response.info.current_sha3x_pow,
                        permit: None,
                    };
                    let _unused = self
                        .inner_request_tx
                        .send(InnerRequest::PerformCatchUpSync(perform_catch_up_sync));
                }
                let our_tip_rx = self.share_chain_random_x.chain_pow().await;

                if self.config.randomx_enabled && response.info.current_random_x_pow > our_tip_rx.as_u128() {
                    let perform_catch_up_sync = PerformCatchUpSync {
                        algo: PowAlgorithm::RandomX,
                        peer: peer_id,
                        last_block_from_them: None,
                        their_height: response.info.current_random_x_height,
                        // their_pow: response.info.current_random_x_pow,
                        permit: None,
                    };
                    let _unused = self
                        .inner_request_tx
                        .send(InnerRequest::PerformCatchUpSync(perform_catch_up_sync));
                }
            },
            Err(error) => {
                error!(target: LOG_TARGET, "Failed to parse peer id: {error:?}");
            },
        }
    }

    async fn handle_direct_peer_exchange_response(&mut self, response: DirectPeerInfoResponse) {
        info!(
            target: LOG_TARGET,
            "[DIRECT_PEER_EXCHANGE_RESP] New peer info: {} with {} peers",
            response.peer_id, response.best_peers.len()
        );
        match response.peer_id.parse::<PeerId>() {
            Ok(peer_id) => {
                if response.info.version != PROTOCOL_VERSION {
                    debug!(target: LOG_TARGET, "Peer {} has an outdated version, skipping", response.peer_id);
                    let _ = self.swarm.disconnect_peer_id(peer_id);
                    return;
                }
                let mut num_peers_added = 0;
                let num_peers_received = response.best_peers.len();
                for mut peer in response.best_peers {
                    debug!(
                        target: LOG_TARGET,
                        "[DIRECT_PEER_EXCHANGE_RESP] New peer info: {:?}, squad: {}  [rx {}, sha {}]received from {}",
                        peer.peer_id, peer.squad, peer.current_random_x_height, peer.current_sha3x_height, peer_id
                    );
                    if let Some(peer_id) = peer.peer_id {
                        // Reset the timestamp so that we can try to ping them
                        peer.timestamp = EpochTime::now().as_u64();
                        if self.add_peer(peer, peer_id).await {
                            num_peers_added += 1;
                        }
                    }
                }

                // Update peer diagnostics
                if let Some(client) = &self.diagnostics_broadcast_client {
                    let _unused = client.send_peer_response(
                        peer_id,
                        num_peers_received,
                        num_peers_added,
                        self.peer_type(&peer_id).await,
                    );
                }

                // if we are a seed peer, end here
                if self.config.is_seed_peer {
                    debug!(
                        target: LOG_TARGET,
                        "[DIRECT_PEER_EXCHANGE_RESP] Is seed peer, skipping direct peer exchange with {}",
                        response.peer_id
                    );
                    return;
                }

                // Keep going until we have all the peers
                if num_peers_added > 0 {
                    debug!(
                        target: LOG_TARGET,
                        "[DIRECT_PEER_EXCHANGE_RESP] {} peers added from peer {}",
                        num_peers_added, peer_id
                    );
                    self.initiate_direct_peer_exchange(&peer_id).await;
                } else {
                    debug!(target: LOG_TARGET, "[DIRECT_PEER_EXCHANGE_RESP] No peers added from peer {}", peer_id);
                }

                // Once we have peer info from the seed peers, disconnect from them, but only if we dialed the seed
                // peer. If the seed peer dialed us, we should keep the connection active
                if self.dialed_seed_peers.remove(&peer_id) {
                    info!(target: LOG_TARGET, "[DIRECT_PEER_EXCHANGE_RESP] Disconnecting from seed peer {}", peer_id);
                    let _ = self.swarm.disconnect_peer_id(peer_id);
                }
            },
            Err(error) => {
                error!(target: LOG_TARGET, "[DIRECT_PEER_EXCHANGE_RESP] Failed to parse peer id: {error:?}");
            },
        }
    }

    /// Handles share chain sync request (coming from other peer).
    async fn handle_sync_missing_blocks_request(
        &mut self,
        channel: ResponseChannel<Result<SyncMissingBlocksResponse, String>>,
        request: SyncMissingBlocksRequest,
        from: &PeerId,
    ) {
        debug!(target: LOG_TARGET, "Incoming Share chain sync request of len: {}", request.missing_blocks().len());
        let share_chain = match request.algo() {
            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
        };
        let local_peer_id = *self.swarm.local_peer_id();
        let blocks = share_chain.get_blocks(request.missing_blocks()).await;
        if blocks.is_empty() {
            warn!(
                target: LOG_TARGET,
                "No blocks found for sync request: {} {} from {}",
                request.algo(),
                request.missing_blocks().iter().map(
                    |(height, hash)| format!("{}({:x}{:x}{:x}{:x})", height, hash[0], hash[1], hash[2], hash[3])
                ).collect::<Vec<String>>().join(","),
                from
            );
            let _unused = self
                .swarm
                .behaviour_mut()
                .share_chain_sync
                .send_response(channel, Err("No blocks found".to_string()))
                .inspect_err(|_e| {
                    error!(target: LOG_TARGET, "Failed to send block sync response");
                });
            return;
        }
        let response = SyncMissingBlocksResponse::new(local_peer_id, request.algo(), &blocks);

        let _unused = self
            .swarm
            .behaviour_mut()
            .share_chain_sync
            .send_response(channel, Ok(response))
            .inspect_err(|_e| {
                error!(target: LOG_TARGET, "Failed to send block sync response");
            });
    }

    /// Handle share chain sync response.
    /// All the responding blocks will be tried to put into local share chain.
    async fn handle_sync_missing_blocks_response(&mut self, response: SyncMissingBlocksResponse, depth: usize) {
        debug!(target: MESSAGE_LOGGING_LOG_TARGET, "Share chain sync response: {response}");
        let peer = *response.peer_id();

        if response.version() != PROTOCOL_VERSION {
            trace!(target: LOG_TARGET, "Peer {} has an outdated version, skipping", peer);
            return;
        }

        if response.blocks.is_empty() {
            trace!(target: LOG_TARGET, "Peer {} sent 0 blocks", peer);
            return;
        }
        // if !self.sync_in_progress.load(Ordering::SeqCst) {
        // return;
        // },
        let algo = response.algo();
        let blocks: Vec<_> = response.into_blocks().into_iter().collect();
        info!(target: SYNC_REQUEST_LOG_TARGET, "Received sync response for chain {} from {} with blocks {:?}", algo,  peer, blocks.iter().map(|a| format!("{}({:x}{:x}{:x}{:x})",a.height, a.hash[0], a.hash[1], a.hash[2], a.hash[3])).collect::<Vec<String>>());
        // tokio::spawn(async move {

        for block in blocks {
            let add_sync_req = InnerRequest::AddSyncedBlock {
                algo,
                block: Arc::new(block),
                source_peer: peer,
                depth,
            };

            let _unused = self.inner_request_tx.send(add_sync_req);
        }
        //     match share_chasin.add_synced_blocks(blocks).await {
        //         Ok(new_tip) => {
        //             info!(target: LOG_TARGET, "[{:?}] Synced blocks added to share chain: {}",algo, new_tip);
        //             let missing_parents = new_tip.into_missing_parents_vec();
        //             if !missing_parents.is_empty() {
        //                 if depth + 1 > max_sync_depth {
        //                     info!(target: SYNC_REQUEST_LOG_TARGET, "Sync depth reached max depth of {}",
        // max_sync_depth);                     return;
        //                 }

        //                 let sync_share_chain = SyncMissingBlocks {
        //                     algo,
        //                     peer,
        //                     missing_parents,
        //                     is_from_new_block_notify: false,
        //                     depth: depth + 1,
        //                 };

        //                 let _unused = tx.send(InnerRequest::DoSyncMissingBlocks(sync_share_chain));
        //             }
        //         },
        //         Err(error) => {
        //             error!(target: LOG_TARGET, "Failed to add synced blocks to share chain: {error:?}");
        //             peer_store
        //                 .write()
        //                 .await
        //                 .move_to_grey_list(peer, format!("Block failed validation: {error}"));
        //         },
        //     };
        //     if timer.elapsed() > MAX_ACCEPTABLE_P2P_MESSAGE_TIMEOUT {
        //         warn!(target: LOG_TARGET, "Share chain sync response took too long: {:?}", timer.elapsed());
        //     }
        // });
    }

    /// Trigger share chain sync with another peer with the highest known block height.
    /// Note: this is a "stop-the-world" operation, many operations are skipped when synchronizing.
    #[allow(clippy::too_many_lines)]
    async fn sync_missing_blocks(&mut self, sync_share_chain: SyncMissingBlocks) {
        debug!(target: LOG_TARGET, "Syncing share chain...");
        let SyncMissingBlocks {
            peer,
            algo,
            mut missing_parents,
            is_from_new_block_notify,
            depth,
        } = sync_share_chain;
        let peer_height = match self.network_peer_store.read().await.get(&peer) {
            Some(p) => match algo {
                PowAlgorithm::RandomX => p.peer_info.current_random_x_height,
                PowAlgorithm::Sha3x => p.peer_info.current_sha3x_height,
            },
            None => 0,
        };
        missing_parents
            .retain(|(height, _)| *height >= peer_height.saturating_sub(10).saturating_sub(self.share_window * 2));

        if depth + 1 > self.config.max_missing_blocks_sync_depth {
            info!(
                target: SYNC_REQUEST_LOG_TARGET,
                "Sync depth reached max depth of {}",
                self.config.max_missing_blocks_sync_depth
            );
            return;
        }

        if is_from_new_block_notify {
            info!(
                target: SYNC_REQUEST_LOG_TARGET,
                "[{}] Sending sync to connected peers for blocks {:?} from notify",
                algo, missing_parents.iter().map(|(height, hash)|format!("{}({:x}{:x}{:x}{:x})",height, hash[0], hash[1], hash[2], hash[3])).collect::<Vec<String>>()
            );
        } else {
            debug!(
                target: SYNC_REQUEST_LOG_TARGET,
                "[{}] Sending sync to connected peers for blocks {:?} from sync",
                algo, missing_parents.iter().map(|(height, hash)|format!("{}({:x}{:x}{:x}{:x})",height, hash[0], hash[1], hash[2], hash[3])).collect::<Vec<String>>()
            );
        }
        if missing_parents.is_empty() {
            warn!(target: LOG_TARGET, "Sync called but with no missing parents.");
            return;
        }

        let share_chain = match algo {
            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
        };
        let blocks_already_received = share_chain.do_blocks_exist(&missing_parents).await;
        missing_parents.retain(|(height, hash)| {
            !blocks_already_received
                .iter()
                .any(|(existing_height, existing_hash)| existing_height == height && existing_hash == hash)
        });

        if missing_parents.is_empty() {
            info!(target: SYNC_REQUEST_LOG_TARGET, "All missing blocks have already been received");
            return;
        }

        // If it's not from new_block_notify, ask only the peer that sent the blocks
        if !is_from_new_block_notify {
            info!(
                target: SYNC_REQUEST_LOG_TARGET,
                "Sending sync request direct to peer {} for blocks {:?} because we did not receive it from sync",
                peer, missing_parents.iter().map(|(height, hash)|format!("{}({:x}{:x}{:x}{:x})",height, hash[0], hash[1], hash[2], hash[3])).collect::<Vec<String>>()
            );

            let outbound_id = self
                .swarm
                .behaviour_mut()
                .share_chain_sync
                .send_request(&peer, SyncMissingBlocksRequest::new(algo, missing_parents.clone()));
            self.missing_blocks_sync_request_depth.insert(outbound_id, depth + 1);
            return;
        }
        // ask our connected peers rather than everyone swarming the original peer
        // let mut sent_to_original_peer = false;
        let connected_peers: Vec<_> = self.swarm.connected_peers().copied().collect();
        let min_height = missing_parents.iter().map(|(height, _)| height).min().unwrap_or(&0);
        let mut peers_asked = 0;
        let mut highest_peer_height = 0;
        for connected_peer in connected_peers {
            let read_lock = self.network_peer_store.read().await;
            if let Some(p) = read_lock.get(&connected_peer) {
                match algo {
                    PowAlgorithm::RandomX => {
                        if p.peer_info.current_random_x_height > highest_peer_height {
                            highest_peer_height = p.peer_info.current_random_x_height;
                        }
                        if p.peer_info.current_random_x_height.saturating_add(20) < *min_height {
                            info!(
                                target: SYNC_REQUEST_LOG_TARGET,
                                "Peer {} is too far behind for RandomX sync",
                                connected_peer
                            );
                            continue;
                        }
                    },
                    PowAlgorithm::Sha3x => {
                        if p.peer_info.current_sha3x_height > highest_peer_height {
                            highest_peer_height = p.peer_info.current_sha3x_height;
                        }
                        if p.peer_info.current_sha3x_height.saturating_add(20) < *min_height {
                            info!(
                                target: SYNC_REQUEST_LOG_TARGET,
                                "Peer {} is too far behind for Sha3x sync",
                                connected_peer
                            );
                            continue;
                        }
                    },
                }
            }
            drop(read_lock);

            let outbound_id = self.swarm.behaviour_mut().share_chain_sync.send_request(
                &connected_peer,
                SyncMissingBlocksRequest::new(algo, missing_parents.clone()),
            );
            self.missing_blocks_sync_request_depth.insert(outbound_id, depth + 1);
            peers_asked += 1;
        }

        if peers_asked == 0 {
            warn!(
                target: SYNC_REQUEST_LOG_TARGET,
                "[{}] No connected peers had a high enough chain to ask for this missing block. Highest peer height:{}",
                algo, highest_peer_height
            );
        }
    }

    async fn peer_type(&self, peer_id: &PeerId) -> PeerType {
        let network_peer_store = self.network_peer_store.read().await;
        if network_peer_store.is_seed_peer(peer_id) {
            return PeerType::SeedPeer;
        }
        if network_peer_store.get(peer_id).is_some() {
            if self.relay_store.read().await.is_relay(peer_id) {
                PeerType::RelayPeer
            } else {
                PeerType::PrivatePeer
            }
        } else if network_peer_store.non_squad_peers().get(&peer_id.to_base58()).is_some() {
            PeerType::NonSquadPeer
        } else {
            PeerType::Unknown
        }
    }

    /// Main method to handle libp2p events.
    #[allow(clippy::too_many_lines)]
    async fn handle_event(&mut self, event: SwarmEvent<ServerNetworkBehaviourEvent>) {
        debug!(target: MESSAGE_LOGGING_LOG_TARGET, "New event: {}", format_swarm_event(&event));

        match event {
            SwarmEvent::ConnectionEstablished {
                peer_id,
                endpoint,
                num_established,
                concurrent_dial_errors,
                established_in,
                ..
            } => {
                {
                    let peer_store_read_lock = self.network_peer_store.read().await;

                    if peer_store_read_lock.is_blacklisted(&peer_id) {
                        warn!(
                            target: LOG_TARGET,
                            "Connection established with blacklisted peer: {peer_id:?} -> {endpoint:?} ({num_established:?}/{concurrent_dial_errors:?}/{established_in:?})"
                        );
                        let _ = self.swarm.disconnect_peer_id(peer_id);
                        return;
                    }

                    if endpoint.is_dialer() {
                        // we have dialed this connection, so  let see if we dialed a seed peer.
                        if peer_store_read_lock.is_seed_peer(&peer_id) {
                            // we have dialed a seed peer, lets make sure we note this down so that we can disconnect
                            // after exchanging peers
                            self.dialed_seed_peers.insert(peer_id);
                        }
                    }
                }

                info!(
                    target: LOG_TARGET,
                    "Connection established: {peer_id:?} -> {endpoint:?} ({num_established:?}/{concurrent_dial_errors:?}/{established_in:?})"
                );
                if let Some(client) = &self.diagnostics_broadcast_client {
                    let _unused = client.send_peer_connected(peer_id, self.peer_type(&peer_id).await);
                }
                // if num_established == NonZeroU32::new(1).expect("Can't fail") {
                self.initiate_direct_peer_exchange(&peer_id).await;
                // self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                // }
            },
            SwarmEvent::Dialing { peer_id, .. } => {
                info!(target: LOG_TARGET, "Dialing: {peer_id:?}");
                if let Some(diagnostics_broadcast_client) = &self.diagnostics_broadcast_client {
                    if let Some(peer_id) = peer_id {
                        let _unused =
                            diagnostics_broadcast_client.send_new_peer(peer_id, self.peer_type(&peer_id).await);
                        debug!(target: LOG_TARGET, "[Diagnostics] Attempting to dial peer '{:?}'", peer_id);
                    }
                }
            },
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(target: LOG_TARGET, "Listening on {address:?}");
                let timer = Instant::now();
                if !is_localhost_or_private(&address) {
                    info!(target: LOG_TARGET, "Publishing peer info");

                    // broadcast peer info
                    if let Err(error) = self.broadcast_peer_info().await {
                        warn!(target: LOG_TARGET, "Failed to broadcast peer info: {error}");
                    }

                    // lets dial the seed peers again, so that they can update our new address
                    // We will disconnect again after a peer sync
                    // if let Err(e) = self.dial_seed_peers().await {
                    //     warn!(target: LOG_TARGET, "Failed to dial seed peers: {e:?}");
                    // }
                }
                if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                    warn!(target: LOG_TARGET, "Peer info publishing took too long: {:?}", timer.elapsed());
                }
            },
            SwarmEvent::ConnectionClosed {
                peer_id,
                connection_id: _,
                endpoint,
                num_established,
                cause,
            } => {
                // // Ignore dials where we can't get hold of the person
                // if !endpoint.is_dialer() {
                //     warn!(target: LOG_TARGET, squad = &self.config.squad; "Connection closed: {peer_id:?} ->
                // {endpoint:?} ({num_established:?}) -> {cause:?}"); }
                warn!(target: LOG_TARGET, "Connection closed: {peer_id:?} -> {endpoint:?} ({num_established:?}) -> {cause:?}");
            },
            SwarmEvent::IncomingConnectionError {
                connection_id,
                local_addr,
                send_back_addr,
                error,
            } => {
                info!(target: LOG_TARGET, "Incoming connection error: {connection_id:?} -> {local_addr:?} -> {send_back_addr:?} -> {error:?}");
            },
            SwarmEvent::ListenerError { listener_id, error } => {
                error!(target: LOG_TARGET, "Listener error: {listener_id:?} -> {error:?}");
            },
            SwarmEvent::ExternalAddrExpired { address } => {
                info!(target: LOG_TARGET, "External address expired: {address:?}");
                // self.attempt_relay_reservation().await;
            },
            SwarmEvent::OutgoingConnectionError {
                peer_id: Some(peer_id),
                error,
                ..
            } => {
                match error {
                    DialError::Transport(transport_error) => {
                        // There are a lot of cancelled errors, so ignore them
                        warn!(target: LOG_TARGET, "Outgoing connection error, ignoring: {peer_id:?} -> {transport_error:?}");
                    },
                    _ => {
                        warn!(target: LOG_TARGET, "Outgoing connection error: {peer_id:?} -> {error:?}");
                        self.network_peer_store
                            .write()
                            .await
                            .move_to_grey_list(peer_id, format!("Outgoing connection error: {error}"));
                    },
                };
            },
            SwarmEvent::Behaviour(event) => {
                match event {
                    ServerNetworkBehaviourEvent::Mdns(mdns_event) => {
                        trace!(target: LOG_TARGET, "ServerNetworkBehaviourEvent::Mdns: {:?}", mdns_event.clone());
                        match mdns_event {
                            mdns::Event::Discovered(peers) => {
                                for (peer, addr) in peers {
                                    self.swarm.add_peer_address(peer, addr);
                                    // self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer);
                                }
                            },
                            mdns::Event::Expired(peers) => {
                                for (peer, _addr) in peers {
                                    self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer);
                                }
                            },
                        }
                    },
                    ServerNetworkBehaviourEvent::Gossipsub(event) => {
                        trace!(target: LOG_TARGET, "ServerNetworkBehaviourEvent::Gossipsub: {:?}", event);
                        match event {
                            gossipsub::Event::Message {
                                message,
                                message_id,
                                propagation_source,
                            } => match self.handle_new_gossipsub_message(message, propagation_source).await {
                                Ok(res) => {
                                    let _unused = self.swarm.behaviour_mut().gossipsub.report_message_validation_result(
                            &message_id,
                            &propagation_source,
                            res,
                        ).inspect_err(|e| {
                            error!(target: LOG_TARGET, "Failed to report message validation result: {e:?}");
                        });
                                },
                                Err(error) => {
                                    error!(target: LOG_TARGET, "Failed to handle gossipsub message: {error:?}");
                                    let _unused = self.swarm.behaviour_mut().gossipsub.report_message_validation_result(
                                &message_id,
                                &propagation_source,
                                MessageAcceptance::Reject,
                            ).inspect_err(|e| {
                                error!(target: LOG_TARGET, "Failed to report message validation result: {e:?}");
                            });
                                },
                            },
                            gossipsub::Event::Subscribed { peer_id, .. } => {
                                self.initiate_direct_peer_exchange(&peer_id).await;
                            },
                            gossipsub::Event::Unsubscribed { .. } => {},
                            gossipsub::Event::GossipsubNotSupported { .. } => {},
                        }
                    },
                    ServerNetworkBehaviourEvent::MetaDataExchange(event) => {
                        trace!(target: LOG_TARGET, "ServerNetworkBehaviourEvent::MetaDataExchange: {:?}", event);
                        match event {
                            request_response::Event::Message { peer, message } => match message {
                                request_response::Message::Request {
                                    request_id: _request_id,
                                    request,
                                    channel,
                                } => {
                                    self.handle_meta_data_exchange_request(channel, request).await;
                                },
                                request_response::Message::Response {
                                    request_id: _request_id,
                                    response,
                                } => match response {
                                    Ok(response) => {
                                        self.handle_meta_data_exchange_response(response).await;
                                    },
                                    Err(error) => {
                                        error!(target: LOG_TARGET, "REQ-RES peer: {peer} info response error: {error:?}");
                                    },
                                },
                            },
                            request_response::Event::OutboundFailure { peer, error, .. } => {
                                // Peers can be offline
                                debug!(target: LOG_TARGET, "REQ-RES meta data outbound failure: {peer} -> {error:?}");
                            },
                            request_response::Event::InboundFailure { peer, error, .. } => {
                                error!(target: LOG_TARGET, "REQ-RES  meta data inbound failure: {peer} -> {error:?}");
                            },
                            request_response::Event::ResponseSent { .. } => {},
                        }
                    },
                    ServerNetworkBehaviourEvent::DirectPeerExchange(event) => {
                        trace!(target: LOG_TARGET, "ServerNetworkBehaviourEvent::DirectPeerExchange: {:?}", event);
                        match event {
                            request_response::Event::Message { peer, message } => {
                                trace!(target: PEER_INFO_LOGGING_LOG_TARGET, "DirectPeerExchange: {:?}", message);
                                match message {
                                    request_response::Message::Request {
                                        request_id: _request_id,
                                        request,
                                        channel,
                                    } => {
                                        self.handle_direct_peer_exchange_request(channel, request).await;
                                    },
                                    request_response::Message::Response {
                                        request_id: _request_id,
                                        response,
                                    } => match response {
                                        Ok(response) => {
                                            self.handle_direct_peer_exchange_response(response).await;
                                        },
                                        Err(error) => {
                                            error!(target: LOG_TARGET, "REQ-RES peer: {peer} info response error: {error:?}");
                                        },
                                    },
                                }
                            },
                            request_response::Event::OutboundFailure { peer, error, .. } => {
                                // Peers can be offline
                                debug!(target: LOG_TARGET, "REQ-RES peer info outbound failure: {peer} -> {error:?}");
                                // TODO: find out why this errors
                                // self.network_peer_store
                                //     .move_to_grey_list(
                                //         peer,
                                //         format!("ShareChainError during direct peer exchange: {}",
                                // error.to_string()),     )
                                //     .await;
                            },
                            request_response::Event::InboundFailure { peer, error, .. } => {
                                error!(target: LOG_TARGET, "REQ-RES  peer info inbound failure: {peer} -> {error:?}");
                            },
                            request_response::Event::ResponseSent { .. } => {},
                        }
                    },
                    ServerNetworkBehaviourEvent::ShareChainSync(event) => {
                        trace!(target: LOG_TARGET, "ServerNetworkBehaviourEvent::ShareChainSync: {:?}", event);
                        match event {
                            request_response::Event::Message { peer, message } => match message {
                                request_response::Message::Request {
                                    request_id: _request_id,
                                    request,
                                    channel,
                                } => {
                                    self.handle_sync_missing_blocks_request(channel, request, &peer).await;
                                },
                                request_response::Message::Response { request_id, response } => {
                                    let current_depth = self.missing_blocks_sync_request_depth.remove(&request_id);
                                    if let Some(depth) = current_depth {
                                        match response {
                                            Ok(response) => {
                                                self.handle_sync_missing_blocks_response(response, depth).await;
                                            },
                                            Err(error) => {
                                                error!(target: LOG_TARGET, "REQ-RES share chain sync response error: {error:?}");
                                            },
                                        }
                                    } else {
                                        warn!(target: SYNC_REQUEST_LOG_TARGET, "Received a response for a request that we didn't send: {peer} -> {response:?}");
                                    }
                                },
                            },
                            request_response::Event::OutboundFailure {
                                peer,
                                error,
                                request_id,
                            } => {
                                debug!(target: LOG_TARGET, "REQ-RES outbound failure: {peer} -> {error:?}");
                                self.missing_blocks_sync_request_depth.remove(&request_id);
                                let mut should_grey_list = true;
                                match error {
                                    OutboundFailure::DialFailure => {
                                        debug!(target: SYNC_REQUEST_LOG_TARGET, "Share chain sync request failed: {peer} -> {error:?}");
                                    },
                                    OutboundFailure::ConnectionClosed => {
                                        // I think it might upgrade to a DCTUR so no need to grey list
                                        debug!(target: SYNC_REQUEST_LOG_TARGET, "Share chain sync request failed: {peer} -> {error:?}");
                                        should_grey_list = false;
                                    },
                                    _ => {
                                        warn!(target: SYNC_REQUEST_LOG_TARGET, "Share chain sync request failed: {peer} -> {error:?}");
                                    },
                                }
                                if should_grey_list {
                                    self.network_peer_store.write().await.move_to_grey_list(
                                        peer,
                                        format!("ShareChainError during share chain sync:{}", error),
                                    );
                                    let _ = self.swarm.disconnect_peer_id(peer).inspect(|e| {
                                        warn!(target: SYNC_REQUEST_LOG_TARGET, "Failed to disconnect peer {peer}: {e:?}");
                                    });
                                }

                                // Remove peer from peer store to try to sync from another peer,
                                // if the peer goes online/accessible again, the peer store will have it again.
                                // self.network_peer_store.remove(&peer).await;
                            },
                            request_response::Event::InboundFailure { peer, error, .. } => {
                                error!(target: LOG_TARGET, "REQ-RES inbound failure: {peer} -> {error:?}");
                            },
                            request_response::Event::ResponseSent { .. } => {},
                        };
                    },
                    ServerNetworkBehaviourEvent::CatchUpSync(event) => {
                        trace!(target: LOG_TARGET, "ServerNetworkBehaviourEvent::CatchUpSync: {:?}", event);
                        match event {
                            request_response::Event::Message { peer: _peer, message } => match message {
                                request_response::Message::Request {
                                    request_id: _request_id,
                                    request,
                                    channel,
                                } => {
                                    self.handle_catch_up_sync_request(channel, request).await;
                                },
                                request_response::Message::Response { request_id, response } => {
                                    let permit = self.release_catchup_sync_permit(request_id);
                                    match response {
                                        Ok(response) => {
                                            self.handle_catch_up_sync_response(response, permit).await;
                                        },
                                        Err(error) => {
                                            error!(target: LOG_TARGET, "REQ-RES catch up sync response error: {error:?}");
                                        },
                                    }
                                },
                            },
                            request_response::Event::OutboundFailure {
                                peer,
                                error,
                                request_id,
                            } => {
                                // Peers can be offline
                                debug!(target: LOG_TARGET, "REQ-RES outbound failure: {peer} -> {error:?}");

                                let should_remove = self
                                    .randomx_in_progress_syncs
                                    .get(&peer)
                                    .map(|r| r.0 == request_id)
                                    .unwrap_or(false);
                                if should_remove {
                                    if let Some((_r, permit)) = self.randomx_in_progress_syncs.remove(&peer) {
                                        // Probably don't need to do this
                                        info!(target: SYNC_REQUEST_LOG_TARGET, "Removing randomx_in_progress_syncs: {peer} -> {error:?}");
                                        self.last_failed_randomx_peer = Some(peer);
                                        drop(permit);
                                    }
                                }

                                let should_remove = self
                                    .sha3x_in_progress_syncs
                                    .get(&peer)
                                    .map(|r| r.0 == request_id)
                                    .unwrap_or(false);
                                if should_remove {
                                    if let Some((_r, permit)) = self.sha3x_in_progress_syncs.remove(&peer) {
                                        info!(target: SYNC_REQUEST_LOG_TARGET, "Removing sha3x_in_progress_syncs: {peer} -> {error:?}");
                                        self.last_failed_sha3x_peer = Some(peer);
                                        // Probably don't need to do this
                                        drop(permit);
                                    }
                                }

                                let mut should_grey_list = true;
                                match error {
                                    OutboundFailure::DialFailure => {
                                        debug!(target: SYNC_REQUEST_LOG_TARGET, "Catch up sync request failed: {peer} -> {error:?}");
                                    },
                                    OutboundFailure::ConnectionClosed => {
                                        // I think it might upgrade to a DCTUR so no need to grey list
                                        debug!(target: SYNC_REQUEST_LOG_TARGET, "Catch up sync request failed: {peer} -> {error:?}");
                                        should_grey_list = false;
                                    },
                                    _ => {
                                        warn!(target: SYNC_REQUEST_LOG_TARGET, "Catch up sync request failed: {peer} -> {error:?}");
                                    },
                                }
                                if should_grey_list {
                                    self.network_peer_store.write().await.move_to_grey_list(
                                        peer,
                                        format!("ShareChainError during catch up sync:{}", error),
                                    );
                                }
                                // Remove peer from peer store to try to sync from another peer,
                                // if the peer goes online/accessible again, the peer store will have it again.
                                // self.network_peer_store.remove(&peer).await;
                            },
                            request_response::Event::InboundFailure { peer, error, .. } => {
                                error!(target: LOG_TARGET, "REQ-RES inbound failure: {peer} -> {error:?}");
                            },
                            request_response::Event::ResponseSent { .. } => {},
                        };
                    },

                    ServerNetworkBehaviourEvent::Identify(event) => {
                        trace!(target: LOG_TARGET, "ServerNetworkBehaviourEvent::Identify: {:?}", event);
                        match event {
                            identify::Event::Received { peer_id, info, .. } => {
                                self.handle_peer_identified(peer_id, info).await
                            },
                            identify::Event::Error { peer_id, error, .. } => {
                                warn!("Failed to identify peer {peer_id:?}: {error:?}");
                                // self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                                // self.swarm.behaviour_mut().kademlia.remove_peer(&peer_id);
                            },
                            _ => {},
                        }
                    },
                    ServerNetworkBehaviourEvent::RelayServer(event) => {
                        info!(target: LOG_TARGET, "[RELAY SERVER]: {event:?}");
                    },
                    ServerNetworkBehaviourEvent::RelayClient(event) => {
                        info!(target: LOG_TARGET, "[RELAY CLIENT]: {event:?}");
                        if let relay::client::Event::ReservationReqAccepted {
                            relay_peer_id,
                            renewal: _,
                            limit,
                        } = event
                        {
                            if let Some(l) = limit {
                                let mut lock = self.relay_store.write().await;
                                lock.confirm_reservation(
                                    relay_peer_id,
                                    Instant::now() + l.duration().unwrap_or(Duration::from_secs(60)),
                                );
                            } else {
                                warn!(target: LOG_TARGET, "Relay peer {relay_peer_id} accepted reservation request with no limit set. This is not expected.");
                            }
                        }
                    },
                    ServerNetworkBehaviourEvent::Dcutr(event) => {
                        info!(target: "tari::p2pool::dcutr", "[DCUTR]: {event:?}");
                    },
                    ServerNetworkBehaviourEvent::Autonat(event) => {
                        trace!(target: LOG_TARGET, "ServerNetworkBehaviourEvent::Autonat: {:?}", event);
                        self.handle_autonat_event(event).await
                    },
                    ServerNetworkBehaviourEvent::Ping(event) => {
                        debug!(target: LOG_TARGET, "[PING]: {event:?}");
                        // Remove a peer from the greylist if we are in contact with them
                        self.network_peer_store
                            .write()
                            .await
                            .set_last_ping(&event.peer, EpochTime::now());
                    },
                }
            },
            _ => {},
        };
    }

    async fn handle_catch_up_sync_request(
        &mut self,
        channel: ResponseChannel<Result<CatchUpSyncResponse, String>>,
        request: CatchUpSyncRequest,
    ) {
        let our_peer_id = *self.swarm.local_peer_id();
        info!(target: SYNC_REQUEST_LOG_TARGET, "Received catch up sync request from {} for chain {}", our_peer_id, request.algo());
        let algo = request.algo();
        let share_chare = match request.algo() {
            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
        };

        let (blocks, our_tip, our_achieved_pow) = match share_chare
            .request_sync(
                request.i_have(),
                MAX_CATCH_UP_BLOCKS_TO_RETURN,
                request.last_block_received(),
            )
            .await
        {
            Ok((blocks, our_tip, our_achieved_pow)) => {
                (blocks, our_tip.unwrap_or_default(), our_achieved_pow.as_u128())
            },
            Err(error) => {
                error!(target: LOG_TARGET, "Failed to get blocks, tip and pow from share chain: {error:?}");
                if self
                    .swarm
                    .behaviour_mut()
                    .catch_up_sync
                    .send_response(
                        channel,
                        Err("Failed to get blocks, tip and pow from share chain".to_string()),
                    )
                    .is_err()
                {
                    error!(target: LOG_TARGET, "Failed to send block sync response");
                }
                return;
            },
        };

        if self
            .swarm
            .behaviour_mut()
            .catch_up_sync
            .send_response(
                channel,
                Ok(CatchUpSyncResponse::new(
                    algo,
                    our_peer_id,
                    &blocks,
                    our_tip,
                    our_achieved_pow,
                )),
            )
            .is_err()
        {
            error!(target: LOG_TARGET, "Failed to send block sync response");
        }
    }

    fn release_catchup_sync_permit(&mut self, request_id: OutboundRequestId) -> Option<OwnedSemaphorePermit> {
        let should_remove: Vec<PeerId> = self
            .randomx_in_progress_syncs
            .iter()
            .filter_map(|(peer, r)| if r.0 == request_id { Some(*peer) } else { None })
            .collect();
        for peer in should_remove {
            self.last_failed_randomx_peer = None;
            if let Some((_r, permit)) = self.randomx_in_progress_syncs.remove(&peer) {
                return Some(permit);
            }
        }

        let should_remove: Vec<PeerId> = self
            .sha3x_in_progress_syncs
            .iter()
            .filter_map(|(peer, r)| if r.0 == request_id { Some(*peer) } else { None })
            .collect();
        for peer in should_remove {
            self.last_failed_sha3x_peer = None;
            if let Some((_r, permit)) = self.sha3x_in_progress_syncs.remove(&peer) {
                return Some(permit);
            }
        }
        None
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_catch_up_sync_response(
        &mut self,
        response: CatchUpSyncResponse,
        permit: Option<OwnedSemaphorePermit>,
    ) {
        debug!(target: MESSAGE_LOGGING_LOG_TARGET, "Catch up sync response: {response}");
        if response.version != PROTOCOL_VERSION {
            trace!(target: LOG_TARGET, "Peer {} has an outdated version, skipping", response.peer_id());
            return;
        }
        let peer = *response.peer_id();

        let timer = Instant::now();
        let algo = response.algo();
        let share_chain = match algo {
            PowAlgorithm::RandomX => {
                self.randomx_last_sync_requested_block = None;
                self.share_chain_random_x.clone()
            },
            PowAlgorithm::Sha3x => {
                self.sha3x_last_sync_requested_block = None;
                self.share_chain_sha3x.clone()
            },
        };
        let their_tip_hash = *response.tip_hash();
        let their_height = response.tip_height();
        let their_pow = response.achieved_pow();
        let mut blocks: Vec<_> = response.into_blocks().into_iter().collect();
        if blocks.is_empty() {
            warn!(target: SYNC_REQUEST_LOG_TARGET, "Peer {} sent 0 blocks for catch up sync", peer);
            let _res = self.swarm.disconnect_peer_id(peer);
            self.network_peer_store
                .write()
                .await
                .move_to_blacklist(&peer, "Peer sent 0 blocks for catch up sync".to_string());
            return;
        }
        info!(target: SYNC_REQUEST_LOG_TARGET, "Received catch up sync response for chain {} from {} with blocks {}. Their tip: {}:{}", algo,  peer, blocks.iter().map(|a| a.height.to_string()).join(", "), their_height, &their_tip_hash.to_hex()[0..8]);
        let tx = self.inner_request_tx.clone();
        let network_peer_store = self.network_peer_store.clone();
        let recent_synced_tips = self.recent_synced_tips.get(&algo).cloned().unwrap();
        // self.print_debug_chain_graph().await;
        let synced_bool = match algo {
            PowAlgorithm::RandomX => self.are_we_synced_with_randomx_p2pool.clone(),

            PowAlgorithm::Sha3x => self.are_we_synced_with_sha3x_p2pool.clone(),
        };

        tokio::spawn(async move {
            let feedback_string = blocks
                .iter()
                .map(|a| {
                    format!(
                        "{}({:x}{:x}{:x}{:x})",
                        a.height, a.hash[0], a.hash[1], a.hash[2], a.hash[3]
                    )
                })
                .collect::<Vec<String>>();
            // Check if we have received their tip
            let mut must_continue_sync = true;
            if blocks.iter().any(|b| b.hash == their_tip_hash) {
                info!(target: SYNC_REQUEST_LOG_TARGET, "Catch up sync completed for chain {} from {}", algo, peer);
                must_continue_sync = false;
            }

            blocks.sort_by(|a, b| a.height.cmp(&b.height));
            let last_block_from_them = blocks.last().map(|b| (b.height, b.hash));
            // let mut new_tip = ChainAddResult::default();
            // let mut blocks_added = Vec::new();
            for b in blocks {
                let message = InnerRequest::AddSyncedBlock {
                    algo,
                    block: Arc::new(b),
                    source_peer: peer,
                    depth: 0,
                };
                let _unused = tx.send(message);
                // match share_chain.add_synced_blocks(&[b.clone()]).await {
                //     Ok(result) => {
                //         blocks_added.push(format!("{}({})", b.height, &b.hash.to_hex()[0..8]));
                //         new_tip.combine(result);
                //     },
                //     Err(error) => {
                //         error!(target: SYNC_REQUEST_LOG_TARGET, squad; "Failed to add Catchup synced blocks to share
                // chain: {error:?}");         network_peer_store
                //             .write()
                //             .await
                //             .move_to_grey_list(peer, format!("Block failed validation: {error}"));
                //         return;
                //     },
                // }
            }
            {
                if let Some(ref last_block) = last_block_from_them {
                    let mut lock = recent_synced_tips.write().await;
                    lock.put(peer, *last_block);
                }
            }

            info!(target: LOG_TARGET, "[{:?}] Blocks via catchup sync added {:?}", algo, feedback_string);
            // info!(target: LOG_TARGET, "[{:?}] Blocks via catchup sync result {}", algo, new_tip);
            // let missing_parents = new_tip.into_missing_parents_vec();
            // if !missing_parents.is_empty() {
            //     let sync_share_chain = SyncMissingBlocks {
            //         algo,
            //         peer,
            //         missing_parents,
            //         is_from_new_block_notify: false,
            //         depth: 0,
            //     };
            //     let _unused = tx.send(InnerRequest::DoSyncMissingBlocks(sync_share_chain));
            // }

            // info!(target: SYNC_REQUEST_LOG_TARGET, squad = &squad; "Synced blocks added to share chain");
            let our_pow = share_chain.get_total_chain_pow().await;
            must_continue_sync = must_continue_sync && (their_pow > our_pow.as_u128());
            info!(target: SYNC_REQUEST_LOG_TARGET, "[{:?}] must continue: {}", algo, must_continue_sync);

            let mut peer_store_write_lock = network_peer_store.write().await;
            let num_catchups = peer_store_write_lock.num_catch_ups(&peer);
            info!(target: SYNC_REQUEST_LOG_TARGET, "[{:?}] num catchups: {:?}", algo, num_catchups);
            if must_continue_sync && num_catchups.map(|n| n < MAX_CATCH_UP_ATTEMPTS).unwrap_or(true) {
                info!(target: SYNC_REQUEST_LOG_TARGET, "Continue catch up for {}", peer);
                peer_store_write_lock.add_catch_up_attempt(&peer);
                synced_bool.store(false, std::sync::atomic::Ordering::SeqCst);
                let perform_catch_up_sync = PerformCatchUpSync {
                    algo,
                    peer,
                    last_block_from_them,
                    their_height,
                    // their_pow,
                    permit,
                };
                let _unused = tx.send(InnerRequest::PerformCatchUpSync(perform_catch_up_sync));
            } else {
                info!(target: SYNC_REQUEST_LOG_TARGET, "Catch up sync completed for chain {} from {} after {} catchups", algo, peer, num_catchups.map(|s| s.to_string()).unwrap_or_else(|| "None".to_string()));

                peer_store_write_lock.reset_catch_up_attempts(&peer);
            }
            if timer.elapsed() > MAX_ACCEPTABLE_P2P_MESSAGE_TIMEOUT {
                warn!(target: LOG_TARGET, "Catch up sync response took too long: {:?}", timer.elapsed());
            }
        });
    }

    async fn handle_peer_identified(&mut self, peer_id: PeerId, info: Info) {
        if *self.swarm.local_peer_id() == peer_id {
            warn!(target: LOG_TARGET, "Dialled ourselves");
            return;
        }

        // if self.swarm.external_addresses().count() > 0 {
        //     debug!(target: LOG_TARGET, "No need to relay, we have an external address already. {}",
        // self.swarm.external_addresses().map(|a| a.to_string()).collect::<Vec<String>>().join(", "));     // Check
        // if we can relay     // warn!(target: LOG_TARGET, "No external addresses");
        //     // self.swarm.add_external_address(info.observed_addr.clone());
        //     // info!(target: LOG_TARGET, "We have an external address already, no need to relay.");
        //     return;
        // }

        if self.config.is_seed_peer {
            return;
        }

        let is_relay = info.protocols.iter().any(|p| *p == relay::HOP_PROTOCOL_NAME);

        // Do not disconnect from relays
        if !info
            .protocols
            .iter()
            .any(|p| *p == CATCH_UP_SYNC_REQUEST_RESPONSE_PROTOCOL)
        {
            warn!(target: LOG_TARGET, "Peer does not support current catchup sync protocol, will disconnect");
            let _res = self.swarm.disconnect_peer_id(peer_id);

            // return;
        }

        let is_seed_peer = self.network_peer_store.read().await.is_seed_peer(&peer_id);

        // adding peer to kademlia and gossipsub
        for addr in info.listen_addrs {
            // self.swarm.behaviour_mut().kademlia.add_address(&peer_id, addr.clone());
            if Self::is_p2p_address_without_circuit(&addr)
                // && addr.is_global_ip()
                && is_relay
                && !is_seed_peer
            {
                let mut lock = self.relay_store.write().await;
                if lock.add_possible_relay(peer_id, addr.clone()) {
                    info!(target: LOG_TARGET, "Added possible relay: {peer_id:?} -> {addr:?}");
                }
            }
        }
        // }
    }

    #[allow(clippy::too_many_lines)]
    async fn perform_catch_up_sync(&mut self, perform_catch_up_sync: PerformCatchUpSync) -> Result<(), Error> {
        let PerformCatchUpSync {
            algo,
            peer,
            last_block_from_them,
            their_height,
            // their_pow: _,
            mut permit,
        } = perform_catch_up_sync;

        // Check if we have blacklisted them.
        {
            if self.network_peer_store.read().await.is_blacklisted(&peer) {
                warn!(target: SYNC_REQUEST_LOG_TARGET, "Peer {} is blacklisted, not syncing", peer);
                return Ok(());
            }
            if self.network_peer_store.read().await.is_greylisted(&peer) {
                warn!(target: SYNC_REQUEST_LOG_TARGET, "Peer {} is greylisted, not syncing", peer);
                return Ok(());
            }
        }

        // First check if we have a sync in progress for this peer.

        let (share_chain, semaphore, in_progress_syncs, last_progress) = match algo {
            PowAlgorithm::RandomX => {
                if let Some(last_sync_peer) = self.last_failed_randomx_peer {
                    if last_sync_peer == peer {
                        warn!(target: SYNC_REQUEST_LOG_TARGET, "Peer {} is the last failed sync peer, not syncing this round", peer);
                        self.last_failed_randomx_peer = None;
                        return Ok(());
                    }
                }
                (
                    self.share_chain_random_x.clone(),
                    self.randomx_sync_semaphore.clone(),
                    &mut self.randomx_in_progress_syncs,
                    self.randomx_last_sync_requested_block.take(),
                )
            },
            PowAlgorithm::Sha3x => {
                if let Some(last_sync_peer) = self.last_failed_sha3x_peer {
                    if last_sync_peer == peer {
                        warn!(target: SYNC_REQUEST_LOG_TARGET, "Peer {} is the last failed sync peer, not syncing this round", peer);
                        self.last_failed_sha3x_peer = None;
                        return Ok(());
                    }
                }
                (
                    self.share_chain_sha3x.clone(),
                    self.sha3x_sync_semaphore.clone(),
                    &mut self.sha3x_in_progress_syncs,
                    self.sha3x_last_sync_requested_block.take(),
                )
            },
        };
        // let our_pow = share_chain.get_total_chain_pow().await;

        if in_progress_syncs.contains_key(&peer) {
            warn!(target: SYNC_REQUEST_LOG_TARGET, "Already in progress with sync from {}", peer);
            return Ok(());
        }

        if permit.is_none() {
            permit = match semaphore.try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    warn!(target: SYNC_REQUEST_LOG_TARGET, "Could not acquire semaphore for catch up sync for peer: {}", peer);
                    return Ok(());
                },
            };
        } else {
            // Check to see if there is a better peer and release the permit if that is the case
            // {
            //     let peer_store_lock = self.network_peer_store.read().await;
            //     // this only gets called after sync completes, lets set synced status = true
            //     let (_max_known_network_height, max_known_network_pow, peer_with_best) =
            //         peer_store_lock.max_known_network_height(algo);

            //     if let Some(peer_with_best) = peer_with_best {
            //         if their_pow < max_known_network_pow {
            //             warn!(target: SYNC_REQUEST_LOG_TARGET, "There is a peer with better POW than {}, so not
            // trying to sync with them. Best peer is currently:{}", peer, peer_with_best);
            //             return Ok(());
            //         }
            //     }
            // }
        }

        let permit = permit.unwrap();
        let (mut i_have_blocks, last_block_from_them) = match (last_block_from_them, last_progress) {
            (None, Some(last_progress)) => {
                // this is most likely a new catchup sync request, while the previous attempt failed so we ask with both
                // I have blocks and last block
                let i_have = share_chain
                    .create_catchup_sync_blocks(CATCH_UP_SYNC_BLOCKS_IN_I_HAVE)
                    .await;
                (i_have, Some(last_progress))
            },
            (None, None) => {
                // new catchup sync request, no previous attempt
                let i_have = share_chain
                    .create_catchup_sync_blocks(CATCH_UP_SYNC_BLOCKS_IN_I_HAVE)
                    .await;
                (i_have, None)
            },
            (Some(last_block_from_them), None) => {
                // This is a continuation of catchup sync, we dont need to return any I have blocks
                (vec![], Some(last_block_from_them))
            },
            (Some(last_block_from_them), Some(_last_progress)) => {
                // This should not happen, but as a fail safe we will return the last block we requested not
                // last_progress
                (vec![], Some(last_block_from_them))
            },
        };

        {
            let lock = self.recent_synced_tips.get(&algo).unwrap().read().await;
            for item in lock.iter() {
                i_have_blocks.insert(0, *item.1);
            }
        }

        info!(target: SYNC_REQUEST_LOG_TARGET, "[{:?}] Sending catch up sync to {} for blocks {}, last block received {}. Their height:{}",
                algo,
            peer, i_have_blocks.iter().map(|a| a.0.to_string()).join(", "), last_block_from_them.map(|a| a.0.to_string()).unwrap_or_else(|| "None".to_string()), their_height);

        // lets save the last attempted sync last block
        match algo {
            PowAlgorithm::RandomX => {
                self.randomx_last_sync_requested_block = last_block_from_them;
            },
            PowAlgorithm::Sha3x => {
                self.sha3x_last_sync_requested_block = last_block_from_them;
            },
        }

        // get the sync bool
        let sync_status = match algo {
            PowAlgorithm::RandomX => self.are_we_synced_with_randomx_p2pool.clone(),
            PowAlgorithm::Sha3x => self.are_we_synced_with_sha3x_p2pool.clone(),
        };
        let our_tip = share_chain.get_tip().await?.unwrap_or_default();
        if our_tip.0 < their_height.saturating_sub(10) {
            info!(target: SYNC_REQUEST_LOG_TARGET, "We({}) are out by more than 10 blocks from syncing peer({}), setting sync status to false", our_tip.0, their_height);
            sync_status.store(false, std::sync::atomic::Ordering::SeqCst);
        }

        let outbound_request_id = self.swarm.behaviour_mut().catch_up_sync.send_request(
            &peer,
            CatchUpSyncRequest::new(algo, i_have_blocks, last_block_from_them),
        );
        in_progress_syncs.insert(peer, (outbound_request_id, permit));
        self.network_peer_store
            .write()
            .await
            .update_last_sync_attempt(peer, algo);

        Ok(())
    }

    fn is_p2p_address_without_circuit(address: &Multiaddr) -> bool {
        address.iter().any(|p| matches!(p, Protocol::P2p(_))) &&
            !address.iter().any(|p| matches!(p, Protocol::P2pCircuit))
    }

    async fn handle_autonat_event(&mut self, event: autonat::Event) {
        match event {
            autonat::Event::StatusChanged { old: _old, new } => match new {
                NatStatus::Public(public_address) => {
                    info!(target: LOG_TARGET, "[AUTONAT]: Our public address is {public_address}");
                    // self.swarm
                    //     .behaviour_mut()
                    //     .relay_server
                    //     .set_public_address(public_address.clone());
                    self.is_private_nat = false;
                },
                NatStatus::Private => {
                    warn!(target: LOG_TARGET, "[AUTONAT]: We are behind a NAT, connecting to relay!");
                    // let lock = self.relay_store.read().await;
                    // if !lock.has_active_relay() {
                    // drop(lock);
                    self.is_private_nat = true;
                    // }
                },
                _ => {
                    debug!(target: LOG_TARGET, "[AUTONAT] Ignoring unknown status {new:?}");
                },
            },
            autonat::Event::OutboundProbe(probe_event) => match probe_event {
                OutboundProbeEvent::Error { probe_id, peer, error } => {
                    error!(target: LOG_TARGET, "[AUTONAT] Outbound probe error: {probe_id:?} -> {peer:?} -> {error:?}");
                },
                _ => {
                    debug!(target: LOG_TARGET, "[AUTONAT] {probe_event:?}");
                },
            },
            _ => {
                debug!(target: LOG_TARGET, "[AUTONAT] {event:?}");
            },
        }
    }

    async fn attempt_relay_reservation(&mut self, peer_id: PeerId, addresses: &[Multiaddr]) {
        // Can happen that a previous lock already set the relaty
        // if self.swarm.external_addresses().count() > 0 {
        //     warn!(target: LOG_TARGET, "No need to relay, we have an external address or relay already");
        //     return;
        // }

        for addr in addresses {
            if !Self::is_p2p_address_without_circuit(addr) {
                warn!(target: LOG_TARGET, "Attempeted to reserve relay address, but it is not a p2p address without circuit: {:?}", addr);
                continue;
            }
            let mut relay_lock = self.relay_store.write().await;
            relay_lock.add_pending_reservation(peer_id, addr.clone());

            let listen_addr = addr.clone().with(Protocol::P2pCircuit);
            info!(target: LOG_TARGET, "Try to listen on {:?}...", listen_addr);
            match self.swarm.listen_on(listen_addr.clone()) {
                Ok(_) => {
                    // The relay is not confirmed yet, we still need to negotiate the relay
                    // info!(target: LOG_TARGET, "Listening on {listen_addr:?}");
                    // relay.is_circuit_established = true;
                    break;
                },
                Err(error) => {
                    warn!(target: LOG_TARGET, "Failed to listen on relay address ({:?}): {:?}", listen_addr, error);
                },
            }
        }
    }

    async fn handle_query(&mut self, query: P2pServiceQuery) {
        match query {
            P2pServiceQuery::ConnectionInfo(reply) => {
                let connection_info = self.get_libp2p_connection_info();
                let _unused = reply.send(connection_info);
            },

            P2pServiceQuery::GetPeers(reply) => {
                let store_read_lock = self.network_peer_store.read().await;
                let connected_peers = store_read_lock.whitelist_peers();
                let mut white_list_res = vec![];
                for (p, info) in connected_peers {
                    white_list_res.push(ConnectedPeerInfo {
                        peer_id: p.to_string(),
                        peer_info: Some(info.peer_info.clone()),
                        last_grey_list_reason: info.last_grey_list_reason.clone(),
                        last_ping: info.last_ping,
                    });
                }
                let grey_list_peers = store_read_lock.greylist_peers();
                let mut grey_list_res = vec![];
                for (p, info) in grey_list_peers {
                    grey_list_res.push(ConnectedPeerInfo {
                        peer_id: p.to_string(),
                        peer_info: Some(info.peer_info.clone()),
                        last_grey_list_reason: info.last_grey_list_reason.clone(),
                        last_ping: info.last_ping,
                    });
                }
                let _unused = reply.send((white_list_res, grey_list_res));
            },
            P2pServiceQuery::GetConnections(reply) => {
                let connected_peers = self.swarm.connected_peers();
                let mut res = vec![];
                let store_read_lock = self.network_peer_store.read().await;
                for p in connected_peers {
                    let peer_info = store_read_lock.get(p).cloned();
                    if let Some(peer_info) = peer_info {
                        let p = ConnectedPeerInfo {
                            peer_id: p.to_string(),
                            peer_info: Some(peer_info.peer_info),
                            last_grey_list_reason: peer_info.last_grey_list_reason,
                            last_ping: peer_info.last_ping,
                        };
                        res.push(p);
                    } else {
                        res.push(ConnectedPeerInfo {
                            peer_id: p.to_string(),
                            peer_info: None,
                            last_grey_list_reason: None,
                            last_ping: None,
                        });
                    }
                }
                let _unused = reply.send(res);
            },
            P2pServiceQuery::GetChain {
                pow_algo,
                height,
                count,
                response,
            } => {
                let share_chain = match pow_algo {
                    PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
                    PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
                };
                let blocks = match share_chain.all_blocks(Some(height), count, true).await {
                    Ok(blocks) => blocks,
                    Err(e) => {
                        error!(target: LOG_TARGET, "Failed to get blocks from height: {e:?}");
                        vec![]
                    },
                };
                let _unused = response.send(blocks);
            },
        }
    }

    fn get_libp2p_connection_info(&mut self) -> ConnectionInfo {
        let network_info = self.swarm.network_info();
        let connection_counters = network_info.connection_counters();
        let connection_info = ConnectionInfo {
            listener_addresses: self.swarm.external_addresses().cloned().collect(),
            connected_peers: self.swarm.connected_peers().count(),
            network_info: NetworkInfo {
                num_peers: network_info.num_peers(),
                connection_counters: ConnectionCounters {
                    pending_incoming: connection_counters.num_pending_incoming(),
                    pending_outgoing: connection_counters.num_pending_outgoing(),
                    established_incoming: connection_counters.num_established_incoming(),
                    established_outgoing: connection_counters.num_established_outgoing(),
                },
            },
        };
        connection_info
    }

    async fn handle_inner_request(&mut self, req: InnerRequest) {
        match req {
            InnerRequest::DoSyncMissingBlocks(sync_chain) => {
                self.sync_missing_blocks(sync_chain).await;
            },
            InnerRequest::PerformCatchUpSync(perform_catch_up_sync) => {
                if let Err(e) = self.perform_catch_up_sync(perform_catch_up_sync).await {
                    error!(target: LOG_TARGET, "Failed to perform catch up sync: {e:?}");
                }
            },
            InnerRequest::AddSyncedBlock {
                algo,
                block,
                source_peer,
                depth,
            } => {
                let timer = Instant::now();
                // First check if the peer is blacklisted
                {
                    if self.network_peer_store.read().await.is_blacklisted(&source_peer) {
                        warn!(target: SYNC_REQUEST_LOG_TARGET, "Peer {} is blacklisted, not syncing", source_peer);
                        return;
                    }
                }
                if timer.elapsed().as_millis() > 100 {
                    warn!(target: LOG_TARGET, "Adding block took too long. Step 1: {:?}", timer.elapsed());
                }
                let (share_chain, synced_bool) = match algo {
                    PowAlgorithm::RandomX => (
                        self.share_chain_random_x.clone(),
                        self.are_we_synced_with_randomx_p2pool.clone(),
                    ),
                    PowAlgorithm::Sha3x => (
                        self.share_chain_sha3x.clone(),
                        self.are_we_synced_with_sha3x_p2pool.clone(),
                    ),
                };
                info!(target: SYNC_REQUEST_LOG_TARGET, "Adding block {}({:x}{:x}{:x}{:x}) to share chain from peer {}", block.height, block.hash[0], block.hash[1], block.hash[2], block.hash[3], source_peer);
                match share_chain.add_synced_blocks(vec![block]).await {
                    Ok(result) => {
                        if timer.elapsed().as_millis() > 100 {
                            warn!(target: LOG_TARGET, "Adding block took too long. Step 2: {:?}", timer.elapsed());
                        }

                        info!(target: LOG_TARGET, "[{:?}] Blocks via catchup sync result {}", algo, result);
                        let missing_parents = result.into_missing_parents_vec();
                        if !missing_parents.is_empty() && depth < self.config.max_missing_blocks_sync_depth {
                            let sync_share_chain = SyncMissingBlocks {
                                algo,
                                peer: source_peer,
                                missing_parents,
                                is_from_new_block_notify: false,
                                depth: depth + 1,
                            };
                            let tx = self.inner_request_tx.clone();
                            let _unused = tx.send(InnerRequest::DoSyncMissingBlocks(sync_share_chain));
                        }
                        let our_pow = share_chain.get_total_chain_pow().await;
                        if timer.elapsed().as_millis() > 100 {
                            warn!(target: LOG_TARGET, "Adding block took too long. Step 3: {:?}", timer.elapsed());
                        }

                        let peer_store_lock = self.network_peer_store.read().await;
                        if timer.elapsed().as_millis() > 100 {
                            warn!(target: LOG_TARGET, "Adding block took too long. Step 4: {:?}", timer.elapsed());
                        }

                        // this only gets called after sync completes, lets set synced status = true
                        let (max_known_network_height, max_known_network_pow, peer_with_best) =
                            peer_store_lock.max_known_network_height(algo);

                        if our_pow.as_u128() >= max_known_network_pow {
                            info!(target: SYNC_REQUEST_LOG_TARGET, "[{}] our pow is greater than max known network pow, we are now synced", algo);
                            synced_bool.store(true, std::sync::atomic::Ordering::SeqCst);
                        } else {
                            info!(target: SYNC_REQUEST_LOG_TARGET, "[{}] our pow is less than max known network pow, we are not synced, will continue to search for better POW. Best peer is {} at height {}", algo, peer_with_best.map(|p| p.to_base58()).unwrap_or_else(|| "None".to_string()), max_known_network_height);
                        }
                    },
                    Err(error) => {
                        error!(target: SYNC_REQUEST_LOG_TARGET, "Failed to add Catchup synced blocks from {source_peer:?} to share chain: {error:?}");
                        let _unused = self.swarm.disconnect_peer_id(source_peer);
                        self.network_peer_store
                            .write()
                            .await
                            .move_to_blacklist(&source_peer, format!("Block failed validation: {error}"));
                    },
                }
            },
        }
    }

    /// Main loop of the service that drives the events and libp2p swarm forward.
    #[allow(clippy::too_many_lines)]
    async fn main_loop(&mut self) -> Result<(), Error> {
        let mut publish_peer_info_interval = tokio::time::interval_at(
            tokio::time::Instant::now() + self.config.peer_info_publish_interval,
            self.config.peer_info_publish_interval,
        );
        publish_peer_info_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut grey_list_clear_interval = tokio::time::interval(self.config.grey_list_clear_interval);
        grey_list_clear_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut black_list_clear_interval = tokio::time::interval(self.config.black_list_clear_interval);
        black_list_clear_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut meta_data_exchange_interval = tokio::time::interval(self.config.meta_data_exchange_interval);
        meta_data_exchange_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut peer_exchange_interval = tokio::time::interval(self.config.peer_exchange_interval);
        peer_exchange_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut connection_stats_publish = tokio::time::interval(Duration::from_secs(10));
        connection_stats_publish.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut seek_connections_interval = tokio::time::interval(Duration::from_secs(20));
        seek_connections_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut connection_churn_interval = tokio::time::interval(Duration::from_secs(CONNECTION_CHURN_DELAY_SECS));
        connection_churn_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut relay_job_interval = tokio::time::interval(Duration::from_secs(30));
        relay_job_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut debug_chain_graph = if self.config.debug_print_chain {
            tokio::time::interval(Duration::from_secs(60))
        } else {
            // only once a day, but even then will be skipped
            tokio::time::interval(Duration::from_secs(60 * 60 * 24))
        };
        debug_chain_graph.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let shutdown_signal = self.shutdown.to_signal().clone();
        tokio::pin!(shutdown_signal);
        tokio::pin!(grey_list_clear_interval);
        tokio::pin!(black_list_clear_interval);
        tokio::pin!(meta_data_exchange_interval);
        tokio::pin!(peer_exchange_interval);
        tokio::pin!(connection_stats_publish);
        tokio::pin!(seek_connections_interval);
        tokio::pin!(connection_churn_interval);
        tokio::pin!(relay_job_interval);

        loop {
            select! {
                // biased;
                _ = &mut shutdown_signal => {
                    info!(target: LOG_TARGET,"Shutting down p2p service...");
                    return Ok(());
                }
                req = self.query_rx.recv() => {
                    let timer = Instant::now();
                    match req {
                        Some(req) => self.handle_query(req).await,
                        None => warn!(target: LOG_TARGET, "Failed to receive query from channel. Sender dropped?"),
                    }
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Query handling took too long: {:?}", timer.elapsed());
                    }
                },
                _ = relay_job_interval.tick() => {

                    let timer = Instant::now();

                    info!(target: LOG_TARGET, "Running relay job");
                    let relay_store = self.relay_store.read().await;
                    let expiring = relay_store.get_expiring_reservations(Duration::from_secs(60));
                    info!(target: LOG_TARGET, "Number of expiring relay reservations: {}", expiring.len());
                    drop(relay_store);
                    for (peer_id, address) in expiring {
                        info!(target: LOG_TARGET, "Expiring relay reservation for peer {} at address {}, attempting to renew", peer_id, address);
                        self.attempt_relay_reservation(peer_id, &[address]).await;
                    }

                    if self.swarm.external_addresses().count() == 0 && self.is_private_nat {
                        info!(target: LOG_TARGET, "No external addresses, attempting to reserve relay...");
                        let relay_store = self.relay_store.read().await;
                        let addresses = relay_store.get_potential_relays().into_iter().take(3);
                        drop(relay_store);
                        for (peer_id, address) in addresses {
                            self.attempt_relay_reservation(peer_id, &[address]).await;
                        }
                    }
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Relay job took too long: {:?}", timer.elapsed());
                    }
                },
                _ = connection_churn_interval.tick() => {
                    let timer = Instant::now();
                    if !self.config.is_seed_peer {

                        let store_read_lock = self.network_peer_store.read().await;

                        let mut squad_peers = Vec::new();
                        let mut non_squad_peers = Vec::new();



                        for peer in self.swarm.connected_peers(){
                            let peer_type = store_read_lock.peer_type(peer);
                                match peer_type{
                                    Some(AddPeerStatus::Greylisted) | Some(AddPeerStatus::NewPeer) | Some(AddPeerStatus::Existing) => {
                                        if let Some(record) = store_read_lock.get(peer){
                                            squad_peers.push(record.clone())
                                        }
                                    },
                                    // Assume None is non-squad.
                                    _ => non_squad_peers.push(*peer),
                                }
                        }
                        drop(store_read_lock);

                        // Relays can be non-squad
                        let relays = self.relay_store.read().await.get_relay_peer_ids();
                        let mut num_non_squads = 0;
                        for peer_id in &non_squad_peers {
                            num_non_squads += 1;
                            // let peer_id = non_squad_peers.pop().expect("should be able to pop peer");
                            if relays.contains(peer_id) {
                                debug!(target: LOG_TARGET, "Non-squad peer is a relay, not disconnecting: {}", peer_id);
                                continue;
                            }
                            if num_non_squads > 1 {
                            debug!(target: LOG_TARGET, "Disconnecting non squad peer due to churn: {}", peer_id);
                            let _ = self.swarm.disconnect_peer_id(*peer_id);
                            }
                        }


                        // lets trim some connection, this will bring the connections down to a min of 5
                        // We remove the worst sha and rx chains here
                        while squad_peers.len() > MAX_OUTBOUND_SQUAD_PEERS.saturating_sub(2){
                            // we remove one sha3 and one rx
                            squad_peers.sort_by(|a, b| {
                                b.peer_info.current_sha3x_pow.cmp(&a.peer_info.current_sha3x_pow)
                            });
                            let peer = squad_peers.remove(squad_peers.len()-1);

                            debug!(target: LOG_TARGET, "Disconnecting non squad peer due to churn: {}", peer.peer_id);
                            let _ = self.swarm.disconnect_peer_id(peer.peer_id);
                            squad_peers.sort_by(|a, b| {
                                b.peer_info.current_random_x_pow.cmp(&a.peer_info.current_random_x_pow)
                            });
                            let peer = squad_peers.remove(squad_peers.len()-1);
                            debug!(target: LOG_TARGET, "Disconnecting non squad peer due to churn: {}", peer.peer_id);
                            let _ = self.swarm.disconnect_peer_id(peer.peer_id);
                        }
                    }
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Seeking connections took too long: {:?}", timer.elapsed());
                    }
                },
                _ = seek_connections_interval.tick() => {
                    let timer = Instant::now();
                    if !self.config.is_seed_peer {


                        {
                            let mesh = self.swarm.behaviour().gossipsub.all_mesh_peers();
                            info!(target: LOG_TARGET, "Mesh peers: {:?}", mesh.map(|p| p.to_base58()).collect::<Vec<String>>().join(", "));
                            let all_peers = self.swarm.behaviour().gossipsub.all_peers();
                            info!(target: LOG_TARGET, "All peers: {:?}", all_peers.map(|(p, topic)| format!("{}-{}", p.to_base58(), topic.iter().join("|"))).collect::<Vec<String>>().join(", "));
                        }

                        let mut store_write_lock = self.network_peer_store.write().await;

                        let mut squad_peers = Vec::new();
                        let mut non_squad_peers = Vec::new();
                        // We try and go for 10 outbound connections
                        // 6 are for good squad peers we want to use
                        // 2 are for random squad peers so that we dont form islands
                        // 2 are for non squad peers to no form squad islands

                        for peer in self.swarm.connected_peers(){
                            if let Some(peer_type) = store_write_lock.peer_type(peer){
                                match peer_type{
                                    AddPeerStatus::NonSquad => non_squad_peers.push(*peer),
                                    _ => {
                                        if let Some(record) = store_write_lock.get(peer){
                                            squad_peers.push(record.clone())
                                        }
                                    }
                                }
                            }
                        }
                        let mut num_dialed = 0;
                        if non_squad_peers.len() < MAX_OUTBOUND_NON_SQUAD_PEERS{
                            debug!(target: LOG_TARGET, "Less than the required non squad peers, dialing non squad peers");
                            for record in store_write_lock.random_non_squad_peers_to_dial(20){
                                if !self.swarm.is_connected(&record.peer_id) &&
                                !store_write_lock.is_seed_peer(&record.peer_id)
                                {
                                    store_write_lock.update_last_dial_attempt(&record.peer_id);
                                    info!(
                                        target: LOG_TARGET,
                                        "Dialing peer: {:?} with height(rx/sha) {}/{} on {}",
                                        record.peer_id, record.peer_info.current_random_x_height,
                                        record.peer_info.current_sha3x_height,
                                        record.peer_info.public_addresses().iter().map(|a| a.to_string()).collect::<Vec<String>>().join(", ")
                                    );
                                    let addresses = record.peer_info.public_addresses().into_iter().filter(|a| !is_localhost_or_private(a)).collect::<Vec<_>>();
                                    let dial_opts = DialOpts::peer_id(record.peer_id)
                                        .addresses(addresses)
                                        .extend_addresses_through_behaviour()
                                        .build();
                                    let _unused = self.swarm.dial(dial_opts).map_err(|e| {
                                        warn!(target: LOG_TARGET, "Failed to dial peer: {e:?}");
                                    });
                                    num_dialed += 1;
                                    // We can only do 30 connections
                                    // after 30 it starts cancelling dials
                                    if num_dialed >=  MAX_OUTBOUND_NON_SQUAD_PEERS {
                                        break;
                                    }
                                }
                            }
                        }
                        let mut num_dialed = 0;
                        if squad_peers.len() < MAX_OUTBOUND_SQUAD_PEERS {
                            debug!(target: LOG_TARGET, "Less than the required squad peers, dialing squad peers");
                            for record in store_write_lock.best_squad_peers_to_dial(100, self.config.randomx_enabled, self.config.sha3x_enabled) {
                                if !self.swarm.is_connected(&record.peer_id) &&
                                    !store_write_lock.is_seed_peer(&record.peer_id)
                                {
                                    store_write_lock.update_last_dial_attempt(&record.peer_id);
                                    info!(
                                        target: LOG_TARGET,
                                        "Dialing peer: {:?} with height(rx/sha) {}/{} on {}",
                                        record.peer_id, record.peer_info.current_random_x_height,
                                        record.peer_info.current_sha3x_height,
                                        record.peer_info.public_addresses().iter().map(|a| a.to_string()).collect::<Vec<String>>().join(", ")
                                    );
                                    let addresses = record.peer_info.public_addresses().into_iter().filter(|a| !is_localhost_or_private(a)).collect::<Vec<_>>();
                                    let dial_opts = DialOpts::peer_id(record.peer_id)
                                        .addresses(addresses)
                                        .extend_addresses_through_behaviour()
                                        .build();
                                    let _unused = self.swarm.dial(dial_opts).map_err(|e| {
                                        warn!(target: LOG_TARGET, "Failed to dial peer: {e:?}");
                                    });

                                    num_dialed += 1;
                                    // lets go up to 8 squad connections
                                    if num_dialed > MAX_OUTBOUND_SQUAD_PEERS.saturating_sub(squad_peers.len()) {
                                        break;
                                    }
                                }
                            }
                        }
                        drop(store_write_lock);
                    }
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Seeking connections took too long: {:?}", timer.elapsed());
                    }
                },
                inner_req = self.inner_request_rx.recv() => {
                    let timer = Instant::now();
                    let mut request_type = "None";
                    match inner_req {
                        Some(inner_req) => {
                            request_type  = match inner_req {
                                InnerRequest::DoSyncMissingBlocks(_) => "DoSyncMissingBlocks",
                                InnerRequest::PerformCatchUpSync(_) => "PerformCatchUpSync",
                                InnerRequest::AddSyncedBlock { .. } => "AddSyncedBlock",
                            };
                            self.handle_inner_request(inner_req).await;
                        },
                        None => {
                            warn!(target: LOG_TARGET, "Failed to receive inner request from channel. Sender dropped?");
                        }
                    }
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Inner request handling of {} took too long: {:?}", request_type, timer.elapsed());
                    }
                }
                blocks = self.client_broadcast_block_rx.recv() => {
                    let timer = Instant::now();

                    match blocks {
                        Some(res) => {
                            self.broadcast_block(res).await;
                        },
                        None => {
                            warn!(target: LOG_TARGET, "Failed to receive blocks from channel. Sender dropped?");
                        }
                    }
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Client broadcast took too long: {:?}", timer.elapsed());
                    }
                },
                event = self.swarm.select_next_some() => {
                    let timer = Instant::now();
                    self.handle_event(event).await;
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Event handling took too long: {:?}", timer.elapsed());
                    }
                 },
                _ = publish_peer_info_interval.tick() => {
                    let timer = Instant::now();
                    info!(target: LOG_TARGET, "Publishing peer info");

                    // broadcast peer info
                    if let Err(error) = self.broadcast_peer_info().await {
                        warn!(target: LOG_TARGET, "Failed to broadcast peer info: {error}");
                    }

                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Peer info publishing took too long: {:?}", timer.elapsed());
                    }
                },
                _ = meta_data_exchange_interval.tick() =>  {
                    let timer = Instant::now();
                    if !self.config.is_seed_peer && self.config.sync_job_enabled {
                        let mut connected_peers = self.swarm.connected_peers().copied().collect::<Vec::<_>>();
                        let mut rng = thread_rng();
                        connected_peers.shuffle(&mut rng);
                        if let Ok(my_info) = self
             .create_peer_info(self.swarm.external_addresses().cloned().collect())
            .await
            .inspect_err(|error| {
                error!(target: LOG_TARGET, "Failed to create peer info: {error:?}");
            }) {
                        for peer in connected_peers.iter().take(NUM_PEERS_TO_META_DATA_EXCHANGE) {
                            // Update their latest tip.
                            self.initiate_meta_data_exchange(peer, my_info.clone()).await;
                        }

                    }
                }
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Chain height exchange took too long: {:?}", timer.elapsed());
                    }
                },
                _ = peer_exchange_interval.tick() =>  {
                    let timer = Instant::now();
                    if !self.config.is_seed_peer && self.config.sync_job_enabled {
                        let mut connected_peers = self.swarm.connected_peers().copied().collect::<Vec::<_>>();
                        let mut rng = thread_rng();
                        connected_peers.shuffle(&mut rng);
                        for peer in connected_peers.iter().take(NUM_PEERS_TO_PEER_INFO_EXCHANGE) {
                            // Update their latest tip.
                            self.initiate_direct_peer_exchange(peer).await;
                        }
                    }
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Chain height exchange took too long: {:?}", timer.elapsed());
                    }
                },
               _ = grey_list_clear_interval.tick() => {
                    let timer = Instant::now();
                    self.network_peer_store.write().await.clear_grey_list();
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Clearing grey list took too long: {:?}", timer.elapsed());
                    }
                },
                _ = black_list_clear_interval.tick() => {
                    let timer = Instant::now();
                    self.network_peer_store.write().await.clear_black_list();
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Clearing black list took too long: {:?}", timer.elapsed());
                    }
                },
                _ = connection_stats_publish.tick() => {
                    let timer = Instant::now();
                    let connection_info = self.get_libp2p_connection_info();
                    let _unused = self.stats_broadcast_client.send_libp2p_stats(
                        connection_info.network_info.connection_counters.pending_incoming,
                        connection_info.network_info.connection_counters.pending_outgoing,
                        connection_info.network_info.connection_counters.established_incoming,
                        connection_info.network_info.connection_counters.established_outgoing,
                    );
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Publishing connection stats took too long: {:?}", timer.elapsed());
                    }
                },
                _ = debug_chain_graph.tick() => {
                    if self.config.debug_print_chain {
                        self.print_debug_chain_graph().await;
                    }
                },
            }
        }
    }

    async fn get_same_squad_peer_records(&self, filter_peers: &[PeerId], is_relay: bool) -> Vec<PeerStoreRecord> {
        let network_peer_store = self.network_peer_store.read().await;
        let same_squad_peers = network_peer_store.get_known_same_squad_peer_records();

        // Prepare filter sets
        let local_peer_id = *self.swarm.local_peer_id();
        let filter_peers: HashSet<_> = filter_peers
            .iter()
            .copied()
            .chain(std::iter::once(local_peer_id))
            .collect();
        let known_relays: HashSet<_> = self.relay_store.read().await.get_relay_peer_ids().into_iter().collect();

        // Filter peers
        same_squad_peers
            .into_iter()
            .filter(|record| {
                let is_filtered = filter_peers.contains(&record.peer_id);
                let is_known_relay = known_relays.contains(&record.peer_id);
                !is_filtered && (is_relay == is_known_relay)
            })
            .cloned() // Consider avoiding this if possible
            .collect()
    }

    /// Main loop of the diagnostic service that drives the events and libp2p swarm forward.
    #[allow(clippy::too_many_lines)]
    async fn main_loop_diagnostic_mode(&mut self) -> Result<(), Error> {
        #[derive(Debug)]
        enum DiagnosticState {
            ConnectToSeedPeers,
            WaitForSeedPeersDownload,
            ConnectToPrivatePeers,
            WaitForPrivatePeersDownload,
            ConnectToRelayPeers,
            WaitForRelayPeersDownload,
            Done,
        }
        const CONNECT_INITIAL_WAIT: Duration = Duration::from_secs(20);
        const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
        const RESPONSE_TIMEOUT: Duration = Duration::from_secs(150);

        fn success_next_state(state: DiagnosticState) -> (DiagnosticState, Instant) {
            let next = match state {
                DiagnosticState::ConnectToSeedPeers => DiagnosticState::WaitForSeedPeersDownload,
                DiagnosticState::WaitForSeedPeersDownload => DiagnosticState::ConnectToPrivatePeers,
                DiagnosticState::ConnectToPrivatePeers => DiagnosticState::WaitForPrivatePeersDownload,
                DiagnosticState::WaitForPrivatePeersDownload => DiagnosticState::ConnectToRelayPeers,
                DiagnosticState::ConnectToRelayPeers => DiagnosticState::WaitForRelayPeersDownload,
                DiagnosticState::WaitForRelayPeersDownload => DiagnosticState::Done,
                DiagnosticState::Done => DiagnosticState::Done,
            };
            (next, Instant::now())
        }

        fn failed_next_state(state: DiagnosticState) -> (DiagnosticState, Instant) {
            let next = match state {
                DiagnosticState::ConnectToSeedPeers => DiagnosticState::Done,
                DiagnosticState::WaitForSeedPeersDownload => DiagnosticState::Done,
                DiagnosticState::ConnectToPrivatePeers => DiagnosticState::ConnectToRelayPeers,
                DiagnosticState::WaitForPrivatePeersDownload => DiagnosticState::ConnectToRelayPeers,
                DiagnosticState::ConnectToRelayPeers => DiagnosticState::Done,
                DiagnosticState::WaitForRelayPeersDownload => DiagnosticState::Done,
                DiagnosticState::Done => DiagnosticState::Done,
            };
            (next, Instant::now())
        }

        let mut publish_peer_info_interval = tokio::time::interval_at(
            tokio::time::Instant::now() + self.config.peer_info_publish_interval,
            self.config.peer_info_publish_interval,
        );
        publish_peer_info_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut connection_stats_publish = tokio::time::interval(Duration::from_secs(10));
        connection_stats_publish.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut diagnostic_mode_interval = tokio::time::interval(Duration::from_secs(1));
        diagnostic_mode_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let shutdown_signal = self.shutdown.to_signal().clone();
        tokio::pin!(shutdown_signal);
        tokio::pin!(connection_stats_publish);
        tokio::pin!(diagnostic_mode_interval);

        let diagnostics_broadcast_client = self
            .diagnostics_broadcast_client
            .clone()
            .ok_or_else(|| anyhow_error("Diagnostics broadcast client not available in diagnostic mode"))?;
        let diagnostics_receiver_client = self
            .diagnostics_receiver_client
            .clone()
            .ok_or_else(|| anyhow_error("Diagnostics receiver client not available in diagnostic mode"))?;
        let mut diagnostic_state = DiagnosticState::ConnectToSeedPeers;
        let mut start = Instant::now();
        let uptime: Instant = Instant::now();
        let mut dialed_seed_peers = Vec::new();
        let mut dialed_private_peers = Vec::new();
        let mut dialed_relay_peers = Vec::new();
        loop {
            select! {
                // biased;
                _ = &mut shutdown_signal => {
                    info!(target: LOG_TARGET,"[Diagnostics] Shutting down p2p service...");
                    return Ok(());
                }
                _ = diagnostic_mode_interval.tick() => {
                    match diagnostic_state {
                        DiagnosticState::ConnectToSeedPeers => {
                            info!(target: LOG_TARGET, "[Diagnostics] {:?}... ({:.2?})", diagnostic_state, uptime.elapsed());
                            if dialed_seed_peers.is_empty() {
                                let seed_peers = self.parse_seed_peers().await?;
                                self.network_peer_store.write().await.add_seed_peers(seed_peers.keys().copied().collect());
                                for (peer, addr) in &seed_peers {
                                    info!(target: LOG_TARGET, "[Diagnostics] Adding seed peer: {:?} -> {:?}", peer, addr);
                                    self.swarm.add_peer_address(*peer, addr.clone());
                                    if self.swarm.dial(DialOpts::peer_id(*peer).build()).is_ok() {
                                        let _unused = diagnostics_broadcast_client.send_new_peer(*peer, PeerType::SeedPeer);
                                        debug!(
                                            target: LOG_TARGET,
                                            "[Diagnostics] Successfully dialed seed peer '{peer}' at '{addr}'"
                                        );
                                    }
                                    dialed_seed_peers.push(*peer);
                                }
                            }
                            // Verify that we have at least some seed peers connected
                            if let Ok(seeds) = diagnostics_receiver_client.get_connected_peers(PeerType::SeedPeer).await {
                                if start.elapsed() > CONNECT_INITIAL_WAIT && !seeds.is_empty() {
                                    debug!(
                                        target: LOG_TARGET,
                                        "[Diagnostics] {:?} connected to {} seed peers",
                                        diagnostic_state, seeds.len()
                                    );
                                    (diagnostic_state, start) = success_next_state(diagnostic_state);
                                } else if start.elapsed() > CONNECT_TIMEOUT {
                                    warn!(
                                        target: LOG_TARGET,
                                        "[Diagnostics] {:?} failed, no seed peers connected",
                                        diagnostic_state
                                    );
                                    (diagnostic_state, start) = failed_next_state(diagnostic_state);
                                } else {
                                    debug!(
                                        target: LOG_TARGET,
                                        "[Diagnostics] {:?} waiting for seed peers to connect...",
                                        diagnostic_state
                                    );
                                    continue;
                                }
                            }
                        },
                        DiagnosticState::WaitForSeedPeersDownload => {
                            info!(target: LOG_TARGET, "[Diagnostics] {:?}... ({:.2?})", diagnostic_state, uptime.elapsed());
                            let (mut seen_all, mut seen_count) = (true, 0);
                            if let Ok(seeds_data) = diagnostics_receiver_client.get_peer_diagnostic_info(PeerType::SeedPeer).await {
                                seeds_data.iter().for_each(|(_peer, data)| {
                                    if data.response_time.is_none() {
                                        seen_all = false;
                                    } else {
                                        seen_count += 1;
                                    }
                                });
                            }

                            if seen_all {
                                (diagnostic_state, start) = success_next_state(diagnostic_state);
                            } else if start.elapsed() > RESPONSE_TIMEOUT && seen_count > 0 {
                                warn!(
                                    target: LOG_TARGET,
                                    "[Diagnostics] {:?} non-definitive, only {} peers responded.",
                                    diagnostic_state, seen_count
                                );
                                (diagnostic_state, start) = success_next_state(diagnostic_state);
                            } else if start.elapsed() > RESPONSE_TIMEOUT {
                                warn!(
                                    target: LOG_TARGET,
                                    "[Diagnostics] {:?} failed, no seed peers responded, exiting.",
                                    diagnostic_state
                                );
                                (diagnostic_state, start) = failed_next_state(diagnostic_state);
                            } else {
                                // Nothing here
                            }
                        }
                        DiagnosticState::ConnectToPrivatePeers => {
                            info!(target: LOG_TARGET, "[Diagnostics] {:?}... ({:.2?})", diagnostic_state, uptime.elapsed());
                            let peer_records = self.get_same_squad_peer_records(&dialed_seed_peers, false).await;
                            debug!(
                                target: LOG_TARGET,
                                "[Diagnostics] {:?} Found {} private peers to connect to",
                                diagnostic_state,
                                peer_records.len()
                            );
                            // Select some random peers
                            let mut peer_records = peer_records.choose_multiple(&mut thread_rng(), 50).collect_vec();
                            peer_records.retain(|&x| !dialed_private_peers.contains(&x.peer_id));
                            // Dial selected peers
                            for record in &peer_records {
                                let dial_opts = DialOpts::peer_id(record.peer_id).addresses(
                                    record.peer_info.public_addresses().clone()
                                ).extend_addresses_through_behaviour().build();
                                if self.swarm.dial(dial_opts).is_ok() {
                                    let _unused = diagnostics_broadcast_client.send_new_peer(
                                        record.peer_id,
                                        PeerType::PrivatePeer
                                    );
                                    debug!(
                                        target: LOG_TARGET,
                                        "[Diagnostics] Successfully dialed peer '{:?}'",
                                        record.peer_id
                                    );
                                    dialed_private_peers.push(record.peer_id);
                                };
                            }
                            // Verify that we have enough private peers connected
                            if let Ok(privates) = diagnostics_receiver_client.get_connected_peers(PeerType::PrivatePeer).await {
                                if privates.len() >= 10 || start.elapsed() > CONNECT_TIMEOUT && !privates.is_empty() {
                                    debug!(
                                        target: LOG_TARGET,
                                        "[Diagnostics] {:?} connected to {} private peers",
                                        diagnostic_state, privates.len()
                                    );
                                    (diagnostic_state, start) = success_next_state(diagnostic_state);
                                } else if start.elapsed() > CONNECT_TIMEOUT && privates.is_empty() {
                                    warn!(
                                        target: LOG_TARGET,
                                        "[Diagnostics] {:?} failed, no private peers connected",
                                        diagnostic_state
                                    );
                                    (diagnostic_state, start) = failed_next_state(diagnostic_state);
                                } else {
                                    debug!(
                                        target: LOG_TARGET,
                                        "[Diagnostics] {:?} waiting for private peers to connect...",
                                        diagnostic_state
                                    );
                                    continue;
                                }
                            }
                        },
                        DiagnosticState::WaitForPrivatePeersDownload => {
                            info!(target: LOG_TARGET, "[Diagnostics] {:?}... ({:.2?})", diagnostic_state, uptime.elapsed());
                            let mut seen_count = 0;
                            if let Ok(peers_data) = diagnostics_receiver_client.get_peer_diagnostic_info(PeerType::PrivatePeer).await {
                                peers_data.iter().for_each(|(_peer, data)| {
                                    if data.response_time.is_some() {
                                        seen_count += 1;
                                    }
                                });
                            }

                            if seen_count >= 5 {
                                (diagnostic_state, start) = success_next_state(diagnostic_state);
                            } else if start.elapsed() > RESPONSE_TIMEOUT && seen_count > 0 {
                                warn!(
                                    target: LOG_TARGET,
                                    "[Diagnostics] {:?} non-definitive, only {} peers responded.",
                                    diagnostic_state, seen_count
                                );
                                (diagnostic_state, start) = success_next_state(diagnostic_state);
                            } else if start.elapsed() > RESPONSE_TIMEOUT {
                                warn!(
                                    target: LOG_TARGET,
                                    "[Diagnostics] {:?} failed, no peers responded.",
                                    diagnostic_state
                                );
                                (diagnostic_state, start) = failed_next_state(diagnostic_state);
                            } else {
                                // Nothing here
                            }
                        }
                        DiagnosticState::ConnectToRelayPeers => {
                            info!(target: LOG_TARGET, "[Diagnostics] {:?}... ({:.2?})", diagnostic_state, uptime.elapsed());
                            let peer_records = self.get_same_squad_peer_records(&dialed_seed_peers, true).await;
                            debug!(
                                target: LOG_TARGET,
                                "[Diagnostics] {:?} Found {} relay peers to connect to",
                                diagnostic_state,
                                peer_records.len()
                            );
                            // Select some random peers
                            let mut peer_records = peer_records.choose_multiple(&mut thread_rng(), 50).collect_vec();
                            peer_records.retain(|&x| !dialed_relay_peers.contains(&x.peer_id));
                            // Dial selected peers
                            for record in &peer_records {
                                let dial_opts = DialOpts::peer_id(record.peer_id).addresses(
                                    record.peer_info.public_addresses().clone()
                                ).extend_addresses_through_behaviour().build();
                                if self.swarm.dial(dial_opts).is_ok() {
                                    let _unused = diagnostics_broadcast_client.send_new_peer(
                                        record.peer_id,
                                        PeerType::RelayPeer
                                    );
                                    debug!(
                                        target: LOG_TARGET,
                                        "[Diagnostics] Successfully dialed peer '{:?}'",
                                        record.peer_id
                                    );
                                };
                                dialed_relay_peers.push(record.peer_id);
                            }
                            // Verify that we have enough relay peers connected
                            if let Ok(relays) = diagnostics_receiver_client.get_connected_peers(PeerType::RelayPeer).await {
                                if relays.len() >= 10 || start.elapsed() > CONNECT_TIMEOUT && !relays.is_empty()  {
                                    debug!(
                                        target: LOG_TARGET,
                                        "[Diagnostics] {:?} connected to {} relay peers",
                                        diagnostic_state, relays.len()
                                    );
                                    (diagnostic_state, start) = success_next_state(diagnostic_state);
                                } else if start.elapsed() > CONNECT_TIMEOUT && relays.is_empty() {
                                    warn!(
                                        target: LOG_TARGET,
                                        "[Diagnostics] {:?} failed, no relay peers connected",
                                        diagnostic_state
                                    );
                                    (diagnostic_state, start) = failed_next_state(diagnostic_state);
                                } else {
                                    debug!(
                                        target: LOG_TARGET,
                                        "[Diagnostics] {:?} waiting for relay peers to connect...",
                                        diagnostic_state
                                    );
                                    continue;
                                }
                            }
                        },
                        DiagnosticState::WaitForRelayPeersDownload => {
                            info!(target: LOG_TARGET, "[Diagnostics] {:?}... ({:.2?})", diagnostic_state, uptime.elapsed());
                            let mut seen_count  = 0;
                            if let Ok(relays_data) = diagnostics_receiver_client.get_peer_diagnostic_info(PeerType::RelayPeer).await {
                                relays_data.iter().for_each(|(_peer, data)| {
                                    if data.response_time.is_some() {
                                        seen_count += 1;
                                    }
                                });
                            }

                            if seen_count >= 5 {
                                (diagnostic_state, start) = success_next_state(diagnostic_state);
                            } else if start.elapsed() > RESPONSE_TIMEOUT && seen_count > 0 {
                                warn!(
                                    target: LOG_TARGET,
                                    "[Diagnostics] {:?} non-definitive, only {} peers responded.",
                                    diagnostic_state, seen_count
                                );
                                (diagnostic_state, start) = success_next_state(diagnostic_state);
                            } else if start.elapsed() > RESPONSE_TIMEOUT {
                                warn!(
                                    target: LOG_TARGET,
                                    "[Diagnostics] {:?} failed, no peers responded.",
                                    diagnostic_state
                                );
                                (diagnostic_state, start) = failed_next_state(diagnostic_state);
                            } else {
                                // Nothing here
                            }
                        }
                        DiagnosticState::Done => {
                            info!(target: LOG_TARGET, "[Diagnostics] {:?}... ({:.2?})", diagnostic_state, uptime.elapsed());
                            // Print diagnostics to json file
                            let mut seeds_data = diagnostics_receiver_client
                                .get_peer_diagnostic_info(PeerType::SeedPeer)
                                .await
                                .unwrap_or_default()
                                .values()
                                .cloned()
                                .collect::<Vec<DiagnosticPeerInfo>>();
                            seeds_data.sort_by(|a, b| {
                                b.number_of_peers_received
                                    .cmp(&a.number_of_peers_received)
                                    .then_with(|| a.connected_at.cmp(&b.connected_at))
                            });
                            seeds_data = seeds_data.into_iter().take(10).collect::<Vec<_>>();

                            let mut private_peers_data = diagnostics_receiver_client
                                .get_peer_diagnostic_info(PeerType::PrivatePeer)
                                .await
                                .unwrap_or_default()
                                .values()
                                .cloned()
                                .collect::<Vec<DiagnosticPeerInfo>>();
                            private_peers_data.sort_by(|a, b| {
                                b.number_of_peers_received
                                    .cmp(&a.number_of_peers_received)
                                    .then_with(|| a.connected_at.cmp(&b.connected_at))
                            });
                            private_peers_data = private_peers_data.into_iter().take(10).collect::<Vec<_>>();

                            let mut relays_data = diagnostics_receiver_client
                                .get_peer_diagnostic_info(PeerType::RelayPeer)
                                .await
                                .unwrap_or_default()
                                .values()
                                .cloned()
                                .collect::<Vec<DiagnosticPeerInfo>>();
                            relays_data.sort_by(|a, b| {
                                b.number_of_peers_received
                                    .cmp(&a.number_of_peers_received)
                                    .then_with(|| a.connected_at.cmp(&b.connected_at))
                            });
                            relays_data = relays_data.into_iter().take(10).collect::<Vec<_>>();

                            let diagnostics_data = [
                                ("seeds", json!({ "seeds (1st 10)": seeds_data })),
                                ("relays", json!({ "relays (1st 10)": relays_data })),
                                ("private peers", json!({ "private peers (1st 10)": private_peers_data })),
                            ];
                            let local_node = ("local_node", json!(
                                {"local_node":
                                    {
                                        "1. squad": self.squad,
                                        "2. peer_id": self.swarm.local_peer_id(),
                                        "3. public_addresses": self.swarm.external_addresses().collect::<Vec<_>>(),
                                        "4. diagnoatics ended after": format!("{:.2?}", uptime.elapsed()),
                                    }
                                })
                            );
                            let summary = ("summary", json!(
                                {
                                    "summary": {
                                        // Seeds
                                        " 1. Connect to DNS seeds              ":
                                            seeds_data.iter().any(|peer| peer.connected_at.is_some()),
                                        " 2. Number of DNS seeds connected     ":
                                            seeds_data.iter().filter(
                                                |peer| peer.connected_at.is_some()
                                            ).count(),
                                        " 3. Number of DNS seeds responded     ":
                                            seeds_data.iter().filter(
                                                |peer| peer.response_time.is_some()).count(),
                                        " 4. Number of peers from DNS seeds    ":
                                            seeds_data.iter().map(
                                                |peer| peer.number_of_peers_received.unwrap_or_default()
                                            ).sum::<usize>(),
                                        " 5. New peers added from DNS seeds    ":
                                            seeds_data.iter().map(
                                                |peer| peer.number_of_peers_added.unwrap_or_default()
                                            ).sum::<usize>(),
                                        // Relay peers
                                        " 6. Connect to relay peers            ":
                                            relays_data.iter().any(|peer| peer.connected_at.is_some()),
                                        " 7. Number of relay peers connected   ":
                                            relays_data.iter().filter(
                                                |peer| peer.connected_at.is_some()
                                            ).count(),
                                        " 8. Number of relay peers responded   ":
                                            relays_data.iter().filter(
                                                |peer| peer.response_time.is_some()
                                            ).count(),
                                        " 9. Number of peers from relay peers  ":
                                            relays_data.iter().map(
                                                |peer| peer.number_of_peers_received.unwrap_or_default()
                                            ).sum::<usize>(),
                                        "10. New peers added from relay peers  ":
                                            relays_data.iter().map(
                                                |peer| peer.number_of_peers_added.unwrap_or_default()
                                            ).sum::<usize>(),
                                        // Private peers
                                        "11. Connect to private peers          ":
                                            private_peers_data.iter().any(|peer| peer.connected_at.is_some()),
                                        "12. Number of private peers connected ":
                                            private_peers_data.iter().filter(
                                                |peer| peer.connected_at.is_some()
                                            ).count(),
                                        "13. Number of private peers responded ":
                                            private_peers_data.iter().filter(
                                                |peer| peer.response_time.is_some()
                                            ).count(),
                                        "14. Number of peers from private peers":
                                            private_peers_data.iter().map(
                                                |peer| peer.number_of_peers_received.unwrap_or_default()
                                            ).sum::<usize>(),
                                        "15. New peers added from private peers":
                                            private_peers_data.iter().map(
                                                |peer| peer.number_of_peers_added.unwrap_or_default()
                                            ).sum::<usize>(),
                                    }
                                })
                            );

                            let file_path = self.config.diagnostic_mode_file_path.clone().unwrap_or_default().join(
                                "./diagnostic_results.json"
                            );
                            if let Ok(file) = File::create(file_path.clone()) {
                                if let Err(e) = to_writer_pretty(&file, &local_node.1) {
                                    warn!(
                                        target: LOG_TARGET,
                                        "[Diagnostics] Failed to write {} diagnostics to file: {e}",
                                        local_node.0
                                    );
                                }
                                let _unused = writeln!(&file);
                                if let Err(e) = to_writer_pretty(&file, &summary.1) {
                                    warn!(
                                        target: LOG_TARGET,
                                        "[Diagnostics] Failed to write {} diagnostics to file: {e}",
                                        summary.0
                                    );
                                }
                                let _unused = writeln!(&file);
                                for (data_name, diagnostics) in &diagnostics_data {
                                    if let Err(e) = to_writer_pretty(&file, diagnostics) {
                                        warn!(
                                            target: LOG_TARGET,
                                            "[Diagnostics] Failed to write {data_name} diagnostics to file: {e}",
                                        );
                                    }
                                    let _unused = writeln!(&file);
                                }
                            }
                            else {
                                warn!(
                                    target: LOG_TARGET,
                                    "[Diagnostics] Failed to create diagnostics file at path: {:?}",
                                    file_path
                                );
                            }
                            info!(target: LOG_TARGET, "Diagnostics written to file: {}", file_path.display());

                            // Send shutdown signal
                            self.shutdown.trigger();
                        },
                    }
                },
                event = self.swarm.select_next_some() => {
                    let timer = Instant::now();
                    match event {
                        // These events must be ignored in diagnostic mode
                        SwarmEvent::Behaviour(ServerNetworkBehaviourEvent::MetaDataExchange(_) ) |
                        SwarmEvent::Behaviour(ServerNetworkBehaviourEvent::ShareChainSync(_)) |
                        SwarmEvent::Behaviour(ServerNetworkBehaviourEvent::CatchUpSync(_)) |
                        SwarmEvent::Behaviour(ServerNetworkBehaviourEvent::Gossipsub(_)) => {},
                        // Other events must be handled
                        _ => self.handle_event(event).await,
                    }
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "[Diagnostics] Event handling took too long: {:?}", timer.elapsed());
                    }
                 },
                _ = connection_stats_publish.tick() => {
                    let timer = Instant::now();
                    let connection_info = self.get_libp2p_connection_info();
                    let _unused = self.stats_broadcast_client.send_libp2p_stats(
                        connection_info.network_info.connection_counters.pending_incoming,
                        connection_info.network_info.connection_counters.pending_outgoing,
                        connection_info.network_info.connection_counters.established_incoming,
                        connection_info.network_info.connection_counters.established_outgoing,
                    );
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(
                            target: LOG_TARGET,
                            "[Diagnostics] Publishing connection stats took too long: {:?}",
                            timer.elapsed()
                        );
                    }
                },
            }
        }
    }

    async fn print_debug_chain_graph(&self) {
        if self.config.randomx_enabled {
            self.print_debug_chain_graph_inner(&self.share_chain_random_x, "randomx")
                .await;
        }
        if self.config.sha3x_enabled {
            self.print_debug_chain_graph_inner(&self.share_chain_sha3x, "sha3x")
                .await;
        }
    }

    async fn print_debug_chain_graph_inner(&self, chain: &S, prefix: &str) {
        let time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(true)
            .open(format!("{}_blocks_{}.txt", prefix, time))
            .unwrap();

        // file.write(b"@startuml\n").unwrap();
        file.write_all(b"digraph B {\n").unwrap();
        file.write_all(b"node [shape=\"box\"]\n").unwrap();
        if let Some(tip) = chain.get_tip().await.unwrap() {
            file.write_all(&format!("comment=\"{} - {}\"\n", tip.0, &tip.1.to_hex()[0..8]).into_bytes())
                .unwrap();
            file.write_all(format!("B{} [style=\"bold,filled\" penwidth=2]\n", &tip.1.to_hex()[0..8]).as_bytes())
                .unwrap();
        }
        let formatter = human_format::Formatter::new();
        let blocks = chain.all_blocks(None, 10000, false).await.expect("errored");
        for b in blocks {
            file.write_all(
                format!(
                    "B{} [label=\"{} - {} {}{} {}..{}\"]\n",
                    &b.hash.to_hex()[0..8],
                    &b.height,
                    &b.hash.to_hex()[0..8],
                    if b.verified.is_verified() { "" } else { "UNVERIFIED " },
                    formatter.format(b.target_difficulty().as_u64() as f64),
                    &b.miner_wallet_address.to_base58()[0..6],
                    &b.miner_wallet_address.to_base58()[84..]
                )
                .as_bytes(),
            )
            .unwrap();
            file.write_all(format!("B{} -> B{}\n", &b.hash.to_hex()[0..8], &b.prev_hash.to_hex()[0..8]).as_bytes())
                .unwrap();
            for u in b.uncles.iter().take(3) {
                file.write_all(
                    format!(
                        "B{} -> B{} [style=dotted]\n",
                        &b.hash.to_hex()[0..8],
                        &u.1.to_hex()[0..8]
                    )
                    .as_bytes(),
                )
                .unwrap();
            }
            if b.uncles.len() > 3 {
                file.write_all(
                    format!(
                        "B{} -> B{}others [style=dotted, label=\"{} others\"]\n",
                        &b.hash.to_hex()[0..8],
                        &b.hash.to_hex()[0..8],
                        b.uncles.len() - 3
                    )
                    .as_bytes(),
                )
                .unwrap();
            }
        }

        file.write_all(b"}\n").unwrap();
    }

    async fn parse_seed_peers(&mut self) -> Result<HashMap<PeerId, Multiaddr>, Error> {
        let mut seed_peers_result = HashMap::new();

        if self.config.seed_peers.is_empty() {
            return Ok(seed_peers_result);
        }

        let mut opts = ResolverOpts::default();
        opts.edns0 = true;
        opts.try_tcp_on_error = true;
        let dns_resolver = TokioAsyncResolver::tokio(ResolverConfig::default(), opts);
        for seed_peer in &self.config.seed_peers {
            let addr = seed_peer.parse::<Multiaddr>()?;
            let addr_parts = addr.iter().collect_vec();
            if addr_parts.is_empty() {
                return Err(anyhow!("Seed peer address is empty"));
            }
            let is_dns_addr = matches!(addr_parts.first(), Some(Protocol::Dnsaddr(_)));
            let peer_id = match addr.iter().last() {
                Some(Protocol::P2p(peer_id)) => Some(peer_id),
                _ => None,
            };
            if peer_id.is_none() && !is_dns_addr {
                return Err(anyhow!("Seed peer address does not contain peer id"));
            }

            if is_dns_addr {
                // lookup all addresses in dnsaddr multi address
                let addr_str = match addr.iter().last() {
                    Some(Protocol::Dnsaddr(addr_str)) => Some(addr_str.to_string()),
                    _ => None,
                };
                if let Some(addr_str) = addr_str {
                    match dns_resolver.txt_lookup(format!("_dnsaddr.{}", addr_str)).await {
                        Ok(result) => {
                            for txt in result {
                                if let Some(chars) = txt.txt_data().first() {
                                    match self.parse_dnsaddr_txt(chars) {
                                        Ok(parsed_addr) => {
                                            let peer_id = match parsed_addr.iter().last() {
                                                Some(Protocol::P2p(peer_id)) => Some(peer_id),
                                                _ => None,
                                            };
                                            if let Some(peer_id) = peer_id {
                                                seed_peers_result.insert(peer_id, parsed_addr);
                                            }
                                        },
                                        Err(error) => {
                                            warn!(target: LOG_TARGET, "Skipping invalid DNS entry: {:?}: {error:?}", chars);
                                        },
                                    }
                                }
                            }
                        },
                        Err(error) => {
                            error!(target: LOG_TARGET, "Failed to lookup domain records: {error:?}");
                        },
                    }
                }
            } else {
                seed_peers_result.insert(peer_id.unwrap(), addr);
            }
        }

        Ok(seed_peers_result)
    }

    /// Adding all peer addresses to kademlia DHT and run bootstrap to get peers.
    async fn join_seed_peers(&mut self, seed_peers: HashMap<PeerId, Multiaddr>) {
        let mut peers_to_add = Vec::with_capacity(seed_peers.len());
        seed_peers.iter().for_each(|(peer_id, addr)| {
            info!(target: LOG_TARGET, "Adding seed peer: {:?} -> {:?}", peer_id, addr);
            // self.swarm.behaviour_mut().kademlia.add_address(peer_id, addr.clone());
            self.swarm.add_peer_address(*peer_id, addr.clone());
            peers_to_add.push(*peer_id);
            let _unused = self.swarm.dial(DialOpts::peer_id(*peer_id).build()).inspect_err(|e| {
                warn!(target: LOG_TARGET, "Failed to dial seed peer: {e:?}");
            });
        });
        self.network_peer_store.write().await.add_seed_peers(peers_to_add);
    }

    fn parse_dnsaddr_txt(&self, txt: &[u8]) -> Result<Multiaddr, Error> {
        let txt_str = String::from_utf8(txt.to_vec())?;
        match txt_str.strip_prefix("dnsaddr=") {
            None => Err(anyhow!("Missing `dnsaddr=` prefix.")),
            Some(a) => Ok(Multiaddr::try_from(a)?),
        }
    }

    pub async fn dial_seed_peers(&mut self) -> Result<(), Error> {
        info!(target: LOG_TARGET, "Dialing seed peers...");
        let seed_peers = self.parse_seed_peers().await.inspect_err(|e| {
            warn!(target: LOG_TARGET, "Failed to parse seed peers: {e:?}");
        })?;
        self.join_seed_peers(seed_peers).await;
        Ok(())
    }

    /// Starts p2p service.
    /// Please note that this is a blocking call!
    pub async fn start(&mut self) -> Result<(), Error> {
        // listen on local address
        let listen_configs = [
            // Try IPv6 first
            (IpAddr::from_str("::").unwrap(), "ip6", "IPv6"),
            // Fallback to IPv4
            (IpAddr::from_str("0.0.0.0").unwrap(), "ip4", "IPv4"),
        ];

        let port = self.port;
        let mut listeners_started_count = 0;

        for (addr_ip, ip_label, readable_name) in &listen_configs {
            let quic_multiaddr_str = format!("/{}/{}/udp/{}/quic-v1", ip_label, addr_ip, port);
            match quic_multiaddr_str.parse::<Multiaddr>() {
                Ok(quic_maddr) => {
                    match self.swarm.listen_on(quic_maddr.clone()) {
                        Ok(_) => {
                            info!(target: LOG_TARGET, "Listening on QUIC {} address: {}", readable_name, quic_maddr);
                            listeners_started_count += 1;
                        },
                        Err(e) => {
                            if addr_ip.is_ipv6() && is_eafnosupport(&e) {
                                warn!(target: LOG_TARGET, "QUIC: {} not supported on this system (EAFNOSUPPORT). Skipping {}. Error: {}", readable_name, quic_maddr, e);
                            } else {
                                // Log other errors (e.g. port in use) as more severe warnings or errors
                                error!(target: LOG_TARGET, "Failed to listen on QUIC address {}: {}", quic_maddr, e);
                            }
                        },
                    }
                },
                Err(e) => {
                    error!(target: LOG_TARGET, "Failed to parse QUIC multiaddress '{}': {}", quic_multiaddr_str, e);
                },
            }

            let tcp_multiaddr_str = format!("/{}/{}/tcp/{}", ip_label, addr_ip, port);
            match tcp_multiaddr_str.parse::<Multiaddr>() {
                Ok(tcp_maddr) => match self.swarm.listen_on(tcp_maddr.clone()) {
                    Ok(_) => {
                        info!(target: LOG_TARGET, "Listening on TCP {} address: {}", readable_name, tcp_maddr);
                        listeners_started_count += 1;
                    },
                    Err(e) => {
                        if addr_ip.is_ipv6() && is_eafnosupport(&e) {
                            warn!(target: LOG_TARGET, "TCP: {} not supported on this system (EAFNOSUPPORT). Skipping {}. Error: {}", readable_name, tcp_maddr, e);
                        } else {
                            error!(target: LOG_TARGET, "Failed to listen on TCP address {}: {}", tcp_maddr, e);
                        }
                    },
                },
                Err(e) => {
                    error!(target: LOG_TARGET, "Failed to parse TCP multiaddress '{}': {}", tcp_multiaddr_str, e);
                },
            }
        }

        if listeners_started_count == 0 {
            error!(target: LOG_TARGET, "Failed to start any P2P listeners. Application cannot continue.");
            return Err(anyhow!(
                "P2P listeners could not be established. Check IPv4/IPv6 configuration and port availability."
            ));
        }
        // external address
        if let Some(external_addr) = &self.config.external_addr {
            self.swarm.add_external_address(
                // format!("/ip4/{}/tcp/{}", external_addr, self.port)
                external_addr.parse()?,
            );
        }
        self.subscribe_to_topics().await;

        if self.config.diagnostic_mode {
            debug!(target: LOG_TARGET, "Starting main loop diagnostic mode");
            if let Err(e) = self.main_loop_diagnostic_mode().await {
                warn!(target: LOG_TARGET, "Diagnostic loop failed: {e:?}");
                return Err(e);
            }
        } else {
            if let Err(e) = self.dial_seed_peers().await {
                warn!(target: LOG_TARGET, "Failed to dial seed peers: {e:?}");
                return Err(e);
            }
            debug!(target: LOG_TARGET, "Starting main loop normal mode");
            if let Err(e) = self.main_loop().await {
                warn!(target: LOG_TARGET, "Main loop failed: {e:?}");
                return Err(e);
            }
        }
        info!(target: LOG_TARGET,"P2P service has been stopped!");
        Ok(())
    }
}

fn format_swarm_event<TBehaviourOutEvent: std::fmt::Debug>(event: &SwarmEvent<TBehaviourOutEvent>) -> String {
    match event {
        SwarmEvent::Behaviour(_) => "SwarmEvent::Behaviour({..})".to_string(),
        _ => format!("{:?})", event),
    }
}

fn is_localhost_or_private(addr: &Multiaddr) -> bool {
    for proto in addr {
        if let libp2p::multiaddr::Protocol::Ip4(ip) = proto {
            if ip.is_loopback() || ip.is_private() {
                return true;
            }
        }
        if let libp2p::multiaddr::Protocol::Ip6(ip) = proto {
            if ip.is_loopback() {
                return true;
            }
        }
    }
    false
}
