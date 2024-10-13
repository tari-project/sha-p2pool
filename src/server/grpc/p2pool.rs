// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::HashMap,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use digest::typenum::Diff;
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
use tari_common_types::{tari_address::TariAddress, types::FixedHash};
use tari_core::{
    consensus::ConsensusManager,
    proof_of_work::{randomx_difficulty, randomx_factory::RandomXFactory, sha3x_difficulty, Difficulty, PowAlgorithm},
};
use tari_shutdown::ShutdownSignal;
use tari_utilities::hex::Hex;
use tokio::{
    sync::{RwLock, Semaphore},
    time::timeout,
};
use tonic::{Request, Response, Status};

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
        p2p::Squad,
        stats_store::StatsStore,
    },
    sharechain::{block::Block, BlockValidationParams, ShareChain, SHARE_COUNT},
};

const LOG_TARGET: &str = "tari::p2pool::server::grpc::p2pool";

pub fn min_difficulty(consensus_manager: &ConsensusManager, pow: PowAlgorithm, height: u64) -> Difficulty {
    let consensus_constants = consensus_manager.consensus_constants(height);
    match pow {
        PowAlgorithm::RandomX => {
            Difficulty::from_u64(consensus_constants.min_pow_difficulty(pow).as_u64() / 4).expect("Bad difficulty")
        },
        PowAlgorithm::Sha3x => {
            Difficulty::from_u64(consensus_constants.min_pow_difficulty(pow).as_u64() / 4).expect("Bad difficulty")
        }, // SHA min difficulty is too low. Will be updated in future
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
    squad: Squad,
    coinbase_extras_sha3x: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    coinbase_extras_random_x: Arc<RwLock<HashMap<String, Vec<u8>>>>,
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
        squad: Squad,
        coinbase_extras_sha3x: Arc<RwLock<HashMap<String, Vec<u8>>>>,
        coinbase_extras_random_x: Arc<RwLock<HashMap<String, Vec<u8>>>>,
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
            squad,
            coinbase_extras_sha3x,
            coinbase_extras_random_x,
        })
    }

    /// Submits a new block to share chain and broadcasts to the p2p network.
    pub async fn submit_share_chain_block(&self, block: &Block) -> Result<(), Status> {
        let pow_algo = block.original_block_header.pow.pow_algo;
        let share_chain = match pow_algo {
            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
        };
        let timer = Instant::now();
        warn!(target: LOG_TARGET, "dbg 2. {}", timer.elapsed().as_millis());
        match share_chain.submit_block(block).await {
            Ok(_) => {
                warn!(target: LOG_TARGET, "dbg 2. {}", timer.elapsed().as_millis());
                self.stats_store
                    .inc(&algo_stat_key(pow_algo, MINER_STAT_ACCEPTED_BLOCKS_COUNT), 1)
                    .await;
                warn!(target: LOG_TARGET, "dbg 2. {}", timer.elapsed().as_millis());
                let res = self
                    .p2p_client
                    .broadcast_block(block)
                    .await
                    .map_err(|error| Status::internal(error.to_string()));
                warn!(target: LOG_TARGET, "dbg 2. {}", timer.elapsed().as_millis());
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
    #[allow(clippy::too_many_lines)]
    async fn get_new_block(
        &self,
        request: Request<GetNewBlockRequest>,
    ) -> Result<Response<GetNewBlockResponse>, Status> {
        let timeout_duration = MAX_ACCEPTABLE_GRPC_TIMEOUT;

        let result = timeout(timeout_duration, async {
            let timer = Instant::now();

            let grpc_req = request.into_inner();

            // extract pow algo
            let grpc_block_header_pow = grpc_req.pow.ok_or(Status::invalid_argument("missing pow in request"))?;
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
            let _miner_data = template_response
                .miner_data
                .ok_or_else(|| Status::internal("missing miner data"))?;
            // let reward = miner_data.reward;

            // update coinbase extras cache
            let wallet_payment_address = TariAddress::from_str(grpc_req.wallet_payment_address.as_str())
                .map_err(|error| Status::failed_precondition(format!("Invalid wallet payment address:  {}", error)))?;
            let mut coinbase_extras_lock = match pow_algo {
                PowAlgorithm::RandomX => self.coinbase_extras_random_x.write().await,
                PowAlgorithm::Sha3x => self.coinbase_extras_sha3x.write().await,
            };
            coinbase_extras_lock.insert(
                wallet_payment_address.to_base58(),
                util::convert_coinbase_extra(self.squad.clone(), grpc_req.coinbase_extra)
                    .map_err(|error| Status::internal(format!("failed to convert coinbase extra {error:?}")))?,
            );
            drop(coinbase_extras_lock);

            // request new block template with shares as coinbases
            let share_chain = match pow_algo {
                PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
                PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
            };
            let shares = share_chain.generate_shares(self.squad.clone()).await;

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
            let chain = match pow_algo {
                PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
                PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
            };
            let mut target_difficulty = chain
                .get_target_difficulty()
                .await
                .map_err(|e| Status::internal("Could not get target difficutly"))?;

            if target_difficulty < min_difficulty {
                target_difficulty = min_difficulty;
            }

            if target_difficulty > actual_diff {
                warn!(
                    target: LOG_TARGET,
                    "Target difficulty is higher than actual difficulty. Target: {}, Actual: {}",
                    target_difficulty,
                    actual_diff
                );
                // Never go higher than the network.
                target_difficulty = actual_diff;
            }
            if let Some(miner_data) = response.miner_data.as_mut() {
                miner_data.target_difficulty = target_difficulty.as_u64();
            }

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
            Ok(response) => response,
            Err(e) => {
                error!(target: LOG_TARGET, "get_new_block timed out: {e:?}");
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
        // if self.submit_block_semaphore.available_permits() == 0 {
        //     return Err(Status::resource_exhausted("submit_block semaphore is full"));
        // }
        // // Only one submit at a time
        // let _permit = self.submit_block_semaphore.acquire().await;
        debug!(target: LOG_TARGET, "submit_block permit acquired: {}", timer.elapsed().as_millis());

        warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
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
        let grpc_pow_algo = PowAlgos::from_i32(i32::try_from(grpc_block_header_pow.pow_algo).map_err(|error| {
            error!("Failed to get pow algo: {error:?}");
            Status::internal("general error")
        })?)
        .ok_or_else(|| Status::internal("invalid block header pow algo in request"))?;

        warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
        debug!(target: LOG_TARGET, "Trace - getting new block from share chain: {}", timer.elapsed().as_millis());
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
            .new_block(grpc_block, self.squad.clone())
            .await
            .map_err(|error| Status::internal(error.to_string()))?;

            warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
            let origin_block_header = &&block.original_block_header.clone();

        debug!(target: LOG_TARGET, "Trace - getting block difficulty: {}", timer.elapsed().as_millis());
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
            target: LOG_TARGET,
            "Submitted {} block difficulty: {}",
            origin_block_header.pow.pow_algo, request_block_difficulty
        );
        warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
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
        debug!(target: LOG_TARGET, "Trace - getting network difficulty: {}", timer.elapsed().as_millis());
        warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
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
        warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
        let network_difficulty_matches = request_block_difficulty >= network_difficulty;
        debug!(target: LOG_TARGET, "Trace - saving max difficulty: {}", timer.elapsed().as_millis());
        let mut max_difficulty = self.stats_max_difficulty_since_last_success.write().await;
        if *max_difficulty < request_block_difficulty {
            *max_difficulty = request_block_difficulty;
        }

        block.achieved_difficulty = request_block_difficulty;

        debug!(target: LOG_TARGET, "Trace - checking if can submit to main chain: {}", timer.elapsed().as_millis());
        warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
        if network_difficulty_matches {
            // submit block to base node
            warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
            let (metadata, extensions, _inner) = request.into_parts();
            info!(target: LOG_TARGET, "🔗 Submitting block  {} to base node...", origin_block_header.hash());

            let grpc_request = Request::from_parts(metadata, extensions, grpc_request_payload);
            warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
            match self.client.write().await.submit_block(grpc_request).await {
                Ok(_resp) => {
                    *max_difficulty = Difficulty::min();
                    warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
                    self.stats_store
                        .inc(&algo_stat_key(pow_algo, P2POOL_STAT_ACCEPTED_BLOCKS_COUNT), 1)
                        .await;
                    info!(
                        target: LOG_TARGET,
                        "💰 New matching block found and sent to network! Block hash: {}",
                        origin_block_header.hash()
                    );
                    block.sent_to_main_chain = true;
                    self.submit_share_chain_block(&block).await?;
                },
                Err(error) => {
                    warn!(
                        target: LOG_TARGET,
                        "Failed to submit block  {} to Tari network: {error:?}",
                        origin_block_header.hash()
                    );
                    warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
                    self.stats_store
                        .inc(&algo_stat_key(pow_algo, P2POOL_STAT_REJECTED_BLOCKS_COUNT), 1)
                        .await;
                    block.sent_to_main_chain = false;
                    self.submit_share_chain_block(&block).await?;

                    if timer.elapsed() > MAX_ACCEPTABLE_GRPC_TIMEOUT {
                        warn!(target: LOG_TARGET, "submit_block took {}ms and errored", timer.elapsed().as_millis());
                    }
                    return Err(Status::internal(error.to_string()));
                },
            }
        } else {
            warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
        debug!(target: LOG_TARGET, "Trace - submitting to share chain: {}", timer.elapsed().as_millis());
            block.sent_to_main_chain = false;
            // Don't error if we can't submit it.
            match self.submit_share_chain_block(&block).await {
                Ok(_) => {
                    let pow_type = origin_block_header.pow.pow_algo.to_string();
                    info!(target: LOG_TARGET, "🔗 Block submitted to {} share chain!", pow_type);
                },
                Err(error) => {
                    warn!(target: LOG_TARGET, "Failed to submit block to share chain: {error:?}");
                },
            };
        }

        warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
        debug!(target: LOG_TARGET, "Trace - getting stats:{} ", timer.elapsed().as_millis());
        let stats = self
            .stats_store
            .get_many(&[
                algo_stat_key(pow_algo, MINER_STAT_ACCEPTED_BLOCKS_COUNT),
                algo_stat_key(pow_algo, MINER_STAT_REJECTED_BLOCKS_COUNT),
                algo_stat_key(pow_algo, P2POOL_STAT_ACCEPTED_BLOCKS_COUNT),
                algo_stat_key(pow_algo, P2POOL_STAT_REJECTED_BLOCKS_COUNT),
            ])
            .await;
        warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
        info!(target: LOG_TARGET,
            "========= Max difficulty: {}. Network difficulty {}. Miner(A/R): {}/{}. Pool(A/R) {}/{}. ==== ",
            max_difficulty.as_u64().to_formatted_string(&Locale::en),
            network_difficulty.as_u64().to_formatted_string(&Locale::en),
            stats[0],
            stats[1],
            stats[2],
            stats[3]
        );

        warn!(target: LOG_TARGET, "here 1: {}",  timer.elapsed().as_millis());
        if timer.elapsed() > MAX_ACCEPTABLE_GRPC_TIMEOUT {
            warn!(target: LOG_TARGET, "submit_block took {}ms", timer.elapsed().as_millis());
        }

        Ok(Response::new(SubmitBlockResponse {
            block_hash: block.hash.to_vec(),
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
