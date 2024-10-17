// Copyright 2024. The Tari Project
//
// Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
// following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
// disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
// following disclaimer in the documentation and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
// products derived from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
// INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
// WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
// USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use tari_core::proof_of_work::{lwma_diff::LinearWeightedMovingAverage, AccumulatedDifficulty, Difficulty};

use crate::sharechain::{
    error::Error,
    p2block::P2Block,
    p2chain_level::P2ChainLevel,
    BLOCK_TARGET_TIME,
    DIFFICULTY_ADJUSTMENT_WINDOW,
};
// const LOG_TARGET: &str = "tari::p2pool::sharechain::chain";
pub const SAFETY_MARGIN: usize = 20;

pub struct P2Chain {
    pub cached_shares: Option<HashMap<String, (u64, Vec<u8>)>>,
    levels: VecDeque<P2ChainLevel>,
    total_size: usize,
    share_window: usize,
    total_accumulated_tip_difficulty: AccumulatedDifficulty,
    current_tip: u64,
    pub lwma: LinearWeightedMovingAverage,
}

impl P2Chain {
    pub fn get_at_height(&self, height: u64) -> Option<&P2ChainLevel> {
        let tip = self.levels.front()?.height;
        if height > tip {
            return None;
        }
        let index = tip.checked_sub(height);
        self.levels
            .get(usize::try_from(index?).expect("32 bit systems not supported"))
    }

    pub fn get_mut_at_height(&mut self, height: u64) -> Option<&mut P2ChainLevel> {
        let tip = self.levels.front()?.height;
        if height > tip {
            return None;
        }
        let index = tip.checked_sub(height);
        self.levels
            .get_mut(usize::try_from(index?).expect("32 bit systems not supported"))
    }

    fn decrease_total_chain_difficulty(&mut self, difficulty: Difficulty) -> Result<(), Error> {
        self.total_accumulated_tip_difficulty = self
            .total_accumulated_tip_difficulty
            .checked_sub_difficulty(difficulty)
            .ok_or(Error::DifficultyOverflow)?;
        Ok(())
    }

    pub fn new_empty(total_size: usize, share_window: usize) -> Self {
        let levels = VecDeque::with_capacity(total_size + 1);
        let lwma = LinearWeightedMovingAverage::new(DIFFICULTY_ADJUSTMENT_WINDOW, BLOCK_TARGET_TIME)
            .expect("Failed to create LWMA");
        Self {
            cached_shares: None,
            levels,
            total_size,
            share_window,
            total_accumulated_tip_difficulty: AccumulatedDifficulty::min(),
            current_tip: 0,
            lwma,
        }
    }

    pub fn is_full(&self) -> bool {
        self.levels.len() >= self.total_size + SAFETY_MARGIN
    }

    pub fn add_new_level_to_tip(&mut self, block_level: P2ChainLevel) -> Result<(), Error> {
        if self.is_full() {
            self.levels.pop_back().ok_or(Error::BlockLevelNotFound)?;
        }

        // index 0 in the deque might not be the tip so lets ensee if we need to offset our index calculations
        let highest_height = match self.levels.front() {
            Some(level) => level.height,
            None => 0,
        };

        let index =
            usize::try_from(self.current_tip.saturating_sub(highest_height)).expect("32 bit systems not supported");
        if self.levels.len() >= (self.share_window + index) {
            // 0..share_window is the actively counted pow part
            // last level currently in the window sits at index share_window
            // We need to remove its pow before addng thew new tip
            let level = self.levels[self.share_window + index - 1].clone();
            for (hash, block) in level.blocks {
                if hash == level.chain_block {
                    // its the main block, so remove its difficulty
                    self.decrease_total_chain_difficulty(block.target_difficulty)?;
                    for (height, block_hash) in &block.uncles {
                        let link_level = self.get_at_height(*height).ok_or(Error::BlockLevelNotFound)?;
                        let uncle_block = link_level.blocks.get(&block_hash).ok_or(Error::BlockNotFound)?;
                        self.decrease_total_chain_difficulty(uncle_block.target_difficulty)?;
                    }
                }
            }
        }
        self.current_tip = block_level.height;

        self.levels.push_front(block_level);

        self.cached_shares = None;
        Ok(())
    }

    pub fn add_block_to_chain(&mut self, block: Arc<P2Block>) -> Result<(), Error> {
        // edge case if its the first block
        if self.get_tip().is_none() {
            self.lwma
                .add_back(block.original_block.header.timestamp, block.target_difficulty);
            self.total_accumulated_tip_difficulty =
                AccumulatedDifficulty::from_u128(block.target_difficulty.as_u64() as u128)
                    .expect("Difficulty will always fit into accumulated difficulty");
            let new_level = P2ChainLevel::new(block);
            self.add_new_level_to_tip(new_level)?;
            return Ok(());
        }
        // now we know the tip exist and we can unwrap it safely
        let tip = self.get_tip().unwrap();

        // easy it builds on the tip
        if tip.chain_block == block.prev_hash {
            self.lwma
                .add_back(block.original_block.header.timestamp, block.target_difficulty);
            self.total_accumulated_tip_difficulty = self
                .total_accumulated_tip_difficulty
                .checked_add_difficulty(block.target_difficulty)
                .ok_or_else(|| Error::DifficultyOverflow)?;
            // lets add the uncles this block contains.
            for uncle in block.uncles.iter() {
                let uncle_level = self.get_at_height(uncle.0).ok_or_else(|| Error::UncleBlockNotFound)?;
                let uncle_block = uncle_level
                    .blocks
                    .get(&uncle.1)
                    .ok_or_else(|| Error::UncleBlockNotFound)?;
                let uncle_target_difficulty = uncle_block.target_difficulty;
                // drop the references so we can mutable borrow self
                let _ = uncle_block;
                let _ = uncle_level;
                self.total_accumulated_tip_difficulty = self
                    .total_accumulated_tip_difficulty
                    .checked_add_difficulty(uncle_target_difficulty)
                    .ok_or_else(|| Error::DifficultyOverflow)?;
            }
            let new_level = P2ChainLevel::new(block);
            self.add_new_level_to_tip(new_level)?;
            return Ok(());
        }

        // ok so it's not build on tip, so lets see where what total accumulated difficulty for this chain is:
        let mut total_work = AccumulatedDifficulty::from_u128(block.target_difficulty.as_u64() as u128)
            .expect("Difficulty will always fit into accumulated difficulty");
        for uncle in block.uncles.iter() {
            let uncle_block = self
                .get_at_height(uncle.0)
                .ok_or_else(|| Error::UncleBlockNotFound)?
                .blocks
                .get(&uncle.1)
                .ok_or_else(|| Error::UncleBlockNotFound)?;
            total_work = total_work
                .checked_add_difficulty(uncle_block.target_difficulty)
                .ok_or_else(|| Error::DifficultyOverflow)?;
        }
        let mut current_counting_block = &block;
        let mut counter = 1;

        while let Some(parent) = self.get_parent_block(current_counting_block) {
            total_work = total_work
                .checked_add_difficulty(parent.target_difficulty)
                .ok_or_else(|| Error::DifficultyOverflow)?;
            current_counting_block = parent;
            for uncle in parent.uncles.iter() {
                let uncle_block = self
                    .get_at_height(uncle.0)
                    .ok_or_else(|| Error::UncleBlockNotFound)?
                    .blocks
                    .get(&uncle.1)
                    .ok_or_else(|| Error::UncleBlockNotFound)?;
                total_work = total_work
                    .checked_add_difficulty(uncle_block.target_difficulty)
                    .ok_or_else(|| Error::DifficultyOverflow)?;
            }
            counter += 1;
            if counter >= self.share_window {
                break;
            }
        }

        if total_work > self.total_accumulated_tip_difficulty {
            // we need to reorg the chain
            // lets start by resetting the lwma
            self.lwma = LinearWeightedMovingAverage::new(DIFFICULTY_ADJUSTMENT_WINDOW, BLOCK_TARGET_TIME)
                .expect("Failed to create LWMA");
            self.lwma
                .add_front(block.original_block.header.timestamp, block.target_difficulty);
            let chain_height = self.get_mut_at_height(block.height);
            match chain_height {
                Some(height) => {
                    height.add_block(block.clone(), true)?;
                    self.cached_shares = None;
                },
                None => {
                    let new_level = P2ChainLevel::new(block.clone());
                    self.add_new_level_to_tip(new_level)?;
                },
            }

            let mut current_block = block;
            while self.get_at_height(current_block.height.saturating_sub(1)).is_some() {
                let parent_level = (self.get_at_height(current_block.height.saturating_sub(1)).unwrap()).clone();
                if current_block.prev_hash != parent_level.chain_block {
                    // safety check
                    let nextblock = parent_level.blocks.get(&current_block.prev_hash);
                    if nextblock.is_none() {
                        return Err(Error::BlockNotFound);
                    }
                    // fix the main chain
                    let mut_parent_level = self.get_mut_at_height(self.current_tip.saturating_sub(1)).unwrap();
                    mut_parent_level.chain_block = current_block.prev_hash.clone();
                    current_block = nextblock.unwrap().clone();
                    self.lwma.add_front(
                        current_block.original_block.header.timestamp,
                        current_block.target_difficulty,
                    );
                } else {
                    if !self.lwma.is_full() {
                        // we still need more blocks to fill up the lwma
                        let nextblock = parent_level.blocks.get(&current_block.prev_hash);
                        if nextblock.is_none() {
                            return Err(Error::BlockNotFound);
                        }

                        current_block = nextblock.unwrap().clone();

                        self.lwma.add_front(
                            current_block.original_block.header.timestamp,
                            current_block.target_difficulty,
                        );
                    }
                }
                if current_block.height == 0 {
                    // edge case if there is less than the lwa size or share window in chain
                    break;
                }
            }
            self.total_accumulated_tip_difficulty = total_work;
        } else {
            // just an orphan reorg block,
            match self.get_mut_at_height(block.height) {
                Some(level) => level.add_block(block, false)?,
                None => {
                    // So things got a bit more complicated, we dont have this level
                    // let first assume the new orphan block is higher
                    while self.levels.front().expect("we already checked its not empty").height < block.height - 1 {
                        if self.is_full() {
                            self.levels.pop_back().ok_or(Error::BlockLevelNotFound)?;
                        }
                        let level = P2ChainLevel::new_empty(
                            self.levels.front().expect("we already checked its not empty").height + 1,
                        );
                        self.levels.push_front(level);
                    }
                    if self.levels.front().expect("we already checked its not empty").height < block.height {
                        if self.is_full() {
                            self.levels.pop_back().ok_or(Error::BlockLevelNotFound)?;
                        }
                        let level = P2ChainLevel::new(block);
                        self.levels.push_front(level);
                        return Ok(());
                    }
                    // if its not at the front, it might be at the back
                    // if its full we can exit as there is no chance of it being at the bottom end with a whole chain in
                    // front of it.
                    if !self.is_full() {
                        while self.levels.back().expect("we already checked its not empty").height > block.height + 1 {
                            if self.is_full() {
                                return Ok(());
                            }
                            let level = P2ChainLevel::new_empty(
                                self.levels
                                    .back()
                                    .expect("we already checked its not empty")
                                    .height
                                    .saturating_sub(1),
                            );
                            self.levels.push_back(level);
                        }
                        if self.levels.back().expect("we already checked its not empty").height > block.height {
                            if self.is_full() {
                                return Ok(());
                            }
                            let level = P2ChainLevel::new(block);
                            self.levels.push_back(level);
                        }
                    }
                },
            }
        }

        Ok(())
    }

    pub fn get_parent_block(&self, block: &P2Block) -> Option<&Arc<P2Block>> {
        let parent_height = match block.height.checked_sub(1) {
            Some(height) => height,
            None => return None,
        };
        let parent_level = match self.get_at_height(parent_height) {
            Some(level) => level,
            None => return None,
        };
        parent_level.blocks.get(&block.prev_hash)
    }

    pub fn get_tip(&self) -> Option<&P2ChainLevel> {
        self.get_at_height(self.current_tip)
    }

    pub fn get_height(&self) -> u64 {
        self.get_tip().map(|tip| tip.height).unwrap_or(0)
    }

    pub fn get_max_level_height(&self) -> u64 {
        self.levels.front().map(|level| level.height).unwrap_or(0)
    }

    pub fn get_max_chain_length(&self) -> usize {
        self.levels.len()
    }
}

#[cfg(test)]
mod test {
    use tari_common_types::types::BlockHash;
    use tari_core::{
        blocks::{Block, BlockHeader},
        proof_of_work::Difficulty,
        transactions::aggregated_body::AggregateBody,
    };
    use tari_utilities::epoch_time::EpochTime;

    use super::*;
    use crate::sharechain::in_memory::test::new_random_address;

    #[test]
    fn test_only_keeps_size() {
        let mut chain = P2Chain::new_empty(10, 5);
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..30 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2Block::builder()
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_tari_block(tari_block.clone())
                .build();

            chain.add_block_to_chain(block.clone()).unwrap();
        }

        for i in 0..30 {
            let level = chain.get_at_height(i).unwrap();
            assert_eq!(level.block_in_main_chain().unwrap().original_block.header.nonce, i);
        }

        let address = new_random_address();
        tari_block.header.nonce = 30;
        let block = P2Block::builder()
            .with_timestamp(EpochTime::now())
            .with_height(30)
            .with_tari_block(tari_block.clone())
            .with_miner_wallet_address(address.clone())
            .build();

        chain.add_block_to_chain(block.clone()).unwrap();

        let level = chain.get_at_height(10).unwrap();
        assert_eq!(level.block_in_main_chain().unwrap().original_block.header.nonce, 10);

        assert!(chain.get_at_height(0).is_none());
    }

    #[test]
    fn get_tips() {
        let mut chain = P2Chain::new_empty(10, 5);

        let mut prev_hash = BlockHash::zero();
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..30 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2Block::builder()
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .with_miner_wallet_address(address.clone())
                .with_prev_hash(prev_hash)
                .build();
            prev_hash = block.generate_hash();
            chain.add_block_to_chain(block.clone()).unwrap();

            let level = chain.get_tip().unwrap();
            assert_eq!(level.block_in_main_chain().unwrap().original_block.header.nonce, i);
        }
    }

    #[test]
    fn get_parent() {
        let mut chain = P2Chain::new_empty(10, 5);

        let mut prev_hash = BlockHash::zero();
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..31 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2Block::builder()
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_prev_hash(prev_hash)
                .with_tari_block(tari_block.clone())
                .build();

            prev_hash = block.generate_hash();
            chain.add_block_to_chain(block.clone()).unwrap();
        }

        for i in 2..31 {
            let level = chain.get_at_height(i).unwrap();
            let block = level.block_in_main_chain().unwrap();
            let parent = chain.get_parent_block(&block).unwrap();
            assert_eq!(parent.original_block.header.nonce, i - 1);
        }

        let level = chain.get_at_height(1).unwrap();
        let block = level.block_in_main_chain().unwrap();
        assert!(chain.get_parent_block(&block).is_none());
    }

    #[test]
    fn add_blocks_to_chain_happy_path() {
        let mut chain = P2Chain::new_empty(10, 5);

        let mut timestamp = EpochTime::now();
        let mut prev_hash = BlockHash::zero();

        for i in 1..32 {
            let address = new_random_address();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let block = P2Block::builder()
                .with_timestamp(timestamp)
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(i as u64).unwrap())
                .with_prev_hash(prev_hash)
                .build();

            prev_hash = block.generate_hash();

            chain.add_block_to_chain(block).unwrap();

            let level = chain.get_tip().unwrap();
            assert_eq!(
                level.block_in_main_chain().unwrap().target_difficulty,
                Difficulty::from_u64(i as u64).unwrap()
            );
        }
    }

    #[test]
    fn add_blocks_to_chain_small_reorg() {
        let mut chain = P2Chain::new_empty(10, 5);

        let mut timestamp = EpochTime::now();
        let mut prev_hash = BlockHash::zero();

        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 1..32 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let block = P2Block::builder()
                .with_timestamp(timestamp)
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(i as u64).unwrap())
                .with_tari_block(tari_block.clone())
                .with_prev_hash(prev_hash)
                .build();

            prev_hash = block.generate_hash();
            chain.add_block_to_chain(block).unwrap();
        }
        let level = chain.get_tip().unwrap();
        let tip_hash = level.block_in_main_chain().unwrap().generate_hash();
        assert_eq!(
            level.block_in_main_chain().unwrap().target_difficulty,
            Difficulty::from_u64(31).unwrap()
        );
        assert_eq!(level.block_in_main_chain().unwrap().original_block.header.nonce, 31);
        assert_eq!(level.block_in_main_chain().unwrap().height, 31);
        assert_eq!(
            chain.total_accumulated_tip_difficulty,
            AccumulatedDifficulty::from_u128(145).unwrap() // 31+30+29+28+27
        );

        let block_29 = chain.get_at_height(29).unwrap().block_in_main_chain().unwrap();
        prev_hash = block_29.generate_hash();
        timestamp = block_29.timestamp;

        // for i in 10..12 {
        let address = new_random_address();
        timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
        tari_block.header.nonce = 30 * 2;
        let block = P2Block::builder()
            .with_timestamp(timestamp)
            .with_height(30)
            .with_miner_wallet_address(address.clone())
            .with_target_difficulty(Difficulty::from_u64(30).unwrap())
            .with_tari_block(tari_block.clone())
            .with_prev_hash(prev_hash)
            .build();

        prev_hash = block.generate_hash();

        chain.add_block_to_chain(block).unwrap();
        let level = chain.get_tip().unwrap();
        // still the old tip
        assert_eq!(tip_hash, level.block_in_main_chain().unwrap().generate_hash());

        let address = new_random_address();

        tari_block.header.nonce = 31 * 2;
        timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
        let block = P2Block::builder()
            .with_timestamp(timestamp)
            .with_height(31)
            .with_miner_wallet_address(address.clone())
            .with_target_difficulty(Difficulty::from_u64(32).unwrap())
            .with_tari_block(tari_block.clone())
            .with_prev_hash(prev_hash)
            .build();

        chain.add_block_to_chain(block).unwrap();
        let level = chain.get_tip().unwrap();
        // now it should be the new tip
        assert_ne!(tip_hash, level.block_in_main_chain().unwrap().generate_hash());
        assert_eq!(
            level.block_in_main_chain().unwrap().target_difficulty,
            Difficulty::from_u64(32).unwrap()
        );
        assert_eq!(level.block_in_main_chain().unwrap().original_block.header.nonce, 31 * 2);
        assert_eq!(level.block_in_main_chain().unwrap().height, 31);
        assert_eq!(
            chain.total_accumulated_tip_difficulty,
            AccumulatedDifficulty::from_u128(146).unwrap() // 32+30+29+28+27
        );
    }

    #[test]
    fn calculate_total_difficulty_correctly() {
        let mut chain = P2Chain::new_empty(10, 5);

        let mut timestamp = EpochTime::now();
        let mut prev_hash = BlockHash::zero();

        for i in 1..15 {
            let address = new_random_address();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let block = P2Block::builder()
                .with_timestamp(timestamp)
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .with_prev_hash(prev_hash)
                .build();

            prev_hash = block.generate_hash();

            chain.add_block_to_chain(block).unwrap();
        }
        assert_eq!(
            chain.total_accumulated_tip_difficulty,
            AccumulatedDifficulty::from_u128(50).unwrap()
        );
    }

    #[test]
    fn calculate_total_difficulty_correctly_with_uncles() {
        let mut chain = P2Chain::new_empty(10, 5);

        let mut timestamp = EpochTime::now();
        let mut prev_hash = BlockHash::zero();

        for i in 0..10 {
            let address = new_random_address();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let mut uncles = Vec::new();
            if i > 1 {
                let prev_hash_uncle = chain.get_at_height(i - 2).unwrap().chain_block;
                // lets create an uncle block
                let block = P2Block::builder()
                    .with_timestamp(timestamp)
                    .with_height(i - 1)
                    .with_miner_wallet_address(address.clone())
                    .with_target_difficulty(Difficulty::from_u64(9).unwrap())
                    .with_prev_hash(prev_hash_uncle)
                    .build();
                uncles.push((i - 1, block.hash));
                chain.add_block_to_chain(block).unwrap();
            }
            let block = P2Block::builder()
                .with_timestamp(timestamp)
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .with_uncles(uncles)
                .with_prev_hash(prev_hash)
                .build();

            prev_hash = block.generate_hash();

            chain.add_block_to_chain(block).unwrap();
        }
        let level = chain.get_tip().unwrap();
        assert_eq!(
            level.block_in_main_chain().unwrap().target_difficulty,
            Difficulty::from_u64(10).unwrap()
        );
        assert_eq!(level.block_in_main_chain().unwrap().height, 9);
        assert_eq!(
            chain.total_accumulated_tip_difficulty,
            AccumulatedDifficulty::from_u128(95).unwrap() //(10+9)*5
        );
    }

    #[test]
    fn reorg_with_uncles() {
        let mut chain = P2Chain::new_empty(10, 5);

        let mut timestamp = EpochTime::now();
        let mut prev_hash = BlockHash::zero();

        for i in 0..10 {
            let address = new_random_address();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let mut uncles = Vec::new();
            if i > 1 {
                let prev_hash_uncle = chain.get_at_height(i - 2).unwrap().chain_block;
                // lets create an uncle block
                let block = P2Block::builder()
                    .with_timestamp(timestamp)
                    .with_height(i - 1)
                    .with_miner_wallet_address(address.clone())
                    .with_target_difficulty(Difficulty::from_u64(9).unwrap())
                    .with_prev_hash(prev_hash_uncle)
                    .build();
                uncles.push((i - 1, block.hash));
                chain.add_block_to_chain(block).unwrap();
            }
            let block = P2Block::builder()
                .with_timestamp(timestamp)
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .with_uncles(uncles)
                .with_prev_hash(prev_hash)
                .build();

            prev_hash = block.generate_hash();

            chain.add_block_to_chain(block).unwrap();
        }

        let address = new_random_address();
        timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
        let mut uncles = Vec::new();
        let prev_hash_uncle = chain.get_at_height(6).unwrap().chain_block;
        // lets create an uncle block
        let block = P2Block::builder()
            .with_timestamp(timestamp)
            .with_height(7)
            .with_miner_wallet_address(address.clone())
            .with_target_difficulty(Difficulty::from_u64(10).unwrap())
            .with_prev_hash(prev_hash_uncle)
            .build();
        uncles.push((7, block.hash));
        chain.add_block_to_chain(block).unwrap();
        prev_hash = chain.get_at_height(7).unwrap().chain_block;
        let block = P2Block::builder()
            .with_timestamp(timestamp)
            .with_height(8)
            .with_miner_wallet_address(address.clone())
            .with_target_difficulty(Difficulty::from_u64(11).unwrap())
            .with_uncles(uncles)
            .with_prev_hash(prev_hash)
            .build();
        let new_block_hash = block.hash;

        chain.add_block_to_chain(block).unwrap();
        // lets create an uncle block
        let mut uncles = Vec::new();
        let block = P2Block::builder()
            .with_timestamp(timestamp)
            .with_height(8)
            .with_miner_wallet_address(address.clone())
            .with_target_difficulty(Difficulty::from_u64(10).unwrap())
            .with_prev_hash(prev_hash)
            .build();
        uncles.push((8, block.hash));
        chain.add_block_to_chain(block).unwrap();
        let block = P2Block::builder()
            .with_timestamp(timestamp)
            .with_height(9)
            .with_miner_wallet_address(address.clone())
            .with_target_difficulty(Difficulty::from_u64(11).unwrap())
            .with_uncles(uncles)
            .with_prev_hash(new_block_hash)
            .build();

        chain.add_block_to_chain(block).unwrap();
        let level = chain.get_tip().unwrap();
        assert_eq!(
            level.block_in_main_chain().unwrap().target_difficulty,
            Difficulty::from_u64(11).unwrap()
        );
        assert_eq!(level.block_in_main_chain().unwrap().height, 9);
        assert_eq!(
            chain.total_accumulated_tip_difficulty,
            AccumulatedDifficulty::from_u128(99).unwrap() //(10+9)*3 +10+11*2
        );
    }
}
