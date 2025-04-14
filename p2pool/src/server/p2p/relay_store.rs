//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use libp2p::{Multiaddr, PeerId};
use log::warn;

const LOG_TARGET: &str = "tari::p2pool::relay_store";
#[derive(Debug)]
struct Reservation {
    address: Multiaddr,
    expires_at: Option<Instant>,
    pending: bool,
}

#[derive(Debug, Default)]
pub(crate) struct RelayStore {
    possible_relays: HashMap<PeerId, HashSet<Multiaddr>>,
    active_reservations: HashMap<PeerId, Reservation>,
}

impl RelayStore {
    pub fn add_possible_relay(&mut self, peer: PeerId, address: Multiaddr) -> bool {
        self.possible_relays.entry(peer).or_default().insert(address)
    }

    pub fn confirm_reservation(&mut self, peer: PeerId, expires_at: Instant) {
        if let Some(reservation) = self.active_reservations.get_mut(&peer) {
            reservation.pending = false;
            reservation.expires_at = Some(expires_at);
        } else {
            warn!(target: LOG_TARGET, "No reservation found for peer {}", peer);
        }
    }

    pub fn add_pending_reservation(&mut self, peer: PeerId, address: Multiaddr) {
        if let Some(reservation) = self.active_reservations.get_mut(&peer) {
            reservation.pending = true;
            reservation.address = address;
        } else {
            self.active_reservations.insert(peer, Reservation {
                address,
                expires_at: None,
                pending: true,
            });
        }
    }

    pub fn get_potential_relays(&self) -> Vec<(PeerId, Multiaddr)> {
        self.possible_relays
            .iter()
            .flat_map(|(peer_id, addresses)| addresses.iter().map(|address| (*peer_id, address.clone())))
            .collect()
    }

    pub fn get_expiring_reservations(&self, within_duration: Duration) -> Vec<(PeerId, Multiaddr)> {
        // self.active_reservations
        //     .iter()
        //     .map(|(peer_id, reservation)| (peer_id.clone(), reservation.address.clone()))
        //     .collect()
        self.active_reservations
            .iter()
            .filter_map(|(p, r)| {
                if let Some(expires_at) = r.expires_at {
                    if expires_at < Instant::now() + within_duration {
                        Some((*p, r.address.clone()))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn get_relay_peer_ids(&self) -> Vec<PeerId> {
        // self.possible_relays.keys().copied().collect()
        self.active_reservations
            .iter()
            .filter_map(|(p, r)| {
                if let Some(expires) = r.expires_at {
                    if expires > Instant::now() {
                        Some(*p)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn is_relay(&self, peer_id: &PeerId) -> bool {
        self.possible_relays.contains_key(peer_id)
    }
}
