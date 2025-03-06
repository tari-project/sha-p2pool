// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Error;
use libp2p::{
    futures::StreamExt,
    gossipsub::{self, Message, MessageAcceptance},
    identify::{self},
    identity::Keypair,
    mdns::{self},
    multiaddr::Protocol,
    request_response::{self},
    swarm::{dial_opts::DialOpts, DialError, SwarmEvent},
    PeerId,
    Swarm,
};
use log::{debug, error, info, trace, warn};
use rand::{seq::SliceRandom, thread_rng};
use tari_shutdown::ShutdownSignal;
use tari_utilities::epoch_time::EpochTime;
use tokio::{
    select,
    sync::{
        mpsc::{self},
        RwLock,
    },
    time::MissedTickBehavior,
};

use crate::{
    diagnostics::config,
    server::{
        http::stats_collector::StatsBroadcastClient,
        p2p::{
            messages::MetaDataResponse,
            peer_store::PeerStore,
            relay_store::RelayStore,
            InnerService,
            P2pServiceQuery,
            ServerNetworkBehaviour,
            ServerNetworkBehaviourEvent,
            ShareChainInfo,
            SubscriptionTopic,
            MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT,
            PEER_INFO_TOPIC,
        },
        PROTOCOL_VERSION,
    },
};

const LOG_TARGET: &str = "tari::p2pool::diagnostics::p2p";
const MESSAGE_LOGGING_LOG_TARGET: &str = LOG_TARGET;
const PEER_INFO_LOGGING_LOG_TARGET: &str = LOG_TARGET;
// Time to start up and catch up before we start processing new tip messages
const NUM_PEERS_TO_META_DATA_EXCHANGE: usize = 8;
const NUM_PEERS_TO_PEER_INFO_EXCHANGE: usize = 8;

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct Config {
    pub external_addr: Option<String>,
    pub seed_peers: Vec<String>,
    pub peer_info_publish_interval: Duration,
    pub stable_peer: bool,
    pub private_key_folder: PathBuf,
    pub private_key: Option<Keypair>,
    pub mdns_enabled: bool,
    pub relay_server_disabled: bool,
    pub squad_override: Option<String>,
    pub squad_prefix: String,
    pub num_squads: usize,
    pub user_agent: String,
    pub grey_list_clear_interval: Duration,
    pub black_list_clear_interval: Duration,
    pub meta_data_exchange_interval: Duration,
    pub peer_exchange_interval: Duration,
    pub is_seed_peer: bool,
    pub debug_print_chain: bool,
    pub sync_job_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            external_addr: None,
            seed_peers: vec![],
            peer_info_publish_interval: Duration::from_secs(60 * 5),
            stable_peer: true,
            private_key_folder: PathBuf::from("."),
            private_key: None,
            mdns_enabled: false,
            relay_server_disabled: false,
            squad_prefix: "default".to_string(),
            squad_override: None,
            num_squads: 1,
            user_agent: "tari-p2pool".to_string(),
            grey_list_clear_interval: Duration::from_secs(60 * 15),
            black_list_clear_interval: Duration::from_secs(60 * 60),
            meta_data_exchange_interval: Duration::from_secs(5),
            peer_exchange_interval: Duration::from_secs(60 * 60),
            is_seed_peer: false,
            debug_print_chain: false,
            sync_job_enabled: true,
        }
    }
}

/// Service is the implementation that holds every peer-to-peer related logic
/// that makes sure that all the communications, syncing, broadcasting etc... are done.
// Allow type complexity for now. It is caused by the hashmap of arc of rwlock of lru cache
// it should be removed in future.
#[allow(clippy::type_complexity)]
pub struct Service {
    config: Config,
    shutdown_signal: ShutdownSignal,
    _query_tx: mpsc::Sender<P2pServiceQuery>,
    query_rx: mpsc::Receiver<P2pServiceQuery>,
    // service client related channels
    // TODO: consider mpsc channels instead of broadcast to not miss any message (might drop)
    stats_broadcast_client: StatsBroadcastClient,
    inner_service: InnerService,
}

impl Service {
    /// Constructs a new Service from the provided config.
    /// It also instantiates libp2p swarm inside.
    pub async fn new(
        config: &config::Config,
        shutdown_signal: ShutdownSignal,
        stats_broadcast_client: StatsBroadcastClient,
        swarm: Swarm<ServerNetworkBehaviour>,
        squad: String,
    ) -> Result<Self, Error> {
        let _res = stats_broadcast_client.send_info_changed(squad.clone(), *swarm.local_peer_id());

        let network_peer_store = PeerStore::new(stats_broadcast_client.clone(), squad.clone());
        // client related channels
        let (_query_tx, query_rx) = mpsc::channel(100);
        Ok(Self {
            config: config.p2p_service.clone(),
            shutdown_signal,
            _query_tx,
            query_rx,
            stats_broadcast_client,
            inner_service: InnerService::new(
                squad,
                swarm,
                Arc::new(RwLock::new(network_peer_store)),
                Arc::new(RwLock::new(RelayStore::default())),
                vec![SubscriptionTopic {
                    topic: PEER_INFO_TOPIC.to_string(),
                    squad: false,
                    not_for_seed_peer: false,
                }],
                config.p2p_service.is_seed_peer,
                config.p2p_port,
                config.p2p_service.seed_peers.clone(),
                config.p2p_service.external_addr.clone(),
            ),
        })
    }

    pub async fn start(&mut self) -> Result<(), Error> {
        self.inner_service.start().await?;
        warn!(target: LOG_TARGET, "Starting main loop");
        self.main_loop().await?;
        info!(target: LOG_TARGET,"P2P service has been stopped!");
        Ok(())
    }

    pub fn local_peer_id(&self) -> PeerId {
        *self.inner_service.swarm.local_peer_id()
    }

    async fn share_chain_info(&self) -> ShareChainInfo {
        ShareChainInfo {
            current_height_sha3x: 1,
            current_height_random_x: 1,
            current_pow_sha3x: 1,
            current_pow_random_x: 1,
            user_agent: self.config.user_agent.clone(),
        }
    }

    /// Main method to handle any message comes from gossipsub.
    #[allow(clippy::too_many_lines)]
    async fn handle_new_gossipsub_message(
        &mut self,
        message: Message,
        _propagation_source: PeerId,
    ) -> Result<MessageAcceptance, Error> {
        debug!(target: MESSAGE_LOGGING_LOG_TARGET, "New gossipsub message: {message:?}");
        let _unused = self.stats_broadcast_client.send_gossipsub_message_received();
        let source_peer = message.source;
        if let Some(source_peer) = source_peer {
            let topic = message.topic.to_string();
            match topic {
                topic if topic == InnerService::network_topic(PEER_INFO_TOPIC) => {
                    return Ok(self.inner_service.handle_peer_info_topic(message, source_peer).await)
                },
                _ => {
                    debug!(target: MESSAGE_LOGGING_LOG_TARGET, "Unknown topic {topic:?}!");
                    warn!(target: LOG_TARGET, "Unknown topic {topic:?}!");
                    return Ok(MessageAcceptance::Reject);
                },
            }
        } else {
            warn!(target: LOG_TARGET, "No peer found for message");
        }
        Ok(MessageAcceptance::Reject)
    }

    async fn handle_meta_data_exchange_response(&mut self, response: MetaDataResponse) {
        if response.info.version != PROTOCOL_VERSION {
            debug!(target: LOG_TARGET, "Peer {} has an outdated version, skipping", response.peer_id);
            return;
        }
        info!(target: PEER_INFO_LOGGING_LOG_TARGET, "[META_DATA_EXCHANGE_RESP] New peer info: {}", response.peer_id);
        match response.peer_id.parse::<PeerId>() {
            Ok(peer_id) => {
                if response.info.squad != self.inner_service.squad {
                    warn!(target: LOG_TARGET, "Peer {} is not in the same squad, skipping", peer_id);
                    let mut are_we_their_relay = false;
                    for address in &response.info.public_addresses() {
                        for protocol in address {
                            if let Protocol::P2p(p2p) = protocol {
                                if p2p == self.local_peer_id() {
                                    are_we_their_relay = true;
                                    break;
                                }
                            }
                        }
                    }
                    if !are_we_their_relay {
                        let _ = self.inner_service.swarm.disconnect_peer_id(peer_id);
                    }
                    return;
                }
                if self.inner_service.add_peer(response.info.clone(), peer_id).await {
                    self.inner_service
                        .swarm
                        .behaviour_mut()
                        .gossipsub
                        .add_explicit_peer(&peer_id);
                }
                // Once we have peer info from the seed peers, disconnect from them.
                if self
                    .inner_service
                    .network_peer_store
                    .read()
                    .await
                    .is_seed_peer(&peer_id)
                {
                    info!(target: LOG_TARGET, "Disconnecting from seed peer {}", peer_id);
                    let _ = self.inner_service.swarm.disconnect_peer_id(peer_id);
                    return;
                }

                // If they are talking an older version, disconnect
                if response.info.version != PROTOCOL_VERSION {
                    warn!(target: LOG_TARGET, "Peer {} has an outdated version, disconnecting", peer_id);
                    let _ = self.inner_service.swarm.disconnect_peer_id(peer_id);
                }
            },
            Err(error) => {
                error!(target: LOG_TARGET, "Failed to parse peer id: {error:?}");
            },
        }
    }

    /// Main method to handle libp2p events.
    #[allow(clippy::too_many_lines)]
    async fn handle_event(&mut self, event: SwarmEvent<ServerNetworkBehaviourEvent>, share_chain_info: ShareChainInfo) {
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
                {
                    if self
                        .inner_service
                        .network_peer_store
                        .read()
                        .await
                        .is_blacklisted(&peer_id)
                    {
                        warn!(
                            target: LOG_TARGET,
                            "Connection established with blacklisted peer: {peer_id:?} -> {endpoint:?} ({num_established:?}/{concurrent_dial_errors:?}/{established_in:?})"
                        );
                        let _ = self.inner_service.swarm.disconnect_peer_id(peer_id);
                        return;
                    }
                }
                info!(
                    target: LOG_TARGET,
                    "Connection established: {peer_id:?} -> {endpoint:?} ({num_established:?}/{concurrent_dial_errors:?}/{established_in:?})"
                );
                // if num_established == NonZeroU32::new(1).expect("Can't fail") {
                self.inner_service
                    .initiate_direct_peer_exchange(share_chain_info, &peer_id)
                    .await;
                // self.inner_service.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                // }
            },
            SwarmEvent::Dialing { peer_id, .. } => {
                info!(target: LOG_TARGET, "Dialing: {peer_id:?}");
            },
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(target: LOG_TARGET, "Listening on {address:?}");
            },
            SwarmEvent::ConnectionClosed {
                peer_id,
                connection_id: _,
                endpoint,
                num_established,
                cause,
            } => {
                // // Ignore dials where we can't get hold of the person
                // if !endpoint.is_dialer() {
                //     warn!(target: LOG_TARGET, squad = &self.config.squad; "Connection closed: {peer_id:?} ->
                // {endpoint:?} ({num_established:?}) -> {cause:?}"); }
                warn!(target: LOG_TARGET, "Connection closed: {peer_id:?} -> {endpoint:?} ({num_established:?}) -> {cause:?}");
            },
            SwarmEvent::IncomingConnectionError {
                connection_id,
                local_addr,
                send_back_addr,
                error,
            } => {
                info!(target: LOG_TARGET, "Incoming connection error: {connection_id:?} -> {local_addr:?} -> {send_back_addr:?} -> {error:?}");
            },
            SwarmEvent::ListenerError { listener_id, error } => {
                error!(target: LOG_TARGET, "Listener error: {listener_id:?} -> {error:?}");
            },
            SwarmEvent::ExternalAddrExpired { address } => {
                warn!(target: LOG_TARGET, "External address has expired: {address:?}. TODO: Do we need to create a new one?");
                self.inner_service.attempt_relay_reservation().await;
            },
            SwarmEvent::OutgoingConnectionError {
                peer_id: Some(peer_id),
                error,
                ..
            } => {
                match error {
                    DialError::Transport(transport_error) => {
                        // There are a lot of cancelled errors, so ignore them
                        warn!(target: LOG_TARGET, "Outgoing connection error, ignoring: {peer_id:?} -> {transport_error:?}");
                    },
                    _ => {
                        warn!(target: LOG_TARGET, "Outgoing connection error: {peer_id:?} -> {error:?}");
                        self.inner_service
                            .network_peer_store
                            .write()
                            .await
                            .move_to_grey_list(peer_id, format!("Outgoing connection error: {error}"));
                    },
                };
            },
            SwarmEvent::Behaviour(event) => {
                match event {
                    ServerNetworkBehaviourEvent::Mdns(mdns_event) => {
                        trace!(target: LOG_TARGET, "ServerNetworkBehaviourEvent::Mdns: {:?}", mdns_event.clone());
                        match mdns_event {
                            mdns::Event::Discovered(peers) => {
                                for (peer, addr) in peers {
                                    self.inner_service.swarm.add_peer_address(peer, addr);
                                    // self.inner_service.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer);
                                }
                            },
                            mdns::Event::Expired(peers) => {
                                for (peer, _addr) in peers {
                                    self.inner_service
                                        .swarm
                                        .behaviour_mut()
                                        .gossipsub
                                        .remove_explicit_peer(&peer);
                                }
                            },
                        }
                    },
                    ServerNetworkBehaviourEvent::Gossipsub(event) => {
                        trace!(target: LOG_TARGET, "ServerNetworkBehaviourEvent::Gossipsub: {:?}", event);
                        match event {
                            gossipsub::Event::Message {
                                message,
                                message_id,
                                propagation_source,
                            } => match self.handle_new_gossipsub_message(message, propagation_source).await {
                                Ok(res) => {
                                    let _unused = self.inner_service.swarm.behaviour_mut().gossipsub.report_message_validation_result(
                                            &message_id,
                                            &propagation_source,
                                            res,
                                        ).inspect_err(|e| {
                                            error!(target: LOG_TARGET, "Failed to report message validation result: {e:?}");
                                        });
                                },
                                Err(error) => {
                                    error!(target: LOG_TARGET, "Failed to handle gossipsub message: {error:?}");
                                    let _unused = self.inner_service.swarm.behaviour_mut().gossipsub.report_message_validation_result(
                                            &message_id,
                                            &propagation_source,
                                            MessageAcceptance::Reject,
                                        ).inspect_err(|e| {
                                            error!(target: LOG_TARGET, "Failed to report message validation result: {e:?}");
                                        });
                                },
                            },
                            gossipsub::Event::Subscribed { peer_id, .. } => {
                                self.inner_service
                                    .initiate_direct_peer_exchange(share_chain_info, &peer_id)
                                    .await;
                            },
                            gossipsub::Event::Unsubscribed { .. } => {},
                            gossipsub::Event::GossipsubNotSupported { .. } => {},
                        }
                    },
                    ServerNetworkBehaviourEvent::MetaDataExchange(event) => {
                        trace!(target: LOG_TARGET, "ServerNetworkBehaviourEvent::MetaDataExchange: {:?}", event);
                        match event {
                            request_response::Event::Message { peer, message } => match message {
                                request_response::Message::Request {
                                    request_id: _request_id,
                                    request,
                                    channel,
                                } => {
                                    self.inner_service
                                        .handle_meta_data_exchange_request(share_chain_info, channel, request)
                                        .await;
                                },
                                request_response::Message::Response {
                                    request_id: _request_id,
                                    response,
                                } => match response {
                                    Ok(response) => {
                                        self.handle_meta_data_exchange_response(response).await;
                                    },
                                    Err(error) => {
                                        error!(target: LOG_TARGET, "REQ-RES peer: {peer} info response error: {error:?}");
                                    },
                                },
                            },
                            request_response::Event::OutboundFailure { peer, error, .. } => {
                                // Peers can be offline
                                debug!(target: LOG_TARGET, "REQ-RES meta data outbound failure: {peer:?} -> {error:?}");
                            },
                            request_response::Event::InboundFailure { peer, error, .. } => {
                                error!(target: LOG_TARGET, "REQ-RES  meta data inbound failure: {peer:?} -> {error:?}");
                            },
                            request_response::Event::ResponseSent { .. } => {},
                        }
                    },
                    ServerNetworkBehaviourEvent::DirectPeerExchange(event) => {
                        trace!(target: LOG_TARGET, "ServerNetworkBehaviourEvent::DirectPeerExchange: {:?}", event);
                        match event {
                            request_response::Event::Message { peer, message } => {
                                trace!(target: PEER_INFO_LOGGING_LOG_TARGET, "DirectPeerExchange: {:?}", message);
                                match message {
                                    request_response::Message::Request {
                                        request_id: _request_id,
                                        request,
                                        channel,
                                    } => {
                                        self.inner_service
                                            .handle_direct_peer_exchange_request(share_chain_info, channel, request)
                                            .await;
                                    },
                                    request_response::Message::Response {
                                        request_id: _request_id,
                                        response,
                                    } => match response {
                                        Ok(response) => {
                                            self.inner_service
                                                .handle_direct_peer_exchange_response(share_chain_info, response)
                                                .await;
                                        },
                                        Err(error) => {
                                            error!(target: LOG_TARGET, "REQ-RES peer: {peer} info response error: {error:?}");
                                        },
                                    },
                                }
                            },
                            request_response::Event::OutboundFailure { peer, error, .. } => {
                                // Peers can be offline
                                debug!(target: LOG_TARGET, "REQ-RES peer info outbound failure: {peer:?} -> {error:?}");
                                // TODO: find out why this errors
                                // self.inner_service.network_peer_store
                                //     .move_to_grey_list(
                                //         peer,
                                //         format!("ShareChainError during direct peer exchange: {}",
                                // error.to_string()),     )
                                //     .await;
                            },
                            request_response::Event::InboundFailure { peer, error, .. } => {
                                error!(target: LOG_TARGET, "REQ-RES  peer info inbound failure: {peer:?} -> {error:?}");
                            },
                            request_response::Event::ResponseSent { .. } => {},
                        }
                    },
                    ServerNetworkBehaviourEvent::ShareChainSync(_) => {
                        debug!(target: LOG_TARGET, "'ServerNetworkBehaviourEvent::ShareChainSync' not handled")
                    },
                    ServerNetworkBehaviourEvent::CatchUpSync(_) => {
                        debug!(target: LOG_TARGET, "'ServerNetworkBehaviourEvent::ShareChainSync' not handled")
                    },
                    ServerNetworkBehaviourEvent::Identify(event) => {
                        trace!(target: LOG_TARGET, "ServerNetworkBehaviourEvent::Identify: {:?}", event);
                        match event {
                            identify::Event::Received { peer_id, info, .. } => {
                                self.inner_service.handle_peer_identified(peer_id, info).await
                            },
                            identify::Event::Error { peer_id, error, .. } => {
                                warn!("Failed to identify peer {peer_id:?}: {error:?}");
                                // self.inner_service.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                                // self.inner_service.swarm.behaviour_mut().kademlia.remove_peer(&peer_id);
                            },
                            _ => {},
                        }
                    },
                    ServerNetworkBehaviourEvent::RelayServer(event) => {
                        info!(target: LOG_TARGET, "[RELAY SERVER]: {event:?}");
                    },
                    ServerNetworkBehaviourEvent::RelayClient(event) => {
                        info!(target: LOG_TARGET, "[RELAY CLIENT]: {event:?}");
                    },
                    ServerNetworkBehaviourEvent::Dcutr(event) => {
                        info!(target: LOG_TARGET, "[DCUTR]: {event:?}");
                    },
                    ServerNetworkBehaviourEvent::Autonat(event) => {
                        trace!(target: LOG_TARGET, "ServerNetworkBehaviourEvent::Autonat: {:?}", event);
                        self.inner_service.handle_autonat_event(event).await
                    },
                    ServerNetworkBehaviourEvent::Ping(event) => {
                        info!(target: LOG_TARGET, "[PING]: {event:?}");
                        // Remove a peer from the greylist if we are in contact with them
                        self.inner_service
                            .network_peer_store
                            .write()
                            .await
                            .set_last_ping(&event.peer, EpochTime::now());
                    },
                }
            },
            _ => {},
        };
    }

    async fn handle_query(&mut self, query: P2pServiceQuery) {
        self.inner_service.handle_query(query).await
    }

    /// Main loop of the service that drives the events and libp2p swarm forward.
    #[allow(clippy::too_many_lines)]
    async fn main_loop(&mut self) -> Result<(), Error> {
        let mut publish_peer_info_interval = tokio::time::interval_at(
            tokio::time::Instant::now() + self.config.peer_info_publish_interval,
            self.config.peer_info_publish_interval,
        );
        publish_peer_info_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut grey_list_clear_interval = tokio::time::interval(self.config.grey_list_clear_interval);
        grey_list_clear_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut black_list_clear_interval = tokio::time::interval(self.config.black_list_clear_interval);
        black_list_clear_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut meta_data_exchange_interval = tokio::time::interval(self.config.meta_data_exchange_interval);
        meta_data_exchange_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut peer_exchange_interval = tokio::time::interval(self.config.peer_exchange_interval);
        peer_exchange_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut connection_stats_publish = tokio::time::interval(Duration::from_secs(10));
        connection_stats_publish.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut seek_connections_interval = tokio::time::interval(Duration::from_secs(20));
        seek_connections_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let shutdown_signal = self.shutdown_signal.clone();
        tokio::pin!(shutdown_signal);
        tokio::pin!(grey_list_clear_interval);
        tokio::pin!(black_list_clear_interval);
        tokio::pin!(meta_data_exchange_interval);
        tokio::pin!(peer_exchange_interval);
        tokio::pin!(connection_stats_publish);
        tokio::pin!(seek_connections_interval);

        let uptime = Instant::now();
        loop {
            // info!(target: LOG_TARGET, "P2P service main loop iter");
            select! {
                // biased;
                _ = &mut shutdown_signal => {
                    info!(target: LOG_TARGET,"Shutting down p2p service...");
                    return Ok(());
                }
                req = self.query_rx.recv() => {
                    let timer = Instant::now();
                    match req {
                        Some(req) => {
                            self.handle_query(req).await;
                        },
                        None => {
                            warn!(target: LOG_TARGET, "Failed to receive query from channel. Sender dropped?");
                        }
                    }
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Query handling took too long: {:?}", timer.elapsed());
                    }

                },
                _ = seek_connections_interval.tick() => {
                    let timer = Instant::now();
                    if !self.config.is_seed_peer {
                        let info = self.inner_service.swarm.network_info();
                        let counters = info.connection_counters();

                        // let num_connections = counters.num_established_incoming() + counters.num_established_outgoing();
                        let num_connections = counters.num_established_outgoing();
                        if num_connections > 8 {
                            continue;
                        }
                         if num_connections == 0 && uptime.elapsed() < Duration::from_secs(60) {

                            match self.inner_service.dial_seed_peers().await {
                                Ok(_) => {},
                                Err(e) => {
                                    warn!(target: LOG_TARGET, "Failed to dial seed peers: {e:?}");
                                },
                            }
                            // continue;
                         }

                        let mut num_dialed = 0;
                        let mut store_write_lock = self.inner_service.network_peer_store.write().await;

                        let mut peers_to_dial = vec![];
                        for record in store_write_lock.best_peers_to_dial(100) {
                            // Only dial seed peers if we have 0 connections
                            if !self.inner_service.swarm.is_connected(&record.peer_id)
                             &&  !store_write_lock.is_seed_peer(&record.peer_id)  {
                                // if &record.peer_id.to_string() != "12D3KooWD6GY3c8cz6AwKaDaqmqGCbmewhjKT5ULN9JUB5oUgWjS" {
                                // store_write_lock.update_last_dial_attempt(&record.peer_id);
                                   // info!(target: LOG_TARGET, "Skipping dialing peer: {:?} with height(rx/sha) {}/{} on {}", record.peer_id, record.peer_info.current_random_x_height, record.peer_info.current_sha3x_height, record.peer_info.public_addresses().iter().map(|a| a.to_string()).collect::<Vec<String>>().join(", "));
                                    // continue;
                                // }
                                store_write_lock.update_last_dial_attempt(&record.peer_id);
                                info!(target: LOG_TARGET, "Dialing peer: {:?} with height(rx/sha) {}/{} on {}", record.peer_id, record.peer_info.current_random_x_height, record.peer_info.current_sha3x_height, record.peer_info.public_addresses().iter().map(|a| a.to_string()).collect::<Vec<String>>().join(", "));
                                let dial_opts=  DialOpts::peer_id(record.peer_id).addresses(record.peer_info.public_addresses().clone()).extend_addresses_through_behaviour().build();
                                // let dial_opts=  DialOpts::peer_id(record.peer_id).addresses(vec!["/ip4/152.228.210.16/tcp/19001/p2p/12D3KooWD6GY3c8cz6AwKaDaqmqGCbmewhjKT5ULN9JUB5oUgWjS".parse().unwrap(), "/ip4/152.228.210.16/udp/19001/quic-v1/p2p/12D3KooWD6GY3c8cz6AwKaDaqmqGCbmewhjKT5ULN9JUB5oUgWjS".parse().unwrap()]).build();
                                // let dial_opts = DialOpts::unknown_peer_id().address("/ip4/152.228.210.16/tcp/19001/p2p/12D3KooWD6GY3c8cz6AwKaDaqmqGCbmewhjKT5ULN9JUB5oUgWjS".parse().unwrap()).build();
                                let _unused = self.inner_service.swarm.dial(dial_opts).map_err(|e| {
                                    warn!(target: LOG_TARGET, "Failed to dial peer: {e:?}");
                                });
                                // self.inner_service.initiate_direct_peer_exchange(&record.peer_id).await;
                                peers_to_dial.push(record.peer_id);
                                num_dialed += 1;
                                // We can only do 30 connections
                                // after 30 it starts cancelling dials
                                if num_dialed > 10 {
                                    break;
                                }
                            }


                        }
                        drop(store_write_lock);
                        // for peer in peers_to_dial {
                        //     self.inner_service.initiate_direct_peer_exchange(&peer).await;
                        // }
                     }
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Seeking connections took too long: {:?}", timer.elapsed());
                    }
                },
                event = self.inner_service.swarm.select_next_some() => {
                    let timer = Instant::now();
                    self.handle_event(event, self.share_chain_info().await).await;
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Event handling took too long: {:?}", timer.elapsed());
                    }
                 },
                _ = publish_peer_info_interval.tick() => {
                    let timer = Instant::now();
                    info!(target: LOG_TARGET, "Publishing peer info");

                    // broadcast peer info
                    if let Err(error) = self.inner_service.broadcast_peer_info(self.share_chain_info().await).await {
                        warn!(target: LOG_TARGET, "Failed to broadcast peer info: {error:?}");
                    }

                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Peer info publishing took too long: {:?}", timer.elapsed());
                    }
                },
                _ = meta_data_exchange_interval.tick() =>  {
                    let timer = Instant::now();
                    if !self.config.is_seed_peer && self.config.sync_job_enabled {
                        let mut connected_peers = self.inner_service.swarm.connected_peers().copied().collect::<Vec::<_>>();
                        let mut rng = thread_rng();
                        connected_peers.shuffle(&mut rng);
                        for peer in connected_peers.iter().take(NUM_PEERS_TO_META_DATA_EXCHANGE) {
                            // Update their latest tip.
                            self.inner_service.initiate_meta_data_exchange(self.share_chain_info().await, peer).await;

                        }
                        // self.try_sync_from_best_peer().await;
                    }
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Chain height exchange took too long: {:?}", timer.elapsed());
                    }
                },

                _ = peer_exchange_interval.tick() =>  {
                    let timer = Instant::now();
                    if !self.config.is_seed_peer && self.config.sync_job_enabled {
                        let mut connected_peers = self.inner_service.swarm.connected_peers().copied().collect::<Vec::<_>>();
                        let mut rng = thread_rng();
                        connected_peers.shuffle(&mut rng);
                        for peer in connected_peers.iter().take(NUM_PEERS_TO_PEER_INFO_EXCHANGE) {
                            // Update their latest tip.
                            self.inner_service.initiate_direct_peer_exchange(self.share_chain_info().await, peer).await;

                        }
                        // self.try_sync_from_best_peer().await;
                    }
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Chain height exchange took too long: {:?}", timer.elapsed());
                    }
                },
               _ = grey_list_clear_interval.tick() => {
                    let timer = Instant::now();
                    self.inner_service.network_peer_store.write().await.clear_grey_list();
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Clearing grey list took too long: {:?}", timer.elapsed());
                    }
                },
                _ = black_list_clear_interval.tick() => {
                    let timer = Instant::now();
                    self.inner_service.network_peer_store.write().await.clear_black_list();
                    if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Clearing black list took too long: {:?}", timer.elapsed());
                    }
                },
                _ = connection_stats_publish.tick() => {
                    let timer = Instant::now();
                   let connection_info = self.inner_service.get_libp2p_connection_info();
                   let _unused = self.stats_broadcast_client.send_libp2p_stats(
                    connection_info.network_info.connection_counters.pending_incoming,
                    connection_info.network_info.connection_counters.pending_outgoing,
                    connection_info.network_info.connection_counters.established_incoming,
                    connection_info.network_info.connection_counters.established_outgoing,
                   );
                   if timer.elapsed() > MAX_ACCEPTABLE_NETWORK_EVENT_TIMEOUT {
                        warn!(target: LOG_TARGET, "Publishing connection stats took too long: {:?}", timer.elapsed());
                    }
                },
            }
        }
    }
}
