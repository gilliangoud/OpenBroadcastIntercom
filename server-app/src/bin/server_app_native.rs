#[cfg(not(feature = "native"))]
fn main() -> anyhow::Result<()> {
    println!("Build with `--features native` to run the Intercom Server macOS app.");
    Ok(())
}

#[cfg(feature = "native")]
fn main() -> anyhow::Result<()> {
    native::main()
}

#[cfg(feature = "native")]
mod native {
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use anyhow::{Context, Result};
    use tauri::Manager;
    use tokio::sync::oneshot;
    use tracing_subscriber::EnvFilter;

    pub fn main() -> Result<()> {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env().add_directive("server=info".parse()?))
            .try_init();

        let runtime_slot: Arc<Mutex<Option<ServerAppRuntime>>> = Arc::new(Mutex::new(None));
        let setup_runtime_slot = Arc::clone(&runtime_slot);
        let result = tauri::Builder::default()
            .setup(move |app| {
                let app_data_dir = app
                    .path()
                    .app_data_dir()
                    .context("resolve server app data directory")?;
                let runtime = spawn_server_app_runtime(app_data_dir)?;
                let admin_url: tauri::Url = runtime
                    .admin_url
                    .parse()
                    .context("parse server admin URL")?;

                *setup_runtime_slot
                    .lock()
                    .map_err(|_| anyhow::anyhow!("server app runtime state is poisoned"))? =
                    Some(runtime);

                tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    tauri::WebviewUrl::External(admin_url),
                )
                .title("Intercom Server")
                .inner_size(1180.0, 780.0)
                .min_inner_size(840.0, 560.0)
                .center()
                .build()?;
                Ok(())
            })
            .run(tauri::generate_context!("tauri.conf.json"))
            .context("run Intercom Server app");

        if let Some(runtime) = runtime_slot
            .lock()
            .map_err(|_| anyhow::anyhow!("server app runtime state is poisoned"))?
            .take()
        {
            runtime.shutdown()?;
        }
        result
    }

    struct ServerAppRuntime {
        admin_url: String,
        shutdown_tx: Option<oneshot::Sender<()>>,
        join: Option<thread::JoinHandle<Result<()>>>,
    }

    impl ServerAppRuntime {
        fn shutdown(mut self) -> Result<()> {
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.send(());
            }
            if let Some(join) = self.join.take() {
                join.join()
                    .map_err(|_| anyhow::anyhow!("server app runtime thread panicked"))??;
            }
            Ok(())
        }
    }

    fn spawn_server_app_runtime(app_data_dir: std::path::PathBuf) -> Result<ServerAppRuntime> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let join = thread::Builder::new()
            .name("intercom-server-app-runtime".to_string())
            .spawn(move || {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .context("build server app runtime")?
                    .block_on(async move {
                        let config = server_app::ServerAppConfig::for_app_data(app_data_dir);
                        let mut handle = match server_app::start_server_app_runtime(config).await {
                            Ok(handle) => handle,
                            Err(err) => {
                                let _ = ready_tx.send(Err(err.to_string()));
                                return Err(err);
                            }
                        };
                        let admin_url = handle
                            .admin_addr
                            .map(|addr| format!("http://127.0.0.1:{}/admin", addr.port()))
                            .unwrap_or_else(|| "http://127.0.0.1:40002/admin".to_string());
                        let _ = ready_tx.send(Ok(admin_url));
                        tokio::select! {
                            _ = shutdown_rx => {
                                handle.shutdown();
                                Ok(())
                            }
                            result = handle.wait() => result,
                        }
                    })
            })
            .context("spawn server app runtime")?;

        let admin_url = ready_rx
            .recv_timeout(Duration::from_secs(10))
            .context("server app runtime did not report readiness")?
            .map_err(anyhow::Error::msg)?;

        Ok(ServerAppRuntime {
            admin_url,
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
        })
    }
}
