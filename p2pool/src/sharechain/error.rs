// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::num::TryFromIntError;

use tari_common_types::{
    tari_address::TariAddressError,
    types::{FixedHash, FixedHashSizeError},
};
use tari_core::{
    consensus::ConsensusBuilderError,
    proof_of_work::{monero_rx::MergeMineError, DifficultyError},
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ShareChainError {
    #[error("Tari address error: {0}")]
    TariAddress(#[from] TariAddressError),
    #[error("Invalid block: {reason}")]
    InvalidBlock { reason: String },
    #[error("Number conversion error: {0}")]
    FromIntConversion(#[from] TryFromIntError),
    #[error("Consensus builder error: {0}")]
    ConsensusBuilder(#[from] ConsensusBuilderError),
    #[error("Failed to convert to block hash: {0}")]
    BlockHashConversion(#[from] FixedHashSizeError),
    #[error("Block validation error: {0}")]
    BlockValidation(String),
    #[error("Difficulty calculation has overflowed")]
    DifficultyOverflow,
    #[error("Uncle block not found in chain")]
    UncleBlockNotFound,
    #[error("Block not found in chain")]
    BlockNotFound,
    #[error("Expected Block level not found in chain")]
    BlockLevelNotFound,
    #[error("Validation error: {0}")]
    ValidationError(#[from] ValidationError),
    #[error("Missing block validation params!")]
    MissingBlockValidationParams,
    #[error("Uncle block was in main chain. Height: {height}, Hash: {hash}")]
    UncleInMainChain { height: u64, hash: FixedHash },
    #[error("Uncle block does not link back to main chain")]
    UncleParentNotInMainChain,
    #[error("Block does not have correct total work accumulated")]
    BlockTotalWorkMismatch,
    #[error("Other: {0}")]
    Anyhow(#[from] anyhow::Error),
}

#[derive(Error, Debug)]
pub enum ValidationError {
    #[error("Block contains uncles that are too old")]
    UncleTooOld,
    #[error("Block contains uncles on the same height or higher")]
    UnclesOnSameHeightOrHigher,
    #[error("Block contains uncles before the uncle start height")]
    UnclesBeforeStartHeight,
    #[error("Block has too many uncles")]
    TooManyUncles,
    #[error("Proof of work algorithm does not match chain algorithm")]
    InvalidPowAlgorithm,
    #[error("Number conversion error: {0}")]
    FromIntConversion(#[from] TryFromIntError),
    #[error("Missing block validation params!")]
    MissingBlockValidationParams,
    #[error("Difficulty calculation error: {0}")]
    Difficulty(#[from] DifficultyError),
    #[error("RandomX difficulty calculation error: {0}")]
    RandomXDifficulty(#[from] MergeMineError),
    #[error("Block achieved difficulty is below the target")]
    DifficultyTarget,
}
