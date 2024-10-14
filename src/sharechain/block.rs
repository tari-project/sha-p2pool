// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::ops::Mul;

use blake2::Blake2b;
use digest::consts::U32;
use lazy_static::lazy_static;
use libp2p::Multiaddr;
use serde::{Deserialize, Serialize};
use tari_common::configuration::Network;
use tari_common_types::{tari_address::TariAddress, types::BlockHash};
use tari_core::{
    blocks::{genesis_block::get_genesis_block, BlockHeader, BlocksHashDomain},
    consensus::DomainSeparatedConsensusHasher,
    proof_of_work::Difficulty,
};
use tari_utilities::{epoch_time::EpochTime, hex::Hex};

use crate::{impl_conversions, sharechain::CHAIN_ID};

lazy_static! {
    pub static ref CURRENT_CHAIN_ID: String = {
        let network = Network::get_current_or_user_setting_or_default();
        let network_genesis_block = get_genesis_block(network);
        let network_genesis_block_hash = network_genesis_block.block().header.hash().to_hex();
        format!("{network_genesis_block_hash}_{CHAIN_ID}")
    };
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct Block {
    #[serde(default)]
    pub version: u32,
    pub chain_id: String,
    pub hash: BlockHash,
    pub timestamp: EpochTime,
    pub prev_hash: BlockHash,
    pub height: u64,
    pub original_block_header: BlockHeader,
    pub miner_wallet_address: Option<TariAddress>,
    pub sent_to_main_chain: bool,
    pub achieved_difficulty: Difficulty,
    pub miner_coinbase_extra: Vec<u8>,
}
impl_conversions!(Block);

impl Block {
    pub fn builder() -> BlockBuilder {
        BlockBuilder::new()
    }

    pub fn generate_hash(&self) -> BlockHash {
        let mut hasher = DomainSeparatedConsensusHasher::<BlocksHashDomain, Blake2b<U32>>::new("block")
            .chain(&self.prev_hash)
            .chain(&self.height);

        if let Some(miner_wallet_address) = &self.miner_wallet_address {
            hasher = hasher.chain(&miner_wallet_address.to_hex());
        }

        hasher.chain(&self.original_block_header).finalize().into()
    }
}

pub(crate) struct BlockBuilder {
    block: Block,
    use_specific_hash: bool,
}

impl BlockBuilder {
    pub fn new() -> Self {
        Self {
            use_specific_hash: false,
            block: Block {
                version: 3,
                chain_id: CURRENT_CHAIN_ID.clone(),
                hash: Default::default(),
                timestamp: EpochTime::now(),
                prev_hash: Default::default(),
                height: 0,
                original_block_header: BlockHeader::new(0),
                miner_wallet_address: Default::default(),
                sent_to_main_chain: false,
                achieved_difficulty: Difficulty::min(),
                miner_coinbase_extra: vec![],
            },
        }
    }

    pub fn with_timestamp(&mut self, timestamp: EpochTime) -> &mut Self {
        self.block.timestamp = timestamp;
        self
    }

    pub fn with_prev_hash(&mut self, prev_hash: BlockHash) -> &mut Self {
        self.block.prev_hash = prev_hash;
        self
    }

    pub fn with_height(&mut self, height: u64) -> &mut Self {
        self.block.height = height;
        self
    }

    pub fn with_original_block_header(&mut self, original_block_header: BlockHeader) -> &mut Self {
        self.block.original_block_header = original_block_header;
        self
    }

    pub fn with_miner_coinbase_extra(&mut self, coinbase_extra: Vec<u8>) -> &mut Self {
        self.block.miner_coinbase_extra = coinbase_extra;
        self
    }

    pub fn with_miner_wallet_address(&mut self, miner_wallet_address: TariAddress) -> &mut Self {
        self.block.miner_wallet_address = Some(miner_wallet_address);
        self
    }

    pub fn with_specific_hash(&mut self, hash: BlockHash) -> &mut Self {
        self.block.hash = hash;
        self.use_specific_hash = true;
        self
    }

    pub fn build(&mut self) -> Block {
        if !self.use_specific_hash {
            self.block.hash = self.block.generate_hash();
        }
        // self.block.hash = self.block.generate_hash();
        self.block.clone()
    }
}
