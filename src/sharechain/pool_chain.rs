use std::collections::{HashMap, VecDeque};

use digest::block_buffer::Block;
use minotari_app_grpc::tari_rpc::NewBlockCoinbase;

use crate::sharechain::{block_level::BlockLevel, pool_block::PoolBlock};

pub(crate) struct PoolChain {
    cached_shares: Option<Vec<NewBlockCoinbase>>,
    levels: VecDeque<BlockLevel>,
    tip_level_index: usize,
    max_levels: usize,
}

impl PoolChain {
    pub fn new(max_levels: usize) -> Self {
        if max_levels == 0 {
            panic!("PoolChain must have at least one level");
        }
        Self {
            cached_shares: None,
            levels: VecDeque::new(),
            tip_level_index: 0,
            max_levels,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    pub fn add_future_block(&mut self, block: PoolBlock) {
        let height = block.height;

        let mut index = 0;
        // for level in self.levels.iter_mut() {
        //     if level.height() == height {
        //         level.add_block(block);
        //         return;
        //     }
        //     if level.height() < height {
        //         break;
        //     }
        //     index += 1;
        // }

        todo!()
        // self.levels.insert(index, value);
    }

    fn drop_last_if_full(&mut self) {
        if self.is_full() {
            // We might have a gap in blocks, and if we are removing the tip, then we need to move the tip to the
            // highest next continuous set of blocks
            if self.tip_level_index == self.levels.len().saturating_sub(1) {
                if let Some(next_tip) = self.levels.get(self.tip_level_index - 1) {
                    let mut curr_tip = next_tip.height();
                    let mut curr_index = self.tip_level_index - 1;
                    while let Some(next_tip) = self.levels.get(curr_index - 1) {
                        if next_tip.height() == curr_tip.saturating_sub(1) {
                            self.tip_level_index = self.tip_level_index.saturating_sub(1);
                            curr_tip = next_tip.height();
                        } else {
                            break;
                        }
                    }
                }
                self.tip_level_index = self.tip_level_index.saturating_sub(1);
            }
            // Finally remove the block
            let removed_level = self.levels.pop_back();
        }
    }

    pub fn insert_level(&mut self, index: usize, block: PoolBlock) {
        todo!()
        // self.levels.insert(index, BlockLevel::new(vec![block], block.height));
        // self.drop_last_if_full();
    }

    pub fn new_level(&mut self, block: PoolBlock, is_tip: bool) {
        let height = block.height;
        self.levels.push_front(BlockLevel::new(vec![block], height));
        if is_tip {
            self.tip_level_index = 0;
        }
        self.drop_last_if_full();
    }

    pub fn is_full(&self) -> bool {
        self.levels.len() >= self.max_levels.saturating_add(1)
    }

    pub fn tip_height(&self) -> u64 {
        self.levels
            .get(self.tip_level_index)
            .map(|level| level.height())
            .unwrap_or(0)
    }

    pub fn get_at_height(&self, height: u64) -> Option<&BlockLevel> {
        let tip = self.levels.front()?.height();
        if height > tip {
            return None;
        }
        let index = tip.checked_sub(height);
        self.levels
            .get(usize::try_from(index?).expect("32 bit systems not supported"))
    }
}

mod test {

    use super::*;
    use crate::sharechain::pool_block::PoolBlockBuilder;

    #[test]
    fn add_block() {
        let mut chain = PoolChain::new(1);
        let block = PoolBlockBuilder::new().with_height(1).build();
        chain.new_level(block, true);
        let block2 = PoolBlockBuilder::new().with_height(2).build();

        chain.new_level(block2, true);
    }
}
