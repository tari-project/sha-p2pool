// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use clap::Parser;
use signal_hook::consts::{SIGINT, SIGQUIT, SIGTERM};
use signal_hook::iterator::Signals;
use tari_shutdown::Shutdown;

use crate::cli::Cli;

mod cli;
mod server;
mod sharechain;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // shutdown hook
    let mut cli_shutdown = Shutdown::new();
    let cli_shutdown_signal = cli_shutdown.to_signal();
    let mut signals = Signals::new([SIGINT, SIGTERM, SIGQUIT])?;
    let signals_handle = signals.handle();
    tokio::spawn(async move {
        let _ = signals.forever().next();
        cli_shutdown.trigger();
    });

    // cli
    Cli::parse().handle_command(cli_shutdown_signal).await?;
    signals_handle.close();
    Ok(())
}
