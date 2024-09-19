// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use log::{debug, error, info, warn};
use minotari_app_grpc::tari_rpc::pow_algo::PowAlgos;
use minotari_app_grpc::tari_rpc::{
    base_node_client::BaseNodeClient, sha_p2_pool_server::ShaP2Pool, GetNewBlockRequest, GetNewBlockResponse,
    GetNewBlockTemplateWithCoinbasesRequest, NewBlockTemplateRequest, SubmitBlockRequest, SubmitBlockResponse,
};
use num_format::{Locale, ToFormattedString};
use std::collections::HashMap;
use std::sync::Arc;
use tari_common::configuration::Network;
use tari_common_types::types::FixedHash;
use tari_core::consensus;
use tari_core::consensus::ConsensusManager;
use tari_core::proof_of_work::randomx_factory::RandomXFactory;
use tari_core::proof_of_work::{randomx_difficulty, sha3x_difficulty, PowAlgorithm};
use tari_shutdown::ShutdownSignal;
use tari_utilities::hex::Hex;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};

use crate::server::http::stats::{
    algo_stat_key, MINER_STAT_ACCEPTED_BLOCKS_COUNT, MINER_STAT_REJECTED_BLOCKS_COUNT,
    P2POOL_STAT_ACCEPTED_BLOCKS_COUNT, P2POOL_STAT_REJECTED_BLOCKS_COUNT,
};
use crate::server::stats_store::StatsStore;
use crate::sharechain::BlockValidationParams;
use crate::{
    server::{
        grpc::{error::Error, util},
        p2p,
    },
    sharechain::{block::Block, ShareChain, SHARE_COUNT},
};

const LOG_TARGET: &str = "p2pool::server::grpc::p2pool";

pub fn min_difficulty(pow: PowAlgorithm) -> Result<u64, Error> {
    let network = Network::get_current_or_user_setting_or_default();
    let consensus_constants = match network {
        Network::MainNet => consensus::ConsensusConstants::mainnet(),
        Network::StageNet => consensus::ConsensusConstants::stagenet(),
        Network::NextNet => consensus::ConsensusConstants::nextnet(),
        Network::LocalNet => consensus::ConsensusConstants::localnet(),
        Network::Igor => consensus::ConsensusConstants::igor(),
        Network::Esmeralda => consensus::ConsensusConstants::esmeralda(),
    };
    let consensus_constants = consensus_constants.first().ok_or(Error::NoConsensusConstants)?;
    let min_difficulty = match pow {
        PowAlgorithm::RandomX => consensus_constants.min_pow_difficulty(pow).as_u64() / 2000,
        PowAlgorithm::Sha3x => consensus_constants.min_pow_difficulty(pow).as_u64(),
    };

    Ok(min_difficulty)
}

/// P2Pool specific gRPC service to provide `get_new_block` and `submit_block` functionalities.
pub(crate) struct ShaP2PoolGrpc<S>
where
    S: ShareChain,
{
    /// Base node client
    client: Arc<Mutex<BaseNodeClient<tonic::transport::Channel>>>,
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
    block_height_difficulty_cache: Arc<Mutex<HashMap<u64, u64>>>,
    stats_max_difficulty_since_last_success: Arc<Mutex<u64>>,
}

impl<S> ShaP2PoolGrpc<S>
where
    S: ShareChain,
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
            client: Arc::new(Mutex::new(
                util::connect_base_node(base_node_address, shutdown_signal).await?,
            )),
            p2p_client,
            share_chain_sha3x,
            share_chain_random_x,
            stats_store,
            block_validation_params: BlockValidationParams::new(
                random_x_factory,
                consensus_manager,
                genesis_block_hash,
            ),
            block_height_difficulty_cache: Arc::new(Mutex::new(HashMap::new())),
            stats_max_difficulty_since_last_success: Arc::new(Mutex::new(0)),
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
                info!(target: LOG_TARGET, "Broadcast new block: {:?}", block.hash.to_hex());
                self.p2p_client
                    .broadcast_block(block)
                    .await
                    .map_err(|error| Status::internal(error.to_string()))
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
where
    S: ShareChain,
{
    /// Returns a new block (that can be mined) which contains all the shares generated
    /// from the current share chain as coinbase transactions.
    async fn get_new_block(
        &self,
        request: Request<GetNewBlockRequest>,
    ) -> Result<Response<GetNewBlockResponse>, Status> {
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
        let template_response = self.client.lock().await.get_new_block_template(req).await?.into_inner();
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
            .lock()
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
            .clone()
            .miner_data
            .ok_or_else(|| Status::internal("missing miner data"))?;
        debug!("Inserting height cached difficulty: {}", miner_data.target_difficulty);
        if let Some(header) = &response.block {
            let height = header.header.as_ref().map(|h| h.height).unwrap_or(0);
            self.block_height_difficulty_cache
                .lock()
                .await
                .insert(height, miner_data.target_difficulty);
        }
        let min_difficulty =
            min_difficulty(pow_algo).map_err(|_| Status::internal("failed to get minimum difficulty"))?;
        let mut target_difficulty = miner_data.target_difficulty / SHARE_COUNT;
        if target_difficulty < min_difficulty {
            target_difficulty = min_difficulty;
        }

        if let Some(mut miner_data) = response.miner_data {
            miner_data.target_difficulty = target_difficulty;
            response.miner_data = Some(miner_data);
        }

        Ok(Response::new(GetNewBlockResponse {
            block: Some(response),
            target_difficulty,
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
        info!("Submitted block difficulty: {}", request_block_difficulty);
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
        let network_difficulty = *self
            .block_height_difficulty_cache
            .lock()
            .await
            .get(&(origin_block_header.height))
            .unwrap_or(&0);
        let network_difficulty_matches = request_block_difficulty.as_u64() >= network_difficulty;
        let mut max_difficulty = self.stats_max_difficulty_since_last_success.lock().await;
        if *max_difficulty < request_block_difficulty.as_u64() {
            *max_difficulty = request_block_difficulty.as_u64();
        }
        info!(
            "Max difficulty: {}. Network difficulty {}",
            max_difficulty.to_formatted_string(&Locale::en),
            network_difficulty.to_formatted_string(&Locale::en)
        );

        if !network_difficulty_matches {
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
            return Ok(Response::new(SubmitBlockResponse {
                block_hash: block.hash.to_vec(),
            }));
        }

        // submit block to base node
        let (metadata, extensions, _inner) = request.into_parts();
        info!("🔗 Submitting block  {} to base node...", origin_block_header.hash());

        let grpc_request = Request::from_parts(metadata, extensions, grpc_request_payload);
        match self.client.lock().await.submit_block(grpc_request).await {
            Ok(resp) => {
                self.stats_store
                    .inc(&algo_stat_key(pow_algo, P2POOL_STAT_ACCEPTED_BLOCKS_COUNT), 1)
                    .await;
                info!(
                    "💰 New matching block found and sent to network! Block hash: {}",
                    origin_block_header.hash()
                );
                block.sent_to_main_chain = true;
                self.submit_share_chain_block(&block).await?;
                Ok(resp)
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
                Ok(Response::new(SubmitBlockResponse {
                    block_hash: block.hash.to_vec(),
                }))
            },
        }
    }
}
