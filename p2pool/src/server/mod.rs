// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

mod config;
pub use config::*;

#[allow(clippy::module_inception)]
pub mod server;

pub mod diagnostics;

pub mod grpc;
pub mod http;
pub mod p2p;

pub const PROTOCOL_VERSION: u64 = 33;
