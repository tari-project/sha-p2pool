use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use libp2p::PeerId;
use log::{debug, info};
use moka::future::{Cache, CacheBuilder};

use crate::server::p2p::messages::PeerInfo;

#[derive(Copy, Clone, Debug)]
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
    inner: Cache<PeerId, PeerStoreRecord>,
    // Max time to live for the items to avoid non-existing peers in list.
    ttl: Duration,
    tip_of_block_height: RwLock<Option<PeerStoreBlockHeightTip>>,
}

impl PeerStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: CacheBuilder::new(100_000)
                .time_to_live(ttl)
                .build(),
            ttl,
            tip_of_block_height: RwLock::new(None),
        }
    }

    pub async fn add(&self, peer_id: PeerId, peer_info: PeerInfo) {
        self.inner.insert(peer_id, PeerStoreRecord::new(peer_info)).await;
        self.set_tip_of_block_height().await;
    }

    pub async fn peer_count(&self) -> u64 {
        self.inner.entry_count()
    }

    async fn set_tip_of_block_height(&self) {
        if let Some((k, v)) =
            self.inner.iter()
                .max_by(|(k1, v1), (k2, v2)| {
                    v1.peer_info.current_height.cmp(&v2.peer_info.current_height)
                }) {
            // save result
            if let Ok(mut tip_height_opt) = self.tip_of_block_height.write() {
                if tip_height_opt.is_none() {
                    let _ = tip_height_opt.insert(
                        PeerStoreBlockHeightTip::new(
                            *k,
                            v.peer_info.current_height,
                        )
                    );
                } else {
                    let mut tip_height = tip_height_opt.unwrap();
                    tip_height.peer_id = *k;
                    tip_height.height = v.peer_info.current_height;
                }
            }
        }
    }

    pub async fn tip_of_block_height(&self) -> Option<PeerStoreBlockHeightTip> {
        if let Ok(result) = self.tip_of_block_height.read() {
            if result.is_some() {
                return Some(result.unwrap());
            }
        }
        None
    }

    pub async fn cleanup(&self) -> Vec<PeerId> {
        debug!("PEER STORE - cleanup");
        let mut expired_peers = vec![];

        for (k, v) in self.inner.iter() {
            debug!("PEER STORE - {:?} -> {:?}", k, v);
            let elapsed = v.created.elapsed();
            let expired = elapsed.gt(&self.ttl);
            debug!("{:?} ttl elapsed: {:?} <-> {:?}, Expired: {:?}", k, elapsed, &self.ttl, expired);
            if expired {
                expired_peers.push(*k);
                self.inner.remove(k.as_ref()).await;
            }
        }

        self.set_tip_of_block_height().await;

        expired_peers
    }
}