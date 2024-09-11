// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use crate::server::http::stats::models::Stats;
use std::sync::Arc;
use std::time::Duration;
use tari_core::proof_of_work::PowAlgorithm;
use tokio::sync::RwLock;
use tokio::time::Instant;

#[derive(Clone)]
pub struct CachedStats {
    stats: Stats,
    last_update: Instant,
}

impl CachedStats {
    pub fn new(stats: Stats, last_update: Instant) -> Self {
        Self { stats, last_update }
    }
}

pub struct StatsCache {
    ttl: Duration,
    stats_sha3x: Arc<RwLock<Option<CachedStats>>>,
    stats_randomx: Arc<RwLock<Option<CachedStats>>>,
}

impl Default for StatsCache {
    fn default() -> Self {
        Self::new(Duration::from_secs(10))
    }
}

impl StatsCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            stats_sha3x: Arc::new(RwLock::new(None)),
            stats_randomx: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn update(&self, stats: Stats, pow_algo: PowAlgorithm) {
        let stats_lock = match pow_algo {
            PowAlgorithm::RandomX => self.stats_randomx.clone(),
            PowAlgorithm::Sha3x => self.stats_sha3x.clone(),
        };
        let mut stats_lock = stats_lock.write().await;
        match &mut *stats_lock {
            Some(curr_stats) => {
                curr_stats.stats = stats;
                curr_stats.last_update = Instant::now();
            },
            None => {
                *stats_lock = Some(CachedStats::new(stats, Instant::now()));
            },
        }
    }

    pub async fn stats(&self, pow_algo: PowAlgorithm) -> Option<Stats> {
        let stats_lock = match pow_algo {
            PowAlgorithm::RandomX => self.stats_randomx.clone(),
            PowAlgorithm::Sha3x => self.stats_sha3x.clone(),
        };
        let lock = stats_lock.read().await;
        let last_update = lock.as_ref()?.last_update;
        if lock.is_some() && Instant::now().duration_since(last_update) > self.ttl {
            drop(lock);
            let mut lock = stats_lock.write().await;
            *lock = None;
            return None;
        }
        (*lock).as_ref().map(|cached_stats| cached_stats.stats.clone())
    }
}
