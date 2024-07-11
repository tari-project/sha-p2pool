// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::slice::Iter;
use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use log::{debug, error, info, warn};
use minotari_app_grpc::tari_rpc::{NewBlockCoinbase, SubmitBlockRequest};
use tari_common_types::tari_address::TariAddress;
use tari_core::blocks::BlockHeader;
use tari_core::proof_of_work::sha3x_difficulty;
use tari_utilities::{epoch_time::EpochTime, hex::Hex};
use tokio::sync::{RwLock, RwLockWriteGuard};

use crate::sharechain::{
    error::{BlockConvertError, Error},
    Block, ShareChain, ShareChainResult, SubmitBlockResult, ValidateBlockResult, MAX_BLOCKS_COUNT, SHARE_COUNT,
};

const LOG_TARGET: &str = "in_memory_share_chain";

pub struct InMemoryShareChain {
    max_blocks_count: usize,
    block_levels: Arc<RwLock<Vec<BlockLevel>>>,
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
        if self.height != block.height() {
            return Err(Error::InvalidBlock(block));
        }
        self.blocks.push(block);
        Ok(())
    }
}

fn genesis_block() -> Block {
    Block::builder().with_height(0).build()
}

impl Default for InMemoryShareChain {
    fn default() -> Self {
        Self {
            max_blocks_count: MAX_BLOCKS_COUNT,
            block_levels: Arc::new(RwLock::new(vec![BlockLevel::new(vec![genesis_block()], 0)])),
        }
    }
}

#[allow(dead_code)]
impl InMemoryShareChain {
    pub fn new(max_blocks_count: usize) -> Self {
        Self {
            max_blocks_count,
            block_levels: Arc::new(RwLock::new(vec![BlockLevel::new(vec![genesis_block()], 0)])),
        }
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
            .max_by(|block1, block2| block1.height().cmp(&block2.height()))
            .cloned()
    }

    /// Returns the current (strongest) chain
    fn chain(&self, block_level_iter: Iter<'_, BlockLevel>) -> Vec<Block> {
        let mut result = vec![];
        block_level_iter.for_each(|level| {
            level
                .blocks
                .iter()
                .max_by(|block1, block2| {
                    let diff1 = if let Ok(diff) = sha3x_difficulty(block1.original_block_header()) {
                        diff.as_u64()
                    } else {
                        0
                    };
                    let diff2 = if let Ok(diff) = sha3x_difficulty(block2.original_block_header()) {
                        diff.as_u64()
                    } else {
                        0
                    };
                    diff1.cmp(&diff2)
                })
                .iter()
                .cloned()
                .for_each(|block| {
                    result.push(block.clone());
                });
        });

        result
    }

    // TODO: use integers instead of floats
    /// Generating number of shares for all the miners.
    async fn miners_with_shares(&self) -> HashMap<String, f64> {
        let mut result: HashMap<String, f64> = HashMap::new(); // target wallet address -> number of shares
        let block_levels = self.block_levels.read().await;
        let chain = self.chain(block_levels.iter());
        chain.iter().for_each(|block| {
            if let Some(miner_wallet_address) = block.miner_wallet_address() {
                let addr = miner_wallet_address.to_base58();
                if let Some(curr_hash_rate) = result.get(&addr) {
                    result.insert(addr, curr_hash_rate + 1.0);
                } else {
                    result.insert(addr, 1.0);
                }
            }
        });

        result
    }

    /// Validating a new block.
    async fn validate_block(
        &self,
        last_block: Option<&Block>,
        block: &Block,
        sync: bool,
    ) -> ShareChainResult<ValidateBlockResult> {
        if sync && last_block.is_none() {
            return Ok(ValidateBlockResult::new(true, false));
        }

        if let Some(last_block) = last_block {
            // check if we have outdated tip of chain
            let block_height_diff = block.height() as i64 - last_block.height() as i64;
            if block_height_diff > 1 {
                warn!("Out-of-sync chain, do a sync now...");
                return Ok(ValidateBlockResult::new(false, true));
            }

            // validate hash
            if block.hash() != block.generate_hash() {
                warn!(target: LOG_TARGET, "❌ Invalid block, hashes do not match");
                return Ok(ValidateBlockResult::new(false, false));
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
        sync: bool,
    ) -> ShareChainResult<SubmitBlockResult> {
        let chain = self.chain(block_levels.iter());
        let last_block = chain.last();

        // validate
        let validate_result = self.validate_block(last_block, block, sync).await?;
        if !validate_result.valid {
            return if !validate_result.need_sync {
                Err(Error::InvalidBlock(block.clone()))
            } else {
                Ok(SubmitBlockResult::new(true))
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
            .filter(|level| level.height == block.height())
            .last()
        {
            let found = found_level
                .blocks
                .iter()
                .filter(|curr_block| curr_block.generate_hash() == block.generate_hash())
                .count()
                > 0;
            if !found {
                found_level.add_block(block.clone())?;
                info!(target: LOG_TARGET, "🆕 New block added: {:?}", block.hash().to_hex());
            }
        } else if let Some(last_block) = last_block {
            if last_block.height() < block.height() {
                block_levels.push(BlockLevel::new(vec![block.clone()], block.height()));
                info!(target: LOG_TARGET, "🆕 New block added: {:?}", block.hash().to_hex());
            }
        } else {
            block_levels.push(BlockLevel::new(vec![block.clone()], block.height()));
            info!(target: LOG_TARGET, "🆕 New block added: {:?}", block.hash().to_hex());
        }

        Ok(SubmitBlockResult::new(validate_result.need_sync))
    }
}

#[async_trait]
impl ShareChain for InMemoryShareChain {
    async fn submit_block(&self, block: &Block) -> ShareChainResult<SubmitBlockResult> {
        let mut block_levels_write_lock = self.block_levels.write().await;
        let result = self
            .submit_block_with_lock(&mut block_levels_write_lock, block, false)
            .await;
        let chain = self.chain(block_levels_write_lock.iter());
        let last_block = chain.last().ok_or_else(|| Error::Empty)?;
        info!(target: LOG_TARGET, "⬆️  Current height: {:?}", last_block.height());
        result
    }

    async fn submit_blocks(&self, blocks: Vec<Block>, sync: bool) -> ShareChainResult<SubmitBlockResult> {
        let mut block_levels_write_lock = self.block_levels.write().await;

        if sync {
            let chain = self.chain(block_levels_write_lock.iter());
            if let Some(last_block) = chain.last() {
                if last_block.hash() != genesis_block().hash()
                    && (last_block.height() as usize) < MAX_BLOCKS_COUNT
                    && (blocks[0].height() as usize) > MAX_BLOCKS_COUNT
                {
                    block_levels_write_lock.clear();
                }
            }
        }

        for block in blocks {
            let result = self
                .submit_block_with_lock(&mut block_levels_write_lock, &block, sync)
                .await?;
            if result.need_sync {
                return Ok(SubmitBlockResult::new(true));
            }
        }

        let chain = self.chain(block_levels_write_lock.iter());
        let last_block = chain.last().ok_or_else(|| Error::Empty)?;
        info!(target: LOG_TARGET, "⬆️  Current height: {:?}", last_block.height());

        Ok(SubmitBlockResult::new(false))
    }

    async fn tip_height(&self) -> ShareChainResult<u64> {
        let block_levels_read_lock = self.block_levels.read().await;
        let chain = self.chain(block_levels_read_lock.iter());
        let last_block = chain.last().ok_or_else(|| Error::Empty)?;
        Ok(last_block.height())
    }

    async fn generate_shares(&self, reward: u64) -> Vec<NewBlockCoinbase> {
        let mut result = vec![];
        let miners = self.miners_with_shares().await;

        // calculate full hash rate and shares
        miners
            .iter()
            .map(|(addr, rate)| (addr, rate / SHARE_COUNT as f64))
            .filter(|(_, share)| *share > 0.0)
            .for_each(|(addr, share)| {
                let curr_reward = ((reward as f64) * share) as u64;
                info!(target: LOG_TARGET, "{addr} -> SHARE: {share:?} T, REWARD: {curr_reward:?}");
                result.push(NewBlockCoinbase {
                    address: addr.clone(),
                    value: curr_reward,
                    stealth_payment: true,
                    revealed_value_proof: true,
                    coinbase_extra: vec![],
                });
            });

        result
    }

    async fn new_block(&self, request: &SubmitBlockRequest) -> ShareChainResult<Block> {
        let origin_block_grpc = request
            .block
            .as_ref()
            .ok_or_else(|| BlockConvertError::MissingField("block".to_string()))?;
        let origin_block_header_grpc = origin_block_grpc
            .header
            .as_ref()
            .ok_or_else(|| BlockConvertError::MissingField("header".to_string()))?;
        let origin_block_header = BlockHeader::try_from(origin_block_header_grpc.clone())
            .map_err(BlockConvertError::GrpcBlockHeaderConvert)?;

        let block_levels_read_lock = self.block_levels.read().await;
        let chain = self.chain(block_levels_read_lock.iter());
        let last_block = chain.last().ok_or_else(|| Error::Empty)?;

        Ok(Block::builder()
            .with_timestamp(EpochTime::now())
            .with_prev_hash(last_block.generate_hash())
            .with_height(last_block.height() + 1)
            .with_original_block_header(origin_block_header)
            .with_miner_wallet_address(
                TariAddress::from_hex(request.wallet_payment_address.as_str()).map_err(Error::TariAddress)?,
            )
            .build())
    }

    async fn blocks(&self, from_height: u64) -> ShareChainResult<Vec<Block>> {
        let block_levels_read_lock = self.block_levels.read().await;
        let chain = self.chain(block_levels_read_lock.iter());
        Ok(chain
            .iter()
            .filter(|block| block.height() > from_height)
            .cloned()
            .collect())
    }

    async fn validate_block(&self, block: &Block) -> ShareChainResult<ValidateBlockResult> {
        let block_levels_read_lock = self.block_levels.read().await;
        let chain = self.chain(block_levels_read_lock.iter());
        let last_block = chain.last().ok_or_else(|| Error::Empty)?;
        self.validate_block(Some(last_block), block, false).await
    }
}
