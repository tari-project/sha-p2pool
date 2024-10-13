use log::{error, info};
use num_format::{Locale, ToFormattedString};
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
    max_difficulty: Difficulty,
    network_difficulty: Difficulty,
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
            max_difficulty: Difficulty::min(),
            network_difficulty: Difficulty::min(),
        }
    }

    pub fn create_client(&self) -> StatsClient {
        StatsClient {
            request_tx: self.request_tx.clone(),
        }
    }

    fn handle_stat(&mut self, sample: StatData) {
        match sample {
            StatData::ChainStats { chain, .. } => {},
            StatData::MinerBlockAccepted { pow_algo, .. } => {
                self.miner_accepted += 1;
            },
            StatData::MinerBlockRejected { pow_algo, .. } => {
                self.miner_rejected += 1;
            },
            StatData::PoolBlockAccepted { pow_algo, .. } => {
                self.pool_accepted += 1;
            },
            StatData::PoolBlockRejected { pow_algo, .. } => {
                self.pool_rejected += 1;
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
                    info!(target: LOG_TARGET,
                            "========= Max difficulty: {}. Network difficulty {}. Miner(A/R): {}/{}. Pool(A/R) {}/{}. ==== ",
                            self.max_difficulty.as_u64().to_formatted_string(&Locale::en),
                            self.network_difficulty.as_u64().to_formatted_string(&Locale::en),
                            self.miner_accepted,
                            self.miner_rejected,
                            self.pool_accepted,
                            self.pool_rejected
                        );
                },
                res = self.request_rx.recv() => {
                    match res {
                        Some(StatsRequest::GetStats(pow, tx)) => {
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
}

impl StatData {
    pub fn timestamp(&self) -> EpochTime {
        match self {
            StatData::ChainStats { timestamp, .. } => *timestamp,
            StatData::MinerBlockAccepted { timestamp, .. } => *timestamp,
            StatData::MinerBlockRejected { timestamp, .. } => *timestamp,
            StatData::PoolBlockAccepted { timestamp, .. } => *timestamp,
            StatData::PoolBlockRejected { timestamp, .. } => *timestamp,
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
}
