use std::time::Duration;

use human_format::Formatter;
use log::{error, info};
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
    miner_accepted: u64,
    miner_rejected: u64,
    pool_accepted: u64,
    pool_rejected: u64,
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
}

impl StatsCollector {
    pub(crate) fn new(shutdown_signal: ShutdownSignal, stats_broadcast_receiver: Receiver<StatData>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        Self {
            shutdown_signal,
            stats_broadcast_receiver,
            request_rx: rx,
            request_tx: tx,
            first_stat_received: None,
            miner_accepted: 0,
            miner_rejected: 0,
            pool_accepted: 0,
            pool_rejected: 0,
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
        }
    }

    pub fn create_client(&self) -> StatsClient {
        StatsClient {
            request_tx: self.request_tx.clone(),
        }
    }

    fn handle_stat(&mut self, sample: StatData) {
        match sample {
            StatData::ChainStats { .. } => {},
            StatData::MinerBlockAccepted { .. } => {
                self.miner_accepted += 1;
            },
            StatData::MinerBlockRejected { .. } => {
                self.miner_rejected += 1;
            },
            StatData::PoolBlockAccepted { .. } => {
                self.pool_accepted += 1;
            },
            StatData::PoolBlockRejected { .. } => {
                self.pool_rejected += 1;
            },
            StatData::ChainChanged {
                algo, height, length, ..
            } => match algo {
                PowAlgorithm::Sha3x => {
                    self.sha3x_chain_height = height;
                    self.sha3x_chain_length = length;
                },
                PowAlgorithm::RandomX => {
                    self.randomx_chain_height = height;
                    self.randomx_chain_length = length;
                },
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
                timestamp,
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
                timestamp,
            } => match pow_algo {
                PowAlgorithm::Sha3x => {
                    self.sha_network_difficulty = network_difficulty;
                },
                PowAlgorithm::RandomX => {
                    self.randomx_network_difficulty = network_difficulty;
                },
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
                                    "========= Uptime: {}. Chains:  Rx {}..{}, Sha3 {}..{}. Difficulty (Target/Network): Rx: {}/{} Sha3x: {}/{} Miner(A/R): {}/{}. Pool(A/R) {}/{}. Peers(a/g/b) {}/{}/{} ==== ",
                                    humantime::format_duration(Duration::from_secs(
                                        EpochTime::now().as_u64().checked_sub(
                                            self.first_stat_received.unwrap_or_else(|| EpochTime::now()).as_u64())
                                .unwrap_or_default())),
                                    self.randomx_chain_height.saturating_sub(self.randomx_chain_length.saturating_sub(1)),
                                    self.randomx_chain_height,
                                    self.sha3x_chain_height.saturating_sub(self.sha3x_chain_length.saturating_sub(1)),
                                    self.sha3x_chain_height,
                                    formatter.format(self.randomx_target_difficulty.as_u64() as f64 ),
            formatter.format(                            self.randomx_network_difficulty.as_u64() as f64),
                                    formatter.format(self.sha_target_difficulty.as_u64() as f64),
                                    formatter.format(self.sha_network_difficulty.as_u64() as f64),
                                    self.miner_accepted,
                                    self.miner_rejected,
                                    self.pool_accepted,
                                    self.pool_rejected,
                                    self.total_peers,
                                    self.total_grey_list,
                                    self.total_black_list
                                );
                        },
                        res = self.request_rx.recv() => {
                            match res {
                                Some(StatsRequest::GetStats(_pow, _tx)) => {
                                    todo!();
                                    // let _ = tx.send(hashrate);
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
                                    error!(target: LOG_TARGET, "Error receiving hashrate sample: {:?}", e);
                                    break;
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
}

pub(crate) struct GetStatsResponse {}

#[derive(Clone)]
pub(crate) struct ChainStats {
    pow_algo: PowAlgorithm,
    height: u64,
    length: u64,
}

#[derive(Clone)]
pub(crate) enum StatData {
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
    ChainStats {
        chain: ChainStats,
        timestamp: EpochTime,
    },
    MinerBlockAccepted {
        pow_algo: PowAlgorithm,
        timestamp: EpochTime,
    },
    MinerBlockRejected {
        pow_algo: PowAlgorithm,
        timestamp: EpochTime,
    },

    PoolBlockAccepted {
        pow_algo: PowAlgorithm,
        timestamp: EpochTime,
    },
    PoolBlockRejected {
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
}

impl StatData {
    pub fn timestamp(&self) -> EpochTime {
        match self {
            StatData::ChainStats { timestamp, .. } => *timestamp,
            StatData::MinerBlockAccepted { timestamp, .. } => *timestamp,
            StatData::MinerBlockRejected { timestamp, .. } => *timestamp,
            StatData::PoolBlockAccepted { timestamp, .. } => *timestamp,
            StatData::PoolBlockRejected { timestamp, .. } => *timestamp,
            StatData::ChainChanged { timestamp, .. } => *timestamp,
            StatData::NewPeer { timestamp, .. } => *timestamp,
            StatData::TargetDifficultyChanged { timestamp, .. } => *timestamp,
            StatData::NetworkDifficultyChanged { timestamp, .. } => *timestamp,
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
        let _ = self
            .tx
            .send(data)
            .inspect_err(|_e| error!(target: LOG_TARGET, "Error broadcasting stats"));
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
}
