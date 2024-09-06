use crate::server::http::stats::models::Stats;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::Instant;

#[derive(Clone)]
pub struct CachedStats {
    stats: Stats,
    last_update: Instant,
}

impl CachedStats {
    pub fn new(stats: Stats, last_update: Instant) -> Self {
        Self {
            stats,
            last_update,
        }
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn last_update(&self) -> Instant {
        self.last_update
    }
}


pub struct StatsCache {
    ttl: Duration,
    stats: Arc<RwLock<Option<CachedStats>>>,
}

impl StatsCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            stats: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn update(&self, stats: Stats) {
        let mut stats_lock = self.stats.write().await;
        match &mut *stats_lock {
            Some(curr_stats) => {
                curr_stats.stats = stats;
                curr_stats.last_update = Instant::now();
            }
            None => {
                *stats_lock = Some(CachedStats::new(stats, Instant::now()));
            }
        }
    }

    pub async fn stats(&self) -> Option<Stats> {
        let lock = self.stats.read().await;
        if lock.is_some() && Instant::now().duration_since(lock.as_ref()?.last_update) > self.ttl {
            let mut lock = self.stats.write().await;
            *lock = None;
            return None;
        }
        (*lock).as_ref().map(|cached_stats| cached_stats.stats.clone())
    }
}

