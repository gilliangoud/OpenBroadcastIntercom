use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct ServerAppConfig {
    pub audio_bind: SocketAddr,
    pub control_bind: SocketAddr,
    pub admin_bind: SocketAddr,
    pub admin_state_file: PathBuf,
    pub recordings_dir: PathBuf,
    pub whisper_model_dir: PathBuf,
    pub deepfilternet_model_dir: PathBuf,
    pub deepfilternet_coreml_model_dir: PathBuf,
    pub advertise_name: Option<String>,
}

impl ServerAppConfig {
    pub fn for_app_data(app_data_dir: impl AsRef<Path>) -> Self {
        let app_data_dir = app_data_dir.as_ref();
        Self {
            audio_bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 40000),
            control_bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 40001),
            admin_bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 40002),
            admin_state_file: app_data_dir.join("intercom-state.json"),
            recordings_dir: app_data_dir.join("recordings"),
            whisper_model_dir: app_data_dir.join("intercom-models"),
            deepfilternet_model_dir: app_data_dir.join("deepfilternet-models"),
            deepfilternet_coreml_model_dir: app_data_dir.join("deepfilternet-coreml-models"),
            advertise_name: Some("RedLine Server".to_string()),
        }
    }

    pub fn admin_url(&self) -> String {
        format!("http://127.0.0.1:{}/admin", self.admin_bind.port())
    }

    pub fn into_server_runtime_config(self) -> server::ServerRuntimeConfig {
        server::ServerRuntimeConfig {
            audio_bind: self.audio_bind,
            control_bind: self.control_bind,
            admin_bind: Some(self.admin_bind),
            admin_state_file: Some(self.admin_state_file),
            admin_auth: server::HttpAuthConfig::disabled(),
            enrollment_policy: server::EnrollmentPolicy::Auto,
            advertise_name: self.advertise_name,
            disable_discovery: false,
            recordings_dir: self.recordings_dir,
            debug_audio_dir: None,
            whisper_command: None,
            whisper_model: None,
            whisper_model_dir: self.whisper_model_dir,
            deepfilternet_model_dir: self.deepfilternet_model_dir,
            deepfilternet_coreml_model_dir: self.deepfilternet_coreml_model_dir,
            transcription_engine: server::TranscriptionEngineMode::Disabled,
        }
    }
}

pub async fn start_server_app_runtime(
    config: ServerAppConfig,
) -> Result<server::ServerRuntimeHandle> {
    if let Some(parent) = config.admin_state_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create server app state directory {}", parent.display()))?;
    }
    std::fs::create_dir_all(&config.recordings_dir).with_context(|| {
        format!(
            "create server app recordings directory {}",
            config.recordings_dir.display()
        )
    })?;
    std::fs::create_dir_all(&config.whisper_model_dir).with_context(|| {
        format!(
            "create server app Whisper model directory {}",
            config.whisper_model_dir.display()
        )
    })?;
    std::fs::create_dir_all(&config.deepfilternet_model_dir).with_context(|| {
        format!(
            "create server app DeepFilterNet model directory {}",
            config.deepfilternet_model_dir.display()
        )
    })?;
    std::fs::create_dir_all(&config.deepfilternet_coreml_model_dir).with_context(|| {
        format!(
            "create server app DeepFilterNet Core ML model directory {}",
            config.deepfilternet_coreml_model_dir.display()
        )
    })?;
    server::start_runtime(config.into_server_runtime_config()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_app_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("intercom-server-app-{name}-{}", std::process::id()))
    }

    #[test]
    fn defaults_bind_admin_to_loopback_and_store_state_in_app_data() {
        let app_dir = temp_app_dir("defaults");
        let config = ServerAppConfig::for_app_data(&app_dir);

        assert_eq!(config.audio_bind, SocketAddr::from(([0, 0, 0, 0], 40000)));
        assert_eq!(config.control_bind, SocketAddr::from(([0, 0, 0, 0], 40001)));
        assert_eq!(config.admin_bind, SocketAddr::from(([127, 0, 0, 1], 40002)));
        assert_eq!(config.admin_state_file, app_dir.join("intercom-state.json"));
        assert_eq!(config.recordings_dir, app_dir.join("recordings"));
        assert_eq!(config.whisper_model_dir, app_dir.join("intercom-models"));
        assert_eq!(
            config.deepfilternet_model_dir,
            app_dir.join("deepfilternet-models")
        );
        assert_eq!(
            config.deepfilternet_coreml_model_dir,
            app_dir.join("deepfilternet-coreml-models")
        );
    }

    #[test]
    fn loopback_admin_is_not_advertised_by_discovery() {
        let config = ServerAppConfig::for_app_data(temp_app_dir("discovery"));
        let runtime_config = config.into_server_runtime_config();

        assert_eq!(
            server::discovery_admin_port(runtime_config.admin_bind),
            None
        );
    }

    #[tokio::test]
    async fn startup_failure_reports_port_bind_error() {
        let held = std::net::UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let held_addr = held.local_addr().unwrap();
        let mut config = ServerAppConfig::for_app_data(temp_app_dir("bind-error"));
        config.audio_bind = held_addr;
        config.control_bind = SocketAddr::from(([127, 0, 0, 1], 0));
        config.admin_bind = SocketAddr::from(([127, 0, 0, 1], 0));

        let err = match start_server_app_runtime(config).await {
            Ok(handle) => {
                handle.shutdown();
                panic!("server app runtime unexpectedly started")
            }
            Err(err) => err,
        };

        assert!(err.to_string().contains("bind UDP audio socket"));
    }
}
