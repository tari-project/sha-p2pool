// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;

use axum::{routing::get, Router};
use log::info;
use tari_shutdown::ShutdownSignal;
use thiserror::Error;
use tokio::{io, sync::mpsc::Sender};

use super::stats_collector::StatsClient;
use crate::{
    server::{
        http::{health, stats::handlers, version},
        p2p::{peer_store::PeerStore, P2pServiceQuery, Squad},
    },
    sharechain::ShareChain,
};

const LOG_TARGET: &str = "tari::p2pool::server::stats";

#[derive(Clone, Debug)]
pub struct Config {
    pub enabled: bool,
    pub port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 19000,
        }
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O error: {0}")]
    IO(#[from] io::Error),
}

pub struct HttpServer {
    stats_client: StatsClient,
    port: u16,
    squad: Squad,
    p2p_service_client: Sender<P2pServiceQuery>,
    shutdown_signal: ShutdownSignal,
}

#[derive(Clone)]
pub struct AppState {
    stats_client: StatsClient,
    pub squad: Squad,
    pub p2p_service_client: Sender<P2pServiceQuery>,
}

impl HttpServer {
    pub fn new(
        stats_client: StatsClient,
        port: u16,
        squad: Squad,
        p2p_service_client: Sender<P2pServiceQuery>,
        shutdown_signal: ShutdownSignal,
    ) -> Self {
        Self {
            stats_client,
            port,
            squad,
            p2p_service_client,
            shutdown_signal,
        }
    }

    /// Routes adds all the needed paths for the axum router.
    pub fn routes(&self) -> Router {
        Router::new()
            .route("/stats", get(handlers::handle_get_stats))
            .route("/miners", get(handlers::handle_miners_with_shares))
            .route("/health", get(health::handle_health))
            .route("/version", get(version::handle_version))
            .route("/chain", get(handlers::handle_chain))
            .route("/connections", get(handlers::handle_connections))
            .with_state(AppState {
                stats_client: self.stats_client.clone(),
                squad: self.squad.clone(),
                p2p_service_client: self.p2p_service_client.clone(),
            })
    }

    /// Starts the stats http server on the port passed in ['StatsServer::new']
    pub async fn start(&self) -> Result<(), Error> {
        let router = self.routes();
        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", self.port))
            .await
            .map_err(Error::IO)?;
        info!(target: LOG_TARGET, "Starting Stats HTTP server at http://127.0.0.1:{}", self.port);
        axum::serve(listener, router)
            .with_graceful_shutdown(self.shutdown_signal.clone())
            .await
            .map_err(Error::IO)?;
        info!(target: LOG_TARGET, "Stats HTTP server stopped!");
        Ok(())
    }
}
