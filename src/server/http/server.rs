// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;

use axum::{routing::get, Router};
use digest::consts::P2;
use log::info;
use tari_shutdown::ShutdownSignal;
use thiserror::Error;
use tokio::{io, sync::mpsc::Sender};

use crate::{
    server::{
        http::{
            health,
            stats::{cache::StatsCache, handlers},
            version,
        },
        p2p::{peer_store::PeerStore, P2pServiceQuery, Squad},
        stats_store::StatsStore,
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

pub struct HttpServer<S>
where S: ShareChain
{
    share_chain_sha3x: Arc<S>,
    share_chain_random_x: Arc<S>,
    peer_store: Arc<PeerStore>,
    stats_store: Arc<StatsStore>,
    port: u16,
    squad: Squad,
    stats_cache: Arc<StatsCache>,
    p2p_service_client: Sender<P2pServiceQuery>,
    shutdown_signal: ShutdownSignal,
}

#[derive(Clone)]
pub struct AppState {
    pub share_chain_sha3x: Arc<dyn ShareChain>,
    pub share_chain_random_x: Arc<dyn ShareChain>,
    pub peer_store: Arc<PeerStore>,
    pub stats_store: Arc<StatsStore>,
    pub squad: Squad,
    pub p2p_service_client: Sender<P2pServiceQuery>,
    pub stats_cache: Arc<StatsCache>,
}

impl<S> HttpServer<S>
where S: ShareChain
{
    pub fn new(
        share_chain_sha3x: Arc<S>,
        share_chain_random_x: Arc<S>,
        peer_store: Arc<PeerStore>,
        stats_store: Arc<StatsStore>,
        port: u16,
        squad: Squad,
        stats_cache: Arc<StatsCache>,
        p2p_service_client: Sender<P2pServiceQuery>,
        shutdown_signal: ShutdownSignal,
    ) -> Self {
        Self {
            share_chain_sha3x,
            share_chain_random_x,
            peer_store,
            stats_store,
            port,
            squad,
            stats_cache,
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
                share_chain_sha3x: self.share_chain_sha3x.clone(),
                share_chain_random_x: self.share_chain_random_x.clone(),
                peer_store: self.peer_store.clone(),
                stats_store: self.stats_store.clone(),
                squad: self.squad.clone(),
                p2p_service_client: self.p2p_service_client.clone(),
                stats_cache: self.stats_cache.clone(),
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
