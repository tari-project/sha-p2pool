use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use libp2p::{gossipsub, mdns, noise, PeerId, Swarm, tcp, yamux};
use libp2p::futures::{StreamExt, TryFutureExt};
use libp2p::gossipsub::{Event, IdentTopic, Message, PublishError, Topic};
use libp2p::mdns::tokio::Tokio;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use log::{error, info, warn};
use rand::random;
use serde::{Deserialize, Serialize};
use tokio::{io, select};
use tokio::sync::{Mutex, MutexGuard};

use crate::server::config;
use crate::server::p2p::{Error, LibP2PError, messages};
use crate::server::p2p::messages::PeerInfo;
use crate::server::p2p::peer_store::PeerStore;
use crate::sharechain::ShareChain;

const PEER_INFO_TOPIC: &str = "peer_info";

#[derive(NetworkBehaviour)]
pub struct ServerNetworkBehaviour {
    pub mdns: mdns::Behaviour<Tokio>,
    pub gossipsub: gossipsub::Behaviour,
    // pub request_response: json::Behaviour<grpc::rpc::>,
}

// TODO: implement ServiceClient and wire into TariBaseNodeGrpc
pub struct ServiceClient<S>
    where S: ShareChain + Send + Sync + 'static
{}

pub struct Service<S>
    where S: ShareChain + Send + Sync + 'static,
{
    swarm: Swarm<ServerNetworkBehaviour>,
    port: u16,
    share_chain: Arc<S>,
    peer_store: PeerStore,
}

impl<S> Service<S>
    where S: ShareChain + Send + Sync + 'static,
{
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
    pub fn new(config: &config::Config, share_chain: Arc<S>) -> Result<Self, Error> {
        Ok(Self {
            swarm: Self::new_swarm(config)?,
            port: config.p2p_port,
            share_chain,
            peer_store: PeerStore::new(config.idle_connection_timeout),
        })
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

    async fn subscribe_to_peer_info(&mut self) {
        self.swarm.behaviour_mut().gossipsub.subscribe(&IdentTopic::new(PEER_INFO_TOPIC))
            .expect("must be subscribed to node_info topic");
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
                    }
                    Err(error) => {
                        error!("Can't deserialize node info payload: {:?}", error);
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
                },
                 next = self.swarm.select_next_some() => self.handle_event(next).await,
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

        self.subscribe_to_peer_info().await;

        self.main_loop().await
    }
}

