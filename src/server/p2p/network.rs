// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::{
    futures::StreamExt,
    gossipsub,
    gossipsub::{IdentTopic, Message, PublishError},
    identity::Keypair,
    kad,
    kad::{store::MemoryStore, Event, Mode},
    mdns,
    mdns::tokio::Tokio,
    multiaddr::Protocol,
    noise, request_response,
    request_response::{cbor, ResponseChannel},
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, StreamProtocol, Swarm,
};
use log::{debug, error, info, warn};
use tari_common::configuration::Network;
use tari_utilities::hex::Hex;
use tokio::{
    fs::File,
    io,
    io::{AsyncReadExt, AsyncWriteExt},
    select,
    sync::{broadcast, broadcast::error::RecvError},
};

use crate::server::p2p::messages::LocalShareChainSyncRequest;
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
const LOG_TARGET: &str = "p2p_service";
const STABLE_PRIVATE_KEY_FILE: &str = "p2pool_private.key";

#[derive(Clone, Debug)]
pub struct Config {
    pub seed_peers: Vec<String>,
    pub peer_info_publish_interval: Duration,
    pub stable_peer: bool,
    pub private_key_folder: PathBuf,
    pub mdns_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            seed_peers: vec![],
            peer_info_publish_interval: Duration::from_secs(5),
            stable_peer: false,
            private_key_folder: PathBuf::from("."),
            mdns_enabled: false,
        }
    }
}

#[derive(NetworkBehaviour)]
pub struct ServerNetworkBehaviour {
    pub mdns: Toggle<mdns::Behaviour<Tokio>>,
    pub gossipsub: gossipsub::Behaviour,
    pub share_chain_sync: cbor::Behaviour<ShareChainSyncRequest, ShareChainSyncResponse>,
    pub kademlia: kad::Behaviour<MemoryStore>,
}

/// Service is the implementation that holds every peer-to-peer related logic
/// that makes sure that all the communications, syncing, broadcasting etc... are done.
pub struct Service<S>
where
    S: ShareChain + Send + Sync + 'static,
{
    swarm: Swarm<ServerNetworkBehaviour>,
    port: u16,
    share_chain: Arc<S>,
    peer_store: Arc<PeerStore>,
    config: Config,
    sync_in_progress: Arc<AtomicBool>,
    share_chain_sync_tx: broadcast::Sender<LocalShareChainSyncRequest>,
    share_chain_sync_rx: broadcast::Receiver<LocalShareChainSyncRequest>,

    // service client related channels
    // TODO: consider mpsc channels instead of broadcast to not miss any message (might drop)
    client_broadcast_block_tx: broadcast::Sender<Block>,
    client_broadcast_block_rx: broadcast::Receiver<Block>,
}

impl<S> Service<S>
where
    S: ShareChain + Send + Sync + 'static,
{
    /// Constructs a new Service from the provided config.
    /// It also instantiates libp2p swarm inside.
    pub async fn new(
        config: &config::Config,
        share_chain: Arc<S>,
        sync_in_progress: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        let swarm = Self::new_swarm(config).await?;
        let peer_store = Arc::new(PeerStore::new(&config.peer_store));

        // client related channels
        let (broadcast_block_tx, broadcast_block_rx) = broadcast::channel::<Block>(1000);
        let (share_chain_sync_tx, share_chain_sync_rx) = broadcast::channel::<LocalShareChainSyncRequest>(1000);

        Ok(Self {
            swarm,
            port: config.p2p_port,
            share_chain,
            peer_store,
            config: config.p2p_service.clone(),
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

        // if we have a saved private key, just use it
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
            .map_err(|e| Error::LibP2P(LibP2PError::Noise(e)))?
            .with_behaviour(move |key_pair| {
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

                let mut mdns_service = Toggle::from(None);
                if config.p2p_service.mdns_enabled {
                    mdns_service = Toggle::from(Some(
                        mdns::Behaviour::new(mdns::Config::default(), key_pair.public().to_peer_id())
                            .map_err(|e| Error::LibP2P(LibP2PError::IO(e)))?,
                    ));
                }

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
        if self.sync_in_progress.load(Ordering::Relaxed) {
            return Ok(());
        }

        // get peer info
        let share_chain = self.share_chain.clone();
        let current_height = share_chain.tip_height().await.map_err(Error::ShareChain)?;
        let peer_info_raw: Vec<u8> = PeerInfo::new(current_height).try_into()?;

        // broadcast peer info
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(IdentTopic::new(Self::topic_name(PEER_INFO_TOPIC)), peer_info_raw)
            .map_err(|error| Error::LibP2P(LibP2PError::Publish(error)))?;

        Ok(())
    }

    /// Broadcasting a new mined [`Block`] to the network (assume it is already validated with the network).
    async fn broadcast_block(&mut self, result: Result<Block, RecvError>) {
        if self.sync_in_progress.load(Ordering::Relaxed) {
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
                            .publish(IdentTopic::new(Self::topic_name(NEW_BLOCK_TOPIC)), block_raw)
                            .map_err(|error| Error::LibP2P(LibP2PError::Publish(error)))
                        {
                            Ok(_) => {},
                            Err(error) => {
                                error!(target: LOG_TARGET, "Failed to broadcast new block: {error:?}")
                            },
                        }
                    },
                    Err(error) => {
                        error!(target: LOG_TARGET, "Failed to convert block to bytes: {error:?}")
                    },
                }
            },
            Err(error) => error!(target: LOG_TARGET, "Failed to receive new block: {error:?}"),
        }
    }

    /// Generates the gossip sub topic names based on the current Tari network to avoid mixing up
    /// blocks and peers with different Tari networks.
    fn topic_name(topic: &str) -> String {
        let network = Network::get_current_or_user_setting_or_default().to_string();
        format!("{network}_{topic}")
    }

    /// Subscribing to a gossipsub topic.
    fn subscribe(&mut self, topic: &str) {
        self.swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&IdentTopic::new(Self::topic_name(topic)))
            .expect("must be subscribed to topic");
    }

    /// Subscribes to all topics we need.
    fn subscribe_to_topics(&mut self) {
        self.subscribe(PEER_INFO_TOPIC);
        self.subscribe(NEW_BLOCK_TOPIC);
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
            topic if topic == Self::topic_name(PEER_INFO_TOPIC) => match messages::PeerInfo::try_from(message) {
                Ok(payload) => {
                    debug!(target: LOG_TARGET, "New peer info: {peer:?} -> {payload:?}");
                    self.peer_store.add(peer, payload).await;
                    if !self.sync_in_progress.load(Ordering::Relaxed) {
                        if let Some(tip) = self.peer_store.tip_of_block_height().await {
                            if let Ok(curr_height) = self.share_chain.tip_height().await {
                                if curr_height < tip.height {
                                    self.sync_share_chain().await;
                                }
                            }
                        }
                    }
                },
                Err(error) => {
                    error!(target: LOG_TARGET, "Can't deserialize peer info payload: {:?}", error);
                },
            },
            // TODO: send a signature that proves that the actual block was coming from this peer
            // TODO: (sender peer's wallet address should be included always in the conibases with a fixed percent (like 20%))
            topic if topic == Self::topic_name(NEW_BLOCK_TOPIC) => {
                if self.sync_in_progress.load(Ordering::Relaxed) {
                    return;
                }

                match Block::try_from(message) {
                    Ok(payload) => {
                        debug!(target: LOG_TARGET,"🆕 New block from broadcast: {:?}", &payload.hash().to_hex());
                        match self.share_chain.submit_block(&payload).await {
                            Ok(result) => {
                                if result.need_sync {
                                    self.sync_share_chain().await;
                                }
                            },
                            Err(error) => {
                                error!(target: LOG_TARGET, "Could not add new block to local share chain: {error:?}");
                            },
                        }
                    },
                    Err(error) => {
                        error!(target: LOG_TARGET, "Can't deserialize broadcast block payload: {:?}", error);
                    },
                }
            },
            _ => {
                warn!(target: LOG_TARGET, "Unknown topic {topic:?}!");
            },
        }
    }

    /// Handles share chain sync request (coming from other peer).
    async fn handle_share_chain_sync_request(
        &mut self,
        channel: ResponseChannel<ShareChainSyncResponse>,
        request: ShareChainSyncRequest,
    ) {
        debug!(target: LOG_TARGET, "Incoming Share chain sync request: {request:?}");
        match self.share_chain.blocks(request.from_height).await {
            Ok(blocks) => {
                if self
                    .swarm
                    .behaviour_mut()
                    .share_chain_sync
                    .send_response(channel, ShareChainSyncResponse::new(blocks.clone()))
                    .is_err()
                {
                    error!(target: LOG_TARGET, "Failed to send block sync response");
                }
            },
            Err(error) => error!(target: LOG_TARGET, "Failed to get blocks from height: {error:?}"),
        }
    }

    /// Handle share chain sync response.
    /// All the responding blocks will be tried to put into local share chain.
    async fn handle_share_chain_sync_response(&mut self, response: ShareChainSyncResponse) {
        if self.sync_in_progress.load(Ordering::Relaxed) {
            self.sync_in_progress.store(false, Ordering::Relaxed);
        }
        debug!(target: LOG_TARGET, "Share chain sync response: {response:?}");
        match self.share_chain.submit_blocks(response.blocks, true).await {
            Ok(result) => {
                if result.need_sync {
                    self.sync_share_chain().await;
                }
            },
            Err(error) => {
                error!(target: LOG_TARGET, "Failed to add synced blocks to share chain: {error:?}");
            },
        }
    }

    /// Trigger share chain sync with another peer with the highest known block height.
    /// Note: this is a "stop-the-world" operation, many operations are skipped when synchronizing.
    async fn sync_share_chain(&mut self) {
        if self.sync_in_progress.load(Ordering::Relaxed) {
            warn!(target: LOG_TARGET, "Sync already in progress...");
            return;
        }
        self.sync_in_progress.store(true, Ordering::Relaxed);

        debug!(target: LOG_TARGET, "Syncing share chain...");
        match self.peer_store.tip_of_block_height().await {
            Some(result) => {
                debug!(target: LOG_TARGET, "Found highest known block height: {result:?}");
                debug!(target: LOG_TARGET, "Send share chain sync request: {result:?}");
                // we always send from_height as zero now, to not miss any blocks
                self.swarm
                    .behaviour_mut()
                    .share_chain_sync
                    .send_request(&result.peer_id, ShareChainSyncRequest::new(0));
            },
            None => {
                self.sync_in_progress.store(false, Ordering::Relaxed);
                error!(target: LOG_TARGET, "Failed to get peer with highest share chain height!")
            },
        }
    }

    /// Starts an initial share chain synchronization.
    /// This can be called with [`tokio::spawn`].
    /// Note: this is a "stop-the-world" operation, many operations are skipped when synchronizing.
    async fn initial_share_chain_sync(
        in_progress: Arc<AtomicBool>,
        peer_store: Arc<PeerStore>,
        share_chain: Arc<S>,
        share_chain_sync_tx: broadcast::Sender<LocalShareChainSyncRequest>,
        timeout: Duration,
    ) {
        info!(target: LOG_TARGET, "Initially syncing share chain (timeout: {timeout:?})...");
        in_progress.store(true, Ordering::Relaxed);
        let start = Instant::now();
        loop {
            if Instant::now().duration_since(start) >= timeout {
                break;
            }
            if let Some(result) = peer_store.tip_of_block_height().await {
                if let Ok(tip) = share_chain.tip_height().await {
                    if tip < result.height {
                        break;
                    }
                }
            }
        } // wait for the first height
        match peer_store.tip_of_block_height().await {
            Some(result) => {
                debug!(target: LOG_TARGET, "Found highest block height: {result:?}");
                match share_chain.tip_height().await {
                    Ok(tip) => {
                        if tip < result.height {
                            if let Err(error) = share_chain_sync_tx.send(LocalShareChainSyncRequest::new(
                                result.peer_id,
                                ShareChainSyncRequest::new(0),
                            )) {
                                error!("Failed to send share chain sync request: {error:?}");
                            }
                        } else {
                            in_progress.store(false, Ordering::Relaxed);
                        }
                    },
                    Err(error) => {
                        in_progress.store(false, Ordering::Relaxed);
                        error!(target: LOG_TARGET, "Failed to get latest height of share chain: {error:?}")
                    },
                }
            },
            None => {
                in_progress.store(false, Ordering::Relaxed);
                error!(target: LOG_TARGET, "Failed to get peer with highest share chain height!")
            },
        }
    }

    /// Main method to handle libp2p events.
    async fn handle_event(&mut self, event: SwarmEvent<ServerNetworkBehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(target: LOG_TARGET, "Listening on {address:?}");
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
                        error!(target: LOG_TARGET, "REQ-RES outbound failure: {peer:?} -> {error:?}");
                    },
                    request_response::Event::InboundFailure { peer, error, .. } => {
                        error!(target: LOG_TARGET, "REQ-RES inbound failure: {peer:?} -> {error:?}");
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
                        addresses.iter().for_each(|addr| {
                            self.swarm.add_peer_address(peer, addr.clone());
                        });
                        self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer);
                        if let Some(old_peer) = old_peer {
                            self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&old_peer);
                        }
                    },
                    _ => debug!(target: LOG_TARGET, "[KADEMLIA] {event:?}"),
                },
            },
            _ => {},
        };
    }

    /// Main loop of the service that drives the events and libp2p swarm forward.
    async fn main_loop(&mut self) -> Result<(), Error> {
        let mut publish_peer_info_interval = tokio::time::interval(self.config.peer_info_publish_interval);

        loop {
            select! {
                event = self.swarm.select_next_some() => {
                   self.handle_event(event).await;
                }
                block = self.client_broadcast_block_rx.recv() => {
                    self.broadcast_block(block).await;
                }
                _ = publish_peer_info_interval.tick() => {
                    // handle case when we have some peers removed
                    let expired_peers = self.peer_store.cleanup().await;
                    for exp_peer in expired_peers {
                        self.swarm.behaviour_mut().kademlia.remove_peer(&exp_peer);
                        self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&exp_peer);
                    }
                    match self.share_chain.tip_height().await {
                        Ok(tip) => {
                            info!(target: LOG_TARGET, "Share chain: {tip:?}");
                        },
                        Err(error) => {
                            error!(target: LOG_TARGET, "Failed to show tip height: {error:?}");
                        },
                    }

                    // broadcast peer info
                    debug!(target: LOG_TARGET, "Peer count: {:?}", self.peer_store.peer_count().await);
                    if let Err(error) = self.broadcast_peer_info().await {
                        match error {
                            Error::LibP2P(LibP2PError::Publish(PublishError::InsufficientPeers)) => {
                                warn!(target: LOG_TARGET, "No peers to broadcast peer info!");
                            }
                            Error::LibP2P(LibP2PError::Publish(PublishError::Duplicate)) => {}
                            _ => {
                                error!(target: LOG_TARGET, "Failed to publish node info: {error:?}");
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
            }
        }
    }

    // TODO: add support for dns addresses without peer ID
    /// Adding all peer addresses to kademlia DHT and run bootstrap to get peers.
    fn join_seed_peers(&mut self) -> Result<(), Error> {
        if self.config.seed_peers.is_empty() {
            return Ok(());
        }

        for seed_peer in &self.config.seed_peers {
            let addr = seed_peer
                .parse::<Multiaddr>()
                .map_err(|error| Error::LibP2P(LibP2PError::MultiAddrParse(error)))?;
            let peer_id = match addr.iter().last() {
                Some(Protocol::P2p(peer_id)) => Some(peer_id),
                _ => None,
            };
            if peer_id.is_none() {
                return Err(Error::LibP2P(LibP2PError::MissingPeerId(seed_peer.clone())));
            }
            self.swarm.behaviour_mut().kademlia.add_address(&peer_id.unwrap(), addr);
        }

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
        self.swarm
            .listen_on(
                format!("/ip4/0.0.0.0/tcp/{}", self.port)
                    .parse()
                    .map_err(|e| Error::LibP2P(LibP2PError::MultiAddrParse(e)))?,
            )
            .map_err(|e| Error::LibP2P(LibP2PError::Transport(e)))?;

        self.join_seed_peers()?;
        self.subscribe_to_topics();

        // start initial share chain sync
        let in_progress = self.sync_in_progress.clone();
        let peer_store = self.peer_store.clone();
        let share_chain = self.share_chain.clone();
        let share_chain_sync_tx = self.share_chain_sync_tx.clone();
        let handle = tokio::spawn(async move {
            Self::initial_share_chain_sync(
                in_progress,
                peer_store,
                share_chain,
                share_chain_sync_tx,
                Duration::from_secs(30),
            )
            .await;
        });

        self.main_loop().await
    }
}
