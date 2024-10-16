// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{collections::HashMap, sync::Arc};

use axum::{extract::State, http::StatusCode, Json};
use log::{error, info};
use serde::Serialize;
use tari_core::{proof_of_work::PowAlgorithm, transactions::tari_amount::MicroMinotari};
use tari_utilities::{epoch_time::EpochTime, hex::Hex};
use tokio::sync::oneshot;

use super::{models::ChainStats, MAX_ACCEPTABLE_HTTP_TIMEOUT};
use crate::server::{
    http::{
        server::AppState,
        stats::{
            algo_stat_key,
            models::{BlockStats, EstimatedEarnings, SquadDetails, Stats},
            MINER_STAT_ACCEPTED_BLOCKS_COUNT,
            MINER_STAT_REJECTED_BLOCKS_COUNT,
            P2POOL_STAT_ACCEPTED_BLOCKS_COUNT,
            P2POOL_STAT_REJECTED_BLOCKS_COUNT,
        },
    },
    p2p::{ConnectedPeerInfo, ConnectionCounters, ConnectionInfo, NetworkInfo, P2pServiceQuery},
};

const LOG_TARGET: &str = "tari::p2pool::server::stats::get";

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

#[derive(Serialize)]
pub(crate) struct Connections {
    allow_list: Vec<ConnectedPeerInfo>,
    grey_list: Vec<ConnectedPeerInfo>,
}
pub(crate) async fn handle_connections(State(state): State<AppState>) -> Result<Json<Connections>, StatusCode> {
    let timer = std::time::Instant::now();
    let (tx, rx) = oneshot::channel();
    state
        .p2p_service_client
        .send(P2pServiceQuery::GetConnectedPeers(tx))
        .await
        .map_err(|error| {
            error!(target: LOG_TARGET, "Failed to get connection info: {error:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let res = rx.await.map_err(|e| {
        error!(target: LOG_TARGET, "Failed to receive from oneshot: {e:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if timer.elapsed() > MAX_ACCEPTABLE_HTTP_TIMEOUT {
        error!(target: LOG_TARGET, "handle_connections took too long: {}ms", timer.elapsed().as_millis());
    }
    Ok(Json(Connections {
        allow_list: res.0.clone(),
        grey_list: res.1.clone(),
    }))
}

pub(crate) async fn handle_chain(State(state): State<AppState>) -> Result<Json<Vec<BlockResult>>, StatusCode> {
    let timer = std::time::Instant::now();
    // let chain = state.share_chain_sha3x.blocks(0).await.map_err(|error| {
    //     error!(target: LOG_TARGET, "Failed to get blocks of share chain: {error:?}");
    //     StatusCode::INTERNAL_SERVER_ERROR
    // })?;

    // let result = chain
    //     .iter()
    //     .map(|block| BlockResult {
    //         chain_id: block.chain_id.clone(),
    //         hash: block.hash.to_hex(),
    //         timestamp: block.timestamp,
    //         prev_hash: block.prev_hash.to_hex(),
    //         height: block.height,
    //         // original_block_header: block.original_block_header().clone(),
    //         miner_wallet_address: block.miner_wallet_address.as_ref().map(|a| a.to_base58()),
    //         sent_to_main_chain: block.sent_to_main_chain,
    //         achieved_difficulty: block.achieved_difficulty.as_u64(),
    //         candidate_block_prev_hash: block.original_block_header.prev_hash.to_hex(),
    //         candidate_block_height: block.original_block_header.height,
    //         algo: block.original_block_header.pow.pow_algo.to_string(),
    //     })
    //     .collect();
    todo!();

    // if timer.elapsed() > MAX_ACCEPTABLE_HTTP_TIMEOUT {
    // error!(target: LOG_TARGET, "handle_chain took too long: {}ms", timer.elapsed().as_millis());
    // }
    // Ok(Json())
}

pub(crate) async fn handle_miners_with_shares(
    State(state): State<AppState>,
) -> Result<Json<HashMap<String, HashMap<String, u64>>>, StatusCode> {
    // let timer = std::time::Instant::now();
    // let mut result = HashMap::with_capacity(2);
    // result.insert(
    //     PowAlgorithm::Sha3x.to_string().to_lowercase(),
    //     state
    //         .share_chain_sha3x
    //         .miners_with_shares(state.squad.clone())
    //         .await
    //         .map_err(|error| {
    //             error!(target: LOG_TARGET, "Failed to get Sha3x miners with shares: {error:?}");
    //             StatusCode::INTERNAL_SERVER_ERROR
    //         })?,
    // );
    // result.insert(
    //     PowAlgorithm::RandomX.to_string().to_lowercase(),
    //     state
    //         .share_chain_random_x
    //         .miners_with_shares(state.squad.clone())
    //         .await
    //         .map_err(|error| {
    //             error!(target: LOG_TARGET, "Failed to get RandomX miners with shares: {error:?}");
    //             StatusCode::INTERNAL_SERVER_ERROR
    //         })?,
    // );

    // if timer.elapsed() > MAX_ACCEPTABLE_HTTP_TIMEOUT {
    //     error!(target: LOG_TARGET, "handle_miners_with_shares took too long: {}ms", timer.elapsed().as_millis());
    // }

    // Ok(Json(result))
    todo!()
}

pub(crate) async fn handle_get_stats(State(state): State<AppState>) -> Result<Json<Stats>, StatusCode> {
    let timer = std::time::Instant::now();
    info!(target: LOG_TARGET, "handle_get_stats");

    let sha3x_stats = get_chain_stats(state.clone(), PowAlgorithm::Sha3x).await?;
    let randomx_stats = get_chain_stats(state.clone(), PowAlgorithm::RandomX).await?;
    // let peer_count = state.peer_store.peer_count().await;
    let peer_count = 0;
    let connected = peer_count > 0;
    // let connected_since = state.peer_store.last_connected();
    let connected_since = None;
    let (tx, rx) = oneshot::channel();
    state
        .p2p_service_client
        .send(P2pServiceQuery::GetConnectionInfo(tx))
        .await
        .map_err(|error| {
            error!(target: LOG_TARGET, "Failed to get connection info: {error:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let connection_info = rx.await.map_err(|error| {
        error!(target: LOG_TARGET, "Failed to get connection info: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    // let connection_info = ConnectionInfo {
    //     listener_addresses: vec![],
    //     connected_peers: 0,
    //     network_info: NetworkInfo {
    //         num_peers: 0,
    //         connection_counters: ConnectionCounters {
    //             pending_incoming: 0,
    //             pending_outgoing: 0,
    //             established_incoming: 0,
    //             established_outgoing: 0,
    //         },
    //     },
    // };

    let stats = Stats {
        connected,
        peer_count,
        connection_info,
        connected_since,
        randomx_stats,
        sha3x_stats,
    };
    if timer.elapsed() > MAX_ACCEPTABLE_HTTP_TIMEOUT {
        error!(target: LOG_TARGET, "handle_get_stats took too long: {}ms", timer.elapsed().as_millis());
    }
    Ok(Json(stats))
}

#[allow(clippy::too_many_lines)]
async fn get_chain_stats(state: AppState, algo: PowAlgorithm) -> Result<ChainStats, StatusCode> {
    return Ok(ChainStats {
        share_chain_height: 0,
        share_chain_length: 0,
        squad: SquadDetails::new(state.squad.to_string(), state.squad.formatted()),
    });
    // return from cache if possible
    // let stats_cache = state.stats_cache.clone();
    // if let Some(stats) = stats_cache.stats(algo).await {
    // return Ok(stats);
    // }

    // let share_chain = match algo {
    // PowAlgorithm::RandomX => state.share_chain_random_x.clone(),
    // PowAlgorithm::Sha3x => state.share_chain_sha3x.clone(),
    // };
    // let chain = share_chain.blocks(0).await.map_err(|error| {
    // error!(target: LOG_TARGET, "Failed to get blocks of share chain: {error:?}");
    // StatusCode::INTERNAL_SERVER_ERROR
    // })?;

    // connected

    // let shares = share_chain
    //     .miners_with_shares(state.squad.clone())
    //     .await
    //     .map_err(|error| {
    //         error!(target: LOG_TARGET, "Failed to get miners with shares: {error:?}");
    //         StatusCode::INTERNAL_SERVER_ERROR
    //     })?;

    // TODO: Remove this field

    // let share_chain_height = share_chain.tip_height().await.map_err(|error| {
    // error!(target: LOG_TARGET, "Failed to get tip height of share chain: {error:?}");
    // StatusCode::INTERNAL_SERVER_ERROR
    // })?;

    // hash rate
    // let pool_hash_rate = share_chain.hash_rate().await.map_err(|error| {
    // error!(target: LOG_TARGET, "Failed to get hash rate of share chain: {error:?}");
    // StatusCode::INTERNAL_SERVER_ERROR
    // })?;
    todo!();
    let share_chain_length = 0;
    let share_chain_height = 0;

    let result = ChainStats {
        // num_of_miners: shares.keys().len(),
        // num_of_miners: 0,
        share_chain_height,
        share_chain_length,
        // pool_hash_rate: pool_hash_rate.to_string(),
        // pool_total_earnings: MicroMinotari::from(0),
        // pool_total_estimated_earnings: EstimatedEarnings::new(MicroMinotari::from(0)),
        // total_earnings: Default::default(),
        // estimated_earnings: Default::default(),
        // miner_block_stats: miner_block_stats(state.stats_store.clone(), algo).await,
        // p2pool_block_stats: p2pool_block_stats(state.stats_store.clone(), algo).await,
        squad: SquadDetails::new(state.squad.to_string(), state.squad.formatted()),
    };

    // stats_cache.update(result.clone(), algo).await;

    Ok(result)
}

// vasync fn miner_block_stats(stats_store: Arc<StatsStore>, algo: PowAlgorithm) -> BlockStats {
//     BlockStats::new(
//         stats_store
//             .get(&algo_stat_key(algo, MINER_STAT_ACCEPTED_BLOCKS_COUNT))
//             .await,
//         stats_store
//             .get(&algo_stat_key(algo, MINER_STAT_REJECTED_BLOCKS_COUNT))
//             .await,
//     )
// }

// async fn p2pool_block_stats(stats_store: Arc<StatsStore>, algo: PowAlgorithm) -> BlockStats {
//     BlockStats::new(
//         stats_store
//             .get(&algo_stat_key(algo, P2POOL_STAT_ACCEPTED_BLOCKS_COUNT))
//             .await,
//         stats_store
//             .get(&algo_stat_key(algo, P2POOL_STAT_REJECTED_BLOCKS_COUNT))
//             .await,
//     )
// }
