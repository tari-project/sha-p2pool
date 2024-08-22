// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

pub const MINER_STAT_ACCEPTED_BLOCKS_COUNT: &str = "miner_accepted_blocks_count";
pub const MINER_STAT_REJECTED_BLOCKS_COUNT: &str = "miner_rejected_blocks_count";
pub const P2POOL_STAT_ACCEPTED_BLOCKS_COUNT: &str = "p2pool_accepted_blocks_count";
pub const P2POOL_STAT_REJECTED_BLOCKS_COUNT: &str = "p2pool_rejected_blocks_count";

pub mod handlers;
pub mod models;
