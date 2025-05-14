// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

mod cli;
mod server;
mod sharechain;
mod stratum;

pub const PROFILING_LOG_TARGET: &str = "tari::profiling";
pub use cli::{handle_start, run_with_cli, Cli, Commands, LibP2pInfo, StartArgs};
pub use server::{
    p2p::util::{generate_identity, GenerateIdentityResult},
    Config as ShaP2PoolConfig,
};
pub use sharechain::{lmdb_block_storage::LmdbBlockStorage, p2block::P2BlockBuilder, p2chain::P2Chain};
pub fn anyhow_error(msg: &str) -> anyhow::Error {
    anyhow::Error::new(std::io::Error::new(std::io::ErrorKind::Other, msg))
}
