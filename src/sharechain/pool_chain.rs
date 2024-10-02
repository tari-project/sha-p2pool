use std::collections::{HashMap, VecDeque};

use minotari_app_grpc::tari_rpc::NewBlockCoinbase;

use crate::sharechain::{block_level::BlockLevel, pool_block::PoolBlock};

pub(crate) struct PoolChain {
    cached_shares: Option<Vec<NewBlockCoinbase>>,
    levels: VecDeque<BlockLevel>,
    tip_level_index: usize,
    max_levels: usize,
    orphans: HashMap<u64, Vec<PoolBlock>>,
}

impl PoolChain {
    pub fn new(max_levels: usize, max_orphans: usize) -> Self {
        Self {
            cached_shares: None,
            levels: VecDeque::new(),
            tip_level_index: 0,
            max_levels,
            orphans: HashMap::with_capacity(max_orphans),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    pub fn new_level(&mut self, block: PoolBlock, is_tip: bool) {
        let height = block.height;
        self.levels.push_front(BlockLevel::new(vec![block], height));
        if is_tip {
            self.tip_level_index = 0;
        }
        if self.is_full() {
            // We might have a gap in blocks, and if we are removing the tip, then we need to move the tip to the
            // highest next continuous set of blocks
            if self.tip_level_index == self.levels.len().saturating_sub(1) {
                if let Some(next_tip) = self.levels.get(self.tip_level_index - 1) {
                    let mut curr_tip = next_tip.height;
                    let mut curr_index = self.tip_level_index - 1;
                    while let Some(next_tip) = self.levels.get(curr_tip - 1) {
                        if next_tip.height == curr_tip.height.saturating_sub(1) {
                            self.tip_level_index = self.tip_level_index.saturating_sub(1);
                            curr_tip = next_tip;
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

    pub fn is_full(&self) -> bool {
        self.levels.len() >= self.max_levels.saturating_add(1)
    }

    pub fn tip_height(&self) -> u64 {
        self.levels
            .get(self.tip_level_index)
            .map(|level| level.height)
            .unwrap_or(0)
    }

    pub fn get_at_height(&self, height: u64) -> Option<&BlockLevel> {
        let tip = self.levels.front()?.height;
        if height > tip {
            return None;
        }
        let index = tip.checked_sub(height);
        self.levels
            .get(usize::try_from(index?).expect("32 bit systems not supported"))
    }
}
