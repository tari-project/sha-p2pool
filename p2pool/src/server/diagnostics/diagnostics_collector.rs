// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{collections::HashMap, fmt::Debug, time::Duration};

use libp2p::PeerId;
use log::*;
use serde::Serialize;
use tari_shutdown::Shutdown;
use tari_utilities::epoch_time::EpochTime;
use tokio::{
    sync::{broadcast::Receiver, oneshot},
    time::MissedTickBehavior,
};

#[derive(Serialize, Clone, Debug)]
pub struct DiagnosticPeerInfo {
    pub peer_id: PeerId,
    #[serde(with = "duration_as_seconds")]
    pub dial_time: Duration,
    pub dial_succeeded: bool,
    #[serde(with = "epoch_time_as_rfc3339")]
    pub connected_at: Option<EpochTime>,
    #[serde(with = "epoch_time_as_rfc3339")]
    pub requested_peers_at: Option<EpochTime>,
    pub number_of_peers: Option<usize>,
    #[serde(with = "duration_option_as_seconds")]
    pub response_time: Option<Duration>,
}

mod epoch_time_as_rfc3339 {
    use chrono::{Local, TimeZone};
    use serde::Serializer;

    use super::*;

    pub fn serialize<S>(time: &Option<EpochTime>, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        match time {
            Some(t) => {
                let dt = Local
                    .timestamp_opt(i64::try_from(t.as_u64()).unwrap_or_default(), 0)
                    .single()
                    .unwrap_or_default();
                serializer.serialize_some(&dt.to_rfc3339())
            },
            None => serializer.serialize_none(),
        }
    }
}

mod duration_as_seconds {
    use std::time::Duration;

    use serde::Serializer;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        serializer.serialize_some(&(duration.as_secs() as f64 + f64::from(duration.subsec_millis()) * 1e-3))
    }
}

mod duration_option_as_seconds {
    use std::time::Duration;

    use serde::Serializer;

    pub fn serialize<S>(duration: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        match duration {
            Some(d) => serializer.serialize_some(&(d.as_secs() as f64 + f64::from(d.subsec_millis()) * 1e-3)),
            None => serializer.serialize_none(),
        }
    }
}

const LOG_TARGET: &str = "tari::p2pool::diagnostics_collector";
pub struct DiagnosticsCollector {
    shutdown: Shutdown,
    broadcast_receiver: tokio::sync::broadcast::Receiver<DiagnosticData>,
    request_tx: tokio::sync::mpsc::Sender<DiagnosticRequest>,
    request_rx: tokio::sync::mpsc::Receiver<DiagnosticRequest>,
    first_data_received: Option<EpochTime>,
    seed_peer_info: HashMap<String, DiagnosticPeerInfo>,
    private_peer_info: HashMap<String, DiagnosticPeerInfo>,
    relay_peer_info: HashMap<String, DiagnosticPeerInfo>,
}

impl DiagnosticsCollector {
    pub(crate) fn new(shutdown: Shutdown, broadcast_receiver: Receiver<DiagnosticData>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        Self {
            shutdown,
            broadcast_receiver,
            request_rx: rx,
            request_tx: tx,
            first_data_received: None,
            seed_peer_info: HashMap::new(),
            private_peer_info: HashMap::new(),
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
        debug!(target: LOG_TARGET, "[Diagnostics] handle diagnostics {data:?}");
        match data {
            DiagnosticData::NewSeedPeer {
                peer_id,
                dial_time,
                dial_succeeded,
                ..
            } => {
                self.seed_peer_info.insert(peer_id.to_base58(), DiagnosticPeerInfo {
                    peer_id,
                    dial_time,
                    dial_succeeded,
                    connected_at: None,
                    requested_peers_at: None,
                    number_of_peers: None,
                    response_time: None,
                });
            },
            DiagnosticData::NewPrivatePeer {
                peer_id,
                dial_time,
                dial_succeeded,
                ..
            } => {
                self.private_peer_info.insert(peer_id.to_base58(), DiagnosticPeerInfo {
                    peer_id,
                    dial_time,
                    dial_succeeded,
                    connected_at: None,
                    requested_peers_at: None,
                    number_of_peers: None,
                    response_time: None,
                });
            },
            DiagnosticData::NewRelayPeer {
                peer_id,
                dial_time,
                dial_succeeded,
                ..
            } => {
                self.relay_peer_info.insert(peer_id.to_base58(), DiagnosticPeerInfo {
                    peer_id,
                    dial_time,
                    dial_succeeded,
                    connected_at: None,
                    requested_peers_at: None,
                    number_of_peers: None,
                    response_time: None,
                });
            },
            DiagnosticData::PeerRequest { peer_id, .. } => {
                let existing_peer = self
                    .private_peer_info
                    .get_mut(&peer_id.to_base58())
                    .or_else(|| self.seed_peer_info.get_mut(&peer_id.to_base58()))
                    .or_else(|| self.relay_peer_info.get_mut(&peer_id.to_base58()));
                if let Some(peer) = existing_peer {
                    if peer.requested_peers_at.is_none() {
                        peer.requested_peers_at = Some(EpochTime::now());
                    }
                }
            },
            DiagnosticData::PeerConnected { peer_id, .. } => {
                let existing_peer = self
                    .private_peer_info
                    .get_mut(&peer_id.to_base58())
                    .or_else(|| self.seed_peer_info.get_mut(&peer_id.to_base58()))
                    .or_else(|| self.relay_peer_info.get_mut(&peer_id.to_base58()));
                if let Some(peer) = existing_peer {
                    if peer.connected_at.is_none() {
                        peer.connected_at = Some(EpochTime::now());
                    }
                }
            },
            DiagnosticData::PeerResponse {
                peer_id,
                number_of_peers,
                ..
            } => {
                let existing_peer = self
                    .private_peer_info
                    .get_mut(&peer_id.to_base58())
                    .or_else(|| self.seed_peer_info.get_mut(&peer_id.to_base58()))
                    .or_else(|| self.relay_peer_info.get_mut(&peer_id.to_base58()));
                if let Some(peer) = existing_peer {
                    if let Some(number) = peer.number_of_peers {
                        if number_of_peers > number {
                            peer.number_of_peers = Some(number_of_peers);
                            peer.response_time = peer.requested_peers_at.and_then(|requested_peers_at| {
                                EpochTime::now()
                                    .checked_sub(requested_peers_at)
                                    .map(|epoch_time| Duration::from_secs(epoch_time.as_u64()))
                            });
                        }
                    } else {
                        peer.number_of_peers = Some(number_of_peers);
                        peer.response_time = peer.requested_peers_at.and_then(|requested_peers_at| {
                            EpochTime::now()
                                .checked_sub(requested_peers_at)
                                .map(|epoch_time| Duration::from_secs(epoch_time.as_u64()))
                        });
                    }
                }
            },
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn run(&mut self) -> Result<(), anyhow::Error> {
        let mut diagnostics_report_timer = tokio::time::interval(tokio::time::Duration::from_secs(10));
        diagnostics_report_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut shutdown_signal = self.shutdown.to_signal();
        loop {
            tokio::select! {
                _ = shutdown_signal.wait() => {
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
                        self.private_peer_info.len(),
                        self.relay_peer_info.len(),
                    );
                },
                res = self.request_rx.recv() => {
                    match res {
                        Some(DiagnosticRequest::GetSeedPeerInfo(tx)) => {
                            let _unused  = tx.send(self.seed_peer_info.clone())
                                .inspect_err(|e|
                                    error!(target: LOG_TARGET, "Error seed peers diagnostics response: {:?}", e)
                                );
                        },
                        Some(DiagnosticRequest::GetPrivatePeerInfo(tx)) => {
                            let _unused  = tx.send(self.private_peer_info.clone())
                                .inspect_err(|e|
                                    error!(target: LOG_TARGET, "Error peers diagnostics response: {:?}", e)
                                );
                        },
                        Some(DiagnosticRequest::GetRelayPeerInfo(tx)) => {
                            let _unused  = tx.send(self.relay_peer_info.clone())
                                .inspect_err(|e|
                                    error!(target: LOG_TARGET, "Error relay peers diagnostics response: {:?}", e)
                                );
                        },
                        Some(DiagnosticRequest::GetConnectedPeers(tx)) => {
                            let connected_peers: Vec<_> = self.seed_peer_info.values()
                                .chain(self.private_peer_info.values())
                                .chain(self.relay_peer_info.values())
                                .filter_map(|d| d.connected_at.map(|_| d.peer_id))
                                .collect();
                            let _unused = tx.send(connected_peers)
                                .inspect_err(|e|
                                    error!(target: LOG_TARGET, "Error connected peers diagnostics response: {:?}", e)
                                );
                        }
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

#[derive(Clone, Debug)]
pub(crate) enum DiagnosticData {
    NewSeedPeer {
        peer_id: PeerId,
        dial_time: Duration,
        dial_succeeded: bool,
        timestamp: EpochTime,
    },
    NewPrivatePeer {
        peer_id: PeerId,
        dial_time: Duration,
        dial_succeeded: bool,
        timestamp: EpochTime,
    },
    NewRelayPeer {
        peer_id: PeerId,
        dial_time: Duration,
        dial_succeeded: bool,
        timestamp: EpochTime,
    },
    PeerRequest {
        peer_id: PeerId,
        timestamp: EpochTime,
    },
    PeerResponse {
        peer_id: PeerId,
        number_of_peers: usize,
        timestamp: EpochTime,
    },
    PeerConnected {
        peer_id: PeerId,
        timestamp: EpochTime,
    },
}

impl DiagnosticData {
    pub fn timestamp(&self) -> EpochTime {
        match self {
            DiagnosticData::NewSeedPeer { timestamp, .. } => *timestamp,
            DiagnosticData::NewPrivatePeer { timestamp, .. } => *timestamp,
            DiagnosticData::NewRelayPeer { timestamp, .. } => *timestamp,
            DiagnosticData::PeerRequest { timestamp, .. } => *timestamp,
            DiagnosticData::PeerResponse { timestamp, .. } => *timestamp,
            DiagnosticData::PeerConnected { timestamp, .. } => *timestamp,
        }
    }
}

#[allow(clippy::enum_variant_names)]
pub(crate) enum DiagnosticRequest {
    GetSeedPeerInfo(tokio::sync::oneshot::Sender<HashMap<String, DiagnosticPeerInfo>>),
    GetPrivatePeerInfo(tokio::sync::oneshot::Sender<HashMap<String, DiagnosticPeerInfo>>),
    GetRelayPeerInfo(tokio::sync::oneshot::Sender<HashMap<String, DiagnosticPeerInfo>>),
    GetConnectedPeers(tokio::sync::oneshot::Sender<Vec<PeerId>>),
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

    pub async fn get_private_peer_diagnostic_info(&self) -> Result<HashMap<String, DiagnosticPeerInfo>, anyhow::Error> {
        let (tx, rx) = oneshot::channel();
        self.request_tx.send(DiagnosticRequest::GetPrivatePeerInfo(tx)).await?;
        Ok(rx.await?)
    }

    pub async fn get_relay_peer_diagnostic_info(&self) -> Result<HashMap<String, DiagnosticPeerInfo>, anyhow::Error> {
        let (tx, rx) = oneshot::channel();
        self.request_tx.send(DiagnosticRequest::GetRelayPeerInfo(tx)).await?;
        Ok(rx.await?)
    }

    pub async fn get_connected_peers(&self) -> Result<Vec<PeerId>, anyhow::Error> {
        let (tx, rx) = oneshot::channel();
        self.request_tx.send(DiagnosticRequest::GetConnectedPeers(tx)).await?;
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
        dial_succeeded: bool,
    ) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::NewSeedPeer {
            peer_id,
            dial_time,
            dial_succeeded,
            timestamp: EpochTime::now(),
        })
    }

    pub fn send_new_private_peer(
        &self,
        peer_id: PeerId,
        dial_time: Duration,
        dial_succeeded: bool,
    ) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::NewPrivatePeer {
            peer_id,
            timestamp: EpochTime::now(),
            dial_time,
            dial_succeeded,
        })
    }

    pub fn send_new_relay_peer(
        &self,
        peer_id: PeerId,
        dial_time: Duration,
        dial_succeeded: bool,
    ) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::NewRelayPeer {
            peer_id,
            timestamp: EpochTime::now(),
            dial_time,
            dial_succeeded,
        })
    }

    pub fn send_new_peer_request(&self, peer_id: PeerId) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::PeerRequest {
            peer_id,
            timestamp: EpochTime::now(),
        })
    }

    pub fn send_peer_response(&self, peer_id: PeerId, number_of_peers: usize) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::PeerResponse {
            peer_id,
            number_of_peers,
            timestamp: EpochTime::now(),
        })
    }

    pub fn send_peer_connected(&self, peer_id: PeerId) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::PeerConnected {
            peer_id,
            timestamp: EpochTime::now(),
        })
    }
}
