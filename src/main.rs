// Copyright 2024 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{
    builder::{Styles, styling::AnsiColor},
    Parser, Subcommand,
};
use itertools::Itertools;
use libp2p::futures::SinkExt;
use libp2p::identity::Keypair;
use tari_common::configuration::Network;
use tari_common::initialize_logging;
use tari_shutdown::{Shutdown, ShutdownSignal};
use tokio::{select, time};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::server::{p2p, Server};
use crate::server::p2p::peer_store::PeerStore;
use crate::server::p2p::Tribe;
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

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Parser, Debug)]
struct StartArgs {
    /// (Optional) gRPC port to use.
    #[arg(short, long, value_name = "grpc-port")]
    grpc_port: Option<u16>,

    /// (Optional) p2p port to use. It is used to connect p2pool nodes.
    #[arg(short, long, value_name = "p2p-port")]
    p2p_port: Option<u16>,

    /// (Optional) stats server port to use.
    #[arg(long, value_name = "stats-server-port")]
    stats_server_port: Option<u16>,

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
    seed_peers: Option<Vec<String>>,

    /// If set, Tari provided seed peers will NOT be automatically added to seed peers list.
    #[arg(long, value_name = "no-default-seed-peers", default_value_t = false)]
    no_default_seed_peers: bool,

    /// Starts the node as a stable peer.
    ///
    /// Identity of the peer will be saved locally (to --private-key-location)
    /// and ID of the Peer remains the same.
    #[arg(long, value_name = "stable-peer", default_value_t = false)]
    stable_peer: bool,

    /// Tribe to enter (a team of miners).
    /// A tribe can have any name.
    #[arg(
        long, value_name = "tribe", default_value = "default", value_parser = validate_tribe
    )]
    tribe: String,

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

    /// Stats server disabled
    ///
    /// If set, local stats HTTP server is disabled.
    #[arg(long, value_name = "stats-server-disabled", default_value_t = false)]
    stats_server_disabled: bool,
}

#[derive(Subcommand, Clone, Debug)]
enum Commands {
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
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// (Optional) base dir.
    #[arg(short, long, value_name = "base-dir")]
    base_dir: Option<PathBuf>,
}

fn validate_tribe(tribe: &str) -> Result<String, String> {
    if tribe.trim().is_empty() {
        return Err(String::from("tribe must be set"));
    }

    Ok(String::from(tribe))
}

impl Cli {
    pub fn base_dir(&self) -> PathBuf {
        self.base_dir
            .clone()
            .unwrap_or_else(|| dirs::home_dir().unwrap().join(".tari/p2pool"))
    }
}

async fn server(cli: Arc<Cli>, args: &StartArgs, shutdown_signal: ShutdownSignal, enable_logging: bool)
                -> anyhow::Result<Server<InMemoryShareChain>> {
    if enable_logging {
        // logger setup
        if let Err(e) = initialize_logging(
            &cli.base_dir().join("configs/logs.yml"),
            &cli.base_dir(),
            include_str!("../log4rs_sample.yml"),
        ) {
            eprintln!("{}", e);
            return Err(e.into());
        }
    }

    let mut config_builder = server::Config::builder();
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
    config_builder.with_stats_server_enabled(!args.stats_server_disabled);
    if let Some(stats_server_port) = args.stats_server_port {
        config_builder.with_stats_server_port(stats_server_port);
    }

    let config = config_builder.build();
    let share_chain = InMemoryShareChain::default();

    Ok(Server::new(config, share_chain, shutdown_signal).await?)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let shutdown = Shutdown::new();
    let cli = Cli::parse();

    let cli_ref = Arc::new(cli.clone());
    match &cli.command {
        Commands::Start { args } => {
            server(cli_ref.clone(), args, shutdown.to_signal(), true).await?.start().await?;
        }
        Commands::GenerateIdentity => {
            let result = p2p::util::generate_identity().await?;
            print!("{}", serde_json::to_value(result)?);
        }
        Commands::ListTribes { args } => {
            // start server asynchronously
            let cli_ref = cli_ref.clone();
            let mut args_clone = args.clone();
            args_clone.mining_disabled = true;
            let shutdown_signal = shutdown.to_signal();
            let (peer_store_channel_tx, peer_store_channel_rx) =
                oneshot::channel::<Arc<PeerStore>>();
            let handle: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
                let mut server = server(cli_ref, &args_clone, shutdown_signal, false).await?;
                let _ = peer_store_channel_tx.send(server.p2p_service().network_peer_store().clone());
                server.start().await?;
                Ok(())
            });

            // wait for peer store from started server
            let peer_store = peer_store_channel_rx.await?;

            // collect tribes for 30 seconds
            let mut tribes = vec![];
            let timeout = time::sleep(Duration::from_secs(30));
            tokio::pin!(timeout);
            loop {
                select! {
                    () = &mut timeout => {
                        break;
                    }
                    current_tribes = peer_store.tribes() => {
                        tribes = current_tribes;
                    }
                }
            }
            handle.abort();
            let tribes = tribes.iter().map(|tribe| tribe.to_string()).collect_vec();
            print!("{}", serde_json::to_value(tribes)?);
        }
    }

    Ok(())
}
