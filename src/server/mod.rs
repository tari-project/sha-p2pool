// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

pub use config::*;

mod config;

#[allow(clippy::module_inception)]
pub mod server;

pub mod grpc;
pub mod http;
pub mod p2p;
