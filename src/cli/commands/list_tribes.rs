// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use itertools::Itertools;
use tari_shutdown::{Shutdown, ShutdownSignal};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::{select, time};

use crate::cli::args::{Cli, StartArgs};
use crate::cli::commands::util;
use crate::server::p2p::peer_store::PeerStore;

pub async fn handle_list_tribes(
    cli: Arc<Cli>,
    args: &StartArgs,
    cli_shutdown_signal: ShutdownSignal,
) -> anyhow::Result<()> {
    // start server asynchronously
    let cli_ref = cli.clone();
    let mut args_clone = args.clone();
    args_clone.mining_disabled = true;
    args_clone.stats_server_disabled = true;
    let mut shutdown = Shutdown::new();
    let shutdown_signal = shutdown.to_signal();
    let (peer_store_channel_tx, peer_store_channel_rx) = oneshot::channel::<Arc<PeerStore>>();
    let handle: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
        let mut server = util::server(cli_ref, &args_clone, shutdown_signal, false).await?;
        match peer_store_channel_tx.send(server.p2p_service().network_peer_store().clone()) {
            Ok(_) => server.start().await?,
            Err(_) => return Err(anyhow!("Failed to start server")),
        }

        Ok(())
    });

    // wait for peer store from started server
    let peer_store = peer_store_channel_rx.await?;

    // collect tribes for 30 seconds
    let mut tribes = vec![];
    let timeout = time::sleep(Duration::from_secs(30));
    tokio::pin!(timeout);
    tokio::pin!(cli_shutdown_signal);
    loop {
        select! {
            _ = &mut cli_shutdown_signal => {
                break;
            }
            () = &mut timeout => {
                break;
            }
            current_tribes = peer_store.tribes() => {
                tribes = current_tribes;
                if tribes.len() > 1 {
                    break;
                }
            }
        }
    }
    shutdown.trigger();
    handle.await??;

    let tribes = tribes.iter().map(|tribe| tribe.to_string()).collect_vec();
    print!("{}", serde_json::to_value(tribes)?);

    Ok(())
}
