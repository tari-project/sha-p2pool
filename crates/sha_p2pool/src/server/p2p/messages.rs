use libp2p::gossipsub::Message;
use serde::{Deserialize, Serialize};

use crate::server::p2p::Error;

macro_rules! impl_message_try_from {
    ($type:ty) => {
        impl TryFrom<Message> for $type {
            type Error = Error;

            fn try_from(message: Message) -> Result<Self, Self::Error> {
                deserialize_message::<$type>(message.data.as_slice())
            }
        }
    };
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PeerInfo {
    pub current_height: u64,
}
impl_message_try_from!(PeerInfo);

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