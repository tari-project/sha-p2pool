// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use num::BigUint;
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
    pub connected: bool,
    pub connected_since: Option<EpochTime>,
    pub num_of_miners: usize,
    pub last_block_won: Option<StatsBlock>,
    pub share_chain_height: u64,
    pub pool_hash_rate: BigUint,
}
