// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;

use tari_shutdown::ShutdownSignal;

use crate::cli::{
    args::{Cli, StartArgs},
    commands::util,
};

pub async fn handle_start(cli: Arc<Cli>, args: &StartArgs, cli_shutdown_signal: ShutdownSignal) -> anyhow::Result<()> {
    util::server(cli, args, cli_shutdown_signal, true)
        .await?
        .start()
        .await?;
    Ok(())
}
