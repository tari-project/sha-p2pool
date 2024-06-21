use std::net::{AddrParseError, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;

use log::{error, info};
use minotari_app_grpc::tari_rpc::base_node_server::BaseNodeServer;
use minotari_app_grpc::tari_rpc::sha_p2_pool_server::ShaP2PoolServer;
use thiserror::Error;

use crate::server::{config, grpc, p2p};
use crate::server::grpc::base_node::TariBaseNodeGrpc;
use crate::server::grpc::error::TonicError;
use crate::server::grpc::p2pool::ShaP2PoolGrpc;
use crate::sharechain::ShareChain;

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
    where S: ShareChain + Send + Sync + 'static
{
    config: config::Config,
    p2p_service: p2p::Service<S>,
    base_node_grpc_service: BaseNodeServer<TariBaseNodeGrpc>,
    p2pool_grpc_service: ShaP2PoolServer<ShaP2PoolGrpc<S>>,
}

// TODO: add graceful shutdown
impl<S> Server<S>
    where S: ShareChain + Send + Sync + 'static
{
    pub async fn new(config: config::Config, share_chain: S) -> Result<Self, Error> {
        let share_chain = Arc::new(share_chain);
        let mut p2p_service: p2p::Service<S> = p2p::Service::new(&config, share_chain.clone()).map_err(Error::P2PService)?;

        let base_node_grpc_service = TariBaseNodeGrpc::new(config.base_node_address.clone()).await.map_err(Error::Grpc)?;
        let base_node_grpc_server = BaseNodeServer::new(base_node_grpc_service);

        let p2pool_grpc_service = ShaP2PoolGrpc::new(config.base_node_address.clone(), p2p_service.client(), share_chain.clone()).await.map_err(Error::Grpc)?;
        let p2pool_server = ShaP2PoolServer::new(p2pool_grpc_service);

        Ok(Self { config, p2p_service, base_node_grpc_service: base_node_grpc_server, p2pool_grpc_service: p2pool_server })
    }

    pub async fn start_grpc(
        base_node_service: BaseNodeServer<TariBaseNodeGrpc>,
        p2pool_service: ShaP2PoolServer<ShaP2PoolGrpc<S>>,
        grpc_port: u16,
    ) -> Result<(), Error> {
        info!("Starting gRPC server on port {}!", &grpc_port);

        tonic::transport::Server::builder()
            .add_service(base_node_service)
            .add_service(p2pool_service)
            .serve(
                SocketAddr::from_str(
                    format!("0.0.0.0:{}", grpc_port).as_str()
                ).map_err(Error::AddrParse)?
            )
            .await
            .map_err(|err| {
                error!("GRPC encountered an error: {:?}", err);
                Error::Grpc(grpc::error::Error::Tonic(TonicError::Transport(err)))
            })?;

        info!("gRPC server stopped!");

        Ok(())
    }

    pub async fn start(&mut self) -> Result<(), Error> {
        info!("Starting Tari SHA-3 mining P2Pool...");

        // local base node and p2pool node grpc services
        let base_node_grpc_service = self.base_node_grpc_service.clone();
        let p2pool_grpc_service = self.p2pool_grpc_service.clone();
        let grpc_port = self.config.grpc_port;
        tokio::spawn(async move {
            match Self::start_grpc(base_node_grpc_service, p2pool_grpc_service, grpc_port).await {
                Ok(_) => {}
                Err(error) => {
                    error!("GRPC Server encountered an error: {:?}", error);
                }
            }
        });

        self.p2p_service.start().await.map_err(Error::P2PService)
    }
}
