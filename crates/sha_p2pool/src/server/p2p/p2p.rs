use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use libp2p::{gossipsub, kad, mdns, noise, PeerId, Swarm, tcp, yamux};
use libp2p::futures::{StreamExt, TryFutureExt};
use libp2p::kad::{Mode, Quorum, Record};
use libp2p::kad::store::MemoryStore;
use libp2p::mdns::tokio::Tokio;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use log::{error, info};
use serde::{Deserialize, Serialize};
use tokio::{io, select};
use tokio::sync::Mutex;

use crate::server::config;
use crate::server::p2p::{Error, LibP2PError};
use crate::sharechain::ShareChain;

#[derive(NetworkBehaviour)]
pub struct ServerNetworkBehaviour {
    pub mdns: mdns::Behaviour<Tokio>,
    pub gossipsub: gossipsub::Behaviour,
    pub kademlia: kad::Behaviour<MemoryStore>,
    // pub request_response: json::Behaviour<grpc::rpc::>,
}

#[derive(Serialize, Deserialize)]
pub struct NodeInfo {
    pub current_height: u64,
}

pub struct Service<S>
    where S: ShareChain + Send + Sync + 'static,
{
    swarm: Arc<Mutex<Swarm<ServerNetworkBehaviour>>>,
    port: u16,
    share_chain: Arc<S>,
}

impl<S> Service<S>
    where S: ShareChain + Send + Sync + 'static,
{
    fn new_swarm(config: &config::Config) -> Result<Swarm<ServerNetworkBehaviour>, Error> {
        let mut swarm = libp2p::SwarmBuilder::with_new_identity()
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

                // grpc
                // let router = Server::builder().add_service(
                //     ShareChainServer::new(ShareChainGrpc::new(InMemoryShareChain::new()))
                // ).into_service();
                // let behaviour = json::Behaviour::<GreetRequest, GreetResponse>::new(
                //     [(StreamProtocol::new("/grpc"), ProtocolSupport::Full)],
                //     request_response::Config::default(),
                // );


                Ok(ServerNetworkBehaviour {
                    gossipsub,
                    mdns: mdns::Behaviour::new(
                        mdns::Config::default(),
                        key_pair.public().to_peer_id(),
                    )
                        .map_err(|e| Error::LibP2P(LibP2PError::IO(e)))?,
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
    pub fn new(config: &config::Config, share_chain: Arc<S>) -> Result<Self, Error> {
        Ok(Self {
            swarm: Arc::new(Mutex::new(Self::new_swarm(config)?)),
            port: config.p2p_port,
            share_chain,
        })
    }

    async fn start_publish_node_info(&self) {
        let swarm = self.swarm.clone();
        let share_chain = self.share_chain.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                select! {
                    _ = interval.tick() => {
                        // TODO: somehow update always the node info value to the latest
                        info!("Publishing node info...");

                        // store current node info in Kademlia DHT
                        let key = swarm.lock().await.local_peer_id().to_base58().into_bytes();
                        let current_height_result = share_chain.tip_height().await;
                        if let Err(error) = current_height_result {
                            error!("Failed to get tip of local share chain: {error:?}");
                            continue;
                        }
                        let node_info_result = serde_cbor::to_vec(&NodeInfo {
                            current_height: current_height_result.unwrap(),
                        }).map_err(Error::Serialize);
                        if let Err(error) = node_info_result {
                            error!("Failed to serialize Node Info: {error:?}");
                            continue;
                        }
                        let query_id_result = swarm.lock().await.behaviour_mut().kademlia.put_record(
                            Record::new(
                                key.clone(),
                                node_info_result.unwrap(),
                            ),
                            Quorum::All,
                        );
                        if let Err(error) = query_id_result {
                            error!("Failed to put kademlia record: {error:?}");
                            continue;
                        }

                        // set ourself as a provider
                        let provide_result =
                        swarm.lock().await.behaviour_mut().kademlia.start_providing(kad::RecordKey::new(&key));
                        if let Err(error) = provide_result {
                            error!("Failed to start providing kademlia record: {error:?}");
                        }
                    }
                }
            }
        });
    }

    async fn event_loop(&self) -> Result<(), Error> {
        loop {
            let mut swarm = self.swarm.lock().await;
            select! {
                 next = swarm.select_next_some() => match next {
                    SwarmEvent::NewListenAddr { address, .. } => {
                        info!("Listening on {address:?}");
                    },
                    SwarmEvent::Behaviour(event) => match event {
                        ServerNetworkBehaviourEvent::Mdns(mdns_event) => match mdns_event {
                          mdns::Event::Discovered(peers) => {
                            for (peer, addr) in peers {
                                info!("Discovered new peer {} at {}", peer, addr);
                                swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer);
                                swarm.behaviour_mut().kademlia.add_address(&peer, addr);
                            }
                          },
                            mdns::Event::Expired(peers) => {
                                for (peer, addr) in peers {
                                    info!("Expired peer {} at {}", peer, addr);
                                    swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer);
                                    swarm.behaviour_mut().kademlia.remove_address(&peer, &addr);
                                }
                            },
                        },
                        ServerNetworkBehaviourEvent::Gossipsub(event) => {
                            info!("[GOSSIP] {event:?}");
                        },
                    ServerNetworkBehaviourEvent::Kademlia(event) => {
                            info!("[Kademlia] {event:?}");
                        }},
                 _ => {}
                }
            }
        }
    }

    pub async fn start(&self) -> Result<(), Error> {
        self.swarm
            .lock()
            .await
            .listen_on(
                format!("/ip4/0.0.0.0/tcp/{}", self.port)
                    .parse()
                    .map_err(|e| Error::LibP2P(LibP2PError::MultiAddrParse(e)))?,
            )
            .map_err(|e| Error::LibP2P(LibP2PError::Transport(e)))?;

        self.start_publish_node_info().await;

        self.event_loop().await
    }
}

