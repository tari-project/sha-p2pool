// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::{HashMap, VecDeque},
    str::FromStr,
    sync::Arc,
    time::Instant,
};

use log::{debug, error, info, warn};
use minotari_app_grpc::tari_rpc::{
    base_node_client::BaseNodeClient,
    pow_algo::PowAlgos,
    sha_p2_pool_server::ShaP2Pool,
    GetNewBlockRequest,
    GetNewBlockResponse,
    GetNewBlockTemplateWithCoinbasesRequest,
    SubmitBlockRequest,
    SubmitBlockResponse,
};
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
use tari_shutdown::ShutdownSignal;
use tari_utilities::hex::Hex;
use tokio::{sync::RwLock, time::timeout};
use tonic::{Request, Response, Status};

use crate::{
    server::{
        grpc::{error::Error, util, util::convert_coinbase_extra, MAX_ACCEPTABLE_GRPC_TIMEOUT},
        http::stats_collector::StatsBroadcastClient,
        p2p,
        p2p::Squad,
    },
    sharechain::{p2block::P2Block, BlockValidationParams, ShareChain},
};

pub const MAX_STORED_TEMPLATES_RX: usize = 100;
pub const MAX_STORED_TEMPLATES_SHA3X: usize = 100;
const LOG_TARGET: &str = "tari::p2pool::server::grpc::p2pool";

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
    stats_broadcast: StatsBroadcastClient,
    /// Block validation params to be used when checking block difficulty.
    block_validation_params: BlockValidationParams,
    sha3_block_height_difficulty_cache: Arc<RwLock<HashMap<u64, Difficulty>>>,
    randomx_block_height_difficulty_cache: Arc<RwLock<HashMap<u64, Difficulty>>>,
    stats_max_difficulty_since_last_success: Arc<RwLock<Difficulty>>,
    squad: Squad,
    coinbase_extras_sha3x: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    coinbase_extras_random_x: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    template_store_sha3x: RwLock<HashMap<FixedHash, P2Block>>,
    list_of_templates_sha3x: RwLock<VecDeque<FixedHash>>,
    template_store_rx: RwLock<HashMap<FixedHash, P2Block>>,
    list_of_templates_rx: RwLock<VecDeque<FixedHash>>,
}

impl<S> ShaP2PoolGrpc<S>
where S: ShareChain
{
    pub async fn new(
        base_node_address: String,
        p2p_client: p2p::ServiceClient,
        share_chain_sha3x: Arc<S>,
        share_chain_random_x: Arc<S>,
        shutdown_signal: ShutdownSignal,
        random_x_factory: RandomXFactory,
        consensus_manager: ConsensusManager,
        genesis_block_hash: FixedHash,
        stats_broadcast: StatsBroadcastClient,
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
            stats_broadcast,
            block_validation_params: BlockValidationParams::new(
                random_x_factory,
                consensus_manager.clone(),
                genesis_block_hash,
            ),
            sha3_block_height_difficulty_cache: Arc::new(RwLock::new(HashMap::new())),
            randomx_block_height_difficulty_cache: Arc::new(RwLock::new(HashMap::new())),
            stats_max_difficulty_since_last_success: Arc::new(RwLock::new(Difficulty::min())),
            squad,
            coinbase_extras_sha3x,
            coinbase_extras_random_x,
            template_store_sha3x: RwLock::new(HashMap::new()),
            list_of_templates_sha3x: RwLock::new(VecDeque::with_capacity(MAX_STORED_TEMPLATES_SHA3X + 1)),
            template_store_rx: RwLock::new(HashMap::new()),
            list_of_templates_rx: RwLock::new(VecDeque::with_capacity(MAX_STORED_TEMPLATES_RX + 1)),
        })
    }

    /// Submits a new block to share chain and broadcasts to the p2p network.
    pub async fn submit_share_chain_block(&self, block: Arc<P2Block>) -> Result<(), Status> {
        let pow_algo = block.original_block.header.pow.pow_algo;
        let share_chain = match pow_algo {
            PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
            PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
        };
        match share_chain.submit_block(block.clone()).await {
            Ok(_) => {
                let _ = self.stats_broadcast.send_miner_block_accepted(pow_algo);
                let res = self
                    .p2p_client
                    .broadcast_block(block.clone())
                    .map_err(|error| Status::internal(error.to_string()));
                if res.is_ok() {
                    info!(target: LOG_TARGET, "Broadcast new block: {:?}", block.hash.to_hex());
                }
                res
            },
            Err(error) => {
                warn!(target: LOG_TARGET, "Failed to add new block: {error:?}");
                let _ = self.stats_broadcast.send_miner_block_rejected(pow_algo);
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
            let grpc_pow_algo: PowAlgos = grpc_block_header_pow
                .pow_algo
                .try_into()
                .map_err(|_| Status::internal("invalid block header pow algo in request"))?;
            let pow_algo = match grpc_pow_algo {
                PowAlgos::Randomx => PowAlgorithm::RandomX,
                PowAlgos::Sha3x => PowAlgorithm::Sha3x,
            };

            // update coinbase extras cache
            // investigate lock usage
            let wallet_payment_address = TariAddress::from_str(grpc_req.wallet_payment_address.as_str())
                .map_err(|error| Status::failed_precondition(format!("Invalid wallet payment address:  {}", error)))?;

            // request new block template with shares as coinbases
            let share_chain = match pow_algo {
                PowAlgorithm::RandomX => self.share_chain_random_x.clone(),
                PowAlgorithm::Sha3x => self.share_chain_sha3x.clone(),
            };
            let coinbase_extra =
                convert_coinbase_extra(self.squad.clone(), grpc_req.coinbase_extra).unwrap_or_default();
            let mut new_tip_block = (*share_chain
                .generate_new_tip_block(&wallet_payment_address, coinbase_extra)
                .await
                .map_err(|error| Status::internal(format!("failed to convert coinbase extra {error:?}")))?)
            .clone();
            let shares = share_chain
                .generate_shares(&new_tip_block)
                .await
                .map_err(|error| Status::internal(format!("failed to convert coinbase extra {error:?}")))?;

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
            let mut tari_block: Block = grpc_block
                .clone()
                .try_into()
                .map_err(|e| Status::internal(format!("Could not convert gprc block to tari block: {}", e)))?;
            // we set the nonce to 0 in order to find the template again.
            tari_block.header.nonce = 0;
            new_tip_block.original_block = tari_block;
            let tari_hash = new_tip_block.original_block.hash();

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
            let target_difficulty = share_chain.get_target_difficulty(height).await;
            new_tip_block.target_difficulty = target_difficulty;
            new_tip_block.fix_hash();

            if let Some(miner_data) = response.miner_data.as_mut() {
                let _ = self
                    .stats_broadcast
                    .send_network_difficulty(pow_algo, Difficulty::from_u64(miner_data.target_difficulty).unwrap());
                // what happens p2pool difficulty > base chain diff
                if target_difficulty.as_u64() < miner_data.target_difficulty {
                    miner_data.target_difficulty = target_difficulty.as_u64();
                }
            }

            let _ = self.stats_broadcast.send_target_difficulty(pow_algo, target_difficulty);

            // save template
            match pow_algo {
                PowAlgorithm::Sha3x => {
                    let mut write_lock = self.list_of_templates_sha3x.write().await;
                    write_lock.push_back(tari_hash.clone());
                    if write_lock.len() > MAX_STORED_TEMPLATES_SHA3X {
                        let _ = write_lock.pop_front();
                    }
                },
                PowAlgorithm::RandomX => {
                    let mut write_lock = self.list_of_templates_rx.write().await;
                    write_lock.push_back(tari_hash.clone());
                    if write_lock.len() > MAX_STORED_TEMPLATES_RX {
                        let _ = write_lock.pop_front();
                    }
                },
            };

            match pow_algo {
                PowAlgorithm::Sha3x => self.template_store_sha3x.write().await.insert(tari_hash, new_tip_block),
                PowAlgorithm::RandomX => self.template_store_rx.write().await.insert(tari_hash, new_tip_block),
            };

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
            //todo dont remove, just peek
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


            p2pool_block.original_block = tari_block;
            let mined_tari_hash = p2pool_block.original_block.hash();

            debug!(target: LOG_TARGET, "Trace - getting block difficulty: {}", timer.elapsed().as_millis());
            // Check block's difficulty compared to the latest network one to increase the probability
            // to get the block accepted (and also a block with lower difficulty than latest one is invalid anyway).
            let request_block_difficulty = match p2pool_block.original_block.header.pow.pow_algo {
                PowAlgorithm::Sha3x => sha3x_difficulty(&p2pool_block.original_block.header)
                    .map_err(|error| Status::internal(error.to_string()))?,
                PowAlgorithm::RandomX => randomx_difficulty(
                    &p2pool_block.original_block.header,
                    self.block_validation_params.random_x_factory(),
                    self.block_validation_params.genesis_block_hash(),
                    self.block_validation_params.consensus_manager(),
                )
                    .map_err(|error| Status::internal(error.to_string()))?,
            };
            info!(
            target: LOG_TARGET,
            "Submitted {} block difficulty: {}",
            p2pool_block.original_block.header.pow.pow_algo, request_block_difficulty
        );

            debug!(target: LOG_TARGET, "Trace - getting network difficulty: {}", timer.elapsed().as_millis());
            let network_difficulty = match p2pool_block.original_block.header.pow.pow_algo {
                PowAlgorithm::Sha3x => self
                    .sha3_block_height_difficulty_cache
                    .read()
                    .await
                    .get(&(p2pool_block.original_block.header.height))
                    .copied()
                    .unwrap_or_else(Difficulty::min),
                PowAlgorithm::RandomX => self
                    .randomx_block_height_difficulty_cache
                    .read()
                    .await
                    .get(&(p2pool_block.original_block.header.height))
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
                match self.client.write().await.submit_block(grpc_request).await {
                    Ok(_resp) => {
                        *max_difficulty = Difficulty::min();
                        let _ = self.stats_broadcast.send_pool_block_accepted(pow_algo);
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
                        let _ = self.stats_broadcast.send_pool_block_rejected(pow_algo);
                        p2pool_block.sent_to_main_chain = false;

                        if timer.elapsed() > MAX_ACCEPTABLE_GRPC_TIMEOUT {
                            warn!(target: LOG_TARGET, "submit_block took {}ms and errored", timer.elapsed().as_millis());
                        }
                        return Err(Status::internal(error.to_string()));
                    },
                }
            }

            debug!(target: LOG_TARGET, "Trace - submitting to share chain: {}", timer.elapsed().as_millis());
            let p2pool_block = Arc::new(p2pool_block);
            // Don't error if we can't submit it.
            match self.submit_share_chain_block(p2pool_block.clone()).await {
                Ok(_) => {
                    let pow_type = p2pool_block.original_block.header.pow.pow_algo.to_string();
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
