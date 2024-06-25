use clap::builder::Styles;
use clap::builder::styling::AnsiColor;
use clap::Parser;
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
#[command(about = "⛏ Decentralized pool mining for Tari network ⛏", long_about = None)]
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
    /// Please note that these addresses must be in libp2p multi address format!
    /// e.g.: /dnsaddr/libp2p.io
    #[arg(short, long, value_name = "seed-peers")]
    seed_peers: Option<Vec<String>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

    let config = config_builder.build();
    let share_chain = InMemoryShareChain::default();
    let mut server = server::Server::new(config, share_chain).await?;
    server.start().await?;
    Ok(())
}
