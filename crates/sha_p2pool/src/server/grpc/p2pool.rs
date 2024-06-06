use std::collections::HashMap;
use std::sync::Arc;

use log::info;
use minotari_app_grpc::tari_rpc::{GetNewBlockRequest, GetNewBlockResponse, GetNewBlockTemplateWithCoinbasesRequest, NewBlockCoinbase, NewBlockTemplateRequest, PowAlgo};
use minotari_app_grpc::tari_rpc::base_node_client::BaseNodeClient;
use minotari_app_grpc::tari_rpc::pow_algo::PowAlgos;
use minotari_app_grpc::tari_rpc::sha_p2_pool_server::ShaP2Pool;
use minotari_node_grpc_client::BaseNodeGrpcClient;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};

use crate::server::grpc::error::Error;
use crate::server::grpc::error::TonicError;

pub struct ShaP2PoolGrpc {
    client: Arc<Mutex<BaseNodeClient<tonic::transport::Channel>>>,
}

impl ShaP2PoolGrpc {
    pub async fn new(base_node_address: String) -> Result<Self, Error> {
        // TODO: add retry mechanism to try at least 3 times before failing
        let client = BaseNodeGrpcClient::connect(base_node_address)
            .await
            .map_err(|e| Error::Tonic(TonicError::Transport(e)))?;

        Ok(Self { client: Arc::new(Mutex::new(client)) })
    }

    // TODO: complete implementation to find the right shares
    async fn generate_shares(&self, request: &GetNewBlockRequest, reward: u64) -> Vec<NewBlockCoinbase> {
        let mut miners = HashMap::<String, u64>::new(); // target wallet address -> hash rate

        // TODO: remove, only for testing now, get miners from outside of this module using P2P network/sharechain
        miners.insert(request.wallet_payment_address.clone(), 100);
        miners.insert("6ee38cf177a8fbf818d93ba5bbca6078efd88cef5c57927ce65dd0716ca3ee655a".to_string(), 100);

        // calculate full hash rate and shares
        let full_hash_rate: u64 = miners.iter()
            .map(|(_, rate)| rate)
            .sum();
        miners.iter()
            .map(|(addr, rate)| (addr, rate / full_hash_rate))
            .for_each(|(addr, share)| {
                info!("{addr} -> {share:?}");
            });

        todo!()
    }
}

#[tonic::async_trait]
impl ShaP2Pool for ShaP2PoolGrpc {
    async fn get_new_block(&self, request: Request<GetNewBlockRequest>) -> Result<Response<GetNewBlockResponse>, Status> {
        let template_request = request.into_inner();
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
        let mut new_block_template_req = GetNewBlockTemplateWithCoinbasesRequest::default();
        new_block_template_req.algo = Some(pow_algo);
        new_block_template_req.coinbases = self.generate_shares(&template_request, reward).await;
        let response = self.client.lock().await
            .get_new_block_template_with_coinbases(new_block_template_req).await?.into_inner();
        let miner_data = response.clone().miner_data.ok_or_else(|| Status::internal("missing miner data"))?;
        let target_difficulty = miner_data.target_difficulty;

        Ok(Response::new(GetNewBlockResponse {
            block: Some(response),
            target_difficulty,
        }))
    }
}