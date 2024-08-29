// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::env;
use std::sync::Arc;

use libp2p::identity::Keypair;
use tari_common::configuration::Network;
use tari_common::initialize_logging;
use tari_shutdown::ShutdownSignal;

use crate::cli::args::{Cli, StartArgs};
use crate::server as main_server;
use crate::server::p2p::Tribe;
use crate::server::Server;
use crate::sharechain::in_memory::InMemoryShareChain;

pub async fn server(
    cli: Arc<Cli>,
    args: &StartArgs,
    shutdown_signal: ShutdownSignal,
    enable_logging: bool,
) -> anyhow::Result<Server<InMemoryShareChain>> {
    if enable_logging {
        // logger setup
        if let Err(e) = initialize_logging(
            &cli.base_dir().join("configs/logs.yml"),
            &cli.base_dir(),
            include_str!("../../../log4rs_sample.yml"),
        ) {
            eprintln!("{}", e);
            return Err(e.into());
        }
    }

    let mut config_builder = main_server::Config::builder();
    if let Some(grpc_port) = args.grpc_port {
        config_builder.with_grpc_port(grpc_port);
    }
    if let Some(p2p_port) = args.p2p_port {
        config_builder.with_p2p_port(p2p_port);
    }

    config_builder.with_tribe(Tribe::from(args.tribe.clone()));

    // set default tari network specific seed peer address
    let mut seed_peers = vec![];
    let network = Network::get_current_or_user_setting_or_default();
    if network != Network::LocalNet && !args.no_default_seed_peers {
        let default_seed_peer = format!("/dnsaddr/{}.p2pool.tari.com", network.as_key_str());
        seed_peers.push(default_seed_peer);
    }
    if let Some(cli_seed_peers) = args.seed_peers.clone() {
        seed_peers.extend(cli_seed_peers.iter().cloned());
    }
    config_builder.with_seed_peers(seed_peers);

    config_builder.with_stable_peer(args.stable_peer);
    config_builder.with_private_key_folder(args.private_key_folder.clone());

    // try to extract env var based private key
    if let Ok(identity_cbor) = env::var("SHA_P2POOL_IDENTITY") {
        let identity_raw = hex::decode(identity_cbor.as_bytes())?;
        let private_key = Keypair::from_protobuf_encoding(identity_raw.as_slice())?;
        config_builder.with_private_key(Some(private_key));
    }

    config_builder.with_mining_enabled(!args.mining_disabled);
    config_builder.with_mdns_enabled(!args.mdns_disabled);
    config_builder.with_http_server_enabled(!args.http_server_disabled);
    if let Some(stats_server_port) = args.stats_server_port {
        config_builder.with_stats_server_port(stats_server_port);
    }
    config_builder.with_base_node_address(args.base_node_address.clone());

    let config = config_builder.build();
    let share_chain = InMemoryShareChain::default();

    Ok(Server::new(config, share_chain, shutdown_signal).await?)
}
