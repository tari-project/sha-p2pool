// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

//! P2p module contains all the peer-to-peer related implementations and communications.
//! This module uses hardly `libp2p` to communicate between peers efficiently.

use std::time::Duration;

pub use error::*;
pub use network::*;

pub mod client;
mod error;
mod global_ip;
pub mod messages;
mod network;
pub mod peer_store;
mod relay_store;
pub mod util;
