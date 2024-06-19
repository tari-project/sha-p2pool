use async_trait::async_trait;
use minotari_app_grpc::tari_rpc::{NewBlockCoinbase, SubmitBlockRequest};

use crate::sharechain::block::Block;
use crate::sharechain::error::Error;

pub mod in_memory;
pub mod block;
pub mod error;

pub type ShareChainResult<T> = Result<T, Error>;

#[async_trait]
pub trait ShareChain {
    async fn submit_block(&self, block: &Block) -> ShareChainResult<()>;

    async fn tip_height(&self) -> ShareChainResult<u64>;

    async fn generate_shares(&self, reward: u64) -> Vec<NewBlockCoinbase>;

    async fn new_block(&self, request: &SubmitBlockRequest) -> ShareChainResult<Block>;
}