// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    time::Instant,
};

use libp2p::PeerId;
use log::warn;
use tari_core::proof_of_work::PowAlgorithm;
use tari_utilities::epoch_time::EpochTime;

use crate::server::{http::stats_collector::StatsBroadcastClient, p2p::messages::PeerInfo};

const LOG_TARGET: &str = "tari::p2pool::peer_store";
// const PEER_BAN_TIME: Duration = Duration::from_secs(60 * 5);
const MAX_GREY_LISTINGS: u64 = 5;

/// A record in peer store that holds all needed info of a peer.
#[derive(Clone, Debug)]
pub(crate) struct PeerStoreRecord {
    pub peer_id: PeerId,
    pub peer_info: PeerInfo,
    pub created: Instant,
    pub last_rx_sync_attempt: Option<Instant>,
    pub last_sha3x_sync_attempt: Option<Instant>,
    pub num_grey_listings: u64,
    pub last_grey_list_reason: Option<String>,
    pub catch_up_attempts: u64,
    pub last_ping: Option<EpochTime>,
}

impl PeerStoreRecord {
    pub fn new(peer_id: PeerId, peer_info: PeerInfo) -> Self {
        Self {
            peer_id,
            peer_info,
            last_rx_sync_attempt: None,
            last_sha3x_sync_attempt: None,
            num_grey_listings: 0,
            created: Instant::now(),
            last_grey_list_reason: None,
            catch_up_attempts: 0,
            last_ping: None,
        }
    }

    pub fn last_seen(&self) -> EpochTime {
        self.last_ping
            .unwrap_or_else(|| EpochTime::from(self.peer_info.timestamp))
    }
}

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
    blacklist_peers: HashMap<String, PeerStoreRecord>,
    stats_broadcast_client: StatsBroadcastClient,
    seed_peers: Vec<PeerId>,
}

impl PeerStore {
    /// Constructs a new peer store with config.
    pub fn new(stats_broadcast_client: StatsBroadcastClient) -> Self {
        Self {
            stats_broadcast_client,
            whitelist_peers: HashMap::new(),
            greylist_peers: HashMap::new(),
            blacklist_peers: HashMap::new(),
            seed_peers: Vec::new(),
        }
    }

    pub fn get(&self, peer_id: &PeerId) -> Option<&PeerStoreRecord> {
        self.whitelist_peers.get(&peer_id.to_base58()).or_else(|| {
            self.greylist_peers
                .get(&peer_id.to_base58())
                .or_else(|| self.blacklist_peers.get(&peer_id.to_base58()))
        })
    }

    pub fn set_last_ping(&mut self, peer_id: &PeerId, timestamp: EpochTime) {
        if let Some(entry) = self.whitelist_peers.get_mut(&peer_id.to_base58()) {
            let mut new_record = entry.clone();
            new_record.last_ping = Some(timestamp);
            *entry = new_record;
        }
        if let Some(entry) = self.greylist_peers.get_mut(&peer_id.to_base58()) {
            let mut new_record = entry.clone();
            new_record.last_ping = Some(timestamp);
            *entry = new_record;
            // Move it to the whitelist
            let mut record = self.greylist_peers.remove(&peer_id.to_base58()).unwrap();
            record.num_grey_listings = 0;

            self.whitelist_peers.insert(peer_id.to_base58(), record);
        }

        if let Some(entry) = self.blacklist_peers.get_mut(&peer_id.to_base58()) {
            let mut new_record = entry.clone();
            new_record.last_ping = Some(timestamp);
            *entry = new_record;
        }
    }

    pub fn max_known_network_height(&self, algo: PowAlgorithm) -> (u64, u128, Option<PeerId>) {
        let mut max_height = 0;
        let mut max_pow = 0;
        let mut peer_with_highest = None;
        let now = EpochTime::now().as_u64();
        for record in self.whitelist_peers.values() {
            // Only consider peers that we have spoken to.
            if record.last_ping.map(|t| t.as_u64() < now - 60).unwrap_or(true) {
                continue;
            }
            match algo {
                PowAlgorithm::RandomX => {
                    let achieved_pow = record.peer_info.current_random_x_pow;
                    if achieved_pow > max_pow {
                        max_pow = achieved_pow;
                        max_height = record.peer_info.current_random_x_height;
                        peer_with_highest = Some(record.peer_id);
                    }
                },
                PowAlgorithm::Sha3x => {
                    let achieved_pow = record.peer_info.current_sha3x_pow;
                    if achieved_pow > max_pow {
                        max_pow = achieved_pow;
                        max_height = record.peer_info.current_sha3x_height;
                        peer_with_highest = Some(record.peer_id);
                    }
                },
            }
        }

        (max_height, max_pow, peer_with_highest)
    }

    pub fn add_seed_peers(&mut self, mut peer_ids: Vec<PeerId>) {
        self.seed_peers.append(&mut peer_ids);
    }

    pub fn is_seed_peer(&self, peer_id: &PeerId) -> bool {
        self.seed_peers.contains(peer_id)
    }

    pub fn num_catch_ups(&self, peer: &PeerId) -> Option<u64> {
        self.whitelist_peers
            .get(&peer.to_base58())
            .map(|record| record.catch_up_attempts)
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
            self.blacklist_peers.contains_key(&peer_id.to_base58())
    }

    pub fn whitelist_peers(&self) -> &HashMap<String, PeerStoreRecord> {
        &self.whitelist_peers
    }

    pub fn greylist_peers(&self) -> &HashMap<String, PeerStoreRecord> {
        &self.greylist_peers
    }

    pub fn get_known_peers(&self) -> HashSet<PeerId> {
        self.whitelist_peers
            .keys()
            .chain(self.greylist_peers.keys())
            .chain(self.blacklist_peers.keys())
            .filter_map(|peer_id| PeerId::from_str(peer_id).ok())
            .collect()
    }

    pub fn best_peers_to_share(&self, count: usize, other_nodes_peers: &[PeerId]) -> Vec<PeerStoreRecord> {
        let mut peers = self.whitelist_peers.values().collect::<Vec<_>>();
        // ignore all peers records that are older than 10 minutes
        // let timestamp = EpochTime::now().as_u64() - 600;
        // peers.retain(|peer| {
        //     peer.last_new_tip_notify
        //         .as_ref()
        //         .map(|n| n.timestamp)
        //         .unwrap_or(peer.peer_info.timestamp) >
        //         timestamp
        // });
        peers.retain(|peer| !peer.peer_info.public_addresses().is_empty() && peer.last_ping.is_some());
        peers.sort_by_key(|a| a.last_seen());
        peers.reverse();

        peers.retain(|peer| !other_nodes_peers.contains(&peer.peer_id));
        peers.truncate(count);
        peers.into_iter().cloned().collect()
    }

    pub fn reset_last_sync_attempt(&mut self, peer_id: &PeerId) {
        if let Some(entry) = self.whitelist_peers.get_mut(&peer_id.to_base58()) {
            let mut new_record = entry.clone();
            new_record.last_rx_sync_attempt = None;
            new_record.last_sha3x_sync_attempt = None;
            *entry = new_record;
        }
    }

    pub fn update_last_sync_attempt(&mut self, peer_id: PeerId, algo: PowAlgorithm) {
        if let Some(entry) = self.whitelist_peers.get_mut(&peer_id.to_base58()) {
            let mut new_record = entry.clone();
            match algo {
                PowAlgorithm::RandomX => {
                    new_record.last_rx_sync_attempt = Some(Instant::now());
                },
                PowAlgorithm::Sha3x => {
                    new_record.last_sha3x_sync_attempt = Some(Instant::now());
                },
            }
            *entry = new_record;
        }
        if let Some(entry) = self.greylist_peers.get_mut(&peer_id.to_base58()) {
            let mut new_record = entry.clone();
            match algo {
                PowAlgorithm::RandomX => {
                    new_record.last_rx_sync_attempt = Some(Instant::now());
                },
                PowAlgorithm::Sha3x => {
                    new_record.last_sha3x_sync_attempt = Some(Instant::now());
                },
            }
            *entry = new_record;
        }
    }

    /// Add a new peer to store.
    /// If a peer already exists, just replaces it.
    pub async fn add(&mut self, peer_id: PeerId, peer_info: PeerInfo) -> AddPeerStatus {
        // Seed peers are automatically greylisted so that we don't overwhelm them with syncs
        if self.seed_peers.contains(&peer_id) {
            let mut peer_record = PeerStoreRecord::new(peer_id, peer_info.clone());
            peer_record.last_grey_list_reason = Some("Seed peer".to_string());

            self.greylist_peers.insert(peer_id.to_base58(), peer_record);
            return AddPeerStatus::Greylisted;
        }
        if self.blacklist_peers.contains_key(&peer_id.to_base58()) {
            return AddPeerStatus::Blacklisted;
        }

        if let Some(_grey) = self.greylist_peers.get(&peer_id.to_base58()) {
            return AddPeerStatus::Greylisted;
        }

        if let Some(entry) = self.whitelist_peers.get_mut(&peer_id.to_base58()) {
            let previous_record = entry.clone();
            let mut new_record = PeerStoreRecord::new(peer_id, peer_info);
            new_record.catch_up_attempts = previous_record.catch_up_attempts;
            new_record.last_rx_sync_attempt = previous_record.last_rx_sync_attempt;
            new_record.last_sha3x_sync_attempt = previous_record.last_sha3x_sync_attempt;
            new_record.created = entry.created;
            new_record.last_ping = entry.last_ping;
            new_record.num_grey_listings = entry.num_grey_listings;
            new_record.last_grey_list_reason = entry.last_grey_list_reason.clone();

            *entry = new_record;
            // self.whitelist_peers.insert(peer_id, PeerStoreRecord::new(peer_info));
            return AddPeerStatus::Existing;
        }

        self.whitelist_peers
            .insert(peer_id.to_base58(), PeerStoreRecord::new(peer_id, peer_info));
        let _unused = self.stats_broadcast_client.send_new_peer(
            self.whitelist_peers.len() as u64,
            self.greylist_peers.len() as u64,
            self.blacklist_peers.len() as u64,
        );

        // self.peer_removals.insert(peer_id, removal_count).await;
        // }

        // self.set_tip_of_block_heights().await;
        AddPeerStatus::NewPeer
    }

    pub fn clear_grey_list(&mut self) {
        for (peer_id, record) in self.greylist_peers.drain() {
            if record.num_grey_listings >= MAX_GREY_LISTINGS {
                warn!(target: LOG_TARGET, "Blacklisting peer {} because of: {}", peer_id, record.last_grey_list_reason.as_ref().unwrap_or(&"unknown".to_string()));
                self.blacklist_peers.insert(peer_id.clone(), record.clone());
            } else {
                if self.seed_peers.contains(&record.peer_id) {
                    // Don't put seed peers in the whitelist
                    continue;
                }
                self.whitelist_peers.insert(peer_id.clone(), record.clone());
            }
        }
        let _unused = self.stats_broadcast_client.send_new_peer(
            self.whitelist_peers.len() as u64,
            self.greylist_peers.len() as u64,
            self.blacklist_peers.len() as u64,
        );
    }

    pub fn clear_black_list(&mut self) {
        for (peer_id, mut record) in self.blacklist_peers.drain() {
            record.catch_up_attempts = 0;
            record.last_grey_list_reason = None;
            record.num_grey_listings = 0;
            self.whitelist_peers.insert(peer_id, record);
        }
        let _unused = self.stats_broadcast_client.send_new_peer(
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

    pub fn move_to_grey_list(&mut self, peer_id: PeerId, reason: String) {
        if self.whitelist_peers.contains_key(&peer_id.to_base58()) {
            let record = self.whitelist_peers.remove(&peer_id.to_base58());
            if let Some(mut record) = record {
                warn!(target: LOG_TARGET, "Greylisting peer {} because of: {}", peer_id, reason);
                record.last_grey_list_reason = Some(reason.clone());
                record.num_grey_listings += 1;
                self.greylist_peers.insert(peer_id.to_base58(), record);
                let _unused = self.stats_broadcast_client.send_new_peer(
                    self.whitelist_peers.len() as u64,
                    self.greylist_peers.len() as u64,
                    self.blacklist_peers.len() as u64,
                );
            }
        }
    }

    // pub fn is_blacklisted(&self, peer_id: &PeerId) -> bool {
    //     self.blacklist_peers.contains_key(&peer_id.to_base58())
    // }

    // pub fn is_whitelisted(&self, peer_id: &PeerId) -> bool {
    //     if self.whitelist_peers.contains_key(&peer_id.to_base58()) {
    //         return true;
    //     }
    //     if self.whitelist_peers.is_empty() && self.greylist_peers.contains_key(&peer_id.to_base58()) {
    //         return true;
    //     }
    //     false
    // }
}
