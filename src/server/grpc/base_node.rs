use std::sync::Arc;

use libp2p::futures::channel::mpsc;
use libp2p::futures::SinkExt;
use log::error;
use minotari_app_grpc::tari_rpc;
use minotari_app_grpc::tari_rpc::{Block, BlockBlobRequest, BlockGroupRequest, BlockGroupResponse, BlockHeaderResponse, BlockHeight, BlockTimingResponse, ConsensusConstants, Empty, FetchMatchingUtxosRequest, GetActiveValidatorNodesRequest, GetBlocksRequest, GetHeaderByHashRequest, GetMempoolTransactionsRequest, GetNewBlockBlobResult, GetNewBlockResult, GetNewBlockTemplateWithCoinbasesRequest, GetNewBlockWithCoinbasesRequest, GetPeersRequest, GetShardKeyRequest, GetShardKeyResponse, GetSideChainUtxosRequest, GetTemplateRegistrationsRequest, HeightRequest, HistoricalBlock, ListConnectedPeersResponse, ListHeadersRequest, MempoolStatsResponse, NetworkStatusResponse, NewBlockTemplate, NewBlockTemplateRequest, NewBlockTemplateResponse, NodeIdentity, SearchKernelsRequest, SearchUtxosRequest, SoftwareUpdate, StringValue, SubmitBlockResponse, SubmitTransactionRequest, SubmitTransactionResponse, SyncInfoResponse, SyncProgressResponse, TipInfoResponse, TransactionStateRequest, TransactionStateResponse, ValueAtHeightResponse};
use minotari_app_grpc::tari_rpc::base_node_client::BaseNodeClient;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status, Streaming};
use tonic::transport::Channel;

use crate::server::grpc::error::Error;
use crate::server::grpc::util;

const LIST_HEADERS_PAGE_SIZE: usize = 10;
const GET_BLOCKS_PAGE_SIZE: usize = 10;
const GET_TOKENS_IN_CIRCULATION_PAGE_SIZE: usize = 1_000;

const GET_DIFFICULTY_PAGE_SIZE: usize = 1_000;

#[macro_export]
macro_rules! proxy_simple_result {
    ($self:ident, $call:ident, $request:ident) => {
        match $self.client.lock().await.$call($request.into_inner()).await {
            Ok(resp) => Ok(resp),
            Err(error) => {
                error!("Error while calling {:?} on base node: {:?}", stringify!($call), error);
                Err(error)
            }
        }
    };
}

macro_rules! proxy_stream_result {
    ($self:ident, $call:ident, $request:ident, $page_size:ident) => {
        streaming_response(String::from(stringify!($call)),
                                 $self.client.lock().await.$call($request.into_inner()).await,
                                 $page_size,
        ).await
    };

    ($self:ident, $call:ident, $request:ident, $page_size:expr) => {
        streaming_response(String::from(stringify!($call)),
                                 $self.client.lock().await.$call($request.into_inner()).await,
                                 $page_size,
        ).await
    };
}

/// Returns a streaming response for any gRPC methods.
async fn streaming_response<R>(
    call: String,
    result: Result<Response<Streaming<R>>, Status>,
    page_size: usize)
    -> Result<Response<mpsc::Receiver<Result<R, Status>>>, Status>
    where R: Send + Sync + 'static,
{
    match result {
        Ok(response) => {
            let (mut tx, rx) = mpsc::channel(page_size);
            tokio::spawn(async move {
                let mut stream = response.into_inner();
                tokio::spawn(async move {
                    while let Ok(Some(next_message)) = stream.message().await {
                        if let Err(e) = tx.send(Ok(next_message)).await {
                            error!("failed to send '{call}' response message: {e}");
                        }
                    }
                });
            });
            Ok(Response::new(rx))
        }
        Err(status) => Err(status)
    }
}

/// Base node gRPC service that proxies all the requests to base node when miner calls them.
/// This makes sure that any extra call towards base node is served.
pub struct TariBaseNodeGrpc
{
    client: Arc<Mutex<BaseNodeClient<Channel>>>,
}

impl TariBaseNodeGrpc
{
    pub async fn new(
        base_node_address: String,
    ) -> Result<Self, Error> {
        Ok(Self { client: Arc::new(Mutex::new(util::connect_base_node(base_node_address).await?)) })
    }
}

#[tonic::async_trait]
impl tari_rpc::base_node_server::BaseNode for TariBaseNodeGrpc
{
    type ListHeadersStream = mpsc::Receiver<Result<BlockHeaderResponse, Status>>;
    async fn list_headers(&self, request: Request<ListHeadersRequest>) -> Result<Response<Self::ListHeadersStream>, Status> {
        proxy_stream_result!(self, list_headers, request, LIST_HEADERS_PAGE_SIZE)
    }
    async fn get_header_by_hash(&self, request: Request<GetHeaderByHashRequest>) -> Result<Response<BlockHeaderResponse>, Status> {
        proxy_simple_result!(self, get_header_by_hash, request)
    }
    type GetBlocksStream = mpsc::Receiver<Result<HistoricalBlock, Status>>;
    async fn get_blocks(&self, request: Request<GetBlocksRequest>) -> Result<Response<Self::GetBlocksStream>, Status> {
        proxy_stream_result!(self, get_blocks, request, GET_BLOCKS_PAGE_SIZE)
    }
    async fn get_block_timing(&self, request: Request<HeightRequest>) -> Result<Response<BlockTimingResponse>, Status> {
        proxy_simple_result!(self, get_block_timing, request)
    }
    async fn get_constants(&self, request: Request<BlockHeight>) -> Result<Response<ConsensusConstants>, Status> {
        proxy_simple_result!(self, get_constants, request)
    }
    async fn get_block_size(&self, request: Request<BlockGroupRequest>) -> Result<Response<BlockGroupResponse>, Status> {
        proxy_simple_result!(self, get_block_size, request)
    }
    async fn get_block_fees(&self, request: Request<BlockGroupRequest>) -> Result<Response<BlockGroupResponse>, Status> {
        proxy_simple_result!(self, get_block_fees, request)
    }
    async fn get_version(&self, request: Request<Empty>) -> Result<Response<StringValue>, Status> {
        proxy_simple_result!(self, get_version, request)
    }
    async fn check_for_updates(&self, request: Request<Empty>) -> Result<Response<SoftwareUpdate>, Status> {
        proxy_simple_result!(self, check_for_updates, request)
    }
    type GetTokensInCirculationStream = mpsc::Receiver<Result<ValueAtHeightResponse, Status>>;

    async fn get_tokens_in_circulation(&self, request: Request<GetBlocksRequest>) -> Result<Response<Self::GetTokensInCirculationStream>, Status> {
        proxy_stream_result!(self, get_tokens_in_circulation, request, GET_TOKENS_IN_CIRCULATION_PAGE_SIZE)
    }

    type GetNetworkDifficultyStream = mpsc::Receiver<Result<tari_rpc::NetworkDifficultyResponse, Status>>;

    async fn get_network_difficulty(&self, request: Request<HeightRequest>) -> Result<Response<Self::GetNetworkDifficultyStream>, Status> {
        proxy_stream_result!(self, get_network_difficulty, request, GET_DIFFICULTY_PAGE_SIZE)
    }

    async fn get_new_block_template(&self, request: Request<NewBlockTemplateRequest>) -> Result<Response<NewBlockTemplateResponse>, Status> {
        proxy_simple_result!(self, get_new_block_template, request)
    }

    async fn get_new_block(&self, request: Request<NewBlockTemplate>) -> Result<Response<GetNewBlockResult>, Status> {
        proxy_simple_result!(self, get_new_block, request)
    }

    async fn get_new_block_with_coinbases(&self, request: Request<GetNewBlockWithCoinbasesRequest>) -> Result<Response<GetNewBlockResult>, Status> {
        proxy_simple_result!(self, get_new_block_with_coinbases, request)
    }

    async fn get_new_block_template_with_coinbases(&self, request: Request<GetNewBlockTemplateWithCoinbasesRequest>) -> Result<Response<GetNewBlockResult>, Status> {
        proxy_simple_result!(self, get_new_block_template_with_coinbases, request)
    }

    async fn get_new_block_blob(&self, request: Request<NewBlockTemplate>) -> Result<Response<GetNewBlockBlobResult>, Status> {
        proxy_simple_result!(self, get_new_block_blob, request)
    }

    async fn submit_block(&self, request: Request<Block>) -> Result<Response<SubmitBlockResponse>, Status> {
        proxy_simple_result!(self, submit_block, request)
    }

    async fn submit_block_blob(&self, request: Request<BlockBlobRequest>) -> Result<Response<SubmitBlockResponse>, Status> {
        proxy_simple_result!(self, submit_block_blob, request)
    }

    async fn submit_transaction(&self, request: Request<SubmitTransactionRequest>) -> Result<Response<SubmitTransactionResponse>, Status> {
        proxy_simple_result!(self, submit_transaction, request)
    }

    async fn get_sync_info(&self, request: Request<Empty>) -> Result<Response<SyncInfoResponse>, Status> {
        proxy_simple_result!(self, get_sync_info, request)
    }

    async fn get_sync_progress(&self, request: Request<Empty>) -> Result<Response<SyncProgressResponse>, Status> {
        proxy_simple_result!(self, get_sync_progress, request)
    }

    async fn get_tip_info(&self, request: Request<Empty>) -> Result<Response<TipInfoResponse>, Status> {
        proxy_simple_result!(self, get_tip_info, request)
    }

    type SearchKernelsStream = mpsc::Receiver<Result<HistoricalBlock, Status>>;

    async fn search_kernels(&self, request: Request<SearchKernelsRequest>) -> Result<Response<Self::SearchKernelsStream>, Status> {
        proxy_stream_result!(self, search_kernels, request, GET_BLOCKS_PAGE_SIZE)
    }

    type SearchUtxosStream = mpsc::Receiver<Result<HistoricalBlock, Status>>;

    async fn search_utxos(&self, request: Request<SearchUtxosRequest>) -> Result<Response<Self::SearchUtxosStream>, Status> {
        proxy_stream_result!(self, search_utxos, request, GET_BLOCKS_PAGE_SIZE)
    }

    type FetchMatchingUtxosStream = mpsc::Receiver<Result<tari_rpc::FetchMatchingUtxosResponse, Status>>;

    async fn fetch_matching_utxos(&self, request: Request<FetchMatchingUtxosRequest>) -> Result<Response<Self::FetchMatchingUtxosStream>, Status> {
        proxy_stream_result!(self, fetch_matching_utxos, request, GET_BLOCKS_PAGE_SIZE)
    }

    type GetPeersStream = mpsc::Receiver<Result<tari_rpc::GetPeersResponse, Status>>;

    async fn get_peers(&self, request: Request<GetPeersRequest>) -> Result<Response<Self::GetPeersStream>, Status> {
        proxy_stream_result!(self, get_peers, request, GET_BLOCKS_PAGE_SIZE)
    }

    type GetMempoolTransactionsStream = mpsc::Receiver<Result<tari_rpc::GetMempoolTransactionsResponse, Status>>;

    async fn get_mempool_transactions(&self, request: Request<GetMempoolTransactionsRequest>) -> Result<Response<Self::GetMempoolTransactionsStream>, Status> {
        proxy_stream_result!(self, get_mempool_transactions, request, GET_BLOCKS_PAGE_SIZE)
    }

    async fn transaction_state(&self, request: Request<TransactionStateRequest>) -> Result<Response<TransactionStateResponse>, Status> {
        proxy_simple_result!(self, transaction_state, request)
    }

    async fn identify(&self, request: Request<Empty>) -> Result<Response<NodeIdentity>, Status> {
        proxy_simple_result!(self, identify, request)
    }

    async fn get_network_status(&self, request: Request<Empty>) -> Result<Response<NetworkStatusResponse>, Status> {
        proxy_simple_result!(self, get_network_status, request)
    }

    async fn list_connected_peers(&self, request: Request<Empty>) -> Result<Response<ListConnectedPeersResponse>, Status> {
        proxy_simple_result!(self, list_connected_peers, request)
    }

    async fn get_mempool_stats(&self, request: Request<Empty>) -> Result<Response<MempoolStatsResponse>, Status> {
        proxy_simple_result!(self, get_mempool_stats, request)
    }

    type GetActiveValidatorNodesStream = mpsc::Receiver<Result<tari_rpc::GetActiveValidatorNodesResponse, Status>>;

    async fn get_active_validator_nodes(&self, request: Request<GetActiveValidatorNodesRequest>) -> Result<Response<Self::GetActiveValidatorNodesStream>, Status> {
        proxy_stream_result!(self, get_active_validator_nodes, request, 1000)
    }

    async fn get_shard_key(&self, request: Request<GetShardKeyRequest>) -> Result<Response<GetShardKeyResponse>, Status> {
        proxy_simple_result!(self, get_shard_key, request)
    }

    type GetTemplateRegistrationsStream = mpsc::Receiver<Result<tari_rpc::GetTemplateRegistrationResponse, Status>>;

    async fn get_template_registrations(&self, request: Request<GetTemplateRegistrationsRequest>) -> Result<Response<Self::GetTemplateRegistrationsStream>, Status> {
        proxy_stream_result!(self, get_template_registrations, request, 10)
    }

    type GetSideChainUtxosStream = mpsc::Receiver<Result<tari_rpc::GetSideChainUtxosResponse, Status>>;

    async fn get_side_chain_utxos(&self, request: Request<GetSideChainUtxosRequest>) -> Result<Response<Self::GetSideChainUtxosStream>, Status> {
        proxy_stream_result!(self, get_side_chain_utxos, request, 10)
    }
}