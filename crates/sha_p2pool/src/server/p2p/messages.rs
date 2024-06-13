use libp2p::gossipsub::Message;
use libp2p::PeerId;
use serde::{Deserialize, Serialize};

use crate::server::p2p::Error;
use crate::sharechain::Block;

macro_rules! impl_conversions {
    ($type:ty) => {
        impl TryFrom<Message> for $type {
            type Error = Error;

            fn try_from(message: Message) -> Result<Self, Self::Error> {
                deserialize_message::<$type>(message.data.as_slice())
            }
        }

        impl TryInto<Vec<u8>> for $type {
            type Error = Error;

            fn try_into(self) -> Result<Vec<u8>, Self::Error> {
                serialize_message(&self)
            }
        }
    };
}
pub fn deserialize_message<'a, T>(raw_message: &'a [u8]) -> Result<T, Error>
    where T: Deserialize<'a>,
{
    serde_cbor::from_slice(raw_message).map_err(Error::SerializeDeserialize)
}

pub fn serialize_message<T>(input: &T) -> Result<Vec<u8>, Error>
    where T: Serialize,
{
    serde_cbor::to_vec(input).map_err(Error::SerializeDeserialize)
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PeerInfo {
    pub current_height: u64,
}
impl_conversions!(PeerInfo);
impl PeerInfo {
    pub fn new(current_height: u64) -> Self {
        Self { current_height }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ValidateBlockRequest(Block);
impl_conversions!(ValidateBlockRequest);
impl ValidateBlockRequest {
    pub fn new(block: Block) -> Self {
        Self(block)
    }
    
    pub fn block(&self) -> Block {
        self.0.clone()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ValidateBlockResult {
    pub peer_id: PeerId,
    pub block: Block,
    pub valid: bool,
}
impl_conversions!(ValidateBlockResult);
impl ValidateBlockResult {
    pub fn new(
        peer_id: PeerId,
        block: Block,
        valid: bool,
    ) -> Self {
        Self {
            peer_id,
            block,
            valid,
        }
    }
}