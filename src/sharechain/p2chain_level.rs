use std::{collections::HashMap, sync::Arc};

use tari_common_types::types::{BlockHash, FixedHash};

use crate::sharechain::{error::Error, p2block::P2Block};

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
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
// INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
// WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
// USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
/// A collection of blocks with the same height.
#[derive(Debug, Clone)]
pub struct P2ChainLevel {
    pub blocks: HashMap<BlockHash, Arc<P2Block>>,
    pub height: u64,
    pub chain_block: BlockHash,
}

impl P2ChainLevel {
    pub fn new(block: Arc<P2Block>) -> Self {
        let mut blocks = HashMap::new();
        // although this is the only block on this level, it might not be part of the main chain, so we need to set this
        // later
        let chain_block = FixedHash::zero();
        let height = block.height;
        blocks.insert(block.hash, block);
        Self {
            blocks,
            height,
            chain_block,
        }
    }

    pub fn new_empty(height: u64) -> Self {
        Self {
            blocks: HashMap::new(),
            height,
            chain_block: BlockHash::zero(),
        }
    }

    pub fn add_block(&mut self, block: Arc<P2Block>) -> Result<(), crate::sharechain::error::Error> {
        if self.height != block.height {
            return Err(Error::InvalidBlock {
                reason: "Block height does not match the chain level height".to_string(),
            });
        }
        self.blocks.insert(block.hash, block);
        Ok(())
    }

    pub fn block_in_main_chain(&self) -> Option<&Arc<P2Block>> {
        self.blocks.get(&self.chain_block)
    }
}

#[cfg(test)]
mod test {
    use tari_utilities::epoch_time::EpochTime;

    use crate::sharechain::{in_memory::test::new_random_address, p2block::P2Block, p2chain_level::P2ChainLevel};

    #[test]
    fn it_gets_the_block_chain() {
        let address = new_random_address();
        let block = P2Block::builder()
            .with_timestamp(EpochTime::now())
            .with_height(0)
            .with_miner_wallet_address(address.clone())
            .build();
        let mut chain_level = P2ChainLevel::new(block.clone());
        chain_level.chain_block = block.generate_hash();

        assert_eq!(
            chain_level.block_in_main_chain().unwrap().generate_hash(),
            block.generate_hash()
        );

        let block_2 = P2Block::builder()
            .with_timestamp(EpochTime::now())
            .with_height(0)
            // this is not correct, but we want the hashes to be different from the blocks
            .with_prev_hash(block.generate_hash())
            .with_miner_wallet_address(address.clone())
            .build();

        chain_level.add_block(block_2.clone()).unwrap();
        assert_eq!(
            chain_level.block_in_main_chain().unwrap().generate_hash(),
            block.generate_hash()
        );

        let block_3 = P2Block::builder()
            .with_timestamp(EpochTime::now())
            .with_height(0)
            // this is not correct, but we want the hashes to be different from the blocks
            .with_prev_hash(block_2.generate_hash())
            .with_miner_wallet_address(address)
            .build();

        chain_level.add_block(block_3.clone()).unwrap();
        chain_level.chain_block = block_3.generate_hash();

        assert_eq!(
            chain_level.block_in_main_chain().unwrap().generate_hash(),
            block_3.generate_hash()
        );
    }
}
