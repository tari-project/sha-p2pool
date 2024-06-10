use async_trait::async_trait;
use tari_core::blocks::BlockHeader;
use thiserror::Error;

mod grpc;
mod in_memory;

pub struct Block {
    original_header: BlockHeader,

}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Internal error: {0}")]
    Internal(String),
}

pub type ShareChainError<T> = Result<T, Error>;

#[async_trait]
pub trait ShareChain {
    async fn submit_block(&self, block: Block) -> ShareChainError<()>;

    async fn tip_height(&self) -> ShareChainError<u64>;
}