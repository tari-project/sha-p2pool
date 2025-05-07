// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command},
    time::{Duration, Instant},
};

use cucumber::codegen::anyhow;
use libp2p::PeerId;
use log::*;
use minotari_app_grpc::{
    authentication::ClientAuthenticationInterceptor,
    tari_rpc::sha_p2_pool_client::ShaP2PoolClient,
};
use reqwest::Client;
use serde_json::Value;
use sha_p2pool::{LibP2pInfo, ShaP2PoolConfig, StartArgs};
use tari_common::{configuration::Network, network_check::set_network_if_choice_valid, MAX_GRPC_MESSAGE_SIZE};
use tari_core::proof_of_work::Difficulty;
use tonic::{codegen::InterceptedService, transport::Channel as TonicChannel};

use crate::{get_port, wait_for_service, TariWorld, TestResult};

pub const LOG_TARGET: &str = "cucumber::p2pool_process";
pub const LIBP2P_INFO_FILE: &str = "libp2p_info.json";

pub type ShaP2PoolGrpcClient = ShaP2PoolClient<InterceptedService<TonicChannel, ClientAuthenticationInterceptor>>;

#[derive(Debug)]
pub struct P2PoolProcess {
    pub name: String,
    pub temp_dir_path: PathBuf,
    pub is_seed_node: bool,
    pub diagnostic_mode: bool,
    pub config: ShaP2PoolConfig,
    pub node_id: PeerId,
    pub squad: String,
    pub running_instance: Option<Child>,
    pub grpc_port: u16,
    pub p2p_port: u16,
    pub connected_base_node: String,
}

impl Drop for P2PoolProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

#[allow(clippy::too_many_lines)]
pub async fn spawn_p2pool_node_and_wait_for_start(
    world: &mut TariWorld,
    is_seed_node: bool,
    diagnostic_mode: bool,
    p2pool_name: String,
    squad: String,
    connected_base_node: String,
) -> TestResult<()> {
    let mut node_config = ShaP2PoolConfig::default();
    std::env::set_var("TARI_NETWORK", "localnet");
    set_network_if_choice_valid(Network::LocalNet)?;
    let is_seed_node = if diagnostic_mode { false } else { is_seed_node };

    let temp_dir_path: PathBuf;

    if let Some(node_ps) = world.p2pool_nodes.get(&p2pool_name) {
        temp_dir_path = node_ps.temp_dir_path.clone();
        node_config = node_ps.config.clone();
    } else {
        node_config.p2p_service.squad_prefix = squad.clone();
        node_config.p2p_service.squad_override = Some(squad.clone());
        node_config.p2p_service.is_seed_peer = is_seed_node;
        node_config.p2p_service.peer_info_publish_interval = Duration::from_secs(1);
        node_config.p2p_service.peer_exchange_interval = Duration::from_secs(1);
        node_config.p2p_service.meta_data_exchange_interval = Duration::from_secs(1);
        node_config.p2p_service.diagnostic_mode = diagnostic_mode;
        if diagnostic_mode {
            node_config.p2p_service.randomx_enabled = false;
            node_config.p2p_service.sha3x_enabled = false;
            node_config.network_silence_delay = u64::from(u16::MAX);
        } else {
            node_config.p2p_service.randomx_enabled = true;
            node_config.p2p_service.sha3x_enabled = true;
            node_config.network_silence_delay = 0;
        }
        // Each spawned p2pool node will use different ports
        node_config.p2p_port = get_port(world, 18000..18499, Duration::from_secs(20)).ok_or("p2p_port no free port")?;
        node_config.grpc_port =
            get_port(world, 18500..18999, Duration::from_secs(20)).ok_or("grpc_port no free port")?;
        if is_seed_node {
            node_config.http_server.enabled = false;
        } else {
            node_config.http_server.port =
                get_port(world, 19000..19499, Duration::from_secs(20)).ok_or("http_server_port no free port")?;
            node_config.http_server.enabled = true;
        }
        // The format for this addrress can be either "/ip4/127.0.0.1/tcp/{}" or "/ip4/127.0.0.1/udp/{}/quic-v1"
        node_config.p2p_service.external_addr = Some(format!("/ip4/127.0.0.1/udp/{}/quic-v1", node_config.p2p_port));
        // Create a new temporary directory
        temp_dir_path = world
            .current_base_dir
            .as_ref()
            .expect("p2pool dir on world")
            .join("p2pool_nodes")
            .join(format!("{}_grpc_port_{}", p2pool_name.clone(), node_config.grpc_port));
        if let Err(err) = fs::create_dir_all(&temp_dir_path) {
            return Err(format!(
                "Failed to create temp_dir_path at: '{}', error: {}",
                temp_dir_path.display(),
                err
            )
            .into());
        }
        if let Err(err) = fs::create_dir_all(temp_dir_path.join("configs")) {
            return Err(format!(
                "Failed to create configs dir at: '{}', error: {}",
                temp_dir_path.join("configs").display(),
                err
            )
            .into());
        }
    };

    let msg = format!(
        "Initializing p2pool node: '{}', p2p_port; '{}', grpc_port: '{}', http_server.port: '{}', is_seed_node: '{}', \
         base_dir: '{}'",
        p2pool_name,
        node_config.p2p_port,
        node_config.grpc_port,
        node_config.http_server.port,
        is_seed_node,
        temp_dir_path.display()
    );
    debug!(target: LOG_TARGET, "{}", msg);
    println!("{}", msg);

    let base_node_address = if let Some(base_node_process) = world.base_nodes.get(&connected_base_node) {
        format!("http://127.0.0.1:{}", base_node_process.grpc_port)
    } else if let Some(base_node_process) = world.base_nodes.get_index(0) {
        format!("http://127.0.0.1:{}", base_node_process.1.grpc_port)
    } else {
        let msg = format!(
            "No base node found for p2pool node '{}' to connect to; at least one base node must be spawned before any \
             p2pool nodes can be spawned",
            p2pool_name
        );
        debug!(target: LOG_TARGET, "{}", msg);
        return Err(msg.to_string().into());
    };

    // Find seed peers in world
    // - /ip4/127.0.0.1/tcp/<p2p_port>
    let seed_peers = world
        .p2pool_nodes
        .iter()
        .filter(|(_, process)| process.is_seed_node && process.squad == squad)
        .map(|(_, value)| format!("/ip4/127.0.0.1/tcp/{}/p2p/{}", value.p2p_port, value.node_id))
        .collect::<Vec<String>>();
    for seed_peer in &seed_peers {
        debug!(target: LOG_TARGET, "'{}' has seed peer: '{}'", p2pool_name, seed_peer);
    }

    let args = StartArgs {
        base_dir: Some(temp_dir_path.clone()),
        grpc_port: Some(node_config.grpc_port),
        p2p_port: Some(node_config.p2p_port),
        stats_server_port: Some(node_config.http_server.port),
        external_address: node_config.p2p_service.external_addr.clone(),
        base_node_address,
        seed_peers: if seed_peers.is_empty() { None } else { Some(seed_peers) },
        no_default_seed_peers: true,
        stable_peer: true,
        squad_prefix: node_config.p2p_service.squad_prefix.clone(),
        squad_override: node_config.p2p_service.squad_override.clone(),
        num_squads: 1,
        private_key_folder: PathBuf::default(),
        is_seed_peer: node_config.p2p_service.is_seed_peer,
        mdns_disabled: false,
        relay_server_disabled: true,
        relay_server_max_circuits: None,
        relay_server_max_circuits_per_peer: None,
        http_server_disabled: !node_config.http_server.enabled,
        user_agent: None,
        peer_publish_interval: Some(node_config.p2p_service.peer_info_publish_interval.as_secs()),
        debug_print_chain: false,
        diagnostic_mode: node_config.p2p_service.diagnostic_mode,
        diagnostic_mode_file_path: None,
        max_connections: None,
        randomx_disabled: !node_config.p2p_service.randomx_enabled,
        sha3x_disabled: !node_config.p2p_service.sha3x_enabled,
        block_time_sha: Some(1),
        block_time_rx: Some(1),
        share_window: Some(100),
        export_libp2p_info: Some(temp_dir_path.join(LIBP2P_INFO_FILE).clone()),
        network_silence_delay: {
            // Note: Any value above u16::MAX will be set to u16::MAX
            let bytes = node_config.network_silence_delay.to_le_bytes();
            Some(u16::from_le_bytes([bytes[0], bytes[1]]))
        },
        minimum_sha3_target_difficulty: Some(Difficulty::min().as_u64()),
        minimum_randomx_target_difficulty: Some(Difficulty::min().as_u64()),
        grpc_cache_seconds: None,
    };

    let name_cloned = p2pool_name.clone();
    let temp_dir_path_clone = temp_dir_path.clone();
    let running_instance = {
        let mut command = Command::new(get_p2pool_exe_path());
        command.current_dir(temp_dir_path_clone.clone());
        command.env("TARI_NETWORK", "localnet");
        command.arg("start");
        for arg in to_args_command_line(args) {
            command.arg(arg);
        }
        match command.spawn() {
            Ok(child) => child,
            Err(err) => return Err(format!("Failed to start p2pool node '{}': {}", name_cloned, err).into()),
        }
    };

    let libp2p_info = LibP2pInfo::read_from_file(&temp_dir_path.join(LIBP2P_INFO_FILE), Duration::from_secs(30))?;

    let process = P2PoolProcess {
        name: p2pool_name.clone(),
        temp_dir_path: temp_dir_path.clone(),
        is_seed_node,
        diagnostic_mode,
        config: node_config.clone(),
        node_id: libp2p_info.peer_id,
        squad: libp2p_info.squad,
        running_instance: Some(running_instance),
        grpc_port: node_config.grpc_port,
        p2p_port: node_config.p2p_port,
        connected_base_node,
    };
    debug!(target: LOG_TARGET, "Initialized: {:?}", process);

    world.p2pool_nodes.insert(p2pool_name, process);
    if !is_seed_node {
        wait_for_service(node_config.p2p_port).await;
        wait_for_service(node_config.grpc_port).await;
    }

    Ok(())
}

impl P2PoolProcess {
    pub async fn get_grpc_client(&self) -> anyhow::Result<ShaP2PoolClient<TonicChannel>> {
        let dst = format!("http://127.0.0.1:{}", self.config.grpc_port);
        debug!(target: LOG_TARGET, "get_grpc_client: trying to connect to '{}'", dst);
        Ok(ShaP2PoolClient::connect(dst)
            .await?
            .max_decoding_message_size(MAX_GRPC_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_GRPC_MESSAGE_SIZE))
    }

    pub fn kill(&mut self) {
        if let Some(child) = &mut self.running_instance {
            if let Err(e) = child.kill() {
                println!(
                    "Failed to kill p2pool node: '{}', process id: {}, path: '{}', error: '{}'",
                    self.name.clone(),
                    child.id(),
                    get_p2pool_exe_path().display(),
                    e
                );
            }
        }

        // Wait till the ports are cleared
        let mut p2p_port_closed = false;
        let mut grpc_port_closed = false;
        let mut attempts = 0;
        loop {
            if !p2p_port_closed && TcpListener::bind(("127.0.0.1", self.config.p2p_port)).is_ok() {
                p2p_port_closed = true;
            }
            if !grpc_port_closed && TcpListener::bind(("127.0.0.1", self.config.grpc_port)).is_ok() {
                grpc_port_closed = true;
            }
            attempts += 1;
            std::thread::sleep(std::time::Duration::from_millis(250));
            if attempts >= 40 {
                break;
            }
        }
    }
}

pub async fn shut_down_node(world: &mut TariWorld, p2pool_name: String) -> TestResult<()> {
    debug!(target: LOG_TARGET, "shut down p2pool node '{}'", p2pool_name);

    let p2pool_process: &mut P2PoolProcess = world.get_p2pool_node(&p2pool_name)?;
    p2pool_process.kill();

    Ok(())
}

pub async fn restart_node(world: &mut TariWorld, p2pool_name: String) -> TestResult<()> {
    debug!(target: LOG_TARGET, "re-start p2pool node '{}'", p2pool_name);

    let p2pool_process = world.get_p2pool_node(&p2pool_name)?;
    let is_seed_node = p2pool_process.is_seed_node;
    let diagnostic_mode = p2pool_process.diagnostic_mode;
    let name = p2pool_process.name.clone();
    let squad = p2pool_process.squad.clone();
    let connected_base_node = p2pool_process.connected_base_node.clone();

    spawn_p2pool_node_and_wait_for_start(
        world,
        is_seed_node,
        diagnostic_mode,
        name.clone(),
        squad.clone(),
        connected_base_node.clone(),
    )
    .await
}

#[allow(clippy::too_many_lines)]
pub fn to_args_command_line(args: StartArgs) -> Vec<String> {
    let mut args_vec = Vec::new();

    if let Some(base_dir) = args.base_dir {
        args_vec.push(format!("--base-dir={}", base_dir.display()));
    }

    if let Some(grpc_port) = args.grpc_port {
        args_vec.push(format!("--grpc-port={}", grpc_port));
    }

    if let Some(p2p_port) = args.p2p_port {
        args_vec.push(format!("--p2p-port={}", p2p_port));
    }

    if let Some(stats_server_port) = args.stats_server_port {
        args_vec.push(format!("--stats-server-port={}", stats_server_port));
    }

    if let Some(external_address) = args.external_address {
        args_vec.push(format!("--external-address={}", external_address));
    }

    args_vec.push(format!("--base-node-address={}", args.base_node_address));

    if let Some(seed_peers) = args.seed_peers {
        args_vec.push(format!("--seed-peers={}", seed_peers.join(",")));
    }

    if args.no_default_seed_peers {
        args_vec.push("--no-default-seed-peers".to_string());
    }

    if args.stable_peer {
        args_vec.push("--stable-peer".to_string());
    }

    args_vec.push(format!("--squad-prefix={}", args.squad_prefix));

    if let Some(squad_override) = args.squad_override {
        args_vec.push(format!("--squad-override={}", squad_override));
    }

    args_vec.push(format!("--num-squads={}", args.num_squads));

    if args.private_key_folder != PathBuf::default() {
        args_vec.push(format!("--private-key-folder={}", args.private_key_folder.display()));
    }

    if args.is_seed_peer {
        args_vec.push("--is-seed-peer".to_string());
    }

    if args.mdns_disabled {
        args_vec.push("--mdns-disabled".to_string());
    }

    if args.relay_server_disabled {
        args_vec.push("--relay-server-disabled".to_string());
    }

    if let Some(relay_server_max_circuits) = args.relay_server_max_circuits {
        args_vec.push(format!("--relay-server-max-circuits={}", relay_server_max_circuits));
    }

    if let Some(relay_server_max_circuits_per_peer) = args.relay_server_max_circuits_per_peer {
        args_vec.push(format!(
            "--relay-server-max-circuits-per-peer={}",
            relay_server_max_circuits_per_peer
        ));
    }

    if args.http_server_disabled {
        args_vec.push("--http-server-disabled".to_string());
    }

    if let Some(user_agent) = args.user_agent {
        args_vec.push(format!("--user-agent={}", user_agent));
    }

    if let Some(peer_publish_interval) = args.peer_publish_interval {
        args_vec.push(format!("--peer-publish-interval={}", peer_publish_interval));
    }

    if args.debug_print_chain {
        args_vec.push("--debug-print-chain".to_string());
    }

    if args.diagnostic_mode {
        args_vec.push("--diagnostic-mode".to_string());
    }

    if args.diagnostic_mode_file_path.is_some() {
        args_vec.push(format!(
            "--diagnostic-mode-file-path={}",
            args.diagnostic_mode_file_path.unwrap().display()
        ));
    }

    if let Some(max_connections) = args.max_connections {
        args_vec.push(format!("--max-connections={}", max_connections));
    }

    if args.randomx_disabled {
        args_vec.push("--randomx-disabled".to_string());
    }

    if args.sha3x_disabled {
        args_vec.push("--sha3x-disabled".to_string());
    }

    if let Some(block_time) = args.block_time_sha {
        args_vec.push(format!("--block-time-sha={}", block_time));
    }

    if let Some(block_time) = args.block_time_rx {
        args_vec.push(format!("--block-time-rx={}", block_time));
    }

    if let Some(share_window) = args.share_window {
        args_vec.push(format!("--share-window={}", share_window));
    }

    if let Some(path) = args.export_libp2p_info {
        args_vec.push(format!("--export-libp2p-info={}", path.display()));
    }

    if let Some(delay) = args.network_silence_delay {
        args_vec.push(format!("--network-silence-delay={}", delay));
    }

    if let Some(difficulty) = args.minimum_sha3_target_difficulty {
        args_vec.push(format!("--minimum-sha3-target-difficulty={}", difficulty));
    }

    if let Some(difficulty) = args.minimum_randomx_target_difficulty {
        args_vec.push(format!("--minimum-randomx-target-difficulty={}", difficulty));
    }

    args_vec
}

pub fn get_p2pool_exe_path() -> PathBuf {
    #[cfg(windows)]
    {
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        crate_root.join("../target/release/sha_p2pool.exe")
    }
    #[cfg(not(windows))]
    {
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        crate_root.join("../target/release/sha_p2pool")
    }
}

pub async fn verify_diagnostic_file_created(
    world: &mut TariWorld,
    p2pool_name: String,
    seconds: u64,
) -> TestResult<()> {
    debug!(target: LOG_TARGET, "verify '{}' creates a diagnostic file", p2pool_name);

    let p2pool_process = world.get_p2pool_node(&p2pool_name)?;
    let diagnostic_file_path = p2pool_process.temp_dir_path.join("diagnostic_results.json");
    let start = Instant::now();
    while !diagnostic_file_path.exists() && start.elapsed() < Duration::from_secs(seconds) {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    if !diagnostic_file_path.exists() {
        return Err(format!("Diagnostic file not found at: '{}'", diagnostic_file_path.display()).into());
    }

    Ok(())
}

pub async fn verify_peer_connected(world: &mut TariWorld, p2pool_name: String, peer_name: String) -> TestResult<()> {
    debug!(target: LOG_TARGET, "verify '{}' is connected to peer '{}'", p2pool_name, peer_name);
    let start = Instant::now();

    let p2pool_process = world.get_p2pool_node(&p2pool_name)?;
    if !p2pool_process.config.http_server.enabled {
        return Err(format!("p2pool node '{}' doesn't have the http server enabled", p2pool_name).into());
    }
    let connections_url = format!(
        "http://127.0.0.1:{}/connections",
        p2pool_process.config.http_server.port
    );
    let p2pool_client = Client::new();

    let peer_process = world.get_p2pool_node(&peer_name)?;

    let mut counter = 0;
    while start.elapsed() < Duration::from_secs(360) {
        let response = p2pool_client.get(connections_url.clone()).send().await?;
        if response.status().is_success() {
            let response_json: Value = response.json().await?;
            if let Some(peers) = response_json.get("peers") {
                if let Some(peers_array) = peers.as_array() {
                    if peers_array
                        .iter()
                        .any(|peer| peer["peer_id"] == peer_process.node_id.to_base58())
                    {
                        debug!(
                            target: LOG_TARGET,
                            "Node '{}' is connected to peer '{}' (waited {:.2?})",
                            p2pool_name, peer_name, start.elapsed()
                        );
                        return Ok(());
                    }
                } else {
                    return Err(format!(
                        "Unexpected response to {}, 'peers' array not found: {}",
                        connections_url, peers
                    )
                    .into());
                }
            } else {
                return Err(format!(
                    "Unexpected response to {}, 'peers' key not found: {}",
                    connections_url, response_json
                )
                .into());
            }
        } else {
            return Err(format!(
                "Failed to query {} for connections: {}",
                connections_url,
                response.status()
            )
            .into());
        }
        if counter % 50 == 0 {
            debug!(
                target: LOG_TARGET,
                "Iteration {}: waiting {:.2?} for '{}' to show peer connected",
                counter, start.elapsed(), connections_url
            );
        }
        counter += 1;

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let msg = format!("Peer '{}' is NOT connected to '{}'", peer_name, p2pool_name);
    error!(target: LOG_TARGET, "{}", msg);
    Err(msg.to_string().into())
}
