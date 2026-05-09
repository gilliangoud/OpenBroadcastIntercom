#[cfg(not(feature = "native"))]
fn main() -> anyhow::Result<()> {
    use clap::Parser;

    bridge_app::init_tracing()?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(bridge_app::run(bridge_app::Args::parse()))
}

#[cfg(feature = "native")]
fn main() -> anyhow::Result<()> {
    native::main()
}

#[cfg(feature = "native")]
mod native {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
    use std::thread;
    use std::time::Duration;

    use anyhow::{Context, Result};
    use clap::Parser;

    pub fn main() -> Result<()> {
        bridge_app::init_tracing()?;
        let mut args = bridge_app::Args::parse();
        args.no_open = true;
        let title = "RedLine Bridge".to_string();
        let url = format!("http://{}", args.bind);
        let shutdown_bind = args.bind;

        let _runtime_handle = spawn_bridge_app(args)?;
        thread::sleep(Duration::from_millis(500));
        let result = run_tauri_window(title, url);
        if let Err(err) = post_stop_all(shutdown_bind) {
            tracing::warn!(%err, "failed to stop bridge routes while closing native bridge app");
        }
        result
    }

    fn spawn_bridge_app(args: bridge_app::Args) -> Result<thread::JoinHandle<Result<()>>> {
        thread::Builder::new()
            .name("intercom-bridge-app-runtime".to_string())
            .spawn(move || {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .context("build bridge app runtime")?
                    .block_on(bridge_app::run(args))
            })
            .context("spawn bridge app runtime")
    }

    fn run_tauri_window(title: String, url: String) -> Result<()> {
        let external_url: tauri::Url = url.parse().context("parse bridge app URL")?;

        tauri::Builder::default()
            .setup(move |app| {
                tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    tauri::WebviewUrl::External(external_url.clone()),
                )
                .title(title.clone())
                .inner_size(980.0, 760.0)
                .min_inner_size(720.0, 520.0)
                .center()
                .build()?;
                Ok(())
            })
            .run(tauri::generate_context!("tauri.conf.json"))
            .context("run Tauri bridge app")?;
        Ok(())
    }

    fn post_stop_all(bind: SocketAddr) -> Result<()> {
        let target = if bind.ip().is_unspecified() {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bind.port())
        } else {
            bind
        };
        let mut stream = TcpStream::connect_timeout(&target, Duration::from_secs(1))
            .with_context(|| format!("connect bridge app shutdown endpoint at {target}"))?;
        let request = format!(
            "POST /api/routes/stop-all HTTP/1.1\r\nHost: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .context("send bridge app shutdown request")?;
        let mut response = [0_u8; 256];
        let _ = stream.read(&mut response);
        Ok(())
    }
}
