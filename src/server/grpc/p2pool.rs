// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;

use log::{debug, info, warn};
use minotari_app_grpc::tari_rpc::{
    base_node_client::BaseNodeClient, pow_algo::PowAlgos, sha_p2_pool_server::ShaP2Pool, GetNewBlockRequest,
    GetNewBlockResponse, GetNewBlockTemplateWithCoinbasesRequest, HeightRequest, NewBlockTemplateRequest, PowAlgo,
    SubmitBlockRequest, SubmitBlockResponse,
};
use tari_core::proof_of_work::sha3x_difficulty;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};

use crate::{
    server::{
        grpc::{error::Error, util},
        p2p,
    },
    sharechain::{block::Block, ShareChain, SHARE_COUNT},
};

const LOG_TARGET: &str = "p2pool::server::grpc::p2pool";

/// P2Pool specific gRPC service to provide `get_new_block` and `submit_block` functionalities.
pub struct ShaP2PoolGrpc<S>
where
    S: ShareChain + Send + Sync + 'static,
{
    /// Base node client
    client: Arc<Mutex<BaseNodeClient<tonic::transport::Channel>>>,
    /// P2P service client
    p2p_client: p2p::ServiceClient,
    /// Current share chain
    share_chain: Arc<S>,
}

impl<S> ShaP2PoolGrpc<S>
where
    S: ShareChain + Send + Sync + 'static,
{
    pub async fn new(
        base_node_address: String,
        p2p_client: p2p::ServiceClient,
        share_chain: Arc<S>,
    ) -> Result<Self, Error> {
        Ok(Self {
            client: Arc::new(Mutex::new(util::connect_base_node(base_node_address).await?)),
            p2p_client,
            share_chain,
        })
    }

    /// Submits a new block to share chain and broadcasts to the p2p network.
    pub async fn submit_share_chain_block(&self, block: &Block) -> Result<(), Status> {
        if let Err(error) = self.share_chain.submit_block(block).await {
            warn!(target: LOG_TARGET, "Failed to add new block: {error:?}");
        }
        debug!(target: LOG_TARGET, "Broadcast new block with height: {:?}", block.height());
        self.p2p_client
            .broadcast_block(block)
            .await
            .map_err(|error| Status::internal(error.to_string()))
    }
}

#[tonic::async_trait]
impl<S> ShaP2Pool for ShaP2PoolGrpc<S>
where
    S: ShareChain + Send + Sync + 'static,
{
    /// Returns a new block (that can be mined) which contains all the shares generated
    /// from the current share chain as coinbase transactions.
    async fn get_new_block(
        &self,
        _request: Request<GetNewBlockRequest>,
    ) -> Result<Response<GetNewBlockResponse>, Status> {
        let mut pow_algo = PowAlgo::default();
        pow_algo.set_pow_algo(PowAlgos::Sha3x);

        // request original block template to get reward
        let req = NewBlockTemplateRequest {
            algo: Some(pow_algo.clone()),
            max_weight: 0,
        };
        let template_response = self.client.lock().await.get_new_block_template(req).await?.into_inner();
        let miner_data = template_response
            .miner_data
            .ok_or_else(|| Status::internal("missing miner data"))?;
        let reward = miner_data.reward;

        // request new block template with shares as coinbases
        let shares = self.share_chain.generate_shares(reward).await;

        let response = self
            .client
            .lock()
            .await
            .get_new_block_template_with_coinbases(GetNewBlockTemplateWithCoinbasesRequest {
                algo: Some(pow_algo),
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
        let target_difficulty = miner_data.target_difficulty / SHARE_COUNT;

        Ok(Response::new(GetNewBlockResponse {
            block: Some(response),
            target_difficulty,
        }))
    }

    /// Validates the submitted block with the p2pool network, checks for difficulty matching
    /// with network (using base node), submits mined block to base node and submits new p2pool block
    /// to p2pool network.
    async fn submit_block(
        &self,
        request: Request<SubmitBlockRequest>,
    ) -> Result<Response<SubmitBlockResponse>, Status> {
        let grpc_block = request.get_ref();
        let grpc_request_payload = grpc_block
            .block
            .clone()
            .ok_or_else(|| Status::internal("missing block in request"))?;
        let mut block = self
            .share_chain
            .new_block(grpc_block)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;

        // validate block with other peers
        let validation_result = self
            .p2p_client
            .validate_block(&block)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        if !validation_result {
            return Err(Status::invalid_argument("invalid block"));
        }

        let origin_block_header = block
            .original_block_header()
            .as_ref()
            .ok_or_else(|| Status::internal("missing original block header"))?;

        // Check block's difficulty compared to the latest network one to increase the probability
        // to get the block accepted (and also a block with lower difficulty than latest one is invalid anyway).
        let request_block_difficulty =
            sha3x_difficulty(origin_block_header).map_err(|error| Status::internal(error.to_string()))?;
        let mut network_difficulty_stream = self
            .client
            .lock()
            .await
            .get_network_difficulty(HeightRequest {
                from_tip: 0,
                start_height: origin_block_header.height - 1,
                end_height: origin_block_header.height,
            })
            .await?
            .into_inner();
        let mut network_difficulty_matches = false;
        while let Ok(Some(diff_resp)) = network_difficulty_stream.message().await {
            if origin_block_header.height == diff_resp.height + 1
                && request_block_difficulty.as_u64() > diff_resp.difficulty
            {
                network_difficulty_matches = true;
            }
        }

        if !network_difficulty_matches {
            block.set_sent_to_main_chain(false);
            self.submit_share_chain_block(&block).await?;
            return Ok(Response::new(SubmitBlockResponse {
                block_hash: block.hash().to_vec(),
            }));
        }

        // submit block to base node
        let (metadata, extensions, _inner) = request.into_parts();
        let grpc_request = Request::from_parts(metadata, extensions, grpc_request_payload);
        match self.client.lock().await.submit_block(grpc_request).await {
            Ok(resp) => {
                info!("💰 New matching block found and sent to network!");
                block.set_sent_to_main_chain(true);
                self.submit_share_chain_block(&block).await?;
                Ok(resp)
            },
            Err(_) => {
                block.set_sent_to_main_chain(false);
                self.submit_share_chain_block(&block).await?;
                Ok(Response::new(SubmitBlockResponse {
                    block_hash: block.hash().to_vec(),
                }))
            },
        }
    }
}
