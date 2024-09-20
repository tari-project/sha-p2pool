// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{collections::HashMap, sync::Arc, time::Instant};

use log::{debug, error, info, warn};
use minotari_app_grpc::tari_rpc::{
    base_node_client::BaseNodeClient,
    pow_algo::PowAlgos,
    sha_p2_pool_server::ShaP2Pool,
    GetNewBlockRequest,
    GetNewBlockResponse,
    GetNewBlockTemplateWithCoinbasesRequest,
    NewBlockTemplateRequest,
    SubmitBlockRequest,
    SubmitBlockResponse,
};
use num_format::{Locale, ToFormattedString};
use tari_common_types::types::FixedHash;
use tari_core::{
    consensus::ConsensusManager,
    proof_of_work::{randomx_difficulty, randomx_factory::RandomXFactory, sha3x_difficulty, Difficulty, PowAlgorithm},
};
use tari_shutdown::ShutdownSignal;
use tari_utilities::hex::Hex;
use tokio::sync::{RwLock, Semaphore};
use tonic::{Request, Response, Status};
const MIN_DIFFICULTY_REDUCTION_RATE: u64 = 2000;

use crate::{
    server::{
        grpc::{error::Error, util, MAX_ACCEPTABLE_GRPC_TIMEOUT},
        http::stats::{
            algo_stat_key,
            MINER_STAT_ACCEPTED_BLOCKS_COUNT,
            MINER_STAT_REJECTED_BLOCKS_COUNT,
            P2POOL_STAT_ACCEPTED_BLOCKS_COUNT,
            P2POOL_STAT_REJECTED_BLOCKS_COUNT,
        },
        p2p,
        stats_store::StatsStore,
    },
    sharechain::{block::Block, BlockValidationParams, ShareChain, SHARE_COUNT},
};

const LOG_TARGET: &str = "p2pool::server::grpc::p2pool";

pub fn min_difficulty(consensus_manager: &ConsensusManager, pow: PowAlgorithm, height: u64) -> Difficulty {
    let consensus_constants = consensus_manager.consensus_constants(height);
    match pow {
        PowAlgorithm::RandomX => consensus_constants
            .min_pow_difficulty(pow)
            .checked_div_u64(MIN_DIFFICULTY_REDUCTION_RATE)
            .expect("checked_div_u64 should only fail on div by 0"),
        PowAlgorithm::Sha3x => consensus_constants
            .min_pow_difficulty(pow)
            .checked_div_u64(MIN_DIFFICULTY_REDUCTION_RATE)
            .expect("checked_div_u64 should only fail on div by 0"),
    }
}

/// P2Pool specific gRPC service to provide `get_new_block` and `submit_block` functionalities.
pub(crate) struct ShaP2PoolGrpc<S>
where S: ShareChain
{
    /// Base node client
    client: Arc<RwLock<BaseNodeClient<tonic::transport::Channel>>>,
    /// P2P service client
    p2p_client: p2p::ServiceClient,
    /// SHA-3 share chain
    share_chain_sha3x: Arc<S>,
    /// RandomX share chain
    share_chain_random_x: Arc<S>,
    /// Stats store
    stats_store: Arc<StatsStore>,
    /// Block validation params to be used when checking block difficulty.
    block_validation_params: BlockValidationParams,
    sha3_block_height_difficulty_cache: Arc<RwLock<HashMap<u64, Difficulty>>>,
    randomx_block_height_difficulty_cache: Arc<RwLock<HashMap<u64, Difficulty>>>,
    stats_max_difficulty_since_last_success: Arc<RwLock<Difficulty>>,
    consensus_manager: ConsensusManager,
    submit_block_semaphore: Arc<Semaphore>,
}

impl<S> ShaP2PoolGrpc<S>
where S: ShareChain
{
    pub async fn new(
        base_node_address: String,
        p2p_client: p2p::ServiceClient,
        share_chain_sha3x: Arc<S>,
        share_chain_random_x: Arc<S>,
        stats_store: Arc<StatsStore>,
        shutdown_signal: ShutdownSignal,
        random_x_factory: RandomXFactory,
        consensus_manager: ConsensusManager,
        genesis_block_hash: FixedHash,
    ) -> Result<Self, Error> {
        Ok(Self {
            client: Arc::new(RwLock::new(
                util::connect_base_node(base_node_address, shutdown_signal).await?,
            )),
            p2p_client,
            share_chain_sha3x,
            share_chain_random_x,
            stats_store,
            block_validation_params: BlockValidationParams::new(
                random_x_factory,
                consensus_manager.clone(),
                genesis_block_hash,
            ),
            sha3_block_height_difficulty_cache: Arc::new(RwLock::new(HashMap::new())),
            randomx_block_height_difficulty_cache: Arc::new(RwLock::new(HashMap::new())),
            stats_max_difficulty_since_last_success: Arc::new(RwLock::new(Difficulty::min())),
            consensus_manager,
            submit_block_semaphore: Arc::new(Semaphore::new(1)),
        })
    }

    /// Submits a new block to share chain and broadcasts to the p2p network.
    pub async fn submit_share_chain_block(&self, block: &Block) -> Result<(), Status> {
        let pow_algo = block.original_block_header.pow.pow_algo;
        let share_chain = match pow_algo {
            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
        };
        match share_chain.submit_block(block).await {
            Ok(_) => {
                self.stats_store
                    .inc(&algo_stat_key(pow_algo, MINER_STAT_ACCEPTED_BLOCKS_COUNT), 1)
                    .await;
                let res = self
                    .p2p_client
                    .broadcast_block(block)
                    .await
                    .map_err(|error| Status::internal(error.to_string()));
                if res.is_ok() {
                    info!(target: LOG_TARGET, "Broadcast new block: {:?}", block.hash.to_hex());
                }
                res
            },
            Err(error) => {
                warn!(target: LOG_TARGET, "Failed to add new block: {error:?}");
                self.stats_store
                    .inc(&algo_stat_key(pow_algo, MINER_STAT_REJECTED_BLOCKS_COUNT), 1)
                    .await;
                Ok(())
            },
        }
    }
}

#[tonic::async_trait]
impl<S> ShaP2Pool for ShaP2PoolGrpc<S>
where S: ShareChain
{
    /// Returns a new block (that can be mined) which contains all the shares generated
    /// from the current share chain as coinbase transactions.
    async fn get_new_block(
        &self,
        request: Request<GetNewBlockRequest>,
    ) -> Result<Response<GetNewBlockResponse>, Status> {
        let timer = Instant::now();
        // extract pow algo
        let grpc_block_header_pow = request
            .into_inner()
            .pow
            .ok_or(Status::invalid_argument("missing pow in request"))?;
        let grpc_pow_algo = PowAlgos::from_i32(grpc_block_header_pow.pow_algo)
            .ok_or_else(|| Status::internal("invalid block header pow algo in request"))?;
        let pow_algo = match grpc_pow_algo {
            PowAlgos::Randomx => PowAlgorithm::RandomX,
            PowAlgos::Sha3x => PowAlgorithm::Sha3x,
        };

        // request original block template to get reward
        let req = NewBlockTemplateRequest {
            algo: Some(grpc_block_header_pow.clone()),
            max_weight: 0,
        };
        let template_response = self
            .client
            .write()
            .await
            .get_new_block_template(req)
            .await?
            .into_inner();
        let miner_data = template_response
            .miner_data
            .ok_or_else(|| Status::internal("missing miner data"))?;
        let reward = miner_data.reward;

        // request new block template with shares as coinbases
        let share_chain = match pow_algo {
            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
        };
        let shares = share_chain.generate_shares(reward).await;

        let mut response = self
            .client
            .write()
            .await
            .get_new_block_template_with_coinbases(GetNewBlockTemplateWithCoinbasesRequest {
                algo: Some(grpc_block_header_pow),
                max_weight: 0,
                coinbases: shares,
            })
            .await?
            .into_inner();

        // set target difficulty
        let miner_data = response
            .miner_data
            .clone()
            .ok_or_else(|| Status::internal("missing miner data"))?;

        let grpc_block = response
            .block
            .as_ref()
            .ok_or_else(|| Status::internal("missing missing block"))?;
        let height = grpc_block
            .header
            .as_ref()
            .map(|h| h.height)
            .ok_or_else(|| Status::internal("missing missing header"))?;
        let actual_diff = Difficulty::from_u64(miner_data.target_difficulty)
            .map_err(|e| Status::internal(format!("Invalid target difficulty: {}", e)))?;
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
        let min_difficulty = min_difficulty(&self.consensus_manager, pow_algo, height);
        let mut target_difficulty = Difficulty::from_u64(
            miner_data
                .target_difficulty
                .checked_div(SHARE_COUNT)
                .expect("Should only fail on div by 0"),
        )
        .map_err(|error| {
            error!("Failed to get target difficulty: {error:?}");
            Status::internal(format!("Failed to get target difficulty:  {}", error))
        })?;
        if target_difficulty < min_difficulty {
            target_difficulty = min_difficulty;
        }

        if let Some(miner_data) = response.miner_data.as_mut() {
            miner_data.target_difficulty = target_difficulty.as_u64();
        }

        if timer.elapsed() > MAX_ACCEPTABLE_GRPC_TIMEOUT {
            warn!("get_new_block took {}ms", timer.elapsed().as_millis());
        }
        Ok(Response::new(GetNewBlockResponse {
            block: Some(response),
            target_difficulty: target_difficulty.as_u64(),
        }))
    }

    /// Validates the submitted block with the p2pool network, checks for difficulty matching
    /// with network (using base node), submits mined block to base node and submits new p2pool block
    /// to p2pool network.
    #[allow(clippy::too_many_lines)]
    async fn submit_block(
        &self,
        request: Request<SubmitBlockRequest>,
    ) -> Result<Response<SubmitBlockResponse>, Status> {
        let timer = Instant::now();
        debug!("Queuing for submit block");
        // Only one submit at a time
        let _permit = self.submit_block_semaphore.acquire().await;
        debug!("submit_block permit acquired: {}", timer.elapsed().as_millis());

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
        let grpc_pow_algo = PowAlgos::from_i32(i32::try_from(grpc_block_header_pow.pow_algo).map_err(|error| {
            error!("Failed to get pow algo: {error:?}");
            Status::internal("general error")
        })?)
        .ok_or_else(|| Status::internal("invalid block header pow algo in request"))?;

        // get new share chain block
        let pow_algo = match grpc_pow_algo {
            PowAlgos::Randomx => PowAlgorithm::RandomX,
            PowAlgos::Sha3x => PowAlgorithm::Sha3x,
        };
        let share_chain = match pow_algo {
            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
        };
        let mut block = share_chain
            .new_block(grpc_block)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;

        let origin_block_header = &&block.original_block_header.clone();

        // Check block's difficulty compared to the latest network one to increase the probability
        // to get the block accepted (and also a block with lower difficulty than latest one is invalid anyway).
        let request_block_difficulty = match origin_block_header.pow.pow_algo {
            PowAlgorithm::Sha3x => {
                sha3x_difficulty(origin_block_header).map_err(|error| Status::internal(error.to_string()))?
            },
            PowAlgorithm::RandomX => randomx_difficulty(
                origin_block_header,
                self.block_validation_params.random_x_factory(),
                self.block_validation_params.genesis_block_hash(),
                self.block_validation_params.consensus_manager(),
            )
            .map_err(|error| Status::internal(error.to_string()))?,
        };
        info!(
            "Submitted {} block difficulty: {}",
            origin_block_header.pow.pow_algo, request_block_difficulty
        );
        // TODO: Cache this so that we don't ask each time. If we have a block we should not
        // waste time before submitting it, or we might lose a share
        // let mut network_difficulty_stream = self
        //     .client
        //     .lock()
        //     .await
        //     .get_network_difficulty(HeightRequest {
        //         from_tip: 0,
        //         start_height: origin_block_header.height - 1,
        //         end_height: origin_block_header.height,
        //     })
        //     .await?
        //     .into_inner();
        // let mut network_difficulty_matches = false;
        // while let Ok(Some(diff_resp)) = network_difficulty_stream.message().await {
        //     dbg!("Diff resp: {:?}", &diff_resp);
        //     if origin_block_header.height == diff_resp.height + 1
        //         && request_block_difficulty.as_u64() >= diff_resp.difficulty
        //     {
        //         network_difficulty_matches = true;
        //     }
        // }
        let network_difficulty = match origin_block_header.pow.pow_algo {
            PowAlgorithm::Sha3x => self
                .sha3_block_height_difficulty_cache
                .read()
                .await
                .get(&(origin_block_header.height))
                .copied()
                .unwrap_or_else(Difficulty::min),
            PowAlgorithm::RandomX => self
                .randomx_block_height_difficulty_cache
                .read()
                .await
                .get(&(origin_block_header.height))
                .copied()
                .unwrap_or_else(Difficulty::min),
        };
        let network_difficulty_matches = request_block_difficulty >= network_difficulty;
        let mut max_difficulty = self.stats_max_difficulty_since_last_success.write().await;
        if *max_difficulty < request_block_difficulty {
            *max_difficulty = request_block_difficulty;
        }

        block.achieved_difficulty = request_block_difficulty;

        if network_difficulty_matches {
            // submit block to base node
            let (metadata, extensions, _inner) = request.into_parts();
            info!("🔗 Submitting block  {} to base node...", origin_block_header.hash());

            let grpc_request = Request::from_parts(metadata, extensions, grpc_request_payload);
            match self.client.write().await.submit_block(grpc_request).await {
                Ok(_resp) => {
                    *max_difficulty = Difficulty::min();
                    self.stats_store
                        .inc(&algo_stat_key(pow_algo, P2POOL_STAT_ACCEPTED_BLOCKS_COUNT), 1)
                        .await;
                    info!(
                        "💰 New matching block found and sent to network! Block hash: {}",
                        origin_block_header.hash()
                    );
                    block.sent_to_main_chain = true;
                    self.submit_share_chain_block(&block).await?;
                },
                Err(error) => {
                    warn!(
                        "Failed to submit block  {} to Tari network: {error:?}",
                        origin_block_header.hash()
                    );
                    self.stats_store
                        .inc(&algo_stat_key(pow_algo, P2POOL_STAT_REJECTED_BLOCKS_COUNT), 1)
                        .await;
                    block.sent_to_main_chain = false;
                    self.submit_share_chain_block(&block).await?;

                    return Err(Status::internal(error.to_string()));
                },
            }
        } else {
            block.sent_to_main_chain = false;
            // Don't error if we can't submit it.
            match self.submit_share_chain_block(&block).await {
                Ok(_) => {
                    let pow_type = origin_block_header.pow.pow_algo.to_string();
                    info!("🔗 Block submitted to {} share chain!", pow_type);
                },
                Err(error) => {
                    warn!("Failed to submit block to share chain: {error:?}");
                },
            };
        }

        let stats = self
            .stats_store
            .get_many(&[
                algo_stat_key(pow_algo, MINER_STAT_ACCEPTED_BLOCKS_COUNT),
                algo_stat_key(pow_algo, MINER_STAT_REJECTED_BLOCKS_COUNT),
                algo_stat_key(pow_algo, P2POOL_STAT_ACCEPTED_BLOCKS_COUNT),
                algo_stat_key(pow_algo, P2POOL_STAT_REJECTED_BLOCKS_COUNT),
            ])
            .await;
        info!(
            "========= Max difficulty: {}. Network difficulty {}. Miner(A/R): {}/{}. Pool(A/R) {}/{}. ==== ",
            max_difficulty.as_u64().to_formatted_string(&Locale::en),
            network_difficulty.as_u64().to_formatted_string(&Locale::en),
            stats[0],
            stats[1],
            stats[2],
            stats[3]
        );

        if timer.elapsed() > MAX_ACCEPTABLE_GRPC_TIMEOUT {
            warn!("submit_block took {}ms", timer.elapsed().as_millis());
        }

        Ok(Response::new(SubmitBlockResponse {
            block_hash: block.hash.to_vec(),
        }))
    }
}
