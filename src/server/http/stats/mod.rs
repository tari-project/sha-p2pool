// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use tari_core::proof_of_work::PowAlgorithm;

/// Returns a stat key with the provided PoW algorithm.
pub fn algo_stat_key(algo: PowAlgorithm, stat_key: &str) -> String {
    format!("{}_{}", algo.to_string().to_lowercase(), stat_key)
}

pub mod handlers;
pub mod models;

pub(crate) const MAX_ACCEPTABLE_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
