// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::collections::HashMap;

use tokio::sync::RwLock;

pub struct StatsStore {
    stats: RwLock<HashMap<String, u64>>,
}

impl StatsStore {
    pub fn new() -> Self {
        Self {
            stats: RwLock::new(HashMap::new()),
        }
    }

    /// Returns one stat by [`key`].
    pub async fn get(&self, key: &String) -> u64 {
        let read_lock = self.stats.read().await;
        read_lock.get(key).cloned().unwrap_or(0)
    }

    /// Increments stat with given key.
    /// If the value is not found by key, simply create new value.
    pub async fn inc(&self, key: &String, by: u64) {
        let mut write_lock = self.stats.write().await;
        match write_lock.get(key) {
            Some(stat) => {
                let value = stat + by;
                write_lock.insert(key.clone(), value);
            },
            None => {
                write_lock.insert(key.clone(), by);
            },
        }
    }
}
