use std::collections::HashMap;

use async_trait::async_trait;
use minotari_app_grpc::tari_rpc::NewBlockCoinbase;
use rand::random;

use crate::sharechain::{Block, ShareChain, ShareChainResult};

pub struct InMemoryShareChain {}

impl InMemoryShareChain {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl ShareChain for InMemoryShareChain {
    async fn submit_block(&self, block: Block) -> ShareChainResult<()> {
        //TODO: implement
        Ok(())
    }

    async fn tip_height(&self) -> ShareChainResult<u64> {
        //TODO: implement
        Ok(random())
    }

    fn generate_shares(&self, reward: u64) -> Vec<NewBlockCoinbase> {
        let mut result = vec![];
        // TODO: get miners with hashrates from chain
        let mut miners = HashMap::<String, f64>::new(); // target wallet address -> hash rate

        // TODO: remove, only for testing now, get miners from chain
        miners.insert("260396abcc66770f67ca4cdd296cc133e63b88578f3c362d4fa0ff7b05da1bc5a74c78a415009fa49eda8fd8721c20fb4617a833aa630c9790157b6b6f716f0ac72e2e".to_string(), 100.0);
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