// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::path::PathBuf;

use clap::{
    builder::{styling::AnsiColor, Styles},
    Parser,
};
use env_logger::Builder;
use log::LevelFilter;

use crate::sharechain::in_memory::InMemoryShareChain;

mod server;
mod sharechain;

fn cli_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::BrightYellow.on_default())
        .usage(AnsiColor::BrightYellow.on_default())
        .literal(AnsiColor::BrightGreen.on_default())
        .placeholder(AnsiColor::BrightCyan.on_default())
        .error(AnsiColor::BrightRed.on_default())
        .invalid(AnsiColor::BrightRed.on_default())
        .valid(AnsiColor::BrightGreen.on_default())
}

#[derive(Parser)]
#[command(version)]
#[command(styles = cli_styles())]
#[command(about = "⛏ Decentralized mining pool for Tari network ⛏", long_about = None)]
struct Cli {
    /// Log level
    #[arg(short, long, value_name = "log-level", default_value = Some("info"))]
    log_level: LevelFilter,

    /// (Optional) gRPC port to use.
    #[arg(short, long, value_name = "grpc-port")]
    grpc_port: Option<u16>,

    /// (Optional) p2p port to use. It is used to connect p2pool nodes.
    #[arg(short, long, value_name = "p2p-port")]
    p2p_port: Option<u16>,

    /// (Optional) seed peers.
    /// Any amount of seed peers can be added to join a p2pool network.
    ///
    /// Please note that these addresses must be in libp2p multi address format and must contain peer ID!
    ///
    /// e.g.: /ip4/127.0.0.1/tcp/52313/p2p/12D3KooWCUNCvi7PBPymgsHx39JWErYdSoT3EFPrn3xoVff4CHFu
    #[arg(short, long, value_name = "seed-peers")]
    seed_peers: Option<Vec<String>>,

    /// Starts the node as a stable peer.
    ///
    /// Identity of the peer will be saved locally (to --private-key-location)
    /// and ID of the Peer remains the same.
    #[arg(long, value_name = "stable-peer", default_value_t = false)]
    stable_peer: bool,

    /// Private key folder.
    ///
    /// Needs --stable-peer to be set.
    #[arg(
        long,
        value_name = "private-key-folder",
        requires = "stable_peer",
        default_value = "."
    )]
    private_key_folder: PathBuf,

    /// Mining disabled
    ///
    /// In case it is set, the node will only handle p2p operations,
    /// will be syncing with share chain, but not starting gRPC services and no Tari base node needed.
    /// By setting this it can be used as a stable node for routing only.
    #[arg(long, value_name = "mining-disabled", default_value_t = false)]
    mining_disabled: bool,

    /// mDNS disabled
    ///
    /// If set, mDNS local peer discovery is disabled.
    #[arg(long, value_name = "mdns-disabled", default_value_t = false)]
    mdns_disabled: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // cli
    let cli = Cli::parse();
    Builder::new().filter_level(cli.log_level).init();
    let mut config_builder = server::Config::builder();
    if let Some(grpc_port) = cli.grpc_port {
        config_builder.with_grpc_port(grpc_port);
    }
    if let Some(p2p_port) = cli.p2p_port {
        config_builder.with_p2p_port(p2p_port);
    }
    if let Some(seed_peers) = cli.seed_peers {
        config_builder.with_seed_peers(seed_peers);
    }
    config_builder.with_stable_peer(cli.stable_peer);
    config_builder.with_private_key_folder(cli.private_key_folder);
    config_builder.with_mining_enabled(!cli.mining_disabled);
    config_builder.with_mdns_enabled(!cli.mdns_disabled);

    // server start
    let config = config_builder.build();
    let share_chain = InMemoryShareChain::default();
    let mut server = server::Server::new(config, share_chain).await?;
    server.start().await?;
    Ok(())
}
