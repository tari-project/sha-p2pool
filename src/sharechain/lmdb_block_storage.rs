use std::{
    collections::HashMap,
    env,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use rkv::{
    backend::{Lmdb, LmdbEnvironment},
    Manager,
    Rkv,
    StoreOptions,
};
use tari_common_types::types::BlockHash;
use tari_utilities::ByteArray;
use tempfile::{Builder, TempDir};

use super::P2Block;

pub(crate) struct LmdbBlockStorage {
    // path: PathBuf,
    temp_dir: TempDir,
    file_handle: Arc<RwLock<Rkv<LmdbEnvironment>>>,
}

impl LmdbBlockStorage {
    pub fn new_from_temp_dir() -> Self {
        let root = Builder::new().prefix("p2pool").tempdir().unwrap();
        fs::create_dir_all(root.path()).unwrap();
        let path = root.path();
        let mut manager = Manager::<LmdbEnvironment>::singleton().write().unwrap();
        let file_handle = manager.get_or_create(path, Rkv::new::<Lmdb>).unwrap();

        // {
        //     let env = file_handle.read().unwrap();

        //     // Then you can use the environment handle to get a handle to a datastore:
        //     let store = env.open_single("block_cache", StoreOptions::create()).unwrap();
        // }
        Self {
            temp_dir: root,
            file_handle,
        }
    }
}

impl BlockCache for LmdbBlockStorage {
    fn get(&self, hash: &BlockHash) -> Option<Arc<P2Block>> {
        None
        // let env = self.file_handle.read().expect("reader");
        // // Then you can use the environment handle to get a handle to a datastore:
        // let store = env.open_single("block_cache", StoreOptions::create()).unwrap();
        // let reader = env.read().expect("reader");
        // let block = store.get(&reader, hash.as_bytes()).unwrap();
        // // let block = block.map(|b| Arc::new(bincode::deserialize(&b).unwrap()));
        // todo!()
    }
}

pub trait BlockCache {
    fn get(&self, hash: &BlockHash) -> Option<Arc<P2Block>>;
}

#[cfg(test)]
pub mod test {
    use super::*;

    pub(crate) struct InMemoryBlockCache {
        blocks: HashMap<BlockHash, Arc<P2Block>>,
    }

    impl InMemoryBlockCache {
        pub fn new() -> Self {
            Self { blocks: HashMap::new() }
        }
    }

    impl BlockCache for InMemoryBlockCache {
        fn get(&self, hash: &BlockHash) -> Option<Arc<P2Block>> {
            self.blocks.get(hash).cloned()
        }
    }
}
