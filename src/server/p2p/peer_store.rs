// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use libp2p::PeerId;
use log::warn;
use tari_core::proof_of_work::PowAlgorithm;
use tari_utilities::epoch_time::EpochTime;

use crate::server::{http::stats_collector::StatsBroadcastClient, p2p::messages::PeerInfo};

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
pub(crate) struct PeerStoreRecord {
    pub peer_id: PeerId,
    pub peer_info: PeerInfo,
    pub created: Instant,
    pub last_sync_attempt: Option<Instant>,
    pub last_grey_list_reason: Option<String>,
    pub catch_up_attempts: u64,
}

impl PeerStoreRecord {
    pub fn new(peer_id: PeerId, peer_info: PeerInfo) -> Self {
        Self {
            peer_id,
            peer_info,
            last_sync_attempt: None,
            created: Instant::now(),
            last_grey_list_reason: None,
            catch_up_attempts: 0,
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
    whitelist_peers: HashMap<String, PeerStoreRecord>,
    greylist_peers: HashMap<String, PeerStoreRecord>,
    blacklist_peers: HashSet<String>,
    stats_broadcast_client: StatsBroadcastClient,
}

impl PeerStore {
    /// Constructs a new peer store with config.
    pub fn new(_config: &PeerStoreConfig, stats_broadcast_client: StatsBroadcastClient) -> Self {
        Self {
            stats_broadcast_client,
            whitelist_peers: HashMap::new(),
            greylist_peers: HashMap::new(),
            blacklist_peers: HashSet::new(),
            // peers_max_fail: config.peers_max_fail,
            // tip_of_block_height_sha3x: RwLock::new(None),
            // tip_of_block_height_random_x: RwLock::new(None),
            // peer_removals: CacheBuilder::new(100_000).time_to_live(config.peer_record_ttl).build(),
            // banned_peers: CacheBuilder::new(100_000).time_to_live(PEER_BAN_TIME).build(),
        }
    }

    pub fn num_catch_ups(&self, peer: &PeerId) -> Option<usize> {
        self.whitelist_peers
            .get(&peer.to_base58())
            .map(|record| record.catch_up_attempts as usize)
    }

    pub fn add_catch_up_attempt(&mut self, peer_id: &PeerId) {
        if let Some(entry) = self.whitelist_peers.get_mut(&peer_id.to_base58()) {
            let mut new_record = entry.clone();
            new_record.catch_up_attempts += 1;
            *entry = new_record;
        }
    }

    pub fn reset_catch_up_attempts(&mut self, peer_id: &PeerId) {
        if let Some(entry) = self.whitelist_peers.get_mut(&peer_id.to_base58()) {
            let mut new_record = entry.clone();
            new_record.catch_up_attempts = 0;
            *entry = new_record;
        }
    }

    pub fn exists(&self, peer_id: &PeerId) -> bool {
        self.whitelist_peers.contains_key(&peer_id.to_base58()) ||
            self.greylist_peers.contains_key(&peer_id.to_base58()) ||
            self.blacklist_peers.contains(&peer_id.to_base58())
    }

    pub fn whitelist_peers(&self) -> &HashMap<String, PeerStoreRecord> {
        &self.whitelist_peers
    }

    pub fn greylist_peers(&self) -> &HashMap<String, PeerStoreRecord> {
        &self.greylist_peers
    }

    pub fn best_peers_to_sync(&self, count: usize, algo: PowAlgorithm) -> Vec<PeerStoreRecord> {
        let mut peers = self.whitelist_peers.values().collect::<Vec<_>>();
        // ignore all peers records that are older than 30 minutes
        let timestamp = EpochTime::now().as_u64() - 60 * 1;
        peers.retain(|peer| peer.peer_info.timestamp > timestamp);
        match algo {
            PowAlgorithm::RandomX => {
                peers.sort_by(|a, b| {
                    a.peer_info
                        .current_random_x_height
                        .cmp(&b.peer_info.current_random_x_height)
                });
            },

            PowAlgorithm::Sha3x => {
                peers.sort_by(|a, b| a.peer_info.current_sha3x_height.cmp(&b.peer_info.current_sha3x_height));
            },
        }
        peers.reverse();
        peers.truncate(count);
        peers.into_iter().map(|record| record.clone()).collect()
    }

    pub fn update_last_sync_attempt(&mut self, peer_id: PeerId) {
        if let Some(entry) = self.whitelist_peers.get_mut(&peer_id.to_base58()) {
            let mut new_record = entry.clone();
            new_record.last_sync_attempt = Some(Instant::now());
            *entry = new_record;
        }
        if let Some(entry) = self.greylist_peers.get_mut(&peer_id.to_base58()) {
            let mut new_record = entry.clone();
            new_record.last_sync_attempt = Some(Instant::now());
            *entry = new_record;
        }
    }

    /// Add a new peer to store.
    /// If a peer already exists, just replaces it.
    pub async fn add(&mut self, peer_id: PeerId, peer_info: PeerInfo) -> (AddPeerStatus, Option<Instant>) {
        if self.blacklist_peers.contains(&peer_id.to_base58()) {
            return (AddPeerStatus::Blacklisted, None);
        }

        if let Some(grey) = self.greylist_peers.get(&peer_id.to_base58()) {
            return (AddPeerStatus::Greylisted, grey.last_sync_attempt);
        }

        if let Some(entry) = self.whitelist_peers.get_mut(&peer_id.to_base58()) {
            let previous_sync_attempt = entry.last_sync_attempt;
            let mut new_record = PeerStoreRecord::new(peer_id, peer_info);
            new_record.last_sync_attempt = previous_sync_attempt;
            new_record.created = entry.created;

            *entry = new_record;
            // self.whitelist_peers.insert(peer_id, PeerStoreRecord::new(peer_info));
            return (AddPeerStatus::Existing, previous_sync_attempt);
        }

        self.whitelist_peers
            .insert(peer_id.to_base58(), PeerStoreRecord::new(peer_id, peer_info));
        let _ = self.stats_broadcast_client.send_new_peer(
            self.whitelist_peers.len() as u64,
            self.greylist_peers.len() as u64,
            self.blacklist_peers.len() as u64,
        );

        // self.peer_removals.insert(peer_id, removal_count).await;
        // }

        // self.set_tip_of_block_heights().await;
        (AddPeerStatus::NewPeer, None)
    }

    pub fn clear_grey_list(&mut self) {
        for (peer_id, record) in self.greylist_peers.iter() {
            self.whitelist_peers.insert(peer_id.clone(), record.clone());
        }
        self.greylist_peers.clear();
        let _ = self.stats_broadcast_client.send_new_peer(
            self.whitelist_peers.len() as u64,
            self.greylist_peers.len() as u64,
            self.blacklist_peers.len() as u64,
        );
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
        self.whitelist_peers.len() as u64
    }

    pub async fn move_to_grey_list(&mut self, peer_id: PeerId, reason: String) {
        if self.whitelist_peers.contains_key(&peer_id.to_base58()) {
            let record = self.whitelist_peers.remove(&peer_id.to_base58());
            if let Some(mut record) = record {
                warn!(target: LOG_TARGET, "Greylisting peer {} because of: {}", peer_id, reason);
                record.last_grey_list_reason = Some(reason.clone());
                self.greylist_peers.insert(peer_id.to_base58(), record);
                let _ = self.stats_broadcast_client.send_new_peer(
                    self.whitelist_peers.len() as u64,
                    self.greylist_peers.len() as u64,
                    self.blacklist_peers.len() as u64,
                );
            }
        }
    }

    pub fn is_blacklisted(&self, peer_id: &PeerId) -> bool {
        self.blacklist_peers.contains(&peer_id.to_base58())
    }

    pub fn is_whitelisted(&self, peer_id: &PeerId) -> bool {
        if self.whitelist_peers.contains_key(&peer_id.to_base58()) {
            return true;
        }
        if self.whitelist_peers.is_empty() && self.greylist_peers.contains_key(&peer_id.to_base58()) {
            return true;
        }
        return false;
    }
}
