mod server;
mod sharechain;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let config = server::Config::builder().build();
    let mut server = server::Server::new(config).await?;
    server.start().await?;
    Ok(())
}
