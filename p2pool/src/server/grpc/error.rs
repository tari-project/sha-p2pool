// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use minotari_app_grpc::authentication::BasicAuthError;
use thiserror::Error;
use tonic::transport::Error as TonicTransport;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Shutdown")]
    Shutdown,
    #[error("Tonic error: {0}")]
    TonicTransport(#[from] TonicTransport),
    #[error("Tonic error: {0}")]
    BasicAuth(#[from] BasicAuthError),
}
