// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;

use tari_shutdown::Shutdown;

use crate::cli::{
    args::{Cli, StartArgs},
    commands::util,
};

pub async fn handle_diagnostics(cli: Arc<Cli>, args: &StartArgs, cli_shutdown: Shutdown) -> anyhow::Result<()> {
    let mut args = args.clone();
    set_diagnostic_mode(&mut args);
    util::server(cli, &args, cli_shutdown, true).await?.start().await?;
    Ok(())
}

pub fn set_diagnostic_mode(args: &mut StartArgs) {
    args.is_seed_peer = false;
    args.http_server_disabled = false;
    args.peer_publish_interval = Some(30);
    args.debug_print_chain = false;
    args.diagnostic_mode = true;
    args.randomx_disabled = true;
    args.sha3x_disabled = true;
    args.network_silence_delay = Some(u16::MAX);
}
