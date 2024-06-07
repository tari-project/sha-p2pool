use std::future::Future;
use std::ops::Deref;
use std::sync::Arc;

use libp2p::futures::channel::mpsc;
use libp2p::futures::SinkExt;
use log::{error, info, warn};
use minotari_app_grpc::tari_rpc;
use minotari_app_grpc::tari_rpc::{Block, BlockBlobRequest, BlockGroupRequest, BlockGroupResponse, BlockHeaderResponse, BlockHeight, BlockTimingResponse, ConsensusConstants, Empty, FetchMatchingUtxosRequest, GetActiveValidatorNodesRequest, GetBlocksRequest, GetHeaderByHashRequest, GetMempoolTransactionsRequest, GetNewBlockBlobResult, GetNewBlockResult, GetNewBlockTemplateWithCoinbasesRequest, GetNewBlockWithCoinbasesRequest, GetPeersRequest, GetShardKeyRequest, GetShardKeyResponse, GetSideChainUtxosRequest, GetTemplateRegistrationsRequest, HeightRequest, HistoricalBlock, ListConnectedPeersResponse, ListHeadersRequest, MempoolStatsResponse, NetworkStatusResponse, NewBlockCoinbase, NewBlockTemplate, NewBlockTemplateRequest, NewBlockTemplateResponse, NodeIdentity, PowAlgo, SearchKernelsRequest, SearchUtxosRequest, SoftwareUpdate, StringValue, SubmitBlockResponse, SubmitTransactionRequest, SubmitTransactionResponse, SyncInfoResponse, SyncProgressResponse, TipInfoResponse, TransactionStateRequest, TransactionStateResponse, ValueAtHeightResponse};
use minotari_app_grpc::tari_rpc::base_node_client::BaseNodeClient;
use minotari_node_grpc_client::BaseNodeGrpcClient;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status, Streaming};

use crate::server::grpc::error::{Error, TonicError};

const LIST_HEADERS_PAGE_SIZE: usize = 10;
const GET_BLOCKS_PAGE_SIZE: usize = 10;
const GET_TOKENS_IN_CIRCULATION_PAGE_SIZE: usize = 1_000;

const GET_DIFFICULTY_PAGE_SIZE: usize = 1_000;

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
        TariBaseNodeGrpc::streaming_response(String::from(stringify!($call)),
                                 $self.client.lock().await.$call($request.into_inner()).await,
                                 $page_size,
        ).await
    };

    ($self:ident, $call:ident, $request:ident, $page_size:expr) => {
        TariBaseNodeGrpc::streaming_response(String::from(stringify!($call)),
                                 $self.client.lock().await.$call($request.into_inner()).await,
                                 $page_size,
        ).await
    };
}

pub struct TariBaseNodeGrpc {
    // TODO: check if 1 shared client is enough or we need a pool of clients to operate faster
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
}

#[tonic::async_trait]
impl tari_rpc::base_node_server::BaseNode for TariBaseNodeGrpc {
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
        // TODO: Revisit this part whether this check needed as faster to send to node and see error,
        // TODO: than checking for difficulty, bigger the chance to get accepted!
        // TODO: Maybe worth checking for the last difficulty on network and if current block's difficulty is bigger
        // TODO: than the last difficulty on network, then try to send block.

        // TODO: add logic to check new block before sending to upstream blockchain whether difficulty matches
        // TODO: checking current network difficulty on base node
        // let request_block_header = request.get_ref().clone()
        //     .header.ok_or_else(|| Status::internal("height not present"))?;
        // let mut network_difficulty_stream = self.client.lock().await.get_network_difficulty(HeightRequest {
        //     from_tip: 0,
        //     start_height: request_block_header.height - 1,
        //     end_height: request_block_header.height,
        // }).await?.into_inner();
        // let mut network_difficulty_matches = false;
        // while let Ok(Some(diff_resp)) = network_difficulty_stream.message().await {
        //     if request_block_header.height == diff_resp.height + 1 { // TODO: compare block.difficulty with diff_resp.difficulty
        //         network_difficulty_matches = true;
        //     }
        // }
        //
        // if network_difficulty_matches { // TODO: !network_difficulty_matches
        //     info!("Difficulties do not match (block <-> network)!");
        //     // TODO: simply append new block if valid to sharechain showing that it is not accepted by base node
        //     // TODO: but still need to present on sharechain
        //     return Ok(Response::new(SubmitBlockResponse {
        //         block_hash: vec![], // TODO: get from sharechain
        //     }));
        // }

        match proxy_simple_result!(self, submit_block, request) {
            Ok(resp) => {
                info!("Block sent successfully!");
                // TODO: append new block if valid to sharechain with a flag or something that shows
                // TODO: that this block is accepted, so paid out
                Ok(resp)
            }
            Err(_) => {
                // TODO: simply append new block if valid to sharechain showing that it is not accepted by base node
                // TODO: but still need to present on sharechain
                Ok(Response::new(SubmitBlockResponse {
                    block_hash: vec![], // TODO: get from sharechain
                }))
            }
        }
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