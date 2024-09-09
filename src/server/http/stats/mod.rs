// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use tari_core::proof_of_work::PowAlgorithm;

pub const MINER_STAT_ACCEPTED_BLOCKS_COUNT: &str = "miner_accepted_blocks_count";
pub const MINER_STAT_REJECTED_BLOCKS_COUNT: &str = "miner_rejected_blocks_count";
pub const P2POOL_STAT_ACCEPTED_BLOCKS_COUNT: &str = "p2pool_accepted_blocks_count";
pub const P2POOL_STAT_REJECTED_BLOCKS_COUNT: &str = "p2pool_rejected_blocks_count";

/// Returns a stat key with the provided PoW algorithm.
pub fn algo_stat_key(algo: PowAlgorithm, stat_key: &str) -> String {
    format!("{}_{}", algo.to_string().to_lowercase(), stat_key)
}

pub mod cache;
pub mod handlers;
pub mod models;
