// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use log::{debug, error, info, warn};
use minotari_app_grpc::tari_rpc::{NewBlockCoinbase, SubmitBlockRequest};
use tari_common_types::tari_address::TariAddress;
use tari_core::blocks::BlockHeader;
use tari_utilities::{epoch_time::EpochTime, hex::Hex};
use tokio::sync::{RwLock, RwLockWriteGuard};

use crate::sharechain::{Block, error::{BlockConvertError, Error}, MAX_BLOCKS_COUNT, SHARE_COUNT, ShareChain, ShareChainResult, SubmitBlockResult, ValidateBlockResult};

const LOG_TARGET: &str = "in_memory_share_chain";

pub struct InMemoryShareChain {
    max_blocks_count: usize,
    blocks: Arc<RwLock<Vec<Block>>>,
}

impl Default for InMemoryShareChain {
    fn default() -> Self {
        Self {
            max_blocks_count: MAX_BLOCKS_COUNT,
            blocks: Arc::new(RwLock::new(vec![
                // genesis block
                Block::builder().with_height(0).build(),
            ])),
        }
    }
}

#[allow(dead_code)]
impl InMemoryShareChain {
    pub fn new(max_blocks_count: usize) -> Self {
        Self {
            max_blocks_count,
            blocks: Arc::new(RwLock::new(vec![
                // genesis block
                Block::builder().with_height(0).build(),
            ])),
        }
    }

    async fn miners_with_shares(&self) -> HashMap<String, f64> {
        let mut result: HashMap<String, f64> = HashMap::new(); // target wallet address -> number of shares
        let blocks_read_lock = self.blocks.read().await;
        blocks_read_lock.iter().for_each(|block| {
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

    async fn validate_block(&self, last_block: Option<&Block>, block: &Block, sync: bool) -> ShareChainResult<ValidateBlockResult> {
        if sync && last_block.is_none() {
            return Ok(ValidateBlockResult::new(true, false));
        }

        if let Some(last_block) = last_block {
            let block_height_diff = last_block.height() as i64 - block.height() as i64;
            if block_height_diff < -1 {
                warn!("Out-of-sync chain, do a sync now...");
                return Ok(ValidateBlockResult::new(false, true));
            }
            
            if block.height() <= last_block.height() {
                warn!("Uncle blocks are not handled yet! Current block height: {:?}, Last block height: {:?}", block.height(), last_block.height());
                return Ok(ValidateBlockResult::new(true, false));
            }

            // check if we have this block as last
            if last_block == block {
                warn!(target: LOG_TARGET, "↩️ This block already added, skip");
                return Ok(ValidateBlockResult::new(false, false));
            }

            // validate hash
            if block.hash() != block.generate_hash() {
                warn!(target: LOG_TARGET, "❌ Invalid block, hashes do not match");
                return Ok(ValidateBlockResult::new(false, false));
            }

            // validate height
            if last_block.height() + 1 != block.height() {
                warn!(target: LOG_TARGET, "❌ Invalid block, invalid block height: {:?} != {:?}", last_block.height() + 1, block.height());
                return Ok(ValidateBlockResult::new(false, false));
            }
        } else {
            return Ok(ValidateBlockResult::new(false, true));
        }

        Ok(ValidateBlockResult::new(true, false))
    }

    async fn submit_block_with_lock(
        &self,
        blocks: &mut RwLockWriteGuard<'_, Vec<Block>>,
        block: &Block,
        sync: bool,
    ) -> ShareChainResult<SubmitBlockResult> {
        let block = block.clone();
        let last_block = blocks.last();

        // validate
        let validate_result = self.validate_block(last_block, &block, sync).await?;
        if !validate_result.valid {
            error!(target: LOG_TARGET, "Invalid block!");
            return if !validate_result.need_sync {
                Err(Error::InvalidBlock(block))
            } else {
                Ok(SubmitBlockResult::new(true))
            };
        }

        info!(target: LOG_TARGET, "🆕 New block added: {:?}", block.hash().to_hex());

        if blocks.len() >= self.max_blocks_count {
            let diff = blocks.len() - self.max_blocks_count;
            blocks.drain(0..diff);
        }

        blocks.push(block);

        let last_block = blocks.last().ok_or_else(|| Error::Empty)?;
        info!(target: LOG_TARGET, "⬆️  Current height: {:?}", last_block.height());

        Ok(SubmitBlockResult::new(validate_result.need_sync))
    }
}

#[async_trait]
impl ShareChain for InMemoryShareChain {
    async fn submit_block(&self, block: &Block) -> ShareChainResult<SubmitBlockResult> {
        let mut blocks_write_lock = self.blocks.write().await;
        self.submit_block_with_lock(&mut blocks_write_lock, block, false).await
    }

    async fn submit_blocks(&self, blocks: Vec<Block>, sync: bool) -> ShareChainResult<SubmitBlockResult> {
        let mut blocks_write_lock = self.blocks.write().await;

        // let last_block = blocks_write_lock.last();
        // if (sync && last_block.is_none()) ||
        //     (sync && last_block.is_some() && !blocks.is_empty() && last_block.unwrap().height() < blocks[0].height())
        // {
        //     blocks_write_lock.clear();
        // }
        if sync {
            blocks_write_lock.clear();
        }

        for block in blocks {
            let result = self.submit_block_with_lock(&mut blocks_write_lock, &block, sync)
                .await?;
            if result.need_sync {
                return Ok(SubmitBlockResult::new(true));
            }
        }

        Ok(SubmitBlockResult::new(false))
    }

    async fn tip_height(&self) -> ShareChainResult<u64> {
        let blocks_read_lock = self.blocks.read().await;
        let last_block = blocks_read_lock.last().ok_or_else(|| Error::Empty)?;
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
                debug!(target: LOG_TARGET, "{addr} -> SHARE: {share:?} T, REWARD: {curr_reward:?}");
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

        let blocks_read_lock = self.blocks.read().await;
        let last_block = blocks_read_lock.last().ok_or_else(|| Error::Empty)?;

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
        let blocks_read_lock = self.blocks.read().await;
        Ok(blocks_read_lock
            .iter()
            .filter(|block| block.height() > from_height)
            .cloned()
            .collect())
    }

    async fn validate_block(&self, block: &Block) -> ShareChainResult<ValidateBlockResult> {
        let blocks_read_lock = self.blocks.read().await;
        let last_block = blocks_read_lock.last().ok_or_else(|| Error::Empty)?;
        self.validate_block(Some(last_block), block, false).await
    }
}
