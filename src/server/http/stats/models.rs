// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use serde::{Deserialize, Serialize};
use tari_utilities::epoch_time::EpochTime;
use tari_utilities::hex::Hex;

use crate::sharechain::block::Block;

#[derive(Serialize, Deserialize)]
pub struct StatsBlock {
    pub hash: String,
    pub timestamp: EpochTime,
    pub miner_wallet_address: Option<String>,
}

impl From<Block> for StatsBlock {
    fn from(block: Block) -> Self {
        StatsBlock {
            hash: block.hash().to_hex(),
            timestamp: block.timestamp(),
            miner_wallet_address: block.miner_wallet_address().clone().map(|addr| addr.to_base58()),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Stats {
    pub num_of_miners: usize,
    pub last_block_won: Option<StatsBlock>,
    pub pool_hash_rate: u128,
}
