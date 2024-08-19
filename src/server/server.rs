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
use tari_common::configuration::Network;
use tari_core::consensus::ConsensusManager;
use tari_core::proof_of_work::randomx_factory::RandomXFactory;
use thiserror::Error;

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
    #[error("Consensus manager error: {0}")]
    ConsensusBuilderError(#[from] tari_core::consensus::ConsensusBuilderError),
}

/// Server represents the server running all the necessary components for sha-p2pool.
pub struct Server<S>
where
    S: ShareChain + Send + Sync + 'static,
{
    config: config::Config,
    p2p_service: p2p::Service<S>,
    base_node_grpc_service: Option<BaseNodeServer<TariBaseNodeGrpc>>,
    p2pool_grpc_service: Option<ShaP2PoolServer<ShaP2PoolGrpc<S>>>,
}

// TODO: add graceful shutdown
impl<S> Server<S>
where
    S: ShareChain + Send + Sync + 'static,
{
    pub async fn new(config: config::Config, share_chain: S) -> Result<Self, Error> {
        let share_chain = Arc::new(share_chain);
        let sync_in_progress = Arc::new(AtomicBool::new(true));

        let mut p2p_service: p2p::Service<S> =
            p2p::Service::new(&config, share_chain.clone(), sync_in_progress.clone())
                .await
                .map_err(Error::P2PService)?;

        let mut base_node_grpc_server = None;
        let mut p2pool_server = None;
        let randomx_factory = RandomXFactory::new(1);
        let consensus_manager = ConsensusManager::builder(Network::default()).build()?;
        let genesis_block_hash = consensus_manager.get_genesis_block().hash().clone();
        if config.mining_enabled {
            let base_node_grpc_service = TariBaseNodeGrpc::new(config.base_node_address.clone())
                .await
                .map_err(Error::Grpc)?;
            base_node_grpc_server = Some(BaseNodeServer::new(base_node_grpc_service));

            let p2pool_grpc_service = ShaP2PoolGrpc::new(
                config.base_node_address.clone(),
                p2p_service.client(),
                share_chain.clone(),
                sync_in_progress.clone(),
                randomx_factory,
                consensus_manager,
                genesis_block_hash,
            )
            .await
            .map_err(Error::Grpc)?;
            p2pool_server = Some(ShaP2PoolServer::new(p2pool_grpc_service));
        }

        Ok(Self {
            config,
            p2p_service,
            base_node_grpc_service: base_node_grpc_server,
            p2pool_grpc_service: p2pool_server,
        })
    }

    pub async fn start_grpc(
        base_node_service: BaseNodeServer<TariBaseNodeGrpc>,
        p2pool_service: ShaP2PoolServer<ShaP2PoolGrpc<S>>,
        grpc_port: u16,
    ) -> Result<(), Error> {
        info!(target: LOG_TARGET, "Starting gRPC server on port {}!", &grpc_port);

        tonic::transport::Server::builder()
            .add_service(base_node_service)
            .add_service(p2pool_service)
            .serve(SocketAddr::from_str(format!("0.0.0.0:{}", grpc_port).as_str()).map_err(Error::AddrParse)?)
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
            tokio::spawn(async move {
                match Self::start_grpc(base_node_grpc_service, p2pool_grpc_service, grpc_port).await {
                    Ok(_) => {},
                    Err(error) => {
                        error!(target: LOG_TARGET, "GRPC Server encountered an error: {:?}", error);
                    },
                }
            });
        }

        self.p2p_service.start().await.map_err(Error::P2PService)
    }
}
