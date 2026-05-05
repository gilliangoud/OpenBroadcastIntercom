use clap::Parser;

fn main() -> anyhow::Result<()> {
    desktop::init_tracing()?;
    let args = desktop::Args::parse();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(desktop::run(args))
}
