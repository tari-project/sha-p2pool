// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use log::error;
use thiserror::Error;
use tokio::sync::{
    broadcast,
    broadcast::error::{RecvError, SendError},
};

use crate::sharechain::p2block::P2Block;

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
    BroadcastBlock(#[from] SendError<P2Block>),
}

/// P2P service client.
pub struct ServiceClient {
    broadcast_block_sender: broadcast::Sender<P2Block>,
}

impl ServiceClient {
    pub fn new(broadcast_block_sender: broadcast::Sender<P2Block>) -> Self {
        Self { broadcast_block_sender }
    }

    /// Triggering broadcasting of a new block to p2pool network.
    pub async fn broadcast_block(&self, block: &P2Block) -> Result<(), ClientError> {
        self.broadcast_block_sender
            .send(block.clone())
            .map_err(|error| ClientError::ChannelSend(Box::new(ChannelSendError::BroadcastBlock(error))))?;

        Ok(())
    }
}
