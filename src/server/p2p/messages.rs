// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;

use libp2p::{Multiaddr, PeerId};
use serde::{Deserialize, Serialize};
use tari_common_types::types::FixedHash;
use tari_core::proof_of_work::{AccumulatedDifficulty, PowAlgorithm};
use tari_utilities::epoch_time::EpochTime;

use crate::{
    server::{p2p::Error, PROTOCOL_VERSION},
    sharechain::p2block::P2Block,
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
    pub version: u64,
    pub current_sha3x_height: u64,
    pub current_random_x_height: u64,
    pub current_sha3x_pow: u128,
    pub current_random_x_pow: u128,
    pub squad: String,
    pub timestamp: u64,
    pub user_agent: Option<String>,
    pub user_agent_version: Option<String>,
    public_addresses: Vec<String>,
}
impl_conversions!(PeerInfo);
impl PeerInfo {
    pub fn new(
        current_sha3x_height: u64,
        current_random_x_height: u64,
        current_sha3x_pow: u128,
        current_random_x_pow: u128,
        squad: String,
        public_addresses: Vec<Multiaddr>,
        user_agent: Option<String>,
    ) -> Self {
        let timestamp = EpochTime::now();
        Self {
            version: PROTOCOL_VERSION,
            current_sha3x_height,
            current_random_x_height,
            current_sha3x_pow,
            current_random_x_pow,
            squad,
            timestamp: timestamp.as_u64(),
            user_agent,
            user_agent_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            public_addresses: public_addresses.iter().map(|addr| addr.to_string()).collect(),
        }
    }

    pub fn public_addresses(&self) -> Vec<Multiaddr> {
        self.public_addresses
            .iter()
            .filter_map(|addr| addr.parse().ok())
            .collect()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ShareChainSyncRequest {
    algo: u64,
    missing_blocks: Vec<(u64, FixedHash)>,
}

impl ShareChainSyncRequest {
    pub fn new(algo: PowAlgorithm, missing_blocks: Vec<(u64, FixedHash)>) -> Self {
        Self {
            algo: algo.as_u64(),
            missing_blocks,
        }
    }

    pub fn algo(&self) -> PowAlgorithm {
        PowAlgorithm::try_from(self.algo).unwrap_or(PowAlgorithm::RandomX)
    }

    pub fn missing_blocks(&self) -> &[(u64, FixedHash)] {
        &self.missing_blocks
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CatchUpSyncRequest {
    algo: u64,
    i_have: Vec<(u64, FixedHash)>,
    last_block_received: Option<(u64, FixedHash)>,
}

impl CatchUpSyncRequest {
    pub fn new(
        algo: PowAlgorithm,
        i_have: Vec<(u64, FixedHash)>,
        last_block_received: Option<(u64, FixedHash)>,
    ) -> Self {
        Self {
            algo: algo.as_u64(),
            i_have,
            last_block_received,
        }
    }

    pub fn algo(&self) -> PowAlgorithm {
        PowAlgorithm::try_from(self.algo).unwrap_or(PowAlgorithm::RandomX)
    }

    pub fn i_have(&self) -> &[(u64, FixedHash)] {
        &self.i_have
    }

    pub fn last_block_received(&self) -> Option<(u64, FixedHash)> {
        self.last_block_received
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CatchUpSyncResponse {
    version: u64,
    peer_id: PeerId,
    algo: u64,
    blocks: Vec<P2Block>,
    // the tip is used to work out if we should continuing syncing from them
    tip: (u64, FixedHash),
    achieved_pow: Option<u128>,
}

impl CatchUpSyncResponse {
    pub fn new(
        algo: PowAlgorithm,
        peer_id: PeerId,
        blocks: &[Arc<P2Block>],
        tip: (u64, FixedHash),
        achieved_pow: u128,
    ) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            algo: algo.as_u64(),
            peer_id,
            blocks: blocks.iter().map(|block| (**block).clone()).collect(),
            tip,
            achieved_pow: Some(achieved_pow),
        }
    }

    pub fn achieved_pow(&self) -> u128 {
        self.achieved_pow.unwrap_or_default()
    }

    pub fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }

    pub fn algo(&self) -> PowAlgorithm {
        PowAlgorithm::try_from(self.algo).unwrap_or(PowAlgorithm::RandomX)
    }

    pub fn tip_hash(&self) -> &FixedHash {
        &self.tip.1
    }

    pub fn tip_height(&self) -> u64 {
        self.tip.0
    }

    pub fn into_blocks(self) -> Vec<P2Block> {
        let mut blocks = self.blocks;
        for block in &mut blocks {
            block.verified = false;
        }
        blocks
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DirectPeerInfoRequest {
    pub peer_id: String,
    pub info: PeerInfo,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DirectPeerInfoResponse {
    pub peer_id: String,
    pub info: PeerInfo,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NotifyNewTipBlock {
    pub version: u64,
    peer_id: PeerId,
    pub algo: u64,
    pub new_blocks: Vec<(u64, FixedHash)>,
    pub total_accumulated_difficulty: u128,
    pub timestamp: u64,
}
impl_conversions!(NotifyNewTipBlock);

impl NotifyNewTipBlock {
    pub fn new(
        peer_id: PeerId,
        algo: PowAlgorithm,
        new_blocks: Vec<(u64, FixedHash)>,
        total_acculumted_difficulty: AccumulatedDifficulty,
    ) -> Self {
        let total_acculumted_difficulty = total_acculumted_difficulty.as_u128();
        let timestamp = EpochTime::now().as_u64();
        Self {
            version: PROTOCOL_VERSION,
            peer_id,
            algo: algo.as_u64(),
            new_blocks,
            total_accumulated_difficulty: total_acculumted_difficulty,
            timestamp,
        }
    }

    pub fn algo(&self) -> PowAlgorithm {
        PowAlgorithm::try_from(self.algo).unwrap_or(PowAlgorithm::RandomX)
    }

    pub fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ShareChainSyncResponse {
    version: u64,
    peer_id: PeerId,
    algo: u64,
    blocks: Vec<P2Block>,
}

impl ShareChainSyncResponse {
    pub fn new(peer_id: PeerId, algo: PowAlgorithm, blocks: &[Arc<P2Block>]) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            peer_id,
            algo: algo.as_u64(),
            blocks: blocks.iter().map(|block| (**block).clone()).collect(),
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }

    pub fn algo(&self) -> PowAlgorithm {
        PowAlgorithm::try_from(self.algo).unwrap_or(PowAlgorithm::RandomX)
    }

    pub fn into_blocks(self) -> Vec<P2Block> {
        let mut blocks = self.blocks;
        for block in &mut blocks {
            block.verified = false;
        }
        blocks
    }
}
