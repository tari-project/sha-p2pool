// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::{HashMap, VecDeque},
    slice::Iter,
    str::FromStr,
    sync::Arc,
};

use async_trait::async_trait;
use digest::crypto_common::rand_core::block;
use itertools::{enumerate, Itertools};
use log::*;
use minotari_app_grpc::tari_rpc::{NewBlockCoinbase, SubmitBlockRequest};
use num::{BigUint, Saturating, Zero};
use tari_common_types::{tari_address::TariAddress, types::BlockHash};
use tari_core::{
    blocks,
    consensus::ConsensusManager,
    proof_of_work::{randomx_difficulty, sha3x_difficulty, Difficulty, PowAlgorithm},
};
use tari_utilities::{epoch_time::EpochTime, hex::Hex};
use tokio::sync::{RwLock, RwLockWriteGuard};

use super::MAX_BLOCKS_COUNT;
use crate::{
    server::grpc::p2pool::min_difficulty,
    sharechain::{
        error::{BlockConvertError, Error},
        Block,
        BlockValidationParams,
        ShareChain,
        ShareChainResult,
        SubmitBlockResult,
        ValidateBlockResult,
        BLOCKS_WINDOW,
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

pub(crate) struct InMemoryShareChain {
    max_blocks_count: usize,
    block_levels: Arc<RwLock<BlockLevels>>,
    pow_algo: PowAlgorithm,
    block_validation_params: Option<Arc<BlockValidationParams>>,
    consensus_manager: ConsensusManager,
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
        .build()
}

#[allow(dead_code)]
impl InMemoryShareChain {
    pub fn new(
        max_blocks_count: usize,
        pow_algo: PowAlgorithm,
        block_validation_params: Option<Arc<BlockValidationParams>>,
        consensus_manager: ConsensusManager,
    ) -> Result<Self, Error> {
        if pow_algo == PowAlgorithm::RandomX && block_validation_params.is_none() {
            return Err(Error::MissingBlockValidationParams);
        }
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
        })
    }

    /// Calculates block difficulty based on it's pow algo.
    fn block_difficulty(&self, block: &Block) -> Result<u64, Error> {
        match block.original_block_header.pow.pow_algo {
            PowAlgorithm::RandomX => {
                if let Some(params) = &self.block_validation_params {
                    let difficulty = randomx_difficulty(
                        &block.original_block_header,
                        params.random_x_factory(),
                        params.genesis_block_hash(),
                        params.consensus_manager(),
                    )
                    .map_err(Error::RandomXDifficulty)?;
                    Ok(difficulty.as_u64())
                } else {
                    panic!("No params provided for RandomX difficulty calculation!");
                    // Ok(0)
                }
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
    ) -> ShareChainResult<ValidateBlockResult> {
        if curr_difficulty < min_difficulty(&self.consensus_manager, pow, height) {
            warn!(target: LOG_TARGET, "[{:?}] ❌ Too low difficulty!", self.pow_algo);
            return Ok(ValidateBlockResult::new(false, false));
        }

        Ok(ValidateBlockResult::new(true, false))
    }

    /// Validating a new block.
    async fn validate_block(
        &self,
        last_block: Option<&Block>,
        block: &Block,
        params: Option<Arc<BlockValidationParams>>,
        sync: bool,
    ) -> ShareChainResult<ValidateBlockResult> {
        if block.original_block_header.pow.pow_algo != self.pow_algo {
            warn!(target: LOG_TARGET, "[{:?}] ❌ Pow algorithm mismatch! This share chain uses {:?}!", self.pow_algo, self.pow_algo);
            return Ok(ValidateBlockResult::new(false, false));
        }

        if sync && last_block.is_none() {
            return Ok(ValidateBlockResult::new(true, false));
        }

        if let Some(last_block) = last_block {
            // check if we have outdated tip of chain
            let block_height_diff = i64::try_from(block.height).map_err(Error::FromIntConversion)? -
                i64::try_from(last_block.height).map_err(Error::FromIntConversion)?;
            if block_height_diff > 10 {
                // TODO: use const
                warn!(target: LOG_TARGET,
                    "[{:?}] Out-of-sync chain, do a sync now... Height Diff: {:?}, Last: {:?}, New: {:?}",
                    self.pow_algo,
                    block_height_diff,
                    last_block.height,
                    block.height,
                );
                return Ok(ValidateBlockResult::new(false, true));
            }

            // validate PoW
            let pow_algo = block.original_block_header.pow.pow_algo;
            let curr_difficulty = match pow_algo {
                PowAlgorithm::RandomX => {
                    let random_x_params = params.ok_or(Error::MissingBlockValidationParams)?;
                    randomx_difficulty(
                        &block.original_block_header,
                        random_x_params.random_x_factory(),
                        random_x_params.genesis_block_hash(),
                        random_x_params.consensus_manager(),
                    )
                    .map_err(Error::RandomXDifficulty)
                },
                PowAlgorithm::Sha3x => sha3x_difficulty(&block.original_block_header).map_err(Error::Difficulty),
            };
            match curr_difficulty {
                Ok(curr_difficulty) => {
                    let result =
                        self.validate_min_difficulty(pow_algo, curr_difficulty, block.original_block_header.height)?;
                    if !result.valid {
                        return Ok(result);
                    }
                },
                Err(error) => {
                    warn!(target: LOG_TARGET, "[{:?}] ❌ Invalid PoW!", pow_algo);
                    debug!(target: LOG_TARGET, "[{:?}] Failed to calculate {} difficulty: {error:?}", pow_algo,pow_algo);
                    return Ok(ValidateBlockResult::new(false, false));
                },
            }

            // TODO: check here for miners
            // TODO: (send merkle tree root hash and generate here, then compare the two from miners list and shares)
        } else {
            return Ok(ValidateBlockResult::new(false, true));
        }

        Ok(ValidateBlockResult::new(true, false))
    }

    /// Submits a new block to share chain.
    async fn submit_block_with_lock(
        &self,
        block_levels: &mut RwLockWriteGuard<'_, BlockLevels>,
        block: &Block,
        params: Option<Arc<BlockValidationParams>>,
        sync: bool,
    ) -> ShareChainResult<SubmitBlockResult> {
        let height = block.height;

        if block_levels.levels.is_empty() {
            // TODO: Validate the block
            block_levels
                .levels
                .push_front(BlockLevel::new(vec![block.clone()], block.height));
            return Ok(SubmitBlockResult::new(false));
        }
        let block_levels_len = block_levels.levels.len();

        let tip_level = block_levels.levels.front().ok_or_else(|| Error::Empty)?;
        let tip_height = tip_level.height;

        // Find the parent.
        let parent_height = height
            .checked_sub(1)
            .ok_or_else(|| Error::BlockValidation("Block height is 0".to_string()))?;
        let level_index = tip_height.checked_sub(parent_height).ok_or_else(|| {
            Error::BlockValidation(format!(
                "Block parent height is not stored in levels. parent: {}, tip: {}",
                parent_height, tip_height
            ))
        })? as usize;
        let parent_level = block_levels.levels.get(level_index as usize).ok_or_else(|| {
            Error::BlockValidation(format!(
                "Block parent height is not found in levels. index: {},  parent height:{}",
                level_index, parent_height
            ))
        })?;

        let (parent_index, parent) = parent_level
            .blocks
            .iter()
            .enumerate()
            .find(|(i, b)| block.prev_hash == b.hash)
            .ok_or_else(|| {
                Error::BlockValidation(format!(
                    "Block parent is not found in levels. parent height: {}, parent hash: {}",
                    parent_height,
                    block.prev_hash.to_hex()
                ))
            })?;

        // validate
        let validate_result = self.validate_block(Some(parent), block, params, sync).await?;
        if !validate_result.valid {
            return if validate_result.need_sync {
                Ok(SubmitBlockResult::new(true))
            } else {
                Err(Error::InvalidBlock(block.clone()))
            };
        }

        debug!(target: LOG_TARGET, "Reorging to main chain");
        // parent_level.in_chain_index = parent_index;
        let mut current_block_hash = block.prev_hash.clone();

        // recalculate levels.
        for p_i in (level_index)..block_levels.levels.len() {
            let curr_level = block_levels.levels.get_mut(p_i as usize).ok_or_else(|| {
                Error::BlockValidation(format!(
                    "Block parent height is not found in levels. index: {}, levels length: {}. parent height:{}",
                    level_index, block_levels_len, parent_height
                ))
            })?;
            let (new_index, new_parent) = curr_level
                .blocks
                .iter()
                .enumerate()
                .find(|(i, b)| b.hash == current_block_hash)
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

            curr_level.in_chain_index = new_index;
            current_block_hash = new_parent.prev_hash.clone();
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
                .get_mut(level_index.checked_sub(1).unwrap() as usize)
                .unwrap()
                .add_block(block.clone(), true)?;
        }

        Ok(SubmitBlockResult::new(validate_result.need_sync))
    }
}

#[async_trait]
impl ShareChain for InMemoryShareChain {
    async fn submit_block(&self, block: &Block) -> ShareChainResult<SubmitBlockResult> {
        let mut block_levels_write_lock = self.block_levels.write().await;
        self.submit_block_with_lock(
            &mut block_levels_write_lock,
            block,
            self.block_validation_params.clone(),
            false,
        )
        .await

        // let chain = self.chain(block_levels_write_lock.iter());
        // let last_block = chain.last().ok_or_else(|| Error::Empty)?;
        // info!(target: LOG_TARGET, "[{:?}] ⬆️ Current height: {:?}", self.pow_algo, last_block.height);
    }

    async fn submit_blocks(&self, blocks: Vec<Block>, sync: bool) -> ShareChainResult<SubmitBlockResult> {
        let mut block_levels_write_lock = self.block_levels.write().await;

        for block in blocks {
            let result = self
                .submit_block_with_lock(
                    &mut block_levels_write_lock,
                    &block,
                    self.block_validation_params.clone(),
                    sync,
                )
                .await?;
            if result.need_sync {
                return Ok(SubmitBlockResult::new(true));
            }
        }
        Ok(SubmitBlockResult::new(false))
    }

    async fn tip_height(&self) -> ShareChainResult<u64> {
        let bl = self.block_levels.read().await;
        let tip_level = bl.levels.front().map(|b| b.height).unwrap_or_default();
        Ok(tip_level)
    }

    async fn generate_shares(&self) -> Vec<NewBlockCoinbase> {
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
                if !miner_address.is_none() {
                    let entry = miners_to_shares.entry(miner_address.unwrap().to_base58()).or_insert(0);
                    *entry += MAIN_CHAIN_SHARE_AMOUNT;
                }
            } else {
                error!(target: LOG_TARGET, "No main block in level: {:?}", level.height);
            }
            shares_left = shares_left.saturating_sub(MAIN_CHAIN_SHARE_AMOUNT);
            if shares_left <= 0 {
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
            if shares_left <= 0 {
                break;
            }
        }

        for (key, value) in miners_to_shares {
            res.push(NewBlockCoinbase {
                address: key,
                value,
                stealth_payment: false,
                revealed_value_proof: true,
                coinbase_extra: vec![],
            });
        }

        bl.cached_shares = Some(res.clone());

        res
    }

    async fn new_block(&self, request: &SubmitBlockRequest) -> ShareChainResult<Block> {
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

        Ok(Block::builder()
            .with_timestamp(EpochTime::now())
            .with_prev_hash(last_block.generate_hash())
            .with_height(last_block.height + 1)
            .with_original_block_header(origin_block.header.clone())
            .with_miner_wallet_address(
                TariAddress::from_str(request.wallet_payment_address.as_str()).map_err(Error::TariAddress)?,
            )
            .build())
    }

    async fn blocks(&self, from_height: u64) -> ShareChainResult<Vec<Block>> {
        // Should really only be used in syncing
        let block_levels_read_lock = self.block_levels.read().await;
        let mut res = vec![];
        if block_levels_read_lock.levels.is_empty() {
            return Ok(res);
        }

        if block_levels_read_lock.levels.front().unwrap().height <= from_height {
            return Ok(res);
        }

        for level in block_levels_read_lock.levels.iter() {
            if level.height <= from_height {
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

    async fn miners_with_shares(&self) -> ShareChainResult<HashMap<String, u64>> {
        let bl = self.block_levels.read().await;
        if let Some(ref shares) = bl.cached_shares {
            return Ok(shares.iter().map(|s| (s.address.clone(), s.value)).collect());
        }

        drop(bl);
        let shares = self.generate_shares().await;
        Ok(shares.iter().map(|s| (s.address.clone(), s.value)).collect())
    }
}

#[cfg(test)]
mod test {
    use tari_common::configuration::Network;
    use tari_common_types::tari_address::TariAddressFeatures;
    use tari_crypto::{keys::PublicKey, ristretto::RistrettoPublicKey};

    use super::*;
    use crate::sharechain::MAX_BLOCKS_COUNT;

    fn new_random_address() -> TariAddress {
        let mut rng = rand::thread_rng();
        let (_, pk) = RistrettoPublicKey::random_keypair(&mut rng);
        TariAddress::new_single_address(pk, Network::LocalNet, TariAddressFeatures::INTERACTIVE)
    }

    fn add_blocks(blocks: &mut Vec<Block>, miner: TariAddress, start: u64, n: u64) -> u64 {
        let n = start + n;
        for i in start..n {
            blocks.push(
                Block::builder()
                    .with_height(i)
                    .with_miner_wallet_address(miner.clone())
                    .build(),
            );
        }
        n
    }

    #[ignore]
    #[test]
    fn miners_with_shares_no_outperformers() {
        // setup blocks and miners
        let mut blocks = Vec::new();
        let miner1 = new_random_address();
        let n = add_blocks(&mut blocks, miner1.clone(), 0, 10);
        let miner2 = new_random_address();
        let n = add_blocks(&mut blocks, miner2.clone(), n, 20);
        let miner3 = new_random_address();
        let n = add_blocks(&mut blocks, miner3.clone(), n, 30);
        let miner4 = new_random_address();
        add_blocks(&mut blocks, miner4.clone(), n, 40);

        // execute
        let blocks = blocks.iter().collect_vec();
        let consensus_manager = ConsensusManager::builder(Network::get_current_or_user_setting_or_default())
            .build()
            .unwrap();
        let share_chain =
            InMemoryShareChain::new(MAX_BLOCKS_COUNT, PowAlgorithm::Sha3x, None, consensus_manager).unwrap();
        let shares = share_chain.miners_with_shares(blocks);

        // assert
        assert_eq!(*shares.get(&miner1.to_base58()).unwrap(), 10);
        assert_eq!(*shares.get(&miner2.to_base58()).unwrap(), 20);
        assert_eq!(*shares.get(&miner3.to_base58()).unwrap(), 30);
        assert_eq!(*shares.get(&miner4.to_base58()).unwrap(), 40);
    }

    #[ignore]
    #[test]
    fn miners_with_shares_with_outperformer_dont_fill_remaining() {
        // setup blocks and miners
        let mut blocks = Vec::new();
        let miner1 = new_random_address();
        let n = add_blocks(&mut blocks, miner1.clone(), 0, 60);
        let miner2 = new_random_address();
        let n = add_blocks(&mut blocks, miner2.clone(), n, 60);
        let miner3 = new_random_address();
        let n = add_blocks(&mut blocks, miner3.clone(), n, 80);
        let miner4 = new_random_address();
        add_blocks(&mut blocks, miner4.clone(), n, 10);

        // execute
        let blocks = blocks.iter().collect_vec();
        let consensus_manager = ConsensusManager::builder(Network::get_current_or_user_setting_or_default())
            .build()
            .unwrap();
        let share_chain =
            InMemoryShareChain::new(MAX_BLOCKS_COUNT, PowAlgorithm::Sha3x, None, consensus_manager).unwrap();
        let shares = share_chain.miners_with_shares(blocks);

        // assert
        assert_eq!(*shares.get(&miner1.to_base58()).unwrap(), 60);
        assert_eq!(*shares.get(&miner2.to_base58()).unwrap(), 60);
        assert_eq!(*shares.get(&miner3.to_base58()).unwrap(), 60);
        assert_eq!(*shares.get(&miner4.to_base58()).unwrap(), 10);
    }

    #[ignore]
    #[test]
    fn miners_with_shares_with_outperformer_losing_shares() {
        // setup blocks and miners
        let mut blocks = Vec::new();
        let miner1 = new_random_address();
        let n = add_blocks(&mut blocks, miner1.clone(), 0, 100);
        let miner2 = new_random_address();
        let n = add_blocks(&mut blocks, miner2.clone(), n, 90);
        let miner3 = new_random_address();
        let n = add_blocks(&mut blocks, miner3.clone(), n, 80);
        let miner4 = new_random_address();
        add_blocks(&mut blocks, miner4.clone(), n, 70);

        // execute
        let blocks = blocks.iter().collect_vec();
        let consensus_manager = ConsensusManager::builder(Network::get_current_or_user_setting_or_default())
            .build()
            .unwrap();
        let share_chain =
            InMemoryShareChain::new(MAX_BLOCKS_COUNT, PowAlgorithm::Sha3x, None, consensus_manager).unwrap();
        let shares = share_chain.miners_with_shares(blocks);

        // assert
        assert_eq!(*shares.get(&miner1.to_base58()).unwrap(), 20);
        assert_eq!(*shares.get(&miner2.to_base58()).unwrap(), 60);
        assert_eq!(*shares.get(&miner3.to_base58()).unwrap(), 60);
        assert_eq!(*shares.get(&miner4.to_base58()).unwrap(), 60);
    }

    #[ignore]
    #[test]
    fn miners_with_shares_with_outperformer_losing_all_shares() {
        // setup blocks and miners
        let mut blocks = Vec::new();
        let miner1 = new_random_address();
        let n = add_blocks(&mut blocks, miner1.clone(), 0, 100);
        let miner2 = new_random_address();
        let n = add_blocks(&mut blocks, miner2.clone(), n, 90);
        let miner3 = new_random_address();
        let n = add_blocks(&mut blocks, miner3.clone(), n, 80);
        let miner4 = new_random_address();
        let n = add_blocks(&mut blocks, miner4.clone(), n, 70);
        let miner5 = new_random_address();
        add_blocks(&mut blocks, miner5.clone(), n, 200);

        // execute
        let blocks = blocks.iter().collect_vec();
        let consensus_manager = ConsensusManager::builder(Network::get_current_or_user_setting_or_default())
            .build()
            .unwrap();
        let share_chain =
            InMemoryShareChain::new(MAX_BLOCKS_COUNT, PowAlgorithm::Sha3x, None, consensus_manager).unwrap();
        let shares = share_chain.miners_with_shares(blocks);

        // assert
        assert_eq!(shares.get(&miner1.to_base58()), None);
        assert_eq!(*shares.get(&miner2.to_base58()).unwrap(), 20);
        assert_eq!(*shares.get(&miner3.to_base58()).unwrap(), 60);
        assert_eq!(*shares.get(&miner4.to_base58()).unwrap(), 60);
        assert_eq!(*shares.get(&miner5.to_base58()).unwrap(), 60);
    }
}
