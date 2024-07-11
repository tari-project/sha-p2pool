// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use log::error;
use thiserror::Error;
use tokio::sync::{
    broadcast,
    broadcast::error::{RecvError, SendError},
};

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
    #[error("Send broadcast block error: {0}")]
    BroadcastBlock(#[from] SendError<Block>),
}

/// Contains all the channels a client needs to operate successfully.
pub struct ServiceClientChannels {
    broadcast_block_sender: broadcast::Sender<Block>,
}

impl ServiceClientChannels {
    pub fn new(broadcast_block_sender: broadcast::Sender<Block>) -> Self {
        Self { broadcast_block_sender }
    }
}

/// P2P service client.
pub struct ServiceClient {
    channels: ServiceClientChannels,
}

impl ServiceClient {
    pub fn new(channels: ServiceClientChannels) -> Self {
        Self { channels }
    }

    /// Triggering broadcasting of a new block to p2pool network.
    pub async fn broadcast_block(&self, block: &Block) -> Result<(), ClientError> {
        self.channels
            .broadcast_block_sender
            .send(block.clone())
            .map_err(|error| ClientError::ChannelSend(Box::new(ChannelSendError::BroadcastBlock(error))))?;

        Ok(())
    }
}
