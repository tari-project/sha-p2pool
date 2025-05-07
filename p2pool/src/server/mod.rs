// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

mod config;
use std::{fmt, fmt::Display};

pub use config::*;

#[allow(clippy::module_inception)]
pub mod server;

pub mod diagnostics;

pub mod grpc;
pub mod http;
pub mod p2p;

pub const PROTOCOL_VERSION: u64 = 37;

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum PeerType {
    SeedPeer,
    RelayPeer,
    PrivatePeer,
    NonSquadPeer,
    Unknown,
}

impl Display for PeerType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeerType::SeedPeer => write!(f, "SeedPeer"),
            PeerType::RelayPeer => write!(f, "RelayPeer"),
            PeerType::PrivatePeer => write!(f, "PrivatePeer"),
            PeerType::NonSquadPeer => write!(f, "NonSquadPeer"),
            PeerType::Unknown => write!(f, "UnknownPeerType"),
        }
    }
}
