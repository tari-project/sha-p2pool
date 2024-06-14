use std::sync::Arc;

use minotari_app_grpc::tari_rpc::{GetNewBlockRequest, GetNewBlockResponse, GetNewBlockTemplateWithCoinbasesRequest, NewBlockTemplateRequest, PowAlgo};
use minotari_app_grpc::tari_rpc::base_node_client::BaseNodeClient;
use minotari_app_grpc::tari_rpc::pow_algo::PowAlgos;
use minotari_app_grpc::tari_rpc::sha_p2_pool_server::ShaP2Pool;
use minotari_node_grpc_client::BaseNodeGrpcClient;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};

use crate::server::grpc::error::Error;
use crate::server::grpc::error::TonicError;
use crate::sharechain::ShareChain;

pub struct ShaP2PoolGrpc<S>
    where S: ShareChain + Send + Sync + 'static
{
    client: Arc<Mutex<BaseNodeClient<tonic::transport::Channel>>>,
    share_chain: Arc<S>,
}

impl<S> ShaP2PoolGrpc<S>
    where S: ShareChain + Send + Sync + 'static
{
    pub async fn new(base_node_address: String, share_chain: Arc<S>) -> Result<Self, Error> {
        // TODO: add retry mechanism to try at least 3 times before failing
        let client = BaseNodeGrpcClient::connect(base_node_address)
            .await
            .map_err(|e| Error::Tonic(TonicError::Transport(e)))?;

        Ok(Self { client: Arc::new(Mutex::new(client)), share_chain })
    }
}

#[tonic::async_trait]
impl<S> ShaP2Pool for ShaP2PoolGrpc<S>
    where S: ShareChain + Send + Sync + 'static
{
    async fn get_new_block(&self, request: Request<GetNewBlockRequest>) -> Result<Response<GetNewBlockResponse>, Status> {
        // TODO: revisit GetNewBlockRequest as we get shares from share chain and not including all the time the requested wallet address
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
        let shares = self.share_chain.generate_shares(reward);
        let share_count = shares.len();

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
}