use std::ops::Deref;
use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{error, info, warn};
use thiserror::Error;
use tokio::select;
use tokio::sync::{broadcast, mpsc};
use tokio::sync::broadcast::error::{RecvError, SendError};
use tokio::time::sleep;

use crate::server::p2p::messages::{ShareChainSyncResponse, ValidateBlockRequest, ValidateBlockResult};
use crate::server::p2p::peer_store::PeerStore;
use crate::sharechain::block::Block;

#[derive(Error, Debug)]
pub enum ClientError {
    #[error("Channel send error: {0}")]
    ChannelSend(#[from] Box<ChannelSendError>),
    #[error("Channel receive error: {0}")]
    ChannelReceive(#[from] RecvError),
    #[error("No response for share chain sync request!")]
    NoSyncShareChainResponse,
}

#[derive(Error, Debug)]
pub enum ChannelSendError {
    #[error("Send ValidateBlockRequest error: {0}")]
    ValidateBlockRequest(#[from] SendError<ValidateBlockRequest>),
    #[error("Send broadcast block error: {0}")]
    BroadcastBlock(#[from] SendError<Block>),
    #[error("Send sync share chain request error: {0}")]
    ClientSyncShareChainRequest(#[from] SendError<ClientSyncShareChainRequest>),
}

#[derive(Clone, Debug)]
pub struct ClientSyncShareChainRequest {
    pub request_id: String,
}

impl ClientSyncShareChainRequest {
    pub fn new(request_id: String) -> Self {
        Self { request_id }
    }
}

#[derive(Clone, Debug)]
pub struct ClientSyncShareChainResponse {
    pub request_id: String,
    pub success: bool,
}

impl ClientSyncShareChainResponse {
    pub fn new(request_id: String, success: bool) -> Self {
        Self { request_id, success }
    }
}

pub struct ServiceClientChannels {
    validate_block_sender: broadcast::Sender<ValidateBlockRequest>,
    validate_block_receiver: broadcast::Receiver<ValidateBlockResult>,
    broadcast_block_sender: broadcast::Sender<Block>,
}

impl ServiceClientChannels {
    pub fn new(
        validate_block_sender: broadcast::Sender<ValidateBlockRequest>,
        validate_block_receiver: broadcast::Receiver<ValidateBlockResult>,
        broadcast_block_sender: broadcast::Sender<Block>,
    ) -> Self {
        Self {
            validate_block_sender,
            validate_block_receiver,
            broadcast_block_sender,
        }
    }
}

pub struct ServiceClient {
    channels: ServiceClientChannels,
    peer_store: Arc<PeerStore>,
}

impl ServiceClient {
    pub fn new(
        channels: ServiceClientChannels,
        peer_store: Arc<PeerStore>,
    ) -> Self {
        Self { channels, peer_store }
    }

    pub async fn broadcast_block(&self, block: &Block) -> Result<(), ClientError> {
        self.channels.broadcast_block_sender.send(block.clone())
            .map_err(|error|
                ClientError::ChannelSend(Box::new(ChannelSendError::BroadcastBlock(error)))
            )?;

        Ok(())
    }

    pub async fn validate_block(&self, block: &Block) -> Result<bool, ClientError> {
        info!("[CLIENT] Start block validation");
        let start = Instant::now();

        // send request to validate block
        self.channels.validate_block_sender.send(ValidateBlockRequest::new(block.clone()))
            .map_err(|error|
                ClientError::ChannelSend(Box::new(ChannelSendError::ValidateBlockRequest(error)))
            )?;

        // calculate how many validations we need (more than 2/3 of peers should validate)
        let peer_count = self.peer_store.peer_count().await as f64 + 1.0;
        let min_validation_count = (peer_count / 3.0) * 2.0;
        let min_validation_count = min_validation_count.round() as u64;
        info!("[CLIENT] Minimum validation count: {min_validation_count:?}");

        // wait for the validations to come
        // TODO: listen here for peer_store changes, so we can recalculate min validation count and restart validation flow here
        let timeout = Duration::from_secs(30);
        let mut validate_receiver = self.channels.validate_block_receiver.resubscribe();
        let mut validation_count = 0;
        let block = block.clone();
        while validation_count < min_validation_count {
            select! {
                _ = sleep(timeout) => {
                    warn!("Timing out waiting for validations!");
                    break;
                }
                result = validate_receiver.recv() => {
                    match result {
                        Ok(validate_result) => {
                            info!("New validation: {validate_result:?}");
                            if validate_result.valid && validate_result.block == block.clone() {
                                validation_count+=1;
                            }
                        }
                        Err(error) => {
                            error!("Error during receiving: {error:?}");
                        }
                    }
                }
            }
        }

        let validation_time = Instant::now().duration_since(start);
        info!("Validation took {:?}", validation_time);

        Ok(validation_count >= min_validation_count)
    }
}