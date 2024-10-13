// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

//! This module contains all the gRPC implementations to mimic a real Tari base node interface
//! and also expose the custom SHA-3 P2Pool related gRPC interfaces.
pub mod base_node;
pub mod error;
pub mod p2pool;
pub mod util;

pub(crate) const MAX_ACCEPTABLE_GRPC_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1000);
