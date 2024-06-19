use std::sync::Arc;

use log::info;
use minotari_app_grpc::tari_rpc::{GetNewBlockRequest, GetNewBlockResponse, GetNewBlockTemplateWithCoinbasesRequest, HeightRequest, NewBlockTemplateRequest, PowAlgo, SubmitBlockRequest, SubmitBlockResponse};
use minotari_app_grpc::tari_rpc::base_node_client::BaseNodeClient;
use minotari_app_grpc::tari_rpc::pow_algo::PowAlgos;
use minotari_app_grpc::tari_rpc::sha_p2_pool_server::ShaP2Pool;
use minotari_node_grpc_client::BaseNodeGrpcClient;
use tari_core::proof_of_work::sha3x_difficulty;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};

use crate::server::grpc::error::Error;
use crate::server::grpc::error::TonicError;
use crate::server::p2p;
use crate::sharechain::ShareChain;

const MIN_SHARE_COUNT: usize = 10;

pub struct ShaP2PoolGrpc<S>
    where S: ShareChain + Send + Sync + 'static
{
    client: Arc<Mutex<BaseNodeClient<tonic::transport::Channel>>>,
    p2p_client: p2p::ServiceClient,
    share_chain: Arc<S>,
}

impl<S> ShaP2PoolGrpc<S>
    where S: ShareChain + Send + Sync + 'static
{
    pub async fn new(base_node_address: String, p2p_client: p2p::ServiceClient, share_chain: Arc<S>) -> Result<Self, Error> {
        // TODO: add retry mechanism to try at least 3 times before failing
        let client = BaseNodeGrpcClient::connect(base_node_address)
            .await
            .map_err(|e| Error::Tonic(TonicError::Transport(e)))?;

        Ok(Self { client: Arc::new(Mutex::new(client)), p2p_client, share_chain })
    }
}

#[tonic::async_trait]
impl<S> ShaP2Pool for ShaP2PoolGrpc<S>
    where S: ShareChain + Send + Sync + 'static
{
    async fn get_new_block(&self, _request: Request<GetNewBlockRequest>) -> Result<Response<GetNewBlockResponse>, Status> {
        let mut pow_algo = PowAlgo::default();
        pow_algo.set_pow_algo(PowAlgos::Sha3x);

        // request original block template to get reward
        let req = NewBlockTemplateRequest {
            algo: Some(pow_algo.clone()),
            max_weight: 0,
        };
        let template_response = self.client.lock().await
            .get_new_block_template(req)
            .await?
            .into_inner();
        let miner_data = template_response.miner_data.ok_or_else(|| Status::internal("missing miner data"))?;
        let reward = miner_data.reward;

        // request new block template with shares as coinbases
        let shares = self.share_chain.generate_shares(reward).await;
        let share_count = if shares.len() < MIN_SHARE_COUNT {
            MIN_SHARE_COUNT
        } else {
            shares.len()
        };

        let response = self.client.lock().await
            .get_new_block_template_with_coinbases(GetNewBlockTemplateWithCoinbasesRequest {
                algo: Some(pow_algo),
                max_weight: 0,
                coinbases: shares,
            }).await?.into_inner();

        // set target difficulty
        let miner_data = response.clone().miner_data.ok_or_else(|| Status::internal("missing miner data"))?;
        let target_difficulty = miner_data.target_difficulty / share_count as u64;

        Ok(Response::new(GetNewBlockResponse {
            block: Some(response),
            target_difficulty,
        }))
    }

    async fn submit_block(&self, request: Request<SubmitBlockRequest>) -> Result<Response<SubmitBlockResponse>, Status> {
        let grpc_block = request.get_ref();
        let grpc_request_payload = grpc_block.block.clone()
            .ok_or_else(|| Status::internal("missing block in request"))?;
        let mut block = self.share_chain.new_block(grpc_block).await.map_err(|error| Status::internal(error.to_string()))?;

        // validate block with other peers
        let validation_result = self.p2p_client.validate_block(&block).await
            .map_err(|error| Status::internal(error.to_string()))?;
        if !validation_result {
            return Err(Status::invalid_argument("invalid block"));
        }

        let origin_block_header = block.original_block_header().as_ref()
            .ok_or_else(|| { Status::internal("missing original block header") })?;

        // Check block's difficulty compared to the latest network one to increase the probability
        // to get the block accepted (and also a block with lower difficulty than latest one is invalid anyway).
        let request_block_difficulty = sha3x_difficulty(&origin_block_header)
            .map_err(|error| { Status::internal(error.to_string()) })?;
        let mut network_difficulty_stream = self.client.lock().await.get_network_difficulty(HeightRequest {
            from_tip: 0,
            start_height: origin_block_header.height - 1,
            end_height: origin_block_header.height,
        }).await?.into_inner();
        let mut network_difficulty_matches = false;
        while let Ok(Some(diff_resp)) = network_difficulty_stream.message().await {
            if origin_block_header.height == diff_resp.height + 1
                && request_block_difficulty.as_u64() > diff_resp.difficulty {
                network_difficulty_matches = true;
            }
        }

        if !network_difficulty_matches {
            // TODO: simply append new block if valid to sharechain showing that it is not accepted by base node
            // TODO: but still need to present on sharechain
            block.set_sent_to_main_chain(false);
            self.share_chain.submit_block(&block).await
                .map_err(|error| Status::internal(error.to_string()))?;
            info!("Broadcast block with height: {:?}", block.height());
            self.p2p_client.broadcast_block(&block).await
                .map_err(|error| Status::internal(error.to_string()))?;

            return Ok(Response::new(SubmitBlockResponse {
                block_hash: block.hash().to_vec(),
            }));
        }

        // submit block to base node
        let grpc_request = Request::new(grpc_request_payload);
        match self.client.lock().await.submit_block(grpc_request).await {
            Ok(resp) => {
                info!("Block found and sent successfully! (rewards will be paid out)");

                // TODO: append new block if valid to sharechain with a flag or something that shows
                // TODO: that this block is accepted, so paid out
                block.set_sent_to_main_chain(true);
                self.share_chain.submit_block(&block).await
                    .map_err(|error| Status::internal(error.to_string()))?;
                info!("Broadcast block with height: {:?}", block.height());
                self.p2p_client.broadcast_block(&block).await
                    .map_err(|error| Status::internal(error.to_string()))?;

                Ok(resp)
            }
            Err(_) => {
                info!("submit_block stop - block send failure");
                // TODO: simply append new block if valid to sharechain showing that it is not accepted by base node
                // TODO: but still need to present on sharechain
                block.set_sent_to_main_chain(false);
                self.share_chain.submit_block(&block).await
                    .map_err(|error| Status::internal(error.to_string()))?;
                info!("Broadcast block with height: {:?}", block.height());
                self.p2p_client.broadcast_block(&block).await
                    .map_err(|error| Status::internal(error.to_string()))?;

                Ok(Response::new(SubmitBlockResponse {
                    block_hash: block.hash().to_vec(),
                }))
            }
        }
    }
}