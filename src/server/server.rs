// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::atomic::AtomicBool;
use std::{
    net::{AddrParseError, SocketAddr},
    str::FromStr,
    sync::Arc,
};

use log::{error, info};
use minotari_app_grpc::tari_rpc::{base_node_server::BaseNodeServer, sha_p2_pool_server::ShaP2PoolServer};
use tari_shutdown::ShutdownSignal;
use thiserror::Error;

use crate::server::http::stats::server::StatsServer;
use crate::server::p2p::peer_store::PeerStore;
use crate::server::stats_store::StatsStore;
use crate::{
    server::{
        config, grpc,
        grpc::{base_node::TariBaseNodeGrpc, error::TonicError, p2pool::ShaP2PoolGrpc},
        p2p,
    },
    sharechain::ShareChain,
};

const LOG_TARGET: &str = "p2pool::server::server";

#[derive(Error, Debug)]
pub enum Error {
    #[error("P2P service error: {0}")]
    P2PService(#[from] p2p::Error),
    #[error("gRPC error: {0}")]
    Grpc(#[from] grpc::error::Error),
    #[error("Socket address parse error: {0}")]
    AddrParse(#[from] AddrParseError),
}

/// Server represents the server running all the necessary components for sha-p2pool.
pub struct Server<S>
where
    S: ShareChain,
{
    config: config::Config,
    p2p_service: p2p::Service<S>,
    base_node_grpc_service: Option<BaseNodeServer<TariBaseNodeGrpc>>,
    p2pool_grpc_service: Option<ShaP2PoolServer<ShaP2PoolGrpc<S>>>,
    stats_server: Option<Arc<StatsServer<S>>>,
    shutdown_signal: ShutdownSignal,
}

impl<S> Server<S>
where
    S: ShareChain,
{
    pub async fn new(config: config::Config, share_chain: S, shutdown_signal: ShutdownSignal) -> Result<Self, Error> {
        let share_chain = Arc::new(share_chain);
        let sync_in_progress = Arc::new(AtomicBool::new(true));
        let tribe_peer_store = Arc::new(PeerStore::new(&config.peer_store));
        let network_peer_store = Arc::new(PeerStore::new(&config.peer_store));
        let stats_store = Arc::new(StatsStore::new());

        let mut p2p_service: p2p::Service<S> = p2p::Service::new(
            &config,
            share_chain.clone(),
            tribe_peer_store.clone(),
            network_peer_store.clone(),
            sync_in_progress.clone(),
            shutdown_signal.clone(),
        )
        .await
        .map_err(Error::P2PService)?;

        let mut base_node_grpc_server = None;
        let mut p2pool_server = None;
        if config.mining_enabled {
            let base_node_grpc_service =
                TariBaseNodeGrpc::new(config.base_node_address.clone(), shutdown_signal.clone())
                    .await
                    .map_err(Error::Grpc)?;
            base_node_grpc_server = Some(BaseNodeServer::new(base_node_grpc_service));

            let p2pool_grpc_service = ShaP2PoolGrpc::new(
                config.base_node_address.clone(),
                p2p_service.client(),
                share_chain.clone(),
                stats_store.clone(),
                shutdown_signal.clone(),
            )
            .await
            .map_err(Error::Grpc)?;
            p2pool_server = Some(ShaP2PoolServer::new(p2pool_grpc_service));
        }

        let stats_server = if config.stats_server.enabled {
            Some(Arc::new(StatsServer::new(
                share_chain.clone(),
                tribe_peer_store.clone(),
                stats_store.clone(),
                config.stats_server.port,
                config.p2p_service.tribe.clone(),
                shutdown_signal.clone(),
            )))
        } else {
            None
        };

        Ok(Self {
            config,
            p2p_service,
            base_node_grpc_service: base_node_grpc_server,
            p2pool_grpc_service: p2pool_server,
            stats_server,
            shutdown_signal,
        })
    }

    pub async fn start_grpc(
        base_node_service: BaseNodeServer<TariBaseNodeGrpc>,
        p2pool_service: ShaP2PoolServer<ShaP2PoolGrpc<S>>,
        grpc_port: u16,
        shutdown_signal: ShutdownSignal,
    ) -> Result<(), Error> {
        info!(target: LOG_TARGET, "Starting gRPC server on port {}!", &grpc_port);

        tonic::transport::Server::builder()
            .add_service(base_node_service)
            .add_service(p2pool_service)
            .serve_with_shutdown(
                SocketAddr::from_str(format!("0.0.0.0:{}", grpc_port).as_str()).map_err(Error::AddrParse)?,
                shutdown_signal,
            )
            .await
            .map_err(|err| {
                error!(target: LOG_TARGET, "GRPC encountered an error: {:?}", err);
                Error::Grpc(grpc::error::Error::Tonic(TonicError::Transport(err)))
            })?;

        info!(target: LOG_TARGET, "gRPC server stopped!");

        Ok(())
    }

    pub async fn start(&mut self) -> Result<(), Error> {
        info!(target: LOG_TARGET, "⛏ Starting Tari SHA-3 mining P2Pool...");

        if self.config.mining_enabled {
            // local base node and p2pool node grpc services
            let base_node_grpc_service = self.base_node_grpc_service.clone().unwrap();
            let p2pool_grpc_service = self.p2pool_grpc_service.clone().unwrap();
            let grpc_port = self.config.grpc_port;
            let shutdown_signal = self.shutdown_signal.clone();
            tokio::spawn(async move {
                if let Err(error) =
                    Self::start_grpc(base_node_grpc_service, p2pool_grpc_service, grpc_port, shutdown_signal).await
                {
                    error!(target: LOG_TARGET, "GRPC Server encountered an error: {:?}", error);
                }
            });
        }

        if let Some(stats_server) = &self.stats_server {
            let stats_server = stats_server.clone();
            tokio::spawn(async move {
                if let Err(error) = stats_server.start().await {
                    error!(target: LOG_TARGET, "Stats HTTP server encountered an error: {:?}", error);
                }
            });
        }

        self.p2p_service.start().await.map_err(Error::P2PService)?;

        info!(target: LOG_TARGET, "Server stopped!");

        Ok(())
    }

    pub fn p2p_service(&self) -> &p2p::Service<S> {
        &self.p2p_service
    }
}
