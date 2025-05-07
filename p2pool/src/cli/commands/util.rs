// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{
    cmp::max,
    collections::HashMap,
    env,
    fs,
    fs::File,
    io,
    io::{BufReader, BufWriter},
    path::Path,
    sync::Arc,
    thread::sleep,
    time::{Duration, Instant},
};

use libp2p::{identity::Keypair, PeerId};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use tari_common::{configuration::Network, initialize_logging};
use tari_core::{
    consensus::ConsensusManager,
    proof_of_work::{randomx_factory::RandomXFactory, PowAlgorithm},
};
use tari_shutdown::Shutdown;
use tari_utilities::hex::Hex;
use tokio::sync::RwLock;

use crate::{
    cli::args::{Cli, StartArgs},
    server::{
        self as main_server,
        diagnostics::{DiagnosticsBroadcastClient, DiagnosticsCollector},
        http::stats_collector::{StatsBroadcastClient, StatsCollector},
        server::Server,
    },
    sharechain::{in_memory::InMemoryShareChain, p2block::VerifiedStatus, BlockValidationParams},
};

const LOG_TARGET: &str = "tari::p2pool::server::p2p";

#[allow(clippy::too_many_lines)]
pub async fn server(
    cli: Arc<Cli>,
    args: &StartArgs,
    shutdown: Shutdown,
    enable_logging: bool,
) -> anyhow::Result<Server<InMemoryShareChain>> {
    if enable_logging {
        let _unused = fs::remove_file(cli.base_dir().join("configs/logs.yml"));
        // logger setup
        if let Err(e) = initialize_logging(
            &cli.base_dir().join("configs/logs.yml"),
            &cli.base_dir(),
            if std::env::var("PERFORM_DETAIL_LOGGING").is_ok() {
                include_str!("../../../../log4rs_detailed.yml")
            } else {
                include_str!("../../../../log4rs_sample.yml")
            },
        ) {
            eprintln!("{}", e);
            return Err(e.into());
        }
    }
    info!(target: LOG_TARGET, "{:?}", &cli);

    let mut config_builder = main_server::Config::builder();
    if let Some(grpc_port) = args.grpc_port {
        config_builder.with_grpc_port(grpc_port);
    }
    if let Some(p2p_port) = args.p2p_port {
        config_builder.with_p2p_port(p2p_port);
    }

    if let Some(block_time) = args.block_time_sha {
        config_builder.with_sha3_block_time(block_time);
    }

    if let Some(block_time) = args.block_time_rx {
        config_builder.with_rx_block_time(block_time);
    }
    
    if let Some(num_concurrent_syncs) = args.num_concurrent_syncs{
        config_builder.with_num_concurrent_syncs(num_concurrent_syncs as usize);
    }

    if let Some(share_window) = args.share_window {
        config_builder.with_share_window(share_window);
    }

    config_builder.with_squad_prefix(args.squad_prefix.clone());
    config_builder.with_num_squads(args.num_squads);
    if let Some(squad_override) = args.squad_override.clone() {
        config_builder.with_squad_override(squad_override);
    }

    if let Some(delay) = args.network_silence_delay {
        config_builder.with_network_silence_delay(delay.into());
    }

    if let Some(difficulty) = args.minimum_sha3_target_difficulty {
        config_builder.with_minimum_sha3_target_difficulty(difficulty);
    }

    if let Some(difficulty) = args.minimum_randomx_target_difficulty {
        config_builder.with_minimum_randomx_target_difficulty(difficulty);
    }

    if let Some(grpc_cache_time) = args.grpc_cache_seconds {
        config_builder.with_grpc_cache_time(Duration::from_secs(grpc_cache_time));
    }

    // set default tari network specific seed peer address
    let mut seed_peers = vec![];
    let network = Network::get_current_or_user_setting_or_default();
    if network != Network::LocalNet && !args.no_default_seed_peers {
        let default_seed_peer = format!("/dnsaddr/{}.sha-p2pool.tari.com", network.as_key_str());
        seed_peers.push(default_seed_peer);
    }
    if let Some(cli_seed_peers) = args.seed_peers.clone() {
        let cli_seed_peers: Vec<String> = cli_seed_peers
            .iter()
            .flat_map(|s| s.split(',').map(|s| s.trim().to_string()).collect::<Vec<_>>())
            .collect();
        seed_peers.extend(cli_seed_peers);
    }
    config_builder.with_seed_peers(seed_peers);

    config_builder.with_stable_peer(args.stable_peer);
    config_builder.with_private_key_folder(args.private_key_folder.clone());
    if let Some(max_connections) = args.max_connections {
        config_builder.with_max_outgoing_connections(max_connections);
        config_builder.with_max_incoming_connections(max_connections);
    }
    config_builder.with_randomx_enabled(!args.randomx_disabled);
    config_builder.with_sha3x_enabled(!args.sha3x_disabled);

    // try to extract env var based private key
    if let Ok(identity_cbor) = env::var("SHA_P2POOL_IDENTITY") {
        let identity_raw = hex::decode(identity_cbor.as_bytes())?;
        let private_key = Keypair::from_protobuf_encoding(identity_raw.as_slice())?;
        config_builder.with_private_key(Some(private_key));
    }

    // external address
    if let Some(external_addr) = &args.external_address {
        config_builder.with_external_address(external_addr.clone());
    }
    if let Ok(external_ip) = env::var("SHA_P2POOL_ADDRESS") {
        config_builder.with_external_address(external_ip);
    }

    config_builder.with_is_seed_peer(args.is_seed_peer);
    config_builder.with_mdns_enabled(!args.mdns_disabled);
    config_builder.with_relay_disabled(args.relay_server_disabled);
    config_builder.with_relay_max_circuits(args.relay_server_max_circuits);
    config_builder.with_relay_max_circuits_per_peer(args.relay_server_max_circuits_per_peer);
    config_builder.with_http_server_enabled(!args.http_server_disabled);
    config_builder.with_user_agent(
        args.user_agent
            .as_ref()
            .cloned()
            .unwrap_or_else(|| "tari-p2pool".to_string()),
    );
    config_builder.with_peer_publish_interval(args.peer_publish_interval);
    config_builder.with_debug_print_chain(args.debug_print_chain);
    if let Some(stats_server_port) = args.stats_server_port {
        config_builder.with_stats_server_port(stats_server_port);
    }
    config_builder.with_base_node_address(args.base_node_address.clone());

    config_builder.with_block_cache_file(env::current_dir()?.join("block_cache"));

    config_builder.with_diagnostic_mode(args.diagnostic_mode);
    if let Some(path) = &args.diagnostic_mode_file_path {
        config_builder.with_diagnostic_mode_file_path(path.clone());
    }

    let config = config_builder.build();

    let randomx_factory = RandomXFactory::new(1);
    let consensus_manager = ConsensusManager::builder(Network::get_current_or_user_setting_or_default()).build()?;
    let genesis_block_hash = *consensus_manager.get_genesis_block().hash();

    info!(
        target: "p2pool::server", "Consensus manager initialized with network: {}, and genesis hash {}",
        Network::get_current_or_user_setting_or_default(),
        genesis_block_hash.to_hex()
    );
    let block_validation_params = Arc::new(BlockValidationParams::new(
        randomx_factory,
        consensus_manager.clone(),
        genesis_block_hash,
    ));
    let coinbase_extras_sha3x = Arc::new(RwLock::new(HashMap::<String, Vec<u8>>::new()));

    let swarm = crate::server::p2p::setup::new_swarm(&config).await?;
    let squad = config.p2p_service.squad_override.clone().unwrap_or_else(|| {
        let squad_id =
            (*swarm.local_peer_id().to_bytes().last().unwrap_or(&0) as usize) % max(1, config.p2p_service.num_squads);
        format!("{}_{}", config.p2p_service.squad_prefix.clone(), squad_id)
    });
    info!(target: LOG_TARGET, "Swarm created. Our id: {}, our squad:{}", swarm.local_peer_id(), squad);

    let (stats_tx, stats_rx) = tokio::sync::broadcast::channel(1000);
    let stats_broadcast_client = StatsBroadcastClient::new(stats_tx);
    let stats_collector = StatsCollector::new(shutdown.clone(), stats_rx);

    let (diagnostics_collector, diagnostics_broadcast_client, diagnostics_receiver_client) = if args.diagnostic_mode {
        let (diagnostics_tx, diagnostics_rx) = tokio::sync::broadcast::channel(1000);
        let broadcast_client = DiagnosticsBroadcastClient::new(diagnostics_tx);
        let collector = DiagnosticsCollector::new(shutdown.clone(), diagnostics_rx);
        let receiver_client = collector.create_receiver_client();
        (Some(collector), Some(broadcast_client), Some(receiver_client))
    } else {
        (None, None, None)
    };

    if let Some(path) = args.export_libp2p_info.clone() {
        let libp2p_info = LibP2pInfo {
            peer_id: *swarm.local_peer_id(),
            squad: squad.clone(),
        };
        if let Err(err) = libp2p_info.save_to_file(&path) {
            warn!(target: LOG_TARGET, "Failed to save libp2p info to file: '{}'", err);
        }
    }
    let mut verification_checks = VerifiedStatus::new();
    // we should disabled this one we know this is working.
    verification_checks.set_correct_shares();
    verification_checks.set_median_timestamp();
    verification_checks.set_difficulty_verified();
    verification_checks.set_target_difficulty_verified();
    let share_chain_sha3x = InMemoryShareChain::new(
        config.clone(),
        PowAlgorithm::Sha3x,
        None,
        coinbase_extras_sha3x.clone(),
        stats_broadcast_client.clone(),
        squad.clone(),
        Some(verification_checks),
    )?;
    let coinbase_extras_random_x = Arc::new(RwLock::new(HashMap::<String, Vec<u8>>::new()));
    let share_chain_random_x = InMemoryShareChain::new(
        config.clone(),
        PowAlgorithm::RandomX,
        Some(block_validation_params.clone()),
        coinbase_extras_random_x.clone(),
        stats_broadcast_client.clone(),
        squad.clone(),
        Some(verification_checks),
    )?;
    Server::new(
        config,
        share_chain_sha3x,
        share_chain_random_x,
        stats_collector,
        stats_broadcast_client,
        diagnostics_collector,
        diagnostics_broadcast_client,
        diagnostics_receiver_client,
        shutdown,
        swarm,
        squad,
    )
    .await
}

#[derive(Serialize, Deserialize)]
pub struct LibP2pInfo {
    pub peer_id: PeerId,
    pub squad: String,
}

impl LibP2pInfo {
    pub fn save_to_file(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer(writer, &self)?;
        Ok(())
    }

    pub fn read_from_file(path: &Path, timeout: Duration) -> io::Result<Self> {
        let start = Instant::now();
        while !path.exists() {
            if start.elapsed() >= timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("Timeout waiting for '{}' to be created", path.display()),
                ));
            }
            sleep(Duration::from_millis(100));
        }
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let libp2p_info = serde_json::from_reader(reader)?;
        Ok(libp2p_info)
    }
}
