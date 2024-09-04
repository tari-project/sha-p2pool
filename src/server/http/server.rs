// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use log::info;
use tari_shutdown::ShutdownSignal;
use thiserror::Error;
use tokio::io;

use crate::server::http::stats::handlers;
use crate::server::http::{health, version};
use crate::server::p2p::peer_store::PeerStore;
use crate::server::p2p::Tribe;
use crate::server::stats_store::StatsStore;
use crate::sharechain::ShareChain;

const LOG_TARGET: &str = "p2pool::server::stats";

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

pub struct HttpServer<S>
where
    S: ShareChain,
{
    share_chain_sha3x: Arc<S>,
    share_chain_random_x: Arc<S>,
    peer_store: Arc<PeerStore>,
    stats_store: Arc<StatsStore>,
    port: u16,
    tribe: Tribe,
    shutdown_signal: ShutdownSignal,
}

#[derive(Clone)]
pub struct AppState {
    pub share_chain_sha3x: Arc<dyn ShareChain>,
    pub share_chain_random_x: Arc<dyn ShareChain>,
    pub peer_store: Arc<PeerStore>,
    pub stats_store: Arc<StatsStore>,
    pub tribe: Tribe,
}

impl<S> HttpServer<S>
where
    S: ShareChain,
{
    pub fn new(
        share_chain_sha3x: Arc<S>,
        share_chain_random_x: Arc<S>,
        peer_store: Arc<PeerStore>,
        stats_store: Arc<StatsStore>,
        port: u16,
        tribe: Tribe,
        shutdown_signal: ShutdownSignal,
    ) -> Self {
        Self {
            share_chain_sha3x,
            share_chain_random_x,
            peer_store,
            stats_store,
            port,
            tribe,
            shutdown_signal,
        }
    }

    /// Routes adds all the needed paths for the axum router.
    pub fn routes(&self) -> Router {
        Router::new()
            .route("/stats", get(handlers::handle_get_stats))
            .route("/health", get(health::handle_health))
            .route("/version", get(version::handle_version))
            .with_state(AppState {
                share_chain_sha3x: self.share_chain_sha3x.clone(),
                share_chain_random_x: self.share_chain_random_x.clone(),
                peer_store: self.peer_store.clone(),
                stats_store: self.stats_store.clone(),
                tribe: self.tribe.clone(),
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
