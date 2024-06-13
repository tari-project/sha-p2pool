use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use libp2p::{gossipsub, mdns, noise, Swarm, tcp, yamux};
use libp2p::futures::StreamExt;
use libp2p::gossipsub::{Event, IdentTopic, Message, PublishError, Topic};
use libp2p::mdns::tokio::Tokio;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use log::{error, info, warn};
use tokio::{io, select};
use tokio::sync::{broadcast, Mutex};
use tokio::sync::broadcast::error::RecvError;

use crate::server::config;
use crate::server::p2p::{Error, LibP2PError, messages, ServiceClient, ServiceClientChannels};
use crate::server::p2p::messages::{PeerInfo, ValidateBlockRequest, ValidateBlockResult};
use crate::server::p2p::peer_store::PeerStore;
use crate::sharechain::ShareChain;

const PEER_INFO_TOPIC: &str = "peer_info";
const BLOCK_VALIDATION_REQUESTS_TOPIC: &str = "block_validation_requests";
const BLOCK_VALIDATION_RESULTS_TOPIC: &str = "block_validation_results";

#[derive(NetworkBehaviour)]
pub struct ServerNetworkBehaviour {
    pub mdns: mdns::Behaviour<Tokio>,
    pub gossipsub: gossipsub::Behaviour,
    // pub request_response: json::Behaviour<grpc::rpc::>,
}

pub struct Service<S>
    where S: ShareChain + Send + Sync + 'static,
{
    swarm: Swarm<ServerNetworkBehaviour>,
    port: u16,
    share_chain: Arc<S>,
    peer_store: Arc<PeerStore>,

    // service client related channels
    client_validate_block_req_tx: broadcast::Sender<ValidateBlockRequest>,
    client_validate_block_req_rx: broadcast::Receiver<ValidateBlockRequest>,
    client_validate_block_res_tx: broadcast::Sender<ValidateBlockResult>,
    client_validate_block_res_rx: broadcast::Receiver<ValidateBlockResult>,
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
        let (validate_req_tx, validate_req_rx) = broadcast::channel::<ValidateBlockRequest>(1);
        let (validate_res_tx, validate_res_rx) = broadcast::channel::<ValidateBlockResult>(1);

        Ok(Self {
            swarm,
            port: config.p2p_port,
            share_chain,
            peer_store,
            client_validate_block_req_tx: validate_req_tx,
            client_validate_block_req_rx: validate_req_rx,
            client_validate_block_res_tx: validate_res_tx,
            client_validate_block_res_rx: validate_res_rx,
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
                    message.data.hash(&mut s);
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

    fn subscribe(&mut self, topic: &str) {
        self.swarm.behaviour_mut().gossipsub.subscribe(&IdentTopic::new(topic.clone()))
            .expect("must be subscribed to topic");
    }

    fn subscribe_to_topics(&mut self) {
        self.subscribe(PEER_INFO_TOPIC);
        self.subscribe(BLOCK_VALIDATION_REQUESTS_TOPIC);
        self.subscribe(BLOCK_VALIDATION_RESULTS_TOPIC);
    }

    async fn handle_new_message(&mut self, message: Message) {
        let peer = message.source;
        if peer.is_none() {
            warn!("Message source is not set! {:?}", message);
            return;
        }
        let peer = peer.unwrap();

        let topic = message.topic.as_str();
        match topic {
            PEER_INFO_TOPIC => {
                match messages::PeerInfo::try_from(message) {
                    Ok(payload) => {
                        self.peer_store.add(peer, payload);
                        info!("[PEER STORE] Number of peers: {:?}", self.peer_store.peer_count());
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
                            self.swarm.local_peer_id().clone(),
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
            &_ => {
                warn!("Unknown topic {topic:?}!");
            }
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
                    Event::Message { message, message_id: _message_id, propagation_source: _propagation_source } => {
                        self.handle_new_message(message).await;
                    }
                    Event::Subscribed { .. } => {}
                    Event::Unsubscribed { .. } => {}
                    Event::GossipsubNotSupported { .. } => {}
                }
            },
            _ => {}
        };
    }

    async fn main_loop(&mut self) -> Result<(), Error> {
        // TODO: get from config
        let mut publish_peer_info_interval = tokio::time::interval(Duration::from_secs(5));
        let mut client_validate_block_req_rx = self.client_validate_block_req_rx.resubscribe();

        loop {
            select! {
                _ = publish_peer_info_interval.tick() => {
                    self.peer_store.cleanup();
                    if let Err(error) = self.broadcast_peer_info().await {
                        match error {
                            Error::LibP2P(LibP2PError::Publish(PublishError::InsufficientPeers)) => {
                                warn!("No peers to broadcast peer info!");
                            }
                            _ => {
                                error!("Failed to publish node info: {error:?}");
                            }
                        }
                    }
                }
                result = client_validate_block_req_rx.recv() => {
                    self.handle_client_validate_block_request(result).await;
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

