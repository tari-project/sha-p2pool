use std::collections::HashMap;
use std::sync::Arc;

use minotari_app_grpc::tari_rpc::{GetNewBlockRequest, GetNewBlockResponse, GetNewBlockTemplateWithCoinbasesRequest, NewBlockCoinbase, NewBlockTemplateRequest, PowAlgo};
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

    // TODO: complete implementation to find the right shares
    async fn generate_shares(&self, request: &GetNewBlockRequest, reward: u64) -> Vec<NewBlockCoinbase> {
        let mut result = vec![];
        let mut miners = HashMap::<String, f64>::new(); // target wallet address -> hash rate

        // TODO: remove, only for testing now, get miners from outside of this module using P2P network/sharechain
        miners.insert(request.wallet_payment_address.clone(), 100.0);
        miners.insert("260304a3699f8911c3d949b2eb0394595c8041a36fa13320fa2395b4090ae573a430ac21c5d087ecfcd1922e6ef58cd3f2a1eef2fcbd17e2374a09e0c68036fe6c5f91".to_string(), 100.0);

        // calculate full hash rate and shares
        let full_hash_rate: f64 = miners.values().sum();
        miners.iter()
            .map(|(addr, rate)| (addr, rate / full_hash_rate))
            .filter(|(_, share)| *share > 0.0)
            .for_each(|(addr, share)| {
                let curr_reward = ((reward as f64) * share) as u64;
                // TODO: check if still needed
                // info!("{addr} -> SHARE: {share:?}, REWARD: {curr_reward:?}");
                result.push(NewBlockCoinbase {
                    address: addr.clone(),
                    value: curr_reward,
                    stealth_payment: false,
                    revealed_value_proof: true,
                    coinbase_extra: vec![],
                });
            });

        result
    }
}

#[tonic::async_trait]
impl<S> ShaP2Pool for ShaP2PoolGrpc<S>
    where S: ShareChain + Send + Sync + 'static
{
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
        let shares = self.generate_shares(&template_request, reward).await;
        let share_count = shares.len();
        let response = self.client.lock().await
            .get_new_block_template_with_coinbases(GetNewBlockTemplateWithCoinbasesRequest {
                algo: Some(pow_algo),
                max_weight: 0,
                coinbases: shares,
            }).await?.into_inner();

        // set target difficulty
        let miner_data = response.clone().miner_data.ok_or_else(|| Status::internal("missing miner data"))?;
        // target difficulty is always: `original difficulty` / `number of shares` 
        // let target_difficulty = miner_data.target_difficulty / share_count as u64; // TODO: uncomment this
        let target_difficulty = miner_data.target_difficulty / (share_count as u64 * 10); // TODO: remove this

        Ok(Response::new(GetNewBlockResponse {
            block: Some(response),
            target_difficulty,
        }))
    }
}