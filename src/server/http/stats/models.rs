// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tari_common_types::tari_address::TariAddress;
use tari_core::transactions::tari_amount::MicroMinotari;
use tari_utilities::{epoch_time::EpochTime, hex::Hex};

use crate::{server::p2p::ConnectionInfo, sharechain::p2block::P2Block};

#[derive(Serialize, Deserialize, Clone)]
pub struct StatsBlock {
    pub hash: String,
    pub height: u64,
    pub timestamp: EpochTime,
    pub miner_wallet_address: TariAddress,
}

impl From<Arc<P2Block>> for StatsBlock {
    fn from(block: Arc<P2Block>) -> Self {
        StatsBlock {
            hash: block.hash.to_hex(),
            height: block.height,
            timestamp: block.timestamp,
            miner_wallet_address: block.miner_wallet_address.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
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

#[derive(Serialize, Deserialize, Clone)]
pub struct BlockStats {
    pub accepted: u64,
    pub rejected: u64,
    pub submitted: u64,
}

impl BlockStats {
    pub fn new(accepted: u64, rejected: u64) -> Self {
        Self {
            accepted,
            rejected,
            submitted: accepted + rejected,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SquadDetails {
    pub id: String,
    pub name: String,
}
impl SquadDetails {
    pub fn new(id: String, name: String) -> Self {
        Self { id, name }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Stats {
    pub connected: bool,
    pub peer_count: u64,
    pub connection_info: ConnectionInfo,
    pub connected_since: Option<EpochTime>,
    pub randomx_stats: ChainStats,
    pub sha3x_stats: ChainStats,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ChainStats {
    pub squad: SquadDetails,
    // pub num_of_miners: usize,
    pub share_chain_height: u64,
    pub share_chain_length: u64,
    // pub pool_hash_rate: String,
    // pub pool_total_earnings: MicroMinotari,
    // pub pool_total_estimated_earnings: EstimatedEarnings,
    // pub total_earnings: HashMap<String, u64>,
    // pub estimated_earnings: HashMap<String, EstimatedEarnings>,
    // pub miner_block_stats: BlockStats,
    // pub p2pool_block_stats: BlockStats,
}
