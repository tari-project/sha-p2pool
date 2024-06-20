use clap::Parser;

use crate::sharechain::in_memory::InMemoryShareChain;

mod server;
mod sharechain;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Optional gRPC port to use.
    #[arg(short, long, value_name = "grpc-port")]
    grpc_port: Option<u16>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    // use cli params for constructing server config
    let cli = Cli::parse();
    let mut config_builder = server::Config::builder();
    if let Some(grpc_port) = cli.grpc_port {
        config_builder.with_grpc_port(grpc_port);
    }

    let config = config_builder.build();
    let share_chain = InMemoryShareChain::default();
    let mut server = server::Server::new(config, share_chain).await?;
    server.start().await?;
    Ok(())
}
