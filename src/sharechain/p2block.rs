// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;

use blake2::Blake2b;
use digest::consts::U32;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use tari_common::configuration::Network;
use tari_common_types::{tari_address::TariAddress, types::BlockHash};
use tari_core::{
    blocks::{genesis_block::get_genesis_block, Block, BlockHeader, BlocksHashDomain},
    consensus::DomainSeparatedConsensusHasher,
    proof_of_work::Difficulty,
    transactions::aggregated_body::AggregateBody,
};
use tari_script::script;
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct P2Block {
    #[serde(default)]
    pub version: u32,
    pub hash: BlockHash,
    pub timestamp: EpochTime,
    pub prev_hash: BlockHash,
    pub height: u64,
    pub original_block: Block,
    pub miner_wallet_address: TariAddress,
    pub sent_to_main_chain: bool,
    pub target_difficulty: Difficulty,
    // list of uncles blocks confirmed by this block
    // (height of uncle, hash of uncle)
    pub uncles: Vec<(u64, BlockHash)>,
    pub miner_coinbase_extra: Vec<u8>,
    pub verified: bool,
}
impl_conversions!(P2Block);

impl P2Block {
    pub fn builder() -> BlockBuilder {
        BlockBuilder::new()
    }

    pub fn generate_hash(&self) -> BlockHash {
        DomainSeparatedConsensusHasher::<BlocksHashDomain, Blake2b<U32>>::new("block")
            .chain(&self.prev_hash)
            .chain(&self.version.to_le_bytes())
            .chain(&self.timestamp)
            .chain(&self.height)
            .chain(&self.miner_wallet_address.to_vec())
            .chain(&self.original_block.header)
            .chain(&self.target_difficulty)
            .chain(&self.uncles)
            .chain(&self.miner_coinbase_extra)
            .finalize()
            .into()
    }

    pub fn fix_hash(&mut self) {
        self.hash = self.generate_hash();
    }

    pub fn get_miner_coinbase_extra(&self) -> Vec<u8> {
        let coinbases = self.original_block.body.get_coinbase_outputs();
        let own_script = script!(PushPubKey(Box::new(
            self.miner_wallet_address.public_spend_key().clone()
        )))
        .expect("Constructing a script should not fail");
        for coinbase in coinbases {
            if coinbase.script == own_script {
                return coinbase.features.coinbase_extra.as_ref().to_vec();
            }
        }
        Vec::new()
    }
}

pub(crate) struct BlockBuilder {
    block: P2Block,
    use_specific_hash: bool,
}

impl BlockBuilder {
    pub fn new() -> Self {
        Self {
            use_specific_hash: false,
            block: P2Block {
                version: 7,
                hash: Default::default(),
                timestamp: EpochTime::now(),
                prev_hash: Default::default(),
                height: 0,
                original_block: Block::new(BlockHeader::new(0), AggregateBody::empty()),
                miner_wallet_address: Default::default(),
                sent_to_main_chain: false,
                target_difficulty: Difficulty::min(),
                uncles: Vec::new(),
                miner_coinbase_extra: vec![],
                verified: false,
            },
        }
    }

    pub fn with_timestamp(mut self, timestamp: EpochTime) -> Self {
        self.block.timestamp = timestamp;
        self
    }

    pub fn with_prev_hash(mut self, prev_hash: BlockHash) -> Self {
        self.block.prev_hash = prev_hash;
        self
    }

    pub fn with_height(mut self, height: u64) -> Self {
        self.block.height = height;
        self
    }

    pub fn with_target_difficulty(mut self, target_difficulty: Difficulty) -> Self {
        self.block.target_difficulty = target_difficulty;
        self
    }

    pub fn with_tari_block(mut self, block: Block) -> Self {
        self.block.original_block = block;
        self
    }

    pub fn with_miner_wallet_address(mut self, miner_wallet_address: TariAddress) -> Self {
        self.block.miner_wallet_address = miner_wallet_address;
        self
    }

    pub fn with_miner_coinbase_extra(mut self, coinbase_extra: Vec<u8>) -> Self {
        self.block.miner_coinbase_extra = coinbase_extra;
        self
    }

    pub fn with_uncles(mut self, uncles: Vec<(u64, BlockHash)>) -> Self {
        self.block.uncles = uncles;
        self
    }

    pub fn build(mut self) -> Arc<P2Block> {
        if !self.use_specific_hash {
            self.block.hash = self.block.generate_hash();
        }
        Arc::new(self.block)
    }
}
