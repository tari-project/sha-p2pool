use tokio::sync::broadcast;

use crate::server::p2p::Error;
use crate::server::p2p::messages::{ValidateBlockRequest, ValidateBlockResult};
use crate::sharechain::Block;

struct ServiceClientChannels {
    validate_block_sender: broadcast::Sender<ValidateBlockRequest>,
    validate_block_receiver: broadcast::Receiver<ValidateBlockResult>,
}

pub struct ServiceClient {
    channels: ServiceClientChannels,
}

impl ServiceClient {
    fn new(channels: ServiceClientChannels) -> Self {
        Self { channels }
    }

    pub fn validate_block(&self, block: Block) -> Result<bool, Error> {
        // TODO: continue impl
        todo!()
    }
}