// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use convert_case::{Case, Casing};
use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use hickory_resolver::TokioAsyncResolver;
use itertools::Itertools;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::{dcutr, futures::StreamExt, gossipsub, gossipsub::{IdentTopic, Message, PublishError}, identify, identity::Keypair, kad, kad::{store::MemoryStore, Event, Mode}, mdns, mdns::tokio::Tokio, multiaddr::Protocol, noise, relay, request_response, request_response::{cbor, ResponseChannel}, swarm::{NetworkBehaviour, SwarmEvent}, tcp, yamux, Multiaddr, PeerId, StreamProtocol, Swarm};
use log::kv::{ToValue, Value};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use libp2p::futures::executor::block_on;
use tari_common::configuration::Network;
use tari_core::proof_of_work::PowAlgorithm;
use tari_shutdown::ShutdownSignal;
use tari_utilities::hex::Hex;
use tokio::{
    fs::File,
    io,
    io::{AsyncReadExt, AsyncWriteExt},
    select,
    sync::{broadcast, broadcast::error::RecvError},
    time,
};

use crate::server::p2p::messages::LocalShareChainSyncRequest;
use crate::sharechain::block::CURRENT_CHAIN_ID;
use crate::{
    server::{
        config,
        p2p::{
            messages,
            messages::{PeerInfo, ShareChainSyncRequest, ShareChainSyncResponse},
            peer_store::PeerStore,
            Error, LibP2PError, ServiceClient,
        },
    },
    sharechain::{block::Block, ShareChain},
};

const PEER_INFO_TOPIC: &str = "peer_info";
const NEW_BLOCK_TOPIC: &str = "new_block";
const SHARE_CHAIN_SYNC_REQ_RESP_PROTOCOL: &str = "/share_chain_sync/1";
const LOG_TARGET: &str = "p2pool::server::p2p";
pub const STABLE_PRIVATE_KEY_FILE: &str = "p2pool_private.key";

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Tribe {
    inner: String,
}

impl Tribe {
    pub fn formatted(&self) -> String {
        self.inner.to_case(Case::Lower).replace("_", " ").to_case(Case::Title)
    }
}

impl ToValue for Tribe {
    fn to_value(&self) -> Value {
        Value::from(self.inner.as_str())
    }
}

impl From<String> for Tribe {
    fn from(value: String) -> Self {
        Self {
            inner: value.to_case(Case::Lower).to_case(Case::Snake),
        }
    }
}

impl Display for Tribe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.inner.clone())
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub seed_peers: Vec<String>,
    pub peer_info_publish_interval: Duration,
    pub stable_peer: bool,
    pub private_key_folder: PathBuf,
    pub private_key: Option<Keypair>,
    pub mdns_enabled: bool,
    pub relay_server_enabled: bool,
    pub tribe: Tribe,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            seed_peers: vec![],
            peer_info_publish_interval: Duration::from_secs(5),
            stable_peer: false,
            private_key_folder: PathBuf::from("."),
            private_key: None,
            mdns_enabled: false,
            relay_server_enabled: false,
            tribe: Tribe::from("default".to_string()),
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
}

/// Service is the implementation that holds every peer-to-peer related logic
/// that makes sure that all the communications, syncing, broadcasting etc... are done.
pub struct Service<S>
where
    S: ShareChain,
{
    swarm: Swarm<ServerNetworkBehaviour>,
    port: u16,
    share_chain_sha3x: Arc<S>,
    share_chain_random_x: Arc<S>,
    tribe_peer_store: Arc<PeerStore>,
    network_peer_store: Arc<PeerStore>,
    config: Config,
    sync_in_progress: Arc<AtomicBool>,
    shutdown_signal: ShutdownSignal,
    share_chain_sync_tx: broadcast::Sender<LocalShareChainSyncRequest>,
    share_chain_sync_rx: broadcast::Receiver<LocalShareChainSyncRequest>,

    // service client related channels
    // TODO: consider mpsc channels instead of broadcast to not miss any message (might drop)
    client_broadcast_block_tx: broadcast::Sender<Block>,
    client_broadcast_block_rx: broadcast::Receiver<Block>,
}

impl<S> Service<S>
where
    S: ShareChain,
{
    /// Constructs a new Service from the provided config.
    /// It also instantiates libp2p swarm inside.
    pub async fn new(
        config: &config::Config,
        share_chain_sha3x: Arc<S>,
        share_chain_random_x: Arc<S>,
        tribe_peer_store: Arc<PeerStore>,
        network_peer_store: Arc<PeerStore>,
        sync_in_progress: Arc<AtomicBool>,
        shutdown_signal: ShutdownSignal,
    ) -> Result<Self, Error> {
        let swarm = Self::new_swarm(config).await?;

        // client related channels
        let (broadcast_block_tx, broadcast_block_rx) = broadcast::channel::<Block>(1000);
        let (share_chain_sync_tx, share_chain_sync_rx) = broadcast::channel::<LocalShareChainSyncRequest>(1000);

        Ok(Self {
            swarm,
            port: config.p2p_port,
            share_chain_sha3x,
            share_chain_random_x,
            tribe_peer_store,
            network_peer_store,
            config: config.p2p_service.clone(),
            shutdown_signal,
            client_broadcast_block_tx: broadcast_block_tx,
            client_broadcast_block_rx: broadcast_block_rx,
            sync_in_progress,
            share_chain_sync_tx,
            share_chain_sync_rx,
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
            .map_err(|error| { Error::LibP2P(LibP2PError::Noise(error)) })?
            .with_behaviour(move |key_pair, relay_client| {
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
                    .heartbeat_interval(Duration::from_secs(10))
                    .validation_mode(gossipsub::ValidationMode::Strict)
                    .message_id_fn(message_id_fn)
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
                let relay_server = Toggle::from(Some(relay::Behaviour::new(
                    key_pair.public().to_peer_id(),
                    Default::default(),
                )));

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
                })
            })
            .map_err(|e| Error::LibP2P(LibP2PError::Behaviour(e.to_string())))?
            .with_swarm_config(|c| c.with_idle_connection_timeout(config.idle_connection_timeout))
            .build();

        swarm.behaviour_mut().kademlia.set_mode(Some(Mode::Server));

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
        // get peer info
        let share_chain_sha3x = self.share_chain_sha3x.clone();
        let share_chain_random_x = self.share_chain_random_x.clone();
        let current_height_sha3x = share_chain_sha3x.tip_height().await.map_err(Error::ShareChain)?;
        let current_height_random_x = share_chain_random_x.tip_height().await.map_err(Error::ShareChain)?;
        let peer_info_network_raw: Vec<u8> =
            PeerInfo::new(current_height_sha3x, current_height_random_x, self.config.tribe.clone()).try_into()?;
        let peer_info_tribe_raw: Vec<u8> =
            PeerInfo::new(current_height_sha3x, current_height_random_x, self.config.tribe.clone()).try_into()?;

        // broadcast peer info to network
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(
                IdentTopic::new(Self::network_topic(PEER_INFO_TOPIC)),
                peer_info_network_raw.clone(),
            )
            .map_err(|error| Error::LibP2P(LibP2PError::Publish(error)))?;

        // broadcast peer info to tribe
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(
                IdentTopic::new(Self::tribe_topic(&self.config.tribe, PEER_INFO_TOPIC)),
                peer_info_tribe_raw,
            )
            .map_err(|error| Error::LibP2P(LibP2PError::Publish(error)))?;

        Ok(())
    }

    /// Broadcasting a new mined [`Block`] to the network (assume it is already validated with the network).
    async fn broadcast_block(&mut self, result: Result<Block, RecvError>) {
        if self.sync_in_progress.load(Ordering::SeqCst) {
            return;
        }

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
                                IdentTopic::new(Self::tribe_topic(&self.config.tribe, NEW_BLOCK_TOPIC)),
                                block_raw,
                            )
                            .map_err(|error| Error::LibP2P(LibP2PError::Publish(error)))
                        {
                            Ok(_) => {}
                            Err(error) => {
                                error!(target: LOG_TARGET, tribe = &self.config.tribe; "Failed to broadcast new block: {error:?}")
                            }
                        }
                    }
                    Err(error) => {
                        error!(target: LOG_TARGET, tribe = &self.config.tribe; "Failed to convert block to bytes: {error:?}")
                    }
                }
            }
            Err(error) => {
                error!(target: LOG_TARGET, tribe = &self.config.tribe; "Failed to receive new block: {error:?}")
            }
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
    /// blocks and peers with different Tari networks and the given tribe name.
    fn tribe_topic(tribe: &Tribe, topic: &str) -> String {
        let network = Network::get_current_or_user_setting_or_default().as_key_str();
        let chain_id = CURRENT_CHAIN_ID.clone();
        format!("{network}_{chain_id}_{tribe}_{topic}")
    }

    /// Subscribing to a gossipsub topic.
    fn subscribe(&mut self, topic: &str, tribe: bool) {
        let topic = if tribe {
            Self::tribe_topic(&self.config.tribe, topic)
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
    async fn handle_new_gossipsub_message(&mut self, message: Message) {
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
                    debug!(target: LOG_TARGET, tribe = &self.config.tribe; "[NETWORK] New peer info: {peer:?} -> {payload:?}");
                    self.network_peer_store.add(peer, payload).await;
                }
                Err(error) => {
                    error!(target: LOG_TARGET, tribe = &self.config.tribe; "Can't deserialize peer info payload: {:?}", error);
                }
            },
            topic if topic == Self::tribe_topic(&self.config.tribe, PEER_INFO_TOPIC) => {
                match messages::PeerInfo::try_from(message) {
                    Ok(payload) => {
                        debug!(target: LOG_TARGET, tribe = &self.config.tribe; "[TRIBE] New peer info: {peer:?} -> {payload:?}");
                        self.tribe_peer_store.add(peer, payload).await;

                        // check for SHA-3 tip height
                        if let Some(tip) = self.tribe_peer_store.tip_of_block_height(PowAlgorithm::Sha3x).await {
                            if let Ok(curr_height) = self.share_chain_sha3x.tip_height().await {
                                if curr_height < tip.height {
                                    self.sync_share_chain(PowAlgorithm::Sha3x).await;
                                }
                            }
                        }

                        // check for RandomX tip height
                        if let Some(tip) = self.tribe_peer_store.tip_of_block_height(PowAlgorithm::RandomX).await {
                            if let Ok(curr_height) = self.share_chain_random_x.tip_height().await {
                                if curr_height < tip.height {
                                    self.sync_share_chain(PowAlgorithm::RandomX).await;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        error!(target: LOG_TARGET, tribe = &self.config.tribe; "Can't deserialize peer info payload: {:?}", error);
                    }
                }
            }
            // TODO: send a signature that proves that the actual block was coming from this peer
            // TODO: (sender peer's wallet address should be included always in the conibases with a fixed percent (like 20%))
            topic if topic == Self::tribe_topic(&self.config.tribe, NEW_BLOCK_TOPIC) => {
                if self.sync_in_progress.load(Ordering::SeqCst) {
                    return;
                }

                match Block::try_from(message) {
                    Ok(payload) => {
                        info!(target: LOG_TARGET, tribe = &self.config.tribe; "🆕 New block from broadcast: {:?}", &payload.hash().to_hex());
                        let share_chain = match payload.original_block_header().pow.pow_algo {
                            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
                            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
                        };
                        match share_chain.submit_block(&payload).await {
                            Ok(result) => {
                                if result.need_sync {
                                    self.sync_share_chain(payload.original_block_header().pow.pow_algo)
                                        .await;
                                }
                            }
                            Err(error) => {
                                error!(target: LOG_TARGET, tribe = &self.config.tribe; "Could not add new block to local share chain: {error:?}");
                            }
                        }
                    }
                    Err(error) => {
                        error!(target: LOG_TARGET, tribe = &self.config.tribe; "Can't deserialize broadcast block payload: {:?}", error);
                    }
                }
            }
            _ => {
                warn!(target: LOG_TARGET, tribe = &self.config.tribe; "Unknown topic {topic:?}!");
            }
        }
    }

    /// Handles share chain sync request (coming from other peer).
    async fn handle_share_chain_sync_request(
        &mut self,
        channel: ResponseChannel<ShareChainSyncResponse>,
        request: ShareChainSyncRequest,
    ) {
        debug!(target: LOG_TARGET, tribe = &self.config.tribe; "Incoming Share chain sync request: {request:?}");
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
                    error!(target: LOG_TARGET, tribe = &self.config.tribe; "Failed to send block sync response");
                }
            }
            Err(error) => {
                error!(target: LOG_TARGET, tribe = &self.config.tribe; "Failed to get blocks from height: {error:?}")
            }
        }
    }

    /// Handle share chain sync response.
    /// All the responding blocks will be tried to put into local share chain.
    async fn handle_share_chain_sync_response(&mut self, response: ShareChainSyncResponse) {
        if self.sync_in_progress.load(Ordering::SeqCst) {
            self.sync_in_progress.store(false, Ordering::SeqCst);
        }
        debug!(target: LOG_TARGET, tribe = &self.config.tribe; "Share chain sync response: {response:?}");
        let share_chain = match response.algo {
            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
        };
        match share_chain.submit_blocks(response.blocks, true).await {
            Ok(result) => {
                if result.need_sync {
                    self.sync_share_chain(response.algo).await;
                }
            }
            Err(error) => {
                error!(target: LOG_TARGET, tribe = &self.config.tribe; "Failed to add synced blocks to share chain: {error:?}");
            }
        }
    }

    /// Trigger share chain sync with another peer with the highest known block height.
    /// Note: this is a "stop-the-world" operation, many operations are skipped when synchronizing.
    async fn sync_share_chain(&mut self, algo: PowAlgorithm) {
        if self.sync_in_progress.load(Ordering::Relaxed) {
            warn!(target: LOG_TARGET, "Sync already in progress...");
            return;
        }
        self.sync_in_progress.store(true, Ordering::SeqCst);

        debug!(target: LOG_TARGET, tribe = &self.config.tribe; "Syncing share chain...");
        match self.tribe_peer_store.tip_of_block_height(algo).await {
            Some(result) => {
                debug!(target: LOG_TARGET, tribe = &self.config.tribe; "Found highest known block height: {result:?}");
                debug!(target: LOG_TARGET, tribe = &self.config.tribe; "Send share chain sync request: {result:?}");
                // we always send from_height as zero now, to not miss any blocks
                info!(target: LOG_TARGET, "[{:?}] Syncing share chain...", algo);
                self.swarm
                    .behaviour_mut()
                    .share_chain_sync
                    .send_request(&result.peer_id, ShareChainSyncRequest::new(algo, 0));
            }
            None => {
                self.sync_in_progress.store(false, Ordering::SeqCst);
                error!(target: LOG_TARGET, tribe = &self.config.tribe; "[{:?}] Failed to get peer with highest share chain height!", algo)
            }
        }
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
        tribe: Tribe,
        shutdown_signal: ShutdownSignal,
    ) {
        info!(target: LOG_TARGET, tribe = &tribe; "Initially syncing share chain (timeout: {timeout:?})...");
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
                    info!(target: LOG_TARGET, tribe = &tribe; "Stopped initial syncing...");
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
                    debug!(target: LOG_TARGET, tribe = &tribe; "Found highest block height: {result:?}");
                    match share_chain.tip_height().await {
                        Ok(tip) => {
                            if tip < result.height {
                                if let Err(error) = share_chain_sync_tx.send(LocalShareChainSyncRequest::new(
                                    result.peer_id,
                                    ShareChainSyncRequest::new(algo, 0),
                                )) {
                                    error!(target: LOG_TARGET, tribe = &tribe; "Failed to send share chain sync request: {error:?}");
                                }
                            } else {
                                in_progress.store(false, Ordering::SeqCst);
                            }
                        }
                        Err(error) => {
                            in_progress.store(false, Ordering::SeqCst);
                            error!(target: LOG_TARGET, tribe = &tribe; "Failed to get latest height of share chain: {error:?}")
                        }
                    }
                }
                None => {
                    in_progress.store(false, Ordering::SeqCst);
                    error!(target: LOG_TARGET, tribe = &tribe; "Failed to get peer with highest share chain height!")
                }
            }
        }
    }

    /// Main method to handle libp2p events.
    async fn handle_event(&mut self, event: SwarmEvent<ServerNetworkBehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(target: LOG_TARGET, tribe = &self.config.tribe; "Listening on {address:?}");
            }
            SwarmEvent::Behaviour(event) => match event {
                ServerNetworkBehaviourEvent::Mdns(mdns_event) => match mdns_event {
                    mdns::Event::Discovered(peers) => {
                        for (peer, addr) in peers {
                            self.swarm.add_peer_address(peer, addr);
                            self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer);
                        }
                    }
                    mdns::Event::Expired(peers) => {
                        for (peer, _addr) in peers {
                            self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer);
                        }
                    }
                },
                ServerNetworkBehaviourEvent::Gossipsub(event) => match event {
                    gossipsub::Event::Message {
                        message,
                        message_id: _message_id,
                        propagation_source: _propagation_source,
                    } => {
                        self.handle_new_gossipsub_message(message).await;
                    }
                    gossipsub::Event::Subscribed { .. } => {}
                    gossipsub::Event::Unsubscribed { .. } => {}
                    gossipsub::Event::GossipsubNotSupported { .. } => {}
                },
                ServerNetworkBehaviourEvent::ShareChainSync(event) => match event {
                    request_response::Event::Message { peer: _peer, message } => match message {
                        request_response::Message::Request {
                            request_id: _request_id,
                            request,
                            channel,
                        } => {
                            self.handle_share_chain_sync_request(channel, request).await;
                        }
                        request_response::Message::Response {
                            request_id: _request_id,
                            response,
                        } => {
                            self.handle_share_chain_sync_response(response).await;
                        }
                    },
                    request_response::Event::OutboundFailure { peer, error, .. } => {
                        if self.sync_in_progress.load(Ordering::SeqCst) {
                            self.sync_in_progress.store(false, Ordering::SeqCst);
                        }
                        error!(target: LOG_TARGET, tribe = &self.config.tribe; "REQ-RES outbound failure: {peer:?} -> {error:?}");
                        // Remove peer from peer store to try to sync from another peer,
                        // if the peer goes online/accessible again, the peer store will have it again.
                        self.tribe_peer_store.remove(&peer).await;
                    }
                    request_response::Event::InboundFailure { peer, error, .. } => {
                        if self.sync_in_progress.load(Ordering::SeqCst) {
                            self.sync_in_progress.store(false, Ordering::SeqCst);
                        }
                        error!(target: LOG_TARGET, tribe = &self.config.tribe; "REQ-RES inbound failure: {peer:?} -> {error:?}");
                    }
                    request_response::Event::ResponseSent { .. } => {}
                },
                ServerNetworkBehaviourEvent::Kademlia(event) => match event {
                    Event::RoutingUpdated {
                        peer,
                        old_peer,
                        addresses,
                        ..
                    } => {
                        addresses.iter().for_each(|addr| {
                            self.swarm.add_peer_address(peer, addr.clone());
                        });
                        self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer);
                        if let Some(old_peer) = old_peer {
                            self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&old_peer);
                        }
                    }
                    _ => debug!(target: LOG_TARGET, tribe = &self.config.tribe; "[KADEMLIA] {event:?}"),
                },
                ServerNetworkBehaviourEvent::Identify(event) => match event {
                    identify::Event::Received { peer_id, info } => {
                        self.swarm.add_external_address(info.observed_addr.clone());
                        for addr in info.listen_addrs {
                            self.swarm.behaviour_mut().kademlia.add_address(&peer_id, addr);
                        }
                        self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                    }
                    identify::Event::Error { peer_id, error } => {
                        warn!("Failed to identify peer {peer_id:?}: {error:?}");
                        self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                        self.swarm.behaviour_mut().kademlia.remove_peer(&peer_id);
                    }
                    _ => {}
                },
                ServerNetworkBehaviourEvent::RelayServer(event) => { info!("RELAY SERVER: {event:?}"); }
                ServerNetworkBehaviourEvent::RelayClient(event) => { info!("RELAY CLIENT: {event:?}"); }
                ServerNetworkBehaviourEvent::Dcutr(event) => { info!("DCUTR: {event:?}"); }
            },
            _ => {}
        };
    }

    /// Main loop of the service that drives the events and libp2p swarm forward.
    async fn main_loop(&mut self) -> Result<(), Error> {
        let mut publish_peer_info_interval = tokio::time::interval(self.config.peer_info_publish_interval);
        let mut kademlia_bootstrap_interval = tokio::time::interval(Duration::from_secs(30));
        let shutdown_signal = self.shutdown_signal.clone();
        tokio::pin!(shutdown_signal);

        loop {
            select! {
                _ = &mut shutdown_signal => {
                    info!(target: LOG_TARGET,"Shutting down p2p service...");
                    return Ok(());
                }
                event = self.swarm.select_next_some() => {
                   self.handle_event(event).await;
                }
                block = self.client_broadcast_block_rx.recv() => {
                    self.broadcast_block(block).await;
                }
                _ = publish_peer_info_interval.tick() => {
                    // handle case when we have some peers removed
                    let expired_peers = self.tribe_peer_store.cleanup().await;
                    for exp_peer in expired_peers {
                        self.swarm.behaviour_mut().kademlia.remove_peer(&exp_peer);
                        self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&exp_peer);
                    }

                    // broadcast peer info
                    debug!(target: LOG_TARGET, tribe = &self.config.tribe; "Peer count: {:?}", self.tribe_peer_store.peer_count().await);
                    if let Err(error) = self.broadcast_peer_info().await {
                        match error {
                            Error::LibP2P(LibP2PError::Publish(PublishError::InsufficientPeers)) => {
                                warn!(target: LOG_TARGET, tribe = &self.config.tribe; "No peers to broadcast peer info!");
                            }
                            Error::LibP2P(LibP2PError::Publish(PublishError::Duplicate)) => {}
                            _ => {
                                error!(target: LOG_TARGET, tribe = &self.config.tribe; "Failed to publish node info: {error:?}");
                            }
                        }
                    }
                }
                req = self.share_chain_sync_rx.recv() => {
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
                    if let Err(error) = self.bootstrap_kademlia() {
                        warn!(target: LOG_TARGET, tribe = &self.config.tribe; "Failed to do kademlia bootstrap: {error:?}");
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
                                        }
                                        Err(error) => {
                                            warn!(target: LOG_TARGET, tribe = &self.config.tribe; "Skipping invalid DNS entry: {:?}: {error:?}", chars);
                                        }
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            error!(target: LOG_TARGET, tribe = &self.config.tribe; "Failed to lookup domain records: {error:?}");
                        }
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
            // TODO: do this if we are behind a NAT (use AutoNAT) and we are not providing a relay
            if !self.config.relay_server_enabled {
                self.swarm.dial(addr.clone()).unwrap();
                
                // wait for relay node 
                block_on(async {
                    let mut learned_observed_addr = false;
                    let mut told_relay_observed_addr = false;
                    loop {
                        match self.swarm.next().await.unwrap() {
                            SwarmEvent::Behaviour(ServerNetworkBehaviourEvent::Identify(identify::Event::Sent { .. })) => {
                                told_relay_observed_addr = true;
                            }
                            SwarmEvent::Behaviour(ServerNetworkBehaviourEvent::Identify(identify::Event::Received { .. })) => {
                                learned_observed_addr = true;
                            }
                            _ => {}
                        }
                        if learned_observed_addr && told_relay_observed_addr {
                            break;
                        }
                    }
                });
                
                // listen on relay node
                self.swarm.listen_on(addr.clone().with(Protocol::P2pCircuit))
                    .map_err(|e| Error::LibP2P(LibP2PError::Transport(e))).unwrap();
            }

            self.swarm.behaviour_mut().kademlia.add_address(peer_id, addr.clone());
        });

        self.bootstrap_kademlia()?;

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

    /// Starts p2p service.
    /// Please note that this is a blocking call!
    pub async fn start(&mut self) -> Result<(), Error> {
        // listen on local address
        self.swarm
            .listen_on(
                format!("/ip4/0.0.0.0/tcp/{}", self.port)
                    .parse()
                    .map_err(|e| Error::LibP2P(LibP2PError::MultiAddrParse(e)))?
            )
            .map_err(|e| Error::LibP2P(LibP2PError::Transport(e)))?;

        let seed_peers = self.parse_seed_peers().await?;
        self.join_seed_peers(seed_peers.clone()).await?;
        self.subscribe_to_topics().await;

        // start initial share chain sync
        let in_progress = self.sync_in_progress.clone();
        let peer_store = self.tribe_peer_store.clone();
        let share_chain_sha3x = self.share_chain_sha3x.clone();
        let share_chain_random_x = self.share_chain_random_x.clone();
        let share_chain_sync_tx = self.share_chain_sync_tx.clone();
        let tribe = self.config.tribe.clone();
        let shutdown_signal = self.shutdown_signal.clone();
        tokio::spawn(async move {
            Self::initial_share_chain_sync(
                in_progress,
                peer_store,
                share_chain_sha3x,
                share_chain_random_x,
                share_chain_sync_tx,
                Duration::from_secs(30),
                tribe,
                shutdown_signal,
            )
                .await;
        });

        self.main_loop().await?;
        info!(target: LOG_TARGET,"P2P service has been stopped!");
        Ok(())
    }

    pub fn network_peer_store(&self) -> Arc<PeerStore> {
        self.network_peer_store.clone()
    }
}
