use crate::sharechain::in_memory::InMemoryShareChain;

mod server;
mod sharechain;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let config = server::Config::builder().build();
    let share_chain = InMemoryShareChain::default();
    let mut server = server::Server::new(config, share_chain).await?;
    server.start().await?;
    Ok(())
}
