// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{cmp, collections::HashMap, str::FromStr, sync::Arc};

use async_trait::async_trait;
use log::*;
use minotari_app_grpc::tari_rpc::NewBlockCoinbase;
use tari_common_types::{tari_address::TariAddress, types::FixedHash};
use tari_core::{
    consensus::ConsensusManager,
    proof_of_work::{
        randomx_difficulty,
        sha3x_difficulty,
        AccumulatedDifficulty,
        Difficulty,
        DifficultyAdjustment,
        PowAlgorithm,
    },
};
use tari_utilities::epoch_time::EpochTime;
use tokio::sync::{RwLock, RwLockWriteGuard};

use super::{
    MAIN_REWARD_SHARE,
    MAX_BLOCKS_COUNT,
    MIN_RANDOMX_SCALING_FACTOR,
    MIN_SHA3X_SCALING_FACTOR,
    SHARE_WINDOW,
    UNCLE_REWARD_SHARE,
};
use crate::{
    server::{http::stats_collector::StatsBroadcastClient, PROTOCOL_VERSION},
    sharechain::{
        error::{ShareChainError, ValidationError},
        p2block::P2Block,
        p2chain::P2Chain,
        BlockValidationParams,
        ShareChain,
    },
};

const LOG_TARGET: &str = "tari::p2pool::sharechain::in_memory";
// The max allowed uncles per block
pub const UNCLE_LIMIT: usize = 3;
// The relative age of an uncle, e.g. if the block is height 10, accept uncles heights 7, 8 and 9, then MAX_UNCLE_AGE=3
pub const MAX_UNCLE_AGE: u64 = 3;

// The height when uncles can start being added to the chain. This is to prevent chains with many uncles at
// height 0, which the pool will create while waiting to sync to the chain.
pub const UNCLE_START_HEIGHT: u64 = 10;
pub const MAX_MISSING_PARENTS: usize = 10;

pub(crate) struct InMemoryShareChain {
    p2_chain: Arc<RwLock<P2Chain>>,
    pow_algo: PowAlgorithm,
    block_validation_params: Option<Arc<BlockValidationParams>>,
    consensus_manager: ConsensusManager,
    coinbase_extras: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    stat_client: StatsBroadcastClient,
}

#[allow(dead_code)]
impl InMemoryShareChain {
    pub fn new(
        pow_algo: PowAlgorithm,
        block_validation_params: Option<Arc<BlockValidationParams>>,
        consensus_manager: ConsensusManager,
        coinbase_extras: Arc<RwLock<HashMap<String, Vec<u8>>>>,
        stat_client: StatsBroadcastClient,
    ) -> Result<Self, ShareChainError> {
        if pow_algo == PowAlgorithm::RandomX && block_validation_params.is_none() {
            return Err(ShareChainError::MissingBlockValidationParams);
        }

        Ok(Self {
            p2_chain: Arc::new(RwLock::new(P2Chain::new_empty(MAX_BLOCKS_COUNT, SHARE_WINDOW))),
            pow_algo,
            block_validation_params,
            consensus_manager,
            coinbase_extras,
            stat_client,
        })
    }

    /// Calculates block difficulty based on it's pow algo.
    fn block_difficulty(&self, block: &P2Block) -> Result<u64, ValidationError> {
        match block.original_header.pow.pow_algo {
            PowAlgorithm::RandomX => {
                if let Some(params) = &self.block_validation_params {
                    let difficulty = randomx_difficulty(
                        &block.original_header,
                        params.random_x_factory(),
                        params.genesis_block_hash(),
                        params.consensus_manager(),
                    )
                    .map_err(ValidationError::RandomXDifficulty)?;
                    Ok(difficulty.as_u64())
                } else {
                    panic!("No params provided for RandomX difficulty calculation!");
                    // Ok(0)
                }
            },
            PowAlgorithm::Sha3x => {
                let difficulty = sha3x_difficulty(&block.original_header).map_err(ValidationError::Difficulty)?;
                Ok(difficulty.as_u64())
            },
        }
    }

    /// Validating a new block.
    async fn validate_claimed_difficulty(
        &self,
        block: &P2Block,
        params: Option<Arc<BlockValidationParams>>,
    ) -> Result<Difficulty, ValidationError> {
        if block.original_header.pow.pow_algo != self.pow_algo {
            warn!(target: LOG_TARGET, "[{:?}] ❌ Pow algorithm mismatch! This share chain uses {:?}!", self.pow_algo, self.pow_algo);
            return Err(ValidationError::InvalidPowAlgorithm);
        }

        // validate PoW
        let pow_algo = block.original_header.pow.pow_algo;
        let curr_difficulty = match pow_algo {
            PowAlgorithm::RandomX => {
                let random_x_params = params.ok_or(ValidationError::MissingBlockValidationParams)?;
                randomx_difficulty(
                    &block.original_header,
                    random_x_params.random_x_factory(),
                    random_x_params.genesis_block_hash(),
                    random_x_params.consensus_manager(),
                )
                .map_err(ValidationError::RandomXDifficulty)?
            },
            PowAlgorithm::Sha3x => sha3x_difficulty(&block.original_header).map_err(ValidationError::Difficulty)?,
        };
        if curr_difficulty < block.target_difficulty {
            warn!(target: LOG_TARGET, "[{:?}] ❌ Claimed difficulty is too low! Claimed: {:?}, Actual: {:?}", self.pow_algo, block.target_difficulty, curr_difficulty);
            return Err(ValidationError::DifficultyTarget);
        }

        Ok(curr_difficulty)
    }

    /// Validating a new block.
    async fn validate_block(&self, block: &P2Block) -> Result<(), ValidationError> {
        if block.uncles.len() > UNCLE_LIMIT {
            warn!(target: LOG_TARGET, "[{:?}] ❌ Too many uncles! {:?}", self.pow_algo, block.uncles.len());
            return Err(ValidationError::TooManyUncles);
        }

        if block.height < UNCLE_START_HEIGHT && !block.uncles.is_empty() {
            warn!(target: LOG_TARGET, "[{:?}] ❌ Uncles before the uncle start height! {:?}", self.pow_algo, block.height);
            return Err(ValidationError::UnclesBeforeStartHeight);
        }
        // let test the age of the uncles
        for uncle in block.uncles.iter() {
            if uncle.0 < block.height.saturating_sub(MAX_UNCLE_AGE) {
                warn!(target: LOG_TARGET, "[{:?}] ❌ Uncle is too old! {:?}", self.pow_algo, uncle.0);
                return Err(ValidationError::UncleTooOld);
            }

            if uncle.0 >= block.height {
                warn!(target: LOG_TARGET, "[{:?}] ❌ Uncle is too young! {:?}", self.pow_algo, uncle.0);
                return Err(ValidationError::UnclesOnSameHeightOrHigher);
            }
        }
        Ok(())
    }

    /// Submits a new block to share chain.
    async fn submit_block_with_lock(
        &self,
        p2_chain: &mut RwLockWriteGuard<'_, P2Chain>,
        block: Arc<P2Block>,
        params: Option<Arc<BlockValidationParams>>,
        syncing: bool,
    ) -> Result<bool, ShareChainError> {
        let new_block_p2pool_height = block.height;

        if p2_chain.get_tip().is_none() || block.height == 0 || syncing {
            let _validate_result = self.validate_claimed_difficulty(&block, params).await?;
            return p2_chain.add_block_to_chain(block.clone());
        }

        // this is safe as we already checked it does exist
        let tip_height = p2_chain.get_tip().unwrap().height;
        // We keep more blocks than the share window, but its only to validate the share window. If a block comes in
        // older than the share window is way too old for us to care about.
        if block.height < tip_height.saturating_sub(SHARE_WINDOW as u64) && !syncing {
            return Err(ShareChainError::BlockValidation(
                "Block is older than share window".to_string(),
            ));
        }

        // Check if already added.
        if let Some(level) = p2_chain.level_at_height(new_block_p2pool_height) {
            if level.blocks.contains_key(&block.hash) {
                info!(target: LOG_TARGET, "[{:?}] ✅ Block already added: {:?}", self.pow_algo, block.height);
                return Ok(false);
            }
        }

        // validate
        self.validate_block(&block).await?;
        let _validate_result = self.validate_claimed_difficulty(&block, params).await?;
        let new_block = block.clone();

        // add block to chain
        let new_tip = p2_chain.add_block_to_chain(new_block)?;

        // update coinbase extra cache
        let mut coinbase_extras_lock = self.coinbase_extras.write().await;

        coinbase_extras_lock.insert(block.miner_wallet_address.to_base58(), block.get_miner_coinbase_extra());

        Ok(new_tip)
    }

    async fn find_coinbase_extra(&self, miner_wallet_address: &TariAddress) -> Option<Vec<u8>> {
        let coinbase_extras_lock = self.coinbase_extras.read().await;
        if let Some(found_coinbase_extras) = coinbase_extras_lock.get(&miner_wallet_address.to_base58()) {
            return Some(found_coinbase_extras.clone());
        }

        None
    }

    async fn get_calculate_and_cache_hashmap_of_shares(
        &self,
        p2_chain: &mut RwLockWriteGuard<'_, P2Chain>,
    ) -> Result<HashMap<String, (u64, Vec<u8>)>, ShareChainError> {
        fn update_insert(
            miner_shares: &mut HashMap<String, (u64, Vec<u8>)>,
            miner: String,
            new_share: u64,
            coinbase_extra: Vec<u8>,
        ) {
            match miner_shares.get_mut(&miner) {
                Some((v, extra)) => {
                    *v += new_share;
                    *extra = coinbase_extra;
                },
                None => {
                    miner_shares.insert(miner, (new_share, coinbase_extra));
                },
            }
        }
        let mut miners_to_shares = HashMap::new();

        let tip_level = match p2_chain.get_tip() {
            Some(tip_level) => tip_level,
            None => return Ok(miners_to_shares),
        };

        // we want to count 1 short,as the final share will be for this node
        let stop_height = tip_level.height.saturating_sub(SHARE_WINDOW as u64 - 1);
        let mut cur_block = tip_level
            .blocks
            .get(&tip_level.chain_block)
            .ok_or(ShareChainError::BlockNotFound)?;
        update_insert(
            &mut miners_to_shares,
            cur_block.miner_wallet_address.to_base58(),
            MAIN_REWARD_SHARE,
            cur_block.miner_coinbase_extra.clone(),
        );
        for uncle in cur_block.uncles.iter() {
            let uncle_block = p2_chain
                .level_at_height(uncle.0)
                .ok_or(ShareChainError::UncleBlockNotFound)?
                .blocks
                .get(&uncle.1)
                .ok_or(ShareChainError::UncleBlockNotFound)?;
            update_insert(
                &mut miners_to_shares,
                uncle_block.miner_wallet_address.to_base58(),
                UNCLE_REWARD_SHARE,
                uncle_block.miner_coinbase_extra.clone(),
            );
        }
        while cur_block.height > stop_height {
            cur_block = p2_chain
                .get_parent_block(cur_block)
                .ok_or(ShareChainError::BlockNotFound)?;
            update_insert(
                &mut miners_to_shares,
                cur_block.miner_wallet_address.to_base58(),
                MAIN_REWARD_SHARE,
                cur_block.miner_coinbase_extra.clone(),
            );
            for uncle in cur_block.uncles.iter() {
                let uncle_block = p2_chain
                    .level_at_height(uncle.0)
                    .ok_or(ShareChainError::UncleBlockNotFound)?
                    .blocks
                    .get(&uncle.1)
                    .ok_or(ShareChainError::UncleBlockNotFound)?;
                update_insert(
                    &mut miners_to_shares,
                    uncle_block.miner_wallet_address.to_base58(),
                    UNCLE_REWARD_SHARE,
                    uncle_block.miner_coinbase_extra.clone(),
                );
            }
        }
        p2_chain.cached_shares = Some(miners_to_shares.clone());
        Ok(miners_to_shares)
    }
}

#[async_trait]
impl ShareChain for InMemoryShareChain {
    async fn submit_block(&self, block: Arc<P2Block>) -> Result<bool, ShareChainError> {
        if block.version != PROTOCOL_VERSION {
            return Err(ShareChainError::BlockValidation("Block version is too low".to_string()));
        }
        let mut p2_chain_write_lock = self.p2_chain.write().await;
        let height = block.height;
        debug!(target: LOG_TARGET, "[{:?}] ✅ adding Block via submit (grpc): {:?}", self.pow_algo,height );
        let res = self
            .submit_block_with_lock(
                &mut p2_chain_write_lock,
                block,
                self.block_validation_params.clone(),
                false,
            )
            .await;
        let _ = self.stat_client.send_chain_changed(
            self.pow_algo,
            p2_chain_write_lock.get_height(),
            p2_chain_write_lock.get_max_chain_length() as u64,
        );
        match &res {
            Ok(false) => {
                info!(target: LOG_TARGET, "[{:?}] ✅ added Block: {:?} successfully, no tip change", self.pow_algo, height)
            },
            Ok(true) => {
                info!(target: LOG_TARGET, "[{:?}] ✅ added Block: {:?} successfully, tip changed", self.pow_algo, height)
            },
            Err(ShareChainError::BlockParentDoesNotExist { missing_parents }) => {
                let missing_heights = missing_parents.iter().map(|data| data.0).collect::<Vec<u64>>();
                info!(target: LOG_TARGET, "[{:?}] Missing blocks for the following heights: {:?}", self.pow_algo, missing_heights);
            },
            Err(e) => warn!(target: LOG_TARGET, "Failed to add block from submit (height {}): {}", height, e),
        }
        res
    }

    async fn add_synced_blocks(&self, blocks: &[Arc<P2Block>]) -> Result<bool, ShareChainError> {
        let mut p2_chain_write_lock = self.p2_chain.write().await;
        let mut new_tip = false;

        let mut blocks = blocks.to_vec();
        let mut known_blocks_incoming = Vec::new();
        let mut missing_parents = HashMap::new();
        if !blocks.is_sorted_by_key(|block| block.height) {
            blocks.sort_by(|a, b| a.height.cmp(&b.height));
            //  return Err(ShareChainError::BlockValidation("Blocks are not sorted by height".to_string()));
        }
        for block in blocks.iter() {
            known_blocks_incoming.push(block.hash);
        }

        'outer: for block in blocks {
            if block.version != PROTOCOL_VERSION {
                return Err(ShareChainError::BlockValidation("Block version is too low".to_string()));
            }
            let height = block.height;
            // info!(target: LOG_TARGET, "[{:?}] ✅ adding Block from sync: {:?}", self.pow_algo, height);
            match self
                .submit_block_with_lock(
                    &mut p2_chain_write_lock,
                    block,
                    self.block_validation_params.clone(),
                    true,
                )
                .await
            {
                Ok(tip_change) => {
                    debug!(target: LOG_TARGET, "[{:?}] ✅ added Block: {:?} successfully. Tip changed: {}", self.pow_algo, height, tip_change);
                    new_tip = tip_change;
                },
                Err(e) => {
                    if let ShareChainError::BlockParentDoesNotExist {
                        missing_parents: new_missing_parents,
                    } = e
                    {
                        let mut missing_heights = Vec::new();
                        for new_missing_parent in new_missing_parents {
                            if known_blocks_incoming.contains(&new_missing_parent.1) {
                                continue;
                            }
                            missing_heights.push(new_missing_parent.0);
                            missing_parents.insert(new_missing_parent.1, new_missing_parent.0);
                            if missing_parents.len() > MAX_MISSING_PARENTS {
                                break 'outer;
                            }
                        }
                    } else {
                        warn!(target: LOG_TARGET, "Failed to add block during sync (height {}): {}", height, e);
                        return Err(e);
                    }
                },
            }
        }
        let _ = self.stat_client.send_chain_changed(
            self.pow_algo,
            p2_chain_write_lock.get_height(),
            p2_chain_write_lock.get_max_chain_length() as u64,
        );

        if !missing_parents.is_empty() {
            info!(target: LOG_TARGET, "[{:?}] Missing blocks for the following heights: {:?}", self.pow_algo, missing_parents.values().map(|height| height.to_string()));
            return Err(ShareChainError::BlockParentDoesNotExist {
                missing_parents: missing_parents
                    .into_iter()
                    .map(|(hash, height)| (height, hash))
                    .collect(),
            });
        }
        Ok(new_tip)
    }

    async fn tip_height(&self) -> Result<u64, ShareChainError> {
        let bl = self.p2_chain.read().await;
        let tip_level = bl.get_height();
        Ok(tip_level)
    }

    async fn chain_pow(&self) -> AccumulatedDifficulty {
        let bl = self.p2_chain.read().await;
        bl.total_accumulated_tip_difficulty()
    }

    async fn get_tip(&self) -> Result<Option<(u64, FixedHash)>, ShareChainError> {
        let bl = self.p2_chain.read().await;
        let tip_level = bl.get_tip();
        if let Some(tip_level) = tip_level {
            Ok(Some((tip_level.height, tip_level.chain_block)))
        } else {
            Ok(None)
        }
    }

    async fn get_tip_and_uncles(&self) -> Vec<(u64, FixedHash)> {
        let mut res = Vec::new();
        let bl = self.p2_chain.read().await;
        let tip_level = bl.get_tip();
        if let Some(tip_level) = tip_level {
            res.push((tip_level.height, tip_level.chain_block));
            tip_level.block_in_main_chain().inspect(|block| {
                for uncle in block.uncles.iter() {
                    res.push((uncle.0, uncle.1));
                }
            });
        }
        res
    }

    async fn generate_shares(&self, new_tip_block: &P2Block) -> Result<Vec<NewBlockCoinbase>, ShareChainError> {
        let mut chain_read_lock = self.p2_chain.read().await;
        // first check if there is a cached hashmap of shares
        let mut miners_to_shares = if let Some(ref cached_shares) = chain_read_lock.cached_shares {
            cached_shares.clone()
        } else {
            HashMap::new()
        };
        if miners_to_shares.is_empty() {
            drop(chain_read_lock);
            // if there is none, lets see if we need to calculate one
            let mut wl = self.p2_chain.write().await;
            miners_to_shares = self.get_calculate_and_cache_hashmap_of_shares(&mut wl).await?;
            chain_read_lock = wl.downgrade();
        }

        // lets add the new tip block to the hashmap
        miners_to_shares.insert(
            new_tip_block.miner_wallet_address.to_base58(),
            (MAIN_REWARD_SHARE, new_tip_block.miner_coinbase_extra.clone()),
        );
        for uncle in new_tip_block.uncles.iter() {
            let uncle_block = chain_read_lock
                .level_at_height(uncle.0)
                .ok_or(ShareChainError::UncleBlockNotFound)?
                .blocks
                .get(&uncle.1)
                .ok_or(ShareChainError::UncleBlockNotFound)?;
            miners_to_shares.insert(
                uncle_block.miner_wallet_address.to_base58(),
                (UNCLE_REWARD_SHARE, uncle_block.miner_coinbase_extra.clone()),
            );
        }

        let mut res = vec![];

        for (key, (shares, extra)) in miners_to_shares {
            // find coinbase extra for wallet address
            let address = match TariAddress::from_str(&key) {
                Ok(v) => v,
                Err(e) => {
                    error!(target: LOG_TARGET, "Could not parse address: {}", e);
                    continue;
                },
            };

            res.push(NewBlockCoinbase {
                address: address.to_base58(),
                value: shares,
                stealth_payment: false,
                revealed_value_proof: true,
                coinbase_extra: extra,
            });
        }

        Ok(res)
    }

    async fn generate_new_tip_block(
        &self,
        miner_address: &TariAddress,
        coinbase_extra: Vec<u8>,
    ) -> Result<Arc<P2Block>, ShareChainError> {
        let chain_read_lock = self.p2_chain.read().await;

        // edge case for chain start
        let (last_block_hash, new_height) = match chain_read_lock.get_tip() {
            Some(tip) => {
                let hash = match tip.block_in_main_chain() {
                    Some(block) => block.hash,
                    None => FixedHash::zero(),
                };
                (hash, tip.height.saturating_add(1))
            },
            None => (FixedHash::zero(), 0),
        };

        // lets calculate the uncles
        // uncle rules are:
        // 1. The uncle can only be a max of 3 blocks older than the new tip
        // 2. The uncle can only be an uncle once in the chain
        // 3. The uncle must link back to the main chain
        // 4. The chain height must be above 5
        let mut excluded_uncles: Vec<FixedHash> = vec![];
        let mut uncles: Vec<(u64, FixedHash)> = vec![];
        if new_height >= UNCLE_START_HEIGHT {
            for height in new_height.saturating_sub(MAX_UNCLE_AGE)..new_height {
                if let Some(older_level) = chain_read_lock.level_at_height(height) {
                    let chain_block = older_level
                        .block_in_main_chain()
                        .ok_or(ShareChainError::BlockNotFound)?;
                    // Blocks in the main chain can't be uncles
                    excluded_uncles.push(chain_block.hash);
                    for uncle in &chain_block.uncles {
                        excluded_uncles.push(uncle.1);
                    }
                    for block in &older_level.blocks {
                        uncles.push((height, *block.0));
                    }
                }
            }
            for uncle in &uncles {
                if chain_read_lock.level_at_height(uncle.0).is_none() {
                    excluded_uncles.push((*uncle.1).into());
                    continue;
                }
                if let Some(level) = chain_read_lock.level_at_height(uncle.0) {
                    if let Some(uncle_block) = level.blocks.get(&uncle.1) {
                        let parent = match chain_read_lock.get_parent_block(uncle_block) {
                            Some(parent) => parent,
                            None => {
                                excluded_uncles.push(uncle.1);
                                continue;
                            },
                        };
                        if chain_read_lock
                            .level_at_height(parent.height)
                            .ok_or(ShareChainError::BlockLevelNotFound)?
                            .chain_block !=
                            parent.hash
                        {
                            excluded_uncles.push(uncle.1);
                        }
                    } else {
                        excluded_uncles.push(uncle.1);
                    }
                }
            }

            // Remove excluded.
            for excluded in &excluded_uncles {
                uncles.retain(|uncle| &uncle.1 != excluded);
            }
            uncles.truncate(UNCLE_LIMIT);
        }

        Ok(P2Block::builder()
            .with_timestamp(EpochTime::now())
            .with_prev_hash(last_block_hash)
            .with_height(new_height)
            .with_uncles(uncles)
            .with_miner_wallet_address(miner_address.clone())
            .with_miner_coinbase_extra(coinbase_extra)
            .build())
    }

    async fn get_blocks(&self, requested_blocks: &[(u64, FixedHash)]) -> Vec<Arc<P2Block>> {
        let p2_chain_read_lock = self.p2_chain.read().await;
        let mut blocks = Vec::with_capacity(requested_blocks.len());

        for block in requested_blocks {
            if let Some(level) = p2_chain_read_lock.level_at_height(block.0) {
                if let Some(block) = level.blocks.get(&block.1) {
                    blocks.push(block.clone());
                } else {
                    // if sync requestee only sees their behind on tip, they will fill in fixedhash::zero(), so it wont
                    // find this hash, so we return the curent chain block
                    if let Some(block) = level.block_in_main_chain() {
                        blocks.push(block.clone());
                    }
                }
            }
        }
        blocks
    }

    async fn request_sync(
        &self,
        their_blocks: &[(u64, FixedHash)],
        limit: usize,
        last_block_received: Option<(u64, FixedHash)>,
    ) -> Result<Vec<Arc<P2Block>>, ShareChainError> {
        let p2_chain_read = self.p2_chain.read().await;

        // Assume their blocks are in order highest first.
        let mut split_height = 0;

        if let Some(last_block_received) = last_block_received {
            if let Some(level) = p2_chain_read.level_at_height(last_block_received.0) {
                if let Some(block) = level.blocks.get(&last_block_received.1) {
                    split_height = block.height;
                }
            }
        }

        let mut their_blocks = their_blocks.to_vec();
        // Highest to lowest
        their_blocks.sort_by(|a, b| b.0.cmp(&a.0));
        // their_blocks.reverse();

        let mut split_height2 = 0;
        // Go back and find the split in the chain
        for their_block in their_blocks {
            if let Some(level) = p2_chain_read.level_at_height(their_block.0) {
                if let Some(block) = level.blocks.get(&their_block.1) {
                    // Only split if the block is in the main chain
                    if level.chain_block == block.hash {
                        split_height2 = block.height;
                        break;
                    }
                }
            }
        }

        self.all_blocks(Some(cmp::max(split_height, split_height2)), limit, true)
            .await
    }

    async fn get_target_difficulty(&self, height: u64) -> Difficulty {
        let mut min = self
            .consensus_manager
            .consensus_constants(height)
            .min_pow_difficulty(self.pow_algo);
        match self.pow_algo {
            PowAlgorithm::RandomX => {
                min = Difficulty::from_u64(min.as_u64() / MIN_RANDOMX_SCALING_FACTOR).unwrap();
            },
            PowAlgorithm::Sha3x => {
                min = Difficulty::from_u64(min.as_u64() / MIN_SHA3X_SCALING_FACTOR).unwrap();
            },
        }
        let max = self
            .consensus_manager
            .consensus_constants(height)
            .max_pow_difficulty(self.pow_algo);
        let chain_read_lock = self.p2_chain.read().await;

        let difficulty = chain_read_lock.lwma.get_difficulty().unwrap_or(Difficulty::min());
        cmp::max(min, cmp::min(max, difficulty))
    }

    async fn get_total_chain_pow(&self) -> AccumulatedDifficulty {
        let chain_read_lock = self.p2_chain.read().await;
        chain_read_lock.total_accumulated_tip_difficulty()
    }

    // For debugging only
    async fn all_blocks(
        &self,
        start_height: Option<u64>,
        page_size: usize,
        main_chain_only: bool,
    ) -> Result<Vec<Arc<P2Block>>, ShareChainError> {
        let chain_read_lock = self.p2_chain.read().await;
        let mut res = Vec::with_capacity(page_size);
        let mut num_main_chain_blocks = 0;
        let mut level = if let Some(level) = chain_read_lock.level_at_height(start_height.unwrap_or(0)) {
            level
        } else {
            // we dont have that block, see if we have a higher lowest block than they are asking for and start there
            if start_height.unwrap_or(0) < chain_read_lock.levels.back().map(|l| l.height).unwrap_or(0) {
                chain_read_lock.levels.back().unwrap()
            } else {
                return Ok(res);
            }
        };

        loop {
            for block in level.blocks.values() {
                if block.hash == level.chain_block {
                    num_main_chain_blocks += 1;
                    if main_chain_only {
                        for uncle in &block.uncles {
                            // Always include all the uncles, if we have them
                            if let Some(uncle_block) = level.blocks.get(&uncle.1) {
                                // Uncles should never exist in the main chain, so we don't need to worry about
                                // duplicates
                                res.push(uncle_block.clone());
                            }
                        }
                    }
                }

                res.push(block.clone());
                // Always include at least 2 main chain blocks so that if we called
                // this function with the starting mainchain block we can continue asking for more
                // blocks
                if res.len() >= page_size && (!main_chain_only || num_main_chain_blocks >= 2) {
                    return Ok(res);
                }
            }
            level = if let Some(new_level) = chain_read_lock.level_at_height(level.height + 1) {
                new_level
            } else {
                break;
            };
        }
        Ok(res)
    }

    async fn has_block(&self, height: u64, hash: &FixedHash) -> bool {
        let chain_read_lock = self.p2_chain.read().await;
        if let Some(level) = chain_read_lock.level_at_height(height) {
            return level.blocks.contains_key(hash);
        }
        false
    }
}

#[cfg(test)]
pub mod test {
    use tari_common::configuration::Network;
    use tari_common_types::{tari_address::TariAddressFeatures, types::BlockHash};
    use tari_crypto::{keys::PublicKey, ristretto::RistrettoPublicKey};

    use super::*;

    pub fn new_chain() -> InMemoryShareChain {
        let consensus_manager = ConsensusManager::builder(Network::LocalNet).build().unwrap();
        let coinbase_extras = Arc::new(RwLock::new(HashMap::<String, Vec<u8>>::new()));
        let (stats_tx, _) = tokio::sync::broadcast::channel(1000);
        let stats_broadcast_client = StatsBroadcastClient::new(stats_tx);
        InMemoryShareChain::new(
            PowAlgorithm::Sha3x,
            None,
            consensus_manager,
            coinbase_extras,
            stats_broadcast_client,
        )
        .unwrap()
    }

    pub fn new_random_address() -> TariAddress {
        let mut rng = rand::thread_rng();
        let (_, view) = RistrettoPublicKey::random_keypair(&mut rng);
        let (_, spend) = RistrettoPublicKey::random_keypair(&mut rng);
        TariAddress::new_dual_address(view, spend, Network::LocalNet, TariAddressFeatures::INTERACTIVE)
    }

    #[tokio::test]
    async fn equal_shares() {
        let consensus_manager = ConsensusManager::builder(Network::LocalNet).build().unwrap();
        let coinbase_extras = Arc::new(RwLock::new(HashMap::<String, Vec<u8>>::new()));
        let (stats_tx, _) = tokio::sync::broadcast::channel(1000);
        let stats_broadcast_client = StatsBroadcastClient::new(stats_tx);
        let share_chain = InMemoryShareChain::new(
            PowAlgorithm::Sha3x,
            None,
            consensus_manager,
            coinbase_extras,
            stats_broadcast_client,
        )
        .unwrap();

        let mut timestamp = EpochTime::now();
        let mut prev_hash = BlockHash::zero();
        let static_coinbase_extra = Vec::new();

        for i in 0..15 {
            let address = new_random_address();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let block = P2Block::builder()
                .with_timestamp(timestamp)
                .with_height(i)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(1).unwrap())
                .with_prev_hash(prev_hash)
                .with_miner_coinbase_extra(static_coinbase_extra.clone())
                .build();

            prev_hash = block.generate_hash();

            share_chain.submit_block(block).await.unwrap();
        }

        let mut wl = share_chain.p2_chain.write().await;
        let shares = share_chain
            .get_calculate_and_cache_hashmap_of_shares(&mut wl)
            .await
            .unwrap();
        assert_eq!(shares.len(), 15);
        for share in shares {
            assert_eq!(share.1, (5, static_coinbase_extra.clone()))
        }
    }

    #[tokio::test]
    async fn equal_share_same_participants() {
        let consensus_manager = ConsensusManager::builder(Network::LocalNet).build().unwrap();
        let coinbase_extras = Arc::new(RwLock::new(HashMap::<String, Vec<u8>>::new()));
        let (stats_tx, _) = tokio::sync::broadcast::channel(1000);
        let static_coinbase_extra = Vec::new();
        let stats_broadcast_client = StatsBroadcastClient::new(stats_tx);
        let share_chain = InMemoryShareChain::new(
            PowAlgorithm::Sha3x,
            None,
            consensus_manager,
            coinbase_extras,
            stats_broadcast_client,
        )
        .unwrap();

        let mut timestamp = EpochTime::now();
        let mut prev_hash = BlockHash::zero();
        let mut miners = Vec::new();
        for _ in 0..5 {
            let address = new_random_address();
            miners.push(address);
        }

        for i in 0..15 {
            let address = miners[i % 5].clone();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let block = P2Block::builder()
                .with_timestamp(timestamp)
                .with_height(i as u64)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(1).unwrap())
                .with_prev_hash(prev_hash)
                .with_miner_coinbase_extra(static_coinbase_extra.clone())
                .build();

            prev_hash = block.generate_hash();

            share_chain.submit_block(block).await.unwrap();
        }

        let mut wl = share_chain.p2_chain.write().await;
        let shares = share_chain
            .get_calculate_and_cache_hashmap_of_shares(&mut wl)
            .await
            .unwrap();
        assert_eq!(shares.len(), 5);
        for share in shares {
            assert_eq!(share.1, (15, static_coinbase_extra.clone()))
        }
    }

    #[tokio::test]
    async fn equal_share_same_participants_with_uncles() {
        let consensus_manager = ConsensusManager::builder(Network::LocalNet).build().unwrap();
        let coinbase_extras = Arc::new(RwLock::new(HashMap::<String, Vec<u8>>::new()));
        let (stats_tx, _) = tokio::sync::broadcast::channel(1000);
        let stats_broadcast_client = StatsBroadcastClient::new(stats_tx);
        let static_coinbase_extra = Vec::new();
        let share_chain = InMemoryShareChain::new(
            PowAlgorithm::Sha3x,
            None,
            consensus_manager,
            coinbase_extras,
            stats_broadcast_client,
        )
        .unwrap();

        let mut timestamp = EpochTime::now();
        let mut prev_hash = BlockHash::zero();
        let mut miners = Vec::new();
        for _ in 0..5 {
            let address = new_random_address();
            miners.push(address);
        }

        for i in 0..15 {
            let address = miners[i % 5].clone();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let mut uncles = Vec::new();
            if i > 10 {
                let prev_hash_uncle = share_chain
                    .p2_chain
                    .read()
                    .await
                    .level_at_height(i as u64 - 2)
                    .unwrap()
                    .chain_block;
                // lets create an uncle block
                let block = P2Block::builder()
                    .with_timestamp(timestamp)
                    .with_height(i as u64 - 1)
                    .with_miner_wallet_address(address.clone())
                    .with_target_difficulty(Difficulty::from_u64(1).unwrap())
                    .with_prev_hash(prev_hash_uncle)
                    .with_miner_coinbase_extra(static_coinbase_extra.clone())
                    .build();
                uncles.push((i as u64 - 1, block.hash));
                share_chain.submit_block(block).await.unwrap();
            }
            let block = P2Block::builder()
                .with_timestamp(timestamp)
                .with_height(i as u64)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(1).unwrap())
                .with_uncles(uncles)
                .with_prev_hash(prev_hash)
                .with_miner_coinbase_extra(static_coinbase_extra.clone())
                .build();

            prev_hash = block.generate_hash();

            share_chain.submit_block(block).await.unwrap();
        }

        let mut wl = share_chain.p2_chain.write().await;
        let shares = share_chain
            .get_calculate_and_cache_hashmap_of_shares(&mut wl)
            .await
            .unwrap();
        assert_eq!(shares.len(), 5);
        // we have 1 miner with 15 shares and 4 with 19 shares
        // 15  = 3* full shares (5)
        // 19  = 3* full shares (5) + 1 * uncle(4)
        let mut counter_19 = 0;
        let mut counter_15 = 0;
        for share in shares {
            match share.1 .0 {
                19 => counter_19 += 1,
                15 => counter_15 += 1,
                _ => panic!("Should be 19 or 15"),
            }
        }
        assert_eq!(counter_19, 4);
        assert_eq!(counter_15, 1);
    }

    #[tokio::test]
    async fn test_request_sync_starts_from_highest_match() {
        let chain = new_chain();
        let mut blocks = Vec::new();
        let mut prev_hash = BlockHash::zero();
        for i in 0..10 {
            let block = P2Block::builder()
                .with_height(i)
                .with_prev_hash(prev_hash)
                .with_target_difficulty(Difficulty::from_u64(1).unwrap())
                .build();
            prev_hash = block.generate_hash();
            blocks.push(block);
        }
        chain.add_synced_blocks(&blocks).await.unwrap();
        assert_eq!(chain.tip_height().await.unwrap(), 9);

        let mut their_blocks = Vec::new();
        their_blocks.push((3, blocks[3].hash));

        let res = chain.request_sync(&their_blocks, 10, None).await.unwrap();
        assert_eq!(res.len(), 7);
        let heights = res.iter().map(|block| block.height).collect::<Vec<u64>>();
        assert_eq!(heights, vec![3, 4, 5, 6, 7, 8, 9]);

        // if last block is higher, then we should start from the highest match
        let res = chain
            .request_sync(&their_blocks, 10, Some((5, blocks[5].hash)))
            .await
            .unwrap();

        assert_eq!(res.len(), 5);
        let heights = res.iter().map(|block| block.height).collect::<Vec<u64>>();
        assert_eq!(heights, vec![5, 6, 7, 8, 9]);

        // if there is a match in the middle, we should start from the highest match
        their_blocks.push((7, blocks[7].hash));
        let res = chain
            .request_sync(&their_blocks, 10, Some((5, blocks[5].hash)))
            .await
            .unwrap();

        assert_eq!(res.len(), 3);
        let heights = res.iter().map(|block| block.height).collect::<Vec<u64>>();
        assert_eq!(heights, vec![7, 8, 9]);

        // Add an extra block in their blocks
        let missing_block = P2Block::builder()
            .with_height(11)
            .with_prev_hash(prev_hash)
            .with_target_difficulty(Difficulty::from_u64(10).unwrap())
            .build();
        // prev_hash = block.generate_hash();
        their_blocks.push((11, missing_block.hash));

        let res = chain
            .request_sync(&their_blocks, 10, Some((5, blocks[5].hash)))
            .await
            .unwrap();

        assert_eq!(res.len(), 3);
        let heights = res.iter().map(|block| block.height).collect::<Vec<u64>>();
        assert_eq!(heights, vec![7, 8, 9]);
    }
}
