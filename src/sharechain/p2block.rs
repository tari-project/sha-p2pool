// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::sync::Arc;

use blake2::Blake2b;
use digest::consts::U32;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use tari_common::configuration::Network;
use tari_common_types::{
    tari_address::TariAddress,
    types::{BlockHash, FixedHash},
};
use tari_core::{
    blocks::{genesis_block::get_genesis_block, Block, BlockHeader, BlocksHashDomain},
    consensus::DomainSeparatedConsensusHasher,
    proof_of_work::Difficulty,
    transactions::transaction_components::TransactionOutput,
};
use tari_script::script;
use tari_utilities::{epoch_time::EpochTime, hex::Hex};

use crate::{
    impl_conversions,
    server::PROTOCOL_VERSION,
    sharechain::{ShareChainError, CHAIN_ID},
};

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
    pub version: u64,
    pub hash: BlockHash,
    pub timestamp: EpochTime,
    pub prev_hash: BlockHash,
    pub height: u64,
    pub original_header: BlockHeader,
    pub coinbases: Vec<TransactionOutput>,
    pub other_output_hash: FixedHash,
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
            .chain(&self.original_header)
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
        let own_script = script!(PushPubKey(Box::new(
            self.miner_wallet_address.public_spend_key().clone()
        )))
        .expect("Constructing a script should not fail");
        for coinbase in &self.coinbases {
            if coinbase.script == own_script {
                return coinbase.features.coinbase_extra.as_ref().to_vec();
            }
        }
        Vec::new()
    }

    pub fn populate_tari_data(&mut self, block: Block) -> Result<(), ShareChainError> {
        self.coinbases = block.body.get_coinbase_outputs().into_iter().cloned().collect();
        self.other_output_hash = block
            .body
            .calculate_header_normal_output_mr()
            .map_err(|e| ShareChainError::InvalidBlock { reason: e.to_string() })?;
        self.original_header = block.header;
        Ok(())
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
                version: PROTOCOL_VERSION,
                hash: Default::default(),
                timestamp: EpochTime::now(),
                prev_hash: Default::default(),
                height: 0,
                original_header: BlockHeader::new(0),
                coinbases: Vec::new(),
                other_output_hash: Default::default(),
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

    pub fn with_tari_block(mut self, block: Block) -> Result<Self, ShareChainError> {
        self.block.populate_tari_data(block)?;
        Ok(self)
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
