// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::sharechain::p2block::P2Block;

pub(crate) struct ServiceClient {
    broadcast_block_sender: broadcast::Sender<Arc<P2Block>>,
}

impl ServiceClient {
    pub fn new(broadcast_block_sender: broadcast::Sender<Arc<P2Block>>) -> Self {
        Self { broadcast_block_sender }
    }

    /// Triggering broadcasting of a new block to p2pool network.
    pub fn broadcast_block(&self, block: Arc<P2Block>) -> Result<(), anyhow::Error> {
        self.broadcast_block_sender.send(block)?;

        Ok(())
    }
}
