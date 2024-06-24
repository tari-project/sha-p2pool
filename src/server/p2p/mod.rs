//! P2p module contains all the peer-to-peer related implementations and communications.
//! This module uses hardly `libp2p` to communicate between peers efficiently.

pub use client::*;
pub use error::*;
pub use p2p::*;

mod p2p;
mod error;
pub mod messages;
pub mod peer_store;
pub(crate) mod client;

