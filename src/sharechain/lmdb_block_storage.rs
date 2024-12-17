use std::{
    fs,
    sync::{Arc, RwLock},
};

use rkv::{
    backend::{BackendInfo, Lmdb, LmdbEnvironment},
    Manager,
    Rkv,
    StoreError,
    StoreOptions,
};
use tari_common_types::types::BlockHash;
use tari_utilities::ByteArray;
use tempfile::Builder;

use super::P2Block;
use crate::server::p2p::messages::{deserialize_message, serialize_message};

pub(crate) struct LmdbBlockStorage {
    file_handle: Arc<RwLock<Rkv<LmdbEnvironment>>>,
}

impl LmdbBlockStorage {
    pub fn new_from_temp_dir() -> Self {
        let root = Builder::new().prefix("p2pool").tempdir().unwrap();
        fs::create_dir_all(root.path()).unwrap();
        let path = root.path();
        let mut manager = Manager::<LmdbEnvironment>::singleton().write().unwrap();
        let file_handle = manager.get_or_create(path, Rkv::new::<Lmdb>).unwrap();

        Self { file_handle }
    }
}

impl BlockCache for LmdbBlockStorage {
    fn get(&self, hash: &BlockHash) -> Option<Arc<P2Block>> {
        let env = self.file_handle.read().expect("reader");
        let store = env.open_single("block_cache", StoreOptions::create()).unwrap();
        let reader = env.read().expect("reader");
        let block = store.get(&reader, hash.as_bytes()).unwrap();
        if let Some(block) = block {
            match block {
                rkv::Value::Blob(b) => {
                    let block = Arc::new(deserialize_message(b).unwrap());
                    return Some(block);
                },
                _ => {
                    return None;
                },
            }
        }
        None
    }

    fn insert(&self, hash: BlockHash, block: Arc<P2Block>) {
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
            let store = env.open_single("block_cache", StoreOptions::create()).unwrap();
            // dbg!(_retry);
            let mut writer = env.write().expect("writer");
            let block_blob = serialize_message(&block).unwrap();
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
}

fn resize_db(env: &Rkv<LmdbEnvironment>) {
    let size = env.info().map(|i| i.map_size()).unwrap_or(0);
    // dbg!(size);
    // let new_size = (size as f64 * 1.2f64).ceil() as usize;
    let new_size = size * 2;
    env.set_map_size(new_size).unwrap();
}
pub trait BlockCache {
    fn get(&self, hash: &BlockHash) -> Option<Arc<P2Block>>;
    fn insert(&self, hash: BlockHash, block: Arc<P2Block>);
    fn contains(&self, hash: &BlockHash) -> bool {
        self.get(hash).is_some()
    }
}

#[cfg(test)]
pub mod test {
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

        fn insert(&self, hash: BlockHash, block: Arc<P2Block>) {
            self.blocks.write().unwrap().insert(hash, block);
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
}
