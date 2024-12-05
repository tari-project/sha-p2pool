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
    fmt,
    fmt::{Display, Formatter},
    ops::Deref,
    sync::Arc,
};

use log::{debug, error, info};
use tari_common_types::types::FixedHash;
use tari_core::proof_of_work::{lwma_diff::LinearWeightedMovingAverage, AccumulatedDifficulty};
use tari_utilities::hex::Hex;

use crate::sharechain::{
    error::ShareChainError,
    in_memory::MAX_UNCLE_AGE,
    p2block::P2Block,
    p2chain_level::P2ChainLevel,
    DIFFICULTY_ADJUSTMENT_WINDOW,
};

const LOG_TARGET: &str = "tari::p2pool::sharechain::chain";
// this is the max we are allowed to go over the size
pub const SAFETY_MARGIN: u64 = 20;
// this is the max extra lenght the chain can grow in front of our tip
pub const MAX_EXTRA_SYNC: u64 = 2000;
// this is the max bocks we store that are more than MAX_EXTRA_SYNC in front of our tip
pub const MAX_SYNC_STORE: usize = 200;
// this is the max missing parents we allow to process before we stop processing a chain and wait for more parents
pub const MAX_MISSING_PARENTS: usize = 100;

#[derive(Debug, Clone, Default)]
pub struct ChainAddResult {
    pub new_tip: Option<(FixedHash, u64)>,
    pub missing_blocks: HashMap<FixedHash, u64>,
}

impl ChainAddResult {
    pub fn combine(&mut self, other: ChainAddResult) {
        match (&self.new_tip, other.new_tip) {
            (Some(current_tip), Some(other_tip)) => {
                if other_tip.1 > current_tip.1 {
                    self.new_tip = Some(other_tip);
                }
            },
            (None, Some(new_tip)) => {
                self.new_tip = Some(new_tip);
            },
            _ => {},
        }
        for (hash, height) in other.missing_blocks {
            if self.missing_blocks.len() >= MAX_MISSING_PARENTS {
                break;
            }
            self.missing_blocks.insert(hash, height);
        }
    }

    pub fn set_new_tip(&mut self, hash: FixedHash, height: u64) {
        match self.new_tip {
            Some((_, current_height)) => {
                if height > current_height {
                    self.new_tip = Some((hash, height));
                }
            },
            None => {
                self.new_tip = Some((hash, height));
            },
        };
    }

    pub fn into_missing_parents_vec(self) -> Vec<(u64, FixedHash)> {
        self.missing_blocks
            .into_iter()
            .map(|(hash, height)| (height, hash))
            .collect()
    }
}

impl Display for ChainAddResult {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        if let Some(tip) = self.new_tip {
            writeln!(
                f,
                "Added new tip {}({:x}{:x}{:x}{:x})",
                tip.1, tip.0[0], tip.0[1], tip.0[2], tip.0[3]
            )?;
        } else {
            writeln!(f, "No new tip added")?;
        }
        if !self.missing_blocks.is_empty() {
            let mut missing_blocks: Vec<String> = Vec::new();
            for (hash, height) in &self.missing_blocks {
                missing_blocks.push(format!(
                    "{}({:x}{:x}{:x}{:x})",
                    height, hash[0], hash[1], hash[2], hash[3]
                ));
            }
            writeln!(f, "Missing blocks: {:?}", missing_blocks)?;
        }
        Ok(())
    }
}

pub struct P2Chain {
    pub block_time: u64,
    pub cached_shares: Option<HashMap<String, (u64, Vec<u8>)>>,
    pub(crate) levels: VecDeque<P2ChainLevel>,
    total_size: u64,
    share_window: u64,
    current_tip: u64,
    pub lwma: LinearWeightedMovingAverage,
    sync_store: HashMap<FixedHash, Arc<P2Block>>,
    sync_store_fifo_list: VecDeque<FixedHash>,
}

impl P2Chain {
    pub fn total_accumulated_tip_difficulty(&self) -> AccumulatedDifficulty {
        match self.get_tip() {
            Some(tip) => tip
                .block_in_main_chain()
                .map(|block| block.total_pow())
                .unwrap_or(AccumulatedDifficulty::min()),
            None => AccumulatedDifficulty::min(),
        }
    }

    pub fn level_at_height(&self, height: u64) -> Option<&P2ChainLevel> {
        let tip = self.levels.front()?.height;
        if height > tip {
            return None;
        }
        let index = tip.checked_sub(height);
        self.levels
            .get(usize::try_from(index?).expect("32 bit systems not supported"))
    }

    pub fn get_block_at_height(&self, height: u64, hash: &FixedHash) -> Option<&Arc<P2Block>> {
        let level = self.level_at_height(height)?;
        level.blocks.get(hash)
    }

    #[cfg(test)]
    fn get_chain_block_at_height(&self, height: u64) -> Option<&Arc<P2Block>> {
        let level = self.level_at_height(height)?;
        level.blocks.get(&level.chain_block)
    }

    pub fn level_at_height_mut(&mut self, height: u64) -> Option<&mut P2ChainLevel> {
        let tip = self.levels.front()?.height;
        if height > tip {
            return None;
        }
        let index = tip.checked_sub(height);
        self.levels
            .get_mut(usize::try_from(index?).expect("32 bit systems not supported"))
    }

    pub fn new_empty(total_size: u64, share_window: u64, block_time: u64) -> Self {
        let levels = VecDeque::with_capacity(usize::try_from(total_size).expect("Only 64bit supported") + 1);
        let lwma =
            LinearWeightedMovingAverage::new(DIFFICULTY_ADJUSTMENT_WINDOW, block_time).expect("Failed to create LWMA");
        Self {
            block_time,
            cached_shares: None,
            levels,
            total_size,
            share_window,
            current_tip: 0,
            lwma,
            sync_store: HashMap::new(),
            sync_store_fifo_list: VecDeque::new(),
        }
    }

    pub fn is_full(&self) -> bool {
        // lets check to see if we are over the max sync length
        // Ideally this limit should not be reached ever
        self.levels.len() as u64 >= self.total_size + SAFETY_MARGIN + MAX_EXTRA_SYNC
    }

    fn cleanup_chain(&mut self) -> Result<(), ShareChainError> {
        let mut first_index = self.levels.back().map(|level| level.height).unwrap_or(0);
        let mut current_chain_length = self.current_tip.saturating_sub(first_index);
        // let see if we are the limit for the current chain
        while current_chain_length > self.total_size + SAFETY_MARGIN {
            self.levels.pop_back().ok_or(ShareChainError::BlockLevelNotFound)?;
            first_index = self.levels.back().map(|level| level.height).unwrap_or(0);
            current_chain_length = self.current_tip.saturating_sub(first_index);
        }
        Ok(())
    }

    fn set_new_tip(&mut self, new_height: u64, hash: FixedHash) -> Result<(), ShareChainError> {
        let block = self
            .get_block_at_height(new_height, &hash)
            .ok_or(ShareChainError::BlockNotFound)?
            .clone();
        // edge case for first block
        // if the tip is none and we added a block at height 0, it might return it here as a tip, so we need to check if
        // the newly added block == 0
        self.lwma.add_back(block.timestamp, block.target_difficulty());
        let level = self
            .level_at_height_mut(new_height)
            .ok_or(ShareChainError::BlockLevelNotFound)?;
        level.chain_block = hash;
        self.current_tip = level.height;

        self.cleanup_chain()
    }

    fn verify_chain(&mut self, new_block_height: u64, hash: FixedHash) -> Result<ChainAddResult, ShareChainError> {
        let mut next_level = VecDeque::new();
        next_level.push_back((new_block_height, hash));
        let mut new_tip = ChainAddResult::default();
        while let Some((next_height, next_hash)) = next_level.pop_front() {
            match self.verify_chain_inner(next_height, next_hash) {
                Ok((add_result, do_next_level)) => {
                    new_tip.combine(add_result);
                    if new_tip.missing_blocks.len() >= MAX_MISSING_PARENTS {
                        return Ok(new_tip);
                    }
                    for item in do_next_level {
                        if next_level.contains(&item) {
                            continue;
                        }
                        // Don't get into an infinite loop

                        if item != (next_height, next_hash) {
                            next_level.push_back(item);
                        }
                    }
                },
                Err(e) => return Err(e),
            }
        }
        Ok(new_tip)
    }

    #[allow(clippy::too_many_lines)]
    fn verify_chain_inner(
        &mut self,
        new_block_height: u64,
        hash: FixedHash,
    ) -> Result<(ChainAddResult, Vec<(u64, FixedHash)>), ShareChainError> {
        // we should validate what we can if a block is invalid, we should delete it.
        let mut new_tip = ChainAddResult::default();
        let block = self
            .get_block_at_height(new_block_height, &hash)
            .ok_or(ShareChainError::BlockNotFound)?
            .clone();
        let algo = block.original_header.pow.pow_algo;
        // do we know of the parent
        // we should not check the chain start for parents
        if block.height != 0 {
            if self
                .get_block_at_height(new_block_height.saturating_sub(1), &block.prev_hash)
                .is_none()
            {
                // we dont know the parent
                new_tip
                    .missing_blocks
                    .insert(block.prev_hash, new_block_height.saturating_sub(1));
            }
            // now lets check the uncles
            for uncle in &block.uncles {
                if let Some(uncle_block) = self.get_block_at_height(uncle.0, &uncle.1) {
                    if self.get_parent_block(uncle_block).is_none() {
                        new_tip
                            .missing_blocks
                            .insert(uncle_block.prev_hash, uncle_block.height.saturating_sub(1));
                    }
                } else {
                    new_tip.missing_blocks.insert(uncle.1, uncle.0);
                }
            }
        }

        // lets verify the block
        if !new_tip.missing_blocks.is_empty() {
            let next_level_data = self.calculate_next_level_data(new_block_height, hash);
            return Ok((new_tip, next_level_data));
        }
        self.verify_block(hash, new_block_height)?;
        // we have to reload the block to check if verified is set to true now
        let block = self
            .get_block_at_height(new_block_height, &hash)
            .ok_or(ShareChainError::BlockNotFound)?
            .clone();

        // edge case for chain start
        if self.get_tip().is_none() && new_block_height == 0 {
            self.set_new_tip(new_block_height, hash)?;
            new_tip.set_new_tip(hash, new_block_height);
            return Ok((new_tip, Vec::new()));
        }
        if !block.verified {
            return Ok((new_tip, Vec::new()));
        }

        if self.get_tip().is_some() && self.get_tip().unwrap().chain_block == block.prev_hash {
            // easy this builds on the tip
            info!(target: LOG_TARGET, "[{:?}] New block added to tip, and is now the new tip: {:?}:{}", algo, new_block_height, &block.hash.to_hex()[0..8]);
            for uncle in &block.uncles {
                let uncle_block = self
                    .get_block_at_height(uncle.0, &uncle.1)
                    .ok_or(ShareChainError::BlockNotFound)?;
                let uncle_parent = self
                    .get_parent_block(uncle_block)
                    .ok_or(ShareChainError::BlockNotFound)?;
                let uncle_level = self
                    .level_at_height(uncle.0.saturating_sub(1))
                    .ok_or(ShareChainError::BlockLevelNotFound)?;
                if uncle_level.chain_block != uncle_parent.hash {
                    return Err(ShareChainError::UncleParentNotInMainChain);
                }
                let own_level = self
                    .level_at_height(uncle.0)
                    .ok_or(ShareChainError::BlockLevelNotFound)?;
                if own_level.chain_block == uncle.1 {
                    return Err(ShareChainError::UncleInMainChain {
                        height: uncle.0,
                        hash: uncle.1,
                    });
                }
            }

            self.set_new_tip(new_block_height, hash)?;
            new_tip.set_new_tip(hash, new_block_height);
        } else {
            let mut all_blocks_verified = true;
            debug!(target: LOG_TARGET, "[{:?}] New block is not on the tip, checking for reorg: {:?}", algo, new_block_height);

            let mut current_counting_block = block.clone();
            let mut counter = 1;
            // lets search for either the beginning of the chain, the fork or 2160 block back
            loop {
                if current_counting_block.height == 0 {
                    break;
                }
                if let Some(parent) = self.get_parent_block(&current_counting_block) {
                    if !parent.verified {
                        all_blocks_verified = false;
                        // so this block is unverified, we cannot count it but lets see if it just misses some blocks so
                        // we can ask for them
                        if self.get_parent_block(parent).is_none() {
                            new_tip
                                .missing_blocks
                                .insert(parent.prev_hash, parent.height.saturating_sub(1));
                        }
                        for uncle in &parent.uncles {
                            if self.get_block_at_height(uncle.0, &uncle.1).is_none() {
                                new_tip.missing_blocks.insert(uncle.1, uncle.0);
                            }
                        }
                        // we cannot count unverified blocks
                        break;
                    }
                } else {
                    new_tip.missing_blocks.insert(
                        current_counting_block.prev_hash,
                        current_counting_block.height.saturating_sub(1),
                    );
                    break;
                };
                counter += 1;
                if counter >= self.share_window {
                    break;
                }
                let level = self
                    .level_at_height(current_counting_block.height)
                    .ok_or(ShareChainError::BlockLevelNotFound)?;
                if level.chain_block == current_counting_block.hash {
                    break;
                }
                // we can unwrap as we now the parent exists
                current_counting_block = self.get_parent_block(&current_counting_block).unwrap().clone();
            }

            if !all_blocks_verified {
                let next_level_data = self.calculate_next_level_data(new_block_height, hash);
                return Ok((new_tip, next_level_data));
            }
            if !new_tip.missing_blocks.is_empty() {
                // we are missing blocks, stop counting
                let next_level_data = self.calculate_next_level_data(new_block_height, hash);
                return Ok((new_tip, next_level_data));
            }
            if block.total_pow() > self.total_accumulated_tip_difficulty() {
                new_tip.set_new_tip(hash, new_block_height);
                // we need to reorg the chain
                // lets start by resetting the lwma
                self.lwma = LinearWeightedMovingAverage::new(DIFFICULTY_ADJUSTMENT_WINDOW, self.block_time)
                    .expect("Failed to create LWMA");
                self.lwma.add_front(block.timestamp, block.target_difficulty());
                let chain_height = self
                    .level_at_height_mut(block.height)
                    .ok_or(ShareChainError::BlockLevelNotFound)?;
                chain_height.chain_block = block.hash;
                self.cached_shares = None;
                self.current_tip = block.height;
                // lets fix the chain
                // lets first go up and reset all chain block links
                let mut current_height = block.height;
                while self.level_at_height(current_height.saturating_add(1)).is_some() {
                    let mut_child_level = self.level_at_height_mut(current_height.saturating_add(1)).unwrap();
                    mut_child_level.chain_block = FixedHash::zero();
                    current_height += 1;
                }

                let mut current_block = block;
                let mut counter = 0;
                while self.level_at_height(current_block.height.saturating_sub(1)).is_some() {
                    counter += 1;
                    let parent_level = (self.level_at_height(current_block.height.saturating_sub(1)).unwrap()).clone();
                    if current_block.prev_hash != parent_level.chain_block {
                        // safety check
                        let nextblock = parent_level.blocks.get(&current_block.prev_hash);
                        if nextblock.is_none() {
                            error!(target: LOG_TARGET, "FATAL: Reorging (block in chain) failed because parent block was not found and chain data is corrupted.");
                            panic!(
                                "FATAL: Reorging (block in chain) failed because parent block was not found and chain \
                                 data is corrupted. current_block: {:?}, current tip: {:?}",
                                current_block,
                                self.get_tip()
                            );
                        }
                        // fix the main chain
                        let mut_parent_level = self
                            .level_at_height_mut(current_block.height.saturating_sub(1))
                            .unwrap();
                        mut_parent_level.chain_block = current_block.prev_hash;
                        current_block = nextblock.unwrap().clone();
                        self.lwma
                            .add_front(current_block.timestamp, current_block.target_difficulty());
                    } else if !self.lwma.is_full() {
                        // we still need more blocks to fill up the lwma
                        let nextblock = parent_level.blocks.get(&current_block.prev_hash);
                        if nextblock.is_none() {
                            error!(target: LOG_TARGET, "FATAL: Reorging (block not in chain) failed because parent block was not found and chain data is corrupted.");
                            panic!(
                                "FATAL: Could not calculate LMWA while reorging (block in chain) failed because \
                                 parent block was not found and chain data is corrupted. current_block: {:?}, current \
                                 tip: {:?}",
                                current_block,
                                self.get_tip()
                            );
                        }

                        current_block = nextblock.unwrap().clone();

                        self.lwma
                            .add_front(current_block.timestamp, current_block.target_difficulty());
                    } else {
                        break;
                    }

                    if current_block.height == 0 || counter >= self.share_window {
                        // edge case if there is less than the lwa size or share window in chain
                        break;
                    }
                }
            }
        }

        let next_level_data = self.calculate_next_level_data(new_block_height, hash);

        if !next_level_data.is_empty() {
            debug!(target: LOG_TARGET, "[{:?}] Found link in chain with other blocks we have: {:?}", algo, new_block_height);
        }
        Ok((new_tip, next_level_data))
    }

    fn calculate_next_level_data(&self, height: u64, hash: FixedHash) -> Vec<(u64, FixedHash)> {
        let mut next_level_data = Vec::new();

        // let see if we already have a block is a missing block of some other block
        for check_height in (height + 1)..height + MAX_UNCLE_AGE {
            if let Some(level) = self.level_at_height(check_height) {
                for block in &level.blocks {
                    for uncles in &block.1.uncles {
                        if uncles.1 == hash {
                            next_level_data.push((block.1.height, block.1.hash));
                        }
                    }
                    if block.1.prev_hash == hash {
                        next_level_data.push((block.1.height, block.1.hash));
                    }
                }
            }
        }
        next_level_data
    }

    // this assumes it has no missing parents
    fn verify_block(&mut self, hash: FixedHash, height: u64) -> Result<(), ShareChainError> {
        let level = self
            .level_at_height(height)
            .ok_or(ShareChainError::BlockLevelNotFound)?;
        let block = level.blocks.get(&hash).ok_or(ShareChainError::BlockNotFound)?;
        if block.verified {
            return Ok(());
        }
        let verified = true;

        // lets check the total accumulated difficulty
        let mut total_work = AccumulatedDifficulty::from_u128(u128::from(block.target_difficulty().as_u64()))
            .expect("Difficulty will always fit into accumulated difficulty");
        for uncle in &block.uncles {
            let uncle_block = self
                .get_block_at_height(uncle.0, &uncle.1)
                .ok_or(ShareChainError::BlockNotFound)?;
            total_work = total_work
                .checked_add_difficulty(uncle_block.target_difficulty())
                .ok_or(ShareChainError::DifficultyOverflow)?;
        }

        // special edge case for start, there is no parent
        if height == 0 {
            if block.total_pow().as_u128() != total_work.as_u128() {
                return Err(ShareChainError::BlockTotalWorkMismatch);
            }
            let mut actual_block = block.deref().clone();
            // lets replace this
            actual_block.verified = verified;
            let level = self
                .level_at_height_mut(height)
                .ok_or(ShareChainError::BlockLevelNotFound)?;
            level.blocks.insert(hash, Arc::new(actual_block));
            return Ok(());
        }

        let parent = self
            .get_block_at_height(block.height.saturating_sub(1), &block.prev_hash)
            .ok_or(ShareChainError::BlockNotFound)?;

        if block.total_pow().as_u128() != parent.total_pow().as_u128() + total_work.as_u128() {
            return Err(ShareChainError::BlockTotalWorkMismatch);
        }

        if verified {
            let mut actual_block = block.deref().clone();
            // lets replace this
            actual_block.verified = verified;
            let level = self
                .level_at_height_mut(height)
                .ok_or(ShareChainError::BlockLevelNotFound)?;
            level.blocks.insert(hash, Arc::new(actual_block));
        }

        Ok(())
    }

    fn add_block_inner(&mut self, block: Arc<P2Block>) -> Result<ChainAddResult, ShareChainError> {
        let new_block_height = block.height;
        let block_hash = block.hash;
        // edge case no current chain, lets just add
        if self.levels.is_empty() {
            let new_level = P2ChainLevel::new(block);
            self.levels.push_front(new_level);
            return self.verify_chain(new_block_height, block_hash);
        }

        // now lets add the block
        // The process is:
        // 1. If the height exists in level, then add the block to the level
        // 2. Else, we need to create a new level.
        //   a. If the height is higher than the current front, add empty levels so that there are no gaps
        //   b. Then add the level
        //   c. If the height is lower than the back and the chain is full, don't do anything
        //   d. Otherwise, add empty levels at the back until we reach the block.
        match self.level_at_height_mut(new_block_height) {
            Some(level) => {
                level.add_block(block)?;
                return self.verify_chain(new_block_height, block_hash);
            },
            None => {
                // So things got a bit more complicated, we dont have this level
                while self.levels.front().map(|level| level.height).unwrap_or(0) < new_block_height.saturating_sub(1) {
                    if self.is_full() {
                        self.levels.pop_back().ok_or(ShareChainError::BlockLevelNotFound)?;
                    }
                    let level = P2ChainLevel::new_empty(
                        self.levels.front().expect("we already checked its not empty").height + 1,
                    );
                    self.levels.push_front(level);
                }
                if self.levels.front().map(|level| level.height).unwrap_or(0) < new_block_height {
                    if self.is_full() {
                        self.levels.pop_back().ok_or(ShareChainError::BlockLevelNotFound)?;
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
                            return Ok(ChainAddResult::default());
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
                            return Ok(ChainAddResult::default());
                        }
                        let level = P2ChainLevel::new(block);

                        self.levels.push_back(level);
                    }
                    return self.verify_chain(new_block_height, block_hash);
                }
            },
        }
        // so the chain is full, we should not add below the height of the lowest block
        Ok(ChainAddResult::default())
    }

    pub fn add_block_to_chain(&mut self, block: Arc<P2Block>) -> Result<ChainAddResult, ShareChainError> {
        let new_block_height = block.height;
        let block_hash = block.hash;

        // lets check where this is, do we need to store it in the sync store
        let first_index = self.levels.back().map(|level| level.height).unwrap_or(0);
        if new_block_height >= first_index + self.total_size + SAFETY_MARGIN + MAX_EXTRA_SYNC {
            if self.sync_store.len() > MAX_SYNC_STORE {
                // lets remove the oldest block
                if let Some(hash) = self.sync_store_fifo_list.pop_back() {
                    self.sync_store.remove(&hash);
                }
            }
            self.sync_store.insert(block_hash, block.clone());
            self.sync_store_fifo_list.push_front(block_hash);

            // lets see how long a chain we can build with this block
            let mut current_block_hash = block.prev_hash;
            let mut blocks_to_add = vec![block.hash];

            while let Some(parent) = self.sync_store.get(&current_block_hash) {
                blocks_to_add.push(current_block_hash);
                current_block_hash = parent.prev_hash;
            }
            // lets go forward
            current_block_hash = block.hash;
            'outer_loop: loop {
                for orphan_block in &self.sync_store {
                    if orphan_block.1.prev_hash == current_block_hash {
                        blocks_to_add.push(current_block_hash);
                        current_block_hash = orphan_block.1.hash;
                        continue 'outer_loop;
                    }
                }
                break 'outer_loop;
            }

            let mut new_tip = ChainAddResult::default();
            if blocks_to_add.len() > 150 {
                // we have a potential long chain, lets see if we can do anything with it.
                for block in &blocks_to_add {
                    let p2_block = self
                        .sync_store
                        .get(block)
                        .ok_or(ShareChainError::BlockNotFound)?
                        .clone();
                    match self.add_block_inner(p2_block) {
                        Err(e) => return Err(e),
                        Ok(tip) => {
                            new_tip.combine(tip);
                        },
                    }
                }
            }

            let mut is_parent_in_main_chain = false;
            if let Some(parent_block) = self.get_block_at_height(new_block_height.saturating_sub(1), &block.prev_hash) {
                is_parent_in_main_chain =
                    self.level_at_height(parent_block.height).unwrap().chain_block == block.prev_hash;
            } else {
                new_tip
                    .missing_blocks
                    .insert(block.prev_hash, new_block_height.saturating_sub(1));
            }
            // now lets check the uncles
            for uncle in &block.uncles {
                if self.get_block_at_height(uncle.0, &uncle.1).is_some() {
                    if let Some(level) = self.level_at_height(uncle.0) {
                        if level.chain_block == uncle.1 && is_parent_in_main_chain {
                            // Uncle in main chain is ok if this block is not on the main chain
                            return Err(ShareChainError::UncleInMainChain {
                                height: uncle.0,
                                hash: uncle.1,
                            });
                        }
                    }
                } else {
                    new_tip.missing_blocks.insert(uncle.1, uncle.0);
                }
            }

            return Ok(new_tip);
        }

        // Uncle cannot be the same as prev_hash
        if block.uncles.iter().any(|(_, hash)| hash == &block.prev_hash) {
            return Err(ShareChainError::InvalidBlock {
                reason: "Uncle cannot be the same as prev_hash".to_string(),
            });
        }

        self.add_block_inner(block)
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
            .filter(|&level| level.chain_block != FixedHash::zero())
    }

    pub fn get_height(&self) -> u64 {
        self.get_tip().map(|tip| tip.height).unwrap_or(0)
    }

    pub fn get_max_chain_length(&self) -> usize {
        let first_index = self.levels.back().map(|level| level.height).unwrap_or(0);
        let current_chain_length = self.current_tip.saturating_sub(first_index);
        usize::try_from(current_chain_length).expect("32 bit systems not supported")
    }

    #[cfg(test)]
    fn assert_share_window_verified(&self) {
        let tip = self.get_tip().unwrap();
        let mut current_block = tip.block_in_main_chain().unwrap().clone();
        if !current_block.verified {
            panic!("Tip block is not verified");
        }
        let mut counter = 1;
        while let Some(parent) = self.get_parent_block(&current_block) {
            if !parent.verified {
                panic!("Parent block is not verified");
            }
            current_block = parent.clone();
            for uncle in &parent.uncles {
                if let Some(uncle_block) = self.get_block_at_height(uncle.0, &uncle.1) {
                    if !uncle_block.verified {
                        panic!("Uncle block is not verified");
                    }
                }
            }
            counter += 1;
            if counter >= self.share_window {
                break;
            }
            if parent.height == 0 {
                // edge case if there is less than the lwa size or share window in chain
                break;
            }
        }
    }
}

#[cfg(test)]
mod test {
    use tari_core::{
        blocks::{Block, BlockHeader},
        proof_of_work::{Difficulty, DifficultyAdjustment},
        transactions::aggregated_body::AggregateBody,
    };
    use tari_utilities::epoch_time::EpochTime;

    use super::*;
    use crate::sharechain::{in_memory::test::new_random_address, p2block::P2BlockBuilder};

    #[test]
    fn test_only_keeps_size() {
        let mut chain = P2Chain::new_empty(10, 5, 10);
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        let mut prev_block = None;
        for i in 0..41 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_tari_block(tari_block.clone())
                .unwrap()
                .build()
                .unwrap();
            prev_block = Some(block.clone());

            chain.add_block_to_chain(block.clone()).unwrap();
        }
        // 0..9 blocks should have been trimmed out

        for i in 10..41 {
            let level = chain.level_at_height(i).unwrap();
            assert_eq!(level.block_in_main_chain().unwrap().original_header.nonce, i);
        }

        let level = chain.level_at_height(10).unwrap();
        assert_eq!(level.block_in_main_chain().unwrap().original_header.nonce, 10);

        assert!(chain.level_at_height(0).is_none());
    }

    #[test]
    fn get_tips() {
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..30 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            chain.add_block_to_chain(block.clone()).unwrap();

            let level = chain.get_tip().unwrap();
            assert_eq!(level.height, i);
            assert_eq!(level.block_in_main_chain().unwrap().original_header.nonce, i);
        }
    }

    #[test]
    fn test_does_not_set_tip_unless_full_chain() {
        // we have a window of 5, meaing that we need 5 valid blocks
        // if we dont start at 0, we need a chain of at least 6 blocks
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 1..6 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            chain.add_block_to_chain(block.clone()).unwrap();
            assert!(chain.get_tip().is_none());
        }
        tari_block.header.nonce = 6;
        let address = new_random_address();
        let block = P2BlockBuilder::new(prev_block.as_ref())
            .with_timestamp(EpochTime::now())
            .with_height(6)
            .with_tari_block(tari_block.clone())
            .unwrap()
            .with_miner_wallet_address(address.clone())
            .build()
            .unwrap();
        chain.add_block_to_chain(block.clone()).unwrap();

        let level = chain.get_tip().unwrap();
        assert_eq!(level.height, 6);

        // the whole chain must be verified
        chain.assert_share_window_verified();
        // first block should not be verified
        assert!(!chain.get_chain_block_at_height(1).unwrap().verified);
    }

    #[test]
    fn test_sets_tip_when_full() {
        // this test test if we can add blocks in rev order and when it gets 5 verified blocks it sets the tip
        // to test this properly we need 6 blocks in the chain, and not use 0 as zero will always be valid and counter
        // as chain start block height 2 will only be valid if it has parents aka block 1, so we need share
        // window + 1 blocks in chain--
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        let mut blocks = Vec::new();
        for i in 0..7 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            blocks.push(block.clone());
        }
        chain.add_block_to_chain(blocks[6].clone()).unwrap();
        assert!(chain.get_tip().is_none());
        assert_eq!(chain.current_tip, 0);
        assert_eq!(chain.levels.len(), 1);
        assert_eq!(chain.levels[0].height, 6);

        for i in (2..6).rev() {
            chain.add_block_to_chain(blocks[i].clone()).unwrap();
            assert!(chain.get_tip().is_none());
            assert_eq!(chain.current_tip, 0);
        }
        chain.add_block_to_chain(blocks[1].clone()).unwrap();

        let level = chain.get_tip().unwrap();
        assert_eq!(level.height, 6);
        chain.assert_share_window_verified();
    }

    #[test]
    fn test_sets_tip_when_adding_blocks_from_both_side() {
        // this test test if we can add blocks in rev order and when it gets 5 verified blocks it sets the tip
        // to test this properly we need 6 blocks in the chain, and not use 0 as zero will always be valid and counter
        // as chain start block height 2 will only be valid if it has parents aka block 1, so we need share
        // window + 1 blocks in chain--
        let mut chain = P2Chain::new_empty(20, 10, 10);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        let mut blocks = Vec::new();
        for i in 0..20 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            blocks.push(block.clone());
        }
        for i in 0..9 {
            chain.add_block_to_chain(blocks[i].clone()).unwrap();
            assert_eq!(chain.get_tip().unwrap().height, i as u64);
            chain.add_block_to_chain(blocks[19 - i].clone()).unwrap();
            assert_eq!(chain.get_tip().unwrap().height, i as u64);
        }

        chain.add_block_to_chain(blocks[9].clone()).unwrap();
        assert_eq!(chain.get_tip().unwrap().height, 9);

        chain.add_block_to_chain(blocks[10].clone()).unwrap();
        assert_eq!(chain.get_tip().unwrap().height, 19);

        chain.assert_share_window_verified();
    }

    #[test]
    fn test_sets_tip_when_full_with_uncles() {
        // this test test if we can add blocks in rev order and when it gets 5 verified blocks it sets the tip
        // to test this properly we need 6 blocks in the chain, and not use 0 as zero will always be valid and counter
        // as chain start block height 2 will only be valid if it has parents aka block 1, so we need share
        // window + 1 blocks in chain--
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        let mut blocks = Vec::new();
        for i in 0..6 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            blocks.push(block.clone());
        }
        tari_block.header.nonce = 55;
        let address = new_random_address();
        let uncle_block = P2BlockBuilder::new(Some(&blocks[4]))
            .with_timestamp(EpochTime::now())
            .with_height(5)
            .with_tari_block(tari_block.clone())
            .unwrap()
            .with_miner_wallet_address(address.clone())
            .build()
            .unwrap();

        tari_block.header.nonce = 6;
        let address = new_random_address();
        let block = P2BlockBuilder::new(prev_block.as_ref())
            .with_timestamp(EpochTime::now())
            .with_height(6)
            .with_tari_block(tari_block.clone())
            .unwrap()
            .with_miner_wallet_address(address.clone())
            .with_uncles(&vec![uncle_block.clone()])
            .unwrap()
            .build()
            .unwrap();
        blocks.push(block.clone());

        chain.add_block_to_chain(blocks[6].clone()).unwrap();
        assert!(chain.get_tip().is_none());
        assert_eq!(chain.current_tip, 0);
        assert_eq!(chain.levels.len(), 1);
        assert_eq!(chain.levels[0].height, 6);

        for i in (2..6).rev() {
            chain.add_block_to_chain(blocks[i].clone()).unwrap();
            assert!(chain.get_tip().is_none());
            assert_eq!(chain.current_tip, 0);
        }

        chain.add_block_to_chain(blocks[1].clone()).unwrap();

        assert!(chain.get_tip().is_none());
        chain.add_block_to_chain(uncle_block).unwrap();

        let level = chain.get_tip().unwrap();
        assert_eq!(level.height, 6);
    }

    #[test]
    fn test_dont_set_tip_on_single_high_height() {
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..5 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            chain.add_block_to_chain(block.clone()).unwrap();

            let level = chain.get_tip().unwrap();
            assert_eq!(level.height, i);
        }
        // we do this so we can add a missing parent or 2
        let address = new_random_address();
        let block = P2BlockBuilder::new(prev_block.as_ref())
            .with_timestamp(EpochTime::now())
            .with_height(100)
            .with_tari_block(tari_block.clone())
            .unwrap()
            .with_miner_wallet_address(address.clone())
            .build()
            .unwrap();
        prev_block = Some(block.clone());
        let address = new_random_address();
        let block = P2BlockBuilder::new(prev_block.as_ref())
            .with_timestamp(EpochTime::now())
            .with_height(2000)
            .with_tari_block(tari_block.clone())
            .unwrap()
            .with_miner_wallet_address(address.clone())
            .build()
            .unwrap();
        prev_block = Some(block.clone());

        chain.add_block_to_chain(block.clone()).unwrap();

        let level = chain.get_tip().unwrap();
        assert_eq!(level.height, 4);

        let address = new_random_address();
        let block = P2BlockBuilder::new(prev_block.as_ref())
            .with_timestamp(EpochTime::now())
            .with_height(1000)
            .with_tari_block(tari_block.clone())
            .unwrap()
            .with_miner_wallet_address(address.clone())
            .build()
            .unwrap();
        prev_block = Some(block.clone());
        let address = new_random_address();
        let block = P2BlockBuilder::new(prev_block.as_ref())
            .with_timestamp(EpochTime::now())
            .with_height(20000)
            .with_tari_block(tari_block.clone())
            .unwrap()
            .with_miner_wallet_address(address.clone())
            .build()
            .unwrap();

        chain.add_block_to_chain(block.clone()).unwrap();

        let level = chain.get_tip().unwrap();
        assert_eq!(level.height, 4);
    }

    #[test]
    fn get_parent() {
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..41 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_tari_block(tari_block.clone())
                .unwrap()
                .build()
                .unwrap();

            prev_block = Some(block.clone());
            chain.add_block_to_chain(block.clone()).unwrap();
        }

        for i in 11..41 {
            let level = chain.level_at_height(i).unwrap();
            let block = level.block_in_main_chain().unwrap();
            let parent = chain.get_parent_block(block).unwrap();
            assert_eq!(parent.original_header.nonce, i - 1);
        }

        let level = chain.level_at_height(10).unwrap();
        let block = level.block_in_main_chain().unwrap();
        assert!(chain.get_parent_block(block).is_none());
    }

    #[test]
    fn add_blocks_to_chain_happy_path() {
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let mut timestamp = EpochTime::now();
        let mut prev_block = None;

        for i in 0..32 {
            let address = new_random_address();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(timestamp)
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(i + 1).unwrap())
                .unwrap()
                .build()
                .unwrap();

            prev_block = Some(block.clone());

            chain.add_block_to_chain(block).unwrap();

            let level = chain.get_tip().unwrap();
            assert_eq!(
                level.block_in_main_chain().unwrap().target_difficulty(),
                Difficulty::from_u64(i + 1).unwrap()
            );
        }
    }

    #[test]
    fn add_blocks_to_chain_small_reorg() {
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let mut timestamp = EpochTime::now();
        let mut prev_block = None;

        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..32 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(timestamp)
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .unwrap()
                .with_tari_block(tari_block.clone())
                .unwrap()
                .build()
                .unwrap();

            prev_block = Some(block.clone());
            chain.add_block_to_chain(block).unwrap();
        }
        let level = chain.get_tip().unwrap();
        let tip_hash = level.block_in_main_chain().unwrap().generate_hash();
        assert_eq!(
            level.block_in_main_chain().unwrap().target_difficulty(),
            Difficulty::from_u64(10).unwrap()
        );
        assert_eq!(level.block_in_main_chain().unwrap().original_header.nonce, 31);
        assert_eq!(level.block_in_main_chain().unwrap().height, 31);
        assert_eq!(
            chain.total_accumulated_tip_difficulty(),
            AccumulatedDifficulty::from_u128(320).unwrap()
        );

        let block_29 = chain.level_at_height(29).unwrap().block_in_main_chain().unwrap();
        prev_block = Some((*block_29).clone());
        timestamp = block_29.timestamp;

        let address = new_random_address();
        timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
        tari_block.header.nonce = 30 * 2;
        let block = P2BlockBuilder::new(prev_block.as_ref())
            .with_timestamp(timestamp)
            .with_height(30)
            .with_miner_wallet_address(address.clone())
            .with_target_difficulty(Difficulty::from_u64(9).unwrap())
            .unwrap()
            .with_tari_block(tari_block.clone())
            .unwrap()
            .build()
            .unwrap();

        prev_block = Some(block.clone());

        chain.add_block_to_chain(block).unwrap();
        let level = chain.get_tip().unwrap();
        // still the old tip
        assert_eq!(tip_hash, level.block_in_main_chain().unwrap().generate_hash());

        let address = new_random_address();

        tari_block.header.nonce = 31 * 2;
        timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
        let block = P2BlockBuilder::new(prev_block.as_ref())
            .with_timestamp(timestamp)
            .with_height(31)
            .with_miner_wallet_address(address.clone())
            .with_target_difficulty(Difficulty::from_u64(32).unwrap())
            .unwrap()
            .with_tari_block(tari_block.clone())
            .unwrap()
            .build()
            .unwrap();

        chain.add_block_to_chain(block).unwrap();
        let level = chain.get_tip().unwrap();
        // now it should be the new tip
        assert_ne!(tip_hash, level.block_in_main_chain().unwrap().generate_hash());
        assert_eq!(
            level.block_in_main_chain().unwrap().target_difficulty(),
            Difficulty::from_u64(32).unwrap()
        );
        assert_eq!(level.block_in_main_chain().unwrap().original_header.nonce, 31 * 2);
        assert_eq!(level.block_in_main_chain().unwrap().height, 31);
        assert_eq!(
            chain.total_accumulated_tip_difficulty(),
            AccumulatedDifficulty::from_u128(341).unwrap()
        );
    }

    #[test]
    fn add_blocks_to_chain_super_large_reorg() {
        // this test will verify that we reorg to a completely new chain
        let mut chain = P2Chain::new_empty(10, 5, 20);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..1000 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .unwrap()
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            chain.add_block_to_chain(block).unwrap();
        }

        assert_eq!(chain.current_tip, 999);
        assert_eq!(chain.get_tip().unwrap().chain_block, prev_block.unwrap().hash);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..1000 {
            tari_block.header.nonce = i + 100;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(11).unwrap())
                .unwrap()
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            chain.add_block_to_chain(block).unwrap();
        }
        assert_eq!(chain.current_tip, 999);
        assert_eq!(chain.get_tip().unwrap().chain_block, prev_block.unwrap().hash);
        assert_eq!(
            chain
                .get_tip()
                .unwrap()
                .block_in_main_chain()
                .unwrap()
                .original_header
                .nonce,
            1099
        );

        chain.assert_share_window_verified();
    }

    #[test]
    fn add_blocks_missing_block() {
        // this test will verify that we reorg to a completely new chain
        let mut chain = P2Chain::new_empty(50, 25, 20);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        let mut blocks = Vec::new();
        for i in 0..50 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .unwrap()
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            blocks.push(block);
        }

        for (i, block) in blocks.iter().enumerate().take(50) {
            if i != 25 {
                chain.add_block_to_chain(block.clone()).unwrap();
            }
        }
        assert_eq!(chain.current_tip, 24);
        chain.add_block_to_chain(blocks[25].clone()).unwrap();

        assert_eq!(chain.current_tip, 49);
        assert_eq!(chain.get_tip().unwrap().chain_block, prev_block.unwrap().hash);

        chain.assert_share_window_verified();
    }

    #[test]
    fn reorg_with_missing_uncle() {
        // this test will verify that we reorg to a completely new chain
        let mut chain = P2Chain::new_empty(50, 25, 20);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..50 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .unwrap()
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            chain.add_block_to_chain(block).unwrap();
        }

        assert_eq!(chain.current_tip, 49);
        assert_eq!(chain.get_tip().unwrap().chain_block, prev_block.unwrap().hash);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        let mut uncle_parent = None;
        let mut uncle_block = None;
        for i in 0..50 {
            tari_block.header.nonce = i + 100;
            let address = new_random_address();
            let uncles = if i == 25 {
                let uncle = P2BlockBuilder::new(uncle_parent.as_ref())
                    .with_timestamp(EpochTime::now())
                    .with_height(24)
                    .with_tari_block(tari_block.clone())
                    .unwrap()
                    .with_miner_wallet_address(address.clone())
                    .build()
                    .unwrap();
                uncle_block = Some(uncle.clone());
                vec![uncle]
            } else {
                vec![]
            };
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .with_uncles(&uncles)
                .unwrap()
                .with_target_difficulty(Difficulty::from_u64(11).unwrap())
                .unwrap()
                .build()
                .unwrap();
            if i == 23 {
                uncle_parent = Some(block.clone());
            }
            prev_block = Some(block.clone());
            chain.add_block_to_chain(block).unwrap();
        }

        assert_eq!(chain.current_tip, 49);
        let hash = prev_block.unwrap().hash;
        assert_ne!(chain.get_tip().unwrap().chain_block, hash);
        chain.add_block_to_chain(uncle_block.unwrap()).unwrap();
        assert_eq!(chain.get_tip().unwrap().chain_block, hash);
        assert_eq!(
            chain
                .get_tip()
                .unwrap()
                .block_in_main_chain()
                .unwrap()
                .original_header
                .nonce,
            149
        );

        chain.assert_share_window_verified();
    }

    #[test]
    fn add_blocks_to_chain_super_large_reorg_only_window() {
        // this test will verify that we reorg to a completely new chain
        let mut chain = P2Chain::new_empty(10, 5, 20);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..1000 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .unwrap()
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            chain.add_block_to_chain(block).unwrap();
        }

        assert_eq!(chain.current_tip, 999);
        assert_eq!(chain.get_tip().unwrap().chain_block, prev_block.unwrap().hash);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        let mut blocks = Vec::new();
        for i in 0..1000 {
            tari_block.header.nonce = i + 100;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(11).unwrap())
                .unwrap()
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            blocks.push(block.clone());
        }
        for block in blocks.iter().take(1000).skip(990) {
            chain.add_block_to_chain(block.clone()).unwrap();
        }
        assert_eq!(chain.current_tip, 999);
        assert_eq!(chain.get_tip().unwrap().chain_block, prev_block.unwrap().hash);
        assert_eq!(
            chain
                .get_tip()
                .unwrap()
                .block_in_main_chain()
                .unwrap()
                .original_header
                .nonce,
            1099
        );

        chain.assert_share_window_verified();
    }

    #[test]
    fn calculate_total_difficulty_correctly() {
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let mut timestamp = EpochTime::now();
        let mut prev_block = None;

        for i in 1..15 {
            let address = new_random_address();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(timestamp)
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .unwrap()
                .build()
                .unwrap();

            prev_block = Some(block.clone());

            chain.add_block_to_chain(block).unwrap();
        }
        assert_eq!(
            chain.total_accumulated_tip_difficulty(),
            AccumulatedDifficulty::from_u128(140).unwrap() //(10)*15
        );
    }

    #[test]
    fn calculate_total_difficulty_correctly_with_uncles() {
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let mut timestamp = EpochTime::now();
        let mut prev_block = None;

        for i in 0..10 {
            let address = new_random_address();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let mut uncles = Vec::new();
            if i > 1 {
                let prev_uncle = chain.level_at_height(i - 2).unwrap().block_in_main_chain().unwrap();
                // lets create an uncle block
                let block = P2BlockBuilder::new(Some(prev_uncle))
                    .with_timestamp(timestamp)
                    .with_height(i - 1)
                    .with_miner_wallet_address(address.clone())
                    .with_target_difficulty(Difficulty::from_u64(9).unwrap())
                    .unwrap()
                    .build()
                    .unwrap();
                uncles.push(block.clone());
                chain.add_block_to_chain(block).unwrap();
            }
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(timestamp)
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .unwrap()
                .with_uncles(&uncles)
                .unwrap()
                .build()
                .unwrap();

            prev_block = Some(block.clone());

            chain.add_block_to_chain(block).unwrap();
        }
        let level = chain.get_tip().unwrap();
        assert_eq!(
            level.block_in_main_chain().unwrap().target_difficulty(),
            Difficulty::from_u64(10).unwrap()
        );
        assert_eq!(level.block_in_main_chain().unwrap().height, 9);
        assert_eq!(
            chain.total_accumulated_tip_difficulty(),
            AccumulatedDifficulty::from_u128(172).unwrap()
        );
    }

    #[test]
    fn calculate_total_difficulty_correctly_with_wrapping_blocks() {
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let mut timestamp = EpochTime::now();
        let mut prev_block = None;

        for i in 0..20 {
            let address = new_random_address();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let mut uncles = Vec::new();
            if i > 1 {
                let prev_uncle = chain.level_at_height(i - 2).unwrap().block_in_main_chain().unwrap();
                // lets create an uncle block
                let block = P2BlockBuilder::new(Some(prev_uncle))
                    .with_timestamp(timestamp)
                    .with_height(i - 1)
                    .with_miner_wallet_address(address.clone())
                    .with_target_difficulty(Difficulty::from_u64(9).unwrap())
                    .unwrap()
                    .build()
                    .unwrap();
                uncles.push(block.clone());
                chain.add_block_to_chain(block).unwrap();
            }
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(timestamp)
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .unwrap()
                .with_uncles(&uncles)
                .unwrap()
                .build()
                .unwrap();

            prev_block = Some(block.clone());

            chain.add_block_to_chain(block).unwrap();
        }
        let level = chain.get_tip().unwrap();
        assert_eq!(
            level.block_in_main_chain().unwrap().target_difficulty(),
            Difficulty::from_u64(10).unwrap()
        );
        assert_eq!(level.block_in_main_chain().unwrap().height, 19);
        assert_eq!(
            chain.total_accumulated_tip_difficulty(),
            AccumulatedDifficulty::from_u128(362).unwrap() //(10+9)*20 - (9*2)
        );
    }

    #[test]
    fn reorg_with_uncles() {
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let mut timestamp = EpochTime::now();
        let mut prev_block = None;

        for i in 0..10 {
            let address = new_random_address();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let mut uncles = Vec::new();
            if i > 1 {
                let prev_uncle = chain.level_at_height(i - 2).unwrap().block_in_main_chain().unwrap();
                // lets create an uncle block
                let block = P2BlockBuilder::new(Some(prev_uncle))
                    .with_timestamp(timestamp)
                    .with_height(i - 1)
                    .with_miner_wallet_address(address.clone())
                    .with_target_difficulty(Difficulty::from_u64(9).unwrap())
                    .unwrap()
                    .build()
                    .unwrap();
                uncles.push(block.clone());
                chain.add_block_to_chain(block).unwrap();
            }
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(timestamp)
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .unwrap()
                .with_uncles(&uncles)
                .unwrap()
                .build()
                .unwrap();

            prev_block = Some(block.clone());

            chain.add_block_to_chain(block).unwrap();
        }

        let address = new_random_address();
        timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
        let mut uncles = Vec::new();
        let prev_uncle = chain.level_at_height(6).unwrap().block_in_main_chain().unwrap();
        // lets create an uncle block
        let block = P2BlockBuilder::new(Some(prev_uncle))
            .with_timestamp(timestamp)
            .with_height(7)
            .with_miner_wallet_address(address.clone())
            .with_target_difficulty(Difficulty::from_u64(10).unwrap())
            .unwrap()
            .build()
            .unwrap();
        uncles.push(block.clone());
        chain.add_block_to_chain(block).unwrap();
        prev_block = Some((*chain.level_at_height(7).unwrap().block_in_main_chain().unwrap()).clone());
        let block = P2BlockBuilder::new(prev_block.as_ref())
            .with_timestamp(timestamp)
            .with_height(8)
            .with_miner_wallet_address(address.clone())
            .with_target_difficulty(Difficulty::from_u64(11).unwrap())
            .unwrap()
            .with_uncles(&uncles)
            .unwrap()
            .build()
            .unwrap();
        let new_block = block.clone();

        chain.add_block_to_chain(block).unwrap();
        // lets create an uncle block
        let mut uncles = Vec::new();
        let block = P2BlockBuilder::new(prev_block.as_ref())
            .with_timestamp(timestamp)
            .with_height(8)
            .with_miner_wallet_address(address.clone())
            .with_target_difficulty(Difficulty::from_u64(10).unwrap())
            .unwrap()
            .build()
            .unwrap();
        uncles.push(block.clone());
        chain.add_block_to_chain(block).unwrap();
        let block = P2BlockBuilder::new(Some(&new_block))
            .with_timestamp(timestamp)
            .with_height(9)
            .with_miner_wallet_address(address.clone())
            .with_target_difficulty(Difficulty::from_u64(11).unwrap())
            .unwrap()
            .with_uncles(&uncles)
            .unwrap()
            .build()
            .unwrap();

        chain.add_block_to_chain(block).unwrap();
        let level = chain.get_tip().unwrap();
        assert_eq!(
            level.block_in_main_chain().unwrap().target_difficulty(),
            Difficulty::from_u64(11).unwrap()
        );
        assert_eq!(level.block_in_main_chain().unwrap().height, 9);
        assert_eq!(
            chain.total_accumulated_tip_difficulty(),
            AccumulatedDifficulty::from_u128(176).unwrap()
        );
    }

    #[test]
    fn rerog_less_than_share_window() {
        let mut chain = P2Chain::new_empty(20, 15, 20);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..10 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_target_difficulty(Difficulty::from_u64(9).unwrap())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            chain.add_block_to_chain(block.clone()).unwrap();

            let level = chain.get_tip().unwrap();
            assert_eq!(level.height, i);
            assert_eq!(level.block_in_main_chain().unwrap().original_header.nonce, i);
        }

        assert_eq!(chain.total_accumulated_tip_difficulty().as_u128(), 90);

        // lets create a new chain to reorg to
        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..10 {
            tari_block.header.nonce = i + 100;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            chain.add_block_to_chain(block.clone()).unwrap();

            let level = chain.get_tip().unwrap();

            assert_eq!(level.height, 9);
            if i < 9 {
                // less than 9 it has not reorged yet
                assert_eq!(level.block_in_main_chain().unwrap().original_header.nonce, 9);
            } else {
                // new tip, chain has reorged
                assert_eq!(level.block_in_main_chain().unwrap().original_header.nonce, 109);
            }
        }
        assert_eq!(chain.total_accumulated_tip_difficulty().as_u128(), 100);
    }

    #[test]
    fn rests_levels_after_reorg() {
        let mut chain = P2Chain::new_empty(20, 15, 20);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        for i in 0..10 {
            tari_block.header.nonce = i;
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(EpochTime::now())
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_target_difficulty(Difficulty::from_u64(9).unwrap())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            chain.add_block_to_chain(block.clone()).unwrap();

            let level = chain.get_tip().unwrap();
            assert_eq!(level.height, i);
            assert_eq!(level.block_in_main_chain().unwrap().original_header.nonce, i);
        }
        let level = chain.get_tip().unwrap();
        assert_eq!(level.height, 9);
        assert_eq!(chain.total_accumulated_tip_difficulty().as_u128(), 90);
        assert_eq!(chain.level_at_height(9).unwrap().chain_block, prev_block.unwrap().hash);

        // lets create a new tip to reorg to branching off 2 from the tip
        let prev_block = Some((*chain.level_at_height(7).unwrap().block_in_main_chain().unwrap()).clone());
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());

        tari_block.header.nonce = 100;
        let address = new_random_address();
        let block = P2BlockBuilder::new(prev_block.as_ref())
            .with_timestamp(EpochTime::now())
            .with_height(8)
            .with_tari_block(tari_block.clone())
            .unwrap()
            .with_target_difficulty(Difficulty::from_u64(100).unwrap())
            .unwrap()
            .with_miner_wallet_address(address.clone())
            .build()
            .unwrap();
        assert_eq!(chain.add_block_to_chain(block.clone()).unwrap().missing_blocks.len(), 0);

        let level = chain.get_tip().unwrap();
        assert_eq!(level.height, 8);
        assert_eq!(chain.total_accumulated_tip_difficulty().as_u128(), 172);
        assert_eq!(chain.level_at_height(9).unwrap().chain_block, FixedHash::default());
    }

    #[test]
    fn difficulty_go_up() {
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        let mut timestamp = EpochTime::now();
        let mut target_difficulty = Difficulty::min();

        for i in 0..30 {
            tari_block.header.nonce = i;
            timestamp = timestamp.checked_add(EpochTime::from(5)).unwrap();
            let prev_target_difficulty = target_difficulty;
            target_difficulty = chain
                .lwma
                .get_difficulty()
                .unwrap_or(Difficulty::from_u64(100000).unwrap());
            if i > 1 {
                assert!(target_difficulty > prev_target_difficulty);
            }
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(timestamp)
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(target_difficulty)
                .unwrap()
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            chain.add_block_to_chain(block.clone()).unwrap();

            let level = chain.get_tip().unwrap();
            assert_eq!(level.height, i);
            assert_eq!(level.block_in_main_chain().unwrap().original_header.nonce, i);
        }
    }
    #[test]
    fn difficulty_go_down() {
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let mut prev_block = None;
        let mut tari_block = Block::new(BlockHeader::new(0), AggregateBody::empty());
        let mut timestamp = EpochTime::now();
        let mut target_difficulty = Difficulty::min();

        for i in 0..30 {
            tari_block.header.nonce = i;
            timestamp = timestamp.checked_add(EpochTime::from(15)).unwrap();
            let prev_target_difficulty = target_difficulty;
            target_difficulty = chain
                .lwma
                .get_difficulty()
                .unwrap_or(Difficulty::from_u64(100000).unwrap());
            if i > 1 {
                assert!(target_difficulty < prev_target_difficulty);
            }
            let address = new_random_address();
            let block = P2BlockBuilder::new(prev_block.as_ref())
                .with_timestamp(timestamp)
                .with_height(i)
                .with_tari_block(tari_block.clone())
                .unwrap()
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(target_difficulty)
                .unwrap()
                .build()
                .unwrap();
            prev_block = Some(block.clone());
            chain.add_block_to_chain(block.clone()).unwrap();

            let level = chain.get_tip().unwrap();
            assert_eq!(level.height, i);
            assert_eq!(level.block_in_main_chain().unwrap().original_header.nonce, i);
        }
    }

    #[test]
    fn test_block_cannot_become_tip_if_missing_uncles() {
        // This test adds a block to the tip, and then adds second block,
        // but has an uncle that is not in the chain. This test checks that
        // the tip is not set to the new block, because the uncle is missing.
        let mut chain = P2Chain::new_empty(10, 5, 10);

        let prev_block = None;

        let block1 = P2BlockBuilder::new(prev_block.as_ref())
            .with_height(0)
            .with_target_difficulty(Difficulty::from_u64(10).unwrap())
            .unwrap()
            .build()
            .unwrap();
        chain.add_block_to_chain(block1.clone()).unwrap();

        assert_eq!(chain.current_tip, 0);
        let block1_uncle = P2BlockBuilder::new(prev_block.as_ref())
            .with_height(0)
            .with_target_difficulty(Difficulty::from_u64(9).unwrap())
            .unwrap()
            .build()
            .unwrap();

        let block2 = P2BlockBuilder::new(Some(&block1))
            .with_height(1)
            .with_uncles(&vec![block1_uncle.clone()])
            .unwrap()
            .build()
            .unwrap();
        chain.add_block_to_chain(block2).unwrap();
        // The tip should still be block 1 because block 2 is missing an uncle
        assert_eq!(chain.current_tip, 0);
    }

    #[test]
    fn test_only_reorg_to_chain_if_it_is_verified() {
        let mut chain = P2Chain::new_empty(10, 5, 10);
        let prev_block = None;

        let block = P2BlockBuilder::new(prev_block.as_ref())
            .with_height(0)
            .with_target_difficulty(Difficulty::from_u64(10).unwrap())
            .unwrap()
            .build()
            .unwrap();

        chain.add_block_to_chain(block.clone()).unwrap();
        let block2 = P2BlockBuilder::new(Some(&block))
            .with_height(1)
            .with_target_difficulty(Difficulty::from_u64(10).unwrap())
            .unwrap()
            .build()
            .unwrap();

        chain.add_block_to_chain(block2.clone()).unwrap();

        let missing_uncle = P2BlockBuilder::new(prev_block.as_ref())
            .with_height(0)
            .with_target_difficulty(diff(111))
            .unwrap()
            .build()
            .unwrap();

        let unverified_uncle = P2BlockBuilder::new(Some(&missing_uncle))
            .with_height(1)
            .with_target_difficulty(Difficulty::from_u64(100).unwrap())
            .unwrap()
            .build()
            .unwrap();

        let block2b = P2BlockBuilder::new(Some(&block))
            .with_height(1)
            .with_target_difficulty(Difficulty::from_u64(11).unwrap())
            .unwrap()
            .build()
            .unwrap();

        let block3b = P2BlockBuilder::new(Some(&block2b))
            .with_height(2)
            .with_target_difficulty(diff(100))
            .unwrap()
            .with_uncles(&vec![unverified_uncle.clone()])
            .unwrap()
            .build()
            .unwrap();

        assert_eq!(chain.current_tip, 1);
        assert_eq!(chain.get_tip().unwrap().chain_block, block2.hash);

        chain.add_block_to_chain(block3b).unwrap();

        // Check that we don't reorg
        assert_eq!(chain.current_tip, 1);
        assert_eq!(chain.get_tip().unwrap().chain_block, block2.hash);

        chain.add_block_to_chain(unverified_uncle).unwrap();

        // Now add block 2b
        chain.add_block_to_chain(block2b.clone()).unwrap();
        // But chain tip should not be 3b because it is not verified
        assert_eq!(chain.current_tip, 1);
        assert_eq!(chain.get_tip().unwrap().chain_block, block2b.hash);
    }

    fn diff(i: u64) -> Difficulty {
        Difficulty::from_u64(i).unwrap()
    }
}
