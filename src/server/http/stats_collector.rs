// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::time::Duration;

use human_format::Formatter;
use libp2p::PeerId;
use log::{debug, error, info};
use serde::Serialize;
use tari_core::proof_of_work::{Difficulty, PowAlgorithm};
use tari_shutdown::ShutdownSignal;
use tari_utilities::epoch_time::EpochTime;
use tokio::{
    sync::{broadcast::Receiver, oneshot},
    time::MissedTickBehavior,
};

const LOG_TARGET: &str = "tari::p2pool::server::stats_collector";
pub(crate) struct StatsCollector {
    shutdown_signal: ShutdownSignal,
    stats_broadcast_receiver: tokio::sync::broadcast::Receiver<StatData>,
    request_tx: tokio::sync::mpsc::Sender<StatsRequest>,
    request_rx: tokio::sync::mpsc::Receiver<StatsRequest>,
    first_stat_received: Option<EpochTime>,
    last_squad: Option<String>,
    local_peer_id: Option<PeerId>,
    miner_rx_accepted: u64,
    miner_sha_accepted: u64,
    // miner_rejected: u64,
    pool_rx_accepted: u64,
    pool_sha_accepted: u64,
    // pool_rejected: u64,
    sha_network_difficulty: Difficulty,
    sha_target_difficulty: Difficulty,
    randomx_network_difficulty: Difficulty,
    randomx_target_difficulty: Difficulty,
    sha3x_chain_height: u64,
    sha3x_chain_length: u64,
    randomx_chain_height: u64,
    randomx_chain_length: u64,
    total_peers: u64,
    total_grey_list: u64,
    total_black_list: u64,
    pending_incoming: u32,
    pending_outgoing: u32,
    established_incoming: u32,
    established_outgoing: u32,
    last_gossip_message: EpochTime,
}

impl StatsCollector {
    pub(crate) fn new(shutdown_signal: ShutdownSignal, stats_broadcast_receiver: Receiver<StatData>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        Self {
            shutdown_signal,
            stats_broadcast_receiver,
            request_rx: rx,
            request_tx: tx,
            last_squad: None,
            local_peer_id: None,
            first_stat_received: None,
            miner_rx_accepted: 0,
            miner_sha_accepted: 0,
            // miner_rejected: 0,
            pool_rx_accepted: 0,
            pool_sha_accepted: 0,
            // pool_rejected: 0,
            sha3x_chain_height: 0,
            sha3x_chain_length: 0,
            randomx_chain_height: 0,
            randomx_chain_length: 0,
            total_peers: 0,
            total_grey_list: 0,
            total_black_list: 0,
            sha_network_difficulty: Difficulty::min(),
            randomx_network_difficulty: Difficulty::min(),
            sha_target_difficulty: Difficulty::min(),
            randomx_target_difficulty: Difficulty::min(),
            pending_incoming: 0,
            pending_outgoing: 0,
            established_incoming: 0,
            established_outgoing: 0,
            last_gossip_message: EpochTime::now(),
        }
    }

    pub fn create_client(&self) -> StatsClient {
        StatsClient {
            request_tx: self.request_tx.clone(),
        }
    }

    fn handle_stat(&mut self, sample: StatData) {
        match sample {
            StatData::InfoChanged {
                squad, local_peer_id, ..
            } => {
                self.last_squad = Some(squad);
                self.local_peer_id = Some(local_peer_id);
            },
            StatData::MinerBlockAccepted { pow_algo, .. } => match pow_algo {
                PowAlgorithm::Sha3x => {
                    self.miner_sha_accepted += 1;
                },
                PowAlgorithm::RandomX => {
                    self.miner_rx_accepted += 1;
                },
            },
            StatData::PoolBlockAccepted { pow_algo, .. } => match pow_algo {
                PowAlgorithm::Sha3x => {
                    self.pool_sha_accepted += 1;
                },
                PowAlgorithm::RandomX => {
                    self.pool_rx_accepted += 1;
                },
            },
            StatData::ChainChanged {
                algo, height, length, ..
            } => {
                debug!(target: LOG_TARGET, "Chain changed: {} {} {}", algo, height, length);
                match algo {
                    PowAlgorithm::Sha3x => {
                        self.sha3x_chain_height = height;
                        self.sha3x_chain_length = length;
                    },
                    PowAlgorithm::RandomX => {
                        self.randomx_chain_height = height;
                        self.randomx_chain_length = length;
                    },
                };
            },
            StatData::NewPeer {
                total_peers,
                total_grey_list,
                total_black_list,
                ..
            } => {
                self.total_peers = total_peers;
                self.total_grey_list = total_grey_list;
                self.total_black_list = total_black_list;
            },
            StatData::TargetDifficultyChanged {
                target_difficulty,
                pow_algo,
                timestamp: _,
            } => match pow_algo {
                PowAlgorithm::Sha3x => {
                    self.sha_target_difficulty = target_difficulty;
                },
                PowAlgorithm::RandomX => {
                    self.randomx_target_difficulty = target_difficulty;
                },
            },
            StatData::NetworkDifficultyChanged {
                network_difficulty,
                pow_algo,
                timestamp: _,
            } => match pow_algo {
                PowAlgorithm::Sha3x => {
                    self.sha_network_difficulty = network_difficulty;
                },
                PowAlgorithm::RandomX => {
                    self.randomx_network_difficulty = network_difficulty;
                },
            },
            StatData::LibP2PStats {
                pending_incoming,
                pending_outgoing,
                established_incoming,
                established_outgoing,
                timestamp: _,
            } => {
                self.pending_incoming = pending_incoming;
                self.pending_outgoing = pending_outgoing;
                self.established_incoming = established_incoming;
                self.established_outgoing = established_outgoing;
            },
            StatData::GossipsubMessageReceived { timestamp } => {
                self.last_gossip_message = timestamp;
            },
        }
    }

    pub(crate) async fn run(&mut self) -> Result<(), anyhow::Error> {
        let mut stats_report_timer = tokio::time::interval(tokio::time::Duration::from_secs(10));
        stats_report_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                        _ = self.shutdown_signal.wait() => {
                            break;
                        },
                        _ = stats_report_timer.tick() => {
                            let formatter = Formatter::new();

                            info!(target: LOG_TARGET,
                                    "========= Uptime: {}. v{}, Sqd: {}, Chains:  Rx {}..{}, Sha3 {}..{}. Difficulty (Target/Network): Rx: {}/{} Sha3x: {}/{} Miner accepts(rx/sha): {}/{}. Pool accepts (rx/sha) {}/{}. Peers(a/g/b) {}/{}/{} libp2p (i/o) {}/{} Last gossip: {}==== ",
                                    humantime::format_duration(Duration::from_secs(
                                        EpochTime::now().as_u64().checked_sub(
                                            self.first_stat_received.unwrap_or(EpochTime::now()).as_u64())
                                .unwrap_or_default())),
                                env!("CARGO_PKG_VERSION"),
                                self.last_squad.as_deref().unwrap_or("Not set"),
                                    self.randomx_chain_height.saturating_sub(self.randomx_chain_length),
                                    self.randomx_chain_height,
                                    self.sha3x_chain_height.saturating_sub(self.sha3x_chain_length),
                                    self.sha3x_chain_height,
                                    formatter.format(self.randomx_target_difficulty.as_u64() as f64 ),
            formatter.format(                            self.randomx_network_difficulty.as_u64() as f64),
                                    formatter.format(self.sha_target_difficulty.as_u64() as f64),
                                    formatter.format(self.sha_network_difficulty.as_u64() as f64),
                                    self.miner_rx_accepted,
                                    self.miner_sha_accepted,
                                    self.pool_rx_accepted,
                                    self.pool_sha_accepted,
                                    self.total_peers,
                                    self.total_grey_list,
                                    self.total_black_list,
                                    self.established_incoming,
                                    self.established_outgoing,
                                    humantime::format_duration(Duration::from_secs(
                                        EpochTime::now().as_u64().checked_sub(self.last_gossip_message.as_u64()).unwrap_or_default())),

                                );
                        },
                        res = self.request_rx.recv() => {
                            match res {
                                Some(StatsRequest::GetStats(pow, tx)) => {

                                    match pow {
                                        PowAlgorithm::Sha3x => {
                                            let _  = tx.send(GetStatsResponse {
                                                height: self.sha3x_chain_height,
                                                last_block_time: EpochTime::now(),
                                                num_my_shares: 0,
                                                total_shares: 0,
                                            }).inspect_err(|e| error!(target: LOG_TARGET, "ShareChainError sending stats response: {:?}", e));
                                        },
                                        PowAlgorithm::RandomX => {
                                            let _ = tx.send(GetStatsResponse {
                                                height: self.randomx_chain_height,
                                                last_block_time: EpochTime::now(),
                                                num_my_shares: 0,
                                                total_shares: 0,
                                            }).inspect_err(|e| error!(target: LOG_TARGET, "ShareChainError sending stats response: {:?}", e));
                                        },
                                    }
                                },
                                Some(StatsRequest::GetLatestStats(tx)) => {
                                    let res = (self.last_gossip_message, self.local_peer_id, self.last_squad.clone().unwrap_or_default());
                                    let _res = tx.send(res).inspect_err(|e| error!(target: LOG_TARGET, "ShareChainError sending latest stats message: {:?}", e));
                                },
                                None => {
                                    break;
                                }
                            }
                        },
                        res = self.stats_broadcast_receiver.recv() => {
                            match res {
                                Ok(sample) => {
                                    if self.first_stat_received.is_none() {
                                        self.first_stat_received = Some(sample.timestamp());
                                    }
                                    self.handle_stat(sample);
                                    // Expect 2 samples per second per device
                                    // let entry = self.hashrate_samples.entry(sample.device_id).or_insert_with(|| VecDeque::with_capacity(181));
                            // if entry.len() > 180 {
                                // entry.pop_front();
                            // }
                            // entry.push_back(sample);
                                },
                                Err(e) => {
                                    error!(target: LOG_TARGET, "ShareChainError receiving hashrate sample: {:?}", e);
                                    // break;
                                }
                            }
                                            }
                    }
        }
        Ok(())
    }
}

pub(crate) enum StatsRequest {
    GetStats(PowAlgorithm, tokio::sync::oneshot::Sender<GetStatsResponse>),
    GetLatestStats(tokio::sync::oneshot::Sender<(EpochTime, Option<PeerId>, String)>),
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct GetStatsResponse {
    height: u64,
    last_block_time: EpochTime,
    num_my_shares: u64,
    total_shares: u64,
}

#[derive(Clone)]
pub(crate) enum StatData {
    InfoChanged {
        squad: String,
        local_peer_id: PeerId,
        timestamp: EpochTime,
    },
    TargetDifficultyChanged {
        target_difficulty: Difficulty,
        pow_algo: PowAlgorithm,
        timestamp: EpochTime,
    },
    NetworkDifficultyChanged {
        network_difficulty: Difficulty,
        pow_algo: PowAlgorithm,
        timestamp: EpochTime,
    },
    MinerBlockAccepted {
        pow_algo: PowAlgorithm,
        timestamp: EpochTime,
    },
    PoolBlockAccepted {
        pow_algo: PowAlgorithm,
        timestamp: EpochTime,
    },
    ChainChanged {
        algo: PowAlgorithm,
        height: u64,
        length: u64,
        timestamp: EpochTime,
    },
    NewPeer {
        total_peers: u64,
        total_grey_list: u64,
        total_black_list: u64,
        timestamp: EpochTime,
    },
    LibP2PStats {
        pending_incoming: u32,
        pending_outgoing: u32,
        established_incoming: u32,
        established_outgoing: u32,
        timestamp: EpochTime,
    },
    GossipsubMessageReceived {
        timestamp: EpochTime,
    },
}

impl StatData {
    pub fn timestamp(&self) -> EpochTime {
        match self {
            StatData::InfoChanged { timestamp, .. } => *timestamp,
            StatData::MinerBlockAccepted { timestamp, .. } => *timestamp,
            StatData::PoolBlockAccepted { timestamp, .. } => *timestamp,
            StatData::ChainChanged { timestamp, .. } => *timestamp,
            StatData::NewPeer { timestamp, .. } => *timestamp,
            StatData::TargetDifficultyChanged { timestamp, .. } => *timestamp,
            StatData::NetworkDifficultyChanged { timestamp, .. } => *timestamp,
            StatData::LibP2PStats { timestamp, .. } => *timestamp,
            StatData::GossipsubMessageReceived { timestamp } => *timestamp,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StatsClient {
    request_tx: tokio::sync::mpsc::Sender<StatsRequest>,
}

impl StatsClient {
    pub async fn get_chain_stats(&self, pow_algo: PowAlgorithm) -> Result<GetStatsResponse, anyhow::Error> {
        let (tx, rx) = oneshot::channel();
        self.request_tx.send(StatsRequest::GetStats(pow_algo, tx)).await?;
        Ok(rx.await?)
    }

    pub async fn get_stats_info(&self) -> Result<(EpochTime, Option<PeerId>, String), anyhow::Error> {
        let (tx, rx) = oneshot::channel();
        self.request_tx.send(StatsRequest::GetLatestStats(tx)).await?;
        Ok(rx.await?)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StatsBroadcastClient {
    tx: tokio::sync::broadcast::Sender<StatData>,
}

impl StatsBroadcastClient {
    pub fn new(tx: tokio::sync::broadcast::Sender<StatData>) -> Self {
        Self { tx }
    }

    pub fn broadcast(&self, data: StatData) -> Result<(), anyhow::Error> {
        let _unused = self
            .tx
            .send(data)
            .inspect_err(|_e| error!(target: LOG_TARGET, "ShareChainError broadcasting stats"));
        Ok(())
    }

    pub fn send_miner_block_accepted(&self, pow_algo: PowAlgorithm) -> Result<(), anyhow::Error> {
        let data = StatData::MinerBlockAccepted {
            pow_algo,
            timestamp: EpochTime::now(),
        };
        self.broadcast(data)
    }

    pub fn send_miner_block_rejected(&self, pow_algo: PowAlgorithm) -> Result<(), anyhow::Error> {
        let data = StatData::MinerBlockAccepted {
            pow_algo,
            timestamp: EpochTime::now(),
        };
        self.broadcast(data)
    }

    pub fn send_pool_block_accepted(&self, pow_algo: PowAlgorithm) -> Result<(), anyhow::Error> {
        let data = StatData::PoolBlockAccepted {
            pow_algo,
            timestamp: EpochTime::now(),
        };
        self.broadcast(data)
    }

    pub fn send_pool_block_rejected(&self, pow_algo: PowAlgorithm) -> Result<(), anyhow::Error> {
        let data = StatData::PoolBlockAccepted {
            pow_algo,
            timestamp: EpochTime::now(),
        };
        self.broadcast(data)
    }

    pub fn send_info_changed(&self, squad: String, local_peer_id: PeerId) -> Result<(), anyhow::Error> {
        let data = StatData::InfoChanged {
            squad,
            local_peer_id,
            timestamp: EpochTime::now(),
        };
        self.broadcast(data)
    }

    pub fn send_chain_changed(&self, pow_algo: PowAlgorithm, height: u64, length: u64) -> Result<(), anyhow::Error> {
        let data = StatData::ChainChanged {
            algo: pow_algo,
            height,
            length,
            timestamp: EpochTime::now(),
        };
        self.broadcast(data)
    }

    pub fn send_new_peer(
        &self,
        total_peers: u64,
        total_grey_list: u64,
        total_black_list: u64,
    ) -> Result<(), anyhow::Error> {
        self.broadcast(StatData::NewPeer {
            total_peers,
            total_grey_list,
            total_black_list,
            timestamp: EpochTime::now(),
        })
    }

    pub fn send_target_difficulty(
        &self,
        pow_algo: PowAlgorithm,
        target_difficulty: Difficulty,
    ) -> Result<(), anyhow::Error> {
        let data = StatData::TargetDifficultyChanged {
            target_difficulty,
            pow_algo,
            timestamp: EpochTime::now(),
        };
        self.broadcast(data)
    }

    pub fn send_network_difficulty(
        &self,
        pow_algo: PowAlgorithm,
        network_difficulty: Difficulty,
    ) -> Result<(), anyhow::Error> {
        let data = StatData::NetworkDifficultyChanged {
            network_difficulty,
            pow_algo,
            timestamp: EpochTime::now(),
        };
        self.broadcast(data)
    }

    pub fn send_libp2p_stats(
        &self,
        pending_incoming: u32,
        pending_outgoing: u32,
        established_incoming: u32,
        established_outgoing: u32,
    ) -> Result<(), anyhow::Error> {
        self.broadcast(StatData::LibP2PStats {
            pending_incoming,
            pending_outgoing,
            established_incoming,
            established_outgoing,
            timestamp: EpochTime::now(),
        })
    }

    pub fn send_gossipsub_message_received(&self) -> Result<(), anyhow::Error> {
        self.broadcast(StatData::GossipsubMessageReceived {
            timestamp: EpochTime::now(),
        })
    }
}
