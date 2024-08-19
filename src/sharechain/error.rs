// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::num::TryFromIntError;

use tari_common_types::tari_address::TariAddressError;
use tari_core::consensus::ConsensusBuilderError;
use tari_core::proof_of_work::DifficultyError;
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
    #[error("Consensus builder error: {0}")]
    ConsensusBuilder(#[from] ConsensusBuilderError),
}

#[derive(Error, Debug)]
pub enum BlockConvertError {
    #[error("Missing field: {0}")]
    MissingField(String),
    #[error("Converting gRPC block error: {0}")]
    GrpcBlockConvert(String),
}
