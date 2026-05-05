use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    bridge_app::init_tracing()?;
    bridge_app::run(bridge_app::Args::parse()).await
}
