use async_trait::async_trait;
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
}