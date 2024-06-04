use std::sync::{Arc, Mutex};

use libp2p::futures::channel::mpsc;
use log::info;
use minotari_app_grpc::tari_rpc;
use minotari_app_grpc::tari_rpc::{Block, BlockBlobRequest, BlockGroupRequest, BlockGroupResponse, BlockHeaderResponse, BlockHeight, BlockTimingResponse, ConsensusConstants, Empty, FetchMatchingUtxosRequest, GetActiveValidatorNodesRequest, GetBlocksRequest, GetHeaderByHashRequest, GetMempoolTransactionsRequest, GetNewBlockBlobResult, GetNewBlockResult, GetNewBlockTemplateWithCoinbasesRequest, GetNewBlockWithCoinbasesRequest, GetPeersRequest, GetShardKeyRequest, GetShardKeyResponse, GetSideChainUtxosRequest, GetTemplateRegistrationsRequest, HeightRequest, ListConnectedPeersResponse, ListHeadersRequest, MempoolStatsResponse, NetworkStatusResponse, NewBlockTemplate, NewBlockTemplateRequest, NewBlockTemplateResponse, NodeIdentity, SearchKernelsRequest, SearchUtxosRequest, SoftwareUpdate, StringValue, SubmitBlockResponse, SubmitTransactionRequest, SubmitTransactionResponse, SyncInfoResponse, SyncProgressResponse, TipInfoResponse, TransactionStateRequest, TransactionStateResponse};
use minotari_app_grpc::tari_rpc::base_node_client::BaseNodeClient;
use minotari_node_grpc_client::BaseNodeGrpcClient;
use thiserror::Error;
use tonic::{Request, Response, Status};

pub struct TariBaseNodeGrpc {
    client: Arc<Mutex<BaseNodeClient<tonic::transport::Channel>>>,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Tonic error: {0}")]
    Tonic(#[from] TonicError),
}

#[derive(Error, Debug)]
pub enum TonicError {
    #[error("Transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
}

impl TariBaseNodeGrpc {
    pub async fn new(base_node_address: String) -> Result<Self, Error> {
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
        if let Ok(mut client) = self.client.lock() {
            let result = client
                .get_new_block_template(request.into_inner().clone())
                .await;
        }

        Err(Status::internal(""))
    }

    async fn get_new_block(&self, request: Request<NewBlockTemplate>) -> Result<Response<GetNewBlockResult>, Status> {
        info!("get_new_block called!");
        todo!()
    }


    async fn list_headers(&self, request: Request<ListHeadersRequest>) -> Result<Response<Self::ListHeadersStream>, Status> {
        todo!()
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

    async fn submit_block(&self, request: Request<Block>) -> Result<Response<SubmitBlockResponse>, Status> {
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
        todo!()
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