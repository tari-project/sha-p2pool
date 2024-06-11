use libp2p::futures::channel::mpsc;
use tonic::{Request, Response, Status};

use crate::sharechain::grpc::rpc::{Block, SyncRequest};
use crate::sharechain::grpc::rpc::share_chain_server::ShareChain as GrpcShareChain;
use crate::sharechain::ShareChain;

pub mod rpc {
    tonic::include_proto!("tari.p2pool.sharechain.rpc");
}

#[derive(Debug)]
pub struct ShareChainGrpc<T>
    where T: ShareChain + Send + Sync + 'static,
{
    blockchain: T,
}

impl<T> ShareChainGrpc<T>
    where T: ShareChain + Send + Sync + 'static {
    pub fn new(blockchain: T) -> Self {
        Self {
            blockchain
        }
    }
}

#[tonic::async_trait]
impl<T> GrpcShareChain for ShareChainGrpc<T>
    where T: ShareChain + Send + Sync + 'static,
{
    type SyncStream = mpsc::Receiver<Result<Block, Status>>;

    async fn sync(&self, request: Request<SyncRequest>) -> Result<Response<Self::SyncStream>, Status> {
        todo!()
    }
}