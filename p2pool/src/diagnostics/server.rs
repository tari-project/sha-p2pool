// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use anyhow::Error;
use libp2p::Swarm;
use log::{error, info};
use tari_shutdown::ShutdownSignal;

use crate::{
    diagnostics::{config, p2p},
    server::{
        http::stats_collector::{StatsBroadcastClient, StatsCollector},
        p2p::ServerNetworkBehaviour,
    },
};

const LOG_TARGET: &str = "tari::p2pool::diagnostics::server";

/// Server represents the server running all the necessary components for sha-p2pool.
pub(crate) struct Server {
    p2p_service: p2p::Service,
    stats_collector: Option<StatsCollector>,
}

impl Server {
    pub async fn new(
        config: config::Config,
        stats_collector: StatsCollector,
        stats_broadcast_client: StatsBroadcastClient,
        shutdown_signal: ShutdownSignal,
        swarm: Swarm<ServerNetworkBehaviour>,
        squad: String,
    ) -> Result<Self, Error> {
        let p2p_service = p2p::Service::new(
            &config,
            shutdown_signal.clone(),
            stats_broadcast_client.clone(),
            swarm,
            squad.clone(),
        )
        .await?;

        Ok(Self {
            p2p_service,
            stats_collector: Some(stats_collector),
        })
    }

    pub async fn start(&mut self) -> Result<(), Error> {
        info!(target: LOG_TARGET, "⛏ Starting Tari SHA-3 mining P2Pool...");

        let stats_server = self.stats_collector.take();
        if let Some(mut stats_server) = stats_server {
            tokio::spawn(async move {
                if let Err(err) = stats_server.run().await {
                    error!(target: LOG_TARGET, "Stats collector encountered an error: {:?}", err);
                }

                info!(target: LOG_TARGET, "Stats collector stopped!");
            });
        }

        self.p2p_service.start().await?;

        info!(target: LOG_TARGET, "Server stopped!");

        Ok(())
    }
}
