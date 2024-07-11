// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use async_trait::async_trait;
use minotari_app_grpc::tari_rpc::{NewBlockCoinbase, SubmitBlockRequest};

use crate::sharechain::{block::Block, error::Error};

pub const MAX_BLOCKS_COUNT: usize = 80;

pub const SHARE_COUNT: u64 = 100;

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

#[async_trait]
pub trait ShareChain {
    /// Adds a new block if valid to chain.
    async fn submit_block(&self, block: &Block) -> ShareChainResult<SubmitBlockResult>;

    /// Add multiple blocks at once.
    /// While this operation runs, no other blocks can be added until it's done.
    async fn submit_blocks(&self, blocks: Vec<Block>, sync: bool) -> ShareChainResult<SubmitBlockResult>;

    /// Returns the tip of height in chain (from original Tari block header)
    async fn tip_height(&self) -> ShareChainResult<u64>;

    /// Generate shares based on the previous blocks.
    async fn generate_shares(&self, reward: u64) -> Vec<NewBlockCoinbase>;

    /// Return a new block that could be added via `submit_block`.
    async fn new_block(&self, request: &SubmitBlockRequest) -> ShareChainResult<Block>;

    /// Returns blocks from the given height (`from_height`, exclusive).
    async fn blocks(&self, from_height: u64) -> ShareChainResult<Vec<Block>>;
}
