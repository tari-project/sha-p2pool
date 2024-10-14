// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::HashSet,
    sync::RwLock,
    time::{Duration, Instant},
};

use itertools::Itertools;
use libp2p::PeerId;
use log::{debug, warn};
use moka::future::{Cache, CacheBuilder};
use tari_core::proof_of_work::PowAlgorithm;
use tari_utilities::epoch_time::EpochTime;

use crate::server::p2p::{messages::PeerInfo, Squad};

const LOG_TARGET: &str = "tari::p2pool::server::p2p::peer_store";
// const PEER_BAN_TIME: Duration = Duration::from_secs(60 * 5);

#[derive(Copy, Clone, Debug)]
pub struct PeerStoreConfig {
    pub peer_record_ttl: Duration,
    // pub peers_max_fail: u64,
}

impl Default for PeerStoreConfig {
    fn default() -> Self {
        Self {
            peer_record_ttl: Duration::from_secs(60 * 60),
        }
    }
}

/// A record in peer store that holds all needed info of a peer.
#[derive(Clone, Debug)]
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
// #[derive(Copy, Clone, Debug)]
// pub struct PeerStoreBlockHeightTip {
//     pub peer_id: PeerId,
//     pub height: u64,
// }

// impl PeerStoreBlockHeightTip {
//     pub fn new(peer_id: PeerId, height: u64) -> Self {
//         Self { peer_id, height }
//     }
// }

pub enum AddPeerStatus {
    NewPeer,
    Existing,
    Blacklisted,
    Greylisted,
}

/// A peer store, which stores all the known peers (from broadcasted [`PeerInfo`] messages) in-memory.
/// This implementation is thread safe and async, so an [`Arc<PeerStore>`] is enough to be used to share.
pub struct PeerStore {
    whitelist_peers: Cache<PeerId, PeerStoreRecord>,
    greylist_peers: HashSet<PeerId>,
    blacklist_peers: HashSet<PeerId>,
}

impl PeerStore {
    /// Constructs a new peer store with config.
    pub fn new(config: &PeerStoreConfig) -> Self {
        Self {
            whitelist_peers: CacheBuilder::new(10000).time_to_live(config.peer_record_ttl).build(),
            greylist_peers: HashSet::new(),
            blacklist_peers: HashSet::new(),
            // peers_max_fail: config.peers_max_fail,
            // tip_of_block_height_sha3x: RwLock::new(None),
            // tip_of_block_height_random_x: RwLock::new(None),
            // peer_removals: CacheBuilder::new(100_000).time_to_live(config.peer_record_ttl).build(),
            // banned_peers: CacheBuilder::new(100_000).time_to_live(PEER_BAN_TIME).build(),
        }
    }

    /// Add a new peer to store.
    /// If a peer already exists, just replaces it.
    pub async fn add(&self, peer_id: PeerId, peer_info: PeerInfo) -> AddPeerStatus {
        dbg!(&peer_info);
        // if self.banned_peers.contains_key(&peer_id) {
        // return;
        // }
        // let removal_count = self.peer_removals.get(&peer_id).await.unwrap_or(0);
        // if removal_count >= self.peers_max_fail {
        // warn!("Banning peer {peer_id:?} for {:?}!", PEER_BAN_TIME);
        // self.peer_removals.remove(&peer_id).await;
        // self.banned_peers.insert(peer_id, ()).await;
        // } else {
        if self.blacklist_peers.contains(&peer_id) {
            return AddPeerStatus::Blacklisted;
        }

        if self.greylist_peers.contains(&peer_id) {
            return AddPeerStatus::Greylisted;
        }

        if self.whitelist_peers.contains_key(&peer_id) {
            self.whitelist_peers
                .insert(peer_id, PeerStoreRecord::new(peer_info))
                .await;
            return AddPeerStatus::Existing;
        }

        self.whitelist_peers
            .insert(peer_id, PeerStoreRecord::new(peer_info))
            .await;
        // self.peer_removals.insert(peer_id, removal_count).await;
        // }

        // self.set_tip_of_block_heights().await;
        AddPeerStatus::NewPeer
    }

    /// Removes a peer from store.
    // pub async fn remove(&self, peer_id: &PeerId) {
    //     // if self.banned_peers.contains_key(peer_id) {
    //     // return;
    //     // }
    //     self.peers.remove(peer_id).await;

    //     // counting peer removals
    //     // let removal_count = match self.peer_removals.get(peer_id).await {
    //     //     Some(value) => {
    //     //         let removals = value + 1;
    //     //         self.peer_removals.insert(*peer_id, removals).await;
    //     //         removals
    //     //     },
    //     //     None => {
    //     //         self.peer_removals.insert(*peer_id, 1).await;
    //     //         1
    //     //     },
    //     // };
    //     // if removal_count >= self.peers_max_fail {
    //     //     warn!("Banning peer {peer_id:?} for {:?}!", PEER_BAN_TIME);
    //     //     self.peer_removals.remove(peer_id).await;
    //     //     self.banned_peers.insert(*peer_id, ()).await;
    //     // }

    //     self.set_tip_of_block_heights().await;
    //     self.set_last_connected().await;
    // }

    /// Collects all current squads from all PeerInfo collected from broadcasts.
    // pub async fn squads(&self) -> Vec<Squad> {
    //     self.peers
    //         .iter()
    //         .map(|(_, record)| record.peer_info.squad)
    //         .unique()
    //         .collect_vec()
    // }
    /// Returns count of peers.
    /// Note: it is needed to calculate number of validations needed to make sure a new block is valid.
    pub async fn peer_count(&self) -> u64 {
        self.whitelist_peers.entry_count()
    }

    pub async fn move_to_grey_list(&mut self, peer_id: PeerId) {
        if self.whitelist_peers.contains_key(&peer_id) {
            self.whitelist_peers.remove(&peer_id).await;
            self.greylist_peers.insert(peer_id);
        }
    }

    pub fn is_whitelisted(&self, peer_id: &PeerId) -> bool {
        self.whitelist_peers.contains_key(peer_id)
    }

    // async fn set_tip_of_block_heights(&self) {
    //     self.set_tip_of_block_height(PowAlgorithm::RandomX).await;
    //     self.set_tip_of_block_height(PowAlgorithm::Sha3x).await;
    // }

    // /// Sets the actual highest block height with peer.
    // async fn set_tip_of_block_height(&self, pow: PowAlgorithm) {
    //     let tip_of_block = match pow {
    //         PowAlgorithm::RandomX => &self.tip_of_block_height_random_x,
    //         PowAlgorithm::Sha3x => &self.tip_of_block_height_sha3x,
    //     };
    //     if let Some((k, v)) = self
    //         .peers
    //         .iter()
    //         .filter(|(peer_id, _)| !self.banned_peers.contains_key(peer_id))
    //         .max_by(|(_k1, v1), (_k2, v2)| {
    //             let current_height_v1 = match pow {
    //                 PowAlgorithm::RandomX => v1.peer_info.current_random_x_height,
    //                 PowAlgorithm::Sha3x => v1.peer_info.current_sha3x_height,
    //             };
    //             let current_height_v2 = match pow {
    //                 PowAlgorithm::RandomX => v2.peer_info.current_random_x_height,
    //                 PowAlgorithm::Sha3x => v2.peer_info.current_sha3x_height,
    //             };
    //             current_height_v1.cmp(&current_height_v2)
    //         })
    //     {
    //         // save result
    //         if let Ok(mut tip_height_opt) = tip_of_block.write() {
    //             let current_height = match pow {
    //                 PowAlgorithm::RandomX => v.peer_info.current_random_x_height,
    //                 PowAlgorithm::Sha3x => v.peer_info.current_sha3x_height,
    //             };
    //             if tip_height_opt.is_none() {
    //                 let _ = tip_height_opt.insert(PeerStoreBlockHeightTip::new(*k, current_height));
    //             } else {
    //                 *tip_height_opt = Some(PeerStoreBlockHeightTip::new(*k, current_height));
    //             }
    //         }
    //     } else if let Ok(mut tip_height_opt) = tip_of_block.write() {
    //         *tip_height_opt = None;
    //     } else {
    //         warn!(target: LOG_TARGET, "Failed to set tip height!");
    // }
    // }

    // Returns peer with the highest share chain height.
    // pub async fn tip_of_block_height(&self, pow: PowAlgorithm) -> Option<PeerStoreBlockHeightTip> {
    //     let tip_of_block_height = match pow {
    //         PowAlgorithm::RandomX => &self.tip_of_block_height_random_x,
    //         PowAlgorithm::Sha3x => &self.tip_of_block_height_sha3x,
    //     };
    //     if let Ok(result) = tip_of_block_height.read() {
    //         if result.is_some() {
    //             return Some(result.unwrap());
    //         }
    //     }
    //     None
    // }
}
