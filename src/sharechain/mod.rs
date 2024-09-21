// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::collections::HashMap;

use async_trait::async_trait;
use minotari_app_grpc::tari_rpc::{NewBlockCoinbase, SubmitBlockRequest};
use num::BigUint;
use tari_common_types::types::FixedHash;
use tari_core::{consensus::ConsensusManager, proof_of_work::randomx_factory::RandomXFactory};

use crate::sharechain::{block::Block, error::Error};

/// Chain ID is an identifier which makes sure we apply the same rules to blocks.
/// Note: This must be updated when new logic applied to blocks handling.
pub const CHAIN_ID: usize = 1;

/// How many blocks to keep overall.
pub const MAX_BLOCKS_COUNT: usize = 4000;

/// How many blocks are used to calculate current shares to be paid out.
pub const BLOCKS_WINDOW: usize = 4000;

pub const SHARE_COUNT: u64 = 2000 * 5; // 2000 blocks * MAIN_CHAIN_SHARE_AMOUNT
pub const MAX_SHARES_PER_MINER: u64 = u64::MAX;

// The reward that the miner who finds the block recieves
pub const MINER_REWARD_SHARES: u64 = 200;

// If the share is in the main chain, then it counts 100%
pub const MAIN_CHAIN_SHARE_AMOUNT: u64 = 5;

// Uncle blocks count 40%
pub const UNCLE_BLOCK_SHARE_AMOUNT: u64 = 2;

pub mod block;
pub mod error;
pub mod in_memory;

pub type ShareChainResult<T> = Result<T, Error>;

pub struct SubmitBlockResult {
    pub need_sync: bool,
}

impl SubmitBlockResult {
    pub fn new(need_sync: bool) -> Self {
        Self { need_sync }
    }
}

pub struct ValidateBlockResult {
    pub valid: bool,
    pub need_sync: bool,
}

impl ValidateBlockResult {
    pub fn new(valid: bool, need_sync: bool) -> Self {
        Self { valid, need_sync }
    }
}

pub struct BlockValidationParams {
    random_x_factory: RandomXFactory,
    consensus_manager: ConsensusManager,
    genesis_block_hash: FixedHash,
}

impl BlockValidationParams {
    pub fn new(
        random_x_factory: RandomXFactory,
        consensus_manager: ConsensusManager,
        genesis_block_hash: FixedHash,
    ) -> Self {
        Self {
            random_x_factory,
            consensus_manager,
            genesis_block_hash,
        }
    }

    pub fn random_x_factory(&self) -> &RandomXFactory {
        &self.random_x_factory
    }

    pub fn consensus_manager(&self) -> &ConsensusManager {
        &self.consensus_manager
    }

    pub fn genesis_block_hash(&self) -> &FixedHash {
        &self.genesis_block_hash
    }
}

#[async_trait]
pub(crate) trait ShareChain: Send + Sync + 'static {
    /// Adds a new block if valid to chain.
    async fn submit_block(&self, block: &Block) -> ShareChainResult<SubmitBlockResult>;

    /// Add multiple blocks at once.
    /// While this operation runs, no other blocks can be added until it's done.
    async fn submit_blocks(&self, blocks: Vec<Block>, sync: bool) -> ShareChainResult<SubmitBlockResult>;

    /// Returns the tip of height in chain (from original Tari block header)
    async fn tip_height(&self) -> ShareChainResult<u64>;

    /// Generate shares based on the previous blocks.
    async fn generate_shares(&self) -> Vec<NewBlockCoinbase>;

    /// Return a new block that could be added via `submit_block`.
    async fn new_block(&self, request: &SubmitBlockRequest) -> ShareChainResult<Block>;

    /// Returns blocks from the given height (`from_height`, exclusive).
    async fn blocks(&self, from_height: u64) -> ShareChainResult<Vec<Block>>;

    /// Returns the estimated hash rate of the whole chain
    /// (including all blocks and not just strongest chain).
    /// Returning number is the result in hash/second.
    async fn hash_rate(&self) -> ShareChainResult<BigUint>;

    /// Returns the current miners with all the current shares in the current blocks window.
    async fn miners_with_shares(&self) -> ShareChainResult<HashMap<String, u64>>;
}
