use tonic::{Request, Response, Status};

use crate::sharechain::grpc::rpc::{GetBlockHeightTipRequest, GetBlockHeightTipResponse, SyncRequest};
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

#[tonic::async_trait]
impl<T> GrpcShareChain for ShareChainGrpc<T>
    where T: ShareChain + Send + Sync + 'static,
{
    async fn get_block_height_tip(&self, request: Request<GetBlockHeightTipRequest>) -> Result<Response<GetBlockHeightTipResponse>, Status> {
        Ok(Response::new(GetBlockHeightTipResponse {
            height: self.blockchain.tip_height().await,
        }))
    }

    type SyncStream = ();

    async fn sync(&self, request: Request<SyncRequest>) -> Result<Response<Self::SyncStream>, Status> {
        todo!()
    }
}