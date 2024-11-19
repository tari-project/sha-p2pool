// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{num::NonZeroU32, time::Duration};

use anyhow::Error;
use blake2::Blake2b;
use digest::{consts::U32, generic_array::GenericArray, Digest};
use libp2p::{
    autonat::{self},
    connection_limits::{self, ConnectionLimits},
    dcutr,
    gossipsub::{self, Message, MessageId},
    identify,
    identity::Keypair,
    mdns::{self},
    noise,
    ping,
    relay,
    request_response::{self, cbor},
    swarm::behaviour::toggle::Toggle,
    tcp,
    yamux,
    StreamProtocol,
    Swarm,
};
use tokio::{
    fs::File,
    io::{self, AsyncReadExt, AsyncWriteExt},
};

use super::{
    messages::{CatchUpSyncRequest, CatchUpSyncResponse, DirectPeerInfoRequest, DirectPeerInfoResponse},
    Config,
    ServerNetworkBehaviour,
    CATCH_UP_SYNC_REQUEST_RESPONSE_PROTOCOL,
    DIRECT_PEER_EXCHANGE_REQ_RESP_PROTOCOL,
    SHARE_CHAIN_SYNC_REQ_RESP_PROTOCOL,
    STABLE_PRIVATE_KEY_FILE,
};
use crate::server::{
    config,
    p2p::messages::{ShareChainSyncRequest, ShareChainSyncResponse},
};

/// Generates or reads libp2p private key if stable_peer is set to true otherwise returns a random key.
/// Using this method we can be sure that our Peer ID remains the same across restarts in case of
/// stable_peer is set to true.
pub(crate) async fn keypair(config: &Config) -> Result<Keypair, Error> {
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
            return Ok(Keypair::from_protobuf_encoding(content.as_slice())?);
        }
    }

    // otherwise create a new one
    let key_pair = Keypair::generate_ed25519();
    let mut new_private_key_file = File::create_new(key_path).await?;
    new_private_key_file
        .write_all(key_pair.to_protobuf_encoding()?.as_slice())
        .await?;

    Ok(key_pair)
}

pub(crate) async fn new_swarm(config: &config::Config) -> Result<Swarm<ServerNetworkBehaviour>, Error> {
    let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair(&config.p2p_service).await?)
        .with_tokio()
       .with_tcp(tcp::Config::default().nodelay(true), // Nodelay helps with hole punching
         noise::Config::new, yamux::Config::default)
        ?
        .with_quic_config(|mut config| {
            config.handshake_timeout = Duration::from_secs(30);
            config
        })
        .with_relay_client(noise::Config::new, yamux::Config::default)
        ?
        .with_behaviour(|key_pair, relay_client| {
            // .with_behaviour(move |key_pair, relay_client| {
            // gossipsub

            let id_fn = |msg: &Message| {
                let mut hasher = Blake2b::new();
                hasher.update(&msg.data);
                let id : GenericArray<u8, U32> = hasher.finalize();
                MessageId::new(&id)
            };
            let gossipsub_config = gossipsub::ConfigBuilder::default()
                // .fanout_ttl(Duration::from_secs(10))
                // .max_ihave_length(1000) // Default is 5000
                // .max_messages_per_rpc(Some(1000))
                // We get a lot of messages, so 
                //.duplicate_cache_time(Duration::from_secs(1))
                .message_id_fn(id_fn)
                .validate_messages()
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
                        ?,
                ));
            }

            // relay server
      let mut relay_config =  relay::Config{
        ..Default::default()
    };
    if let Some(max) = config.max_relay_circuits  {
        relay_config.max_circuits = max;
        relay_config.max_reservations = max;
    }
    if let Some(max) = config.max_relay_circuits_per_peer {
        relay_config.max_circuits_per_peer = max;
    }

            let relay_server = if config.p2p_service.relay_server_disabled {
                Toggle::from(None)
            } else {
                Toggle::from(Some(relay::Behaviour::new(key_pair.public().to_peer_id(),
            relay_config.reservation_rate_per_ip(NonZeroU32::new(600).expect("can't fail"), Duration::from_secs(60))
            )))
        };

            Ok(ServerNetworkBehaviour {
                gossipsub,
                mdns: mdns_service,
                share_chain_sync: cbor::Behaviour::<ShareChainSyncRequest, ShareChainSyncResponse>::new(
                    [(
                        StreamProtocol::new(SHARE_CHAIN_SYNC_REQ_RESP_PROTOCOL),
                        request_response::ProtocolSupport::Full,
                    )],
                    request_response::Config::default().with_request_timeout(Duration::from_secs(10)), // 10 is the default
                ),
                direct_peer_exchange: cbor::Behaviour::<DirectPeerInfoRequest, DirectPeerInfoResponse>::new(
                    [(
                        StreamProtocol::new(DIRECT_PEER_EXCHANGE_REQ_RESP_PROTOCOL),
                        request_response::ProtocolSupport::Full,
                    )],
                    request_response::Config::default().with_request_timeout(Duration::from_secs(10)), // 10 is the default
                ),
                catch_up_sync: cbor::Behaviour::<CatchUpSyncRequest, CatchUpSyncResponse>::new(
                    [(
                        StreamProtocol::new(CATCH_UP_SYNC_REQUEST_RESPONSE_PROTOCOL),
                        request_response::ProtocolSupport::Full,
                    )],
                    request_response::Config::default().with_request_timeout(Duration::from_secs(30)), // 10 is the default
                ),
                // kademlia: kad::Behaviour::new(
                    // key_pair.public().to_peer_id(),
                    // MemoryStore::new(key_pair.public().to_peer_id()),
                // ),
                identify: identify::Behaviour::new(identify::Config::new(
                    "/p2pool/1.0.0".to_string(),
                    key_pair.public(),
                ).with_push_listen_addr_updates(true)),
                relay_server,
                relay_client,
                dcutr: dcutr::Behaviour::new(key_pair.public().to_peer_id()),
                autonat: autonat::Behaviour::new(key_pair.public().to_peer_id(), Default::default()),
                connection_limits: connection_limits::Behaviour::new(ConnectionLimits::default().with_max_established_incoming(config.max_incoming_connections).with_max_established_outgoing(config.max_outgoing_connections)),
                ping: ping::Behaviour::new(ping::Config::default())
            })
        })
        ?
        // In most cases libp2p will keep connections open that we need. Setting this higher 
        // will make us keep connections open that we don't need.
        // .with_swarm_config(|c| c.with_idle_connection_timeout(config.idle_connection_timeout))
        .build();

    // All nodes are servers
    // swarm.behaviour_mut().kademlia.set_mode(Some(Mode::Server));

    Ok(swarm)
}
