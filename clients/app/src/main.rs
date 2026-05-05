use clap::Parser;

fn main() -> anyhow::Result<()> {
    desktop::init_tracing()?;
    let args = app::AppArgs::parse();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(app::run_app(args))
}
