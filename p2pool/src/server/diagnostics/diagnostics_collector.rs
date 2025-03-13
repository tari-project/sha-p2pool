// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{collections::HashMap, fmt::Debug, time::Duration};

use libp2p::PeerId;
use log::*;
use serde::Serialize;
use tari_shutdown::ShutdownSignal;
use tari_utilities::epoch_time::EpochTime;
use tokio::{
    sync::{broadcast::Receiver, oneshot},
    time::MissedTickBehavior,
};

#[derive(Serialize, Clone, Debug)]
pub struct DiagnosticPeerInfo {
    pub peer_id: PeerId,
    pub connected_at: Option<EpochTime>,
    pub dial_time: Duration,
    pub requested_peers_at: Option<EpochTime>,
    pub number_of_peers: Option<u64>,
    pub response_time: Option<Duration>,
}

const LOG_TARGET: &str = "tari::p2pool::diagnostics_collector";
pub struct DiagnosticsCollector {
    shutdown_signal: ShutdownSignal,
    broadcast_receiver: tokio::sync::broadcast::Receiver<DiagnosticData>,
    request_tx: tokio::sync::mpsc::Sender<DiagnosticRequest>,
    request_rx: tokio::sync::mpsc::Receiver<DiagnosticRequest>,
    first_data_received: Option<EpochTime>,
    seed_peer_info: HashMap<String, DiagnosticPeerInfo>,
    peer_info: HashMap<String, DiagnosticPeerInfo>,
    relay_peer_info: HashMap<String, DiagnosticPeerInfo>,
}

impl DiagnosticsCollector {
    pub(crate) fn new(shutdown_signal: ShutdownSignal, broadcast_receiver: Receiver<DiagnosticData>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        Self {
            shutdown_signal,
            broadcast_receiver,
            request_rx: rx,
            request_tx: tx,
            first_data_received: None,
            seed_peer_info: HashMap::new(),
            peer_info: HashMap::new(),
            relay_peer_info: HashMap::new(),
        }
    }

    pub fn create_receiver_client(&self) -> DiagnosticsReceiverClient {
        DiagnosticsReceiverClient {
            request_tx: self.request_tx.clone(),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn handle_diagnostic(&mut self, data: DiagnosticData) {
        match data {
            DiagnosticData::NewSeedPeer {
                peer_id,
                dial_time,
                connected,
                ..
            } => {
                self.seed_peer_info.insert(peer_id.to_base58(), DiagnosticPeerInfo {
                    peer_id,
                    connected_at: if connected { Some(EpochTime::now()) } else { None },
                    dial_time,
                    requested_peers_at: None,
                    number_of_peers: None,
                    response_time: None,
                });
            },
            DiagnosticData::SeedPeerRequest { peer_id, .. } => {
                if let Some(peer) = self.seed_peer_info.get_mut(&peer_id.to_base58()) {
                    peer.requested_peers_at = Some(EpochTime::now());
                }
            },
            DiagnosticData::NewPeer {
                peer_id,
                dial_time,
                connected,
                ..
            } => {
                self.peer_info.insert(peer_id.to_base58(), DiagnosticPeerInfo {
                    peer_id,
                    connected_at: if connected { Some(EpochTime::now()) } else { None },
                    dial_time,
                    requested_peers_at: None,
                    number_of_peers: None,
                    response_time: None,
                });
            },
            DiagnosticData::PeerRequest { peer_id, .. } => {
                if let Some(peer) = self.peer_info.get_mut(&peer_id.to_base58()) {
                    peer.requested_peers_at = Some(EpochTime::now());
                }
            },
            DiagnosticData::NewRelayPeer {
                peer_id,
                dial_time,
                connected,
                ..
            } => {
                self.relay_peer_info.insert(peer_id.to_base58(), DiagnosticPeerInfo {
                    peer_id,
                    connected_at: if connected { Some(EpochTime::now()) } else { None },
                    dial_time,
                    requested_peers_at: None,
                    number_of_peers: None,
                    response_time: None,
                });
            },
            DiagnosticData::RelayPeerRequest { peer_id, .. } => {
                if let Some(peer) = self.relay_peer_info.get_mut(&peer_id.to_base58()) {
                    peer.requested_peers_at = Some(EpochTime::now());
                }
            },
            DiagnosticData::PeerResponse {
                peer_id,
                number_of_peers,
                ..
            } => {
                if let Some(peer) = self.peer_info.get_mut(&peer_id.to_base58()) {
                    peer.number_of_peers = Some(number_of_peers);
                    peer.response_time = peer.requested_peers_at.and_then(|requested_peers_at| {
                        EpochTime::now()
                            .checked_sub(requested_peers_at)
                            .map(|epoch_time| Duration::from_secs(epoch_time.as_u64()))
                    });
                } else if let Some(peer) = self.seed_peer_info.get_mut(&peer_id.to_base58()) {
                    peer.number_of_peers = Some(number_of_peers);
                    peer.response_time = peer.requested_peers_at.and_then(|requested_peers_at| {
                        EpochTime::now()
                            .checked_sub(requested_peers_at)
                            .map(|epoch_time| Duration::from_secs(epoch_time.as_u64()))
                    });
                } else if let Some(peer) = self.relay_peer_info.get_mut(&peer_id.to_base58()) {
                    peer.number_of_peers = Some(number_of_peers);
                    peer.response_time = peer.requested_peers_at.and_then(|requested_peers_at| {
                        EpochTime::now()
                            .checked_sub(requested_peers_at)
                            .map(|epoch_time| Duration::from_secs(epoch_time.as_u64()))
                    });
                } else {
                    // Nothing here
                }
            },
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn run(&mut self) -> Result<(), anyhow::Error> {
        let mut diagnostics_report_timer = tokio::time::interval(tokio::time::Duration::from_secs(10));
        diagnostics_report_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = self.shutdown_signal.wait() => {
                    break;
                },
                _ = diagnostics_report_timer.tick() => {
                    info!(
                        target: LOG_TARGET,
                        "========= Uptime: {}. Seeds: {}, Peers: {}, Relay peers: {} ==== ",
                        humantime::format_duration(Duration::from_secs(EpochTime::now().as_u64().checked_sub(
                            self.first_data_received.unwrap_or(EpochTime::now()).as_u64()
                        ).unwrap_or_default())),
                        self.seed_peer_info.len(),
                        self.peer_info.len(),
                        self.relay_peer_info.len(),
                    );
                },
                res = self.request_rx.recv() => {
                    match res {
                        Some(DiagnosticRequest::GetSeedPeerInfo(tx)) => {
                            let _unused  = tx.send(self.seed_peer_info.clone())
                                .inspect_err(|e|
                                    error!(target: LOG_TARGET, "Error with seed peers diagnostics response: {:?}", e)
                                );
                        },
                        Some(DiagnosticRequest::GetPeerInfo(tx)) => {
                            let _unused  = tx.send(self.peer_info.clone())
                                .inspect_err(|e|
                                    error!(target: LOG_TARGET, "Error with peers diagnostics response: {:?}", e)
                                );
                        },
                        Some(DiagnosticRequest::GetRelayPeerInfo(tx)) => {
                            let _unused  = tx.send(self.relay_peer_info.clone())
                                .inspect_err(|e|
                                    error!(target: LOG_TARGET, "Error with relay peers diagnostics response: {:?}", e)
                                );
                        },
                        None => {
                            break;
                        }
                    }
                },
                res = self.broadcast_receiver.recv() => {
                    match res {
                        Ok(data) => {
                            if self.first_data_received.is_none() {
                                self.first_data_received = Some(data.timestamp());
                            }
                            self.handle_diagnostic(data);
                        },
                        Err(e) => {
                            error!(target: LOG_TARGET, "Error receiving diagnostic data: {:?}", e);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) enum DiagnosticData {
    NewSeedPeer {
        peer_id: PeerId,
        dial_time: Duration,
        connected: bool,
        timestamp: EpochTime,
    },
    SeedPeerRequest {
        peer_id: PeerId,
        timestamp: EpochTime,
    },
    NewPeer {
        peer_id: PeerId,
        dial_time: Duration,
        connected: bool,
        timestamp: EpochTime,
    },
    PeerRequest {
        peer_id: PeerId,
        timestamp: EpochTime,
    },
    NewRelayPeer {
        peer_id: PeerId,
        dial_time: Duration,
        connected: bool,
        timestamp: EpochTime,
    },
    RelayPeerRequest {
        peer_id: PeerId,
        timestamp: EpochTime,
    },
    PeerResponse {
        peer_id: PeerId,
        number_of_peers: u64,
        timestamp: EpochTime,
    },
}

impl DiagnosticData {
    pub fn timestamp(&self) -> EpochTime {
        match self {
            DiagnosticData::NewSeedPeer { timestamp, .. } => *timestamp,
            DiagnosticData::SeedPeerRequest { timestamp, .. } => *timestamp,
            DiagnosticData::NewPeer { timestamp, .. } => *timestamp,
            DiagnosticData::PeerRequest { timestamp, .. } => *timestamp,
            DiagnosticData::NewRelayPeer { timestamp, .. } => *timestamp,
            DiagnosticData::RelayPeerRequest { timestamp, .. } => *timestamp,
            DiagnosticData::PeerResponse { timestamp, .. } => *timestamp,
        }
    }
}

#[allow(clippy::enum_variant_names)]
pub(crate) enum DiagnosticRequest {
    GetSeedPeerInfo(tokio::sync::oneshot::Sender<HashMap<String, DiagnosticPeerInfo>>),
    GetPeerInfo(tokio::sync::oneshot::Sender<HashMap<String, DiagnosticPeerInfo>>),
    GetRelayPeerInfo(tokio::sync::oneshot::Sender<HashMap<String, DiagnosticPeerInfo>>),
}

#[derive(Debug, Clone)]
pub(crate) struct DiagnosticsReceiverClient {
    request_tx: tokio::sync::mpsc::Sender<DiagnosticRequest>,
}

impl DiagnosticsReceiverClient {
    pub async fn get_seed_peer_diagnostic_info(&self) -> Result<HashMap<String, DiagnosticPeerInfo>, anyhow::Error> {
        let (tx, rx) = oneshot::channel();
        self.request_tx.send(DiagnosticRequest::GetSeedPeerInfo(tx)).await?;
        Ok(rx.await?)
    }

    pub async fn get_peer_diagnostic_info(&self) -> Result<HashMap<String, DiagnosticPeerInfo>, anyhow::Error> {
        let (tx, rx) = oneshot::channel();
        self.request_tx.send(DiagnosticRequest::GetPeerInfo(tx)).await?;
        Ok(rx.await?)
    }

    pub async fn get_relay_peer_diagnostic_info(&self) -> Result<HashMap<String, DiagnosticPeerInfo>, anyhow::Error> {
        let (tx, rx) = oneshot::channel();
        self.request_tx.send(DiagnosticRequest::GetRelayPeerInfo(tx)).await?;
        Ok(rx.await?)
    }
}

#[derive(Debug, Clone)]
pub struct DiagnosticsBroadcastClient {
    tx: tokio::sync::broadcast::Sender<DiagnosticData>,
}

impl DiagnosticsBroadcastClient {
    pub fn new(tx: tokio::sync::broadcast::Sender<DiagnosticData>) -> Self {
        Self { tx }
    }

    pub fn broadcast(&self, data: DiagnosticData) -> Result<(), anyhow::Error> {
        let _unused = self
            .tx
            .send(data)
            .inspect_err(|_e| error!(target: LOG_TARGET, "Diagnostics broadcasting data"));
        Ok(())
    }

    pub fn send_new_seed_peer(
        &self,
        peer_id: PeerId,
        dial_time: Duration,
        connected: bool,
    ) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::NewSeedPeer {
            peer_id,
            dial_time,
            connected,
            timestamp: EpochTime::now(),
        })
    }

    pub fn send_new_seed_peer_request(&self, peer_id: PeerId) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::SeedPeerRequest {
            peer_id,
            timestamp: EpochTime::now(),
        })
    }

    pub fn send_new_peer(&self, peer_id: PeerId, dial_time: Duration, connected: bool) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::NewPeer {
            peer_id,
            timestamp: EpochTime::now(),
            dial_time,
            connected,
        })
    }

    pub fn send_new_peer_request(&self, peer_id: PeerId) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::PeerRequest {
            peer_id,
            timestamp: EpochTime::now(),
        })
    }

    pub fn send_new_relay_peer(
        &self,
        peer_id: PeerId,
        dial_time: Duration,
        connected: bool,
    ) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::NewRelayPeer {
            peer_id,
            timestamp: EpochTime::now(),
            dial_time,
            connected,
        })
    }

    pub fn send_new_relay_peer_request(&self, peer_id: PeerId) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::RelayPeerRequest {
            peer_id,
            timestamp: EpochTime::now(),
        })
    }

    pub fn send_peer_response(&self, peer_id: PeerId, number_of_peers: u64) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::PeerResponse {
            peer_id,
            number_of_peers,
            timestamp: EpochTime::now(),
        })
    }
}
