// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::time::{SystemTime, UNIX_EPOCH};

use libp2p::PeerId;
use serde::{Deserialize, Serialize};

use crate::server::p2p::Tribe;
use crate::{server::p2p::Error, sharechain::block::Block};

#[macro_export]
macro_rules! impl_conversions {
    ($type:ty) => {
        impl TryFrom<libp2p::gossipsub::Message> for $type {
            type Error = $crate::server::p2p::Error;

            fn try_from(message: libp2p::gossipsub::Message) -> Result<Self, Self::Error> {
                $crate::server::p2p::messages::deserialize_message::<$type>(message.data.as_slice())
            }
        }

        impl TryInto<Vec<u8>> for $type {
            type Error = $crate::server::p2p::Error;

            fn try_into(self) -> Result<Vec<u8>, Self::Error> {
                $crate::server::p2p::messages::serialize_message(&self)
            }
        }
    };
}
pub fn deserialize_message<'a, T>(raw_message: &'a [u8]) -> Result<T, Error>
where
    T: Deserialize<'a>,
{
    serde_cbor::from_slice(raw_message).map_err(Error::SerializeDeserialize)
}

pub fn serialize_message<T>(input: &T) -> Result<Vec<u8>, Error>
where
    T: Serialize,
{
    serde_cbor::to_vec(input).map_err(Error::SerializeDeserialize)
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PeerInfo {
    pub current_height: u64,
    pub tribe: Tribe,
    pub timestamp: u128,
}
impl_conversions!(PeerInfo);
impl PeerInfo {
    pub fn new(current_height: u64, tribe: Tribe) -> Self {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros();
        Self {
            current_height,
            tribe,
            timestamp,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ShareChainSyncRequest {
    pub from_height: u64,
}

impl ShareChainSyncRequest {
    pub fn new(from_height: u64) -> Self {
        Self { from_height }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LocalShareChainSyncRequest {
    pub peer_id: PeerId,
    pub request: ShareChainSyncRequest,
}

impl LocalShareChainSyncRequest {
    pub fn new(peer_id: PeerId, request: ShareChainSyncRequest) -> Self {
        Self { peer_id, request }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ShareChainSyncResponse {
    pub blocks: Vec<Block>,
}

impl ShareChainSyncResponse {
    pub fn new(blocks: Vec<Block>) -> Self {
        Self { blocks }
    }
}
