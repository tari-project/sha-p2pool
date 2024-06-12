use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tari_common_types::types::BlockHash;
use tari_core::blocks::BlockHeader;
use thiserror::Error;

pub mod in_memory;

#[derive(Serialize, Deserialize, Debug)]
pub struct Block {
    hash: BlockHash,
    prev_hash: BlockHash,
    height: u64,
    original_block_header: BlockHeader,
    miners: Vec<String>,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Internal error: {0}")]
    Internal(String),
}

pub type ShareChainResult<T> = Result<T, Error>;

#[async_trait]
pub trait ShareChain {
    async fn submit_block(&self, block: Block) -> ShareChainResult<()>;

    async fn tip_height(&self) -> ShareChainResult<u64>;
}