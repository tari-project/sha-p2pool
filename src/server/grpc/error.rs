// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Tonic error: {0}")]
    Tonic(#[from] TonicError),
    #[error("No consensus constants found")]
    NoConsensusConstants,
    #[error("Shutdown")]
    Shutdown,
}

#[derive(Error, Debug)]
pub enum TonicError {
    #[error("Transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
}
