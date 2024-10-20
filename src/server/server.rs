// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    net::{AddrParseError, SocketAddr},
    str::FromStr,
    sync::Arc,
};

use log::{error, info};
use minotari_app_grpc::tari_rpc::{base_node_server::BaseNodeServer, sha_p2_pool_server::ShaP2PoolServer};
use tari_common::configuration::Network;
use tari_core::{consensus::ConsensusManager, proof_of_work::randomx_factory::RandomXFactory};
use tari_shutdown::ShutdownSignal;
use thiserror::Error;

use super::http::stats_collector::{StatsBroadcastClient, StatsCollector};
use crate::{
    server::{
        config,
        grpc,
        grpc::{base_node::TariBaseNodeGrpc, error::TonicError, p2pool::ShaP2PoolGrpc},
        http::server::HttpServer,
        p2p,
        p2p::peer_store::PeerStore,
    },
    sharechain::ShareChain,
};

const LOG_TARGET: &str = "tari::p2pool::server::server";

#[derive(Error, Debug)]
pub(crate) enum Error {
    #[error("P2P service error: {0}")]
    P2PService(#[from] p2p::Error),
    #[error("gRPC error: {0}")]
    Grpc(#[from] grpc::error::Error),
    #[error("Socket address parse error: {0}")]
    AddrParse(#[from] AddrParseError),
    #[error("Consensus manager error: {0}")]
    ConsensusBuilder(#[from] tari_core::consensus::ConsensusBuilderError),
}

/// Server represents the server running all the necessary components for sha-p2pool.
pub(crate) struct Server<S>
where S: ShareChain
{
    config: config::Config,
    p2p_service: p2p::Service<S>,
    base_node_grpc_service: Option<BaseNodeServer<TariBaseNodeGrpc>>,
    p2pool_grpc_service: Option<ShaP2PoolServer<ShaP2PoolGrpc<S>>>,
    http_server: Option<Arc<HttpServer>>,
    stats_collector: Option<StatsCollector>,
    shutdown_signal: ShutdownSignal,
    stats_broadcast_client: StatsBroadcastClient,
}

impl<S> Server<S>
where S: ShareChain
{
    pub async fn new(
        config: config::Config,
        share_chain_sha3x: S,
        share_chain_random_x: S,
        stats_collector: StatsCollector,
        stats_broadcast_client: StatsBroadcastClient,
        shutdown_signal: ShutdownSignal,
    ) -> Result<Self, Error> {
        let share_chain_sha3x = Arc::new(share_chain_sha3x);
        let share_chain_random_x = Arc::new(share_chain_random_x);
        let network_peer_store = PeerStore::new(&config.peer_store, stats_broadcast_client.clone());

        let stats_client = stats_collector.create_client();

        let mut p2p_service: p2p::Service<S> = p2p::Service::new(
            &config,
            share_chain_sha3x.clone(),
            share_chain_random_x.clone(),
            network_peer_store,
            shutdown_signal.clone(),
        )
        .await
        .map_err(Error::P2PService)?;

        let mut base_node_grpc_server = None;
        let mut p2pool_server = None;
        let randomx_factory = RandomXFactory::new(1);
        let consensus_manager = ConsensusManager::builder(Network::get_current_or_user_setting_or_default()).build()?;
        let genesis_block_hash = *consensus_manager.get_genesis_block().hash();
        if config.mining_enabled {
            let base_node_grpc_service =
                TariBaseNodeGrpc::new(config.base_node_address.clone(), shutdown_signal.clone())
                    .await
                    .map_err(Error::Grpc)?;
            base_node_grpc_server = Some(BaseNodeServer::new(base_node_grpc_service));

            let p2pool_grpc_service = ShaP2PoolGrpc::new(
                config.base_node_address.clone(),
                p2p_service.client(),
                share_chain_sha3x.clone(),
                share_chain_random_x.clone(),
                shutdown_signal.clone(),
                randomx_factory,
                consensus_manager,
                genesis_block_hash,
                stats_broadcast_client.clone(),
                config.p2p_service.squad.clone(),
            )
            .await
            .map_err(Error::Grpc)?;
            p2pool_server = Some(ShaP2PoolServer::new(p2pool_grpc_service));
        }

        let query_client = p2p_service.create_query_client();
        let http_server = if config.http_server.enabled {
            Some(Arc::new(HttpServer::new(
                stats_client,
                config.http_server.port,
                config.p2p_service.squad.clone(),
                query_client,
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
            http_server,
            stats_collector: Some(stats_collector),
            shutdown_signal,
            stats_broadcast_client,
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
                info!(target: LOG_TARGET, "GRPC Server stopped!");
            });
        }

        let stats_server = self.stats_collector.take();
        if let Some(mut stats_server) = stats_server {
            tokio::spawn(async move {
                if let Err(err) = stats_server.run().await {
                    error!(target: LOG_TARGET, "Stats collector encountered an error: {:?}", err);
                }

                info!(target: LOG_TARGET, "Stats collector stopped!");
            });
        }

        if let Some(http_server) = &self.http_server {
            let http_server = http_server.clone();
            tokio::spawn(async move {
                if let Err(error) = http_server.start().await {
                    error!(target: LOG_TARGET, "Stats HTTP server encountered an error: {:?}", error);
                }
                info!(target: LOG_TARGET, "Stats HTTP server stopped!");
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
