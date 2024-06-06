use std::future::Future;
use std::sync::Arc;

use libp2p::futures::channel::mpsc;
use libp2p::futures::channel::mpsc::SendError;
use libp2p::futures::SinkExt;
use log::{error, info, warn};
use minotari_app_grpc::tari_rpc;
use minotari_app_grpc::tari_rpc::{Block, BlockBlobRequest, BlockGroupRequest, BlockGroupResponse, BlockHeaderResponse, BlockHeight, BlockTimingResponse, ConsensusConstants, Empty, FetchMatchingUtxosRequest, GetActiveValidatorNodesRequest, GetBlocksRequest, GetHeaderByHashRequest, GetMempoolTransactionsRequest, GetNewBlockBlobResult, GetNewBlockResult, GetNewBlockTemplateWithCoinbasesRequest, GetNewBlockWithCoinbasesRequest, GetPeersRequest, GetShardKeyRequest, GetShardKeyResponse, GetSideChainUtxosRequest, GetTemplateRegistrationsRequest, HeightRequest, ListConnectedPeersResponse, ListHeadersRequest, MempoolStatsResponse, NetworkStatusResponse, NewBlockCoinbase, NewBlockTemplate, NewBlockTemplateRequest, NewBlockTemplateResponse, NodeIdentity, PowAlgo, SearchKernelsRequest, SearchUtxosRequest, SoftwareUpdate, StringValue, SubmitBlockResponse, SubmitTransactionRequest, SubmitTransactionResponse, SyncInfoResponse, SyncProgressResponse, TipInfoResponse, TransactionStateRequest, TransactionStateResponse};
use minotari_app_grpc::tari_rpc::base_node_client::BaseNodeClient;
use minotari_app_grpc::tari_rpc::pow_algo::PowAlgos;
use minotari_node_grpc_client::BaseNodeGrpcClient;
use tari_common_types::tari_address::TariAddress;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};

use crate::server::grpc::error::{Error, TonicError};

const LIST_HEADERS_PAGE_SIZE: usize = 10;

pub struct TariBaseNodeGrpc {
    client: Arc<Mutex<BaseNodeClient<tonic::transport::Channel>>>,
}

impl TariBaseNodeGrpc {
    pub async fn new(base_node_address: String) -> Result<Self, Error> {
        // TODO: add retry mechanism to try at least 3 times before failing
        let client = BaseNodeGrpcClient::connect(base_node_address)
            .await
            .map_err(|e| Error::Tonic(TonicError::Transport(e)))?;

        Ok(Self { client: Arc::new(Mutex::new(client)) })
    }
}

#[tonic::async_trait]
impl tari_rpc::base_node_server::BaseNode for TariBaseNodeGrpc {
    type FetchMatchingUtxosStream = mpsc::Receiver<Result<tari_rpc::FetchMatchingUtxosResponse, Status>>;
    type GetActiveValidatorNodesStream = mpsc::Receiver<Result<tari_rpc::GetActiveValidatorNodesResponse, Status>>;
    type GetBlocksStream = mpsc::Receiver<Result<tari_rpc::HistoricalBlock, Status>>;
    type GetMempoolTransactionsStream = mpsc::Receiver<Result<tari_rpc::GetMempoolTransactionsResponse, Status>>;
    type GetNetworkDifficultyStream = mpsc::Receiver<Result<tari_rpc::NetworkDifficultyResponse, Status>>;
    type GetPeersStream = mpsc::Receiver<Result<tari_rpc::GetPeersResponse, Status>>;
    type GetSideChainUtxosStream = mpsc::Receiver<Result<tari_rpc::GetSideChainUtxosResponse, Status>>;
    type GetTemplateRegistrationsStream = mpsc::Receiver<Result<tari_rpc::GetTemplateRegistrationResponse, Status>>;
    type GetTokensInCirculationStream = mpsc::Receiver<Result<tari_rpc::ValueAtHeightResponse, Status>>;
    type ListHeadersStream = mpsc::Receiver<Result<tari_rpc::BlockHeaderResponse, Status>>;
    type SearchKernelsStream = mpsc::Receiver<Result<tari_rpc::HistoricalBlock, Status>>;
    type SearchUtxosStream = mpsc::Receiver<Result<tari_rpc::HistoricalBlock, Status>>;

    async fn get_new_block_template(&self, request: Request<NewBlockTemplateRequest>) -> Result<Response<NewBlockTemplateResponse>, Status> {
        info!("get_new_block_template called!");
        self.client.lock().await.get_new_block_template(request.into_inner()).await
    }

    async fn get_new_block(&self, request: Request<NewBlockTemplate>) -> Result<Response<GetNewBlockResult>, Status> {
        info!("get_new_block called!");
        // TODO: remove extra logic and only proxy, move logic to the new p2pool grpc handler
        // let origin_block_template = request.into_inner();
        // let origin = origin_block_template.clone();
        // if let Some(header) = origin_block_template.header {
        //     if let Some(body) = origin_block_template.body {
        //         if let Some(pow) = header.pow {
        //
        //             // simply proxy the request if pow algo is not supported
        //             if pow.pow_algo != PowAlgos::Sha3x as u64 {
        //                 warn!("Only SHA3x PoW supported!");
        //                 return self.client.lock().await.get_new_block(origin).await;
        //             }
        //
        //             // requesting new block template which includes all shares
        //             let mut new_block_template_req = GetNewBlockTemplateWithCoinbasesRequest::default();
        //             let mut new_pow_algo = PowAlgo::default();
        //             new_pow_algo.set_pow_algo(PowAlgos::Sha3x);
        //             new_block_template_req.algo = Some(new_pow_algo);
        //             new_block_template_req.coinbases = vec![
        //                 NewBlockCoinbase {
        //                     address: TariAddress::from_hex("30a815df7b8d7f653ce3252f08a21d570b1ac44958cb4d7af0e0ef124f89b11943")
        //                         .unwrap()
        //                         .to_hex(),
        //                     value: 1,
        //                     stealth_payment: false,
        //                     revealed_value_proof: true,
        //                     coinbase_extra: Vec::new(),
        //                 },
        //             ];
        //             if let Ok(response) = self.client.lock().await
        //                 .get_new_block_template_with_coinbases(new_block_template_req).await {}
        //         }
        //     }
        // }
        // todo!()
        self.client.lock().await.get_new_block(request.into_inner()).await
    }

    async fn submit_block(&self, request: Request<Block>) -> Result<Response<SubmitBlockResponse>, Status> {
        info!("submit_block called!");
        self.client.lock().await.submit_block(request.into_inner()).await
    }

    async fn list_headers(&self, request: Request<ListHeadersRequest>) -> Result<Response<Self::ListHeadersStream>, Status> {
        match self.client.lock().await.list_headers(request.into_inner()).await {
            Ok(response) => {
                let (mut tx, rx) = mpsc::channel(LIST_HEADERS_PAGE_SIZE);
                tokio::spawn(async move {
                    let mut stream = response.into_inner();
                    tokio::spawn(async move {
                        while let Ok(Some(next_message)) = stream.message().await {
                            if let Err(e) = tx.send(Ok(next_message)).await {
                                error!("failed to send 'list_headers' response message: {e}");
                            }
                        }
                    });
                });
                Ok(Response::new(rx))
            }
            Err(status) => Err(status)
        }
    }

    async fn get_header_by_hash(&self, request: Request<GetHeaderByHashRequest>) -> Result<Response<BlockHeaderResponse>, Status> {
        todo!()
    }

    async fn get_blocks(&self, request: Request<GetBlocksRequest>) -> Result<Response<Self::GetBlocksStream>, Status> {
        todo!()
    }

    async fn get_block_timing(&self, request: Request<HeightRequest>) -> Result<Response<BlockTimingResponse>, Status> {
        todo!()
    }

    async fn get_constants(&self, request: Request<BlockHeight>) -> Result<Response<ConsensusConstants>, Status> {
        todo!()
    }

    async fn get_block_size(&self, request: Request<BlockGroupRequest>) -> Result<Response<BlockGroupResponse>, Status> {
        todo!()
    }

    async fn get_block_fees(&self, request: Request<BlockGroupRequest>) -> Result<Response<BlockGroupResponse>, Status> {
        todo!()
    }

    async fn get_version(&self, request: Request<Empty>) -> Result<Response<StringValue>, Status> {
        todo!()
    }

    async fn check_for_updates(&self, request: Request<Empty>) -> Result<Response<SoftwareUpdate>, Status> {
        todo!()
    }

    async fn get_tokens_in_circulation(&self, request: Request<GetBlocksRequest>) -> Result<Response<Self::GetTokensInCirculationStream>, Status> {
        todo!()
    }

    async fn get_network_difficulty(&self, request: Request<HeightRequest>) -> Result<Response<Self::GetNetworkDifficultyStream>, Status> {
        todo!()
    }

    async fn get_new_block_with_coinbases(&self, request: Request<GetNewBlockWithCoinbasesRequest>) -> Result<Response<GetNewBlockResult>, Status> {
        todo!()
    }

    async fn get_new_block_template_with_coinbases(&self, request: Request<GetNewBlockTemplateWithCoinbasesRequest>) -> Result<Response<GetNewBlockResult>, Status> {
        todo!()
    }

    async fn get_new_block_blob(&self, request: Request<NewBlockTemplate>) -> Result<Response<GetNewBlockBlobResult>, Status> {
        todo!()
    }

    async fn submit_block_blob(&self, request: Request<BlockBlobRequest>) -> Result<Response<SubmitBlockResponse>, Status> {
        todo!()
    }

    async fn submit_transaction(&self, request: Request<SubmitTransactionRequest>) -> Result<Response<SubmitTransactionResponse>, Status> {
        todo!()
    }

    async fn get_sync_info(&self, request: Request<Empty>) -> Result<Response<SyncInfoResponse>, Status> {
        todo!()
    }

    async fn get_sync_progress(&self, request: Request<Empty>) -> Result<Response<SyncProgressResponse>, Status> {
        todo!()
    }

    async fn get_tip_info(&self, request: Request<Empty>) -> Result<Response<TipInfoResponse>, Status> {
        self.client.lock().await.get_tip_info(request.into_inner()).await
    }

    async fn search_kernels(&self, request: Request<SearchKernelsRequest>) -> Result<Response<Self::SearchKernelsStream>, Status> {
        todo!()
    }

    async fn search_utxos(&self, request: Request<SearchUtxosRequest>) -> Result<Response<Self::SearchUtxosStream>, Status> {
        todo!()
    }

    async fn fetch_matching_utxos(&self, request: Request<FetchMatchingUtxosRequest>) -> Result<Response<Self::FetchMatchingUtxosStream>, Status> {
        todo!()
    }

    async fn get_peers(&self, request: Request<GetPeersRequest>) -> Result<Response<Self::GetPeersStream>, Status> {
        todo!()
    }

    async fn get_mempool_transactions(&self, request: Request<GetMempoolTransactionsRequest>) -> Result<Response<Self::GetMempoolTransactionsStream>, Status> {
        todo!()
    }

    async fn transaction_state(&self, request: Request<TransactionStateRequest>) -> Result<Response<TransactionStateResponse>, Status> {
        todo!()
    }

    async fn identify(&self, request: Request<Empty>) -> Result<Response<NodeIdentity>, Status> {
        todo!()
    }

    async fn get_network_status(&self, request: Request<Empty>) -> Result<Response<NetworkStatusResponse>, Status> {
        todo!()
    }

    async fn list_connected_peers(&self, request: Request<Empty>) -> Result<Response<ListConnectedPeersResponse>, Status> {
        todo!()
    }

    async fn get_mempool_stats(&self, request: Request<Empty>) -> Result<Response<MempoolStatsResponse>, Status> {
        todo!()
    }

    async fn get_active_validator_nodes(&self, request: Request<GetActiveValidatorNodesRequest>) -> Result<Response<Self::GetActiveValidatorNodesStream>, Status> {
        todo!()
    }

    async fn get_shard_key(&self, request: Request<GetShardKeyRequest>) -> Result<Response<GetShardKeyResponse>, Status> {
        todo!()
    }

    async fn get_template_registrations(&self, request: Request<GetTemplateRegistrationsRequest>) -> Result<Response<Self::GetTemplateRegistrationsStream>, Status> {
        todo!()
    }

    async fn get_side_chain_utxos(&self, request: Request<GetSideChainUtxosRequest>) -> Result<Response<Self::GetSideChainUtxosStream>, Status> {
        todo!()
    }
}