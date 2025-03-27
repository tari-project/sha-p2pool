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

use std::{
    collections::HashMap,
    fs,
    path::Path,
    sync::{Arc, RwLock},
};

use anyhow::{anyhow, Error};
use log::{error, info};
use rkv::{
    backend::{BackendInfo, Lmdb, LmdbEnvironment},
    Manager,
    Rkv,
    StoreError,
    StoreOptions,
};
use tari_common_types::types::BlockHash;
use tari_utilities::ByteArray;

use super::P2Block;

const LOG_TARGET: &str = "tari::p2pool::sharechain::lmdb_block_storage";
pub struct LmdbBlockStorage {
    file_handle: Arc<RwLock<Rkv<LmdbEnvironment>>>,
}

impl LmdbBlockStorage {
    pub fn new_from_temp_dir() -> Self {
        use rand::{distributions::Alphanumeric, Rng};
        use tempfile::Builder;
        let instance: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(7)
            .map(char::from)
            .collect();
        let root = Builder::new().prefix("p2pool").suffix(&instance).tempdir().unwrap();
        fs::create_dir_all(root.path()).unwrap();
        let path = root.path();
        let mut manager = Manager::<LmdbEnvironment>::singleton().write().unwrap();
        let file_handle = manager.get_or_create(path, Rkv::new::<Lmdb>).unwrap();

        Self { file_handle }
    }

    pub fn new_from_path(path: &Path) -> Self {
        info!(target: LOG_TARGET, "Using block storage at {:?}", path);
        if !fs::exists(path).expect("could not get file") {
            fs::create_dir_all(path).unwrap();
        }
        let mut manager = Manager::<LmdbEnvironment>::singleton().write().unwrap();
        let file_handle = manager.get_or_create(path, Rkv::new::<Lmdb>).unwrap();

        let env = file_handle.read().expect("reader");
        let mut migrations = HashMap::new();
        {
            let store = env.open_single("migrations", StoreOptions::create()).unwrap();
            let reader = env.read().expect("reader");
            let iter = store.iter_start(&reader).unwrap();
            for r in iter {
                let (_k, v) = r.unwrap();
                match v {
                    rkv::Value::Str(s) => {
                        migrations.insert(s.to_string(), ());
                    },
                    _ => {
                        panic!("Invalid migration in storage");
                    },
                }
            }
        }
        if !migrations.contains_key("01_remove_block_cache") {
            let store = env.open_single("block_cache", StoreOptions::create()).unwrap();
            let mut writer = env.write().expect("writer");
            store.clear(&mut writer).unwrap();
            writer.commit().unwrap();

            let s = env.open_single("migrations", StoreOptions::create()).unwrap();
            let mut writer = env.write().expect("writer");
            s.put(
                &mut writer,
                1_u16.to_le_bytes(),
                &rkv::Value::Str("01_remove_block_cache"),
            )
            .unwrap();
            writer.commit().unwrap();
        }

        drop(env);

        Self { file_handle }
    }
}

impl BlockCache for LmdbBlockStorage {
    fn get(&self, hash: &BlockHash) -> Option<Arc<P2Block>> {
        let env = self.file_handle.read().expect("reader");
        let store = env.open_single("block_cache_v2", StoreOptions::create()).unwrap();
        let reader = env.read().expect("reader");
        let block = store.get(&reader, hash.as_bytes()).unwrap();
        if let Some(block) = block {
            match block {
                rkv::Value::Blob(b) => {
                    let block = Arc::new(bincode::deserialize(b).unwrap());
                    return Some(block);
                },
                _ => {
                    return None;
                },
            }
        }
        None
    }

    fn delete(&self, hash: &BlockHash) {
        let env = self.file_handle.read().expect("reader");
        let store = env.open_single("block_cache_v2", StoreOptions::create()).unwrap();
        let mut writer = env.write().expect("writer");
        store.delete(&mut writer, hash.as_bytes()).unwrap();
        if let Err(e) = writer.commit() {
            error!(target: LOG_TARGET, "Error deleting block from lmdb: {:?}", e);
        }
    }

    fn insert(&self, hash: BlockHash, block: Arc<P2Block>, force: bool) {
        //First we check if the block already exists, if it does we don't insert it, if force is set, we overwrite it
        if force {
            let env = self.file_handle.read().expect("reader");
            let store = env.open_single("block_cache_v2", StoreOptions::create()).unwrap();
            let reader = env.read().expect("reader");
            let block = store.get(&reader, hash.as_bytes()).unwrap();
            // we dont want to deserialise this block, so we just check if it exists
            if block.is_some() {
                return;
            }
        }
        // Retry if the map is full
        // This weird pattern of setting a bool is so that the env is closed before resizing, otherwise
        // you can't resize with active transactions.
        let mut next_resize = false;

        for _retry in 0..10 {
            let env = self.file_handle.read().expect("reader");
            if next_resize {
                resize_db(&env);
                // next_resize = false;
            }
            let store = env.open_single("block_cache_v2", StoreOptions::create()).unwrap();
            let mut writer = env.write().expect("writer");
            let block_blob = bincode::serialize(&block).unwrap();
            match store.put(&mut writer, hash.as_bytes(), &rkv::Value::Blob(&block_blob)) {
                Ok(_) => match writer.commit() {
                    Ok(_) => {
                        return;
                    },
                    Err(e) => match e {
                        StoreError::MapFull => {
                            next_resize = true;
                        },
                        _ => {
                            panic!("Error committing block to storage: {:?}", e)
                        },
                    },
                },
                Err(e) => match e {
                    StoreError::MapFull => {
                        next_resize = true;
                    },
                    _ => {
                        panic!("Error committing block to storage: {:?}", e)
                    },
                },
            }
        }
    }

    fn all_blocks(&self) -> Result<Vec<Arc<P2Block>>, Error> {
        let env = self.file_handle.read().expect("reader");
        let store = env.open_single("block_cache_v2", StoreOptions::create()).unwrap();
        let reader = env.read().expect("reader");
        let mut res: Vec<Arc<P2Block>> = vec![];
        let iter = store.iter_start(&reader)?;
        for r in iter {
            let (_k, v) = r?;
            match v {
                rkv::Value::Blob(b) => {
                    let block = Arc::new(bincode::deserialize(b).unwrap());
                    res.push(block);
                },
                _ => {
                    return Err(anyhow!("Invalid block in storage"));
                },
            }
        }
        res.sort_by(|a, b| a.height.cmp(&b.height));
        Ok(res)
    }
}

fn resize_db(env: &Rkv<LmdbEnvironment>) {
    let size = env.info().map(|i| i.map_size()).unwrap_or(0);
    // let new_size = (size as f64 * 1.2f64).ceil() as usize;
    let new_size = size * 2;
    env.set_map_size(new_size).unwrap();
}
pub trait BlockCache {
    fn get(&self, hash: &BlockHash) -> Option<Arc<P2Block>>;
    fn delete(&self, hash: &BlockHash);
    fn insert(&self, hash: BlockHash, block: Arc<P2Block>, force: bool);
    fn all_blocks(&self) -> Result<Vec<Arc<P2Block>>, Error>;
}

#[cfg(test)]
pub mod test {
    use std::collections::HashMap;

    use super::*;

    pub(crate) struct InMemoryBlockCache {
        blocks: Arc<RwLock<HashMap<BlockHash, Arc<P2Block>>>>,
    }

    impl InMemoryBlockCache {
        pub fn new() -> Self {
            Self {
                blocks: Arc::new(RwLock::new(HashMap::new())),
            }
        }
    }

    impl BlockCache for InMemoryBlockCache {
        fn get(&self, hash: &BlockHash) -> Option<Arc<P2Block>> {
            self.blocks.read().unwrap().get(hash).cloned()
        }

        fn delete(&self, hash: &BlockHash) {
            self.blocks.write().unwrap().remove(hash);
        }

        fn insert(&self, hash: BlockHash, block: Arc<P2Block>, force: bool) {
            self.blocks.write().unwrap().insert(hash, block);
        }

        fn all_blocks(&self) -> Result<Vec<Arc<P2Block>>, Error> {
            Ok(self.blocks.read().unwrap().values().cloned().collect())
        }
    }

    #[test]
    fn test_saving_and_retrieving_blocks() {
        let cache = LmdbBlockStorage::new_from_temp_dir();
        let block = Arc::new(P2Block::default());
        let hash = block.hash;
        cache.insert(hash, block.clone());
        let retrieved_block = cache.get(&hash).unwrap();
        assert_eq!(block, retrieved_block);
    }

    #[test]
    fn test_deleting_blocks() {
        let cache = LmdbBlockStorage::new_from_temp_dir();
        let block = Arc::new(P2Block::default());
        let hash = block.hash;
        cache.insert(hash, block.clone());
        let retrieved_block = cache.get(&hash).unwrap();
        assert_eq!(block, retrieved_block);
        cache.delete(&hash);
        assert!(cache.get(&hash).is_none());
    }
}
