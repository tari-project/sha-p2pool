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
    ops::Deref,
    sync::Arc,
};

use log::info;
use tari_common_types::types::FixedHash;
use tari_core::proof_of_work::{lwma_diff::LinearWeightedMovingAverage, AccumulatedDifficulty, Difficulty};

use crate::sharechain::{
    error::Error,
    p2block::P2Block,
    p2chain_level::P2ChainLevel,
    BLOCK_TARGET_TIME,
    DIFFICULTY_ADJUSTMENT_WINDOW,
};
const LOG_TARGET: &str = "tari::p2pool::sharechain::chain";
// this is the max we are allowed to go over the size
pub const SAFETY_MARGIN: usize = 20;
// this is the max extra lenght the chain can grow in front of our tip
pub const MAX_EXTRA_SYNC: usize = 2000;

pub struct P2Chain {
    pub cached_shares: Option<HashMap<String, (u64, Vec<u8>)>>,
    pub(crate) levels: VecDeque<P2ChainLevel>,
    total_size: usize,
    share_window: usize,
    total_accumulated_tip_difficulty: AccumulatedDifficulty,
    current_tip: u64,
    pub lwma: LinearWeightedMovingAverage,
}

impl P2Chain {
    pub fn level_at_height(&self, height: u64) -> Option<&P2ChainLevel> {
        let tip = self.levels.front()?.height;
        if height > tip {
            return None;
        }
        let index = tip.checked_sub(height);
        self.levels
            .get(usize::try_from(index?).expect("32 bit systems not supported"))
    }

    fn get_block_at_height(&self, height: u64, hash: &FixedHash) -> Option<&Arc<P2Block>> {
        let level = self.level_at_height(height)?;
        level.blocks.get(hash)
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
        let first_index = self.levels.back().map(|level| level.height).unwrap_or(0);
        let current_chain_length = self.current_tip.saturating_sub(first_index);
        // let see if we are the limit for the current chain
        if current_chain_length >= (self.total_size + SAFETY_MARGIN) as u64 {
            return true;
        }
        // lets check to see if we are over the max sync length
        // Ideally this limit should not be reached ever
        self.levels.len() >= self.total_size + SAFETY_MARGIN + MAX_EXTRA_SYNC
    }

    fn set_new_tip(&mut self, new_height: u64, hash: FixedHash) -> Result<(), Error> {
        let block = self
            .get_block_at_height(new_height, &hash)
            .ok_or(Error::BlockNotFound)?
            .clone();
        // edge case for first block
        // if the tip is none and we added a block at height 0, it might return it here as a tip, so we need to check if
        // the newly added block == 0
        if self.get_tip().is_none() || (self.get_tip().map(|tip| tip.height).unwrap_or(0) == 0 && new_height == 0) {
            self.total_accumulated_tip_difficulty =
                AccumulatedDifficulty::from_u128(block.target_difficulty.as_u64() as u128)
                    .expect("Difficulty will always fit into accumulated difficulty");
        } else {
            self.total_accumulated_tip_difficulty = self
                .total_accumulated_tip_difficulty
                .checked_add_difficulty(block.target_difficulty)
                .ok_or_else(|| Error::DifficultyOverflow)?;
            for uncle in block.uncles.iter() {
                if let Some(uncle_block) = self.get_block_at_height(uncle.0, &uncle.1) {
                    self.total_accumulated_tip_difficulty = self
                        .total_accumulated_tip_difficulty
                        .checked_add_difficulty(uncle_block.target_difficulty)
                        .ok_or(Error::DifficultyOverflow)?;
                }
            }
        }
        let level = self.get_mut_at_height(new_height).ok_or(Error::BlockLevelNotFound)?;
        level.chain_block = hash;
        self.current_tip = level.height;

        // lets see if we need to subtract difficulty now that we have added a block
        if self.current_tip >= self.share_window as u64 {
            // our tip is more than the share window so its possible that we need to drop a block out of the pow window
            if let Some(level) = self
                .level_at_height(self.current_tip.saturating_sub(self.share_window as u64))
                .cloned()
            {
                let block = level.block_in_main_chain().ok_or(Error::BlockNotFound)?;
                self.decrease_total_chain_difficulty(block.target_difficulty)?;
                for (height, block_hash) in &block.uncles {
                    if let Some(link_level) = self.level_at_height(*height) {
                        let uncle_block = link_level.blocks.get(&block_hash).ok_or(Error::BlockNotFound)?;
                        self.decrease_total_chain_difficulty(uncle_block.target_difficulty)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn verify_chain(&mut self, new_block_height: u64, hash: FixedHash) -> Result<(), Error> {
        // we should validate what we can if a block is invalid, we should delete it.
        let mut missing_parents = Vec::new();
        let block = self
            .get_block_at_height(new_block_height, &hash)
            .ok_or(Error::BlockNotFound)?
            .clone();
        let algo = block.original_block.header.pow.pow_algo;

        // do we know of the parent
        // we should not check the chain start for parents
        if block.prev_hash != FixedHash::zero() && block.height != 0 {
            if self
                .get_block_at_height(new_block_height.saturating_sub(1), &block.prev_hash)
                .is_none()
            {
                // we dont know the parent
                missing_parents.push((new_block_height.saturating_sub(1), block.prev_hash.clone()));
            }
            // now lets check the uncles
            for uncle in block.uncles.iter() {
                if self.get_block_at_height(uncle.0, &uncle.1).is_some() {
                    // Uncle cannot be in the main chain
                    if let Some(level) = self.level_at_height(uncle.0) {
                        if level.chain_block == uncle.1 {
                            return Err(Error::UncleInMainChain {
                                height: uncle.0,
                                hash: uncle.1.clone(),
                            });
                        }
                    }
                } else {
                    missing_parents.push((uncle.0, uncle.1.clone()));
                }
            }
        }
        // edge case for first block
        // if the tip is none and we added a block at height 0, it might return it here as a tip, so we need to check if
        // the newly added block == 0
        if self.get_tip().map(|tip| tip.height).unwrap_or(0) == 0 && new_block_height == 0 {
            self.set_new_tip(new_block_height, hash)?;
            return Ok(());
        }

        if !missing_parents.is_empty() {
            return Err(Error::BlockParentDoesNotExist { missing_parents });
        }
        drop(block);
        {
            // lets mark as verified
            let level = self
                .get_mut_at_height(new_block_height)
                .ok_or(Error::BlockLevelNotFound)?;
            let mut block = level.blocks.get(&hash).ok_or(Error::BlockNotFound)?.deref().clone();
            // lets replace this
            block.verified = true;
            let _ = level.blocks.insert(hash, Arc::new(block));
        }

        // is this block part of the main chain?

        if self.get_tip().is_some() &&
            self.get_tip().unwrap().chain_block ==
                self.get_block_at_height(new_block_height, &hash)
                    .ok_or(Error::BlockNotFound)?
                    .prev_hash
        {
            // easy this builds on the tip
            info!(target: LOG_TARGET, "[{:?}] Block building on tip: {:?}", algo, new_block_height);
            self.set_new_tip(new_block_height, hash)?;
        } else {
            info!(target: LOG_TARGET, "[{:?}] Block is not building on tip: {:?}", algo, new_block_height);
            // lets check if we need to reorg here
            let block = self
                .get_block_at_height(new_block_height, &hash)
                .ok_or(Error::BlockNotFound)?
                .clone();
            let mut total_work = AccumulatedDifficulty::from_u128(block.target_difficulty.as_u64() as u128)
                .expect("Difficulty will always fit into accumulated difficulty");
            for uncle in block.uncles.iter() {
                if let Some(uncle_block) = self.get_block_at_height(uncle.0, &uncle.1) {
                    total_work = total_work
                        .checked_add_difficulty(uncle_block.target_difficulty)
                        .ok_or(Error::DifficultyOverflow)?;
                } else {
                    missing_parents.push((uncle.0, uncle.1.clone()));
                }
            }
            if !missing_parents.is_empty() {
                return Err(Error::BlockParentDoesNotExist { missing_parents });
            }
            let mut current_counting_block = block.clone();
            let mut counter = 1;
            while let Some(parent) = self.get_parent_block(&current_counting_block) {
                if !parent.verified {
                    // we cannot count unverified blocks
                    break;
                }
                total_work = total_work
                    .checked_add_difficulty(parent.target_difficulty)
                    .ok_or_else(|| Error::DifficultyOverflow)?;
                current_counting_block = parent.clone();
                for uncle in parent.uncles.iter() {
                    if let Some(uncle_block) = self.get_block_at_height(uncle.0, &uncle.1) {
                        total_work = total_work
                            .checked_add_difficulty(uncle_block.target_difficulty)
                            .ok_or(Error::DifficultyOverflow)?;
                        if !uncle_block.verified {
                            // we cannot count unverified blocks
                            break;
                        }
                    } else {
                        missing_parents.push((uncle.0, uncle.1.clone()));
                    }
                }
                if !missing_parents.is_empty() {
                    break;
                }
                counter += 1;
                if counter >= self.share_window {
                    break;
                }
            }
            if total_work > self.total_accumulated_tip_difficulty && counter >= self.share_window {
                // we need to reorg the chain
                // lets start by resetting the lwma
                self.lwma = LinearWeightedMovingAverage::new(DIFFICULTY_ADJUSTMENT_WINDOW, BLOCK_TARGET_TIME)
                    .expect("Failed to create LWMA");
                self.lwma
                    .add_front(block.original_block.header.timestamp, block.target_difficulty);
                let chain_height = self.get_mut_at_height(block.height).ok_or(Error::BlockLevelNotFound)?;
                chain_height.chain_block = block.hash.clone();
                self.cached_shares = None;
                self.current_tip = block.height;
                // lets fix the chain
                let mut current_block = block;
                while self.level_at_height(current_block.height.saturating_sub(1)).is_some() {
                    let parent_level = (self.level_at_height(current_block.height.saturating_sub(1)).unwrap()).clone();
                    if current_block.prev_hash != parent_level.chain_block {
                        // safety check
                        let nextblock = parent_level.blocks.get(&current_block.prev_hash);
                        if nextblock.is_none() {
                            return Err(Error::BlockNotFound);
                        }
                        // fix the main chain
                        let mut_parent_level = self.get_mut_at_height(current_block.height.saturating_sub(1)).unwrap();
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
            }
        }

        // let see if we already have a block that builds on top of this
        if let Some(next_level) = self.level_at_height(new_block_height + 1).cloned() {
            // we have a height here, lets check the blocks
            for block in next_level.blocks.iter() {
                if block.1.prev_hash == hash {
                    info!(target: LOG_TARGET, "[{:?}] Found block building on top of block: {:?}", algo, new_block_height);
                    // we have a parent here
                    match self.verify_chain(next_level.height, block.0.clone()) {
                        Err(Error::BlockParentDoesNotExist {
                            missing_parents: mut missing,
                        }) => missing_parents.append(&mut missing),
                        Err(e) => return Err(e),
                        Ok(_) => (),
                    }
                }
            }
        }
        if !missing_parents.is_empty() {
            return Err(Error::BlockParentDoesNotExist { missing_parents });
        }
        Ok(())
    }

    pub fn add_block_to_chain(&mut self, block: Arc<P2Block>) -> Result<(), Error> {
        let new_block_height = block.height;
        let block_hash = block.hash.clone();

        // Uncle cannot be the same as prev_hash
        if block.uncles.iter().any(|(_, hash)| hash == &block.prev_hash) {
            return Err(Error::InvalidBlock {
                reason: "Uncle cannot be the same as prev_hash".to_string(),
            });
        }

        // edge case no current chain, lets just add
        if self.get_tip().is_none() {
            let new_level = P2ChainLevel::new(block);
            self.levels.push_front(new_level);
            return self.verify_chain(new_block_height, block_hash);
        }

        // now lets add the block
        match self.get_mut_at_height(new_block_height) {
            Some(level) => {
                level.add_block(block)?;
                return self.verify_chain(new_block_height, block_hash);
            },
            None => {
                // So things got a bit more complicated, we dont have this level
                while self.levels.front().map(|level| level.height).unwrap_or(0) < new_block_height.saturating_sub(1) {
                    if self.is_full() {
                        self.levels.pop_back().ok_or(Error::BlockLevelNotFound)?;
                    }
                    let level = P2ChainLevel::new_empty(
                        self.levels.front().expect("we already checked its not empty").height + 1,
                    );
                    self.levels.push_front(level);
                }
                if self.levels.front().map(|level| level.height).unwrap_or(0) < new_block_height {
                    if self.is_full() {
                        self.levels.pop_back().ok_or(Error::BlockLevelNotFound)?;
                    }
                    let level = P2ChainLevel::new(block);
                    self.levels.push_front(level);
                    return self.verify_chain(new_block_height, block_hash);
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
                    if self.levels.back().map(|level| level.height).unwrap_or(0) > block.height {
                        if self.is_full() {
                            return Ok(());
                        }
                        let level = P2ChainLevel::new(block);
                        self.levels.push_back(level);
                    }
                }
            },
        }

        self.verify_chain(new_block_height, block_hash)
    }

    pub fn get_parent_block(&self, block: &P2Block) -> Option<&Arc<P2Block>> {
        let parent_height = match block.height.checked_sub(1) {
            Some(height) => height,
            None => return None,
        };
        let parent_level = match self.level_at_height(parent_height) {
            Some(level) => level,
            None => return None,
        };
        parent_level.blocks.get(&block.prev_hash)
    }

    pub fn get_tip(&self) -> Option<&P2ChainLevel> {
        self.level_at_height(self.current_tip)
    }

    pub fn get_height(&self) -> u64 {
        self.get_tip().map(|tip| tip.height).unwrap_or(0)
    }

    pub fn get_max_chain_length(&self) -> usize {
        let first_index = self.levels.back().map(|level| level.height).unwrap_or(0);
        let current_chain_length = self.current_tip.saturating_sub(first_index);
        usize::try_from(current_chain_length).expect("32 bit systems not supported")
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
        let mut prev_hash = BlockHash::zero();
        for i in 0..41 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2Block::builder()
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_tari_block(tari_block.clone())
                .with_prev_hash(prev_hash)
                .build();
            prev_hash = block.generate_hash();

            chain.add_block_to_chain(block.clone()).unwrap();
        }
        // 0..9 blocks should have been trimmed out

        for i in 10..41 {
            let level = chain.level_at_height(i).unwrap();
            assert_eq!(level.block_in_main_chain().unwrap().original_block.header.nonce, i);
        }

        let level = chain.level_at_height(10).unwrap();
        assert_eq!(level.block_in_main_chain().unwrap().original_block.header.nonce, 10);

        assert!(chain.level_at_height(0).is_none());
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
    fn does_ot_set_tip_unless_full_chain() {
        let mut chain = P2Chain::new_empty(10, 5);

        let mut prev_hash = BlockHash::zero();
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 1..5 {
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
        }
        tari_block.header.nonce = 5;
        let address = new_random_address();
        let block = P2Block::builder()
            .with_timestamp(EpochTime::now())
            .with_height(5)
            .with_tari_block(tari_block.clone())
            .with_miner_wallet_address(address.clone())
            .with_prev_hash(prev_hash)
            .build();
        prev_hash = block.generate_hash();
        chain.add_block_to_chain(block.clone()).unwrap();

        let level = chain.get_tip().unwrap();
        assert_eq!(chain.get_tip().unwrap().height, 5);
    }

    #[test]
    fn get_parent() {
        let mut chain = P2Chain::new_empty(10, 5);

        let mut prev_hash = BlockHash::zero();
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..41 {
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

        for i in 11..41 {
            let level = chain.level_at_height(i).unwrap();
            let block = level.block_in_main_chain().unwrap();
            let parent = chain.get_parent_block(&block).unwrap();
            assert_eq!(parent.original_block.header.nonce, i - 1);
        }

        let level = chain.level_at_height(10).unwrap();
        let block = level.block_in_main_chain().unwrap();
        assert!(chain.get_parent_block(&block).is_none());
    }

    #[test]
    fn add_blocks_to_chain_happy_path() {
        let mut chain = P2Chain::new_empty(10, 5);

        let mut timestamp = EpochTime::now();
        let mut prev_hash = BlockHash::zero();

        for i in 0..32 {
            let address = new_random_address();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let block = P2Block::builder()
                .with_timestamp(timestamp)
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(i as u64 + 1).unwrap())
                .with_prev_hash(prev_hash)
                .build();

            prev_hash = block.generate_hash();

            chain.add_block_to_chain(block).unwrap();

            let level = chain.get_tip().unwrap();
            assert_eq!(
                level.block_in_main_chain().unwrap().target_difficulty,
                Difficulty::from_u64(i as u64 + 1).unwrap()
            );
        }
    }

    #[test]
    fn add_blocks_to_chain_small_reorg() {
        let mut chain = P2Chain::new_empty(10, 5);

        let mut timestamp = EpochTime::now();
        let mut prev_hash = BlockHash::zero();

        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..32 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let block = P2Block::builder()
                .with_timestamp(timestamp)
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
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
            Difficulty::from_u64(10).unwrap()
        );
        assert_eq!(level.block_in_main_chain().unwrap().original_block.header.nonce, 31);
        assert_eq!(level.block_in_main_chain().unwrap().height, 31);
        assert_eq!(
            chain.total_accumulated_tip_difficulty,
            AccumulatedDifficulty::from_u128(50).unwrap() // 31+30+29+28+27
        );

        let block_29 = chain.level_at_height(29).unwrap().block_in_main_chain().unwrap();
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
            .with_target_difficulty(Difficulty::from_u64(9).unwrap())
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
            AccumulatedDifficulty::from_u128(71).unwrap() // 32+9+10+10+10
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
                let prev_hash_uncle = chain.level_at_height(i - 2).unwrap().chain_block;
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
                let prev_hash_uncle = chain.level_at_height(i - 2).unwrap().chain_block;
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
        let prev_hash_uncle = chain.level_at_height(6).unwrap().chain_block;
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
        prev_hash = chain.level_at_height(7).unwrap().chain_block;
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
