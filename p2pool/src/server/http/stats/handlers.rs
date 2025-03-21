// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Response,
    Json,
};
use log::{error, info};
use prometheus::{Encoder, Gauge, GaugeVec, IntGauge, Opts, Registry, TextEncoder};
use serde::Serialize;
use tari_core::proof_of_work::PowAlgorithm;
use tari_utilities::{epoch_time::EpochTime, hex::Hex};
use tokio::sync::oneshot;

use super::MAX_ACCEPTABLE_HTTP_TIMEOUT;
use crate::server::{
    http::{server::AppState, stats::models::Stats, stats_collector::GetStatsResponse},
    p2p::{ConnectedPeerInfo, P2pServiceQuery},
};

const LOG_TARGET: &str = "tari::p2pool::server::stats::get";
const METRICS_LOG_TARGET: &str = "tari::p2pool::server::stats::metrics";

#[derive(Serialize)]
pub(crate) struct BlockResult {
    hash: String,
    timestamp: EpochTime,
    prev_hash: String,
    height: u64,
    miner_wallet_address: String,
    sent_to_main_chain: bool,
    target_difficulty: u64,
    candidate_block_height: u64,
    // candidate_block_prev_hash: String,
    algo: String,
}

#[derive(Serialize)]
pub(crate) struct PeerList {
    allow_list: Vec<ConnectedPeerInfo>,
    grey_list: Vec<ConnectedPeerInfo>,
}

#[derive(Serialize)]
pub(crate) struct ConnectionsResponse {
    peers: Vec<ConnectedPeerInfo>,
}

pub(crate) async fn handle_connections(State(state): State<AppState>) -> Result<Json<ConnectionsResponse>, StatusCode> {
    let timer = std::time::Instant::now();
    let (tx, rx) = oneshot::channel();
    state
        .p2p_service_client
        .send(P2pServiceQuery::GetConnections(tx))
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

    Ok(Json(ConnectionsResponse { peers: res }))
}

pub(crate) async fn handle_peers(State(state): State<AppState>) -> Result<Json<PeerList>, StatusCode> {
    let timer = std::time::Instant::now();
    let (tx, rx) = oneshot::channel();
    state
        .p2p_service_client
        .send(P2pServiceQuery::GetPeers(tx))
        .await
        .map_err(|error| {
            error!(target: LOG_TARGET, "Failed to get connection info: {error:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut res = rx.await.map_err(|e| {
        error!(target: LOG_TARGET, "Failed to receive from oneshot: {e:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    res.0
        .sort_by(|a, b| a.peer_info.current_sha3x_height.cmp(&b.peer_info.current_sha3x_height));

    if timer.elapsed() > MAX_ACCEPTABLE_HTTP_TIMEOUT {
        error!(target: LOG_TARGET, "handle_connections took too long: {}ms", timer.elapsed().as_millis());
    }
    Ok(Json(PeerList {
        allow_list: res.0.clone(),
        grey_list: res.1.clone(),
    }))
}

pub(crate) async fn handle_chain(
    Query(params): Query<HashMap<String, String>>,
    // algo: PowAlgorithm,
    // height: u64,
    // count: u64,
    State(state): State<AppState>,
) -> Result<Json<Vec<BlockResult>>, StatusCode> {
    let timer = std::time::Instant::now();
    let pow_algo = match params.get("algo") {
        Some(algo) => match algo.to_lowercase().as_str() {
            "sha3x" => PowAlgorithm::Sha3x,
            "randomx" => PowAlgorithm::RandomX,
            _ => {
                error!(target: LOG_TARGET, "Invalid algo: {algo}");
                return Err(StatusCode::BAD_REQUEST);
            },
        },
        None => {
            error!(target: LOG_TARGET, "Missing algo");
            return Err(StatusCode::BAD_REQUEST);
        },
    };
    let height = match params.get("height") {
        Some(height) => match height.parse::<u64>() {
            Ok(height) => height,
            Err(e) => {
                error!(target: LOG_TARGET, "Invalid height: {e:?}");
                return Err(StatusCode::BAD_REQUEST);
            },
        },
        None => 0u64,
    };
    let count = match params.get("count") {
        Some(count) => match count.parse::<usize>() {
            Ok(count) => count,
            Err(e) => {
                error!(target: LOG_TARGET, "Invalid count: {e:?}");
                return Err(StatusCode::BAD_REQUEST);
            },
        },
        None => 20,
    };
    let (tx, rx) = oneshot::channel();
    state
        .p2p_service_client
        .send(P2pServiceQuery::GetChain {
            pow_algo,
            height,
            count,
            response: tx,
        })
        .await
        .map_err(|error| {
            error!(target: LOG_TARGET, "Failed to get chain: {error:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut res = rx.await.map_err(|e| {
        error!(target: LOG_TARGET, "Failed to receive from oneshot: {e:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if timer.elapsed() > MAX_ACCEPTABLE_HTTP_TIMEOUT {
        error!(target: LOG_TARGET, "handle_chain took too long: {}ms", timer.elapsed().as_millis());
    }

    let mut return_value = Vec::with_capacity(res.len());
    for block in &mut res {
        return_value.push(BlockResult {
            hash: block.hash.to_hex(),
            timestamp: block.timestamp,
            prev_hash: block.prev_hash.to_hex(),
            height: block.height,
            miner_wallet_address: block.miner_wallet_address.to_base58(),
            sent_to_main_chain: block.sent_to_main_chain,
            target_difficulty: block.target_difficulty().as_u64(),
            candidate_block_height: block.original_header.height,
            algo: pow_algo.to_string().to_lowercase(),
        });
    }
    Ok(Json(return_value))
}

pub(crate) async fn handle_miners_with_shares(
    State(_state): State<AppState>,
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

    let (last_gossip_message, peer_id, squad) = state.stats_client.get_stats_info().await.map_err(|error| {
        error!(target: LOG_TARGET, "Failed to get last gossip message: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let (rx_stats, sha3x_stats) = get_chain_stats(state.clone()).await?;
    let connected_since = None;
    let (tx, rx) = oneshot::channel();
    state
        .p2p_service_client
        .send(P2pServiceQuery::ConnectionInfo(tx))
        .await
        .map_err(|error| {
            error!(target: LOG_TARGET, "Failed to get connection info: {error:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let connection_info = rx.await.map_err(|error| {
        error!(target: LOG_TARGET, "Failed to get connection info: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let stats = Stats {
        connection_info,
        connected_since,
        randomx_stats: rx_stats,
        sha3x_stats,
        last_gossip_message,
        peer_id: peer_id.map(|p| p.to_base58()).unwrap_or_default(),
        squad,
    };
    if timer.elapsed() > MAX_ACCEPTABLE_HTTP_TIMEOUT {
        error!(target: LOG_TARGET, "handle_get_stats took too long: {}ms", timer.elapsed().as_millis());
    }
    Ok(Json(stats))
}

#[allow(clippy::too_many_lines)]
async fn get_chain_stats(state: AppState) -> Result<(GetStatsResponse, GetStatsResponse), StatusCode> {
    let stats_client = state.stats_client.clone();
    let (rx_stats, sha3x_stats) = (
        stats_client
            .get_chain_stats(PowAlgorithm::RandomX)
            .await
            .map_err(|error| {
                error!(target: LOG_TARGET, "Failed to get chain stats: {error:?}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?,
        stats_client
            .get_chain_stats(PowAlgorithm::Sha3x)
            .await
            .map_err(|error| {
                error!(target: LOG_TARGET, "Failed to get chain stats: {error:?}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?,
    );

    Ok((rx_stats, sha3x_stats))
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
    // let share_chain_length = 0;
    // let share_chain_height = 0;
    //
    // let result = ChainStats {
    //     // num_of_miners: shares.keys().len(),
    //     // num_of_miners: 0,
    //     share_chain_height,
    //     share_chain_length,
    //     // pool_hash_rate: pool_hash_rate.to_string(),
    //     // pool_total_earnings: MicroMinotari::from(0),
    //     // pool_total_estimated_earnings: EstimatedEarnings::new(MicroMinotari::from(0)),
    //     // total_earnings: Default::default(),
    //     // estimated_earnings: Default::default(),
    //     // miner_block_stats: miner_block_stats(state.stats_store.clone(), algo).await,
    //     // p2pool_block_stats: p2pool_block_stats(state.stats_store.clone(), algo).await,
    //     squad: SquadDetails::new(state.squad.to_string(), state.squad.formatted()),
    // };

    // stats_cache.update(result.clone(), algo).await;

    // Ok(result)
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
pub(crate) async fn handle_metrics(State(state): State<AppState>) -> Response {
    let timer = std::time::Instant::now();
    info!(target: METRICS_LOG_TARGET, "handle_metrics");

    // Create a new registry
    let registry = Registry::new();

    // Get stats data
    let stats_result = get_metrics_data(state, &registry).await;

    // Generate the metrics output
    let encoder = TextEncoder::new();
    let metric_families = registry.gather();
    let mut buffer = Vec::new();

    if let Err(e) = encoder.encode(&metric_families, &mut buffer) {
        error!(target: METRICS_LOG_TARGET, "Failed to encode metrics: {e:?}");
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(format!("Failed to encode metrics: {e:?}").into())
            .unwrap();
    }

    if timer.elapsed() > MAX_ACCEPTABLE_HTTP_TIMEOUT {
        error!(target: METRICS_LOG_TARGET, "handle_metrics took too long: {}ms", timer.elapsed().as_millis());
    }

    // Return the metrics in Prometheus format
    match stats_result {
        Ok(_) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/plain; version=0.0.4")
            .body(String::from_utf8_lossy(&buffer).to_string().into())
            .unwrap(),
        Err(e) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(format!("Failed to collect metrics: {e:?}").into())
            .unwrap(),
    }
}

async fn get_metrics_data(state: AppState, registry: &Registry) -> Result<(), StatusCode> {
    // Get full stats which includes chain stats and connection info
    let stats = handle_get_stats(State(state.clone())).await?.0;

    // Get peer info
    let (tx, rx) = oneshot::channel();
    state
        .p2p_service_client
        .send(P2pServiceQuery::GetPeers(tx))
        .await
        .map_err(|error| {
            error!(target: METRICS_LOG_TARGET, "Failed to get peers info: {error:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let peers_info = rx.await.map_err(|error| {
        error!(target: METRICS_LOG_TARGET, "Failed to receive peers info: {error:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Register and set metrics

    // We'll use serde_json to convert the stats to a value we can extract fields from
    let rx_stats_json = serde_json::to_value(&stats.randomx_stats).unwrap_or_default();
    let sha3x_stats_json = serde_json::to_value(&stats.sha3x_stats).unwrap_or_default();

    // Chain height metrics
    let chain_height = GaugeVec::new(Opts::new("p2pool_chain_height", "Current chain height"), &["algorithm"]).unwrap();
    registry.register(Box::new(chain_height.clone())).unwrap();

    if let Some(height) = rx_stats_json.get("height").and_then(|v| v.as_u64()) {
        chain_height.with_label_values(&["randomx"]).set(height as f64);
    }

    if let Some(height) = sha3x_stats_json.get("height").and_then(|v| v.as_u64()) {
        chain_height.with_label_values(&["sha3x"]).set(height as f64);
    }

    // Shares metrics
    let total_shares = GaugeVec::new(Opts::new("p2pool_total_shares", "Total number of shares"), &[
        "algorithm",
    ])
    .unwrap();
    registry.register(Box::new(total_shares.clone())).unwrap();

    if let Some(shares) = rx_stats_json.get("total_shares").and_then(|v| v.as_u64()) {
        total_shares.with_label_values(&["randomx"]).set(shares as f64);
    }

    if let Some(shares) = sha3x_stats_json.get("total_shares").and_then(|v| v.as_u64()) {
        total_shares.with_label_values(&["sha3x"]).set(shares as f64);
    }

    // My shares metrics
    let my_shares = GaugeVec::new(Opts::new("p2pool_my_shares", "Number of my shares"), &["algorithm"]).unwrap();
    registry.register(Box::new(my_shares.clone())).unwrap();

    if let Some(shares) = rx_stats_json.get("num_my_shares").and_then(|v| v.as_u64()) {
        my_shares.with_label_values(&["randomx"]).set(shares as f64);
    }

    if let Some(shares) = sha3x_stats_json.get("num_my_shares").and_then(|v| v.as_u64()) {
        my_shares.with_label_values(&["sha3x"]).set(shares as f64);
    }

    // Connection metrics
    let connected_peers = Gauge::new("p2pool_connected_peers", "Number of connected peers").unwrap();
    registry.register(Box::new(connected_peers.clone())).unwrap();
    connected_peers.set(stats.connection_info.connected_peers as f64);

    // Connection counters
    let pending_incoming = Gauge::new("p2pool_pending_incoming", "Number of pending incoming connections").unwrap();
    registry.register(Box::new(pending_incoming.clone())).unwrap();
    pending_incoming.set(stats.connection_info.network_info.connection_counters.pending_incoming as f64);

    let pending_outgoing = Gauge::new("p2pool_pending_outgoing", "Number of pending outgoing connections").unwrap();
    registry.register(Box::new(pending_outgoing.clone())).unwrap();
    pending_outgoing.set(stats.connection_info.network_info.connection_counters.pending_outgoing as f64);

    let established_incoming = Gauge::new(
        "p2pool_established_incoming",
        "Number of established incoming connections",
    )
    .unwrap();
    registry.register(Box::new(established_incoming.clone())).unwrap();
    established_incoming.set(
        stats
            .connection_info
            .network_info
            .connection_counters
            .established_incoming as f64,
    );

    let established_outgoing = Gauge::new(
        "p2pool_established_outgoing",
        "Number of established outgoing connections",
    )
    .unwrap();
    registry.register(Box::new(established_outgoing.clone())).unwrap();
    established_outgoing.set(
        stats
            .connection_info
            .network_info
            .connection_counters
            .established_outgoing as f64,
    );

    // Peer list metrics
    let whitelist_peers = IntGauge::new("p2pool_whitelist_peers", "Number of whitelisted peers").unwrap();
    registry.register(Box::new(whitelist_peers.clone())).unwrap();
    whitelist_peers.set(peers_info.0.len() as i64);

    let greylist_peers = IntGauge::new("p2pool_greylist_peers", "Number of greylisted peers").unwrap();
    registry.register(Box::new(greylist_peers.clone())).unwrap();
    greylist_peers.set(peers_info.1.len() as i64);

    Ok(())
}
