// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::num::TryFromIntError;

use tari_common_types::{tari_address::TariAddressError, types::FixedHashSizeError};
use tari_core::{
    consensus::ConsensusBuilderError,
    proof_of_work::{monero_rx::MergeMineError, DifficultyError},
};
use thiserror::Error;

use crate::sharechain::block::Block;

#[derive(Error, Debug)]
pub enum Error {
    #[error("gRPC Block conversion error: {0}")]
    BlockConvert(#[from] BlockConvertError),
    #[error("Share chain is empty!")]
    Empty,
    #[error("Tari address error: {0}")]
    TariAddress(#[from] TariAddressError),
    #[error("Invalid block: {0:?}")]
    InvalidBlock(Block),
    #[error("Number conversion error: {0}")]
    FromIntConversion(#[from] TryFromIntError),
    #[error("Difficulty calculation error: {0}")]
    Difficulty(#[from] DifficultyError),
    #[error("RandomX difficulty calculation error: {0}")]
    RandomXDifficulty(#[from] MergeMineError),
    #[error("Consensus builder error: {0}")]
    ConsensusBuilder(#[from] ConsensusBuilderError),
    #[error("Failed to convert to block hash: {0}")]
    BlockHashConversion(#[from] FixedHashSizeError),
    #[error("Block validation error: {0}")]
    BlockValidation(String),
    #[error("Block parent does not exist")]
    BlockParentDoesNotExist { num_missing_parents: u64 },
    #[error("The POW algo is not correct for this chain")]
    WrongPowAlgorithm,
}

#[derive(Error, Debug)]
pub enum BlockConvertError {
    #[error("Missing field: {0}")]
    MissingField(String),
    #[error("Converting gRPC block error: {0}")]
    GrpcBlockConvert(String),
}
