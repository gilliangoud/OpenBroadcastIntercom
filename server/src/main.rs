use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use tokio::net::{TcpListener, UdpSocket};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, default_value = "0.0.0.0:40000")]
    audio_bind: SocketAddr,
    #[arg(long, default_value = "0.0.0.0:40001")]
    control_bind: SocketAddr,
    #[arg(long, default_value = "0.0.0.0:40002")]
    admin_bind: SocketAddr,
    #[arg(long, default_value = "intercom-state.json")]
    admin_state_file: PathBuf,
    #[arg(long, value_enum, default_value_t = server::EnrollmentPolicy::Auto)]
    enrollment_policy: server::EnrollmentPolicy,
    #[arg(long)]
    disable_admin_ui: bool,
    #[arg(long, env = "INTERCOM_ADMIN_TOKEN")]
    admin_token: Option<String>,
    #[arg(long)]
    advertise_name: Option<String>,
    #[arg(long)]
    disable_discovery: bool,
    #[arg(long, default_value = "intercom-recordings")]
    recordings_dir: PathBuf,
    #[arg(long)]
    debug_audio_dir: Option<PathBuf>,
    #[arg(long, env = "INTERCOM_WHISPER_COMMAND")]
    whisper_command: Option<PathBuf>,
    #[arg(long, env = "INTERCOM_WHISPER_MODEL")]
    whisper_model: Option<PathBuf>,
    #[arg(long, default_value = "intercom-models")]
    whisper_model_dir: PathBuf,
    #[arg(long, default_value = "deepfilternet-models")]
    deepfilternet_model_dir: PathBuf,
    #[arg(long, value_enum)]
    transcription_engine: Option<server::TranscriptionEngineMode>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("server=info".parse()?))
        .init();

    let args = Args::parse();
    let transcription_engine = args.transcription_engine.unwrap_or_else(|| {
        if args.whisper_model.is_some() {
            server::TranscriptionEngineMode::BuiltinWhisper
        } else {
            server::TranscriptionEngineMode::Disabled
        }
    });
    let audio_socket = Arc::new(
        UdpSocket::bind(args.audio_bind)
            .await
            .with_context(|| format!("bind UDP audio socket at {}", args.audio_bind))?,
    );
    let control_listener = TcpListener::bind(args.control_bind)
        .await
        .with_context(|| format!("bind WebSocket control listener at {}", args.control_bind))?;
    let admin_listener = if args.disable_admin_ui {
        None
    } else {
        Some(
            TcpListener::bind(args.admin_bind)
                .await
                .with_context(|| format!("bind admin HTTP listener at {}", args.admin_bind))?,
        )
    };

    let admin_auth = args
        .admin_token
        .clone()
        .map_or_else(server::HttpAuthConfig::disabled, |token| {
            server::HttpAuthConfig::token(token, "Intercom Admin")
        });
    let actual_audio_addr = audio_socket.local_addr()?;
    let actual_control_addr = control_listener.local_addr()?;
    let actual_admin_addr = admin_listener
        .as_ref()
        .map(|listener| listener.local_addr())
        .transpose()?;
    let _discovery_handle = if args.disable_discovery {
        None
    } else {
        let advertisement = server::DiscoveryAdvertisement {
            name: args
                .advertise_name
                .clone()
                .unwrap_or_else(server::default_discovery_name),
            control_port: actual_control_addr.port(),
            audio_port: actual_audio_addr.port(),
            admin_port: actual_admin_addr.map(|addr| addr.port()),
            auth_required: admin_auth.is_enabled(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        match server::start_discovery_advertisement(&advertisement) {
            Ok(handle) => {
                tracing::info!(
                    name = %advertisement.name,
                    service = server::DISCOVERY_SERVICE_TYPE,
                    control_port = advertisement.control_port,
                    "Bonjour discovery advertisement started"
                );
                Some(handle)
            }
            Err(err) => {
                tracing::warn!(%err, "Bonjour discovery advertisement unavailable");
                None
            }
        }
    };

    if args.disable_admin_ui {
        tracing::info!(audio = %args.audio_bind, control = %args.control_bind, "intercom server listening");
    } else if admin_auth.is_enabled() {
        tracing::info!(
            audio = %args.audio_bind,
            control = %args.control_bind,
            admin = %args.admin_bind,
            state_file = %args.admin_state_file.display(),
            enrollment_policy = ?args.enrollment_policy,
            recordings_dir = %args.recordings_dir.display(),
            debug_audio_dir = ?args.debug_audio_dir,
            model_dir = %args.whisper_model_dir.display(),
            deepfilternet_model_dir = %args.deepfilternet_model_dir.display(),
            transcription_engine = ?transcription_engine,
            "intercom server listening; admin UI/API requires HTTP authorization"
        );
    } else {
        tracing::warn!(
            audio = %args.audio_bind,
            control = %args.control_bind,
            admin = %args.admin_bind,
            state_file = %args.admin_state_file.display(),
            enrollment_policy = ?args.enrollment_policy,
            recordings_dir = %args.recordings_dir.display(),
            debug_audio_dir = ?args.debug_audio_dir,
            model_dir = %args.whisper_model_dir.display(),
            deepfilternet_model_dir = %args.deepfilternet_model_dir.display(),
            transcription_engine = ?transcription_engine,
            "intercom server listening; admin UI has no authentication"
        );
    }

    server::run_with_options(
        audio_socket,
        control_listener,
        server::RunOptions {
            admin_listener,
            admin_state_file: Some(args.admin_state_file),
            admin_auth,
            enrollment_policy: args.enrollment_policy,
            recordings_dir: args.recordings_dir,
            debug_audio_dir: args.debug_audio_dir,
            whisper_command: args.whisper_command,
            whisper_model: args.whisper_model,
            whisper_model_dir: args.whisper_model_dir,
            deepfilternet_model_dir: args.deepfilternet_model_dir,
            transcription_engine,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_cli_defaults_to_enabled_advertisement() {
        let args = Args::try_parse_from(["server"]).unwrap();

        assert!(!args.disable_discovery);
        assert_eq!(args.advertise_name, None);
    }

    #[test]
    fn discovery_cli_accepts_name_and_disable_flag() {
        let args = Args::try_parse_from([
            "server",
            "--advertise-name",
            "Truck A",
            "--disable-discovery",
        ])
        .unwrap();

        assert!(args.disable_discovery);
        assert_eq!(args.advertise_name.as_deref(), Some("Truck A"));
    }
}
