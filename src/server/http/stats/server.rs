// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use log::info;
use thiserror::Error;
use tokio::io;

use crate::server::http::stats::handlers;
use crate::server::p2p::peer_store::PeerStore;
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

pub struct StatsServer<S>
    where
        S: ShareChain,
{
    share_chain: Arc<S>,
    port: u16,
}

#[derive(Clone)]
pub struct AppState {
    pub share_chain: Arc<dyn ShareChain>,
}

impl<S> StatsServer<S>
    where
        S: ShareChain,
{
    pub fn new(share_chain: Arc<S>, port: u16) -> Self {
        Self { share_chain, port }
    }

    /// Routes adds all the needed paths for the axum router.
    pub fn routes(&self) -> Router {
        Router::new()
            .route("/stats", get(handlers::handle_get_stats))
            .with_state(AppState {
                share_chain: self.share_chain.clone(),
            })
    }

    /// Starts the stats http server on the port passed in ['StatsServer::new']
    pub async fn start(&self) -> Result<(), Error> {
        let router = self.routes();
        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", self.port))
            .await
            .map_err(Error::IO)?;
        info!("Starting Stats HTTP server at http://127.0.0.1:{}", self.port);
        axum::serve(listener, router).await.map_err(Error::IO)
    }
}
