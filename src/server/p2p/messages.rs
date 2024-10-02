// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::time::{SystemTime, UNIX_EPOCH};

use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use tari_core::proof_of_work::PowAlgorithm;

use crate::{
    server::p2p::{Error, Squad},
    sharechain::pool_block::PoolBlock,
};

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
where T: Deserialize<'a> {
    serde_cbor::from_slice(raw_message).map_err(Error::SerializeDeserialize)
}

pub fn serialize_message<T>(input: &T) -> Result<Vec<u8>, Error>
where T: Serialize {
    serde_cbor::to_vec(input).map_err(Error::SerializeDeserialize)
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PeerInfo {
    #[serde(default)]
    pub version: u64,
    pub current_sha3x_height: u64,
    pub current_random_x_height: u64,
    pub squad: Squad,
    pub timestamp: u128,
    pub user_agent: Option<String>,
    pub user_agent_version: Option<String>,
}
impl_conversions!(PeerInfo);
impl PeerInfo {
    pub fn new(current_sha3x_height: u64, current_random_x_height: u64, squad: Squad) -> Self {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros();
        Self {
            version: 1,
            current_sha3x_height,
            current_random_x_height,
            squad,
            timestamp,
            user_agent: Some("tari-p2pool".to_string()),
            user_agent_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ShareChainSyncRequest {
    pub algo: PowAlgorithm,
    pub from_height: u64,
}

impl ShareChainSyncRequest {
    pub fn new(algo: PowAlgorithm, from_height: u64) -> Self {
        Self { algo, from_height }
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
    pub algo: PowAlgorithm,
    pub blocks: Vec<PoolBlock>,
}

impl ShareChainSyncResponse {
    pub fn new(algo: PowAlgorithm, blocks: Vec<PoolBlock>) -> Self {
        Self { algo, blocks }
    }
}
