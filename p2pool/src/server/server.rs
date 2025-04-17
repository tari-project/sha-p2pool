// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    net::SocketAddr,
    str::FromStr,
    sync::{atomic::AtomicBool, Arc},
};

use anyhow::Error;
use libp2p::Swarm;
use log::{error, info};
use minotari_app_grpc::tari_rpc::{base_node_server::BaseNodeServer, sha_p2_pool_server::ShaP2PoolServer};
use tari_common::configuration::Network;
use tari_core::{consensus::ConsensusManager, proof_of_work::randomx_factory::RandomXFactory};
use tari_shutdown::Shutdown;
use tokio::sync::mpsc;

use super::{
    http::stats_collector::{StatsBroadcastClient, StatsCollector},
    p2p::client::ServiceClient,
};
use crate::{
    server::{
        config,
        diagnostics::{DiagnosticsBroadcastClient, DiagnosticsCollector, DiagnosticsReceiverClient},
        grpc::{base_node::TariBaseNodeGrpc, p2pool::ShaP2PoolGrpc},
        http::server::HttpServer,
        p2p,
        p2p::ServerNetworkBehaviour,
    },
    sharechain::ShareChain,
};

const LOG_TARGET: &str = "tari::p2pool::server::server";

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
    diagnostics_collector: Option<DiagnosticsCollector>,
    shutdown: Shutdown,
    are_we_synced_with_randomx_p2pool: Arc<AtomicBool>,
    are_we_synced_with_sha3x_p2pool: Arc<AtomicBool>,
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
        diagnostics_collector: Option<DiagnosticsCollector>,
        diagnostics_broadcast_client: Option<DiagnosticsBroadcastClient>,
        diagnostics_receiver_client: Option<DiagnosticsReceiverClient>,
        shutdown: Shutdown,
        swarm: Swarm<ServerNetworkBehaviour>,
        squad: String,
    ) -> Result<Self, Error> {
        let share_chain_sha3x = Arc::new(share_chain_sha3x);
        let share_chain_random_x = Arc::new(share_chain_random_x);
        let are_we_synced_with_randomx_p2pool = Arc::new(AtomicBool::new(false));
        let are_we_synced_with_sha3x_p2pool = Arc::new(AtomicBool::new(false));
        let stats_client = stats_collector.create_client();

        let mut base_node_grpc_server = None;
        let mut p2pool_server = None;
        let randomx_factory = RandomXFactory::new(1);
        let consensus_manager = ConsensusManager::builder(Network::get_current_or_user_setting_or_default()).build()?;
        let genesis_block_hash = *consensus_manager.get_genesis_block().hash();
        let (broadcast_blocks_tx, broadcast_blocks_rx) = tokio::sync::mpsc::unbounded_channel();
        if !config.p2p_service.is_seed_peer {
            let base_node_grpc_service =
                TariBaseNodeGrpc::new(config.base_node_address.clone(), shutdown.clone()).await?;
            base_node_grpc_server = Some(BaseNodeServer::new(base_node_grpc_service));

            let p2pool_grpc_service = ShaP2PoolGrpc::new(
                config.base_node_address.clone(),
                ServiceClient::new(broadcast_blocks_tx),
                share_chain_sha3x.clone(),
                share_chain_random_x.clone(),
                randomx_factory,
                consensus_manager,
                genesis_block_hash,
                stats_broadcast_client.clone(),
                are_we_synced_with_randomx_p2pool.clone(),
                are_we_synced_with_sha3x_p2pool.clone(),
                squad.clone(),
                config.grpc_cache_time,
            )
            .await?;
            p2pool_server = Some(ShaP2PoolServer::new(p2pool_grpc_service));
        }

        let (query_tx, query_rx) = mpsc::channel(1000);
        let http_server = if config.http_server.enabled {
            Some(Arc::new(HttpServer::new(
                stats_client,
                config.http_server.port,
                query_tx,
                shutdown.clone(),
            )))
        } else {
            None
        };

        let p2p_service: p2p::Service<S> = p2p::Service::new(
            &config,
            share_chain_sha3x.clone(),
            share_chain_random_x.clone(),
            shutdown.clone(),
            are_we_synced_with_randomx_p2pool.clone(),
            are_we_synced_with_sha3x_p2pool.clone(),
            stats_broadcast_client.clone(),
            diagnostics_broadcast_client,
            diagnostics_receiver_client,
            config.share_window,
            swarm,
            squad.clone(),
            broadcast_blocks_rx,
            query_rx,
        )
        .await?;
        // let local_peer_id = p2p_service.local_peer_id();

        Ok(Self {
            config,
            p2p_service,
            base_node_grpc_service: base_node_grpc_server,
            p2pool_grpc_service: p2pool_server,
            http_server,
            stats_collector: Some(stats_collector),
            diagnostics_collector,
            shutdown,
            are_we_synced_with_randomx_p2pool,
            are_we_synced_with_sha3x_p2pool,
        })
    }

    pub async fn start_grpc(
        base_node_service: BaseNodeServer<TariBaseNodeGrpc>,
        p2pool_service: ShaP2PoolServer<ShaP2PoolGrpc<S>>,
        grpc_port: u16,
        shutdown: Shutdown,
    ) -> Result<(), Error> {
        info!(target: LOG_TARGET, "Starting gRPC server on port {}!", &grpc_port);

        let shutdown_signal = shutdown.to_signal();
        tonic::transport::Server::builder()
            .add_service(base_node_service)
            .add_service(p2pool_service)
            .serve_with_shutdown(
                SocketAddr::from_str(format!("0.0.0.0:{}", grpc_port).as_str())?,
                shutdown_signal,
            )
            .await?;

        info!(target: LOG_TARGET, "gRPC server stopped!");

        Ok(())
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

        let sync_start_sha3 = self.are_we_synced_with_sha3x_p2pool.clone();
        let sync_start_rx = self.are_we_synced_with_randomx_p2pool.clone();
        let time = self.config.network_silence_delay;
        tokio::spawn(async move {
            info!(target: LOG_TARGET, "Network silence, waiting for {} s ...", time);
            tokio::time::sleep(tokio::time::Duration::from_secs(time)).await;
            info!(target: LOG_TARGET, "Network silence done, Setting as synced");
            sync_start_sha3.store(true, std::sync::atomic::Ordering::SeqCst);
            sync_start_rx.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        if !self.config.p2p_service.is_seed_peer {
            // local base node and p2pool node grpc services
            let base_node_grpc_service = self.base_node_grpc_service.clone().unwrap();
            let p2pool_grpc_service = self.p2pool_grpc_service.clone().unwrap();
            let grpc_port = self.config.grpc_port;
            let shutdown = self.shutdown.clone();
            tokio::spawn(async move {
                if let Err(error) =
                    Self::start_grpc(base_node_grpc_service, p2pool_grpc_service, grpc_port, shutdown).await
                {
                    error!(target: LOG_TARGET, "GRPC Server encountered an error: {:?}", error);
                }
                info!(target: LOG_TARGET, "GRPC Server stopped!");
            });
        }

        let diagnostics_server = self.diagnostics_collector.take();
        if let Some(mut server) = diagnostics_server {
            tokio::spawn(async move {
                if let Err(err) = server.run().await {
                    error!(target: LOG_TARGET, "Diagnostics collector encountered an error: {:?}", err);
                }

                info!(target: LOG_TARGET, "Diagnostics collector stopped!");
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

        self.p2p_service.start().await?;

        info!(target: LOG_TARGET, "Server stopped!");

        Ok(())
    }
}
