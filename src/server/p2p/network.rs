// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::HashMap,
    fmt::Display,
    fs,
    hash::Hash,
    io::Write,
    num::NonZeroU32,
    ops::ControlFlow,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use convert_case::{Case, Casing};
use hickory_resolver::{
    config::{ResolverConfig, ResolverOpts},
    TokioAsyncResolver,
};
use itertools::Itertools;
use libp2p::{
    autonat::{self, NatStatus},
    dcutr,
    futures::StreamExt,
    gossipsub::{self, IdentTopic, Message, PublishError},
    identify::{self, Info},
    identity::Keypair,
    kad::{self, store::MemoryStore, Event},
    mdns::{self, tokio::Tokio},
    multiaddr::Protocol,
    noise,
    relay,
    request_response::{self, cbor, ResponseChannel},
    swarm::{
        behaviour::toggle::Toggle,
        dial_opts::{DialOpts, PeerCondition},
        NetworkBehaviour,
        SwarmEvent,
    },
    tcp,
    yamux,
    Multiaddr,
    PeerId,
    StreamProtocol,
    Swarm,
};
use log::{
    debug,
    error,
    info,
    kv::{ToValue, Value},
    trace,
    warn,
};
use serde::{Deserialize, Serialize};
use tari_common::configuration::Network;
use tari_common_types::types::FixedHash;
use tari_core::proof_of_work::PowAlgorithm;
use tari_shutdown::ShutdownSignal;
use tari_utilities::hex::Hex;
use tokio::{
    fs::File,
    io::{self, AsyncReadExt, AsyncWriteExt},
    select,
    sync::{
        broadcast::{self, error::RecvError},
        mpsc::{self, Sender},
        oneshot,
        RwLock,
    },
    time::MissedTickBehavior,
};

use super::messages::{DirectPeerInfoRequest, DirectPeerInfoResponse, NotifyNewTipBlock};
use crate::{
    server::{
        config,
        p2p::{
            client::ServiceClient,
            messages::{self, PeerInfo, ShareChainSyncRequest, ShareChainSyncResponse},
            peer_store::{AddPeerStatus, PeerStore},
            relay_store::RelayStore,
            Error,
            LibP2PError,
            MIN_NOTIFY_VERSION,
            MIN_PEER_INFO_VERSION,
        },
    },
    sharechain::{p2block::CURRENT_CHAIN_ID, ShareChain},
};

const PEER_INFO_TOPIC: &str = "peer_info";
const BLOCK_NOTIFY_TOPIC: &str = "block_notify";
const SHARE_CHAIN_SYNC_REQ_RESP_PROTOCOL: &str = "/share_chain_sync/4";
const DIRECT_PEER_EXCHANGE_REQ_RESP_PROTOCOL: &str = "/tari_direct_peer_info/4";
const LOG_TARGET: &str = "tari::p2pool::server::p2p";
const MESSAGE_LOGGING_LOG_TARGET: &str = "tari::p2pool::message_logging";
pub const STABLE_PRIVATE_KEY_FILE: &str = "p2pool_private.key";

const MAX_ACCEPTABLE_P2P_MESSAGE_TIMEOUT: Duration = Duration::from_millis(500);
const SYNC_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Squad {
    inner: String,
}

impl Squad {
    pub fn formatted(&self) -> String {
        self.inner.to_case(Case::Lower).replace("_", " ").to_case(Case::Title)
    }

    pub fn as_string(&self) -> String {
        self.inner.clone()
    }
}

impl ToValue for Squad {
    fn to_value(&self) -> Value {
        Value::from(self.inner.as_str())
    }
}

impl From<String> for Squad {
    fn from(value: String) -> Self {
        Self {
            inner: value.to_case(Case::Lower).to_case(Case::Snake),
        }
    }
}

impl Display for Squad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.inner.clone())
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub external_addr: Option<String>,
    pub seed_peers: Vec<String>,
    pub peer_info_publish_interval: Duration,
    pub stable_peer: bool,
    pub private_key_folder: PathBuf,
    pub private_key: Option<Keypair>,
    pub mdns_enabled: bool,
    pub relay_server_enabled: bool,
    pub squad: Squad,
    pub user_agent: String,
    pub grey_list_clear_interval: Duration,
    pub sync_interval: Duration,
    pub is_seed_peer: bool,
    pub debug_print_chain: bool,
    pub num_peers_to_sync: usize,
    pub max_blocks_to_request: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            external_addr: None,
            seed_peers: vec![],
            peer_info_publish_interval: Duration::from_secs(60), // 1 minute should maybe even be longer
            stable_peer: true,
            private_key_folder: PathBuf::from("."),
            private_key: None,
            mdns_enabled: false,
            relay_server_enabled: false,
            squad: Squad::from("default".to_string()),
            user_agent: "tari-p2pool".to_string(),
            grey_list_clear_interval: Duration::from_secs(20 * 60),
            sync_interval: Duration::from_secs(10),
            is_seed_peer: false,
            debug_print_chain: false,
            num_peers_to_sync: 10,
            max_blocks_to_request: 2500,
        }
    }
}

#[derive(NetworkBehaviour)]
pub struct ServerNetworkBehaviour {
    pub mdns: Toggle<mdns::Behaviour<Tokio>>,
    pub gossipsub: gossipsub::Behaviour,
    pub share_chain_sync: cbor::Behaviour<ShareChainSyncRequest, ShareChainSyncResponse>,
    pub direct_peer_exchange: cbor::Behaviour<DirectPeerInfoRequest, DirectPeerInfoResponse>,
    pub kademlia: kad::Behaviour<MemoryStore>,
    pub identify: identify::Behaviour,
    pub relay_server: relay::Behaviour,
    pub relay_client: relay::client::Behaviour,
    pub dcutr: dcutr::Behaviour,
    pub autonat: autonat::Behaviour,
}

pub enum P2pServiceQuery {
    GetConnectionInfo(oneshot::Sender<ConnectionInfo>),
    GetConnectedPeers(oneshot::Sender<(Vec<ConnectedPeerInfo>, Vec<ConnectedPeerInfo>)>),
}

#[derive(Serialize, Clone)]
pub(crate) struct ConnectedPeerInfo {
    peer_id: String,
    peer_info: PeerInfo,
    last_grey_list_reason: Option<String>,
    // peer_addresses: Vec<Multiaddr>,
    // is_pending: bol,
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

/// Service is the implementation that holds every peer-to-peer related logic
/// that makes sure that all the communications, syncing, broadcasting etc... are done.
pub struct Service<S>
where S: ShareChain
{
    swarm: Swarm<ServerNetworkBehaviour>,
    port: u16,
    share_chain_sha3x: Arc<S>,
    share_chain_random_x: Arc<S>,
    network_peer_store: PeerStore,
    config: Config,
    shutdown_signal: ShutdownSignal,
    // share_chain_sync_tx: broadcast::Sender<LocalShareChainSyncRequest>,
    query_tx: mpsc::Sender<P2pServiceQuery>,
    query_rx: mpsc::Receiver<P2pServiceQuery>,
    // service client related channels
    // TODO: consider mpsc channels instead of broadcast to not miss any message (might drop)
    client_broadcast_block_tx: broadcast::Sender<NotifyNewTipBlock>,
    client_broadcast_block_rx: broadcast::Receiver<NotifyNewTipBlock>,

    relay_store: Arc<RwLock<RelayStore>>,
}

impl<S> Service<S>
where S: ShareChain
{
    /// Constructs a new Service from the provided config.
    /// It also instantiates libp2p swarm inside.
    pub async fn new(
        config: &config::Config,
        share_chain_sha3x: Arc<S>,
        share_chain_random_x: Arc<S>,
        network_peer_store: PeerStore,
        shutdown_signal: ShutdownSignal,
    ) -> Result<Self, Error> {
        let swarm = Self::new_swarm(config).await?;

        // client related channels
        let (broadcast_block_tx, broadcast_block_rx) = broadcast::channel::<NotifyNewTipBlock>(100);
        // let (_share_chain_sync_tx, _share_chain_sync_rx) = broadcast::channel::<LocalShareChainSyncRequest>(1000);
        // let (snooze_block_tx, snooze_block_rx) = mpsc::channel::<(usize, P2Block)>(1000);
        let (query_tx, query_rx) = mpsc::channel(100);

        Ok(Self {
            swarm,
            port: config.p2p_port,
            share_chain_sha3x,
            share_chain_random_x,
            network_peer_store,
            config: config.p2p_service.clone(),
            shutdown_signal,
            client_broadcast_block_tx: broadcast_block_tx,
            client_broadcast_block_rx: broadcast_block_rx,
            query_tx,
            query_rx,
            relay_store: Arc::new(RwLock::new(RelayStore::default())),
        })
    }

    /// Generates or reads libp2p private key if stable_peer is set to true otherwise returns a random key.
    /// Using this method we can be sure that our Peer ID remains the same across restarts in case of
    /// stable_peer is set to true.
    async fn keypair(config: &Config) -> Result<Keypair, Error> {
        if !config.stable_peer {
            return Ok(Keypair::generate_ed25519());
        }

        // if we have a private key set, use it instead
        if let Some(private_key) = &config.private_key {
            return Ok(private_key.clone());
        }

        // if we have a saved private key from file, just use it
        let mut content = vec![];
        let mut key_path = config.private_key_folder.clone();
        key_path.push(STABLE_PRIVATE_KEY_FILE);

        if let Ok(mut file) = File::open(key_path.clone()).await {
            if file.read_to_end(&mut content).await.is_ok() {
                return Keypair::from_protobuf_encoding(content.as_slice())
                    .map_err(|error| Error::LibP2P(LibP2PError::KeyDecoding(error)));
            }
        }

        // otherwise create a new one
        let key_pair = Keypair::generate_ed25519();
        let mut new_private_key_file = File::create_new(key_path)
            .await
            .map_err(|error| Error::LibP2P(LibP2PError::IO(error)))?;
        new_private_key_file
            .write_all(
                key_pair
                    .to_protobuf_encoding()
                    .map_err(|error| Error::LibP2P(LibP2PError::KeyDecoding(error)))?
                    .as_slice(),
            )
            .await
            .map_err(|error| Error::LibP2P(LibP2PError::IO(error)))?;

        Ok(key_pair)
    }

    /// Creates a new swarm from the provided config
    async fn new_swarm(config: &config::Config) -> Result<Swarm<ServerNetworkBehaviour>, Error> {
        let swarm = libp2p::SwarmBuilder::with_existing_identity(Self::keypair(&config.p2p_service).await?)
            .with_tokio()
            .with_tcp(tcp::Config::default(), noise::Config::new, yamux::Config::default)
            .map_err(|error| Error::LibP2P(LibP2PError::Noise(error)))?
            .with_relay_client(noise::Config::new, yamux::Config::default)
            .map_err(|error| Error::LibP2P(LibP2PError::Noise(error)))?
            .with_behaviour(|key_pair, relay_client| {
                // .with_behaviour(move |key_pair, relay_client| {
                // gossipsub

                let gossipsub_config = gossipsub::ConfigBuilder::default()
                    .fanout_ttl(Duration::from_secs(10))
                    .max_ihave_length(1000) // Default is 5000
                    .max_messages_per_rpc(Some(1000))
                    // We get a lot of messages, so 
                    .duplicate_cache_time(Duration::from_secs(1))
                    .build()
                    .map_err(|msg| io::Error::new(io::ErrorKind::Other, msg))?;
                let gossipsub = gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(key_pair.clone()),
                    gossipsub_config,
                )?;

                // mdns
                let mut mdns_service = Toggle::from(None);
                if config.p2p_service.mdns_enabled {
                    mdns_service = Toggle::from(Some(
                        mdns::Behaviour::new(mdns::Config::default(), key_pair.public().to_peer_id())
                            .map_err(|e| Error::LibP2P(LibP2PError::IO(e)))?,
                    ));
                }

                // relay server
          let relay_config =  relay::Config{
            max_reservations: if config.p2p_service.is_seed_peer {  1024 } else { 512 },
            ..Default::default()
        };

                let relay_server = relay::Behaviour::new(key_pair.public().to_peer_id(),
                relay_config.reservation_rate_per_ip(NonZeroU32::new(600).expect("can't fail"), Duration::from_secs(60))
                );

                Ok(ServerNetworkBehaviour {
                    gossipsub,
                    mdns: mdns_service,
                    share_chain_sync: cbor::Behaviour::<ShareChainSyncRequest, ShareChainSyncResponse>::new(
                        [(
                            StreamProtocol::new(SHARE_CHAIN_SYNC_REQ_RESP_PROTOCOL),
                            request_response::ProtocolSupport::Full,
                        )],
                        request_response::Config::default().with_request_timeout(Duration::from_secs(30)), // 10 is the default
                    ),
                    direct_peer_exchange: cbor::Behaviour::<DirectPeerInfoRequest, DirectPeerInfoResponse>::new(
                        [(
                            StreamProtocol::new(DIRECT_PEER_EXCHANGE_REQ_RESP_PROTOCOL),
                            request_response::ProtocolSupport::Full,
                        )],
                        request_response::Config::default().with_request_timeout(Duration::from_secs(30)), // 10 is the default
                    ),
                    kademlia: kad::Behaviour::new(
                        key_pair.public().to_peer_id(),
                        MemoryStore::new(key_pair.public().to_peer_id()),
                    ),
                    identify: identify::Behaviour::new(identify::Config::new(
                        "/p2pool/1.0.0".to_string(),
                        key_pair.public(),
                    )),
                    relay_server,
                    relay_client,
                    dcutr: dcutr::Behaviour::new(key_pair.public().to_peer_id()),
                    autonat: autonat::Behaviour::new(key_pair.public().to_peer_id(), Default::default()),
                })
            })
            .map_err(|e| Error::LibP2P(LibP2PError::Behaviour(e.to_string())))?
            .with_swarm_config(|c| c.with_idle_connection_timeout(config.idle_connection_timeout))
            .build();

        dbg!("Check if we must set the kademlia mode");
        // swarm.behaviour_mut().kademlia.set_mode(Some(Mode::Server));

        Ok(swarm)
    }

    /// Creates a new client for this service, it is thread safe (Send + Sync).
    /// Any amount of clients can be created, no need to share the same one across many components.
    pub fn client(&mut self) -> ServiceClient {
        ServiceClient::new(self.client_broadcast_block_tx.clone())
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

        // broadcast peer info to network
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(
                IdentTopic::new(Self::network_topic(PEER_INFO_TOPIC)),
                peer_info_squad_raw.clone(),
            )
            .map_err(|error| Error::LibP2P(LibP2PError::Publish(error)))?;

        // broadcast peer info to squad
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(
                IdentTopic::new(Self::squad_topic(&self.config.squad, PEER_INFO_TOPIC)),
                peer_info_squad_raw,
            )
            .map_err(|error| Error::LibP2P(LibP2PError::Publish(error)))?;

        Ok(())
    }

    async fn create_peer_info(&mut self, public_addresses: Vec<Multiaddr>) -> Result<PeerInfo, Error> {
        let share_chain_sha3x = self.share_chain_sha3x.clone();
        let share_chain_random_x = self.share_chain_random_x.clone();
        let current_height_sha3x = share_chain_sha3x.tip_height().await.map_err(Error::ShareChain)?;
        let current_height_random_x = share_chain_random_x.tip_height().await.map_err(Error::ShareChain)?;
        let peer_info_squad_raw = PeerInfo::new(
            current_height_sha3x,
            current_height_random_x,
            self.config.squad.as_string(),
            public_addresses,
            Some(self.config.user_agent.clone()),
        );
        Ok(peer_info_squad_raw)
    }

    /// Broadcasting a new mined [`Block`] to the network (assume it is already validated with the network).
    async fn broadcast_block(&mut self, result: Result<NotifyNewTipBlock, RecvError>) {
        dbg!("Broadcast block");
        // if self.sync_in_progress.load(Ordering::SeqCst) {
        //     return;
        // }

        match result {
            Ok(block) => {
                let block_raw_result: Result<Vec<u8>, Error> = block.clone().try_into();
                match block_raw_result {
                    Ok(block_raw) => {
                        match self
                            .swarm
                            .behaviour_mut()
                            .gossipsub
                            .publish(
                                IdentTopic::new(Self::squad_topic(&self.config.squad, BLOCK_NOTIFY_TOPIC)),
                                block_raw,
                            )
                        // .map_err(|error| Error::LibP2P(LibP2PError::Publish(error)))
                        {
                            Ok(_) => {},
                            Err(error) => {
                                if matches!(error, PublishError::InsufficientPeers)  {
                                    debug!(target: LOG_TARGET, squad = &self.config.squad; "No peers to broadcast new block");
                                } else {
                                    error!(target: LOG_TARGET, squad = &self.config.squad; "Failed to broadcast new block: {error:?}");
                                }
                            },
                        }
                    },
                    Err(error) => {
                        error!(target: LOG_TARGET, squad = &self.config.squad; "Failed to convert block to bytes: {error:?}")
                    },
                }
            },
            Err(error) => {
                error!(target: LOG_TARGET, squad = &self.config.squad; "Failed to receive new block: {error:?}")
            },
        }
    }

    /// Generates a gossip sub topic name based on the current Tari network to avoid mixing up
    /// blocks and peers with different Tari networks.
    fn network_topic(topic: &str) -> String {
        let network = Network::get_current_or_user_setting_or_default().as_key_str();
        let chain_id = CURRENT_CHAIN_ID.clone();
        format!("{network}_{chain_id}_{topic}")
    }

    /// Generates a gossip sub topic name based on the current Tari network to avoid mixing up
    /// blocks and peers with different Tari networks and the given squad name.
    fn squad_topic(squad: &Squad, topic: &str) -> String {
        let network = Network::get_current_or_user_setting_or_default().as_key_str();
        let chain_id = CURRENT_CHAIN_ID.clone();
        format!("{network}_{chain_id}_{squad}_{topic}")
    }

    /// Subscribing to a gossipsub topic.
    fn subscribe(&mut self, topic: &str, squad: bool) {
        let topic = if squad {
            Self::squad_topic(&self.config.squad, topic)
        } else {
            Self::network_topic(topic)
        };
        self.swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&IdentTopic::new(topic))
            .expect("must be subscribed to topic");
    }

    /// Subscribes to all topics we need.
    async fn subscribe_to_topics(&mut self) {
        if self.config.is_seed_peer {
            return;
        }
        self.subscribe(PEER_INFO_TOPIC, false);
        self.subscribe(PEER_INFO_TOPIC, true);
        self.subscribe(BLOCK_NOTIFY_TOPIC, true);
    }

    /// Main method to handle any message comes from gossipsub.
    #[allow(clippy::too_many_lines)]
    async fn handle_new_gossipsub_message(&mut self, message: Message) {
        debug!(target: MESSAGE_LOGGING_LOG_TARGET, "New gossipsub message: {message:?}");
        let peer = message.source;
        if peer.is_none() {
            warn!("Message source is not set! {:?}", message);
            return;
        }
        let peer = peer.unwrap();

        let topic = message.topic.to_string();

        match topic {
            topic if topic == Self::network_topic(PEER_INFO_TOPIC) => match messages::PeerInfo::try_from(message) {
                Ok(payload) => {
                    debug!(target: MESSAGE_LOGGING_LOG_TARGET, "[PEERINFO_TOPIC] New peer info: {peer:?} -> {payload:?}");
                    debug!(target: LOG_TARGET, squad = &self.config.squad; "[NETWORK] New peer info: {peer:?} -> {payload:?}");
                    if payload.version < MIN_PEER_INFO_VERSION {
                        trace!(target: LOG_TARGET, squad = &self.config.squad; "Peer {} has an outdated version, skipping", peer);
                        return;
                    }
                    if payload.squad != self.config.squad.as_string() {
                        debug!(target: LOG_TARGET, squad = &self.config.squad; "Peer {} is not in the same squad, skipping. Our squad: {}, their squad:{}", peer, self.config.squad, payload.squad);
                        return;
                    }
                    if !self.config.is_seed_peer {
                        self.add_peer(payload, peer).await;
                    }
                },
                Err(error) => {
                    debug!(target: LOG_TARGET, squad = &self.config.squad; "Can't deserialize peer info payload: {:?}", error);
                },
            },
            topic if topic == Self::squad_topic(&self.config.squad, PEER_INFO_TOPIC) => {
                match messages::PeerInfo::try_from(message) {
                    Ok(payload) => {
                        debug!(target: MESSAGE_LOGGING_LOG_TARGET, "[SQUAD_PEERINFO_TOPIC] New peer info: {peer:?} -> {payload:?}");

                        debug!(target: LOG_TARGET, squad = &self.config.squad; "[squad] New peer info: {peer:?} -> {payload:?}");
                        if payload.version < MIN_PEER_INFO_VERSION {
                            debug!(target: LOG_TARGET, squad = &self.config.squad; "Peer {} has an outdated version, skipping", peer);
                            return;
                        }
                        if !self.config.is_seed_peer {
                            self.add_peer(payload, peer).await;
                        }
                    },
                    Err(error) => {
                        debug!(target: LOG_TARGET, squad = &self.config.squad; "Can't deserialize peer info payload: {:?}", error);
                    },
                }
            },
            // TODO: send a signature that proves that the actual block was coming from this peer
            // TODO: (sender peer's wallet address should be included always in the conibases with a fixed percent (like
            // 20%))
            topic if topic == Self::squad_topic(&self.config.squad, BLOCK_NOTIFY_TOPIC) => {
                debug!(target: MESSAGE_LOGGING_LOG_TARGET, "[SQUAD_NEW_BLOCK_TOPIC] New block from gossip: {peer:?}");

                // if self.sync_in_progress.load(Ordering::SeqCst) {
                //     return;
                // }
                match NotifyNewTipBlock::try_from(message) {
                    Ok(payload) => {
                        if payload.version < MIN_NOTIFY_VERSION {
                            debug!(target: LOG_TARGET, squad = &self.config.squad; "Peer {} has an outdated version, skipping", peer);
                            return;
                        }
                        let payload = Arc::new(payload);
                        debug!(target: MESSAGE_LOGGING_LOG_TARGET, "[SQUAD_NEW_BLOCK_TOPIC] New block from gossip: {peer:?} -> {payload:?}");

                        // If we don't have this peer, try do peer exchange
                        if self.network_peer_store.exists(&peer) {
                            self.initiate_direct_peer_exchange(peer).await;
                        }

                        // Don't add unless we've already synced.

                        debug!(target: LOG_TARGET, squad = &self.config.squad; "🆕 New block from broadcast: {:?}", &payload.new_blocks.iter().map(|b| b.0.to_string()).join(","));
                        let algo = payload.algo();
                        let share_chain = match algo {
                            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
                            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
                        };
                        if share_chain.tip_height().await.unwrap_or_default() == 0 {
                            debug!(target: LOG_TARGET, squad = &self.config.squad; "Share chain tip height is None, skipping block until we have synced");
                            return;
                        }
                        let mut missing_blocks = vec![];
                        for block in &payload.new_blocks {
                            if share_chain.has_block(block.0, &block.1).await {
                                continue;
                            }
                            missing_blocks.push(block.clone());
                        }
                        self.sync_share_chain(algo, peer, missing_blocks).await;
                    },
                    Err(error) => {
                        // TODO: elevate to error
                        debug!(target: LOG_TARGET, squad = &self.config.squad; "Can't deserialize broadcast block payload: {:?}", error);
                        self.network_peer_store
                            .move_to_grey_list(
                                peer,
                                format!("Node sent a block that could not be deserialized: {:?}", error),
                            )
                            .await;
                    },
                }
            },
            _ => {
                debug!(target: MESSAGE_LOGGING_LOG_TARGET, "Unknown topic {topic:?}!");

                warn!(target: LOG_TARGET, squad = &self.config.squad; "Unknown topic {topic:?}!");
            },
        }
    }

    async fn add_peer(&mut self, payload: PeerInfo, peer: PeerId) -> ControlFlow<()> {
        // Don't add ourselves
        if &peer == self.swarm.local_peer_id() {
            return ControlFlow::Continue(());
        }

        for addr in &payload.public_addresses() {
            self.swarm.add_peer_address(peer, addr.clone());
        }
        let (add_status, last_sync_attempt) = self.network_peer_store.add(peer, payload).await;

        match add_status {
            AddPeerStatus::NewPeer => {
                // self.initiate_direct_peer_exchange(peer).await;
            },
            AddPeerStatus::Existing => {},
            AddPeerStatus::Greylisted => {
                debug!(target: LOG_TARGET, "Added peer but it was grey listed");
                return ControlFlow::Continue(());
            },
            AddPeerStatus::Blacklisted => {
                info!(target: LOG_TARGET, "Added peer but it was black listed");
                return ControlFlow::Continue(());
            },
        }

        if last_sync_attempt
            .map(|attempt| attempt.elapsed() > SYNC_TIMEOUT)
            .unwrap_or(false)
        {
            debug!(target: LOG_TARGET, squad = &self.config.squad; "Sync already in progress");
            return ControlFlow::Break(());
        }

        // debug!(target: LOG_TARGET, squad = &self.config.squad; "Syncing peer info from {peer:?} with heights:
        // RandomX: {their_randomx_height}, Sha3x: {their_sha3x_height}");

        // let our_height = self.share_chain_random_x.tip_height().await.unwrap_or_default();
        // if our_height < their_randomx_height {
        //     self.network_peer_store.update_last_sync_attempt(peer);
        //     self.sync_share_chain(PowAlgorithm::RandomX, peer, vec![(
        //         their_randomx_height,
        //         FixedHash::zero(),
        //     )])
        //     .await;
        // }

        // let our_height = self.share_chain_sha3x.tip_height().await.unwrap_or_default();
        // if our_height < their_sha3x_height {
        //     self.network_peer_store.update_last_sync_attempt(peer);
        //     self.sync_share_chain(PowAlgorithm::Sha3x, peer, vec![(their_sha3x_height, FixedHash::zero())])
        //         .await;
        // }
        ControlFlow::Continue(())
    }

    async fn initiate_direct_peer_exchange(&mut self, peer: PeerId) {
        if let Ok(my_info) = self
            .create_peer_info(self.swarm.external_addresses().cloned().collect())
            .await
            .inspect_err(|error| {
                error!(target: LOG_TARGET, squad = &self.config.squad; "Failed to create peer info: {error:?}");
            })
        {
            let local_peer_id = self.swarm.local_peer_id().clone();
            // TODO: Should we send them our details? The problem is that if we send too many of these, libp2p
            // starts dropping requests with "libp2p_relay::priv_client::handler Dropping in-flight connect
            // request because we are at capacity"

            self.swarm
                .behaviour_mut()
                .direct_peer_exchange
                .send_request(&peer, DirectPeerInfoRequest {
                    info: my_info,
                    peer_id: local_peer_id.to_base58(),
                });
        }
    }

    async fn handle_direct_peer_exchange_request(
        &mut self,
        channel: ResponseChannel<DirectPeerInfoResponse>,
        request: DirectPeerInfoRequest,
    ) {
        let local_peer_id = self.swarm.local_peer_id().clone();
        if let Ok(info) = self
            .create_peer_info(self.swarm.external_addresses().cloned().collect())
            .await
            .inspect_err(|error| {
                error!(target: LOG_TARGET, squad = &self.config.squad; "Failed to create peer info: {error:?}");
            })
        {
            if self
                .swarm
                .behaviour_mut()
                .direct_peer_exchange
                .send_response(channel, DirectPeerInfoResponse {
                    peer_id: local_peer_id.to_base58(),
                    info,
                })
                .is_err()
            {
                error!(target: LOG_TARGET, squad = &self.config.squad; "Failed to send peer info response");
            }
        }

        self.add_peer(request.info, request.peer_id.parse().unwrap()).await;
    }

    async fn handle_direct_peer_exchange_response(&mut self, response: DirectPeerInfoResponse) {
        if response.info.version < MIN_PEER_INFO_VERSION {
            debug!(target: LOG_TARGET, squad = &self.config.squad; "Peer {} has an outdated version, skipping", response.peer_id);
            return;
        }
        self.add_peer(response.info, response.peer_id.parse().unwrap()).await;
    }

    /// Handles share chain sync request (coming from other peer).
    async fn handle_share_chain_sync_request(
        &mut self,
        channel: ResponseChannel<ShareChainSyncResponse>,
        request: ShareChainSyncRequest,
    ) {
        info!(target: LOG_TARGET, squad = &self.config.squad; "Incoming Share chain sync request: {request:?}");
        let share_chain = match request.algo() {
            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
        };
        let local_peer_id = self.swarm.local_peer_id().clone();
        match share_chain.get_blocks(request.missing_blocks()).await {
            Ok(blocks) => {
                if self
                    .swarm
                    .behaviour_mut()
                    .share_chain_sync
                    .send_response(
                        channel,
                        ShareChainSyncResponse::new(local_peer_id, request.algo(), &blocks),
                    )
                    .is_err()
                {
                    error!(target: LOG_TARGET, squad = &self.config.squad; "Failed to send block sync response");
                }
            },
            Err(error) => {
                error!(target: LOG_TARGET, squad = &self.config.squad; "Failed to get blocks from height: {error:?}")
            },
        }
    }

    /// Handle share chain sync response.
    /// All the responding blocks will be tried to put into local share chain.
    async fn handle_share_chain_sync_response(&mut self, response: ShareChainSyncResponse) {
        debug!(target: MESSAGE_LOGGING_LOG_TARGET, "Share chain sync response: {response:?}");
        let peer = response.peer_id().clone();

        let timer = Instant::now();
        // if !self.sync_in_progress.load(Ordering::SeqCst) {
        // return;
        // }
        let algo = response.algo().clone();
        let share_chain = match algo {
            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
        };
        let blocks: Vec<_> = response.into_blocks().into_iter().map(|a| Arc::new(a)).collect();
        match share_chain.add_synced_blocks(&blocks).await {
            Ok(result) => {
                info!(target: LOG_TARGET, squad = &self.config.squad; "Synced blocks added to share chain: {result:?}");
                // Ok(())
            },
            Err(error) => match error {
                crate::sharechain::error::Error::BlockParentDoesNotExist { missing_parents } => {
                    self.sync_share_chain(algo, peer, missing_parents).await;
                    return;
                },
                _ => {
                    error!(target: LOG_TARGET, squad = &self.config.squad; "Failed to add synced blocks to share chain: {error:?}");
                    self.network_peer_store
                        .move_to_grey_list(peer, format!("Block failed validation: {}", error))
                        .await;
                },
            },
        };
        if timer.elapsed() > MAX_ACCEPTABLE_P2P_MESSAGE_TIMEOUT {
            warn!(target: LOG_TARGET, squad = &self.config.squad; "Share chain sync response took too long: {:?}", timer.elapsed());
        }
    }

    /// Trigger share chain sync with another peer with the highest known block height.
    /// Note: this is a "stop-the-world" operation, many operations are skipped when synchronizing.
    async fn sync_share_chain(&mut self, algo: PowAlgorithm, peer: PeerId, missing_parents: Vec<(u64, FixedHash)>) {
        // if self.sync_in_progress.load(Ordering::SeqCst) {
        //     warn!(target: LOG_TARGET, "Sync already in progress...");
        //     return;
        // }
        // self.sync_in_progress.store(true, Ordering::SeqCst);

        debug!(target: LOG_TARGET, squad = &self.config.squad; "Syncing share chain...");

        if self.network_peer_store.is_blacklisted(&peer) {
            warn!(target: LOG_TARGET, squad = &self.config.squad; "Peer is blacklisted, skipping sync");
            return;
        }

        if !self.network_peer_store.is_whitelisted(&peer) {
            debug!(target: LOG_TARGET, squad = &self.config.squad; "Peer is not whitelisted, will still try to sync");
            // return;
        }

        debug!(target: LOG_TARGET, squad = &self.config.squad; "Send share chain sync request to specific peer: {peer}");
        let _outbound_id = self
            .swarm
            .behaviour_mut()
            .share_chain_sync
            .send_request(&peer, ShareChainSyncRequest::new(algo, missing_parents));
        return;

        // match self.squad_peer_store.tip_of_block_height(algo).await {
        //     Some(result) => {
        //         debug!(target: LOG_TARGET, squad = &self.config.squad; "Found highest known block height:
        // {result:?}");         debug!(target: LOG_TARGET, squad = &self.config.squad; "Send share chain sync
        // request: {result:?}");         // we always send from_height as zero now, to not miss any blocks
        //         info!(target: LOG_TARGET, "[{:?}] Syncing share chain...", algo);
        //         self.swarm
        //             .behaviour_mut()
        //             .share_chain_sync
        //             .send_request(&result.peer_id, ShareChainSyncRequest::new(algo, 0));
        //     },
        //     None => {
        //         error!(target: LOG_TARGET, squad = &self.config.squad; "[{:?}] Failed to get peer with highest share
        // chain height!", algo)     },
        // }
    }

    /// Main method to handle libp2p events.
    #[allow(clippy::too_many_lines)]
    async fn handle_event(&mut self, event: SwarmEvent<ServerNetworkBehaviourEvent>) {
        debug!(target: MESSAGE_LOGGING_LOG_TARGET, "New event: {event:?}");

        match event {
            SwarmEvent::ConnectionEstablished {
                peer_id,
                endpoint,
                num_established,
                concurrent_dial_errors,
                established_in,
                ..
            } => {
                debug!(target: LOG_TARGET, squad = &self.config.squad; "Connection established: {peer_id:?} -> {endpoint:?} ({num_established:?}/{concurrent_dial_errors:?}/{established_in:?})");
            },
            SwarmEvent::Dialing { peer_id, .. } => {
                debug!(target: LOG_TARGET, squad = &self.config.squad; "Dialing: {peer_id:?}");
            },
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(target: LOG_TARGET, squad = &self.config.squad; "Listening on {address:?}");
            },
            SwarmEvent::Behaviour(event) => match event {
                ServerNetworkBehaviourEvent::Mdns(mdns_event) => match mdns_event {
                    mdns::Event::Discovered(peers) => {
                        for (peer, addr) in peers {
                            self.swarm.add_peer_address(peer, addr);
                            self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer);
                        }
                    },
                    mdns::Event::Expired(peers) => {
                        for (peer, _addr) in peers {
                            self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer);
                        }
                    },
                },
                ServerNetworkBehaviourEvent::Gossipsub(event) => match event {
                    gossipsub::Event::Message {
                        message,
                        message_id: _message_id,
                        propagation_source: _propagation_source,
                    } => {
                        self.handle_new_gossipsub_message(message).await;
                    },
                    gossipsub::Event::Subscribed { .. } => {},
                    gossipsub::Event::Unsubscribed { .. } => {},
                    gossipsub::Event::GossipsubNotSupported { .. } => {},
                },
                ServerNetworkBehaviourEvent::DirectPeerExchange(event) => match event {
                    request_response::Event::Message { peer: _, message } => match message {
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
                        } => {
                            self.handle_direct_peer_exchange_response(response).await;
                        },
                    },
                    request_response::Event::OutboundFailure { peer, error, .. } => {
                        // Peers can be offline
                        debug!(target: LOG_TARGET, squad = &self.config.squad; "REQ-RES peer info outbound failure: {peer:?} -> {error:?}");
                        // TODO: find out why this errors
                        // self.network_peer_store
                        //     .move_to_grey_list(
                        //         peer,
                        //         format!("Error during direct peer exchange: {}", error.to_string()),
                        //     )
                        //     .await;
                    },
                    request_response::Event::InboundFailure { peer, error, .. } => {
                        error!(target: LOG_TARGET, squad = &self.config.squad; "REQ-RES  peer info inbound failure: {peer:?} -> {error:?}");
                    },
                    request_response::Event::ResponseSent { .. } => {},
                },
                ServerNetworkBehaviourEvent::ShareChainSync(event) => match event {
                    request_response::Event::Message { peer: _peer, message } => match message {
                        request_response::Message::Request {
                            request_id: _request_id,
                            request,
                            channel,
                        } => {
                            self.handle_share_chain_sync_request(channel, request).await;
                        },
                        request_response::Message::Response {
                            request_id: _request_id,
                            response,
                        } => {
                            self.handle_share_chain_sync_response(response).await;
                        },
                    },
                    request_response::Event::OutboundFailure { peer, error, .. } => {
                        // Peers can be offline
                        debug!(target: LOG_TARGET, squad = &self.config.squad; "REQ-RES outbound failure: {peer:?} -> {error:?}");
                        // Unlock the permit
                        self.network_peer_store
                            .move_to_grey_list(peer, format!("Error during share chain sync:{}", error.to_string()))
                            .await;

                        // Remove peer from peer store to try to sync from another peer,
                        // if the peer goes online/accessible again, the peer store will have it again.
                        // self.network_peer_store.remove(&peer).await;
                    },
                    request_response::Event::InboundFailure { peer, error, .. } => {
                        error!(target: LOG_TARGET, squad = &self.config.squad; "REQ-RES inbound failure: {peer:?} -> {error:?}");
                    },
                    request_response::Event::ResponseSent { .. } => {},
                },
                ServerNetworkBehaviourEvent::Kademlia(event) => match event {
                    Event::RoutingUpdated { peer, addresses, .. } => {
                        debug!(target: LOG_TARGET, squad = &self.config.squad; "Routing updated: {peer:?} -> {addresses:?}");
                        // addresses.iter().for_each(|addr| {
                        //     self.swarm.add_peer_address(peer, addr.clone());
                        // });
                        // dbg!(peer);
                        // dbg!(old_peer);
                        // dbg!(addresses);
                        // self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer);
                        // if let Some(old_peer) = old_peer {
                        // self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&old_peer);
                        // }
                    },
                    _ => debug!(target: LOG_TARGET, squad = &self.config.squad; "[KADEMLIA] {event:?}"),
                },
                ServerNetworkBehaviourEvent::Identify(event) => match event {
                    identify::Event::Received { peer_id, info, .. } => self.handle_peer_identified(peer_id, info).await,
                    identify::Event::Error { peer_id, error, .. } => {
                        warn!("Failed to identify peer {peer_id:?}: {error:?}");
                        // self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                        self.swarm.behaviour_mut().kademlia.remove_peer(&peer_id);
                    },
                    _ => {},
                },
                ServerNetworkBehaviourEvent::RelayServer(event) => {
                    debug!(target: LOG_TARGET, "[RELAY SERVER]: {event:?}");
                },
                ServerNetworkBehaviourEvent::RelayClient(event) => {
                    debug!(target: LOG_TARGET, "[RELAY CLIENT]: {event:?}");
                },
                ServerNetworkBehaviourEvent::Dcutr(event) => {
                    debug!(target: LOG_TARGET, "[DCUTR]: {event:?}");
                },
                ServerNetworkBehaviourEvent::Autonat(event) => self.handle_autonat_event(event).await,
            },
            _ => {},
        };
    }

    async fn handle_peer_identified(&mut self, peer_id: PeerId, info: Info) {
        if *self.swarm.local_peer_id() == peer_id {
            warn!(target: LOG_TARGET, "Dialled ourselves");
            return;
        }

        if self.swarm.external_addresses().count() > 0 {
            debug!(target: LOG_TARGET, "No need to relay, we have an external address already. {}", self.swarm.external_addresses().map(|a| a.to_string()).collect::<Vec<String>>().join(", "));
            // Check if we can relay
            // warn!(target: LOG_TARGET, "No external addresses");
            // self.swarm.add_external_address(info.observed_addr.clone());
            // info!(target: LOG_TARGET, "We have an external address already, no need to relay.");
            return;
        }

        if self.config.is_seed_peer {
            return;
        }

        let is_relay = info.protocols.iter().any(|p| *p == relay::HOP_PROTOCOL_NAME);

        // adding peer to kademlia and gossipsub
        for addr in info.listen_addrs {
            // self.swarm.behaviour_mut().kademlia.add_address(&peer_id, addr.clone());
            if Self::is_p2p_address(&addr)
                // && addr.is_global_ip()
                && is_relay
            {
                let mut lock = self.relay_store.write().await;
                if lock.add_possible_relay(peer_id, addr.clone()) {
                    info!(target: LOG_TARGET, "Added possible relay: {peer_id:?} -> {addr:?}");
                    drop(lock);
                    self.attempt_relay_reservation().await;
                }
            }
        }
        // }

        // // Try sync from them
        // if self.sync_permits.load(Ordering::SeqCst) > 0 {
        //     self.sync_permits.fetch_sub(1, Ordering::SeqCst);
        //     self.sync_share_chain(PowAlgorithm::Sha3x, Some(peer_id), None).await;
        //     self.sync_permits.fetch_sub(1, Ordering::SeqCst);
        //     self.sync_share_chain(PowAlgorithm::RandomX, Some(peer_id), None).await;
        // }

        // self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
    }

    fn is_p2p_address(address: &Multiaddr) -> bool {
        address.iter().any(|p| matches!(p, Protocol::P2p(_)))
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
                },
                NatStatus::Private => {
                    warn!(target: LOG_TARGET, "[AUTONAT]: We are behind a NAT, connecting to relay!");
                    // let lock = self.relay_store.read().await;
                    // if !lock.has_active_relay() {
                    // drop(lock);
                    if !self.config.is_seed_peer {
                        self.attempt_relay_reservation().await;
                    }
                    // }
                },
                _ => {
                    debug!(target: LOG_TARGET, "[AUTONAT] Ignoring unknown status {new:?}");
                },
            },
            _ => {
                debug!(target: LOG_TARGET, "[AUTONAT] {event:?}");
            },
        }
    }

    async fn attempt_relay_reservation(&mut self) {
        dbg!("Attempt relay reservation");

        // Can happen that a previous lock already set the relaty
        if self.swarm.external_addresses().count() > 0 {
            warn!(target: LOG_TARGET, "No need to relay, we have an external address or relay already");
            return;
        }
        let mut lock = self.relay_store.write().await;
        // TODO: Do relays expire?
        // if lock.has_active_relay() {
        //     // dbg!("Already have an active relay");
        //     return;
        // }
        // dbg!("No, select a relay");
        lock.select_random_relay();
        if let Some(relay) = lock.selected_relay_mut() {
            let addresses = relay.addresses.clone();

            if let Err(err) = self.swarm.dial(
                DialOpts::peer_id(relay.peer_id)
                    .addresses(relay.addresses.clone())
                    .condition(PeerCondition::NotDialing)
                    .build(),
            ) {
                warn!(target: LOG_TARGET, "🚨 Failed to dial relay: {}", err);
            }

            addresses.iter().for_each(|addr| {
                let listen_addr = addr.clone().with(Protocol::P2pCircuit);
                info!(target: LOG_TARGET, "Try to listen on {:?}...", listen_addr);
                match self
                    .swarm
                    .listen_on(listen_addr.clone())
                    .map_err(|e| Error::LibP2P(LibP2PError::Transport(e)))
                {
                    Ok(_) => {
                        info!(target: LOG_TARGET, "Listening on {listen_addr:?}");
                        relay.is_circuit_established = true;
                    },
                    Err(error) => {
                        warn!(target: LOG_TARGET, "Failed to listen on relay address ({:?}): {:?}", listen_addr, error);
                    },
                }
            });
        } else {
            warn!(target: LOG_TARGET, "No relay selected");
        }
    }

    async fn handle_query(&mut self, query: P2pServiceQuery) {
        match query {
            P2pServiceQuery::GetConnectionInfo(reply) => {
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
                let _ = reply.send(connection_info);
            },

            P2pServiceQuery::GetConnectedPeers(reply) => {
                let connected_peers = self.network_peer_store.whitelist_peers();
                let mut white_list_res = vec![];
                for (p, info) in connected_peers {
                    white_list_res.push(ConnectedPeerInfo {
                        peer_id: p.to_string(),
                        peer_info: info.peer_info.clone(),
                        last_grey_list_reason: info.last_grey_list_reason.clone(),
                    });
                }
                let grey_list_peers = self.network_peer_store.greylist_peers();
                let mut grey_list_res = vec![];
                for (p, info) in grey_list_peers {
                    grey_list_res.push(ConnectedPeerInfo {
                        peer_id: p.to_string(),
                        peer_info: info.peer_info.clone(),
                        last_grey_list_reason: info.last_grey_list_reason.clone(),
                    });
                }
                let _ = reply.send((white_list_res, grey_list_res));
            },
        }
    }

    /// Main loop of the service that drives the events and libp2p swarm forward.
    async fn main_loop(&mut self) -> Result<(), Error> {
        let mut publish_peer_info_interval = tokio::time::interval(self.config.peer_info_publish_interval);
        publish_peer_info_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut grey_list_clear_interval = tokio::time::interval(self.config.grey_list_clear_interval);

        let mut sync_interval = tokio::time::interval(self.config.sync_interval);

        let mut debug_chain_graph = if self.config.debug_print_chain {
            tokio::time::interval(Duration::from_secs(30))
        } else {
            // only once a day, but even then will be skipped
            tokio::time::interval(Duration::from_secs(60 * 60 * 24))
        };
        debug_chain_graph.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // TODO: Not sure why this is done on a loop instead of just once....
        // let mut kademlia_bootstrap_interval = tokio::time::interval(Duration::from_secs(12 * 60 * 60));
        // kademlia_bootstrap_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let shutdown_signal = self.shutdown_signal.clone();
        tokio::pin!(shutdown_signal);

        loop {
            select! {
                biased;
                _ = &mut shutdown_signal => {
                    info!(target: LOG_TARGET,"Shutting down p2p service...");
                    return Ok(());
                }
                req = self.query_rx.recv() => {
                    dbg!("query");
                    match req {
                        Some(req) => {
                    self.handle_query(req).await;
                    },
                    None => {
                         warn!(target: LOG_TARGET, "Failed to receive query from channel. Sender dropped?");
                       todo!("Unimplemented");
                    }
                }
                },

                blocks = self.client_broadcast_block_rx.recv() => {
                    dbg!("client broadcast");
                    self.broadcast_block(blocks).await;
                },
                event = self.swarm.select_next_some() => {
                    self.handle_event(event).await;
                 },
                _ = publish_peer_info_interval.tick() => {
                    info!(target: LOG_TARGET, "pub peer info");

                    // broadcast peer info
                    if let Err(error) = self.broadcast_peer_info().await {
                        match error {
                            Error::LibP2P(LibP2PError::Publish(PublishError::InsufficientPeers)) => {
                                warn!(target: LOG_TARGET, squad = &self.config.squad; "No peers to broadcast peer info!");
                            }
                            Error::LibP2P(LibP2PError::Publish(PublishError::Duplicate)) => {}
                            _ => {
                                error!(target: LOG_TARGET, squad = &self.config.squad; "Failed to publish node info: {error:?}");
                            }
                        }
                    }
                },
                _ = sync_interval.tick() =>  {
                    dbg!("Trying to sync");
                    self.try_sync_from_best_peer().await;
                }
                _ = grey_list_clear_interval.tick() => {
                    self.network_peer_store.clear_grey_list();
                },
                _ = debug_chain_graph.tick() => {
                 if self.config.debug_print_chain {
                    self.print_debug_chain_graph().await;
                 }
                },
            }
        }
    }

    async fn try_sync_from_best_peer(&mut self) {
        for algo in &[PowAlgorithm::RandomX, PowAlgorithm::Sha3x] {
            // Find any blocks we are missing.
            let _chain = match algo {
                PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
                PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
            };

            // let missing_blocks = match chain.missing_blocks().await.inspect_err(
            //     |e| error!(target: LOG_TARGET, squad = &self.config.squad; "Failed to get missing blocks: {e:?}"),
            // ) {
            //     Ok(missing_blocks) => missing_blocks,
            //     Err(_) => {
            //         continue;
            //     },
            // };

            // dbg!(&missing_blocks);

            let best_peers = self
                .network_peer_store
                .best_peers_to_sync(self.config.num_peers_to_sync, *algo);
            let our_tip = match algo {
                PowAlgorithm::RandomX => self.share_chain_random_x.tip_height().await.unwrap_or_default(),
                PowAlgorithm::Sha3x => self.share_chain_sha3x.tip_height().await.unwrap_or_default(),
            };

            // info!(target: LOG_TARGET, squad = &self.config.squad; "Best peers to sync: {best_peers:?}");

            for record in best_peers {
                info!(target: LOG_TARGET, squad = &self.config.squad; "Trying to sync from peer: {} rx:{} sha:{}", record.peer_id, record.peer_info.current_random_x_height, record.peer_info.current_sha3x_height );
                let their_height = match algo {
                    PowAlgorithm::RandomX => record.peer_info.current_random_x_height,
                    PowAlgorithm::Sha3x => record.peer_info.current_sha3x_height,
                };
                if their_height > 0 {
                    let mut blocks_to_request = vec![];
                    dbg!(our_tip);
                    dbg!(their_height);
                    for i in (our_tip..=their_height).rev().take(self.config.max_blocks_to_request) {
                        blocks_to_request.push((i, FixedHash::zero()));
                    }

                    // dbg!(blocks_to_request.last());

                    if !blocks_to_request.is_empty() {
                        self.sync_share_chain(*algo, record.peer_id, blocks_to_request).await;
                    } else {
                        info!(target: LOG_TARGET, "No need to sync, we are up to date");
                    }
                }
            }
        }
    }

    async fn print_debug_chain_graph(&self) {
        self.print_debug_chain_graph_inner(&self.share_chain_random_x, "randomx")
            .await;
        self.print_debug_chain_graph_inner(&self.share_chain_sha3x, "sha3x")
            .await;
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
        file.write(b"digraph B {\n").unwrap();
        if let Some(tip) = chain.get_tip().await.unwrap() {
            file.write(&format!("comment=\"{} - {}\"\n", tip.0, &tip.1.to_hex()[0..8]).into_bytes())
                .unwrap();
        }
        let formatter = human_format::Formatter::new();
        let blocks = chain.all_blocks().await.expect("errored");
        for b in blocks {
            file.write(
                format!(
                    "B{} [label=\"{} - {} ({}) {}\"]\n",
                    &b.hash.to_hex()[0..8],
                    &b.height,
                    &b.hash.to_hex()[0..8],
                    if b.verified { "v" } else { "x" },
                    formatter.format(b.target_difficulty.as_u64() as f64)
                )
                .as_bytes(),
            )
            .unwrap();
            file.write(format!("B{} -> B{}\n", &b.hash.to_hex()[0..8], &b.prev_hash.to_hex()[0..8]).as_bytes())
                .unwrap();
            for u in b.uncles.iter().take(3) {
                file.write(
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
                file.write(
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

        file.write(b"}\n").unwrap();
    }

    async fn parse_seed_peers(&mut self) -> Result<HashMap<PeerId, Multiaddr>, Error> {
        let mut seed_peers_result = HashMap::new();

        if self.config.seed_peers.is_empty() {
            return Ok(seed_peers_result);
        }

        let dns_resolver = TokioAsyncResolver::tokio(ResolverConfig::default(), ResolverOpts::default());
        for seed_peer in &self.config.seed_peers {
            let addr = seed_peer
                .parse::<Multiaddr>()
                .map_err(|error| Error::LibP2P(LibP2PError::MultiAddrParse(error)))?;
            let addr_parts = addr.iter().collect_vec();
            if addr_parts.is_empty() {
                return Err(Error::LibP2P(LibP2PError::MultiAddrEmpty));
            }
            let is_dns_addr = matches!(addr_parts.first(), Some(Protocol::Dnsaddr(_)));
            let peer_id = match addr.iter().last() {
                Some(Protocol::P2p(peer_id)) => Some(peer_id),
                _ => None,
            };
            if peer_id.is_none() && !is_dns_addr {
                return Err(Error::LibP2P(LibP2PError::MissingPeerId(seed_peer.clone())));
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
                                            warn!(target: LOG_TARGET, squad = &self.config.squad; "Skipping invalid DNS entry: {:?}: {error:?}", chars);
                                        },
                                    }
                                }
                            }
                        },
                        Err(error) => {
                            error!(target: LOG_TARGET, squad = &self.config.squad; "Failed to lookup domain records: {error:?}");
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
    async fn join_seed_peers(&mut self, seed_peers: HashMap<PeerId, Multiaddr>) -> Result<(), Error> {
        seed_peers.iter().for_each(|(peer_id, addr)| {
            self.swarm.behaviour_mut().kademlia.add_address(peer_id, addr.clone());
        });

        if !seed_peers.is_empty() {
            self.bootstrap_kademlia()?;
        }

        Ok(())
    }

    fn parse_dnsaddr_txt(&self, txt: &[u8]) -> Result<Multiaddr, Error> {
        let txt_str =
            String::from_utf8(txt.to_vec()).map_err(|error| Error::LibP2P(LibP2PError::ConvertBytesToString(error)))?;
        match txt_str.strip_prefix("dnsaddr=") {
            None => Err(Error::LibP2P(LibP2PError::InvalidDnsEntry(
                "Missing `dnsaddr=` prefix.".to_string(),
            ))),
            Some(a) => Ok(Multiaddr::try_from(a).map_err(|error| Error::LibP2P(LibP2PError::MultiAddrParse(error)))?),
        }
    }

    fn bootstrap_kademlia(&mut self) -> Result<(), Error> {
        self.swarm
            .behaviour_mut()
            .kademlia
            .bootstrap()
            .map_err(|error| Error::LibP2P(LibP2PError::KademliaNoKnownPeers(error)))?;

        Ok(())
    }

    pub fn create_query_client(&self) -> Sender<P2pServiceQuery> {
        self.query_tx.clone()
    }

    /// Starts p2p service.
    /// Please note that this is a blocking call!
    pub async fn start(&mut self) -> Result<(), Error> {
        // listen on local address
        self.swarm
            .listen_on(
                format!("/ip4/0.0.0.0/tcp/{}", self.port)
                    .parse()
                    .map_err(|e| Error::LibP2P(LibP2PError::MultiAddrParse(e)))?,
            )
            .map_err(|e| Error::LibP2P(LibP2PError::Transport(e)))?;

        // external address
        if let Some(external_addr) = &self.config.external_addr {
            self.swarm.add_external_address(
                // format!("/ip4/{}/tcp/{}", external_addr, self.port)
                external_addr
                    .parse()
                    .map_err(|e| Error::LibP2P(LibP2PError::MultiAddrParse(e)))?,
            );
        }

        let seed_peers = self.parse_seed_peers().await?;
        self.join_seed_peers(seed_peers).await?;
        self.subscribe_to_topics().await;

        // start initial share chain sync
        // let in_progress = self.sync_in_progress.clone();
        warn!(target: LOG_TARGET, "Starting initial share chain sync...");
        // tokio::spawn(async move {
        //     Self::initial_share_chain_sync(
        //         in_progress,
        //         peer_store,
        //         share_chain_sha3x,
        //         share_chain_random_x,
        //         share_chain_sync_tx,
        //         Duration::from_secs(3),
        //         squad,
        //         shutdown_signal,
        //     )
        //     .await;
        // });

        warn!(target: LOG_TARGET, "Starting main loop");

        self.main_loop().await?;
        info!(target: LOG_TARGET,"P2P service has been stopped!");
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn squad_as_string_no_spaces_no_underscores() {
        let squad = Squad::from("default".to_string());
        assert_eq!(squad.as_string(), "default");
    }

    #[test]
    fn squad_as_string_with_spaces_with_underscores() {
        let squad = Squad::from("default 2".to_string());
        assert_eq!(squad.as_string(), "default_2");
    }
}
