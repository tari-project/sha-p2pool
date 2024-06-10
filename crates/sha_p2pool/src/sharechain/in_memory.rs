use async_trait::async_trait;

use crate::sharechain::{Block, ShareChain, ShareChainError};

pub struct InMemoryShareChain {}

#[async_trait]
impl ShareChain for InMemoryShareChain {
    async fn submit_block(&self, block: Block) -> ShareChainError<()> {
        todo!()
    }

    async fn tip_height(&self) -> ShareChainError<u64> {
        todo!()
    }
}