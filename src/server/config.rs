// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{path::PathBuf, time::Duration};

use libp2p::identity::Keypair;

use crate::server::{http, p2p};

/// Config is the server configuration struct.
#[derive(Clone)]
pub struct Config {
    pub base_node_address: String,
    pub p2p_port: u16,
    pub grpc_port: u16,
    pub idle_connection_timeout: Duration,
    pub p2p_service: p2p::Config,
    pub http_server: http::server::Config,
    pub max_incoming_connections: Option<u32>,
    pub max_outgoing_connections: Option<u32>,
    pub network_silence_delay: u64,
    pub max_relay_circuits: Option<usize>,
    pub max_relay_circuits_per_peer: Option<usize>,
    pub block_time: u64,
    pub share_window: u64,
    pub block_cache_file: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_node_address: String::from("http://127.0.0.1:18182"),
            p2p_port: 0,      // bind to any free port
            grpc_port: 18145, // to possibly not collide with any other ports
            idle_connection_timeout: Duration::from_secs(60),
            p2p_service: p2p::Config::default(),
            http_server: http::server::Config::default(),
            max_incoming_connections: Some(100),
            max_outgoing_connections: Some(20),
            network_silence_delay: 300,
            max_relay_circuits: None,
            max_relay_circuits_per_peer: None,
            block_time: 20,
            share_window: 2160,
            block_cache_file: PathBuf::from("block_cache"),
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

    pub fn with_squad_prefix(&mut self, squad: String) -> &mut Self {
        self.config.p2p_service.squad_prefix = squad;
        self
    }

    pub fn with_num_squads(&mut self, num_squads: usize) -> &mut Self {
        self.config.p2p_service.num_squads = num_squads;
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

    pub fn with_max_incoming_connections(&mut self, config: u32) -> &mut Self {
        self.config.max_incoming_connections = Some(config);
        self
    }

    pub fn with_max_outgoing_connections(&mut self, config: u32) -> &mut Self {
        self.config.max_outgoing_connections = Some(config);
        self
    }

    pub fn with_sha3x_enabled(&mut self, config: bool) -> &mut Self {
        self.config.p2p_service.sha3x_enabled = config;
        self
    }

    pub fn with_randomx_enabled(&mut self, config: bool) -> &mut Self {
        self.config.p2p_service.randomx_enabled = config;
        self
    }

    pub fn with_private_key(&mut self, config: Option<Keypair>) -> &mut Self {
        self.config.p2p_service.private_key = config;
        self
    }

    pub fn with_is_seed_peer(&mut self, config: bool) -> &mut Self {
        self.config.p2p_service.is_seed_peer = config;
        self
    }

    pub fn with_mdns_enabled(&mut self, config: bool) -> &mut Self {
        self.config.p2p_service.mdns_enabled = config;
        self
    }

    pub fn with_relay_disabled(&mut self, config: bool) -> &mut Self {
        self.config.p2p_service.relay_server_disabled = config;
        self
    }

    pub fn with_relay_max_circuits(&mut self, config: Option<usize>) -> &mut Self {
        self.config.max_relay_circuits = config;
        self
    }

    pub fn with_relay_max_circuits_per_peer(&mut self, config: Option<usize>) -> &mut Self {
        self.config.max_relay_circuits_per_peer = config;
        self
    }

    pub fn with_http_server_enabled(&mut self, config: bool) -> &mut Self {
        self.config.http_server.enabled = config;
        self
    }

    pub fn with_external_address(&mut self, config: String) -> &mut Self {
        self.config.p2p_service.external_addr = Some(config);
        self
    }

    pub fn with_stats_server_port(&mut self, config: u16) -> &mut Self {
        self.config.http_server.port = config;
        self
    }

    pub fn with_base_node_address(&mut self, config: String) -> &mut Self {
        self.config.base_node_address = config;
        self
    }

    pub fn with_user_agent(&mut self, config: String) -> &mut Self {
        self.config.p2p_service.user_agent = config;
        self
    }

    pub fn with_peer_publish_interval(&mut self, config: Option<u64>) -> &mut Self {
        if let Some(interval) = config {
            self.config.p2p_service.peer_info_publish_interval = Duration::from_secs(interval);
        }
        self
    }

    pub fn with_debug_print_chain(&mut self, config: bool) -> &mut Self {
        self.config.p2p_service.debug_print_chain = config;
        self
    }

    pub fn with_block_time(&mut self, config: u64) -> &mut Self {
        self.config.block_time = config;
        self
    }

    pub fn with_share_window(&mut self, config: u64) -> &mut Self {
        self.config.share_window = config;
        self
    }

    pub fn with_block_cache_file(&mut self, config: PathBuf) -> &mut Self {
        self.config.block_cache_file = config;
        self
    }

    pub fn build(&self) -> Config {
        self.config.clone()
    }
}
