//! P2p module contains all the peer-to-peer related implementations and communications.
//! This module uses hardly `libp2p` to communicate between peers efficiently.

pub use client::*;
pub use error::*;
pub use p2p::*;

pub(crate) mod client;
mod error;
pub mod messages;
mod p2p;
pub mod peer_store;
