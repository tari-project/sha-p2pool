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
    proof_of_work::{AccumulatedDifficulty, Difficulty},
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
    target_difficulty: Difficulty,
    // list of uncles blocks confirmed by this block
    // (height of uncle, hash of uncle)
    pub uncles: Vec<(u64, BlockHash)>,
    pub miner_coinbase_extra: Vec<u8>,
    pub verified: bool,
    total_pow: AccumulatedDifficulty,
}

impl Default for P2Block {
    fn default() -> Self {
        Self {
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
            total_pow: AccumulatedDifficulty::default(),
        }
    }
}
impl_conversions!(P2Block);

impl P2Block {
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
            .chain(&self.total_pow.as_u128())
            .finalize()
            .into()
    }

    pub fn change_target_difficulty(&mut self, target_difficulty: Difficulty) -> Result<(), ShareChainError> {
        self.total_pow = self
            .total_pow
            .checked_add_difficulty(target_difficulty)
            .ok_or(ShareChainError::DifficultyOverflow)?;
        self.total_pow = self
            .total_pow
            .checked_sub_difficulty(self.target_difficulty)
            .ok_or(ShareChainError::DifficultyOverflow)?;
        self.target_difficulty = target_difficulty;
        self.fix_hash();
        Ok(())
    }

    pub fn target_difficulty(&self) -> Difficulty {
        self.target_difficulty
    }

    pub fn total_pow(&self) -> AccumulatedDifficulty {
        self.total_pow
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

pub struct P2BlockBuilder {
    block: P2Block,
    use_specific_hash: bool,
    added_target_difficulty: bool,
}

impl P2BlockBuilder {
    pub fn new(prev_block_hash_and_pow: Option<(FixedHash, AccumulatedDifficulty)>) -> Self {
        let mut block = P2Block::default();
        match prev_block_hash_and_pow {
            Some((prev_block_hash, total_pow)) => {
                block.prev_hash = prev_block_hash;
                block.total_pow = total_pow;
            },
            None => {
                block.prev_hash = BlockHash::zero();
                block.total_pow = AccumulatedDifficulty::default();
            },
        }
        Self {
            use_specific_hash: false,
            added_target_difficulty: false,
            block,
        }
    }

    #[cfg(test)]
    pub fn new_from_block(block_arg: Option<&P2Block>) -> Self {
        let mut block = P2Block::default();
        if let Some(b) = block_arg {
            block.prev_hash = b.hash;
            block.total_pow = b.total_pow;
        }
        Self {
            use_specific_hash: false,
            added_target_difficulty: false,
            block,
        }
    }

    pub fn with_timestamp(mut self, timestamp: EpochTime) -> Self {
        self.block.timestamp = timestamp;
        self
    }

    pub fn with_height(mut self, height: u64) -> Self {
        self.block.height = height;
        self
    }

    #[cfg(test)]
    pub fn with_target_difficulty(mut self, target_difficulty: Difficulty) -> Result<Self, ShareChainError> {
        self.added_target_difficulty = true;
        self.block.target_difficulty = target_difficulty;
        self.block.total_pow = self
            .block
            .total_pow
            .checked_add_difficulty(target_difficulty)
            .ok_or(ShareChainError::DifficultyOverflow)?;
        Ok(self)
    }

    #[cfg(test)]
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

    pub fn with_uncles(mut self, uncles: &Vec<Arc<P2Block>>) -> Result<Self, ShareChainError> {
        let mut block_uncles = Vec::new();
        for uncle in uncles {
            block_uncles.push((uncle.height, uncle.hash));
            self.block.total_pow = self
                .block
                .total_pow
                .checked_add_difficulty(uncle.target_difficulty)
                .ok_or(ShareChainError::DifficultyOverflow)?;
        }
        self.block.uncles = block_uncles;
        Ok(self)
    }

    pub fn build(mut self) -> Result<Arc<P2Block>, ShareChainError> {
        if !self.added_target_difficulty || self.block.prev_hash == BlockHash::zero() {
            if self.block.prev_hash == BlockHash::zero() {
                self.block.total_pow =
                    AccumulatedDifficulty::from_u128(u128::from(self.block.target_difficulty.as_u64()))
                        .map_err(|_| ShareChainError::DifficultyOverflow)?;
            } else {
                self.block.total_pow = self
                    .block
                    .total_pow
                    .checked_add_difficulty(self.block.target_difficulty)
                    .ok_or(ShareChainError::DifficultyOverflow)?;
            }
        }
        if !self.use_specific_hash {
            self.block.hash = self.block.generate_hash();
        }
        Ok(Arc::new(self.block))
    }
}

#[cfg(test)]
mod test {
    use tari_core::proof_of_work::Difficulty;
    use tari_utilities::epoch_time::EpochTime;

    use crate::sharechain::p2block::P2BlockBuilder;

    #[test]
    fn correctly_updates_block_target_difficulty() {
        let time = EpochTime::now();
        let block = (*P2BlockBuilder::new(None)
            .with_timestamp(time)
            .with_height(0)
            .with_target_difficulty(Difficulty::from_u64(15).unwrap())
            .unwrap()
            .build()
            .unwrap())
        .clone();

        let mut block2 = (*P2BlockBuilder::new(None)
            .with_timestamp(time)
            .with_height(0)
            .with_target_difficulty(Difficulty::from_u64(5).unwrap())
            .unwrap()
            .build()
            .unwrap())
        .clone();

        assert_ne!(block, block2);

        block2
            .change_target_difficulty(Difficulty::from_u64(15).unwrap())
            .unwrap();

        assert_eq!(block, block2);
    }
}
