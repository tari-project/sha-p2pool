// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{cmp, collections::HashMap, str::FromStr, sync::Arc};

use async_trait::async_trait;
use log::*;
use minotari_app_grpc::tari_rpc::NewBlockCoinbase;
use num::{BigUint, Zero};
use tari_common_types::{tari_address::TariAddress, types::FixedHash};
use tari_core::{
    consensus::ConsensusManager,
    proof_of_work::{randomx_difficulty, sha3x_difficulty, Difficulty, DifficultyAdjustment, PowAlgorithm},
};
use tari_utilities::epoch_time::EpochTime;
use tokio::sync::{RwLock, RwLockWriteGuard};

use super::{MAIN_REWARD_SHARE, MAX_BLOCKS_COUNT, SHARE_WINDOW, UNCLE_REWARD_SHARE};
use crate::{
    server::{grpc::p2pool::min_difficulty, http::stats_collector::StatsBroadcastClient, p2p::Squad},
    sharechain::{
        error::{Error, ValidationError},
        p2block::P2Block,
        p2chain::P2Chain,
        BlockValidationParams,
        ShareChain,
    },
};

const LOG_TARGET: &str = "tari::p2pool::sharechain::in_memory";

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
    ) -> Result<Self, Error> {
        if pow_algo == PowAlgorithm::RandomX && block_validation_params.is_none() {
            return Err(Error::MissingBlockValidationParams);
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
        match block.original_block.header.pow.pow_algo {
            PowAlgorithm::RandomX => {
                if let Some(params) = &self.block_validation_params {
                    let difficulty = randomx_difficulty(
                        &block.original_block.header,
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
                let difficulty = sha3x_difficulty(&block.original_block.header).map_err(ValidationError::Difficulty)?;
                Ok(difficulty.as_u64())
            },
        }
    }

    fn validate_min_difficulty(
        &self,
        pow: PowAlgorithm,
        curr_difficulty: Difficulty,
        height: u64,
    ) -> Result<(), ValidationError> {
        if curr_difficulty < min_difficulty(&self.consensus_manager, pow, height) {
            warn!(target: LOG_TARGET, "[{:?}] ❌ Too low difficulty!", self.pow_algo);
            return Err(ValidationError::DifficultyBelowMinimum);
        }

        Ok(())
    }

    /// Validating a new block.
    async fn validate_block(
        &self,
        _last_block: &P2Block,
        block: &P2Block,
        params: Option<Arc<BlockValidationParams>>,
    ) -> Result<Difficulty, ValidationError> {
        if block.original_block.header.pow.pow_algo != self.pow_algo {
            warn!(target: LOG_TARGET, "[{:?}] ❌ Pow algorithm mismatch! This share chain uses {:?}!", self.pow_algo, self.pow_algo);
            return Err(ValidationError::InvalidPowAlgorithm);
        }

        // validate PoW
        let pow_algo = block.original_block.header.pow.pow_algo;
        let curr_difficulty = match pow_algo {
            PowAlgorithm::RandomX => {
                let random_x_params = params.ok_or(ValidationError::MissingBlockValidationParams)?;
                randomx_difficulty(
                    &block.original_block.header,
                    random_x_params.random_x_factory(),
                    random_x_params.genesis_block_hash(),
                    random_x_params.consensus_manager(),
                )
                .map_err(ValidationError::RandomXDifficulty)?
            },
            PowAlgorithm::Sha3x => {
                sha3x_difficulty(&block.original_block.header).map_err(ValidationError::Difficulty)?
            },
        };
        self.validate_min_difficulty(pow_algo, curr_difficulty, block.original_block.header.height)?;

        Ok(curr_difficulty)
    }

    /// Submits a new block to share chain.
    async fn submit_block_with_lock(
        &self,
        p2_chain: &mut RwLockWriteGuard<'_, P2Chain>,
        block: &P2Block,
        params: Option<Arc<BlockValidationParams>>,
        syncing: bool,
    ) -> Result<(), Error> {
        let new_block_p2pool_height = block.height;

        if p2_chain.get_tip().is_none() {
            if (syncing && block.height > MAX_BLOCKS_COUNT as u64) || block.height == 0 {
                p2_chain.add_block_to_chain(block.clone())?;
                return Ok(());
            } else {
                // we just received a propagated block and we dont have any blocks, we need to sync.
                return Err(Error::BlockParentDoesNotExist {
                    num_missing_parents: block.height.saturating_sub(SHARE_WINDOW as u64),
                });
            }
        }

        // this is safe as we already checked it does exist
        let tip_height = p2_chain.get_tip().unwrap().height;
        // We keep more blocks than the share window, but its only to validate the share window. If a block comes in
        // older than the share window is way too old for us to care about.
        if block.height < tip_height.saturating_sub(SHARE_WINDOW as u64) {
            return Err(Error::BlockValidation("Block is older than share window".to_string()));
        }

        // Check if already added.
        if let Some(level) = p2_chain.get_at_height(new_block_p2pool_height) {
            if level.blocks.contains_key(&block.hash) {
                info!(target: LOG_TARGET, "[{:?}] ✅ Block already added: {:?}", self.pow_algo, block.height);
                return Ok(());
            }
        }

        let parent = p2_chain
            .get_parent_block(block)
            .ok_or_else(|| Error::BlockParentDoesNotExist {
                num_missing_parents: block.height.saturating_sub(tip_height),
            })?;

        // validate
        let validate_result = self.validate_block(parent, block, params).await?;
        let mut new_block = block.clone();
        new_block.target_difficulty = validate_result;

        // add block to chain
        p2_chain.add_block_to_chain(new_block)?;

        // update coinbase extra cache
        let mut coinbase_extras_lock = self.coinbase_extras.write().await;

        coinbase_extras_lock.insert(block.miner_wallet_address.to_base58(), block.get_miner_coinbase_extra());

        Ok(())
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
    ) -> Result<HashMap<String, (u64, Vec<u8>)>, Error> {
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
            .ok_or(Error::BlockNotFound)?;
        update_insert(
            &mut miners_to_shares,
            cur_block.miner_wallet_address.to_base58(),
            MAIN_REWARD_SHARE,
            cur_block.miner_coinbase_extra.clone(),
        );
        for uncle in cur_block.uncles.iter() {
            let uncle_block = p2_chain
                .get_at_height(uncle.0)
                .ok_or_else(|| Error::UncleBlockNotFound)?
                .blocks
                .get(&uncle.1)
                .ok_or_else(|| Error::UncleBlockNotFound)?;
            update_insert(
                &mut miners_to_shares,
                uncle_block.miner_wallet_address.to_base58(),
                UNCLE_REWARD_SHARE,
                uncle_block.miner_coinbase_extra.clone(),
            );
        }
        if cur_block.height == stop_height {}
        while cur_block.height > stop_height {
            cur_block = p2_chain.get_parent_block(cur_block).ok_or(Error::BlockNotFound)?;
            update_insert(
                &mut miners_to_shares,
                cur_block.miner_wallet_address.to_base58(),
                MAIN_REWARD_SHARE,
                cur_block.miner_coinbase_extra.clone(),
            );
            for uncle in cur_block.uncles.iter() {
                let uncle_block = p2_chain
                    .get_at_height(uncle.0)
                    .ok_or_else(|| Error::UncleBlockNotFound)?
                    .blocks
                    .get(&uncle.1)
                    .ok_or_else(|| Error::UncleBlockNotFound)?;
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
    async fn submit_block(&self, block: &P2Block) -> Result<(), Error> {
        let mut p2_chain_write_lock = self.p2_chain.write().await;
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
        res
    }

    async fn add_synced_blocks(&self, blocks: &[P2Block]) -> Result<(), Error> {
        let mut p2_chain_write_lock = self.p2_chain.write().await;

        for block in blocks {
            dbg!(block.height);
            match self
                .submit_block_with_lock(
                    &mut p2_chain_write_lock,
                    &block,
                    self.block_validation_params.clone(),
                    true,
                )
                .await
            {
                Ok(_) => (),
                Err(e) => {
                    error!(target: LOG_TARGET, "Failed to add block: {}", e);
                    return Err(e);
                },
            }
        }
        let _ = self.stat_client.send_chain_changed(
            self.pow_algo,
            p2_chain_write_lock.get_height(),
            p2_chain_write_lock.get_max_chain_length() as u64,
        );
        Ok(())
    }

    async fn tip_height(&self) -> Result<u64, Error> {
        let bl = self.p2_chain.read().await;
        let tip_level = bl.get_height();
        Ok(tip_level)
    }

    async fn generate_shares(&self, new_tip_block: &P2Block) -> Result<Vec<NewBlockCoinbase>, Error> {
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
                .get_at_height(uncle.0)
                .ok_or_else(|| Error::UncleBlockNotFound)?
                .blocks
                .get(&uncle.1)
                .ok_or_else(|| Error::UncleBlockNotFound)?;
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
    ) -> Result<P2Block, Error> {
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
        let mut excluded_uncles = vec![];
        let mut uncles = vec![];
        for height in new_height.saturating_sub(3)..new_height {
            let older_level = chain_read_lock.get_at_height(height).ok_or(Error::BlockLevelNotFound)?;
            let chain_block = older_level.block_in_main_chain().ok_or(Error::BlockNotFound)?;
            for uncle in chain_block.uncles.iter() {
                excluded_uncles.push(uncle.1);
            }
            for block in older_level.blocks.iter() {
                if !excluded_uncles.contains(&block.0) {
                    uncles.push((height, block.0.clone()));
                }
            }
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

    async fn blocks(&self, from_height: u64) -> Result<Vec<P2Block>, Error> {
        // Should really only be used in syncing
        let p2_chain_read_lock = self.p2_chain.read().await;
        let mut res = vec![];
        // todo add is empty check
        // unwrap safe here
        let tip_height = p2_chain_read_lock.get_tip().unwrap().height;
        if tip_height < from_height {
            return Ok(res);
        }

        for height in (from_height..=tip_height).rev() {
            p2_chain_read_lock
                .get_at_height(height)
                .ok_or(Error::BlockNotFound)?
                .blocks
                .iter()
                .for_each(|(_, block)| {
                    res.push(block.clone());
                });
        }

        Ok(res)
    }

    async fn hash_rate(&self) -> Result<BigUint, Error> {
        Ok(BigUint::zero())
        // TODO: This calc is wrong
        // let p2_chain = self.p2_chain.read().await;
        // if p2_chain.is_empty() {
        //     return Ok(BigUint::zero());
        // }

        // let blocks = p2_chain
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

    async fn miners_with_shares(&self, _squad: Squad) -> Result<HashMap<String, (u64, Vec<u8>)>, Error> {
        let chain_read_lock = self.p2_chain.read().await;
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
        }
        Ok(miners_to_shares)
    }

    async fn get_target_difficulty(&self, height: u64) -> Difficulty {
        let min = self
            .consensus_manager
            .consensus_constants(height)
            .min_pow_difficulty(self.pow_algo);
        let max = self
            .consensus_manager
            .consensus_constants(height)
            .max_pow_difficulty(self.pow_algo);
        let chain_read_lock = self.p2_chain.read().await;

        let difficulty = chain_read_lock.lwma.get_difficulty().unwrap_or(Difficulty::min());
        cmp::max(min, cmp::min(max, difficulty))
    }
}

#[cfg(test)]
pub mod test {
    use tari_common::configuration::Network;
    use tari_common_types::{tari_address::TariAddressFeatures, types::BlockHash};
    use tari_crypto::{keys::PublicKey, ristretto::RistrettoPublicKey};

    use super::*;

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

        for i in 0..15 {
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

            share_chain.submit_block(&block).await.unwrap();
        }

        let shares = share_chain.miners_with_shares(Squad::default()).await.unwrap();
        assert_eq!(shares.len(), 15);
        for share in shares {
            assert_eq!(share.1, 5)
        }
    }

    #[tokio::test]
    async fn equal_share_same_participants() {
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
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .with_prev_hash(prev_hash)
                .build();

            prev_hash = block.generate_hash();

            share_chain.submit_block(&block).await.unwrap();
        }

        let shares = share_chain.miners_with_shares(Squad::default()).await.unwrap();
        assert_eq!(shares.len(), 5);
        for share in shares {
            assert_eq!(share.1, 15)
        }
    }

    #[tokio::test]
    async fn equal_share_same_participants_with_uncles() {
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
        let mut miners = Vec::new();
        for _ in 0..5 {
            let address = new_random_address();
            miners.push(address);
        }

        for i in 0..15 {
            let address = miners[i % 5].clone();
            timestamp = timestamp.checked_add(EpochTime::from(10)).unwrap();
            let mut uncles = Vec::new();
            if i > 1 {
                let prev_hash_uncle = share_chain
                    .p2_chain
                    .read()
                    .await
                    .get_at_height(i as u64 - 2)
                    .unwrap()
                    .chain_block;
                // lets create an uncle block
                let block = P2Block::builder()
                    .with_timestamp(timestamp)
                    .with_height(i as u64 - 1)
                    .with_miner_wallet_address(address.clone())
                    .with_target_difficulty(Difficulty::from_u64(9).unwrap())
                    .with_prev_hash(prev_hash_uncle)
                    .build();
                uncles.push((i as u64 - 1, block.hash));
                share_chain.submit_block(&block).await.unwrap();
            }
            let block = P2Block::builder()
                .with_timestamp(timestamp)
                .with_height(i as u64)
                .with_miner_wallet_address(address.clone())
                .with_target_difficulty(Difficulty::from_u64(10).unwrap())
                .with_uncles(uncles)
                .with_prev_hash(prev_hash)
                .build();

            prev_hash = block.generate_hash();

            share_chain.submit_block(&block).await.unwrap();
        }

        let shares = share_chain.miners_with_shares(Squad::default()).await.unwrap();
        assert_eq!(shares.len(), 5);
        // we have 3 miners with 27 shares and 2 with 23 shares
        // 27 = 3 *5 + 3*4; 23 = 3 *5 + 2
        let mut counter_27 = 0;
        let mut counter_23 = 0;
        for share in shares {
            match share.1 {
                27 => counter_27 += 1,
                23 => counter_23 += 1,
                _ => panic!("Should be 27 or 23"),
            }
        }
        assert_eq!(counter_27, 3);
        assert_eq!(counter_23, 2);
    }
}
