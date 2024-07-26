// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause


use std::sync::Arc;

use axum::Router;
use axum::routing::get;

use crate::server::http::stats::handlers;
use crate::sharechain::ShareChain;

pub struct StatsServer<S>
    where
        S: ShareChain + Send + Sync + 'static,
{
    router: Router,
    listener: tokio::net::TcpListener,
    share_chain: Arc<S>,
}

impl<S> StatsServer<S>
    where
        S: ShareChain + Send + Sync + 'static,
{
    pub async fn new(share_chain: Arc<S>, port: u16) -> Self {
        let router = Router::new().route("/stats", get(handlers::handle_get_stats));
        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await.unwrap(); // TODO: handle error
        Self {
            router,
            listener,
            share_chain,
        }
    }

    pub async fn start(&self) {
        axum::serve(self.listener, self.router).await.unwrap();
    }
}
