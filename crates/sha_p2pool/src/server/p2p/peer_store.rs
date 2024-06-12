use std::sync::Arc;
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
    peer_id: PeerId,
    height: u64,
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
    tip_of_block_height: Option<PeerStoreBlockHeightTip>,
}

impl PeerStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            ttl,
            tip_of_block_height: None,
        }
    }

    pub fn add(&mut self, peer_id: PeerId, peer_info: PeerInfo) {
        self.inner.insert(peer_id, PeerStoreRecord::new(peer_info));
        self.set_tip_of_block_height();
    }

    fn set_tip_of_block_height(&mut self) {
        if let Some(result) =
            self.inner.iter()
                .max_by(|r1, r2| {
                    r1.peer_info.current_height.cmp(&r2.peer_info.current_height)
                }) {
            self.tip_of_block_height = Some(
                PeerStoreBlockHeightTip::new(
                    *result.key(),
                    result.peer_info.current_height,
                ),
            )
        }
    }

    pub fn tip_of_block_height(&self) -> Option<PeerStoreBlockHeightTip> {
        self.tip_of_block_height
    }

    pub fn cleanup(&mut self) {
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