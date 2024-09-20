// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::HashMap,
    ops::{Add, Div},
    slice::Iter,
    str::FromStr,
    sync::Arc,
};

use async_trait::async_trait;
use itertools::Itertools;
use log::*;
use minotari_app_grpc::tari_rpc::{NewBlockCoinbase, SubmitBlockRequest};
use num::{BigUint, FromPrimitive, Integer, Zero};
use tari_common_types::{tari_address::TariAddress, types::BlockHash};
use tari_core::{
    blocks,
    consensus::ConsensusManager,
    proof_of_work::{randomx_difficulty, sha3x_difficulty, Difficulty, PowAlgorithm},
};
use tari_utilities::{epoch_time::EpochTime, hex::Hex};
use tokio::sync::{RwLock, RwLockWriteGuard};

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
        MAX_SHARES_PER_MINER,
        SHARE_COUNT,
    },
};

const LOG_TARGET: &str = "p2pool::sharechain::in_memory";

pub struct InMemoryShareChain {
    max_blocks_count: usize,
    block_levels: Arc<RwLock<Vec<BlockLevel>>>,
    pow_algo: PowAlgorithm,
    block_validation_params: Option<Arc<BlockValidationParams>>,
    consensus_manager: ConsensusManager,
}

/// A collection of blocks with the same height.
pub struct BlockLevel {
    blocks: Vec<Block>,
    height: u64,
}

impl BlockLevel {
    pub fn new(blocks: Vec<Block>, height: u64) -> Self {
        Self { blocks, height }
    }

    pub fn add_block(&mut self, block: Block) -> Result<(), Error> {
        if self.height != block.height {
            return Err(Error::InvalidBlock(block));
        }
        self.blocks.push(block);
        Ok(())
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
        Ok(Self {
            max_blocks_count,
            block_levels: Arc::new(RwLock::new(vec![BlockLevel::new(vec![genesis_block()], 0)])),
            pow_algo,
            block_validation_params,
            consensus_manager,
        })
    }

    /// Returns the last block in chain
    fn last_block(&self, block_level_iter: Iter<'_, BlockLevel>) -> Option<Block> {
        let levels = block_level_iter.as_slice();
        if levels.is_empty() {
            return None;
        }
        let last_level = &block_level_iter.as_slice().last().unwrap();

        last_level
            .blocks
            .iter()
            .max_by(|block1, block2| block1.height.cmp(&block2.height))
            .cloned()
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
        block_levels: &mut RwLockWriteGuard<'_, Vec<BlockLevel>>,
        block: &Block,
        params: Option<Arc<BlockValidationParams>>,
        sync: bool,
    ) -> ShareChainResult<SubmitBlockResult> {
        let chain = self.chain(block_levels.iter());
        let last_block = chain.last();

        // validate
        let validate_result = self.validate_block(last_block, block, params, sync).await?;
        if !validate_result.valid {
            return if validate_result.need_sync {
                Ok(SubmitBlockResult::new(true))
            } else {
                Err(Error::InvalidBlock(block.clone()))
            };
        }

        // remove the first couple of block levels if needed
        if block_levels.len() >= self.max_blocks_count {
            let diff = block_levels.len() - self.max_blocks_count;
            block_levels.drain(0..diff);
        }

        // look for the matching block level to append the new block to
        if let Some(found_level) = block_levels
            .iter_mut()
            .filter(|level| level.height == block.height)
            .last()
        {
            let found = found_level
                .blocks
                .iter()
                .filter(|curr_block| curr_block.generate_hash() == block.generate_hash())
                .count() >
                0;
            if !found {
                found_level.add_block(block.clone())?;
                debug!(target: LOG_TARGET, "[{:?}] 🆕 New block added at height {:?}: {:?}", self.pow_algo, block.height, block.hash.to_hex());
            }
        } else if let Some(last_block) = last_block {
            if last_block.height < block.height {
                block_levels.push(BlockLevel::new(vec![block.clone()], block.height));
                debug!(target: LOG_TARGET, "[{:?}] 🆕 New block added at height {:?}: {:?}", self.pow_algo, block.height, block.hash.to_hex());
            }
        } else {
            block_levels.push(BlockLevel::new(vec![block.clone()], block.height));
            debug!(target: LOG_TARGET, "[{:?}] 🆕 New block added at height {:?}: {:?}", self.pow_algo, block.height, block.hash.to_hex());
        }

        Ok(SubmitBlockResult::new(validate_result.need_sync))
    }
}

#[async_trait]
impl ShareChain for InMemoryShareChain {
    async fn submit_block(&self, block: &Block) -> ShareChainResult<SubmitBlockResult> {
        let mut block_levels_write_lock = self.block_levels.write().await;
        let result = self
            .submit_block_with_lock(
                &mut block_levels_write_lock,
                block,
                self.block_validation_params.clone(),
                false,
            )
            .await;
        let chain = self.chain(block_levels_write_lock.iter());
        let last_block = chain.last().ok_or_else(|| Error::Empty)?;
        info!(target: LOG_TARGET, "[{:?}] ⬆️ Current height: {:?}", self.pow_algo, last_block.height);
        result
    }

    async fn submit_blocks(&self, blocks: Vec<Block>, sync: bool) -> ShareChainResult<SubmitBlockResult> {
        let mut block_levels_write_lock = self.block_levels.write().await;

        if sync {
            let chain = self.chain(block_levels_write_lock.iter());
            if let Some(last_block) = chain.last() {
                if last_block.hash != genesis_block().hash &&
                    !blocks.is_empty() &&
                    last_block.height < blocks[0].height &&
                    (blocks[0].height - last_block.height) > 1
                {
                    block_levels_write_lock.clear();
                }
            }
        }

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

        let chain = self.chain(block_levels_write_lock.iter());
        let last_block = chain.last().ok_or_else(|| Error::Empty)?;
        info!(target: LOG_TARGET, "[{:?}] ⬆️ Current height: {:?}", self.pow_algo, last_block.height);

        Ok(SubmitBlockResult::new(false))
    }

    async fn tip_height(&self) -> ShareChainResult<u64> {
        let block_levels_read_lock = self.block_levels.read().await;
        let chain = self.chain(block_levels_read_lock.iter());
        let last_block = chain.last().ok_or_else(|| Error::Empty)?;
        Ok(last_block.height)
    }

    async fn generate_shares(&self, reward: u64) -> Vec<NewBlockCoinbase> {
        let chain = self.chain(self.block_levels.read().await.iter());
        let windowed_chain = chain.iter().tail(BLOCKS_WINDOW).collect_vec();
        let miners = self.miners_with_shares(windowed_chain);

        // calculate full hash rate and shares
        miners
            .iter()
            .filter(|(_, share)| **share > 0)
            .map(|(addr, share)| {
                let curr_reward = (reward / SHARE_COUNT) * share;
                debug!(target: LOG_TARGET, "[{:?}] {addr} -> SHARE: {share:?}, REWARD: {curr_reward:?}", self.pow_algo);
                NewBlockCoinbase {
                    address: addr.clone(),
                    value: curr_reward,
                    stealth_payment: true,
                    revealed_value_proof: true,
                    coinbase_extra: vec![],
                }
            })
            .collect_vec()
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
        let chain = self.chain(block_levels_read_lock.iter());
        let last_block = chain.last().ok_or_else(|| Error::Empty)?;

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
        let block_levels_read_lock = self.block_levels.read().await;
        let chain = self.chain(block_levels_read_lock.iter());
        Ok(chain
            .iter()
            .filter(|block| block.height > from_height)
            .cloned()
            .collect())
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
        let levels_lock = self.block_levels.read().await;
        let chain = self.chain(levels_lock.iter());
        let chain = chain.iter().tail(BLOCKS_WINDOW).collect_vec();
        Ok(self.miners_with_shares(chain))
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
