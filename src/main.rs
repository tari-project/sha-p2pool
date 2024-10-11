// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause
#![feature(assert_matches)]
use clap::Parser;
use tari_shutdown::Shutdown;

use crate::cli::Cli;

mod cli;
mod server;
mod sharechain;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    Cli::parse().handle_command(Shutdown::new().to_signal()).await?;
    Ok(())
}
