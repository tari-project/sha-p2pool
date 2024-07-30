// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use itertools::Itertools;
use log::error;

use crate::server::http::stats::models::Stats;
use crate::server::http::stats::server::AppState;

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

    let current_height = state.share_chain.tip_height().await.map_err(|error| {
        error!(target: LOG_TARGET, "Failed to get tip height of share chain: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // hash rate
    let pool_hash_rate = state.share_chain.hash_rate().await.map_err(|error| {
        error!(target: LOG_TARGET, "Failed to get hash rate of share chain: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(Stats { connected, num_of_miners, last_block_won, current_height, pool_hash_rate }))
}
