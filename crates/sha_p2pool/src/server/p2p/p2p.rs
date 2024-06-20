use std::future::Future;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use libp2p::{gossipsub, mdns, noise, request_response, StreamProtocol, Swarm, tcp, yamux};
use libp2p::futures::StreamExt;
use libp2p::gossipsub::{IdentTopic, Message, MessageId, PublishError, Topic};
use libp2p::mdns::tokio::Tokio;
use libp2p::request_response::{cbor, ResponseChannel};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use log::{error, info, warn};
use tokio::{io, select};
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio::sync::broadcast::error::RecvError;

use crate::server::config;
use crate::server::p2p::{ClientSyncShareChainRequest, ClientSyncShareChainResponse, Error, LibP2PError, messages, ServiceClient, ServiceClientChannels};
use crate::server::p2p::messages::{PeerInfo, ShareChainSyncRequest, ShareChainSyncResponse, ValidateBlockRequest, ValidateBlockResult};
use crate::server::p2p::peer_store::PeerStore;
use crate::sharechain::block::Block;
use crate::sharechain::ShareChain;

const PEER_INFO_TOPIC: &str = "peer_info";
const BLOCK_VALIDATION_REQUESTS_TOPIC: &str = "block_validation_requests";
const BLOCK_VALIDATION_RESULTS_TOPIC: &str = "block_validation_results";
const NEW_BLOCK_TOPIC: &str = "new_block";

#[derive(NetworkBehaviour)]
pub struct ServerNetworkBehaviour {
    pub mdns: mdns::Behaviour<Tokio>,
    pub gossipsub: gossipsub::Behaviour,
    pub share_chain_sync: cbor::Behaviour<ShareChainSyncRequest, ShareChainSyncResponse>,
}

pub struct Service<S>
    where S: ShareChain + Send + Sync + 'static,
{
    swarm: Swarm<ServerNetworkBehaviour>,
    port: u16,
    share_chain: Arc<S>,
    peer_store: Arc<PeerStore>,

    // service client related channels
    // TODO: consider mpsc channels instead of broadcast to not miss any message (might drop)
    client_validate_block_req_tx: broadcast::Sender<ValidateBlockRequest>,
    client_validate_block_req_rx: broadcast::Receiver<ValidateBlockRequest>,
    client_validate_block_res_tx: broadcast::Sender<ValidateBlockResult>,
    client_validate_block_res_rx: broadcast::Receiver<ValidateBlockResult>,
    client_broadcast_block_tx: broadcast::Sender<Block>,
    client_broadcast_block_rx: broadcast::Receiver<Block>,
}

impl<S> Service<S>
    where S: ShareChain + Send + Sync + 'static,
{
    pub fn new(config: &config::Config, share_chain: Arc<S>) -> Result<Self, Error> {
        let swarm = Self::new_swarm(config)?;
        let peer_store = Arc::new(
            PeerStore::new(config.idle_connection_timeout),
        );

        // client related channels
        let (validate_req_tx, validate_req_rx) = broadcast::channel::<ValidateBlockRequest>(1000);
        let (validate_res_tx, validate_res_rx) = broadcast::channel::<ValidateBlockResult>(1000);
        let (broadcast_block_tx, broadcast_block_rx) = broadcast::channel::<Block>(1000);

        Ok(Self {
            swarm,
            port: config.p2p_port,
            share_chain,
            peer_store,
            client_validate_block_req_tx: validate_req_tx,
            client_validate_block_req_rx: validate_req_rx,
            client_validate_block_res_tx: validate_res_tx,
            client_validate_block_res_rx: validate_res_rx,
            client_broadcast_block_tx: broadcast_block_tx,
            client_broadcast_block_rx: broadcast_block_rx,
        })
    }

    fn new_swarm(config: &config::Config) -> Result<Swarm<ServerNetworkBehaviour>, Error> {
        let swarm = libp2p::SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|e| Error::LibP2P(LibP2PError::Noise(e)))?
            .with_behaviour(move |key_pair| {
                // gossipsub
                let message_id_fn = |message: &gossipsub::Message| {
                    let mut s = DefaultHasher::new();
                    if let Some(soure_peer) = message.source {
                        soure_peer.to_bytes().hash(&mut s);
                    }
                    message.data.hash(&mut s);
                    // Instant::now().hash(&mut s);
                    gossipsub::MessageId::from(s.finish().to_string())
                };
                let gossipsub_config = gossipsub::ConfigBuilder::default()
                    .heartbeat_interval(Duration::from_secs(10))
                    .validation_mode(gossipsub::ValidationMode::Strict)
                    .message_id_fn(message_id_fn)
                    .build()
                    .map_err(|msg| io::Error::new(io::ErrorKind::Other, msg))?; // Temporary hack because `build` does not return a proper `std::error::Error`.
                let gossipsub = gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(key_pair.clone()),
                    gossipsub_config,
                )?;

                Ok(ServerNetworkBehaviour {
                    gossipsub,
                    mdns: mdns::Behaviour::new(
                        mdns::Config::default(),
                        key_pair.public().to_peer_id(),
                    )
                        .map_err(|e| Error::LibP2P(LibP2PError::IO(e)))?,
                    share_chain_sync: cbor::Behaviour::<ShareChainSyncRequest, ShareChainSyncResponse>::new(
                        [(
                            StreamProtocol::new("/share_chain_sync/1"),
                            request_response::ProtocolSupport::Full,
                        )],
                        request_response::Config::default(),
                    ),
                })
            })
            .map_err(|e| Error::LibP2P(LibP2PError::Behaviour(e.to_string())))?
            .with_swarm_config(|c| c.with_idle_connection_timeout(config.idle_connection_timeout))
            .build();

        Ok(swarm)
    }

    pub fn client(&self) -> ServiceClient {
        ServiceClient::new(
            ServiceClientChannels::new(
                self.client_validate_block_req_tx.clone(),
                self.client_validate_block_res_rx.resubscribe(),
                self.client_broadcast_block_tx.clone(),
            ),
            self.peer_store.clone(),
        )
    }

    async fn handle_client_validate_block_request(&mut self, result: Result<ValidateBlockRequest, RecvError>) {
        match result {
            Ok(request) => {
                let request_raw_result: Result<Vec<u8>, Error> = request.try_into();
                match request_raw_result {
                    Ok(request_raw) => {
                        match self.swarm.behaviour_mut().gossipsub.publish(
                            IdentTopic::new(BLOCK_VALIDATION_REQUESTS_TOPIC),
                            request_raw,
                        ) {
                            Ok(_) => {}
                            Err(error) => {
                                error!("Failed to send block validation request: {error:?}");
                            }
                        }
                    }
                    Err(error) => {
                        error!("Failed to convert block validation request to bytes: {error:?}");
                    }
                }
            }
            Err(error) => {
                error!("Block validation request receive error: {error:?}");
            }
        }
    }

    async fn send_block_validation_result(&mut self, result: ValidateBlockResult) {
        let result_raw_result: Result<Vec<u8>, Error> = result.try_into();
        match result_raw_result {
            Ok(result_raw) => {
                match self.swarm.behaviour_mut().gossipsub.publish(
                    IdentTopic::new(BLOCK_VALIDATION_RESULTS_TOPIC),
                    result_raw,
                ) {
                    Ok(_) => {}
                    Err(error) => {
                        error!("Failed to publish block validation result: {error:?}");
                    }
                }
            }
            Err(error) => {
                error!("Failed to convert block validation result to bytes: {error:?}");
            }
        }
    }

    async fn broadcast_peer_info(&mut self) -> Result<(), Error> {
        // get peer info
        let share_chain = self.share_chain.clone();
        let current_height = share_chain.tip_height().await
            .map_err(Error::ShareChain)?;
        let peer_info_raw: Vec<u8> = PeerInfo::new(current_height).try_into()?;

        // broadcast peer info
        self.swarm.behaviour_mut().gossipsub.publish(IdentTopic::new(PEER_INFO_TOPIC), peer_info_raw)
            .map_err(|error| Error::LibP2P(LibP2PError::Publish(error)))?;

        Ok(())
    }

    async fn broadcast_block(&mut self, result: Result<Block, RecvError>) {
        match result {
            Ok(block) => {
                let block_raw_result: Result<Vec<u8>, Error> = block.try_into();
                match block_raw_result {
                    Ok(block_raw) => {
                        match self.swarm.behaviour_mut().gossipsub.publish(IdentTopic::new(NEW_BLOCK_TOPIC), block_raw)
                            .map_err(|error| Error::LibP2P(LibP2PError::Publish(error))) {
                            Ok(_) => {}
                            Err(error) => error!("Failed to broadcast new block: {error:?}"),
                        }
                    }
                    Err(error) => error!("Failed to convert block to bytes: {error:?}"),
                }
            }
            Err(error) => error!("Failed to receive new block: {error:?}"),
        }
    }

    fn subscribe(&mut self, topic: &str) {
        self.swarm.behaviour_mut().gossipsub.subscribe(&IdentTopic::new(topic))
            .expect("must be subscribed to topic");
    }

    fn subscribe_to_topics(&mut self) {
        self.subscribe(PEER_INFO_TOPIC);
        self.subscribe(BLOCK_VALIDATION_REQUESTS_TOPIC);
        self.subscribe(BLOCK_VALIDATION_RESULTS_TOPIC);
        self.subscribe(NEW_BLOCK_TOPIC);
    }

    async fn handle_new_gossipsub_message(&mut self, message: Message) {
        let peer = message.source;
        if peer.is_none() {
            warn!("Message source is not set! {:?}", message);
            return;
        }
        let peer = peer.unwrap();

        if peer == *self.swarm.local_peer_id() {
            return;
        }

        let topic = message.topic.as_str();
        match topic {
            PEER_INFO_TOPIC => {
                match messages::PeerInfo::try_from(message) {
                    Ok(payload) => {
                        self.peer_store.add(peer, payload);
                        self.sync_share_chain(ClientSyncShareChainRequest::new(format!("{:p}", self))).await;
                    }
                    Err(error) => {
                        error!("Can't deserialize peer info payload: {:?}", error);
                    }
                }
            }
            BLOCK_VALIDATION_REQUESTS_TOPIC => {
                match messages::ValidateBlockRequest::try_from(message) {
                    Ok(payload) => {
                        info!("Block validation request: {payload:?}");
                        // TODO: validate block
                        let validate_result = ValidateBlockResult::new(
                            *self.swarm.local_peer_id(),
                            payload.block(),
                            true, // TODO: validate block
                        );
                        self.send_block_validation_result(validate_result).await;
                    }
                    Err(error) => {
                        error!("Can't deserialize block validation request payload: {:?}", error);
                    }
                }
            }
            BLOCK_VALIDATION_RESULTS_TOPIC => {
                match messages::ValidateBlockResult::try_from(message) {
                    Ok(payload) => {
                        if let Err(error) = self.client_validate_block_res_tx.send(payload) {
                            error!("Failed to send block validation result to clients: {error:?}");
                        }
                    }
                    Err(error) => {
                        error!("Can't deserialize block validation request payload: {:?}", error);
                    }
                }
            }
            NEW_BLOCK_TOPIC => {
                match Block::try_from(message) {
                    Ok(payload) => {
                        info!("New block from broadcast: {:?}", &payload);
                        if let Err(error) = self.share_chain.submit_block(&payload).await {
                            error!("Could not add new block to local share chain: {error:?}");
                        }
                    }
                    Err(error) => {
                        error!("Can't deserialize broadcast block payload: {:?}", error);
                    }
                }
            }
            &_ => {
                warn!("Unknown topic {topic:?}!");
            }
        }
    }

    async fn handle_share_chain_sync_request(&mut self, channel: ResponseChannel<ShareChainSyncResponse>, request: ShareChainSyncRequest) {
        match self.share_chain.blocks(request.from_height).await {
            Ok(blocks) => {
                match self.swarm.behaviour_mut().share_chain_sync.send_response(channel, ShareChainSyncResponse::new(request.request_id, blocks.clone())) {
                    Ok(_) => {}
                    Err(_) => error!("Failed to send block sync response")
                }
            }
            Err(error) => error!("Failed to get blocks from height: {error:?}"),
        }
    }

    async fn handle_share_chain_sync_response(&mut self, response: ShareChainSyncResponse) {
        if let Err(error) = self.share_chain.submit_blocks(response.blocks).await {
            error!("Failed to add synced blocks to share chain: {error:?}");
        }
    }

    async fn sync_share_chain(&mut self, request: ClientSyncShareChainRequest) {
        while self.peer_store.tip_of_block_height().is_none() {} // waiting for the highest blockchain
        match self.peer_store.tip_of_block_height() {
            Some(result) => {
                match self.share_chain.tip_height().await {
                    Ok(tip) => {
                        self.swarm.behaviour_mut().share_chain_sync.send_request(
                            &result.peer_id,
                            ShareChainSyncRequest::new(request.request_id, tip),
                        );
                    }
                    Err(error) => error!("Failed to get latest height of share chain: {error:?}"),
                }
            }
            None => error!("Failed to get peer with highest share chain height!")
        }
    }

    async fn handle_event(&mut self, event: SwarmEvent<ServerNetworkBehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!("Listening on {address:?}");
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
                        for (peer, addr) in peers {
                            self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer);
                        }
                    }
                },
                ServerNetworkBehaviourEvent::Gossipsub(event) => match event {
                    gossipsub::Event::Message { message, message_id: _message_id, propagation_source: _propagation_source } => {
                        self.handle_new_gossipsub_message(message).await;
                    }
                    gossipsub::Event::Subscribed { .. } => {}
                    gossipsub::Event::Unsubscribed { .. } => {}
                    gossipsub::Event::GossipsubNotSupported { .. } => {}
                },
                ServerNetworkBehaviourEvent::ShareChainSync(event) => match event {
                    request_response::Event::Message { peer, message } => match message {
                        request_response::Message::Request { request_id, request, channel } => {
                            self.handle_share_chain_sync_request(channel, request).await;
                        }
                        request_response::Message::Response { request_id, response } => {
                            self.handle_share_chain_sync_response(response).await;
                        }
                    }
                    request_response::Event::OutboundFailure { .. } => {}
                    request_response::Event::InboundFailure { .. } => {}
                    request_response::Event::ResponseSent { .. } => {}
                }
            },
            _ => {}
        };
    }

    async fn main_loop(&mut self) -> Result<(), Error> {
        // TODO: get from config
        let mut publish_peer_info_interval = tokio::time::interval(Duration::from_secs(5));

        loop {
            select! {
            _ = publish_peer_info_interval.tick() => {
                self.peer_store.cleanup();
                if let Err(error) = self.broadcast_peer_info().await {
                    match error {
                        Error::LibP2P(LibP2PError::Publish(PublishError::InsufficientPeers)) => {
                            warn!("No peers to broadcast peer info!");
                        }
                        Error::LibP2P(LibP2PError::Publish(PublishError::Duplicate)) => {}
                        _ => {
                            error!("Failed to publish node info: {error:?}");
                        }
                    }
                }
            }
            result = self.client_validate_block_req_rx.recv() => {
                self.handle_client_validate_block_request(result).await;
            }
            block = self.client_broadcast_block_rx.recv() => {
                self.broadcast_block(block).await;
            }
            event = self.swarm.select_next_some() => {
               self.handle_event(event).await;
            }
        }
        }
    }

    pub async fn start(&mut self) -> Result<(), Error> {
        self.swarm
            .listen_on(
                format!("/ip4/0.0.0.0/tcp/{}", self.port)
                    .parse()
                    .map_err(|e| Error::LibP2P(LibP2PError::MultiAddrParse(e)))?,
            )
            .map_err(|e| Error::LibP2P(LibP2PError::Transport(e)))?;

        self.subscribe_to_topics();

        self.main_loop().await
    }
}

