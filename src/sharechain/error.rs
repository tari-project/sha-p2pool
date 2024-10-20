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
pub enum Error {
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
    #[error("Missing parents")]
    BlockParentDoesNotExist { missing_parents: Vec<(u64, FixedHash)> },
    #[error("Missing block validation params!")]
    MissingBlockValidationParams,
}

#[derive(Error, Debug)]
pub enum ValidationError {
    #[error("Proof of work algorithm does not match chain algorithm")]
    InvalidPowAlgorithm,
    #[error("Difficulty is below the allowed minimum")]
    DifficultyBelowMinimum,
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
