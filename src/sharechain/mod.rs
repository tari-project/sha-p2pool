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
    /// Adds a new block if valid to chain.
    async fn submit_block(&self, block: &Block) -> ShareChainResult<()>;

    /// Add multiple blocks at once.
    /// While this operation runs, no other blocks can be added until it's done.
    async fn submit_blocks(&self, blocks: Vec<Block>) -> ShareChainResult<()>;

    /// Returns the tip of height in chain.
    async fn tip_height(&self) -> ShareChainResult<u64>;

    /// Generate shares based on the previous blocks.
    async fn generate_shares(&self, reward: u64) -> Vec<NewBlockCoinbase>;

    /// Return a new block that could be added via `submit_block`.
    async fn new_block(&self, request: &SubmitBlockRequest) -> ShareChainResult<Block>;

    /// Returns blocks from the given height (`from_height`, exclusive).
    async fn blocks(&self, from_height: u64) -> ShareChainResult<Vec<Block>>;

    /// Validates a block.
    async fn validate_block(&self, block: &Block) -> ShareChainResult<bool>;
}