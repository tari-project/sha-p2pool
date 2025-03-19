// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;

use tari_shutdown::Shutdown;

use crate::cli::{
    args::{Cli, StartArgs},
    commands::util,
};

pub async fn handle_start(cli: Arc<Cli>, args: &StartArgs, cli_shutdown: Shutdown) -> anyhow::Result<()> {
    util::server(cli, args, cli_shutdown, true).await?.start().await?;
    Ok(())
}
