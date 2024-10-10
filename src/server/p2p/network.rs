// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::HashMap,
    fmt::Display,
    hash::{DefaultHasher, Hash, Hasher},
    ops::ControlFlow,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use convert_case::{Case, Casing};
use hickory_resolver::{
    config::{ResolverConfig, ResolverOpts},
    TokioAsyncResolver,
};
use itertools::Itertools;
use libp2p::{
    autonat,
    autonat::NatStatus,
    dcutr,
    futures::StreamExt,
    gossipsub,
    gossipsub::{IdentTopic, Message, PublishError},
    identify,
    identify::Info,
    identity::Keypair,
    kad,
    kad::{store::MemoryStore, Event, Mode},
    mdns,
    mdns::tokio::Tokio,
    multiaddr::Protocol,
    noise,
    relay,
    request_response,
    request_response::{cbor, ResponseChannel},
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
    warn,
};
use serde::{Deserialize, Serialize};
use serde_cbor::tags;
use tari_common::configuration::Network;
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
        Semaphore,
    },
    time::{self, MissedTickBehavior},
};

use crate::{
    server::{
        config,
        p2p::{
            global_ip::GlobalIp,
            messages::{self, LocalShareChainSyncRequest, PeerInfo, ShareChainSyncRequest, ShareChainSyncResponse},
            peer_store::PeerStore,
            relay_store::RelayStore,
            Error,
            LibP2PError,
            ServiceClient,
            MAX_MISSING_PARENTS_TO_SNOOZE,
            MAX_SNOOZES,
            MAX_SNOOZE_DURATION,
        },
    },
    sharechain::{
        block::{Block, CURRENT_CHAIN_ID},
        ShareChain,
    },
};

const PEER_INFO_TOPIC: &str = "peer_info";
const NEW_BLOCK_TOPIC: &str = "new_block";
const SHARE_CHAIN_SYNC_REQ_RESP_PROTOCOL: &str = "/share_chain_sync/1";
const LOG_TARGET: &str = "tari::p2pool::server::p2p";
const MESSAGE_LOGGING_LOG_TARGET: &str = "tari::p2pool::message_logging";
pub const STABLE_PRIVATE_KEY_FILE: &str = "p2pool_private.key";

const MAX_ACCEPTABLE_P2P_MESSAGE_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    pub max_in_progress_sync_requests: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            external_addr: None,
            seed_peers: vec![],
            peer_info_publish_interval: Duration::from_secs(30),
            stable_peer: false,
            private_key_folder: PathBuf::from("."),
            private_key: None,
            mdns_enabled: false,
            relay_server_enabled: false,
            squad: Squad::from("default".to_string()),
            max_in_progress_sync_requests: 10,
        }
    }
}

#[derive(NetworkBehaviour)]
pub struct ServerNetworkBehaviour {
    pub mdns: Toggle<mdns::Behaviour<Tokio>>,
    pub gossipsub: gossipsub::Behaviour,
    pub share_chain_sync: cbor::Behaviour<ShareChainSyncRequest, ShareChainSyncResponse>,
    pub kademlia: kad::Behaviour<MemoryStore>,
    pub identify: identify::Behaviour,
    pub relay_server: Toggle<relay::Behaviour>,
    pub relay_client: relay::client::Behaviour,
    pub dcutr: dcutr::Behaviour,
    pub autonat: autonat::Behaviour,
}

pub enum P2pServiceQuery {
    GetConnectionInfo(oneshot::Sender<ConnectionInfo>),
}

pub struct ConnectionInfo {
    pub listener_addresses: Vec<Multiaddr>,
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
    squad_peer_store: Arc<PeerStore>,
    network_peer_store: Arc<PeerStore>,
    config: Config,
    sync_permits: Arc<AtomicU32>,
    shutdown_signal: ShutdownSignal,
    share_chain_sync_tx: broadcast::Sender<LocalShareChainSyncRequest>,
    share_chain_sync_rx: broadcast::Receiver<LocalShareChainSyncRequest>,

    query_tx: mpsc::Sender<P2pServiceQuery>,
    query_rx: mpsc::Receiver<P2pServiceQuery>,
    // service client related channels
    // TODO: consider mpsc channels instead of broadcast to not miss any message (might drop)
    client_broadcast_block_tx: broadcast::Sender<Block>,
    client_broadcast_block_rx: broadcast::Receiver<Block>,
    snooze_block_tx: mpsc::Sender<(usize, Block)>,
    snooze_block_rx: mpsc::Receiver<(usize, Block)>,

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
        squad_peer_store: Arc<PeerStore>,
        network_peer_store: Arc<PeerStore>,
        shutdown_signal: ShutdownSignal,
    ) -> Result<Self, Error> {
        let swarm = Self::new_swarm(config).await?;

        // client related channels
        let (broadcast_block_tx, broadcast_block_rx) = broadcast::channel::<Block>(1000);
        let (share_chain_sync_tx, share_chain_sync_rx) = broadcast::channel::<LocalShareChainSyncRequest>(1000);
        let (snooze_block_tx, snooze_block_rx) = mpsc::channel::<(usize, Block)>(1000);
        let sync_permits = Arc::new(AtomicU32::new(config.p2p_service.max_in_progress_sync_requests as u32));
        let (query_tx, query_rx) = mpsc::channel(100);

        Ok(Self {
            swarm,
            port: config.p2p_port,
            share_chain_sha3x,
            share_chain_random_x,
            squad_peer_store,
            network_peer_store,
            config: config.p2p_service.clone(),
            shutdown_signal,
            client_broadcast_block_tx: broadcast_block_tx,
            client_broadcast_block_rx: broadcast_block_rx,
            sync_permits,
            share_chain_sync_tx,
            share_chain_sync_rx,
            query_tx,
            query_rx,
            snooze_block_rx,
            snooze_block_tx,
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
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(Self::keypair(&config.p2p_service).await?)
            .with_tokio()
            .with_tcp(tcp::Config::default(), noise::Config::new, yamux::Config::default)
            .map_err(|error| Error::LibP2P(LibP2PError::Noise(error)))?
            .with_relay_client(noise::Config::new, yamux::Config::default)
            .map_err(|error| Error::LibP2P(LibP2PError::Noise(error)))?
            .with_behaviour(|key_pair, relay_client| {
                // .with_behaviour(move |key_pair, relay_client| {
                // gossipsub
                let message_id_fn = |message: &gossipsub::Message| {
                    let mut s = DefaultHasher::new();
                    if let Some(soure_peer) = message.source {
                        soure_peer.to_bytes().hash(&mut s);
                    }
                    message.data.hash(&mut s);
                    gossipsub::MessageId::from(s.finish().to_string())
                };
                let gossipsub_config = gossipsub::ConfigBuilder::default()
                    // .heartbeat_interval(Duration::from_secs(10))
                    // .validation_mode(gossipsub::ValidationMode::Strict)
                     .message_id_fn(message_id_fn)
                    // .max_transmit_size(8192)
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
                let relay_server = if config.p2p_service.relay_server_enabled {
                    Toggle::from(Some(relay::Behaviour::new(
                        key_pair.public().to_peer_id(),
                        Default::default(),
                    )))
                } else {
                    Toggle::from(None)
                };

                Ok(ServerNetworkBehaviour {
                    gossipsub,
                    mdns: mdns_service,
                    share_chain_sync: cbor::Behaviour::<ShareChainSyncRequest, ShareChainSyncResponse>::new(
                        [(
                            StreamProtocol::new(SHARE_CHAIN_SYNC_REQ_RESP_PROTOCOL),
                            request_response::ProtocolSupport::Full,
                        )],
                        request_response::Config::default(),
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
        dbg!("Broadcast peer info");
        // get peer info
        let share_chain_sha3x = self.share_chain_sha3x.clone();
        let share_chain_random_x = self.share_chain_random_x.clone();
        let current_height_sha3x = share_chain_sha3x.tip_height().await.map_err(Error::ShareChain)?;
        let current_height_random_x = share_chain_random_x.tip_height().await.map_err(Error::ShareChain)?;
        let peer_info_network_raw: Vec<u8> =
            PeerInfo::new(current_height_sha3x, current_height_random_x, self.config.squad.clone()).try_into()?;
        let peer_info_squad_raw: Vec<u8> =
            PeerInfo::new(current_height_sha3x, current_height_random_x, self.config.squad.clone()).try_into()?;

        // broadcast peer info to network
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(
                IdentTopic::new(Self::network_topic(PEER_INFO_TOPIC)),
                peer_info_network_raw.clone(),
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

    /// Broadcasting a new mined [`Block`] to the network (assume it is already validated with the network).
    async fn broadcast_block(&mut self, result: Result<Block, RecvError>) {
        dbg!("Broadcast block");
        // if self.sync_in_progress.load(Ordering::SeqCst) {
        //     return;
        // }

        match result {
            Ok(block) => {
                let block_raw_result: Result<Vec<u8>, Error> = block.try_into();
                match block_raw_result {
                    Ok(block_raw) => {
                        match self
                            .swarm
                            .behaviour_mut()
                            .gossipsub
                            .publish(
                                IdentTopic::new(Self::squad_topic(&self.config.squad, NEW_BLOCK_TOPIC)),
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
        self.subscribe(PEER_INFO_TOPIC, false);
        self.subscribe(PEER_INFO_TOPIC, true);
        self.subscribe(NEW_BLOCK_TOPIC, true);
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
                    if payload.squad != self.config.squad {
                        debug!(target: LOG_TARGET, squad = &self.config.squad; "Peer {} is not in the same squad, skipping. Our squad: {}, their squad:{}", peer, self.config.squad, payload.squad);
                        return;
                    }
                    if let ControlFlow::Break(_) = self.add_peer_and_try_sync(payload, peer).await {
                        return;
                    }
                },
                Err(error) => {
                    error!(target: LOG_TARGET, squad = &self.config.squad; "Can't deserialize peer info payload: {:?}", error);
                },
            },
            topic if topic == Self::squad_topic(&self.config.squad, PEER_INFO_TOPIC) => {
                match messages::PeerInfo::try_from(message) {
                    Ok(payload) => {
                        debug!(target: MESSAGE_LOGGING_LOG_TARGET, "[SQUAD_PEERINFO_TOPIC] New peer info: {peer:?} -> {payload:?}");

                        debug!(target: LOG_TARGET, squad = &self.config.squad; "[squad] New peer info: {peer:?} -> {payload:?}");
                        if let ControlFlow::Break(_) = self.add_peer_and_try_sync(payload, peer).await {
                            return;
                        }
                    },
                    Err(error) => {
                        error!(target: LOG_TARGET, squad = &self.config.squad; "Can't deserialize peer info payload: {:?}", error);
                    },
                }
            },
            // TODO: send a signature that proves that the actual block was coming from this peer
            // TODO: (sender peer's wallet address should be included always in the conibases with a fixed percent (like
            // 20%))
            topic if topic == Self::squad_topic(&self.config.squad, NEW_BLOCK_TOPIC) => {
                debug!(target: MESSAGE_LOGGING_LOG_TARGET, "[SQUAD_NEW_BLOCK_TOPIC] New block from gossip: {peer:?}");

                // if self.sync_in_progress.load(Ordering::SeqCst) {
                //     return;
                // }

                match Block::try_from(message) {
                    Ok(payload) => {
                        debug!(target: MESSAGE_LOGGING_LOG_TARGET, "[SQUAD_NEW_BLOCK_TOPIC] New block from gossip: {peer:?} -> {payload:?}");

                        info!(target: LOG_TARGET, squad = &self.config.squad; "🆕 New block from broadcast: {:?}", &payload.hash.to_hex());
                        let share_chain = match payload.original_block_header.pow.pow_algo {
                            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
                            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
                        };
                        // TODO: Treating this as a sync for now.
                        if let Ok((snoozed, num_missing_parents)) =
                            Service::<S>::try_add_propagated_block(&share_chain, payload.clone()).await
                        {
                            self.sync_share_chain(
                                payload.original_block_header.pow.pow_algo,
                                Some(peer),
                                Some(payload.height.saturating_sub(num_missing_parents)),
                            )
                            .await;
                            if snoozed {
                                let _ = self.snooze_block_tx.send((MAX_SNOOZES, payload)).await;
                            }
                        } else {
                            error!(target: LOG_TARGET, squad = &self.config.squad; "Failed to add new block to share chain");
                        }
                    },
                    Err(error) => {
                        error!(target: LOG_TARGET, squad = &self.config.squad; "Can't deserialize broadcast block payload: {:?}", error);
                    },
                }
            },
            _ => {
                debug!(target: MESSAGE_LOGGING_LOG_TARGET, "Unknown topic {topic:?}!");

                warn!(target: LOG_TARGET, squad = &self.config.squad; "Unknown topic {topic:?}!");
            },
        }
    }

    async fn add_peer_and_try_sync(&mut self, payload: PeerInfo, peer: PeerId) -> ControlFlow<()> {
        let current_randomx_height = payload.current_random_x_height;
        let current_sha3x_height = payload.current_sha3x_height;
        self.squad_peer_store.add(peer, payload).await;
        if self.sync_permits.load(Ordering::SeqCst) == 0 {
            debug!(target: LOG_TARGET, squad = &self.config.squad; "Max sync permits reached, skipping peer info sync");
            return ControlFlow::Break(());
        }

        debug!(target: LOG_TARGET, squad = &self.config.squad; "Syncing peer info from {peer:?} with heights: RandomX: {current_randomx_height}, Sha3x: {current_sha3x_height}");
        self.sync_permits.fetch_sub(1, Ordering::SeqCst);
        if let Ok(curr_height) = self.share_chain_sha3x.tip_height().await {
            if curr_height < current_sha3x_height {
                self.sync_share_chain(PowAlgorithm::Sha3x, Some(peer), Some(curr_height.saturating_sub(100)))
                    .await;
            }
        }

        if self.sync_permits.load(Ordering::SeqCst) == 0 {
            debug!(target: LOG_TARGET, squad = &self.config.squad; "Max sync permits reached, skipping peer info sync");
            return ControlFlow::Break(());
        }

        self.sync_permits.fetch_sub(1, Ordering::SeqCst);
        if let Ok(curr_height) = self.share_chain_random_x.tip_height().await {
            if curr_height < current_randomx_height {
                self.sync_share_chain(PowAlgorithm::RandomX, Some(peer), Some(curr_height.saturating_sub(100)))
                    .await;
            }
        }
        ControlFlow::Continue(())
    }

    async fn try_add_propagated_block(
        share_chain: &Arc<S>,
        block: Block,
    ) -> Result<(bool, u64), crate::sharechain::error::Error> {
        match share_chain.add_synced_blocks(vec![block.clone()]).await {
            Ok(_result) => {
                info!(target: LOG_TARGET, "New block added to local share chain via gossip: {}. Height: {}", &block.hash.to_hex(), &block.height);
                Ok((false, 0))
            },
            Err(error) => match error {
                crate::sharechain::error::Error::BlockParentDoesNotExist { num_missing_parents } => {
                    if num_missing_parents < MAX_MISSING_PARENTS_TO_SNOOZE {
                        // let _ = self.snooze_block_tx.send((snoozes_left, block)).await;
                        return Ok((true, num_missing_parents));
                    }
                    error!(target: LOG_TARGET, "Could not add new block to local share chain: {error:?} and too many missing parents");
                    Err(error)
                },
                _ => {
                    error!(target: LOG_TARGET, "Could not add new block to local share chain: {error:?}");
                    Err(error)
                },
            },
        }
    }

    /// Handles share chain sync request (coming from other peer).
    async fn handle_share_chain_sync_request(
        &mut self,
        channel: ResponseChannel<ShareChainSyncResponse>,
        request: ShareChainSyncRequest,
    ) {
        debug!(target: MESSAGE_LOGGING_LOG_TARGET, "Share chain sync request: {request:?}");

        debug!(target: LOG_TARGET, squad = &self.config.squad; "Incoming Share chain sync request: {request:?}");
        let share_chain = match request.algo {
            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
        };
        match share_chain.blocks(request.from_height).await {
            Ok(blocks) => {
                if self
                    .swarm
                    .behaviour_mut()
                    .share_chain_sync
                    .send_response(channel, ShareChainSyncResponse::new(request.algo, blocks.clone()))
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

        let timer = Instant::now();
        // if !self.sync_in_progress.load(Ordering::SeqCst) {
        // return;
        // }
        self.sync_permits.fetch_add(1, Ordering::SeqCst);
        debug!(target: LOG_TARGET, squad = &self.config.squad; "Share chain sync response: {response:?}");
        let share_chain = match response.algo {
            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
        };
        match share_chain.add_synced_blocks(response.blocks).await {
            Ok(result) => {
                info!(target: LOG_TARGET, squad = &self.config.squad; "Synced blocks added to share chain: {result:?}");
                // Ok(())
            },
            Err(error) => {
                error!(target: LOG_TARGET, squad = &self.config.squad; "Failed to add synced blocks to share chain: {error:?}");
            },
        };
        if timer.elapsed() > MAX_ACCEPTABLE_P2P_MESSAGE_TIMEOUT {
            warn!(target: LOG_TARGET, squad = &self.config.squad; "Share chain sync response took too long: {:?}", timer.elapsed());
        }
    }

    /// Trigger share chain sync with another peer with the highest known block height.
    /// Note: this is a "stop-the-world" operation, many operations are skipped when synchronizing.
    async fn sync_share_chain(&mut self, algo: PowAlgorithm, peer: Option<PeerId>, from_tip: Option<u64>) {
        // if self.sync_in_progress.load(Ordering::SeqCst) {
        //     warn!(target: LOG_TARGET, "Sync already in progress...");
        //     return;
        // }
        // self.sync_in_progress.store(true, Ordering::SeqCst);

        debug!(target: LOG_TARGET, squad = &self.config.squad; "Syncing share chain...");

        if let Some(peer_id) = peer {
            info!(target: LOG_TARGET, squad = &self.config.squad; "Send share chain sync request to specific peer: {peer_id:?}");
            self.swarm
                .behaviour_mut()
                .share_chain_sync
                .send_request(&peer_id, ShareChainSyncRequest::new(algo, from_tip.unwrap_or(0)));
            return;
        }

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

    /// Starts an initial share chain synchronization.
    /// This can be called with [`tokio::spawn`].
    /// Note: this is a "stop-the-world" operation, many operations are skipped when synchronizing.
    async fn initial_share_chain_sync(
        in_progress: Arc<AtomicBool>,
        peer_store: Arc<PeerStore>,
        share_chain_sha3x: Arc<S>,
        share_chain_random_x: Arc<S>,
        share_chain_sync_tx: broadcast::Sender<LocalShareChainSyncRequest>,
        timeout: Duration,
        squad: Squad,
        shutdown_signal: ShutdownSignal,
    ) {
        warn!(target: LOG_TARGET, squad = &squad; "Initially syncing share chain (timeout: {timeout:?})...");
        in_progress.store(true, Ordering::SeqCst);
        let sleep = time::sleep(timeout);
        tokio::pin!(sleep);
        tokio::pin!(shutdown_signal);
        loop {
            select! {
                () = &mut sleep => {
                    break;
                }
                _ = &mut shutdown_signal => {
                    info!(target: LOG_TARGET, squad = &squad; "Stopped initial syncing...");
                    return;
                }
                else => {
                    let mut sha3_ready = false;
                    if let Some(result) = peer_store.tip_of_block_height(PowAlgorithm::Sha3x).await {
                        if let Ok(tip) = share_chain_sha3x.tip_height().await {
                            if tip < result.height {
                                sha3_ready = true;
                            }
                        }
                    }

                    let mut randomx_ready = false;
                    if let Some(result) = peer_store.tip_of_block_height(PowAlgorithm::RandomX).await {
                        if let Ok(tip) = share_chain_random_x.tip_height().await {
                            if tip < result.height {
                                randomx_ready = true;
                            }
                        }
                    }

                    if sha3_ready && randomx_ready {
                        break;
                    }
                }
            }
        } // wait for the first height

        let to_sync = vec![
            (PowAlgorithm::Sha3x, share_chain_sha3x.clone()),
            (PowAlgorithm::RandomX, share_chain_random_x.clone()),
        ];
        for (algo, share_chain) in to_sync {
            match peer_store.tip_of_block_height(algo).await {
                Some(result) => {
                    debug!(target: LOG_TARGET, squad = &squad; "Found highest block height: {result:?}");
                    match share_chain.tip_height().await {
                        Ok(tip) => {
                            if tip < result.height {
                                if let Err(error) = share_chain_sync_tx.send(LocalShareChainSyncRequest::new(
                                    result.peer_id,
                                    ShareChainSyncRequest::new(algo, 0),
                                )) {
                                    error!(target: LOG_TARGET, squad = &squad; "Failed to send share chain sync request: {error:?}");
                                }
                            } else {
                                in_progress.store(false, Ordering::SeqCst);
                            }
                        },
                        Err(error) => {
                            in_progress.store(false, Ordering::SeqCst);
                            error!(target: LOG_TARGET, squad = &squad; "Failed to get latest height of share chain: {error:?}")
                        },
                    }
                },
                None => {
                    in_progress.store(false, Ordering::SeqCst);
                    error!(target: LOG_TARGET, squad = &squad; "Failed to get peer with highest share chain height!")
                },
            }
        }
    }

    /// Main method to handle libp2p events.
    #[allow(clippy::too_many_lines)]
    async fn handle_event(&mut self, event: SwarmEvent<ServerNetworkBehaviourEvent>) {
        debug!(target: MESSAGE_LOGGING_LOG_TARGET, "New event: {event:?}");

        match event {
            SwarmEvent::ConnectionEstablished {
                peer_id,
                connection_id,
                endpoint,
                num_established,
                concurrent_dial_errors,
                established_in,
            } => {
                info!(target: LOG_TARGET, squad = &self.config.squad; "Connection established: {peer_id:?} -> {endpoint:?} ({num_established:?}/{concurrent_dial_errors:?}/{established_in:?})");
            },
            SwarmEvent::Dialing { peer_id, connection_id } => {
                info!(target: LOG_TARGET, squad = &self.config.squad; "Dialing: {peer_id:?}");
            },
            SwarmEvent::NewListenAddr { address, .. } => {
                debug!(target: LOG_TARGET, squad = &self.config.squad; "Listening on {address:?}");
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
                        self.sync_permits.fetch_add(1, Ordering::SeqCst);
                        // Remove peer from peer store to try to sync from another peer,
                        // if the peer goes online/accessible again, the peer store will have it again.
                        self.squad_peer_store.remove(&peer).await;
                    },
                    request_response::Event::InboundFailure { peer, error, .. } => {
                        error!(target: LOG_TARGET, squad = &self.config.squad; "REQ-RES inbound failure: {peer:?} -> {error:?}");
                    },
                    request_response::Event::ResponseSent { .. } => {},
                },
                ServerNetworkBehaviourEvent::Kademlia(event) => match event {
                    Event::RoutingUpdated {
                        peer,
                        old_peer,
                        addresses,
                        ..
                    } => {
                        info!(target: LOG_TARGET, squad = &self.config.squad; "Routing updated: {peer:?} -> {addresses:?}");
                        addresses.iter().for_each(|addr| {
                            self.swarm.add_peer_address(peer, addr.clone());
                        });
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

        // self.swarm.add_external_address(info.observed_addr.clone());

        let is_relay = info.protocols.iter().any(|p| *p == relay::HOP_PROTOCOL_NAME);

        // adding peer to kademlia and gossipsub
        for addr in info.listen_addrs {
            // self.swarm.behaviour_mut().kademlia.add_address(&peer_id, addr.clone());
            if Self::is_p2p_address(&addr) && addr.is_global_ip() && is_relay {
                let mut lock = self.relay_store.write().await;
                lock.add_possible_relay(peer_id, addr);
            }
        }

        // Try sync from them
        if self.sync_permits.load(Ordering::SeqCst) > 0 {
            self.sync_permits.fetch_sub(1, Ordering::SeqCst);
            self.sync_share_chain(PowAlgorithm::Sha3x, Some(peer_id), None).await;
            self.sync_permits.fetch_sub(1, Ordering::SeqCst);
            self.sync_share_chain(PowAlgorithm::RandomX, Some(peer_id), None).await;
        }

        // self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
    }

    fn is_p2p_address(address: &Multiaddr) -> bool {
        address.iter().any(|p| matches!(p, Protocol::P2p(_)))
    }

    async fn handle_autonat_event(&mut self, event: autonat::Event) {
        dbg!("Autonat event");
        match event {
            autonat::Event::StatusChanged { old: _old, new } => match self.swarm.behaviour().autonat.public_address() {
                Some(public_address) => {
                    info!(target: LOG_TARGET, "[AUTONAT]: Our public address is {public_address}");
                    // self.swarm
                    //     .behaviour_mut()
                    //     .relay_server
                    //     .set_public_address(public_address.clone());
                },
                None => {
                    warn!(target: LOG_TARGET, "[AUTONAT]: We are behind a NAT, connecting to relay!");
                    let lock = self.relay_store.read().await;
                    if new == NatStatus::Private && !lock.has_active_relay() {
                        drop(lock);
                        self.attempt_relay_reservation().await;
                    }
                },
            },
            _ => {
                debug!(target: LOG_TARGET, "[AUTONAT] {event:?}");
            },
        }
    }

    async fn attempt_relay_reservation(&mut self) {
        dbg!("Attempt relay reservation");
        let mut lock = self.relay_store.write().await;
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
        }
    }

    async fn handle_query(&mut self, query: P2pServiceQuery) {
        match query {
            P2pServiceQuery::GetConnectionInfo(reply) => {
                let connection_info = ConnectionInfo {
                    listener_addresses: self.swarm.external_addresses().cloned().collect(),
                };
                let _ = reply.send(connection_info);
            },
        }
    }

    /// Main loop of the service that drives the events and libp2p swarm forward.
    async fn main_loop(&mut self) -> Result<(), Error> {
        let mut publish_peer_info_interval = tokio::time::interval(self.config.peer_info_publish_interval);
        publish_peer_info_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        // TODO: Not sure why this is done on a loop instead of just once....
        let mut kademlia_bootstrap_interval = tokio::time::interval(Duration::from_secs(12 * 60 * 60));
        kademlia_bootstrap_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let shutdown_signal = self.shutdown_signal.clone();
        tokio::pin!(shutdown_signal);

        loop {
            select! {
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
                event = self.swarm.select_next_some() => {
                   self.handle_event(event).await;
                }
                block = self.client_broadcast_block_rx.recv() => {
                    dbg!("client broadcast");
                    self.broadcast_block(block).await;
                }
                _ = publish_peer_info_interval.tick() => {
                    dbg!("pub peer");
                    // handle case when we have some peers removed
                    let expired_peers = self.squad_peer_store.cleanup().await;
                    for exp_peer in expired_peers {
                        self.swarm.behaviour_mut().kademlia.remove_peer(&exp_peer);
                     //   self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&exp_peer);
                    }

                    // broadcast peer info
                    info!(target: LOG_TARGET, squad = &self.config.squad; "Peer count: {:?}", self.squad_peer_store.peer_count().await);
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
                res = self.snooze_block_rx.recv() => {
                         dbg!("snooze");
                         if let Some((snoozes_left, block)) = res {
                            let snooze_sender = self.snooze_block_tx.clone();
                            let share_chain = match block.original_block_header.pow.pow_algo {
                                PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
                                PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
                            };
                         tokio::spawn(async move {
                            warn!(target: LOG_TARGET, "Trying snoozed block again after {snoozes_left} snoozes left...");
                            tokio::time::sleep(MAX_SNOOZE_DURATION).await;
                            // self.sync_share_chain(
                                // payload.original_block_header.pow.pow_algo,
                                // Some(peer),
                                // Some(payload.height.saturating_sub(num_missing_parents)),
                            // )
                            // .await;
                            if let Ok((snoozed, _num_missing_parents)) = Service::<S>::try_add_propagated_block(&share_chain, block.clone()).await {
                                if snoozed && snoozes_left > 1 {
                                    let _ = snooze_sender.send((snoozes_left - 1, block)).await;
                                }
                            } else {
                                error!(target: LOG_TARGET, "Failed to add snoozed block to share chain");
                            }
                         });
                        } else {
                            error!(target: LOG_TARGET, "Failed to receive snoozed block from channel. Sender dropped?");
                        }
                },
                req = self.share_chain_sync_rx.recv() => {
                    dbg!("share chain sync rx");
                    match req {
                        Ok(request) => {
                            self.swarm.behaviour_mut().share_chain_sync
                                .send_request(&request.peer_id, request.request);
                        }
                        Err(error) => {
                            error!("Failed to receive share chain sync request from channel: {error:?}");
                        }
                    }
                },
                _ = kademlia_bootstrap_interval.tick() => {
                    dbg!("kad boot");
                    if let Err(error) = self.bootstrap_kademlia() {
                        warn!(target: LOG_TARGET, squad = &self.config.squad; "Failed to do kademlia bootstrap: {error:?}");
                    }
                }
            }
        }
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

    pub fn network_peer_store(&self) -> Arc<PeerStore> {
        self.network_peer_store.clone()
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
