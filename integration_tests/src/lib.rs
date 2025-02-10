// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

pub mod p2pool_process;
pub use p2pool_process::P2PoolProcess;
pub mod base_node_process;
pub mod miner;
pub mod world;

use std::{
    net::TcpListener,
    ops::Range,
    path::PathBuf,
    process,
    time::{Duration, Instant},
};

pub use base_node_process::BaseNodeProcess;
use rand::Rng;
pub use world::TariWorld;

pub const THIRTY_SECONDS_WITH_100_MS_SLEEP: u64 = 30 * 10;
pub const HUNDRED_MS: u64 = 100;

type TestResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn get_port(range: Range<u16>, time_out: Duration) -> Option<u16> {
    let min = range.clone().min().expect("A minimum possible port number");
    let max = range.max().expect("A maximum possible port number");

    let start = Instant::now();
    loop {
        let port = rand::thread_rng().gen_range(min..max);

        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Some(port);
        }
        if start.elapsed() > time_out {
            return None;
        }
    }
}

pub fn get_base_dir() -> PathBuf {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_root.join(format!("tests/temp/cucumber_{}", process::id()))
}

// The idea is that if the port is taken it means the service is running.
// If the port is not taken the service hasn't come up yet
pub async fn wait_for_service(port: u16) {
    use tokio::net::TcpStream;

    let max_tries = 4 * 60;
    let mut attempts = 0;

    loop {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)).await {
            drop(stream); // Explicitly drop the connection
            return;
        }

        if attempts >= max_tries {
            panic!("Service on port {} never started", port);
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
        attempts += 1;
    }
}

pub async fn get_p2pool_node_peer_address(world: &TariWorld, peers: &[String]) -> Vec<String> {
    peers
        .iter()
        .filter_map(|peer_string| {
            world
                .p2pool_nodes
                .get(peer_string.as_str())
                .map(|node| node.node_id.to_string())
        })
        .collect()
}

pub async fn get_base_node_peer_addresses(world: &TariWorld, peers: &[String]) -> Vec<String> {
    peers
        .iter()
        .filter_map(|peer_string| {
            world
                .base_nodes
                .get(peer_string.as_str())
                .map(|node| node.identity.to_peer().to_short_string())
        })
        .collect()
}
