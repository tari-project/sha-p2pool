// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::{HashMap, VecDeque},
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};

use log::{debug, error, info, warn};
use minotari_app_grpc::tari_rpc::{
    pow_algo::PowAlgos,
    sha_p2_pool_server::ShaP2Pool,
    Empty,
    GetNewBlockRequest,
    GetNewBlockResponse,
    GetNewBlockTemplateWithCoinbasesRequest,
    GetTipInfoRequest,
    GetTipInfoResponse,
    SubmitBlockRequest,
    SubmitBlockResponse,
};
use minotari_node_grpc_client::BaseNodeGrpcClient;
use tari_common_types::{tari_address::TariAddress, types::FixedHash};
use tari_core::{
    blocks::Block,
    consensus::ConsensusManager,
    proof_of_work::{
        randomx_difficulty,
        randomx_factory::RandomXFactory,
        sha3x_difficulty,
        Difficulty,
        PowAlgorithm,
        PowData,
    },
};
use tari_utilities::hex::Hex;
use tokio::{sync::RwLock, time::timeout};
use tonic::{Request, Response, Status};

use crate::{
    server::{
        grpc::{error::Error, util::convert_coinbase_extra, MAX_ACCEPTABLE_GRPC_TIMEOUT},
        http::stats_collector::StatsBroadcastClient,
        p2p::client::ServiceClient,
    },
    sharechain::{p2block::P2Block, BlockValidationParams, ShareChain},
    PROFILING_LOG_TARGET,
};

pub const MAX_STORED_TEMPLATES_RX: usize = 100;
pub const MAX_STORED_TEMPLATES_SHA3X: usize = 100;
const LOG_TARGET: &str = "tari::p2pool::server::grpc::p2pool";

/// P2Pool specific gRPC service to provide `get_new_block` and `submit_block` functionalities.
pub(crate) struct ShaP2PoolGrpc<S>
where S: ShareChain
{
    /// Base node client
    // client: Arc<RwLock<BaseNodeClient<tonic::transport::Channel>>>,
    client_address: String,
    /// P2P service client
    p2p_client: ServiceClient,
    /// SHA-3 share chain
    share_chain_sha3x: Arc<S>,
    /// RandomX share chain
    share_chain_random_x: Arc<S>,
    /// Stats store
    stats_broadcast: StatsBroadcastClient,
    /// Block validation params to be used when checking block difficulty.
    block_validation_params: BlockValidationParams,
    sha3_block_height_difficulty_cache: Arc<RwLock<HashMap<u64, Difficulty>>>,
    randomx_block_height_difficulty_cache: Arc<RwLock<HashMap<u64, Difficulty>>>,
    stats_max_difficulty_since_last_success: Arc<RwLock<Difficulty>>,
    template_store_sha3x: RwLock<HashMap<FixedHash, P2Block>>,
    list_of_templates_sha3x: RwLock<VecDeque<FixedHash>>,
    template_store_rx: RwLock<HashMap<FixedHash, P2Block>>,
    list_of_templates_rx: RwLock<VecDeque<FixedHash>>,
    are_we_synced_with_randomx_p2pool: Arc<AtomicBool>,
    are_we_synced_with_sha3x_p2pool: Arc<AtomicBool>,
    squad: String,
    cache_get_tip_info: RwLock<(Instant, Option<GetTipInfoResponse>)>,
    cache_time: std::time::Duration,
}

impl<S> ShaP2PoolGrpc<S>
where S: ShareChain
{
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        base_node_address: String,
        p2p_client: ServiceClient,
        share_chain_sha3x: Arc<S>,
        share_chain_random_x: Arc<S>,
        random_x_factory: RandomXFactory,
        consensus_manager: ConsensusManager,
        genesis_block_hash: FixedHash,
        stats_broadcast: StatsBroadcastClient,
        are_we_synced_with_randomx_p2pool: Arc<AtomicBool>,
        are_we_synced_with_sha3x_p2pool: Arc<AtomicBool>,
        squad: String,
        cache_time: std::time::Duration,
    ) -> Result<Self, Error> {
        Ok(Self {
            // client: Arc::new(RwLock::new(
            // util::connect_base_node(base_node_address, shutdown_signal).await?,
            // )),
            client_address: base_node_address,
            p2p_client,
            share_chain_sha3x,
            share_chain_random_x,
            stats_broadcast,
            block_validation_params: BlockValidationParams::new(
                random_x_factory,
                consensus_manager.clone(),
                genesis_block_hash,
            ),
            sha3_block_height_difficulty_cache: Arc::new(RwLock::new(HashMap::new())),
            randomx_block_height_difficulty_cache: Arc::new(RwLock::new(HashMap::new())),
            stats_max_difficulty_since_last_success: Arc::new(RwLock::new(Difficulty::min())),
            template_store_sha3x: RwLock::new(HashMap::new()),
            list_of_templates_sha3x: RwLock::new(VecDeque::with_capacity(MAX_STORED_TEMPLATES_SHA3X + 1)),
            template_store_rx: RwLock::new(HashMap::new()),
            list_of_templates_rx: RwLock::new(VecDeque::with_capacity(MAX_STORED_TEMPLATES_RX + 1)),
            are_we_synced_with_randomx_p2pool,
            are_we_synced_with_sha3x_p2pool,
            squad,
            cache_get_tip_info: RwLock::new((Instant::now(), None)),
            cache_time,
        })
    }

    /// Submits a new block to share chain and broadcasts to the p2p network.
    pub async fn submit_share_chain_block(&self, block: P2Block) -> Result<(), Status> {
        let pow_algo = block.original_header.pow.pow_algo;
        match pow_algo {
            PowAlgorithm::RandomX => {
                if !self.are_we_synced_with_randomx_p2pool.load(Ordering::SeqCst) {
                    info!(target: LOG_TARGET, "We are not synced yet, not submitting block atm");
                    return Ok(());
                }
            },
            PowAlgorithm::Sha3x => {
                if !self.are_we_synced_with_sha3x_p2pool.load(Ordering::SeqCst) {
                    info!(target: LOG_TARGET, "We are not synced yet, not submitting block atm");
                    return Ok(());
                }
            },
        };
        let share_chain = match pow_algo {
            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
        };
        let hash_string = block.original_header.hash().to_hex();
        match share_chain.submit_block(block.clone()).await {
            Ok(new_tip) => {
                if new_tip.new_tip.is_some() {
                    let _unused = self.stats_broadcast.send_miner_block_accepted(pow_algo);
                    let new_block = Arc::new(block);
                    for uncle in share_chain.get_blocks(&new_block.uncles).await {
                        let _unused = self
                            .p2p_client
                            .broadcast_block(uncle.clone())
                            .inspect_err(|e| error!(target: LOG_TARGET, "Failed to broadcast uncle block: {e}"));
                    }
                    // new_blocks.append(&mut uncles);
                    let res = self
                        .p2p_client
                        .broadcast_block(new_block)
                        .map_err(|error| Status::internal(error.to_string()));
                    if res.is_ok() {
                        info!(target: LOG_TARGET, "Broadcast new block: {:?}", hash_string);
                    }
                    return res;
                } else {
                    // missed the tip
                    let _unused = self.stats_broadcast.send_miner_block_rejected(pow_algo);
                }
                Ok(())
            },
            Err(error) => {
                warn!(target: LOG_TARGET, "Failed to add new block: {error:?}");
                let _unused = self.stats_broadcast.send_miner_block_rejected(pow_algo);
                Ok(())
            },
        }
    }
}

#[tonic::async_trait]
impl<S> ShaP2Pool for ShaP2PoolGrpc<S>
where S: ShareChain
{
    async fn get_tip_info(&self, _request: Request<GetTipInfoRequest>) -> Result<Response<GetTipInfoResponse>, Status> {
        debug!(target: LOG_TARGET, "get_tip_info called");
        let timer = Instant::now();
        let timeout_duration = MAX_ACCEPTABLE_GRPC_TIMEOUT;

        let cache_time = self.cache_time;

        let (cache_expired, cached_response) = {
            let rwlock = self.cache_get_tip_info.read().await;
            if rwlock.1.is_none() || rwlock.0.elapsed() > cache_time {
                debug!(target: LOG_TARGET, "get_tip_info cache expired, updating cache: {:?}", rwlock.0.elapsed());
                // If there is a value, but it is expired, we only want one thread to update it, so
                // if there is already a lock on the thread, then we can return an expired value.
                (true, rwlock.1.clone())
            } else {
                (false, rwlock.1.clone())
            }
        };

        if !cache_expired {
            // This should always be true, but just in case, we check again.
            if let Some(cache) = cached_response {
                debug!(target: LOG_TARGET, "get_tip_info cache hit rx/sha:{}/{}: {:?}", cache.p2pool_rx_height, cache.p2pool_sha_height, timer.elapsed());
                return Ok(Response::new(cache));
            }
        }
        // Otherwise see if another thread is trying to update the cache.
        let mut cache_lock = self.cache_get_tip_info.try_write();
        if cache_lock.is_err() {
            // Another thread is already updating the cache, so we can return the expired value.
            if let Some(cache) = cached_response {
                debug!(target: LOG_TARGET, "get_tip_info cache expired, but another process is busy, returning old value: {:?}", timer.elapsed());
                return Ok(Response::new(cache));
            } else {
                // wait for a lock
                cache_lock = Ok(self.cache_get_tip_info.write().await);
            }
        }
        let mut cache_lock = cache_lock.unwrap();
        if cache_lock.0.elapsed() < cache_time && cache_lock.1.is_some() {
            debug!(target: LOG_TARGET, "get_tip_info cache hit after write lock rx/sha:{}/{}: {:?}", cache_lock.1.as_ref().unwrap().p2pool_rx_height, cache_lock.1.as_ref().unwrap().p2pool_sha_height, timer.elapsed());
            return Ok(Response::new(cache_lock.1.clone().unwrap()));
        }

        let result = timeout(timeout_duration, async {
            let (rx_height, rx_hash) = self
                .share_chain_random_x
                .get_tip()
                .await
                .map_err(|e| Status::internal(e.to_string()))?
                .map(|s| (s.0, s.1.to_vec()))
                .unwrap_or((0, FixedHash::zero().to_vec()));

            let (sha3_height, sha3_hash) = self
                .share_chain_sha3x
                .get_tip()
                .await
                .map_err(|e| Status::internal(e.to_string()))?
                .map(|s| (s.0, s.1.to_vec()))
                .unwrap_or((0, FixedHash::zero().to_vec()));

            let mut client = BaseNodeGrpcClient::connect(self.client_address.clone())
                .await
                .map_err(|e| Status::internal(format!("Could not connect to base node {e:?}")))?;
            let tip_info = client.get_tip_info(Empty {}).await?.into_inner();
            let (node_height, node_tip_hash) = tip_info
                .metadata
                .map(|m| (m.best_block_height, m.best_block_hash))
                .unwrap_or_else(|| (0, FixedHash::zero().to_vec()));

            let response = GetTipInfoResponse {
                node_height,
                node_tip_hash,
                p2pool_rx_height: rx_height,
                p2pool_rx_tip_hash: rx_hash.to_vec(),
                p2pool_sha_height: sha3_height,
                p2pool_sha_tip_hash: sha3_hash.to_vec(),
            };
            Ok(response)
        })
        .await;

        match result {
            Ok(response) => match response {
                Ok(r) => {
                    debug!(target: LOG_TARGET, "get_tip_info responded successfully. height:{}rx/{}sha: {:?}",r.p2pool_rx_height, r.p2pool_sha_height, timer.elapsed());
                    // let mut write_lock = self.cache_get_tip_info.write().await;
                    cache_lock.0 = Instant::now();
                    cache_lock.1 = Some(r.clone());
                    Ok(Response::new(r.clone()))
                },
                Err(response) => {
                    error!(target: LOG_TARGET, "get_tip_info failed: {:?}", response);
                    Err(response)
                },
            },
            Err(_) => {
                error!(target: LOG_TARGET, "get_tip_info timed out after {:?}", timer.elapsed());
                Err(Status::deadline_exceeded("get_tip_info timed out"))
            },
        }
    }

    /// Returns a new block (that can be mined) which contains all the shares generated
    /// from the current share chain as coinbase transactions.
    #[allow(clippy::too_many_lines)]
    async fn get_new_block(
        &self,
        request: Request<GetNewBlockRequest>,
    ) -> Result<Response<GetNewBlockResponse>, Status> {
        let timer = Instant::now();
        let timeout_duration = MAX_ACCEPTABLE_GRPC_TIMEOUT;

        let result = timeout(timeout_duration, async {
            let grpc_req = request.into_inner();

            // extract pow algo
            let grpc_block_header_pow = grpc_req.pow.ok_or(Status::invalid_argument("missing pow in request"))?;
            let grpc_pow_algo: PowAlgos = grpc_block_header_pow
                .pow_algo
                .try_into()
                .map_err(|_| Status::internal("invalid block header pow algo in request"))?;
            let pow_algo = match grpc_pow_algo {
                PowAlgos::Randomx => PowAlgorithm::RandomX,
                PowAlgos::Sha3x => PowAlgorithm::Sha3x,
            };

            debug!(target: PROFILING_LOG_TARGET, "get_new_block timer: {:?}", timer.elapsed());
            // update coinbase extras cache
            // investigate lock usage
            let wallet_payment_address = TariAddress::from_str(grpc_req.wallet_payment_address.as_str())
                .map_err(|error| Status::failed_precondition(format!("Invalid wallet payment address:  {}", error)))?;

            debug!(target: PROFILING_LOG_TARGET, "get_new_block timer: {:?}", timer.elapsed());
            // request new block template with shares as coinbases
            let (share_chain, synced_status) = match pow_algo {
                PowAlgorithm::RandomX => (
                    self.share_chain_random_x.clone(),
                    self.are_we_synced_with_randomx_p2pool.load(Ordering::SeqCst),
                ),
                PowAlgorithm::Sha3x => (
                    self.share_chain_sha3x.clone(),
                    self.are_we_synced_with_sha3x_p2pool.load(Ordering::SeqCst),
                ),
            };
            let squad = self.squad.clone();
            let coinbase_extra = convert_coinbase_extra(squad, grpc_req.coinbase_extra).unwrap_or_default();
            debug!(target: PROFILING_LOG_TARGET, "get_new_block timer: {:?}", timer.elapsed());
            let mut new_tip_block = (*share_chain
                .generate_new_tip_block(&wallet_payment_address, coinbase_extra.clone())
                .await
                .map_err(|error| Status::internal(format!("failed to get new tip block {error:?}")))?)
            .clone();
            debug!(target: PROFILING_LOG_TARGET, "get_new_block timer: {:?}", timer.elapsed());
            let (shares, mut target_difficulty) = share_chain
                .generate_shares_and_get_target_difficulty(&new_tip_block, !synced_status)
                .await
                .map_err(|error| Status::internal(format!("failed to generate shares {error:?}")))?;

            debug!(target: PROFILING_LOG_TARGET, "get_new_block timer: {:?}", timer.elapsed());
            let mut client = BaseNodeGrpcClient::connect(self.client_address.clone())
                .await
                .map_err(|e| Status::internal(format!("Could not connect to base node {e:?}")))?;

            debug!(target: PROFILING_LOG_TARGET, "get_new_block timer: {:?}", timer.elapsed());
            let mut response = client
                .get_new_block_template_with_coinbases(GetNewBlockTemplateWithCoinbasesRequest {
                    algo: Some(grpc_block_header_pow),
                    max_weight: 0,
                    coinbases: shares,
                })
                .await?
                .into_inner();

            debug!(target: PROFILING_LOG_TARGET, "get_new_block timer: {:?}", timer.elapsed());
            // set target difficulty
            let miner_data = response
                .miner_data
                .ok_or_else(|| Status::internal("missing miner data"))?;

            let grpc_block = response
                .block
                .as_ref()
                .ok_or_else(|| Status::internal("missing missing block"))?;
            let mut tari_block: Block = grpc_block
                .clone()
                .try_into()
                .map_err(|e| Status::internal(format!("Could not convert gprc block to tari block: {}", e)))?;
            // we set the nonce to 0 in order to find the template again.
            tari_block.header.nonce = 0;
            new_tip_block
                .populate_tari_data(tari_block)
                .map_err(|e| Status::internal(format!("Could not convert gprc block to p2p block: {}", e)))?;
            let tari_hash = new_tip_block.original_header.hash();

            let height = grpc_block
                .header
                .as_ref()
                .map(|h| h.height)
                .ok_or_else(|| Status::internal("missing missing header"))?;
            let actual_diff = Difficulty::from_u64(miner_data.target_difficulty)
                .map_err(|e| Status::internal(format!("Invalid target difficulty: {}", e)))?;
            debug!(target: PROFILING_LOG_TARGET, "get_new_block timer: {:?}", timer.elapsed());
            match pow_algo {
                PowAlgorithm::RandomX => self
                    .randomx_block_height_difficulty_cache
                    .write()
                    .await
                    .insert(height, actual_diff),
                PowAlgorithm::Sha3x => self
                    .sha3_block_height_difficulty_cache
                    .write()
                    .await
                    .insert(height, actual_diff),
            };
            debug!(target: PROFILING_LOG_TARGET, "get_new_block timer: {:?}", timer.elapsed());
            new_tip_block
                .change_target_difficulty(target_difficulty)
                .map_err(|e| Status::internal(format!("Invalid target difficulty: {}", e)))?;

            if let Some(miner_data) = response.miner_data.as_mut() {
                match Difficulty::from_u64(miner_data.target_difficulty) {
                    Ok(diff) => {
                        let _unused = self.stats_broadcast.send_network_difficulty(pow_algo, diff);
                    },
                    Err(e) => {
                        error!(target: LOG_TARGET, "Invalid target difficulty: {e:?}");
                    },
                }

                if target_difficulty.as_u64() < miner_data.target_difficulty && synced_status {
                    miner_data.target_difficulty = target_difficulty.as_u64();
                }

                // If we are not synced, return the target difficulty from the miner data.
                // In future we should remove this duplicate data and only rely on the target_difficulty
                // in miner_data.
                if !synced_status {
                    target_difficulty = Difficulty::from_u64(miner_data.target_difficulty).unwrap();
                }
            }

            let _unused = self.stats_broadcast.send_target_difficulty(pow_algo, target_difficulty);

            debug!(target: PROFILING_LOG_TARGET, "get_new_block timer: {:?}", timer.elapsed());
            // save template
            match pow_algo {
                PowAlgorithm::Sha3x => {
                    let mut write_lock = self.list_of_templates_sha3x.write().await;
                    write_lock.push_back(tari_hash);
                    if write_lock.len() > MAX_STORED_TEMPLATES_SHA3X {
                        let _ = write_lock.pop_front();
                    }
                },
                PowAlgorithm::RandomX => {
                    let mut write_lock = self.list_of_templates_rx.write().await;
                    write_lock.push_back(tari_hash);
                    if write_lock.len() > MAX_STORED_TEMPLATES_RX {
                        let _ = write_lock.pop_front();
                    }
                },
            };

            debug!(target: PROFILING_LOG_TARGET, "get_new_block timer: {:?}", timer.elapsed());
            match pow_algo {
                PowAlgorithm::Sha3x => self.template_store_sha3x.write().await.insert(tari_hash, new_tip_block),
                PowAlgorithm::RandomX => self.template_store_rx.write().await.insert(tari_hash, new_tip_block),
            };

            debug!(target: PROFILING_LOG_TARGET, "get_new_block timer: {:?}", timer.elapsed());
            if timer.elapsed() > MAX_ACCEPTABLE_GRPC_TIMEOUT {
                warn!(target: LOG_TARGET, "get_new_block took {}ms", timer.elapsed().as_millis());
            }
            Ok(Response::new(GetNewBlockResponse {
                block: Some(response),
                target_difficulty: target_difficulty.as_u64(),
            }))
        })
        .await;

        match result {
            Ok(response) => response.inspect_err(|e| error!(target: LOG_TARGET, "get_new_block failed: {e:?}")),
            Err(_) => {
                error!(target: LOG_TARGET, "get_new_block timed out after {}ms.", timer.elapsed().as_millis());
                Err(Status::deadline_exceeded("get_new_block timed out"))
            },
        }
    }

    /// Validates the submitted block with the p2pool network, checks for difficulty matching
    /// with network (using base node), submits mined block to base node and submits new p2pool block
    /// to p2pool network.
    #[allow(clippy::too_many_lines)]
    async fn submit_block(
        &self,
        request: Request<SubmitBlockRequest>,
    ) -> Result<Response<SubmitBlockResponse>, Status> {
        let timeout_duration = MAX_ACCEPTABLE_GRPC_TIMEOUT;

        let result = timeout(timeout_duration, async {
            let timer = Instant::now();
            // Only one submit at a time

            debug!(target: LOG_TARGET, "submit_block permit acquired: {}", timer.elapsed().as_millis());

            debug!("Trace - getting grpc fields");
            // get all grpc request related data
            let grpc_block = request.get_ref();
            let grpc_request_payload = grpc_block
                .block
                .clone()
                .ok_or_else(|| Status::internal("missing block in request"))?;
            let grpc_block_header = grpc_request_payload
                .header
                .clone()
                .ok_or_else(|| Status::internal("missing block header in request"))?;
            let grpc_block_header_pow = grpc_block_header
                .pow
                .ok_or_else(|| Status::internal("missing block header pow in request"))?;
            let grpc_pow_algo:PowAlgos = (i32::try_from(grpc_block_header_pow.pow_algo).map_err(|error| {
                error!("Failed to get pow algo: {error:?}");
                Status::internal("general error")
            })?).try_into()
                .map_err(|_| Status::internal("invalid block header pow algo in request"))?;

            debug!(target: LOG_TARGET, "Trace - getting new block from share chain: {}", timer.elapsed().as_millis());
            // get new share chain block
            let pow_algo = match grpc_pow_algo {
                PowAlgos::Randomx => PowAlgorithm::RandomX,
                PowAlgos::Sha3x => PowAlgorithm::Sha3x,
            };

            let mut tari_block: Block = grpc_request_payload
                .clone()
                .try_into()
                .map_err(|e| Status::internal(format!("Could not convert gprc block to tari block: {}", e)))?;
            let mined_nonce = tari_block.header.nonce;
            let temp_pow_data = tari_block.header.pow.pow_data.clone();
            tari_block.header.nonce = 0;
            tari_block.header.pow.pow_data =PowData::default();
            let tari_hash = tari_block.header.hash();
            tari_block.header.nonce = mined_nonce;
            tari_block.header.pow.pow_data = temp_pow_data;
            let mut p2pool_block = match pow_algo{
                PowAlgorithm::Sha3x =>  self
                    .template_store_sha3x
                    .read()
                    .await.get(&tari_hash)
                    .ok_or(Status::internal("missing template"))?.clone(),
                PowAlgorithm::RandomX =>  self
                    .template_store_rx
                    .read()
                    .await.get(&tari_hash)
                    .ok_or(Status::internal("missing template"))?.clone(),
            };


            p2pool_block.original_header= tari_block.header;
            let mined_tari_hash = p2pool_block.original_header.hash();

            debug!(target: LOG_TARGET, "Trace - getting block difficulty: {}", timer.elapsed().as_millis());
            // Check block's difficulty compared to the latest network one to increase the probability
            // to get the block accepted (and also a block with lower difficulty than latest one is invalid anyway).
            let request_block_difficulty = match p2pool_block.original_header.pow.pow_algo {
                PowAlgorithm::Sha3x => sha3x_difficulty(&p2pool_block.original_header)
                    .map_err(|error| Status::internal(error.to_string()))?,
                PowAlgorithm::RandomX => randomx_difficulty(
                    &p2pool_block.original_header,
                    self.block_validation_params.random_x_factory(),
                    self.block_validation_params.genesis_block_hash(),
                    self.block_validation_params.consensus_manager(),
                )
                    .map_err(|error| Status::internal(error.to_string()))?,
            };
            info!(
            target: LOG_TARGET,
            "Submitted {} block difficulty: {}",
            p2pool_block.original_header.pow.pow_algo, request_block_difficulty
        );

            debug!(target: LOG_TARGET, "Trace - getting network difficulty: {}", timer.elapsed().as_millis());
            let network_difficulty = match p2pool_block.original_header.pow.pow_algo {
                PowAlgorithm::Sha3x => self
                    .sha3_block_height_difficulty_cache
                    .read()
                    .await
                    .get(&(p2pool_block.original_header.height))
                    .copied()
                    .unwrap_or_else(Difficulty::min),
                PowAlgorithm::RandomX => self
                    .randomx_block_height_difficulty_cache
                    .read()
                    .await
                    .get(&(p2pool_block.original_header.height))
                    .copied()
                    .unwrap_or_else(Difficulty::min),
            };
            let network_difficulty_matches = request_block_difficulty >= network_difficulty;
            debug!(target: LOG_TARGET, "Trace - saving max difficulty: {}", timer.elapsed().as_millis());
            let mut max_difficulty = self.stats_max_difficulty_since_last_success.write().await;
            if *max_difficulty < request_block_difficulty {
                *max_difficulty = request_block_difficulty;
            }

            debug!(target: LOG_TARGET, "Trace - checking if can submit to main chain: {}", timer.elapsed().as_millis());
            if network_difficulty_matches {
                // submit block to base node
                let (metadata, extensions, _inner) = request.into_parts();
                info!(target: LOG_TARGET, "🔗 Submitting block  {} to base node...", mined_tari_hash);

                let grpc_request = Request::from_parts(metadata, extensions, grpc_request_payload);
                let mut client = BaseNodeGrpcClient::connect(self.client_address.clone())
                .await
                .map_err(|e| Status::internal(format!("Could not connect to base node {e:?}")))?;

                match client.submit_block(grpc_request).await {
                    Ok(_resp) => {
                        *max_difficulty = Difficulty::min();
                        let _unused = self.stats_broadcast.send_pool_block_accepted(pow_algo);
                        info!(
                        target: LOG_TARGET,
                        "💰 New matching block found and sent to network! Block hash: {}",
                        mined_tari_hash
                    );
                        p2pool_block.sent_to_main_chain = true;
                    },
                    Err(error) => {
                        warn!(
                        target: LOG_TARGET,
                        "Failed to submit block  {} to Tari network: {error:?}",
                        mined_tari_hash
                    );
                        warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
                        let _unused = self.stats_broadcast.send_pool_block_rejected(pow_algo);
                        p2pool_block.sent_to_main_chain = false;

                        if timer.elapsed() > MAX_ACCEPTABLE_GRPC_TIMEOUT {
                            warn!(target: LOG_TARGET, "submit_block took {}ms and errored", timer.elapsed().as_millis());
                        }
                        return Err(Status::internal(error.to_string()));
                    },
                }
            }

            debug!(target: LOG_TARGET, "Trace - submitting to share chain: {}", timer.elapsed().as_millis());
            // Don't error if we can't submit it.
            let pow_type = p2pool_block.original_header.pow.pow_algo.to_string();
            match self.submit_share_chain_block(p2pool_block).await {
                Ok(_) => {
                    info!(target: LOG_TARGET, "🔗 Block submitted to {} share chain!", pow_type);
                },
                Err(error) => {
                    warn!(target: LOG_TARGET, "Failed to submit block to share chain: {error:?}");
                },
            };



            if timer.elapsed() > MAX_ACCEPTABLE_GRPC_TIMEOUT {
                warn!(target: LOG_TARGET, "submit_block took {}ms", timer.elapsed().as_millis());
            }

            Ok(Response::new(SubmitBlockResponse {
                block_hash: tari_hash.to_vec(),
            }))
        }).await;

        match result {
            Ok(response) => match response {
                Ok(response) => Ok(response),
                Err(e) => {
                    error!(target: LOG_TARGET, "submit_block failed: {e:?}");
                    Err(Status::internal("submit_block failed"))
                },
            },
            Err(e) => {
                error!(target: LOG_TARGET, "submit_block timed out: {e:?}");
                Err(Status::deadline_exceeded("submit_block timed out"))
            },
        }
    }
}
