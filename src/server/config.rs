// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{path::PathBuf, time::Duration};

use crate::server::{p2p, p2p::peer_store::PeerStoreConfig};
use crate::server::http::stats;

/// Config is the server configuration struct.
#[derive(Clone)]
pub struct Config {
    pub base_node_address: String,
    pub p2p_port: u16,
    pub grpc_port: u16,
    pub idle_connection_timeout: Duration,
    pub peer_store: PeerStoreConfig,
    pub p2p_service: p2p::Config,
    pub mining_enabled: bool,
    pub stats_server: stats::server::Config,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_node_address: String::from("http://127.0.0.1:18142"),
            p2p_port: 0,      // bind to any free port
            grpc_port: 18145, // to possibly not collide with any other ports
            idle_connection_timeout: Duration::from_secs(30),
            peer_store: PeerStoreConfig::default(),
            p2p_service: p2p::Config::default(),
            mining_enabled: true,
            stats_server: stats::server::Config::default(),
        }
    }
}

impl Config {
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder {
            config: Config::default(),
        }
    }
}

pub struct ConfigBuilder {
    config: Config,
}

#[allow(dead_code)]
impl ConfigBuilder {
    pub fn with_p2p_port(&mut self, port: u16) -> &mut Self {
        self.config.p2p_port = port;
        self
    }

    pub fn with_grpc_port(&mut self, port: u16) -> &mut Self {
        self.config.grpc_port = port;
        self
    }

    pub fn with_idle_connection_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.config.idle_connection_timeout = timeout;
        self
    }

    pub fn with_peer_store_config(&mut self, config: PeerStoreConfig) -> &mut Self {
        self.config.peer_store = config;
        self
    }

    pub fn with_p2p_service_config(&mut self, config: p2p::Config) -> &mut Self {
        self.config.p2p_service = config;
        self
    }

    pub fn with_seed_peers(&mut self, config: Vec<String>) -> &mut Self {
        self.config.p2p_service.seed_peers = config;
        self
    }

    pub fn with_stable_peer(&mut self, config: bool) -> &mut Self {
        self.config.p2p_service.stable_peer = config;
        self
    }

    pub fn with_private_key_folder(&mut self, config: PathBuf) -> &mut Self {
        self.config.p2p_service.private_key_folder = config;
        self
    }

    pub fn with_mining_enabled(&mut self, config: bool) -> &mut Self {
        self.config.mining_enabled = config;
        self
    }

    pub fn with_mdns_enabled(&mut self, config: bool) -> &mut Self {
        self.config.p2p_service.mdns_enabled = config;
        self
    }

    pub fn with_stats_server_enabled(&mut self, config: bool) -> &mut Self {
        self.config.stats_server.enabled = config;
        self
    }

    pub fn with_stats_server_port(&mut self, config: u16) -> &mut Self {
        self.config.stats_server.port = config;
        self
    }

    pub fn build(&self) -> Config {
        self.config.clone()
    }
}
