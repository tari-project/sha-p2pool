// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use clap::Parser;
use tari_shutdown::Shutdown;

use crate::cli::Cli;

mod cli;
mod server;
mod sharechain;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut cli_shutdown = Shutdown::new();
    let cli_shutdown_signal = cli_shutdown.to_signal();
    ctrlc::set_handler(move || cli_shutdown.trigger()).expect("Error setting termination handler");
    Cli::parse().handle_command(cli_shutdown_signal).await?;
    Ok(())
}
