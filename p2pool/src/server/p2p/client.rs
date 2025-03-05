// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use tokio::sync::broadcast;

use crate::server::p2p::messages::NotifyNewTipBlock;

pub struct ServiceClient {
    broadcast_block_sender: broadcast::Sender<NotifyNewTipBlock>,
}

impl ServiceClient {
    pub fn new(broadcast_block_sender: broadcast::Sender<NotifyNewTipBlock>) -> Self {
        Self { broadcast_block_sender }
    }

    /// Triggering broadcasting of a new block to p2pool network.
    pub fn broadcast_block(&self, new_blocks: NotifyNewTipBlock) -> Result<(), anyhow::Error> {
        self.broadcast_block_sender.send(new_blocks)?;

        Ok(())
    }
}
