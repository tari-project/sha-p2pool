// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::collections::HashMap;

use num::BigUint;
use serde::{Deserialize, Serialize};
use tari_core::transactions::tari_amount::MicroMinotari;
use tari_utilities::epoch_time::EpochTime;
use tari_utilities::hex::Hex;

use crate::sharechain::block::Block;

#[derive(Serialize, Deserialize)]
pub struct StatsBlock {
    pub hash: String,
    pub height: u64,
    pub timestamp: EpochTime,
    pub miner_wallet_address: Option<String>,
}

impl From<Block> for StatsBlock {
    fn from(block: Block) -> Self {
        StatsBlock {
            hash: block.hash().to_hex(),
            height: block.height(),
            timestamp: block.timestamp(),
            miner_wallet_address: block.miner_wallet_address().clone().map(|addr| addr.to_base58()),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct EstimatedEarnings {
    #[serde(rename = "1min")]
    pub one_minute: MicroMinotari,
    #[serde(rename = "1h")]
    pub one_hour: MicroMinotari,
    #[serde(rename = "1d")]
    pub one_day: MicroMinotari,
    #[serde(rename = "1w")]
    pub one_week: MicroMinotari,
    #[serde(rename = "30d")]
    pub one_month: MicroMinotari,
}

impl EstimatedEarnings {
    pub fn new(one_minute_earning: MicroMinotari) -> Self {
        Self {
            one_minute: one_minute_earning,
            one_hour: MicroMinotari::from(one_minute_earning.as_u64() * 60),
            one_day: MicroMinotari::from(one_minute_earning.as_u64() * 60 * 24),
            one_week: MicroMinotari::from(one_minute_earning.as_u64() * 60 * 24 * 7),
            one_month: MicroMinotari::from(one_minute_earning.as_u64() * 60 * 24 * 30),
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
    pub pool_total_earnings: MicroMinotari,
    pub pool_total_estimated_earnings: EstimatedEarnings,
    pub total_earnings: HashMap<String, u64>,
    pub estimated_earnings: HashMap<String, EstimatedEarnings>,
}
