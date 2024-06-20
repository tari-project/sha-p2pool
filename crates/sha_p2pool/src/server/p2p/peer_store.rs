use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use libp2p::PeerId;

use crate::server::p2p::messages::PeerInfo;

pub struct PeerStoreRecord {
    peer_info: PeerInfo,
    created: Instant,
}

impl PeerStoreRecord {
    pub fn new(peer_info: PeerInfo) -> Self {
        Self {
            peer_info,
            created: Instant::now(),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct PeerStoreBlockHeightTip {
    pub peer_id: PeerId,
    pub height: u64,
}

impl PeerStoreBlockHeightTip {
    pub fn new(peer_id: PeerId, height: u64) -> Self {
        Self {
            peer_id,
            height,
        }
    }
}

pub struct PeerStore {
    inner: Arc<DashMap<PeerId, PeerStoreRecord>>,
    // Max time to live for the items to avoid non-existing peers in list.
    ttl: Duration,
    tip_of_block_height: RwLock<Option<PeerStoreBlockHeightTip>>,
}

impl PeerStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            ttl,
            tip_of_block_height: RwLock::new(None),
        }
    }

    pub fn add(&self, peer_id: PeerId, peer_info: PeerInfo) {
        self.inner.insert(peer_id, PeerStoreRecord::new(peer_info));
        self.set_tip_of_block_height();
    }

    pub fn peer_count(&self) -> usize {
        self.inner.len()
    }

    fn set_tip_of_block_height(&self) {
        if let Some(result) =
            self.inner.iter()
                .max_by(|r1, r2| {
                    r1.peer_info.current_height.cmp(&r2.peer_info.current_height)
                }) {
            // save result
            if let Ok(mut tip_height_opt) = self.tip_of_block_height.write() {
                if tip_height_opt.is_none() {
                    let _ = tip_height_opt.insert(
                        PeerStoreBlockHeightTip::new(
                            *result.key(),
                            result.peer_info.current_height,
                        )
                    );
                } else {
                    let mut tip_height = tip_height_opt.unwrap();
                    tip_height.peer_id = *result.key();
                    tip_height.height = result.peer_info.current_height;
                }
            }
        }
    }

    pub fn tip_of_block_height(&self) -> Option<PeerStoreBlockHeightTip> {
        if let Ok(result) = self.tip_of_block_height.read() {
            if result.is_some() {
                return Some(result.unwrap());
            }
        }
        None
    }

    pub fn cleanup(&self) {
        self.inner.iter()
            .filter(|record| {
                let elapsed = record.created.elapsed();
                elapsed.gt(&self.ttl)
            }).for_each(|record| {
            self.inner.remove(record.key());
        });
        self.set_tip_of_block_height();
    }
}