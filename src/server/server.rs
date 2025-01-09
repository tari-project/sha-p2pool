// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    net::SocketAddr,
    str::FromStr,
    sync::{atomic::AtomicBool, Arc},
};

use anyhow::Error;
use log::{error, info};
use minotari_app_grpc::tari_rpc::{base_node_server::BaseNodeServer, sha_p2_pool_server::ShaP2PoolServer};
use tari_common::configuration::Network;
use tari_core::{consensus::ConsensusManager, proof_of_work::randomx_factory::RandomXFactory};
use tari_shutdown::ShutdownSignal;

use super::http::stats_collector::{StatsBroadcastClient, StatsCollector};
use crate::{
    server::{
        config,
        grpc::{base_node::TariBaseNodeGrpc, p2pool::ShaP2PoolGrpc},
        http::server::HttpServer,
        p2p,
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
    shutdown_signal: ShutdownSignal,
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
        shutdown_signal: ShutdownSignal,
    ) -> Result<Self, Error> {
        let share_chain_sha3x = Arc::new(share_chain_sha3x);
        let share_chain_random_x = Arc::new(share_chain_random_x);
        let are_we_synced_with_randomx_p2pool = Arc::new(AtomicBool::new(false));
        let are_we_synced_with_sha3x_p2pool = Arc::new(AtomicBool::new(false));
        let stats_client = stats_collector.create_client();

        let mut p2p_service: p2p::Service<S> = p2p::Service::new(
            &config,
            share_chain_sha3x.clone(),
            share_chain_random_x.clone(),
            shutdown_signal.clone(),
            are_we_synced_with_randomx_p2pool.clone(),
            are_we_synced_with_sha3x_p2pool.clone(),
            stats_broadcast_client.clone(),
        )
        .await?;
        let local_peer_id = p2p_service.local_peer_id();

        let mut base_node_grpc_server = None;
        let mut p2pool_server = None;
        let randomx_factory = RandomXFactory::new(1);
        let consensus_manager = ConsensusManager::builder(Network::get_current_or_user_setting_or_default()).build()?;
        let genesis_block_hash = *consensus_manager.get_genesis_block().hash();
        if !config.p2p_service.is_seed_peer {
            let base_node_grpc_service =
                TariBaseNodeGrpc::new(config.base_node_address.clone(), shutdown_signal.clone()).await?;
            base_node_grpc_server = Some(BaseNodeServer::new(base_node_grpc_service));

            let p2pool_grpc_service = ShaP2PoolGrpc::new(
                local_peer_id,
                config.base_node_address.clone(),
                p2p_service.client(),
                share_chain_sha3x.clone(),
                share_chain_random_x.clone(),
                shutdown_signal.clone(),
                randomx_factory,
                consensus_manager,
                genesis_block_hash,
                stats_broadcast_client.clone(),
                are_we_synced_with_randomx_p2pool.clone(),
                are_we_synced_with_sha3x_p2pool.clone(),
                p2p_service.squad.clone(),
            )
            .await?;
            p2pool_server = Some(ShaP2PoolServer::new(p2pool_grpc_service));
        }

        let query_client = p2p_service.create_query_client();
        let http_server = if config.http_server.enabled {
            Some(Arc::new(HttpServer::new(
                stats_client,
                config.http_server.port,
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
            are_we_synced_with_randomx_p2pool,
            are_we_synced_with_sha3x_p2pool,
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
                SocketAddr::from_str(format!("0.0.0.0:{}", grpc_port).as_str())?,
                shutdown_signal,
            )
            .await?;

        info!(target: LOG_TARGET, "gRPC server stopped!");

        Ok(())
    }

    pub async fn start(&mut self) -> Result<(), Error> {
        info!(target: LOG_TARGET, "⛏ Starting Tari SHA-3 mining P2Pool...");

        let sync_start_sha3 = self.are_we_synced_with_sha3x_p2pool.clone();
        let sync_start_rx = self.are_we_synced_with_randomx_p2pool.clone();
        let time = self.config.network_silence_delay;
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(time)).await;
            info!(target: LOG_TARGET, "Network silence, Setting as synced");
            sync_start_sha3.store(true, std::sync::atomic::Ordering::SeqCst);
            sync_start_rx.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        if !self.config.p2p_service.is_seed_peer {
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

        self.p2p_service.start().await?;

        info!(target: LOG_TARGET, "Server stopped!");

        Ok(())
    }
}
