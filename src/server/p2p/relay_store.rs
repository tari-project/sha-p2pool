//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::collections::{HashMap, HashSet};

use libp2p::{Multiaddr, PeerId};
use rand::seq::IteratorRandom;

#[derive(Debug, Clone, Default)]
pub struct RelayStore {
    selected_relay: Option<RelayPeer>,
    possible_relays: HashMap<PeerId, HashSet<Multiaddr>>,
}

impl RelayStore {
    pub fn selected_relay_mut(&mut self) -> Option<&mut RelayPeer> {
        self.selected_relay.as_mut()
    }

    pub fn has_active_relay(&self) -> bool {
        self.selected_relay
            .as_ref()
            .map(|r| r.is_circuit_established)
            .unwrap_or(false)
    }

    pub fn add_possible_relay(&mut self, peer: PeerId, address: Multiaddr) -> bool {
        self.possible_relays.entry(peer).or_default().insert(address)
    }

    pub fn select_random_relay(&mut self) {
        let Some((peer, addrs)) = self.possible_relays.iter().choose(&mut rand::thread_rng()) else {
            return;
        };
        self.selected_relay = Some(RelayPeer {
            peer_id: *peer,
            addresses: addrs.iter().cloned().collect(),
            is_circuit_established: false,
        });
    }
}

#[derive(Debug, Clone)]
pub struct RelayPeer {
    pub peer_id: PeerId,
    pub addresses: Vec<Multiaddr>,
    pub is_circuit_established: bool,
}
