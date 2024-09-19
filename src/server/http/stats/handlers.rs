// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{collections::HashMap, sync::Arc};

use axum::{extract::State, http::StatusCode, Json};
use itertools::Itertools;
use log::error;
use serde::Serialize;
use tari_common::configuration::Network;
use tari_core::{consensus::ConsensusManager, proof_of_work::PowAlgorithm, transactions::tari_amount::MicroMinotari};
use tari_utilities::{epoch_time::EpochTime, hex::Hex};

use crate::{
    server::{
        http::{
            server::AppState,
            stats::{
                algo_stat_key,
                models::{BlockStats, EstimatedEarnings, Stats, TribeDetails},
                MINER_STAT_ACCEPTED_BLOCKS_COUNT,
                MINER_STAT_REJECTED_BLOCKS_COUNT,
                P2POOL_STAT_ACCEPTED_BLOCKS_COUNT,
                P2POOL_STAT_REJECTED_BLOCKS_COUNT,
            },
        },
        stats_store::StatsStore,
    },
    sharechain::SHARE_COUNT,
};

const LOG_TARGET: &str = "p2pool::server::stats::get";

#[derive(Serialize)]
pub(crate) struct BlockResult {
    chain_id: String,
    hash: String,
    timestamp: EpochTime,
    prev_hash: String,
    height: u64,
    // original_block_header: BlockHeader,
    miner_wallet_address: Option<String>,
    sent_to_main_chain: bool,
    achieved_difficulty: u64,
    candidate_block_height: u64,
    candidate_block_prev_hash: String,
    algo: String,
}

pub(crate) async fn handle_chain(State(state): State<AppState>) -> Result<Json<Vec<BlockResult>>, StatusCode> {
    let chain = state.share_chain_sha3x.blocks(0).await.map_err(|error| {
        error!(target: LOG_TARGET, "Failed to get blocks of share chain: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let result = chain
        .iter()
        .map(|block| BlockResult {
            chain_id: block.chain_id.clone(),
            hash: block.hash.to_hex(),
            timestamp: block.timestamp,
            prev_hash: block.prev_hash.to_hex(),
            height: block.height,
            // original_block_header: block.original_block_header().clone(),
            miner_wallet_address: block.miner_wallet_address.as_ref().map(|a| a.to_base58()),
            sent_to_main_chain: block.sent_to_main_chain,
            achieved_difficulty: block.achieved_difficulty.as_u64(),
            candidate_block_prev_hash: block.original_block_header.prev_hash.to_hex(),
            candidate_block_height: block.original_block_header.height,
            algo: block.original_block_header.pow.pow_algo.to_string(),
        })
        .collect();

    Ok(Json(result))
}

pub(crate) async fn handle_miners_with_shares(
    State(state): State<AppState>,
) -> Result<Json<HashMap<String, HashMap<String, u64>>>, StatusCode> {
    let mut result = HashMap::with_capacity(2);
    result.insert(
        PowAlgorithm::Sha3x.to_string().to_lowercase(),
        state.share_chain_sha3x.miners_with_shares().await.map_err(|error| {
            error!(target: LOG_TARGET, "Failed to get Sha3x miners with shares: {error:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?,
    );
    result.insert(
        PowAlgorithm::RandomX.to_string().to_lowercase(),
        state.share_chain_random_x.miners_with_shares().await.map_err(|error| {
            error!(target: LOG_TARGET, "Failed to get RandomX miners with shares: {error:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?,
    );

    Ok(Json(result))
}

pub(crate) async fn handle_get_stats(
    State(state): State<AppState>,
) -> Result<Json<HashMap<String, Stats>>, StatusCode> {
    let mut result = HashMap::with_capacity(2);
    result.insert(
        PowAlgorithm::Sha3x.to_string().to_lowercase(),
        get_stats(state.clone(), PowAlgorithm::Sha3x).await?,
    );
    result.insert(
        PowAlgorithm::RandomX.to_string().to_lowercase(),
        get_stats(state.clone(), PowAlgorithm::RandomX).await?,
    );
    Ok(Json(result))
}

#[allow(clippy::too_many_lines)]
async fn get_stats(state: AppState, algo: PowAlgorithm) -> Result<Stats, StatusCode> {
    // return from cache if possible
    let stats_cache = state.stats_cache.clone();
    if let Some(stats) = stats_cache.stats(algo).await {
        return Ok(stats);
    }

    let share_chain = match algo {
        PowAlgorithm::RandomX => state.share_chain_random_x.clone(),
        PowAlgorithm::Sha3x => state.share_chain_sha3x.clone(),
    };
    let chain = share_chain.blocks(0).await.map_err(|error| {
        error!(target: LOG_TARGET, "Failed to get blocks of share chain: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // connected
    let connected = state.peer_store.peer_count().await > 0;

    // collect number of miners
    let num_of_miners = chain
        .iter()
        .map(|block| block.miner_wallet_address.clone())
        .filter(|addr_opt| addr_opt.is_some())
        .map(|addr| addr.as_ref().unwrap().to_base58())
        .unique()
        .count();

    // last won block
    let last_block_won = chain
        .iter()
        .filter(|block| block.sent_to_main_chain)
        .last()
        .cloned()
        .map(|block| block.into());

    let share_chain_height = share_chain.tip_height().await.map_err(|error| {
        error!(target: LOG_TARGET, "Failed to get tip height of share chain: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // hash rate
    let pool_hash_rate = share_chain.hash_rate().await.map_err(|error| {
        error!(target: LOG_TARGET, "Failed to get hash rate of share chain: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // connected since
    let connected_since = state.peer_store.last_connected();

    // consensus manager
    let network = Network::get_current_or_user_setting_or_default();
    let consensus_manager = ConsensusManager::builder(network).build().map_err(|error| {
        error!(target: LOG_TARGET, "Failed to build consensus manager: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // calculate estimated earnings for all wallet addresses
    let blocks = share_chain.blocks(0).await.map_err(|error| {
        error!(target: LOG_TARGET, "Failed to get blocks of share chain: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let pool_total_rewards: u64 = blocks
        .iter()
        .filter(|block| block.sent_to_main_chain)
        .map(|block| {
            consensus_manager
                .get_block_reward_at(block.original_block_header.height)
                .as_u64()
        })
        .sum();

    // calculate all possibly earned rewards for all the miners until latest point
    let mut miners_with_shares = HashMap::<String, u64>::new();
    let mut miners_with_rewards = HashMap::<String, u64>::new();
    blocks.iter().for_each(|block| {
        if let Some(miner_wallet_address) = &block.miner_wallet_address {
            let miner = miner_wallet_address.to_base58();
            let reward = consensus_manager.get_block_reward_at(block.original_block_header.height);

            // collect share count for miners
            if let Some(shares) = miners_with_shares.get(&miner) {
                miners_with_shares.insert(miner, shares + 1);
            } else {
                miners_with_shares.insert(miner, 1);
            }

            // calculate rewards for miners
            if block.sent_to_main_chain {
                miners_with_shares.iter().for_each(|(addr, share_count)| {
                    let miner_reward = (reward.as_u64() / SHARE_COUNT) * share_count;
                    if let Some(earned_rewards) = miners_with_rewards.get(addr) {
                        miners_with_rewards.insert(addr.clone(), earned_rewards + miner_reward);
                    } else {
                        miners_with_rewards.insert(addr.clone(), miner_reward);
                    }
                });
            }
        }
    });

    let mut estimated_earnings = HashMap::new();
    let mut pool_total_estimated_earnings_1m = 0u64;
    if !blocks.is_empty() {
        // calculate "earning / minute" for all miners
        let first_block_time = blocks.first().unwrap().timestamp;
        let full_interval = EpochTime::now().as_u64() - first_block_time.as_u64();
        miners_with_rewards.iter().for_each(|(addr, total_earn)| {
            pool_total_estimated_earnings_1m += total_earn;
            let reward_per_1m = (total_earn / full_interval) * 60;
            estimated_earnings.insert(addr.clone(), EstimatedEarnings::new(MicroMinotari::from(reward_per_1m)));
        });
        pool_total_estimated_earnings_1m = (pool_total_estimated_earnings_1m / full_interval) * 60;
    }

    let result = Stats {
        connected,
        num_of_miners,
        last_block_won,
        share_chain_height,
        pool_hash_rate: pool_hash_rate.to_string(),
        connected_since,
        pool_total_earnings: MicroMinotari::from(pool_total_rewards),
        pool_total_estimated_earnings: EstimatedEarnings::new(MicroMinotari::from(pool_total_estimated_earnings_1m)),
        total_earnings: miners_with_rewards,
        estimated_earnings,
        miner_block_stats: miner_block_stats(state.stats_store.clone(), algo).await,
        p2pool_block_stats: p2pool_block_stats(state.stats_store.clone(), algo).await,
        tribe: TribeDetails::new(state.tribe.to_string(), state.tribe.formatted()),
    };

    stats_cache.update(result.clone(), algo).await;

    Ok(result)
}

async fn miner_block_stats(stats_store: Arc<StatsStore>, algo: PowAlgorithm) -> BlockStats {
    BlockStats::new(
        stats_store
            .get(&algo_stat_key(algo, MINER_STAT_ACCEPTED_BLOCKS_COUNT))
            .await,
        stats_store
            .get(&algo_stat_key(algo, MINER_STAT_REJECTED_BLOCKS_COUNT))
            .await,
    )
}

async fn p2pool_block_stats(stats_store: Arc<StatsStore>, algo: PowAlgorithm) -> BlockStats {
    BlockStats::new(
        stats_store
            .get(&algo_stat_key(algo, P2POOL_STAT_ACCEPTED_BLOCKS_COUNT))
            .await,
        stats_store
            .get(&algo_stat_key(algo, P2POOL_STAT_REJECTED_BLOCKS_COUNT))
            .await,
    )
}
