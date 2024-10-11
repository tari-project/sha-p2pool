// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::{HashMap, VecDeque},
    slice::Iter,
    str::FromStr,
    sync::Arc,
};

use async_trait::async_trait;
use log::*;
use minotari_app_grpc::tari_rpc::{pow_algo, NewBlockCoinbase, SubmitBlockRequest};
use num::{BigUint, Zero};
use tari_common_types::{tari_address::TariAddress, types::BlockHash};
use tari_core::{
    blocks,
    consensus::ConsensusManager,
    proof_of_work::{
        lwma_diff::LinearWeightedMovingAverage,
        randomx_difficulty,
        sha3x_difficulty,
        Difficulty,
        DifficultyAdjustment,
        PowAlgorithm,
    },
};
use tari_utilities::{epoch_time::EpochTime, hex::Hex};
use tokio::sync::{RwLock, RwLockWriteGuard};

use super::MAX_BLOCKS_COUNT;
use crate::{
    main,
    server::{
        grpc::{p2pool::min_difficulty, util::convert_coinbase_extra},
        p2p::Squad,
    },
    sharechain::{
        error::{BlockConvertError, Error},
        Block,
        BlockValidationParams,
        ShareChain,
        ShareChainResult,
        MAIN_CHAIN_SHARE_AMOUNT,
        MAX_SHARES_PER_MINER,
        MINER_REWARD_SHARES,
        SHARE_COUNT,
        UNCLE_BLOCK_SHARE_AMOUNT,
    },
};

const LOG_TARGET: &str = "tari::p2pool::sharechain::in_memory";

pub(crate) struct BlockLevels {
    cached_shares: Option<Vec<NewBlockCoinbase>>,
    levels: VecDeque<BlockLevel>,
}

impl BlockLevels {
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

pub(crate) struct InMemoryShareChain {
    max_blocks_count: usize,
    block_levels: Arc<RwLock<BlockLevels>>,
    pow_algo: PowAlgorithm,
    block_validation_params: Arc<BlockValidationParams>,
    consensus_manager: ConsensusManager,
    coinbase_extras: Arc<RwLock<HashMap<String, Vec<u8>>>>,
}

/// A collection of blocks with the same height.
pub struct BlockLevel {
    blocks: Vec<Block>,
    height: u64,
    in_chain_index: usize,
}

impl BlockLevel {
    pub fn new(blocks: Vec<Block>, height: u64) -> Self {
        Self {
            blocks,
            height,
            in_chain_index: 0,
        }
    }

    pub fn add_block(&mut self, block: Block, replace_tip: bool) -> Result<(), Error> {
        if self.height != block.height {
            return Err(Error::InvalidBlock(block));
        }
        if replace_tip {
            self.in_chain_index = self.blocks.len();
        }
        self.blocks.push(block);
        Ok(())
    }

    pub fn block_in_main_chain(&self) -> Option<&Block> {
        if self.in_chain_index < self.blocks.len() {
            Some(&self.blocks[self.in_chain_index])
        } else {
            None
        }
    }
}

fn genesis_block() -> Block {
    Block::builder()
        .with_height(0)
        .with_prev_hash(BlockHash::zero())
        .with_timestamp(EpochTime::from_secs_since_epoch(0))
        .with_specific_hash(BlockHash::zero())
        .build()
}

#[allow(dead_code)]
impl InMemoryShareChain {
    pub fn new(
        max_blocks_count: usize,
        pow_algo: PowAlgorithm,
        block_validation_params: Arc<BlockValidationParams>,
        consensus_manager: ConsensusManager,
        coinbase_extras: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    ) -> Result<Self, Error> {
        let mut levels = VecDeque::with_capacity(MAX_BLOCKS_COUNT + 1);
        levels.push_front(BlockLevel::new(vec![genesis_block()], 0));
        let block_levels = BlockLevels {
            cached_shares: None,
            levels,
        };
        Ok(Self {
            max_blocks_count,
            block_levels: Arc::new(RwLock::new(block_levels)),
            pow_algo,
            block_validation_params,
            consensus_manager,
            coinbase_extras,
        })
    }

    /// Calculates block difficulty based on it's pow algo.
    fn block_difficulty(&self, block: &Block) -> Result<u64, Error> {
        match block.original_block_header.pow.pow_algo {
            PowAlgorithm::RandomX => {
                let params = &self.block_validation_params;
                let difficulty = randomx_difficulty(
                    &block.original_block_header,
                    params.random_x_factory(),
                    params.genesis_block_hash(),
                    params.consensus_manager(),
                )
                .map_err(Error::RandomXDifficulty)?;
                Ok(difficulty.as_u64())
            },
            PowAlgorithm::Sha3x => {
                let difficulty = sha3x_difficulty(&block.original_block_header).map_err(Error::Difficulty)?;
                Ok(difficulty.as_u64())
            },
        }
    }

    /// Returns the current (strongest) chain
    fn chain(&self, block_level_iter: Iter<'_, BlockLevel>) -> Vec<Block> {
        let mut result = vec![];
        block_level_iter.for_each(|level| {
            level
                .blocks
                .iter()
                .max_by(|block1, block2| {
                    let diff1 = self.block_difficulty(block1).unwrap_or(0);
                    let diff2 = self.block_difficulty(block2).unwrap_or(0);
                    diff1.cmp(&diff2)
                })
                .iter()
                .copied()
                .for_each(|block| {
                    result.push(block.clone());
                });
        });

        result
    }

    /// Generating number of shares for all the miners.
    fn miners_with_shares(&self, chain: Vec<&Block>) -> HashMap<String, u64> {
        let mut result: HashMap<String, u64> = HashMap::new(); // target wallet address -> number of shares
        let mut extra_shares: HashMap<String, u64> = HashMap::new(); // target wallet address -> number of shares

        // add shares applying the max shares rule
        let mut added_share_count: u64 = 0;
        chain.iter().rev().for_each(|block| {
            if let Some(miner_wallet_address) = &block.miner_wallet_address {
                if added_share_count < SHARE_COUNT {
                    let addr = miner_wallet_address.to_base58();
                    match result.get(&addr) {
                        Some(curr_share_count) => {
                            if *curr_share_count < MAX_SHARES_PER_MINER {
                                result.insert(addr, curr_share_count + 1);
                                added_share_count += 1;
                            } else {
                                match extra_shares.get(&addr) {
                                    None => {
                                        extra_shares.insert(addr, 1);
                                    },
                                    Some(extra_share_count) => {
                                        extra_shares.insert(addr, extra_share_count + 1);
                                    },
                                }
                            }
                        },
                        None => {
                            result.insert(addr, 1);
                            added_share_count += 1;
                        },
                    }
                }
            }
        });

        result
    }

    fn validate_min_difficulty(
        &self,
        pow: PowAlgorithm,
        curr_difficulty: Difficulty,
        height: u64,
    ) -> ShareChainResult<bool> {
        if curr_difficulty < min_difficulty(&self.consensus_manager, pow, height) {
            warn!(target: LOG_TARGET, "[{:?}] ❌ Too low difficulty!", self.pow_algo);
            return Ok(false);
        }

        Ok(true)
    }

    /// Validating a new block.
    async fn validate_block(
        &self,
        last_block: Option<&Block>,
        block: &Block,
        achieved_difficulty: Difficulty,
        target_difficulty: Difficulty,
    ) -> ShareChainResult<()> {
        if block.original_block_header.pow.pow_algo != self.pow_algo {
            warn!(target: LOG_TARGET, "[{:?}] ❌ Pow algorithm mismatch! This share chain uses {:?}!", self.pow_algo, self.pow_algo);
            return Err(Error::WrongPowAlgorithm);
        }

        if let Some(last_block) = last_block {
            // check if we have outdated tip of chain
            let _block_height_diff = block
                .height
                .checked_sub(last_block.height)
                .ok_or(Error::BlockValidation(
                    "Block height is less than last block height".to_string(),
                ))?;
        }

        // validate PoW

        let pow_algo = block.original_block_header.pow.pow_algo;
        let result = self.validate_min_difficulty(pow_algo, achieved_difficulty, block.original_block_header.height)?;
        if !result {
            return Err(Error::BlockValidation(
                "Difficulty lower than minimum difficulty".to_string(),
            ));
        }
        if achieved_difficulty < target_difficulty {
            warn!(target: LOG_TARGET, "[{:?}] ❌ Difficulty too low!", pow_algo);
            return Err(Error::BlockValidation("Difficulty too low".to_string()));
        }

        Ok(())
    }

    /// Submits a new block to share chain.
    #[allow(clippy::too_many_lines)]
    async fn submit_block_with_lock(
        &self,
        block_levels: &mut RwLockWriteGuard<'_, BlockLevels>,
        block: &Block,
        params: &BlockValidationParams,
    ) -> ShareChainResult<()> {
        let height = block.height;
        if block.height == 0 {
            // TODO: Find out where this block 0 is coming from
            return Ok(());
            // return Err(Error::BlockValidation("Block height is 0".to_string()));
        }

        let min_diff = min_difficulty(
            &self.consensus_manager,
            block.original_block_header.pow.pow_algo,
            block.original_block_header.height,
        );
        let pow_algo = block.original_block_header.pow.pow_algo;
        let achieved_difficulty = match pow_algo {
            PowAlgorithm::RandomX => randomx_difficulty(
                &block.original_block_header,
                params.random_x_factory(),
                params.genesis_block_hash(),
                params.consensus_manager(),
            )
            .map_err(Error::RandomXDifficulty),
            PowAlgorithm::Sha3x => sha3x_difficulty(&block.original_block_header).map_err(Error::Difficulty),
        }?;

        if achieved_difficulty < min_diff {
            warn!(target: LOG_TARGET, "[{:?}] ❌ Too low difficulty!", self.pow_algo);
            return Err(Error::BlockValidation("Block difficulty is too low".to_string()));
        }

        if block_levels.levels.is_empty() {
            // TODO: Validate the block
            block_levels
                .levels
                .push_front(BlockLevel::new(vec![block.clone()], block.height));
            return Ok(());
        }

        let block_levels_len = block_levels.levels.len();

        let tip_level = block_levels.levels.front().ok_or_else(|| Error::Empty)?;
        let tip_height = tip_level.height;

        dbg!(block.height);
        dbg!(tip_height);
        // If we are syncing and we are very far behind, clear out the blocks we have and just add it
        if tip_height < block.height.saturating_sub(self.max_blocks_count as u64) {
            // TODO: Validate the block
            block_levels.levels.clear();
            block_levels
                .levels
                .push_front(BlockLevel::new(vec![block.clone()], block.height));
            return Ok(());
        }

        if tip_height == 0 {
            // TODO: Validate the block
            block_levels.levels.clear();
            block_levels
                .levels
                .push_front(BlockLevel::new(vec![block.clone()], block.height));
            return Ok(());
        }

        // Else if there is a gap in the chain, snooze it in the hopes that we will catch up
        if tip_height < block.height.saturating_sub(1) {
            // TODO: Validate the block
            return Err(Error::BlockParentDoesNotExist {
                num_missing_parents: block.height - tip_height,
            });
        }

        // Check if already added.
        if let Some(level) = block_levels.get_at_height(height) {
            if level.blocks.iter().any(|b| b.hash == block.hash) {
                info!(target: LOG_TARGET, "[{:?}] ✅ Block already added: {:?}", self.pow_algo, block.height);
                return Ok(());
            }
        }

        // Find the parent.
        let parent_height = height
            .checked_sub(1)
            .ok_or_else(|| Error::BlockValidation("Block height is 0".to_string()))?;
        let level_index = usize::try_from(tip_height.checked_sub(parent_height).ok_or_else(|| {
            Error::BlockValidation(format!(
                "Block parent height is not stored in levels. parent: {}, tip: {}",
                parent_height, tip_height
            ))
        })?)
        .expect("32 bit systems are not supported");
        let parent_level = block_levels.levels.get(level_index).ok_or_else(|| {
            Error::BlockValidation(format!(
                "Block parent height is not found in levels. index: {},  parent height:{}, levels size: {}",
                level_index, parent_height, block_levels_len
            ))
        })?;

        let parent = parent_level
            .blocks
            .iter()
            .find(|b| block.prev_hash == b.hash)
            .ok_or_else(|| {
                Error::BlockValidation(format!(
                    "Block parent is not found in levels. parent height: {}, parent hash: {}",
                    parent_height,
                    block.prev_hash.to_hex()
                ))
            })?;

        // note: this is an approximation, wait for actual implementation to be accurate.
        let mut lwma = LinearWeightedMovingAverage::new(90, 10).expect("Failed to create LWMA");
        for level in block_levels.levels.iter().skip(20) {
            let main_block = level
                .block_in_main_chain()
                .ok_or_else(|| Error::BlockValidation(format!("No main block in level: {:?}", level.height)))?;
            lwma.add_front(main_block.timestamp, main_block.target_difficulty);
        }
        let target_difficulty = lwma.get_difficulty().unwrap_or_else(|| {
            warn!(target: LOG_TARGET, "[{:?}] Failed to calculate target difficulty", self.pow_algo);
            min_diff
        });
        // todo!("Save difficulty");
        // validate
        self.validate_block(Some(parent), block, achieved_difficulty, target_difficulty)
            .await?;

        let mut num_reorged = 0;
        // debug!(target: LOG_TARGET, "Reorging to main chain");
        // parent_level.in_chain_index = parent_index;
        let mut current_block_hash = block.prev_hash;

        // recalculate levels.
        for p_i in (level_index)..block_levels.levels.len() {
            let curr_level = block_levels.levels.get_mut(p_i).ok_or_else(|| {
                Error::BlockValidation(format!(
                    "Block parent height is not found in levels. index: {}, levels length: {}. parent height:{}",
                    level_index, block_levels_len, parent_height
                ))
            })?;
            let (new_index, new_parent) = curr_level
                .blocks
                .iter()
                .enumerate()
                .find(|(_i, b)| b.hash == current_block_hash)
                .ok_or_else(|| {
                    Error::BlockValidation(format!(
                        "Block parent is not found in levels. parent height: {}, parent hash: {}",
                        parent_height,
                        block.prev_hash.to_hex()
                    ))
                })?;
            if curr_level.in_chain_index == new_index {
                break;
            }
            num_reorged += 1;

            curr_level.in_chain_index = new_index;
            current_block_hash = new_parent.prev_hash;
        }

        if num_reorged > 0 {
            info!(target: LOG_TARGET, "[{:?}] 🔄 Reorged {} blocks", self.pow_algo, num_reorged);
        }
        block_levels.cached_shares = None;
        // remove the first couple of block levels if needed
        if block_levels.levels.len() >= self.max_blocks_count {
            let diff = block_levels.levels.len() - self.max_blocks_count;
            for _ in 0..diff {
                if let Some(level) = block_levels.levels.pop_back() {
                    debug!(target: LOG_TARGET, "[{:?}] 🗑️ Removed block level: {:?}", self.pow_algo, level.height);
                } else {
                    error!(target: LOG_TARGET, "[{:?}] Failed to remove block level!", self.pow_algo);
                }
            }
        }

        // Add the block.
        if level_index == 0 {
            block_levels
                .levels
                .push_front(BlockLevel::new(vec![block.clone()], block.height));
        } else {
            block_levels
                .levels
                .get_mut(level_index.checked_sub(1).unwrap())
                .unwrap()
                .add_block(block.clone(), true)?;
        }

        // update coinbase extra cache
        let mut coinbase_extras_lock = self.coinbase_extras.write().await;
        if let Some(miner_wallet_address) = &block.miner_wallet_address {
            coinbase_extras_lock.insert(miner_wallet_address.to_base58(), block.miner_coinbase_extra.clone());
        }

        info!(target: LOG_TARGET, "[{:?}] ✅ Block added: {:?} Tip is now {}", self.pow_algo, block.height, block_levels.levels.front().map(|b| b.height).unwrap_or_default());

        Ok(())
    }

    async fn find_coinbase_extra(&self, miner_wallet_address: TariAddress) -> Option<Vec<u8>> {
        let coinbase_extras_lock = self.coinbase_extras.read().await;
        if let Some(found_coinbase_extras) = coinbase_extras_lock.get(&miner_wallet_address.to_base58()) {
            return Some(found_coinbase_extras.clone());
        }

        None
    }
}

#[async_trait]
impl ShareChain for InMemoryShareChain {
    async fn submit_block(&self, block: &Block) -> ShareChainResult<()> {
        let mut block_levels_write_lock = self.block_levels.write().await;
        self.submit_block_with_lock(&mut block_levels_write_lock, block, &self.block_validation_params)
            .await

        // let chain = self.chain(block_levels_write_lock.iter());
        // let last_block = chain.last().ok_or_else(|| Error::Empty)?;
        // info!(target: LOG_TARGET, "[{:?}] ⬆️ Current height: {:?}", self.pow_algo, last_block.height);
    }

    async fn add_synced_blocks(&self, blocks: Vec<Block>) -> ShareChainResult<()> {
        let mut block_levels_write_lock = self.block_levels.write().await;

        for block in blocks {
            self.submit_block_with_lock(&mut block_levels_write_lock, &block, &self.block_validation_params)
                .await?;
        }
        Ok(())
    }

    async fn tip_height(&self) -> ShareChainResult<u64> {
        let bl = self.block_levels.read().await;
        let tip_level = bl.levels.front().map(|b| b.height).unwrap_or_default();
        Ok(tip_level)
    }

    async fn generate_shares(&self, squad: Squad) -> Vec<NewBlockCoinbase> {
        let bl = self.block_levels.read().await;
        if let Some(ref cached_shares) = bl.cached_shares {
            return cached_shares.clone();
        }

        drop(bl);

        // Lock for writing so that it doesn't change during calculating
        let mut bl = self.block_levels.write().await;

        // There may be two threads that reach here, so double check if the shares are already calculated
        if let Some(ref cached_shares) = bl.cached_shares {
            return cached_shares.clone();
        }

        // Otherwise, let's calculate the shares
        let mut shares_left = SHARE_COUNT;

        let mut res = vec![];

        // Subtract shares for miner reward.
        shares_left -= MINER_REWARD_SHARES;

        let mut miners_to_shares = HashMap::new();

        for level in &bl.levels {
            let main_block_index = level.in_chain_index;

            // Main blocks
            if let Some(main_block) = level.block_in_main_chain() {
                let miner_address = main_block.miner_wallet_address.clone();
                if miner_address.is_some() {
                    let entry = miners_to_shares.entry(miner_address.unwrap().to_base58()).or_insert(0);
                    *entry += MAIN_CHAIN_SHARE_AMOUNT;
                }
            } else {
                error!(target: LOG_TARGET, "No main block in level: {:?}", level.height);
            }
            shares_left = shares_left.saturating_sub(MAIN_CHAIN_SHARE_AMOUNT);
            if shares_left == 0 {
                break;
            }

            // Only pay out uncles if there are shares left for all of them
            if shares_left < UNCLE_BLOCK_SHARE_AMOUNT * (level.blocks.len() - 1) as u64 {
                warn!(target: LOG_TARGET, "Not enough shares left for uncles! Shares left: {:?} level: {}", shares_left, level.height);
                break;
            }

            // Uncle blocks
            for (i, block) in level.blocks.iter().enumerate() {
                if i == main_block_index {
                    continue;
                }
                let miner_wallet_address = block.miner_wallet_address.clone();
                if miner_wallet_address.is_none() {
                    continue;
                }
                let addr = miner_wallet_address.unwrap().to_base58();
                let share_count = miners_to_shares.entry(addr).or_insert(0);
                *share_count += UNCLE_BLOCK_SHARE_AMOUNT;
                shares_left = shares_left.saturating_sub(UNCLE_BLOCK_SHARE_AMOUNT);
            }
            if shares_left == 0 {
                break;
            }
        }

        for (key, value) in miners_to_shares {
            // find coinbase extra for wallet address
            let mut coinbase_extra = convert_coinbase_extra(squad.clone(), String::new()).unwrap_or_default();
            if let Ok(miner_wallet_address) = TariAddress::from_str(key.as_str()) {
                if let Some(coinbase_extra_found) = self.find_coinbase_extra(miner_wallet_address).await {
                    coinbase_extra = coinbase_extra_found;
                }
            }

            res.push(NewBlockCoinbase {
                address: key,
                value,
                stealth_payment: false,
                revealed_value_proof: true,
                coinbase_extra,
            });
        }

        bl.cached_shares = Some(res.clone());

        res
    }

    async fn new_block(&self, request: &SubmitBlockRequest, squad: Squad) -> ShareChainResult<Block> {
        let origin_block_grpc = request
            .block
            .as_ref()
            .ok_or_else(|| BlockConvertError::MissingField("block".to_string()))?;

        let origin_block =
            blocks::Block::try_from(origin_block_grpc.clone()).map_err(BlockConvertError::GrpcBlockConvert)?;

        // get current share chain
        let block_levels_read_lock = self.block_levels.read().await;
        let last_block = block_levels_read_lock
            .levels
            .front()
            .ok_or_else(|| Error::Empty)?
            .block_in_main_chain()
            .ok_or_else(|| Error::Empty)?;

        let miner_wallet_address =
            TariAddress::from_str(request.wallet_payment_address.as_str()).map_err(Error::TariAddress)?;

        // coinbase extra
        let coinbase_extra =
            if let Some(found_coinbase_extra) = self.find_coinbase_extra(miner_wallet_address.clone()).await {
                found_coinbase_extra.clone()
            } else {
                convert_coinbase_extra(squad, String::new())?
            };

        Ok(Block::builder()
            .with_timestamp(EpochTime::now())
            .with_prev_hash(last_block.hash)
            .with_height(last_block.height + 1)
            .with_miner_coinbase_extra(coinbase_extra)
            .with_original_block_header(origin_block.header.clone())
            .with_miner_wallet_address(miner_wallet_address)
            .build())
    }

    async fn blocks(&self, from_height: u64) -> ShareChainResult<Vec<Block>> {
        // Should really only be used in syncing
        let block_levels_read_lock = self.block_levels.read().await;
        let mut res = vec![];
        if block_levels_read_lock.levels.is_empty() {
            return Ok(res);
        }

        if block_levels_read_lock.levels.front().unwrap().height < from_height {
            return Ok(res);
        }

        for level in &block_levels_read_lock.levels {
            if level.height < from_height {
                break;
            }
            if let Some(block) = level.block_in_main_chain() {
                res.push(block.clone());
            }
        }

        res.reverse();
        Ok(res)
    }

    async fn hash_rate(&self) -> ShareChainResult<BigUint> {
        Ok(BigUint::zero())
        // TODO: This calc is wrong
        // let block_levels = self.block_levels.read().await;
        // if block_levels.is_empty() {
        //     return Ok(BigUint::zero());
        // }

        // let blocks = block_levels
        //     .iter()
        //     .flat_map(|level| level.blocks.clone())
        //     .sorted_by(|block1, block2| block1.timestamp.cmp(&block2.timestamp))
        //     .tail(BLOCKS_WINDOW);

        // // calculate average block time
        // let blocks = blocks.collect_vec();
        // let mut block_times_sum = 0;
        // let mut block_times_count: u64 = 0;
        // for i in 0..blocks.len() {
        //     let current_block = blocks.get(i);
        //     let next_block = blocks.get(i + 1);
        //     if let Some(current_block) = current_block {
        //         if let Some(next_block) = next_block {
        //             block_times_sum += next_block.timestamp.as_u64() - current_block.timestamp.as_u64();
        //             block_times_count += 1;
        //         }
        //     }
        // }

        // // return to avoid division by zero
        // if block_times_sum == 0 || block_times_count == 0 {
        //     return Ok(BigUint::zero());
        // }

        // let avg_block_time: f64 = (block_times_sum / block_times_count) as f64;

        // // collect all hash rates
        // let mut hash_rates_sum = BigUint::zero();
        // let mut hash_rates_count = BigUint::zero();
        // for block in blocks {
        //     let difficulty = self.block_difficulty(&block)?;
        //     let current_hash_rate_f64 = difficulty as f64 / avg_block_time;
        //     let current_hash_rate =
        //         u64::from_f64(current_hash_rate_f64).ok_or(Error::FromF64ToU64Conversion(current_hash_rate_f64))?;
        //     hash_rates_sum = hash_rates_sum.add(current_hash_rate);
        //     hash_rates_count.inc();
        // }

        // Ok(hash_rates_sum.div(hash_rates_count))
    }

    async fn miners_with_shares(&self, squad: Squad) -> ShareChainResult<HashMap<String, u64>> {
        let bl = self.block_levels.read().await;
        if let Some(ref shares) = bl.cached_shares {
            return Ok(shares.iter().map(|s| (s.address.clone(), s.value)).collect());
        }

        drop(bl);
        let shares = self.generate_shares(squad).await;
        Ok(shares.iter().map(|s| (s.address.clone(), s.value)).collect())
    }
}

#[cfg(test)]
mod test {
    use std::assert_matches::assert_matches;

    use tari_common::configuration::Network;
    use tari_core::proof_of_work::randomx_factory::RandomXFactory;

    use super::*;

    #[tokio::test]
    async fn test_submit_block() {
        // execute
        let consensus_manager = ConsensusManager::builder(Network::get_current_or_user_setting_or_default())
            .build()
            .unwrap();
        let coinbase_extras = Arc::new(RwLock::new(HashMap::<String, Vec<u8>>::new()));
        let params = Arc::new(BlockValidationParams::new(
            RandomXFactory::new(1),
            consensus_manager.clone(),
            *consensus_manager.get_genesis_block().hash(),
        ));
        let share_chain =
            InMemoryShareChain::new(20, PowAlgorithm::Sha3x, params, consensus_manager, coinbase_extras).unwrap();

        let block = Block::builder().with_height(355).build();

        share_chain.submit_block(&block).await.unwrap();

        assert_eq!(share_chain.tip_height().await.unwrap(), 355);
        assert_eq!(share_chain.block_levels.read().await.levels.len(), 1);

        let block2 = Block::builder()
            .with_height(376)
            .with_prev_hash(block.hash.clone())
            .build();

        // Because this is higher than the MAX_BLOCKS_COUNT, the first block should be removed
        share_chain.submit_block(&block2).await.unwrap();

        assert_eq!(share_chain.tip_height().await.unwrap(), 376);
        assert_eq!(share_chain.block_levels.read().await.levels.len(), 1);

        let block3 = Block::builder()
            .with_height(377)
            .with_prev_hash(block2.hash.clone())
            .build();

        // This should be added fine, since there is a direct parent.
        share_chain.submit_block(&block3).await.unwrap();

        assert_eq!(share_chain.tip_height().await.unwrap(), 377);
        assert_eq!(share_chain.block_levels.read().await.levels.len(), 2);

        // Add a gap
        let block4 = Block::builder()
            .with_height(378)
            .with_prev_hash(block3.hash.clone())
            .build();

        let block5 = Block::builder()
            .with_height(379)
            .with_prev_hash(block4.hash.clone())
            .build();

        // This should fail, since there is a gap, but tell us to snooze
        let res = share_chain.submit_block(&block5).await;
        assert_matches!(res.unwrap_err(), Error::BlockParentDoesNotExist {
            num_missing_parents: 2
        });

        share_chain.submit_block(&block4).await.unwrap();
        share_chain.submit_block(&block5).await.unwrap();

        assert_eq!(share_chain.tip_height().await.unwrap(), 379);
        assert_eq!(share_chain.block_levels.read().await.levels.len(), 4);
    }
}
