// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tari_shutdown::ShutdownSignal;

use crate::cli::commands;
use crate::cli::util::cli_styles;
use crate::cli::util::validate_tribe;

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Parser, Debug)]
pub struct StartArgs {
    /// (Optional) base dir.
    #[arg(short, long, value_name = "base-dir")]
    base_dir: Option<PathBuf>,

    /// (Optional) gRPC port to use.
    #[arg(short, long, value_name = "grpc-port")]
    pub grpc_port: Option<u16>,

    /// (Optional) p2p port to use. It is used to connect p2pool nodes.
    #[arg(short, long, value_name = "p2p-port")]
    pub p2p_port: Option<u16>,

    /// (Optional) stats server port to use.
    #[arg(long, value_name = "stats-server-port")]
    pub stats_server_port: Option<u16>,

    /// (Optional) seed peers.
    /// Any amount of seed peers can be added to join a p2pool network.
    ///
    /// Please note that these addresses must be in libp2p multi address format and must contain peer ID
    /// or use a dnsaddr multi address!
    ///
    /// By default a Tari provided seed peer is added.
    ///
    /// e.g.:
    /// /ip4/127.0.0.1/tcp/52313/p2p/12D3KooWCUNCvi7PBPymgsHx39JWErYdSoT3EFPrn3xoVff4CHFu
    /// /dnsaddr/esmeralda.p2pool.tari.com
    #[arg(short, long, value_name = "seed-peers")]
    pub seed_peers: Option<Vec<String>>,

    /// If set, Tari provided seed peers will NOT be automatically added to seed peers list.
    #[arg(long, value_name = "no-default-seed-peers", default_value_t = false)]
    pub no_default_seed_peers: bool,

    /// Starts the node as a stable peer.
    ///
    /// Identity of the peer will be saved locally (to --private-key-location)
    /// and ID of the Peer remains the same.
    #[arg(long, value_name = "stable-peer", default_value_t = false)]
    pub stable_peer: bool,

    /// Tribe to enter (a team of miners).
    /// A tribe can have any name.
    #[arg(
        long, value_name = "tribe", default_value = "default", value_parser = validate_tribe
    )]
    pub tribe: String,

    /// Private key folder.
    ///
    /// Needs --stable-peer to be set.
    #[arg(
        long,
        value_name = "private-key-folder",
        requires = "stable_peer",
        default_value = "."
    )]
    pub private_key_folder: PathBuf,

    /// Mining disabled
    ///
    /// In case it is set, the node will only handle p2p operations,
    /// will be syncing with share chain, but not starting gRPC services and no Tari base node needed.
    /// By setting this it can be used as a stable node for routing only.
    #[arg(long, value_name = "mining-disabled", default_value_t = false)]
    pub mining_disabled: bool,

    /// mDNS disabled
    ///
    /// If set, mDNS local peer discovery is disabled.
    #[arg(long, value_name = "mdns-disabled", default_value_t = false)]
    pub mdns_disabled: bool,

    /// HTTP server disabled
    ///
    /// If set, local HTTP server (stats, health-check, status etc...) is disabled.
    #[arg(long, value_name = "http-server-disabled", default_value_t = false)]
    pub http_server_disabled: bool,
}

#[derive(Subcommand, Clone, Debug)]
pub enum Commands {
    /// Starts sha-p2pool node.
    Start {
        #[clap(flatten)]
        args: StartArgs,
    },

    /// Generating new identity.
    GenerateIdentity,

    /// Listing all tribes that are present on the network.
    ListTribes {
        #[clap(flatten)]
        args: StartArgs,
    },
}

#[derive(Clone, Parser)]
#[command(version)]
#[command(styles = cli_styles())]
#[command(about = "⛏ Decentralized mining pool for Tari network ⛏", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

impl Cli {
    pub fn base_dir(&self) -> PathBuf {
        match &self.command {
            Commands::Start { args } => args
                .base_dir
                .clone()
                .unwrap_or_else(|| dirs::home_dir().unwrap().join(".tari/p2pool")),
            Commands::GenerateIdentity => dirs::home_dir().unwrap().join(".tari/p2pool"),
            Commands::ListTribes { args } => args
                .base_dir
                .clone()
                .unwrap_or_else(|| dirs::home_dir().unwrap().join(".tari/p2pool")),
        }
    }

    /// Handles CLI command.
    /// [`Cli::parse`] must be called (to have all the args and params set properly)
    /// before calling this method.
    pub async fn handle_command(&self, cli_shutdown: ShutdownSignal) -> anyhow::Result<()> {
        let cli_ref = Arc::new(self.clone());

        match &self.command {
            Commands::Start { args } => {
                commands::handle_start(cli_ref.clone(), args, cli_shutdown.clone()).await?;
            },
            Commands::GenerateIdentity => {
                commands::handle_generate_identity().await?;
            },
            Commands::ListTribes { args } => {
                commands::handle_list_tribes(cli_ref.clone(), args, cli_shutdown.clone()).await?;
            },
        }

        Ok(())
    }
}
