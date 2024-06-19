use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use log::{info, warn};
use minotari_app_grpc::tari_rpc::{NewBlockCoinbase, SubmitBlockRequest};
use tari_common_types::tari_address::TariAddress;
use tari_core::blocks::BlockHeader;
use tari_utilities::epoch_time::EpochTime;
use tokio::sync::RwLock;

use crate::sharechain::{Block, ShareChain, ShareChainResult};
use crate::sharechain::error::{BlockConvertError, Error};

const DEFAULT_MAX_BLOCKS_COUNT: usize = 5000;

pub struct InMemoryShareChain {
    max_blocks_count: usize,
    blocks: Arc<RwLock<Vec<Block>>>,
}

impl Default for InMemoryShareChain {
    fn default() -> Self {
        Self {
            max_blocks_count: DEFAULT_MAX_BLOCKS_COUNT,
            blocks: Arc::new(
                RwLock::new(
                    vec![
                        // genesis block
                        Block::builder()
                            .with_height(0)
                            .build()
                    ],
                ),
            ),
        }
    }
}

impl InMemoryShareChain {
    pub fn new(max_blocks_count: usize) -> Self {
        Self {
            max_blocks_count,
            blocks: Arc::new(
                RwLock::new(
                    vec![
                        // genesis block
                        Block::builder()
                            .with_height(0)
                            .build()
                    ],
                ),
            ),
        }
    }

    async fn miners_with_hash_rates(&self) -> HashMap<String, f64> {
        let mut result: HashMap<String, f64> = HashMap::new(); // target wallet address -> hash rate
        let blocks_read_lock = self.blocks.read().await;
        blocks_read_lock.iter().for_each(|block| {
            if let Some(miner_wallet_address) = block.miner_wallet_address() {
                let addr = miner_wallet_address.to_hex();
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
            warn!("This block already added, skip");
            return Ok(false);
        }

        // validate hash
        if block.hash() != block.generate_hash() {
            warn!("Invalid block, hashes do not match");
            return Ok(false);
        }

        // validate height
        info!("VALIDATION - Last block: {:?}", last_block);
        if last_block.height() + 1 != block.height() {
            warn!("Invalid block, invalid block height: {:?} != {:?}", last_block.height() + 1, block.height());
            return Ok(false);
        }

        Ok(true)
    }
}

#[async_trait]
impl ShareChain for InMemoryShareChain {
    async fn submit_block(&self, block: &Block) -> ShareChainResult<()> {
        let mut blocks_write_lock = self.blocks.write().await;

        let block = block.clone();

        let last_block = blocks_write_lock.last().ok_or_else(|| Error::Empty)?;

        // validate
        if !self.validate_block(last_block, &block).await? {
            return Err(Error::InvalidBlock(block));
        }

        if blocks_write_lock.len() >= self.max_blocks_count {
            // remove first element to keep the maximum vector size
            blocks_write_lock.remove(0);
        }

        info!("New block added: {:?}", block.clone());

        blocks_write_lock.push(block);

        let last_block = blocks_write_lock.last().ok_or_else(|| Error::Empty)?;
        info!("Current height: {:?}", last_block.height());

        Ok(())
    }

    async fn tip_height(&self) -> ShareChainResult<u64> {
        let blocks_read_lock = self.blocks.read().await;
        let last_block = blocks_read_lock.last().ok_or_else(|| Error::Empty)?;
        Ok(last_block.height())
    }

    async fn generate_shares(&self, reward: u64) -> Vec<NewBlockCoinbase> {
        let mut result = vec![];
        let miners = self.miners_with_hash_rates().await;

        // calculate full hash rate and shares
        let full_hash_rate: f64 = miners.values().sum();
        miners.iter()
            .map(|(addr, rate)| (addr, rate / full_hash_rate))
            .filter(|(_, share)| *share > 0.0)
            .for_each(|(addr, share)| {
                let curr_reward = ((reward as f64) * share) as u64;
                info!("{addr} -> SHARE: {share:?}, REWARD: {curr_reward:?}");
                result.push(NewBlockCoinbase {
                    address: addr.clone(),
                    value: curr_reward,
                    stealth_payment: false,
                    revealed_value_proof: true,
                    coinbase_extra: vec![],
                });
            });

        result
    }

    async fn new_block(&self, request: &SubmitBlockRequest) -> ShareChainResult<Block> {
        let origin_block_grpc = request.block.as_ref()
            .ok_or_else(|| BlockConvertError::MissingField("block".to_string()))?;
        let origin_block_header_grpc = origin_block_grpc.header.as_ref()
            .ok_or_else(|| BlockConvertError::MissingField("header".to_string()))?;
        let origin_block_header = BlockHeader::try_from(origin_block_header_grpc.clone())
            .map_err(BlockConvertError::GrpcBlockHeaderConvert)?;

        let blocks_read_lock = self.blocks.read().await;
        let last_block = blocks_read_lock.last().ok_or_else(|| Error::Empty)?;

        Ok(
            Block::builder()
                .with_timestamp(EpochTime::now())
                .with_prev_hash(last_block.generate_hash())
                .with_height(last_block.height() + 1)
                .with_original_block_header(origin_block_header)
                .with_miner_wallet_address(
                    TariAddress::from_hex(request.wallet_payment_address.as_str())
                        .map_err(Error::TariAddress)?
                )
                .build()
        )
    }
}