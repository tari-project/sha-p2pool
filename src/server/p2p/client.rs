use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{debug, error, warn};
use thiserror::Error;
use tokio::select;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio::sync::broadcast::error::{RecvError, SendError};
use tokio::time::sleep;

use crate::server::p2p::messages::{ValidateBlockRequest, ValidateBlockResult};
use crate::server::p2p::peer_store::PeerStore;
use crate::sharechain::block::Block;

const LOG_TARGET: &str = "p2p_service_client";

#[derive(Error, Debug)]
pub enum ClientError {
    #[error("Channel send error: {0}")]
    ChannelSend(#[from] Box<ChannelSendError>),
    #[error("Channel receive error: {0}")]
    ChannelReceive(#[from] RecvError),
}

#[derive(Error, Debug)]
pub enum ChannelSendError {
    #[error("Send ValidateBlockRequest error: {0}")]
    ValidateBlockRequest(#[from] SendError<ValidateBlockRequest>),
    #[error("Send broadcast block error: {0}")]
    BroadcastBlock(#[from] SendError<Block>),
}

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub block_validation_timeout: Duration,
    pub validate_block_max_retries: u64,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            block_validation_timeout: Duration::from_secs(30),
            validate_block_max_retries: 5,
        }
    }
}

/// Contains all the channels a client needs to operate successfully.
pub struct ServiceClientChannels {
    validate_block_sender: broadcast::Sender<ValidateBlockRequest>,
    validate_block_receiver: Arc<Mutex<mpsc::UnboundedReceiver<ValidateBlockResult>>>,
    broadcast_block_sender: broadcast::Sender<Block>,
    peer_changes_receiver: broadcast::Receiver<()>,
}

impl ServiceClientChannels {
    pub fn new(
        validate_block_sender: broadcast::Sender<ValidateBlockRequest>,
        validate_block_receiver: mpsc::UnboundedReceiver<ValidateBlockResult>,
        broadcast_block_sender: broadcast::Sender<Block>,
        peer_changes_receiver: broadcast::Receiver<()>,
    ) -> Self {
        Self {
            validate_block_sender,
            validate_block_receiver: Arc::new(Mutex::new(validate_block_receiver)),
            broadcast_block_sender,
            peer_changes_receiver,
        }
    }
}

/// P2P service client.
pub struct ServiceClient {
    channels: ServiceClientChannels,
    peer_store: Arc<PeerStore>,
    config: ClientConfig,
}

impl ServiceClient {
    pub fn new(
        channels: ServiceClientChannels,
        peer_store: Arc<PeerStore>,
        config: ClientConfig,
    ) -> Self {
        Self { channels, peer_store, config }
    }

    /// Triggering broadcasting of a new block to p2pool network.
    pub async fn broadcast_block(&self, block: &Block) -> Result<(), ClientError> {
        self.channels.broadcast_block_sender.send(block.clone())
            .map_err(|error|
                ClientError::ChannelSend(Box::new(ChannelSendError::BroadcastBlock(error)))
            )?;

        Ok(())
    }

    async fn validate_block_with_retries(&self, block: &Block, mut retries: u64) -> Result<bool, ClientError> {
        if retries >= self.config.validate_block_max_retries {
            warn!(target: LOG_TARGET, "❗Too many validation retries!");
            return Ok(false);
        }

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
        debug!(target: LOG_TARGET, "Minimum validation count: {min_validation_count:?}");

        // wait for the validations to come
        let mut validate_block_receiver = self.channels.validate_block_receiver.lock().await;
        let mut peer_changes_receiver = self.channels.peer_changes_receiver.resubscribe();
        let mut peers_changed = false;
        let mut validation_count = 0;
        while validation_count < min_validation_count {
            select! {
                _ = sleep(self.config.block_validation_timeout) => {
                    warn!(target: LOG_TARGET, "⏰ Timing out waiting for validations!");
                    break;
                }
                _ = peer_changes_receiver.recv() => {
                    peers_changed = true;
                    break;
                }
                result = validate_block_receiver.recv() => {
                    if let Some(validate_result) = result {
                        if validate_result.valid && validate_result.block == *block {
                            debug!(target: LOG_TARGET, "New validation result: {validate_result:?}");
                            validation_count+=1;
                        }
                    } else {
                        break;
                    }
                }
            }
        }

        if peers_changed {
            retries += 1;
            return Box::pin(self.validate_block_with_retries(block, retries)).await;
        }

        let validation_time = Instant::now().duration_since(start);
        debug!(target: LOG_TARGET, "Validation took {:?}", validation_time);

        Ok(validation_count >= min_validation_count)
    }

    /// Triggers validation of a new block and waits for the result.
    pub async fn validate_block(&self, block: &Block) -> Result<bool, ClientError> {
        self.validate_block_with_retries(block, 0).await
    }
}