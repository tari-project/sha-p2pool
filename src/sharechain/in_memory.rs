use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use log::{debug, info, warn};
use minotari_app_grpc::tari_rpc::{NewBlockCoinbase, SubmitBlockRequest};
use tari_common_types::tari_address::TariAddress;
use tari_core::blocks::BlockHeader;
use tari_utilities::epoch_time::EpochTime;
use tari_utilities::hex::Hex;
use tokio::sync::{RwLock, RwLockWriteGuard};

use crate::sharechain::error::{BlockConvertError, Error};
use crate::sharechain::{Block, ShareChain, ShareChainResult, MAX_BLOCKS_COUNT, SHARE_COUNT};

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

    async fn validate_block(&self, last_block: &Block, block: &Block) -> ShareChainResult<bool> {
        // check if we have this block as last
        if last_block == block {
            warn!(target: LOG_TARGET, "↩️ This block already added, skip");
            return Ok(false);
        }

        // validate hash
        if block.hash() != block.generate_hash() {
            warn!(target: LOG_TARGET, "❌ Invalid block, hashes do not match");
            return Ok(false);
        }

        // validate height
        if last_block.height() + 1 != block.height() {
            warn!(target: LOG_TARGET, "❌ Invalid block, invalid block height: {:?} != {:?}", last_block.height() + 1, block.height());
            return Ok(false);
        }

        Ok(true)
    }

    async fn submit_block_with_lock(
        &self,
        blocks: &mut RwLockWriteGuard<'_, Vec<Block>>,
        block: &Block,
        clear_before_add: bool,
    ) -> ShareChainResult<()> {
        let block = block.clone();

        let last_block = blocks.last();

        // validate
        if !clear_before_add && last_block.is_some() {
            if !self.validate_block(last_block.unwrap(), &block).await? {
                return Err(Error::InvalidBlock(block));
            }
        } else if !clear_before_add && last_block.is_none() {
            return Err(Error::Empty);
        } else if clear_before_add {
            // if we are synchronizing blocks, we trust we receive all the valid blocks
            blocks.clear();
        }

        if blocks.len() >= self.max_blocks_count {
            let diff = blocks.len() - self.max_blocks_count;
            blocks.drain(0..diff);
        }

        info!(target: LOG_TARGET, "🆕 New block added: {:?}", block.hash().to_hex());

        blocks.push(block);

        let last_block = blocks.last().ok_or_else(|| Error::Empty)?;
        info!(target: LOG_TARGET, "⬆️  Current height: {:?}", last_block.height());

        Ok(())
    }
}

#[async_trait]
impl ShareChain for InMemoryShareChain {
    async fn submit_block(&self, block: &Block) -> ShareChainResult<()> {
        let mut blocks_write_lock = self.blocks.write().await;
        self.submit_block_with_lock(&mut blocks_write_lock, block, false)
            .await
    }

    async fn submit_blocks(&self, blocks: Vec<Block>, mut sync: bool) -> ShareChainResult<()> {
        let mut blocks_write_lock = self.blocks.write().await;
        for block in blocks {
            self.submit_block_with_lock(&mut blocks_write_lock, &block, sync)
                .await?;
            sync = false;
        }

        Ok(())
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
                TariAddress::from_hex(request.wallet_payment_address.as_str())
                    .map_err(Error::TariAddress)?,
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

    async fn validate_block(&self, block: &Block) -> ShareChainResult<bool> {
        let blocks_read_lock = self.blocks.read().await;
        let last_block = blocks_read_lock.last().ok_or_else(|| Error::Empty)?;
        self.validate_block(last_block, block).await
    }
}
