use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use libp2p::{gossipsub, mdns, noise, Swarm, tcp, yamux};
use libp2p::mdns::tokio::Tokio;
use libp2p::swarm::NetworkBehaviour;
use tokio::io;

use crate::server::{config, Error, LibP2PError};

#[derive(NetworkBehaviour)]
pub struct ServerNetworkBehaviour {
    pub mdns: mdns::Behaviour<Tokio>,
    pub gossipsub: gossipsub::Behaviour,
}

pub fn swarm(config: &config::Config) -> Result<Swarm<ServerNetworkBehaviour>, Error> {
    let swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(|e| Error::LibP2P(LibP2PError::Noise(e)))?
        .with_behaviour(move |key_pair| {
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

