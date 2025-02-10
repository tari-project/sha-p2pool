// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tari_common_types::tari_address::TariAddress;
use tari_utilities::{epoch_time::EpochTime, hex::Hex};

use crate::{
    server::{http::stats_collector::GetStatsResponse, p2p::ConnectionInfo},
    sharechain::p2block::P2Block,
};

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
pub struct SquadDetails {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Clone)]
pub struct Stats {
    pub connection_info: ConnectionInfo,
    pub connected_since: Option<EpochTime>,
    pub randomx_stats: GetStatsResponse,
    pub sha3x_stats: GetStatsResponse,
    pub last_gossip_message: EpochTime,
    pub peer_id: String,
    pub squad: String,
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
