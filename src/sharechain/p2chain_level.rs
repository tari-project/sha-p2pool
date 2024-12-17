// Copyright 2024. The Tari Project
//
// Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
// following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
// disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
// following disclaimer in the documentation and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
// products derived from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED
// WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A
// PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY
// DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
// PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
// CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR
// OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH
// DAMAGE.

use std::sync::{Arc, RwLock};

use tari_common_types::types::{BlockHash, FixedHash};

use super::lmdb_block_storage::BlockCache;
use crate::sharechain::{error::ShareChainError, p2block::P2Block};

/// A collection of blocks with the same height.
pub struct P2ChainLevel<T: BlockCache> {
    // pub blocks: HashMap<BlockHash, Arc<P2Block>>,
    block_cache: Arc<T>,
    height: u64,
    chain_block: RwLock<BlockHash>,
    block_hashes: RwLock<Vec<BlockHash>>,
}

impl<T: BlockCache> P2ChainLevel<T> {
    pub fn new(block: Arc<P2Block>, block_cache: Arc<T>) -> Self {
        // although this is the only block on this level, it might not be part of the main chain, so we need to set this
        // later
        let chain_block = RwLock::new(FixedHash::zero());
        let height = block.height;
        let hash = block.hash;
        block_cache.insert(block.hash, block);
        Self {
            block_cache,
            height,
            chain_block,
            block_hashes: RwLock::new(vec![hash]),
        }
    }

    pub fn height(&self) -> u64 {
        self.height
    }

    pub fn chain_block(&self) -> BlockHash {
        *self.chain_block.read().expect("read lock")
    }

    pub fn set_chain_block(&self, hash: BlockHash) {
        let mut lock = self.chain_block.write().expect("could not lock");
        *lock = hash;
    }

    pub fn add_block(&self, block: Arc<P2Block>) -> Result<(), ShareChainError> {
        if self.height != block.height {
            return Err(ShareChainError::InvalidBlock {
                reason: "Block height does not match the chain level height".to_string(),
            });
        }
        self.block_hashes.write().expect("could not lock").push(block.hash);
        self.block_cache.insert(block.hash, block);
        Ok(())
    }

    pub fn block_in_main_chain(&self) -> Option<Arc<P2Block>> {
        self.block_cache.get(&self.chain_block())
    }

    pub fn get(&self, hash: &BlockHash) -> Option<Arc<P2Block>> {
        self.block_cache.get(hash)
    }

    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.block_cache.contains(hash)
    }

    pub fn all_blocks(&self) -> Vec<Arc<P2Block>> {
        self.block_hashes
            .read()
            .expect("could not lock")
            .iter()
            .filter_map(|hash| self.block_cache.get(hash))
            .collect()
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use tari_utilities::epoch_time::EpochTime;

    use crate::sharechain::{
        in_memory::test::new_random_address,
        lmdb_block_storage::test::InMemoryBlockCache,
        p2block::P2BlockBuilder,
        p2chain_level::P2ChainLevel,
    };

    #[test]
    fn it_gets_the_block_chain() {
        let address = new_random_address();
        let block = P2BlockBuilder::new(None)
            .with_timestamp(EpochTime::now())
            .with_height(0)
            .with_miner_wallet_address(address.clone())
            .build()
            .unwrap();
        let chain_level = P2ChainLevel::new(block.clone(), Arc::new(InMemoryBlockCache::new()));
        chain_level.set_chain_block(block.generate_hash());

        assert_eq!(
            chain_level.block_in_main_chain().unwrap().generate_hash(),
            block.generate_hash()
        );
        // this is not correct, but we want the hashes to be different from the blocks
        let block_2 = P2BlockBuilder::new(Some(&block))
            .with_timestamp(EpochTime::now())
            .with_height(0)
            .with_miner_wallet_address(address.clone())
            .build()
            .unwrap();

        chain_level.add_block(block_2.clone()).unwrap();
        assert_eq!(
            chain_level.block_in_main_chain().unwrap().generate_hash(),
            block.generate_hash()
        );

        // this is not correct, but we want the hashes to be different from the blocks
        let block_3 = P2BlockBuilder::new(Some(&block_2))
            .with_timestamp(EpochTime::now())
            .with_height(0)
            .with_miner_wallet_address(address)
            .build()
            .unwrap();

        chain_level.add_block(block_3.clone()).unwrap();
        chain_level.set_chain_block(block_3.generate_hash());

        assert_eq!(
            chain_level.block_in_main_chain().unwrap().generate_hash(),
            block_3.generate_hash()
        );
    }
}
