use std::sync::Arc;
use std::time::Duration;

use log::{info, warn};
use thiserror::Error;
use tokio::select;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::{RecvError, SendError};
use tokio::time::sleep;

use crate::server::p2p::messages::{ValidateBlockRequest, ValidateBlockResult};
use crate::server::p2p::peer_store::PeerStore;
use crate::sharechain::Block;

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
    SendValidateBlockRequest(#[from] SendError<ValidateBlockRequest>),
}

pub struct ServiceClientChannels {
    validate_block_sender: broadcast::Sender<ValidateBlockRequest>,
    validate_block_receiver: broadcast::Receiver<ValidateBlockResult>,
}

impl ServiceClientChannels {
    pub fn new(
        validate_block_sender: broadcast::Sender<ValidateBlockRequest>,
        validate_block_receiver: broadcast::Receiver<ValidateBlockResult>,
    ) -> Self {
        Self {
            validate_block_sender,
            validate_block_receiver,
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

    pub async fn validate_block(&self, block: Block) -> Result<bool, ClientError> {
        info!("[CLIENT] Start block validation");
        // send request to validate block
        self.channels.validate_block_sender.send(ValidateBlockRequest::new(block.clone()))
            .map_err(|error|
                ClientError::ChannelSend(Box::new(ChannelSendError::SendValidateBlockRequest(error)))
            )?;

        // calculate how many validations we need (more than 2/3 of peers should validate)
        let peer_count = self.peer_store.peer_count() + 1; // TODO: remove + 1
        info!("[CLIENT] Peer count: {peer_count:?}");
        // TODO: calculate well, if there are 3 peers (including us), then min validation count is:
        // TODO: ((peer_count + 1 / 3) * 2) - 1 rounded to an int
        let min_validation_count = (peer_count / 3) * 2;
        info!("[CLIENT] Minimum validation count: {min_validation_count:?}");

        // wait for the validations to come
        let timeout = Duration::from_secs(30);
        let mut validate_receiver = self.channels.validate_block_receiver.resubscribe();
        let mut validation_count = 0;
        loop {
            select! {
                _ = sleep(timeout) => {
                    warn!("Timing out waiting for validations!");
                    break;
                }
                result = validate_receiver.recv() => {
                    let validate_result = result.map_err(ClientError::ChannelReceive)?;
                    info!("New validation: {validate_result:?}");
                    if validate_result.valid && validate_result.block == block {
                        validation_count+=1;
                    }
                    if validation_count >= min_validation_count {
                        break;
                    }
                }
            }
        }

        Ok(validation_count >= min_validation_count)
    }
}