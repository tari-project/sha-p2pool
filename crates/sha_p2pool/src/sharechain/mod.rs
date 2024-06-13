use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tari_common_types::types::BlockHash;
use tari_core::blocks;
use tari_core::blocks::BlockHeader;
use thiserror::Error;

pub mod in_memory;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Block {
    hash: BlockHash,
    prev_hash: BlockHash,
    height: u64,
    original_block_header: BlockHeader,
    miners: Vec<String>,
}

// TODO: generate real block from share chain here
impl From<blocks::Block> for Block {
    fn from(tari_block: blocks::Block) -> Self {
        Self {
            hash: Default::default(),
            prev_hash: Default::default(),
            height: tari_block.header.height,
            original_block_header: tari_block.header,
            miners: vec![],
        }
    }
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