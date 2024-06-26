use std::sync::RwLock;
use std::time::{Duration, Instant};

use libp2p::PeerId;
use log::debug;
use moka::future::{Cache, CacheBuilder};

use crate::server::p2p::messages::PeerInfo;

const LOG_TARGET: &str = "peer_store";

#[derive(Copy, Clone, Debug)]
pub struct PeerStoreConfig {
    pub peer_record_ttl: Duration,
}

impl Default for PeerStoreConfig {
    fn default() -> Self {
        Self {
            peer_record_ttl: Duration::from_secs(10),
        }
    }
}

/// A record in peer store that holds all needed info of a peer.
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

/// Tip of height from known peers.
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

/// A peer store, which stores all the known peers (from broadcasted [`PeerInfo`] messages) in-memory.
/// This implementation is thread safe and async, so an [`Arc<PeerStore>`] is enough to be used to share.
pub struct PeerStore {
    inner: Cache<PeerId, PeerStoreRecord>,
    // Max time to live for the items to avoid non-existing peers in list.
    ttl: Duration,
    // Peer with the highest share chain height.
    tip_of_block_height: RwLock<Option<PeerStoreBlockHeightTip>>,
}

impl PeerStore {
    /// Constructs a new peer store with config.
    pub fn new(config: &PeerStoreConfig) -> Self {
        Self {
            inner: CacheBuilder::new(100_000)
                .time_to_live(config.peer_record_ttl * 2)
                .build(),
            ttl: config.peer_record_ttl,
            tip_of_block_height: RwLock::new(None),
        }
    }

    /// Add a new peer to store.
    /// If a peer already exists, just replaces it.
    pub async fn add(&self, peer_id: PeerId, peer_info: PeerInfo) {
        self.inner.insert(peer_id, PeerStoreRecord::new(peer_info)).await;
        self.set_tip_of_block_height().await;
    }

    /// Returns count of peers.
    /// Note: it is needed to calculate number of validations needed to make sure a new block is valid.
    pub async fn peer_count(&self) -> u64 {
        self.inner.entry_count()
    }

    /// Sets the actual highest block height with peer.
    async fn set_tip_of_block_height(&self) {
        if let Some((k, v)) =
            self.inner.iter()
                .max_by(|(_k1, v1), (_k2, v2)| {
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

    /// Returns peer with the highest share chain height.
    pub async fn tip_of_block_height(&self) -> Option<PeerStoreBlockHeightTip> {
        if let Ok(result) = self.tip_of_block_height.read() {
            if result.is_some() {
                return Some(result.unwrap());
            }
        }
        None
    }

    /// Clean up expired peers.
    pub async fn cleanup(&self) -> Vec<PeerId> {
        let mut expired_peers = vec![];

        for (k, v) in self.inner.iter() {
            debug!(target: LOG_TARGET, "{:?} -> {:?}", k, v);
            let elapsed = v.created.elapsed();
            let expired = elapsed.gt(&self.ttl);
            debug!(target: LOG_TARGET, "{:?} ttl elapsed: {:?} <-> {:?}, Expired: {:?}", k, elapsed, &self.ttl, expired);
            if expired {
                expired_peers.push(*k);
                self.inner.remove(k.as_ref()).await;
            }
        }

        self.set_tip_of_block_height().await;

        expired_peers
    }
}