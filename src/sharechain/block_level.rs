use minotari_app_grpc::tari_rpc::NewBlockCoinbase;

use crate::sharechain::{
    error::{BlockConvertError, Error},
    pool_block::PoolBlock,
};
/// A collection of blocks with the same height.
pub(crate) struct BlockLevel {
    blocks: Vec<PoolBlock>,
    height: u64,
    in_chain_index: usize,
}

impl BlockLevel {
    pub fn new(blocks: Vec<PoolBlock>, height: u64) -> Self {
        Self {
            blocks,
            height,
            in_chain_index: 0,
        }
    }

    pub fn add_block(&mut self, block: PoolBlock, replace_tip: bool) -> Result<(), Error> {
        if self.height != block.height {
            return Err(Error::InvalidBlock(block));
        }
        if replace_tip {
            self.in_chain_index = self.blocks.len();
        }
        self.blocks.push(block);
        Ok(())
    }

    pub fn block_in_main_chain(&self) -> Option<&PoolBlock> {
        if self.in_chain_index < self.blocks.len() {
            Some(&self.blocks[self.in_chain_index])
        } else {
            None
        }
    }

    pub fn height(&self) -> u64 {
        self.height
    }
}
