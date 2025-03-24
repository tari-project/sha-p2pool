// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::{hash_map::Entry, HashMap},
    fmt::Debug,
    time::{Duration, SystemTime},
};

use libp2p::PeerId;
use log::*;
use serde::Serialize;
use tari_shutdown::Shutdown;
use tokio::{
    sync::{broadcast::Receiver, oneshot},
    time::MissedTickBehavior,
};

use crate::server::PeerType;

#[derive(Serialize, Clone, Debug)]
pub struct DiagnosticPeerInfo {
    pub peer_id: PeerId,
    #[serde(with = "format_time")]
    pub connected_at: Option<SystemTime>,
    #[serde(with = "format_time")]
    pub requested_peers_at: Option<SystemTime>,
    pub number_of_peers: Option<usize>,
    #[serde(rename = "response_time (s)", with = "duration_option_as_seconds")]
    pub response_time: Option<Duration>,
}

mod format_time {
    use std::time::SystemTime;

    use chrono::{DateTime, Local};
    use serde::Serializer;

    pub fn serialize<S>(time: &Option<SystemTime>, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        match time {
            Some(t) => {
                let datetime: DateTime<Local> = DateTime::from(*t);
                let dt = datetime.format("%Y-%m-%d %H:%M:%S%.3f").to_string();
                serializer.serialize_some(&dt)
            },
            None => serializer.serialize_none(),
        }
    }
}

mod duration_option_as_seconds {
    use std::time::Duration;

    use serde::Serializer;

    pub fn serialize<S>(duration: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        match duration {
            Some(d) => {
                let seconds = d.as_secs() as f64 + f64::from(d.subsec_millis()) * 1e-3;
                let formatted = format!("{:.3}", seconds);
                serializer.serialize_some(&formatted)
            },
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
    first_data_received: Option<SystemTime>,
    seed_peer_info: HashMap<String, DiagnosticPeerInfo>,
    private_peer_info: HashMap<String, DiagnosticPeerInfo>,
    relay_peer_info: HashMap<String, DiagnosticPeerInfo>,
    non_squad_peer_info: HashMap<String, DiagnosticPeerInfo>,
    unknown_peer_info: HashMap<String, DiagnosticPeerInfo>,
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
            non_squad_peer_info: HashMap::new(),
            unknown_peer_info: HashMap::new(),
        }
    }

    pub fn create_receiver_client(&self) -> DiagnosticsReceiverClient {
        DiagnosticsReceiverClient {
            request_tx: self.request_tx.clone(),
        }
    }

    fn hydrate_and_get_existing(&mut self, peer_id: PeerId, peer_type: PeerType) -> Option<&mut DiagnosticPeerInfo> {
        let peer_id_base58 = peer_id.to_base58();
        // The peer type could have changed form Unknown to SeedPeer, PrivatePeer or RelayPeer, etc. Move the peer
        // to the correct peer type
        let hydrate_peer = if self.unknown_peer_info.contains_key(&peer_id_base58) {
            if peer_type == PeerType::Unknown {
                None
            } else {
                self.unknown_peer_info.remove(&peer_id_base58)
            }
        } else if self.private_peer_info.contains_key(&peer_id_base58) {
            if peer_type == PeerType::PrivatePeer {
                None
            } else {
                self.private_peer_info.remove(&peer_id_base58)
            }
        } else if self.seed_peer_info.contains_key(&peer_id_base58) {
            if peer_type == PeerType::SeedPeer {
                None
            } else {
                self.seed_peer_info.remove(&peer_id_base58)
            }
        } else if self.relay_peer_info.contains_key(&peer_id_base58) {
            if peer_type == PeerType::RelayPeer {
                None
            } else {
                self.relay_peer_info.remove(&peer_id_base58)
            }
        } else if self.non_squad_peer_info.contains_key(&peer_id_base58) {
            if peer_type == PeerType::NonSquadPeer {
                None
            } else {
                self.non_squad_peer_info.remove(&peer_id_base58)
            }
        } else {
            None
        };

        // Hydrate
        if let Some(peer) = hydrate_peer {
            match peer_type {
                PeerType::SeedPeer => {
                    self.seed_peer_info.insert(peer_id_base58, peer.clone());
                },
                PeerType::PrivatePeer => {
                    self.private_peer_info.insert(peer_id_base58, peer.clone());
                },
                PeerType::RelayPeer => {
                    self.relay_peer_info.insert(peer_id_base58, peer.clone());
                },
                PeerType::NonSquadPeer => {
                    self.non_squad_peer_info.insert(peer_id_base58, peer.clone());
                },
                PeerType::Unknown => {
                    self.unknown_peer_info.insert(peer_id_base58, peer.clone());
                },
            }
        }

        // Now we can get the peer info
        let peer_id_base58 = peer_id.to_base58();
        let peer_info = self
            .private_peer_info
            .get_mut(&peer_id_base58)
            .or_else(|| self.seed_peer_info.get_mut(&peer_id_base58))
            .or_else(|| self.relay_peer_info.get_mut(&peer_id_base58))
            .or_else(|| self.non_squad_peer_info.get_mut(&peer_id_base58))
            .or_else(|| self.unknown_peer_info.get_mut(&peer_id_base58));
        peer_info
    }

    #[allow(clippy::too_many_lines)]
    fn handle_diagnostic(&mut self, data: DiagnosticData) {
        match data {
            DiagnosticData::NewSeedPeer { peer_id, .. } => {
                if let Entry::Vacant(entry) = self.seed_peer_info.entry(peer_id.to_base58()) {
                    entry.insert(DiagnosticPeerInfo {
                        peer_id,
                        connected_at: None,
                        requested_peers_at: None,
                        number_of_peers: None,
                        response_time: None,
                    });
                    debug!(target: LOG_TARGET, "[Diagnostics] handle diagnostics {data:?}");
                }
            },
            DiagnosticData::NewPrivatePeer { peer_id, .. } => {
                if let Entry::Vacant(entry) = self.private_peer_info.entry(peer_id.to_base58()) {
                    entry.insert(DiagnosticPeerInfo {
                        peer_id,
                        connected_at: None,
                        requested_peers_at: None,
                        number_of_peers: None,
                        response_time: None,
                    });
                    debug!(target: LOG_TARGET, "[Diagnostics] handle diagnostics {data:?}");
                }
            },
            DiagnosticData::NewRelayPeer { peer_id, .. } => {
                if let Entry::Vacant(entry) = self.relay_peer_info.entry(peer_id.to_base58()) {
                    entry.insert(DiagnosticPeerInfo {
                        peer_id,
                        connected_at: None,
                        requested_peers_at: None,
                        number_of_peers: None,
                        response_time: None,
                    });
                    debug!(target: LOG_TARGET, "[Diagnostics] handle diagnostics {data:?}");
                }
            },
            DiagnosticData::NewNonSquadPeer { peer_id, .. } => {
                if let Entry::Vacant(entry) = self.non_squad_peer_info.entry(peer_id.to_base58()) {
                    entry.insert(DiagnosticPeerInfo {
                        peer_id,
                        connected_at: None,
                        requested_peers_at: None,
                        number_of_peers: None,
                        response_time: None,
                    });
                    debug!(target: LOG_TARGET, "[Diagnostics] handle diagnostics {data:?}");
                }
            },
            DiagnosticData::NewUnknownPeer { peer_id, .. } => {
                if let Entry::Vacant(entry) = self.unknown_peer_info.entry(peer_id.to_base58()) {
                    entry.insert(DiagnosticPeerInfo {
                        peer_id,
                        connected_at: None,
                        requested_peers_at: None,
                        number_of_peers: None,
                        response_time: None,
                    });
                    debug!(target: LOG_TARGET, "[Diagnostics] handle diagnostics {data:?}");
                }
            },
            DiagnosticData::PeerConnected {
                peer_id, ref peer_type, ..
            } => {
                let existing_peer = self.hydrate_and_get_existing(peer_id, *peer_type);
                if let Some(peer) = existing_peer {
                    if peer.connected_at.is_none() {
                        peer.connected_at = Some(SystemTime::now());
                    }
                } else {
                    match peer_type {
                        PeerType::SeedPeer => {
                            self.seed_peer_info.insert(peer_id.to_base58(), DiagnosticPeerInfo {
                                peer_id,
                                connected_at: Some(SystemTime::now()),
                                requested_peers_at: None,
                                number_of_peers: None,
                                response_time: None,
                            });
                        },
                        PeerType::PrivatePeer => {
                            self.private_peer_info.insert(peer_id.to_base58(), DiagnosticPeerInfo {
                                peer_id,
                                connected_at: Some(SystemTime::now()),
                                requested_peers_at: None,
                                number_of_peers: None,
                                response_time: None,
                            });
                        },
                        PeerType::RelayPeer => {
                            self.relay_peer_info.insert(peer_id.to_base58(), DiagnosticPeerInfo {
                                peer_id,
                                connected_at: Some(SystemTime::now()),
                                requested_peers_at: None,
                                number_of_peers: None,
                                response_time: None,
                            });
                        },
                        PeerType::NonSquadPeer => {
                            self.non_squad_peer_info
                                .insert(peer_id.to_base58(), DiagnosticPeerInfo {
                                    peer_id,
                                    connected_at: Some(SystemTime::now()),
                                    requested_peers_at: None,
                                    number_of_peers: None,
                                    response_time: None,
                                });
                        },
                        PeerType::Unknown => {
                            self.unknown_peer_info.insert(peer_id.to_base58(), DiagnosticPeerInfo {
                                peer_id,
                                connected_at: Some(SystemTime::now()),
                                requested_peers_at: None,
                                number_of_peers: None,
                                response_time: None,
                            });
                        },
                    }
                }
                debug!(target: LOG_TARGET, "[Diagnostics] handle diagnostics {peer_type} {data:?}");
            },
            DiagnosticData::PeerRequest {
                peer_id, ref peer_type, ..
            } => {
                let existing_peer = self.hydrate_and_get_existing(peer_id, *peer_type);
                if let Some(peer) = existing_peer {
                    if peer.requested_peers_at.is_none() {
                        peer.requested_peers_at = Some(SystemTime::now());
                        debug!(target: LOG_TARGET, "[Diagnostics] handle diagnostics {peer_type} {data:?}");
                    }
                } else {
                    warn!(target: LOG_TARGET, "[Diagnostics] handle diagnostics {peer_type} {peer_id} not found");
                }
            },
            DiagnosticData::PeerResponse {
                peer_id,
                ref peer_type,
                number_of_peers,
                ..
            } => {
                let existing_peer = self.hydrate_and_get_existing(peer_id, *peer_type);
                if let Some(peer) = existing_peer {
                    let new_total = if let Some(number) = peer.number_of_peers {
                        number_of_peers + number
                    } else {
                        number_of_peers
                    };
                    peer.number_of_peers = Some(new_total);
                    if peer.response_time.is_none() {
                        peer.response_time = peer.requested_peers_at.map(|requested_peers_at| {
                            SystemTime::now().duration_since(requested_peers_at).unwrap_or_default()
                        });
                    }
                    debug!(target: LOG_TARGET, "[Diagnostics] handle diagnostics {peer_type} {data:?}");
                } else {
                    warn!(target: LOG_TARGET, "[Diagnostics] handle diagnostics {peer_type} {peer_id} not found");
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
                        "========= Uptime: {:?}. Seeds: {}, Peers: {}, Relay peers: {} ==== ",
                        Duration::from_secs(
                            SystemTime::now()
                                .duration_since(self.first_data_received.unwrap_or(SystemTime::now()))
                                .unwrap_or_default()
                                .as_secs()
                        ),
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
        timestamp: SystemTime,
    },
    NewPrivatePeer {
        peer_id: PeerId,
        timestamp: SystemTime,
    },
    NewRelayPeer {
        peer_id: PeerId,
        timestamp: SystemTime,
    },
    NewNonSquadPeer {
        peer_id: PeerId,
        timestamp: SystemTime,
    },
    NewUnknownPeer {
        peer_id: PeerId,
        timestamp: SystemTime,
    },
    PeerConnected {
        peer_id: PeerId,
        peer_type: PeerType,
        timestamp: SystemTime,
    },
    PeerRequest {
        peer_id: PeerId,
        peer_type: PeerType,
        timestamp: SystemTime,
    },
    PeerResponse {
        peer_id: PeerId,
        peer_type: PeerType,
        number_of_peers: usize,
        timestamp: SystemTime,
    },
}

impl DiagnosticData {
    pub fn timestamp(&self) -> SystemTime {
        match self {
            DiagnosticData::NewSeedPeer { timestamp, .. } => *timestamp,
            DiagnosticData::NewPrivatePeer { timestamp, .. } => *timestamp,
            DiagnosticData::NewRelayPeer { timestamp, .. } => *timestamp,
            DiagnosticData::PeerRequest { timestamp, .. } => *timestamp,
            DiagnosticData::PeerResponse { timestamp, .. } => *timestamp,
            DiagnosticData::PeerConnected { timestamp, .. } => *timestamp,
            DiagnosticData::NewNonSquadPeer { timestamp, .. } => *timestamp,
            DiagnosticData::NewUnknownPeer { timestamp, .. } => *timestamp,
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

    pub fn send_new_peer(&self, peer_id: PeerId, peer_type: PeerType) -> Result<(), anyhow::Error> {
        match peer_type {
            PeerType::SeedPeer => self.broadcast(DiagnosticData::NewSeedPeer {
                peer_id,
                timestamp: SystemTime::now(),
            }),
            PeerType::PrivatePeer => self.broadcast(DiagnosticData::NewPrivatePeer {
                peer_id,
                timestamp: SystemTime::now(),
            }),
            PeerType::RelayPeer => self.broadcast(DiagnosticData::NewRelayPeer {
                peer_id,
                timestamp: SystemTime::now(),
            }),
            PeerType::NonSquadPeer => self.broadcast(DiagnosticData::NewNonSquadPeer {
                peer_id,
                timestamp: SystemTime::now(),
            }),
            PeerType::Unknown => self.broadcast(DiagnosticData::NewUnknownPeer {
                peer_id,
                timestamp: SystemTime::now(),
            }),
        }
    }

    pub fn send_exchange_request(&self, peer_id: PeerId, peer_type: PeerType) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::PeerRequest {
            peer_id,
            peer_type,
            timestamp: SystemTime::now(),
        })
    }

    pub fn send_peer_response(
        &self,
        peer_id: PeerId,
        number_of_peers: usize,
        peer_type: PeerType,
    ) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::PeerResponse {
            peer_id,
            peer_type,
            number_of_peers,
            timestamp: SystemTime::now(),
        })
    }

    pub fn send_peer_connected(&self, peer_id: PeerId, peer_type: PeerType) -> Result<(), anyhow::Error> {
        self.broadcast(DiagnosticData::PeerConnected {
            peer_id,
            peer_type,
            timestamp: SystemTime::now(),
        })
    }
}
