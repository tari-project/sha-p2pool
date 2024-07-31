// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use itertools::Itertools;
use log::{error, info};
use tari_common::configuration::Network;
use tari_core::consensus::ConsensusManager;
use tari_core::transactions::tari_amount::MicroMinotari;

use crate::server::http::stats::models::Stats;
use crate::server::http::stats::server::AppState;
use crate::sharechain::SHARE_COUNT;

const LOG_TARGET: &str = "p2pool::server::stats::get";

pub async fn handle_get_stats(State(state): State<AppState>) -> Result<Json<Stats>, StatusCode> {
    let chain = state.share_chain.blocks(0).await.map_err(|error| {
        error!(target: LOG_TARGET, "Failed to get blocks of share chain: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // connected
    let connected = state.peer_store.peer_count().await > 0;

    // collect number of miners
    let num_of_miners = chain.iter()
        .map(|block| block.miner_wallet_address())
        .filter(|addr_opt| addr_opt.is_some())
        .map(|addr| addr.as_ref().unwrap().to_base58())
        .unique()
        .count();

    // last won block
    let last_block_won = chain.iter()
        .filter(|block| block.sent_to_main_chain())
        .last()
        .cloned()
        .map(|block| block.into());

    let share_chain_height = state.share_chain.tip_height().await.map_err(|error| {
        error!(target: LOG_TARGET, "Failed to get tip height of share chain: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // hash rate
    let pool_hash_rate = state.share_chain.hash_rate().await.map_err(|error| {
        error!(target: LOG_TARGET, "Failed to get hash rate of share chain: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // connected since
    let connected_since = state.peer_store.last_connected();

    // consensus manager
    let network = Network::get_current_or_user_setting_or_default();
    let consensus_manager = ConsensusManager::builder(network).build()
        .map_err(|error| {
            error!(target: LOG_TARGET, "Failed to build consensus manager: {error:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // calculate estimated earnings for all wallet addresses
    let blocks = state.share_chain.blocks(0).await.map_err(|error| {
        error!(target: LOG_TARGET, "Failed to get blocks of share chain: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let pool_total_rewards: u64 = blocks.iter()
        .filter(|block| block.sent_to_main_chain())
        .map(|block| consensus_manager.get_block_reward_at(block.original_block_header().height).as_u64())
        .sum();

    // calculate all possibly earned rewards for all the miners until latest point
    let mut miners_with_shares = HashMap::<String, u64>::new();
    let mut miners_with_rewards = HashMap::<String, u64>::new();
    blocks.iter().for_each(|block| {
        if let Some(miner_wallet_address) = block.miner_wallet_address() {
            let miner = miner_wallet_address.to_hex();
            let reward = consensus_manager.get_block_reward_at(block.original_block_header().height);

            // collect share count for miners
            if let Some(shares) = miners_with_shares.get(&miner) {
                miners_with_shares.insert(miner, shares + 1);
            } else {
                miners_with_shares.insert(miner, 1);
            }

            // calculate rewards for miners
            if block.sent_to_main_chain() {
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

    info!("Miner rewards: {miners_with_rewards:?}");

    let mut estimated_earnings = HashMap::new();
    if !blocks.is_empty() {
        // calculate "earning / minute" for all miners
        let first_block_time = blocks.first().unwrap().timestamp();
        let last_block_time = blocks.last().unwrap().timestamp();
        let full_interval = last_block_time.as_u64() - first_block_time.as_u64();
        miners_with_rewards.iter().for_each(|(addr, total_earn)| {
            let reward_per_24h = (full_interval / total_earn) * 60 * 60 * 24; // 24 hours
            estimated_earnings.insert(addr.clone(), reward_per_24h);
        });
    }

    // TODO: continue with estimated 24h earning of the whole pool


    Ok(Json(Stats {
        connected,
        num_of_miners,
        last_block_won,
        share_chain_height,
        pool_hash_rate,
        connected_since,
        pool_total_rewards: MicroMinotari::from(pool_total_rewards),
        estimated_earnings,
    }))
}
