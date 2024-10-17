// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

//! P2p module contains all the peer-to-peer related implementations and communications.
//! This module uses hardly `libp2p` to communicate between peers efficiently.

use std::time::Duration;

pub use client::*;
pub use error::*;
pub use network::*;

pub(crate) mod client;
mod error;
mod global_ip;
pub mod messages;
mod network;
pub mod peer_store;
mod relay_store;
pub mod util;
pub const MAX_MISSING_PARENTS_TO_SNOOZE: u64 = 10;
pub const MAX_SNOOZES: usize = 5;
pub const MAX_SNOOZE_DURATION: Duration = Duration::from_secs(2); // 2 seconds, max 10 seconds of snoozing
pub const MIN_PEER_INFO_VERSION: u64 = 5;
pub const MIN_BLOCK_VERSION: u32 = 6;
