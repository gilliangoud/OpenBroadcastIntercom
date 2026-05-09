use std::collections::HashMap;
use std::fs;
use std::future::{self, Future};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use axum::extract::Request;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use clap::{Parser, ValueEnum};
use client_core::{
    bind_tcp_listener_with_port_fallback, capture_health_with_client_telemetry,
    default_button_capabilities, format_button_ids, format_buttons, format_channels, format_codec,
    format_volumes, load_or_create_client_uid, merge_button_capabilities, parse_channels,
    parse_volumes, run_connection_cue_task, run_control_connection, samples_for_ms,
    send_control_request, supported_codecs, AlertRequest, AudioDecoder, AudioEncoder,
    AudioSettings, ClientAudioBackend, ClientAudioBackendKind, ClientConfig, ClientConnectionEvent,
    ClientControlApi, ClientServerEndpoint, ClientTelemetryCounters, CodecRequest, ControlRequest,
    FullConfigRequest, GainRequest, InputBackendState, MacosMicrophoneModeStatus, OkResponse,
    PlaybackBuffer, StateResponse, TalkModeRequest, DEFAULT_CLIENT_BUTTON_COUNT,
    DEFAULT_CONTROL_PORT, DEFAULT_SERVER_HOST, MAX_CLIENT_BUTTON_COUNT,
};
use common::{
    codec_samples_per_frame, AlertId, AudioPacket, ButtonCapability, ButtonId,
    CaptureChannelHealth, CaptureHealthStatus, ClientLockoutPolicy, ClientRole, Codec,
    ControlMessage, ControlResponse, DesktopCaptureHealthStatus, DirectCallStatus,
    Esp32AudioConfig, IfbConfig, OpusProfile, ProcessingConfig, ProcessingMode, ProcessingProfile,
    StereoConfig, TalkButtonMode, TalkMode, MIX_SAMPLES_PER_FRAME, MIX_SAMPLE_RATE,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use serde::{Deserialize, Serialize};
use tokio::io::{self, AsyncBufReadExt};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tracing_subscriber::EnvFilter;

#[cfg(target_os = "ios")]
mod ios_voice;
#[cfg(target_os = "macos")]
mod macos_mic_mode;
#[cfg(target_os = "macos")]
mod macos_voice;

#[derive(Debug, Parser)]
pub struct Args {
    #[arg(long)]
    pub server_host: Option<String>,
    #[arg(long, default_value = "127.0.0.1:40000")]
    pub server: SocketAddr,
    #[arg(long, default_value = "ws://127.0.0.1:40001")]
    pub control: String,
    #[arg(long)]
    pub user_id: Option<u16>,
    #[arg(long)]
    pub client_uid: Option<String>,
    #[arg(long)]
    pub identity_file: Option<PathBuf>,
    #[arg(long, default_value_t = 0)]
    pub tx_channel: u16,
    #[arg(long, default_value_t = 0)]
    pub listen_channel: u16,
    #[arg(long, value_enum, default_value_t = WireCodec::Pcm16)]
    pub codec: WireCodec,
    #[arg(long, value_enum, default_value_t = WireOpusProfile::Speech24Standard)]
    pub opus_profile: WireOpusProfile,
    #[arg(long, default_value_t = 1.0)]
    pub mic_gain: f32,
    #[arg(long)]
    pub input_limiter: bool,
    #[arg(long)]
    pub disable_input_transient_suppression: bool,
    #[arg(long, default_value_t = 1.0)]
    pub speaker_gain: f32,
    #[arg(long, default_value_t = 40, value_parser = clap::value_parser!(u32).range(0..=250))]
    pub jitter_ms: u32,
    #[arg(long)]
    pub input_device: Option<String>,
    #[arg(long, value_enum, default_value_t = AudioInputBackend::Auto)]
    pub input_backend: AudioInputBackend,
    #[arg(long, value_enum, default_value_t = InputChannelMode::Average)]
    pub input_channel: InputChannelMode,
    #[arg(long)]
    pub output_device: Option<String>,
    #[arg(long)]
    pub debug_audio_dir: Option<PathBuf>,
    #[arg(long, default_value_t = DEFAULT_CLIENT_BUTTON_COUNT, value_parser = clap::value_parser!(u16).range(0..=MAX_CLIENT_BUTTON_COUNT as i64))]
    pub button_count: u16,
    #[arg(long = "button", value_name = "ID[=LABEL]")]
    pub buttons: Vec<ButtonArg>,
    #[arg(long = "button-key", value_name = "ID=KEY")]
    pub button_keys: Vec<ButtonKeyArg>,
    #[arg(long, default_value = "127.0.0.1:41002")]
    pub local_ui_bind: SocketAddr,
    #[arg(long, env = "INTERCOM_LOCAL_UI_TOKEN")]
    pub local_ui_token: Option<String>,
    #[arg(long)]
    pub disable_local_ui: bool,
    #[arg(long, hide = true)]
    pub disable_command_loop: bool,
    #[arg(long)]
    pub list_devices: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum WireCodec {
    Pcm16,
    Pcm24,
    Pcm48,
    Opus,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum WireOpusProfile {
    #[value(name = "speech-16-low", alias = "speech-low")]
    Speech16Low,
    #[value(name = "speech-24-standard", alias = "speech-standard")]
    Speech24Standard,
    #[value(name = "speech-48-high", alias = "speech-high")]
    Speech48High,
    #[value(name = "music-48", alias = "music-high")]
    Music48,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AudioInputBackend {
    #[default]
    Auto,
    Raw,
    VoiceProcessing,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DesktopCpalAudioBackend;

impl ClientAudioBackend for DesktopCpalAudioBackend {
    fn kind(&self) -> ClientAudioBackendKind {
        ClientAudioBackendKind::Raw
    }

    fn prepare(&self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MacosVoiceProcessingAudioBackend;

impl ClientAudioBackend for MacosVoiceProcessingAudioBackend {
    fn kind(&self) -> ClientAudioBackendKind {
        ClientAudioBackendKind::VoiceProcessing
    }

    fn prepare(&self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum InputChannelMode {
    #[default]
    Average,
    Left,
    Right,
}

impl InputChannelMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Average => "average",
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    fn select(self, samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        match self {
            Self::Average => samples.iter().sum::<f32>() / samples.len() as f32,
            Self::Left => samples[0],
            Self::Right => samples.get(1).copied().unwrap_or(samples[0]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InputBackendStatus {
    requested: AudioInputBackend,
    active: Option<AudioInputBackend>,
    note: Option<String>,
}

impl InputBackendStatus {
    fn starting(requested: AudioInputBackend) -> Self {
        Self {
            requested,
            active: None,
            note: Some("input backend is starting".to_string()),
        }
    }

    fn active(
        requested: AudioInputBackend,
        active: AudioInputBackend,
        note: Option<String>,
    ) -> Self {
        Self {
            requested,
            active: Some(active),
            note,
        }
    }

    fn to_client_core(&self) -> InputBackendState {
        InputBackendState {
            requested: self.requested.into(),
            active: self.active.map(Into::into),
            note: self.note.clone(),
        }
    }
}

impl From<AudioInputBackend> for ClientAudioBackendKind {
    fn from(value: AudioInputBackend) -> Self {
        match value {
            AudioInputBackend::Auto => Self::Auto,
            AudioInputBackend::Raw => Self::Raw,
            AudioInputBackend::VoiceProcessing => Self::VoiceProcessing,
        }
    }
}

impl From<WireOpusProfile> for OpusProfile {
    fn from(value: WireOpusProfile) -> Self {
        match value {
            WireOpusProfile::Speech16Low => Self::Speech16Low,
            WireOpusProfile::Speech24Standard => Self::Speech24Standard,
            WireOpusProfile::Speech48High => Self::Speech48High,
            WireOpusProfile::Music48 => Self::Music48,
        }
    }
}

impl From<WireCodec> for Codec {
    fn from(value: WireCodec) -> Self {
        match value {
            WireCodec::Pcm16 => Self::Pcm16,
            WireCodec::Pcm24 => Self::Pcm24,
            WireCodec::Pcm48 => Self::Pcm48,
            WireCodec::Opus => Self::Opus,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct LocalHttpAuth {
    token: Option<Arc<str>>,
    realm: &'static str,
}

impl LocalHttpAuth {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn token(token: impl Into<String>, realm: &'static str) -> Self {
        let token = token.into();
        Self {
            token: (!token.is_empty()).then(|| Arc::<str>::from(token)),
            realm,
        }
    }

    fn is_enabled(&self) -> bool {
        self.token.is_some()
    }

    fn authorizes(&self, headers: &HeaderMap) -> bool {
        let Some(token) = self.token.as_deref() else {
            return true;
        };
        authorization_matches(headers, token)
    }
}

fn authorization_matches(headers: &HeaderMap, token: &str) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };

    if value
        .strip_prefix("Bearer ")
        .is_some_and(|provided| provided == token)
    {
        return true;
    }

    let Some(encoded) = value.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(decoded) = BASE64_STANDARD.decode(encoded) else {
        return false;
    };
    let Ok(decoded) = String::from_utf8(decoded) else {
        return false;
    };
    decoded
        .split_once(':')
        .is_some_and(|(_, password)| password == token)
}

async fn require_local_auth(
    State(auth): State<LocalHttpAuth>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    if auth.authorizes(&headers) {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                format!("Basic realm=\"{}\", charset=\"UTF-8\"", auth.realm),
            )],
            Json(ErrorResponse {
                error: "authentication required".to_string(),
            }),
        )
            .into_response()
    }
}

#[derive(Debug, Clone)]
pub struct ButtonArg {
    id: ButtonId,
    label: String,
}

impl FromStr for ButtonArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (id, label) = value
            .split_once('=')
            .map_or((value, value), |(id, label)| (id, label));
        let id = id.trim();
        if id.is_empty() {
            return Err("button id cannot be empty".to_string());
        }
        let label = if label.trim().is_empty() {
            id
        } else {
            label.trim()
        };
        Ok(Self {
            id: id.to_string(),
            label: label.to_string(),
        })
    }
}

impl From<ButtonArg> for ButtonCapability {
    fn from(value: ButtonArg) -> Self {
        Self {
            id: value.id,
            label: value.label,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ButtonKeyArg {
    button_id: ButtonId,
    key: char,
}

impl FromStr for ButtonKeyArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (button_id, key) = value
            .split_once('=')
            .ok_or_else(|| "button key must be ID=KEY".to_string())?;
        let button_id = button_id.trim();
        if button_id.is_empty() {
            return Err("button id cannot be empty".to_string());
        }
        let mut chars = key.trim().chars();
        let key = chars
            .next()
            .ok_or_else(|| "hotkey cannot be empty".to_string())?;
        if chars.next().is_some() {
            return Err("hotkey must be a single character".to_string());
        }
        Ok(Self {
            button_id: button_id.to_string(),
            key,
        })
    }
}

pub fn init_tracing() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("desktop=info".parse()?))
        .init();
    Ok(())
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    run_until_shutdown(args, future::pending::<()>()).await
}

pub async fn run_until_shutdown(
    args: Args,
    shutdown: impl Future<Output = ()> + Send,
) -> anyhow::Result<()> {
    run_until_shutdown_with_local_api(args, shutdown, None).await
}

pub async fn run_until_shutdown_with_local_api(
    mut args: Args,
    shutdown: impl Future<Output = ()> + Send,
    local_api_tx: Option<std::sync::mpsc::Sender<LocalClientApi>>,
) -> anyhow::Result<()> {
    resolve_endpoint_args(&mut args)?;
    if args.list_devices {
        list_audio_devices()?;
        return Ok(());
    }
    tokio::pin!(shutdown);

    let user_id = args.user_id.unwrap_or(0);
    let client_uid =
        load_or_create_client_uid(args.client_uid.as_deref(), args.identity_file.as_deref())?;
    let control_url = args.control.clone();
    let advertised_buttons = merge_button_capabilities(
        default_button_capabilities(args.button_count),
        args.buttons
            .clone()
            .into_iter()
            .map(ButtonCapability::from)
            .collect(),
    );
    let button_keys = args.button_keys.clone();
    let audio_settings = Arc::new(AudioSettings::new(args.mic_gain, args.speaker_gain));
    let input_backend_status =
        Arc::new(Mutex::new(InputBackendStatus::starting(args.input_backend)));
    let capture_diagnostics = CaptureDiagnostics::new(args.input_channel);
    let telemetry_counters = ClientTelemetryCounters::default();
    let latest_telemetry = Arc::new(Mutex::new(None));
    let debug_audio_tap = args
        .debug_audio_dir
        .clone()
        .map(spawn_desktop_debug_audio_writer)
        .transpose()?;
    let capture_processing_settings =
        Arc::new(CaptureProcessingSettings::new(&ProcessingConfig::default()));
    let capture_options = CapturePipelineOptions {
        channel_mode: args.input_channel,
        input_limiter: args.input_limiter,
        input_transient_suppression: !args.disable_input_transient_suppression,
        input_silence_gate: true,
        processing_settings: Arc::clone(&capture_processing_settings),
        diagnostics: Some(capture_diagnostics.clone()),
        telemetry: Some(telemetry_counters.clone()),
        debug_audio_tap: debug_audio_tap.clone(),
    };
    let runtime_config = Arc::new(Mutex::new(ClientConfig {
        user_id,
        client_uid,
        role: ClientRole::Client,
        listen: vec![args.listen_channel],
        tx: vec![args.tx_channel],
        codec: Codec::from(args.codec),
        opus_profile: OpusProfile::from(args.opus_profile),
        name: String::new(),
        talk_mode: TalkMode::Ptt,
        last_non_muted_talk_mode: TalkMode::Ptt,
        regular_talk_active: false,
        priority: false,
        priority_channels: Vec::new(),
        processing: ProcessingConfig::default(),
        channel_rosters: Vec::new(),
        emergency: None,
        vol: HashMap::new(),
        talker_vol: HashMap::new(),
        buttons: Vec::new(),
        active_buttons: Vec::new(),
        active_direct_calls: Vec::new(),
        last_direct_caller: None,
        direct_call_history: Vec::new(),
        active_alerts: Vec::new(),
        recent_alerts: Vec::new(),
        advertised_buttons: advertised_buttons.clone(),
        ifb: IfbConfig::default(),
        lockout: ClientLockoutPolicy::default(),
        stereo: StereoConfig::default(),
        esp32_audio: Esp32AudioConfig::default(),
    }));
    capture_processing_settings.store(&runtime_config.lock().unwrap().processing);
    let jitter_samples = samples_for_ms(args.jitter_ms);
    let playback = Arc::new(Mutex::new(PlaybackBuffer::new(
        jitter_samples + MIX_SAMPLES_PER_FRAME * 12,
        jitter_samples,
    )));
    let (control_tx, control_rx) = mpsc::channel::<ControlRequest>(16);
    let connection_status = Arc::new(Mutex::new(ClientConnectionEvent::Reconnecting));
    let local_api = LocalClientApi {
        config: Arc::clone(&runtime_config),
        control_tx: control_tx.clone(),
        audio_settings: Arc::clone(&audio_settings),
        input_backend_status: Arc::clone(&input_backend_status),
        playback: Arc::clone(&playback),
        latest_telemetry: Arc::clone(&latest_telemetry),
        connection_status: Arc::clone(&connection_status),
    };
    if let Some(local_api_tx) = local_api_tx {
        let _ = local_api_tx.send(local_api.clone());
    }
    let output_stream = build_output_stream(
        Arc::clone(&playback),
        Arc::clone(&audio_settings),
        args.output_device.as_deref(),
    )?;
    output_stream.play()?;
    let (connection_event_tx, connection_event_rx) = mpsc::channel::<ClientConnectionEvent>(8);
    let (connection_cue_tx, connection_cue_rx) = mpsc::channel::<ClientConnectionEvent>(8);
    let control_config = Arc::clone(&runtime_config);
    let cue_playback = Arc::clone(&playback);
    tokio::spawn(run_connection_cue_task(cue_playback, connection_cue_rx));
    {
        let connection_status = Arc::clone(&connection_status);
        tokio::spawn(async move {
            let mut connection_event_rx = connection_event_rx;
            while let Some(event) = connection_event_rx.recv().await {
                *connection_status.lock().unwrap() = event;
                let _ = connection_cue_tx.send(event).await;
            }
        });
    }
    let control_task = tokio::spawn(async move {
        run_control_connection(
            control_url,
            control_rx,
            control_config,
            Some(connection_event_tx),
        )
        .await
    });
    {
        let config = Arc::clone(&runtime_config);
        let settings = Arc::clone(&capture_processing_settings);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            loop {
                interval.tick().await;
                let processing = config.lock().unwrap().processing.clone();
                settings.store(&processing);
            }
        });
    }
    let capture_health_control_tx = control_tx.clone();
    let capture_health_config = Arc::clone(&runtime_config);
    let capture_health_diagnostics = capture_diagnostics.clone();
    let capture_health_playback = Arc::clone(&playback);
    let capture_health_telemetry = telemetry_counters.clone();
    let capture_health_latest = Arc::clone(&latest_telemetry);
    let telemetry_started_at = Instant::now();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            interval.tick().await;
            let mut health = capture_health_with_client_telemetry(
                capture_health_diagnostics.snapshot(),
                "desktop",
                capture_health_playback.lock().unwrap().stats(),
                capture_health_telemetry.snapshot(),
                "running",
                None,
            );
            health.uptime_ms = telemetry_started_at
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX);
            *capture_health_latest.lock().unwrap() = Some(health.clone());
            let user_id = capture_health_config.lock().unwrap().user_id;
            if user_id == 0 {
                continue;
            }
            if let Err(err) = queue_control_message(
                &capture_health_control_tx,
                ControlMessage::CaptureHealth { user_id, health },
            )
            .await
            {
                tracing::debug!(%err, "could not report desktop capture health");
            }
        }
    });
    let local_ui_task = if args.disable_local_ui {
        tokio::spawn(future::pending::<anyhow::Result<()>>())
    } else {
        let local_ui_auth = args
            .local_ui_token
            .clone()
            .map_or_else(LocalHttpAuth::disabled, |token| {
                LocalHttpAuth::token(token, "RedLine Client")
            });
        tokio::spawn(run_local_ui(args.local_ui_bind, local_api, local_ui_auth))
    };

    let initial_config = runtime_config.lock().unwrap().clone();
    let hello_response = send_control_request(
        &control_tx,
        ControlMessage::Hello {
            user_id: initial_config.user_id,
            requested_user_id: (initial_config.user_id > 0).then_some(initial_config.user_id),
            client_uid: initial_config.client_uid.clone(),
            codecs: supported_codecs(),
            buttons: advertised_buttons.clone(),
            role: ClientRole::Client,
        },
    )
    .await?;
    let preconfigured = match hello_response {
        ControlResponse::Hello {
            preconfigured,
            user_id,
            client_uid,
            enrollment,
        } => {
            if enrollment != common::EnrollmentStatus::Enrolled {
                bail!("client enrollment is {enrollment:?}; waiting for admin approval");
            }
            let mut config = runtime_config.lock().unwrap();
            config.user_id = user_id;
            if !client_uid.is_empty() {
                config.client_uid = client_uid;
            }
            preconfigured
        }
        ControlResponse::Ack => false,
        ControlResponse::Error { message } => bail!("{message}"),
        other => bail!("unexpected hello response: {other:?}"),
    };
    if !preconfigured {
        let startup_config = runtime_config.lock().unwrap().clone();
        queue_control_message(&control_tx, startup_config.control_message()).await?;
    } else {
        tracing::info!("server has preconfigured state; waiting for config_update");
    }

    let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    socket.connect(args.server).await?;
    tracing::info!(local = %socket.local_addr()?, server = %args.server, "desktop client connected");

    let register_socket = Arc::clone(&socket);
    let register_config = Arc::clone(&runtime_config);
    let registration_task = tokio::spawn(async move {
        let seq = AtomicU16::new(0);
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        let mut encoded = Vec::with_capacity(common::HEADER_LEN);
        let mut send_error_logged = false;

        loop {
            interval.tick().await;
            let (user_id, codec) = {
                let config = register_config.lock().unwrap();
                (config.user_id, config.codec)
            };
            let packet =
                AudioPacket::registration(user_id, codec, seq.fetch_add(1, Ordering::Relaxed));
            if let Err(err) = packet.encode(&mut encoded) {
                tracing::warn!(%err, "failed to encode audio registration packet");
                continue;
            }
            match register_socket.send(&encoded).await {
                Ok(_) => {
                    if send_error_logged {
                        tracing::info!("audio UDP registration recovered");
                        send_error_logged = false;
                    }
                }
                Err(err) => {
                    if !send_error_logged {
                        tracing::warn!(%err, "audio UDP registration failed; keeping client alive");
                        send_error_logged = true;
                    }
                }
            }
        }

        #[allow(unreachable_code)]
        anyhow::Ok(())
    });

    let (mic_tx, mut mic_rx) = mpsc::channel::<Vec<i16>>(8);

    let (mut input_stream, active_input_backend_status) = build_input_stream(
        mic_tx,
        Arc::clone(&audio_settings),
        args.input_device.as_deref(),
        args.input_backend,
        capture_options,
    )?;
    *input_backend_status.lock().unwrap() = active_input_backend_status;
    input_stream.play()?;

    let send_socket = Arc::clone(&socket);
    let send_config = Arc::clone(&runtime_config);
    let send_telemetry = telemetry_counters.clone();
    let send_task = tokio::spawn(async move {
        let seq = AtomicU16::new(0);
        let timestamp = AtomicU32::new(0);
        let mut encoded = Vec::with_capacity(common::HEADER_LEN + common::MAX_PAYLOAD_LEN);
        let (mut current_codec, mut current_opus_profile) = {
            let config = send_config.lock().unwrap();
            (config.codec, config.opus_profile)
        };
        let mut audio_encoder = AudioEncoder::new(current_codec, current_opus_profile)?;
        let mut send_error_logged = false;

        while let Some(frame) = mic_rx.recv().await {
            let Some(snapshot) = send_config.lock().unwrap().active_transmit_snapshot() else {
                continue;
            };
            let user_id = snapshot.user_id;
            let tx_targets = snapshot.targets;
            let codec = snapshot.codec;
            let opus_profile = snapshot.opus_profile;

            if codec != current_codec || opus_profile != current_opus_profile {
                match AudioEncoder::new(codec, opus_profile) {
                    Ok(encoder) => {
                        audio_encoder = encoder;
                        current_codec = codec;
                        current_opus_profile = opus_profile;
                        tracing::info!(
                            codec = ?current_codec,
                            opus_profile = ?current_opus_profile,
                            "switched transmit codec"
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            codec = ?codec,
                            opus_profile = ?opus_profile,
                            %err,
                            "could not switch transmit codec"
                        );
                        continue;
                    }
                }
            }

            let payload = match audio_encoder.encode(&frame) {
                Ok(payload) => payload,
                Err(err) => {
                    send_telemetry.record_codec_drop();
                    tracing::warn!(codec = ?current_codec, %err, "failed to encode mic frame");
                    continue;
                }
            };
            let timestamp = timestamp.fetch_add(
                codec_samples_per_frame(current_codec) as u32,
                Ordering::Relaxed,
            );
            for target in tx_targets {
                let packet = AudioPacket {
                    user_id,
                    target,
                    codec: current_codec,
                    seq: seq.fetch_add(1, Ordering::Relaxed),
                    timestamp,
                    payload: payload.clone(),
                };
                if let Err(err) = packet.encode(&mut encoded) {
                    send_telemetry.record_packet_encode_error();
                    tracing::warn!(%err, "failed to encode audio packet");
                    continue;
                }
                match send_socket.send(&encoded).await {
                    Ok(_) => {
                        send_telemetry.record_tx_packet();
                        if send_error_logged {
                            tracing::info!("audio UDP send recovered");
                            send_error_logged = false;
                        }
                    }
                    Err(err) => {
                        send_telemetry.record_tx_send_failure();
                        if !send_error_logged {
                            tracing::warn!(%err, "audio UDP send failed; keeping client alive");
                            send_error_logged = true;
                        }
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        break;
                    }
                }
            }
        }

        anyhow::Ok(())
    });

    let command_config = Arc::clone(&runtime_config);
    let command_control_tx = control_tx.clone();
    let command_task = if args.disable_command_loop {
        tokio::spawn(future::pending::<anyhow::Result<()>>())
    } else if button_keys.is_empty() {
        tokio::spawn(async move { run_command_loop(command_control_tx, command_config).await })
    } else {
        let hotkey_config = Arc::clone(&runtime_config);
        let hotkey_control_tx = control_tx.clone();
        tokio::spawn(
            async move { run_hotkey_loop(button_keys, hotkey_control_tx, hotkey_config).await },
        )
    };

    let recv_socket = Arc::clone(&socket);
    let recv_playback = Arc::clone(&playback);
    let recv_config = Arc::clone(&runtime_config);
    let recv_telemetry = telemetry_counters.clone();
    let recv_task = tokio::spawn(async move {
        let mut buf = vec![0_u8; common::MAX_PACKET_BYTES];
        let mut audio_decoder = AudioDecoder::default();
        let mut recv_error_logged = false;
        loop {
            let len = match recv_socket.recv(&mut buf).await {
                Ok(len) => {
                    if recv_error_logged {
                        tracing::info!("audio UDP receive recovered");
                        recv_error_logged = false;
                    }
                    len
                }
                Err(err) => {
                    if !recv_error_logged {
                        tracing::warn!(%err, "audio UDP receive failed; keeping client alive");
                        recv_error_logged = true;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
            };
            let packet = match AudioPacket::decode(&buf[..len]) {
                Ok(packet) => {
                    recv_telemetry.record_udp_rx_packet();
                    packet
                }
                Err(err) => {
                    recv_telemetry.record_malformed_packet();
                    tracing::warn!(%err, "dropped malformed incoming packet");
                    continue;
                }
            };
            let (receive_channels, opus_profile) = {
                let config = recv_config.lock().unwrap();
                let channels = if config.stereo.active_for_codec(config.codec)
                    && matches!(packet.codec, Codec::Pcm48 | Codec::Opus)
                {
                    2
                } else {
                    1
                };
                (channels, config.opus_profile)
            };
            let samples = match audio_decoder.decode_with_channels(
                packet.codec,
                opus_profile,
                &packet.payload,
                receive_channels,
            ) {
                Ok(samples) => samples,
                Err(err) => {
                    recv_telemetry.record_decode_error();
                    tracing::warn!(codec = ?packet.codec, %err, "dropped invalid audio payload");
                    continue;
                }
            };
            recv_playback
                .lock()
                .unwrap()
                .push_frame(&samples, receive_channels);
        }

        #[allow(unreachable_code)]
        anyhow::Ok(())
    });

    tokio::select! {
        result = send_task => result.context("send task panicked")??,
        result = recv_task => result.context("receive task panicked")??,
        result = registration_task => result.context("registration task panicked")??,
        result = control_task => result.context("control task panicked")??,
        result = command_task => result.context("command task panicked")??,
        result = local_ui_task => result.context("local UI task panicked")??,
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
        _ = &mut shutdown => tracing::info!("shutting down from native app"),
    }

    Ok(())
}

fn resolve_endpoint_args(args: &mut Args) -> anyhow::Result<()> {
    let Some(server_host) = args.server_host.as_deref() else {
        return Ok(());
    };
    let endpoint = ClientServerEndpoint::new(server_host)?;
    let default_control = format!("ws://{DEFAULT_SERVER_HOST}:{DEFAULT_CONTROL_PORT}");
    if args.server == client_core::default_audio_addr() {
        args.server = endpoint.resolve_audio_addr()?;
    }
    if args.control == default_control {
        args.control = endpoint.control_url();
    }
    Ok(())
}

#[derive(Clone)]
pub struct LocalClientApi {
    config: Arc<Mutex<ClientConfig>>,
    control_tx: mpsc::Sender<ControlRequest>,
    audio_settings: Arc<AudioSettings>,
    input_backend_status: Arc<Mutex<InputBackendStatus>>,
    playback: Arc<Mutex<PlaybackBuffer>>,
    latest_telemetry: Arc<Mutex<Option<CaptureHealthStatus>>>,
    connection_status: Arc<Mutex<ClientConnectionEvent>>,
}

type ApiState = LocalClientApi;

#[derive(Debug, Serialize, PartialEq)]
pub struct ErrorResponse {
    error: String,
}

#[cfg(target_os = "macos")]
fn macos_microphone_mode_status() -> Option<MacosMicrophoneModeStatus> {
    macos_mic_mode::status()
}

#[cfg(not(target_os = "macos"))]
fn macos_microphone_mode_status() -> Option<MacosMicrophoneModeStatus> {
    None
}

#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    Unavailable(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadRequest(error) | Self::Unavailable(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            Self::BadRequest(error) => (StatusCode::BAD_REQUEST, error),
            Self::Unavailable(error) => (StatusCode::SERVICE_UNAVAILABLE, error),
        };
        (status, Json(ErrorResponse { error })).into_response()
    }
}

impl LocalClientApi {
    pub fn state(&self) -> StateResponse {
        let snapshot = self.config.lock().unwrap().clone();
        let playback = self.playback.lock().unwrap().stats();
        let telemetry = self.latest_telemetry.lock().unwrap().clone();
        let input_backend_status = self.input_backend_status.lock().unwrap().clone();
        let server_connection = *self.connection_status.lock().unwrap();
        StateResponse::from_runtime_state(
            &snapshot,
            &self.audio_settings,
            input_backend_status.to_client_core(),
            macos_microphone_mode_status(),
            playback,
            telemetry,
            server_connection,
        )
    }

    pub async fn apply_config(&self, request: FullConfigRequest) -> Result<OkResponse, ApiError> {
        let snapshot = self.config.lock().unwrap().clone();
        ensure_local_api_allowed(&snapshot)?;
        validate_full_config_allowed(&snapshot, &request)?;
        let user_id = snapshot.user_id;
        for message in request.control_messages(user_id) {
            forward_control_message(&self.control_tx, message).await?;
        }
        Ok(OkResponse { ok: true })
    }

    pub async fn set_talk_mode(&self, mode: TalkMode) -> Result<OkResponse, ApiError> {
        ensure_control_allowed(self, |lockout| lockout.allow_talk_mode, "talk mode")?;
        send_talk_mode(self, mode).await
    }

    pub async fn mute(&self) -> Result<OkResponse, ApiError> {
        ensure_control_allowed(self, |lockout| lockout.allow_talk_mode, "talk mode")?;
        send_talk_mode(self, TalkMode::Muted).await
    }

    pub async fn unmute(&self) -> Result<OkResponse, ApiError> {
        let snapshot = self.config.lock().unwrap().clone();
        ensure_local_api_allowed(&snapshot)?;
        ensure_policy_allowed(
            &snapshot.lockout,
            |lockout| lockout.allow_talk_mode,
            "talk mode",
        )?;
        let talk_mode = snapshot.restored_unmute_talk_mode();
        send_talk_mode(self, talk_mode).await
    }

    pub async fn talk_down(&self) -> Result<OkResponse, ApiError> {
        ensure_control_allowed(self, |_| true, "talk")?;
        send_regular_talk(self, true).await
    }

    pub async fn talk_up(&self) -> Result<OkResponse, ApiError> {
        ensure_control_allowed(self, |_| true, "talk")?;
        send_regular_talk(self, false).await
    }

    pub async fn talk_toggle(&self) -> Result<OkResponse, ApiError> {
        let snapshot = self.config.lock().unwrap().clone();
        ensure_local_api_allowed(&snapshot)?;
        let active = !snapshot.regular_talk_active;
        send_regular_talk(self, active).await
    }

    pub async fn set_codec(&self, codec: Codec) -> Result<OkResponse, ApiError> {
        let snapshot = self.config.lock().unwrap().clone();
        ensure_local_api_allowed(&snapshot)?;
        ensure_policy_allowed(&snapshot.lockout, |lockout| lockout.allow_codec, "codec")?;
        forward_control_message(
            &self.control_tx,
            ControlMessage::AudioCodec {
                user_id: snapshot.user_id,
                codec,
            },
        )
        .await?;
        Ok(OkResponse { ok: true })
    }

    pub fn set_gain(&self, request: GainRequest) -> Result<OkResponse, ApiError> {
        let snapshot = self.config.lock().unwrap().clone();
        ensure_local_api_allowed(&snapshot)?;
        if let Some(gain) = request.mic_gain {
            self.audio_settings.set_mic_gain(gain);
        }
        if let Some(gain) = request.speaker_gain {
            self.audio_settings.set_speaker_gain(gain);
        }
        Ok(OkResponse { ok: true })
    }

    pub fn queue_set_talk_mode(&self, mode: TalkMode) -> Result<OkResponse, ApiError> {
        let user_id = {
            let mut snapshot = self.config.lock().unwrap();
            ensure_local_api_allowed(&snapshot)?;
            ensure_policy_allowed(
                &snapshot.lockout,
                |lockout| lockout.allow_talk_mode,
                "talk mode",
            )?;
            snapshot.set_talk_mode(mode);
            if mode == TalkMode::Muted {
                snapshot.regular_talk_active = false;
            }
            snapshot.user_id
        };
        self.try_queue_control_message(ControlMessage::TalkMode { user_id, mode })
    }

    pub fn queue_mute(&self) -> Result<OkResponse, ApiError> {
        self.queue_set_talk_mode(TalkMode::Muted)
    }

    pub fn queue_unmute(&self) -> Result<OkResponse, ApiError> {
        let (user_id, mode) = {
            let mut snapshot = self.config.lock().unwrap();
            ensure_local_api_allowed(&snapshot)?;
            ensure_policy_allowed(
                &snapshot.lockout,
                |lockout| lockout.allow_talk_mode,
                "talk mode",
            )?;
            let mode = snapshot.restored_unmute_talk_mode();
            snapshot.set_talk_mode(mode);
            (snapshot.user_id, mode)
        };
        self.try_queue_control_message(ControlMessage::TalkMode { user_id, mode })
    }

    pub fn queue_talk(&self, active: bool) -> Result<OkResponse, ApiError> {
        let user_id = {
            let mut snapshot = self.config.lock().unwrap();
            ensure_local_api_allowed(&snapshot)?;
            snapshot.regular_talk_active = active;
            snapshot.user_id
        };
        self.try_queue_control_message(ControlMessage::Talk { user_id, active })
    }

    pub fn queue_talk_toggle(&self) -> Result<OkResponse, ApiError> {
        let active = {
            let snapshot = self.config.lock().unwrap();
            ensure_local_api_allowed(&snapshot)?;
            !snapshot.regular_talk_active
        };
        self.queue_talk(active)
    }

    pub fn queue_set_codec(&self, codec: Codec) -> Result<OkResponse, ApiError> {
        let user_id = {
            let mut snapshot = self.config.lock().unwrap();
            ensure_local_api_allowed(&snapshot)?;
            ensure_policy_allowed(&snapshot.lockout, |lockout| lockout.allow_codec, "codec")?;
            snapshot.codec = codec;
            snapshot.user_id
        };
        self.try_queue_control_message(ControlMessage::AudioCodec { user_id, codec })
    }

    pub fn queue_button(&self, id: ButtonId, pressed: bool) -> Result<OkResponse, ApiError> {
        let (user_id, button_id) = {
            let mut snapshot = self.config.lock().unwrap();
            ensure_local_api_allowed(&snapshot)?;
            ensure_policy_allowed(
                &snapshot.lockout,
                |lockout| lockout.allow_buttons,
                "buttons",
            )?;
            let Some(button) = snapshot.buttons.iter().find(|button| button.id == id) else {
                return Err(ApiError::BadRequest(format!("unknown button `{id}`")));
            };
            let was_active = snapshot.active_buttons.contains(&id);
            let now_active = match button.mode {
                TalkButtonMode::Momentary => pressed,
                TalkButtonMode::Latching => {
                    if pressed {
                        !was_active
                    } else {
                        was_active
                    }
                }
            };
            set_active_button(&mut snapshot.active_buttons, &id, now_active);
            (snapshot.user_id, id)
        };
        self.try_queue_control_message(ControlMessage::Button {
            user_id,
            button_id,
            pressed,
        })
    }

    pub fn queue_direct_call(
        &self,
        target_user_id: u16,
        active: bool,
        duck: bool,
    ) -> Result<OkResponse, ApiError> {
        let user_id = {
            let mut snapshot = self.config.lock().unwrap();
            ensure_local_api_allowed(&snapshot)?;
            set_active_direct_call(&mut snapshot, target_user_id, active, duck);
            snapshot.user_id
        };
        self.try_queue_control_message(ControlMessage::DirectCall {
            user_id,
            target_user_id,
            active,
            duck,
        })
    }

    pub fn queue_direct_call_toggle(&self, target_user_id: u16) -> Result<OkResponse, ApiError> {
        let active = {
            let snapshot = self.config.lock().unwrap();
            ensure_local_api_allowed(&snapshot)?;
            !snapshot.active_direct_calls.iter().any(|call| {
                call.caller == snapshot.user_id && call.target == target_user_id && call.active
            })
        };
        self.queue_direct_call(target_user_id, active, false)
    }

    pub fn queue_reply_call(&self, active: bool, duck: bool) -> Result<OkResponse, ApiError> {
        let user_id = {
            let mut snapshot = self.config.lock().unwrap();
            ensure_local_api_allowed(&snapshot)?;
            let Some(target_user_id) = snapshot.last_direct_caller else {
                return Err(ApiError::BadRequest(
                    "no last direct caller to reply to".to_string(),
                ));
            };
            set_active_direct_call(&mut snapshot, target_user_id, active, duck);
            snapshot.user_id
        };
        self.try_queue_control_message(ControlMessage::ReplyCall {
            user_id,
            active,
            duck,
        })
    }

    pub fn queue_reply_toggle(&self) -> Result<OkResponse, ApiError> {
        let active = {
            let snapshot = self.config.lock().unwrap();
            ensure_local_api_allowed(&snapshot)?;
            let Some(target) = snapshot.last_direct_caller else {
                return Err(ApiError::BadRequest(
                    "no last direct caller to reply to".to_string(),
                ));
            };
            !snapshot
                .active_direct_calls
                .iter()
                .any(|call| call.caller == snapshot.user_id && call.target == target && call.active)
        };
        self.queue_reply_call(active, false)
    }

    pub fn queue_send_alert(&self, request: AlertRequest) -> Result<OkResponse, ApiError> {
        let user_id = {
            let snapshot = self.config.lock().unwrap();
            ensure_local_api_allowed(&snapshot)?;
            snapshot.user_id
        };
        self.try_queue_control_message(ControlMessage::SendAlert {
            user_id,
            target: request.target,
            message: request.message,
        })
    }

    pub fn queue_ack_alert(&self, alert_id: AlertId) -> Result<OkResponse, ApiError> {
        let user_id = {
            let mut snapshot = self.config.lock().unwrap();
            ensure_local_api_allowed(&snapshot)?;
            snapshot.active_alerts.retain(|alert| alert.id != alert_id);
            snapshot.user_id
        };
        self.try_queue_control_message(ControlMessage::AckAlert { user_id, alert_id })
    }

    pub fn queue_cancel_alert(&self, alert_id: AlertId) -> Result<OkResponse, ApiError> {
        let user_id = {
            let mut snapshot = self.config.lock().unwrap();
            ensure_local_api_allowed(&snapshot)?;
            snapshot.active_alerts.retain(|alert| alert.id != alert_id);
            snapshot.user_id
        };
        self.try_queue_control_message(ControlMessage::CancelAlert { user_id, alert_id })
    }

    fn try_queue_control_message(&self, message: ControlMessage) -> Result<OkResponse, ApiError> {
        let (response_tx, _response_rx) = oneshot::channel();
        self.control_tx
            .try_send(ControlRequest {
                message,
                response_tx,
            })
            .map_err(|err| match err {
                mpsc::error::TrySendError::Full(_) => {
                    ApiError::Unavailable("control command queue is full".to_string())
                }
                mpsc::error::TrySendError::Closed(_) => {
                    ApiError::Unavailable("control connection is not available".to_string())
                }
            })?;
        Ok(OkResponse { ok: true })
    }

    pub async fn button_down(&self, id: ButtonId) -> Result<OkResponse, ApiError> {
        send_button(self, id, true).await
    }

    pub async fn button_up(&self, id: ButtonId) -> Result<OkResponse, ApiError> {
        send_button(self, id, false).await
    }

    pub async fn button_toggle(&self, id: ButtonId) -> Result<OkResponse, ApiError> {
        send_button(self, id, true).await
    }

    pub async fn call_down(&self, target_user_id: u16) -> Result<OkResponse, ApiError> {
        send_direct_call(self, target_user_id, true, false).await
    }

    pub async fn call_up(&self, target_user_id: u16) -> Result<OkResponse, ApiError> {
        send_direct_call(self, target_user_id, false, false).await
    }

    pub async fn call_toggle(&self, target_user_id: u16) -> Result<OkResponse, ApiError> {
        let snapshot = self.config.lock().unwrap().clone();
        ensure_local_api_allowed(&snapshot)?;
        let active = !snapshot.active_direct_calls.iter().any(|call| {
            call.caller == snapshot.user_id && call.target == target_user_id && call.active
        });
        send_direct_call(self, target_user_id, active, false).await
    }

    pub async fn reply_down(&self) -> Result<OkResponse, ApiError> {
        send_reply_call(self, true, false).await
    }

    pub async fn reply_up(&self) -> Result<OkResponse, ApiError> {
        send_reply_call(self, false, false).await
    }

    pub async fn reply_toggle(&self) -> Result<OkResponse, ApiError> {
        let snapshot = self.config.lock().unwrap().clone();
        ensure_local_api_allowed(&snapshot)?;
        let Some(target) = snapshot.last_direct_caller else {
            return Err(ApiError::BadRequest(
                "no last direct caller to reply to".to_string(),
            ));
        };
        let active = !snapshot
            .active_direct_calls
            .iter()
            .any(|call| call.caller == snapshot.user_id && call.target == target && call.active);
        send_reply_call(self, active, false).await
    }

    pub async fn send_alert(&self, request: AlertRequest) -> Result<OkResponse, ApiError> {
        let snapshot = self.config.lock().unwrap().clone();
        ensure_local_api_allowed(&snapshot)?;
        forward_control_message(
            &self.control_tx,
            ControlMessage::SendAlert {
                user_id: snapshot.user_id,
                target: request.target,
                message: request.message,
            },
        )
        .await?;
        Ok(OkResponse { ok: true })
    }

    pub async fn ack_alert(&self, alert_id: AlertId) -> Result<OkResponse, ApiError> {
        let snapshot = self.config.lock().unwrap().clone();
        ensure_local_api_allowed(&snapshot)?;
        forward_control_message(
            &self.control_tx,
            ControlMessage::AckAlert {
                user_id: snapshot.user_id,
                alert_id,
            },
        )
        .await?;
        Ok(OkResponse { ok: true })
    }

    pub async fn cancel_alert(&self, alert_id: AlertId) -> Result<OkResponse, ApiError> {
        let snapshot = self.config.lock().unwrap().clone();
        ensure_local_api_allowed(&snapshot)?;
        forward_control_message(
            &self.control_tx,
            ControlMessage::CancelAlert {
                user_id: snapshot.user_id,
                alert_id,
            },
        )
        .await?;
        Ok(OkResponse { ok: true })
    }
}

impl ClientControlApi for LocalClientApi {
    fn state(&self) -> StateResponse {
        LocalClientApi::state(self)
    }

    async fn apply_config(&self, request: FullConfigRequest) -> Result<OkResponse, String> {
        LocalClientApi::apply_config(self, request)
            .await
            .map_err(|err| err.to_string())
    }

    async fn set_talk_mode(&self, mode: TalkMode) -> Result<OkResponse, String> {
        LocalClientApi::set_talk_mode(self, mode)
            .await
            .map_err(|err| err.to_string())
    }

    async fn mute(&self) -> Result<OkResponse, String> {
        LocalClientApi::mute(self)
            .await
            .map_err(|err| err.to_string())
    }

    async fn unmute(&self) -> Result<OkResponse, String> {
        LocalClientApi::unmute(self)
            .await
            .map_err(|err| err.to_string())
    }

    async fn talk_down(&self) -> Result<OkResponse, String> {
        LocalClientApi::talk_down(self)
            .await
            .map_err(|err| err.to_string())
    }

    async fn talk_up(&self) -> Result<OkResponse, String> {
        LocalClientApi::talk_up(self)
            .await
            .map_err(|err| err.to_string())
    }

    async fn talk_toggle(&self) -> Result<OkResponse, String> {
        LocalClientApi::talk_toggle(self)
            .await
            .map_err(|err| err.to_string())
    }

    async fn set_codec(&self, codec: Codec) -> Result<OkResponse, String> {
        LocalClientApi::set_codec(self, codec)
            .await
            .map_err(|err| err.to_string())
    }

    fn set_gain(&self, request: GainRequest) -> Result<OkResponse, String> {
        LocalClientApi::set_gain(self, request).map_err(|err| err.to_string())
    }

    async fn button_down(&self, id: ButtonId) -> Result<OkResponse, String> {
        LocalClientApi::button_down(self, id)
            .await
            .map_err(|err| err.to_string())
    }

    async fn button_up(&self, id: ButtonId) -> Result<OkResponse, String> {
        LocalClientApi::button_up(self, id)
            .await
            .map_err(|err| err.to_string())
    }

    async fn button_toggle(&self, id: ButtonId) -> Result<OkResponse, String> {
        LocalClientApi::button_toggle(self, id)
            .await
            .map_err(|err| err.to_string())
    }

    async fn call_down(&self, target_user_id: u16) -> Result<OkResponse, String> {
        LocalClientApi::call_down(self, target_user_id)
            .await
            .map_err(|err| err.to_string())
    }

    async fn call_up(&self, target_user_id: u16) -> Result<OkResponse, String> {
        LocalClientApi::call_up(self, target_user_id)
            .await
            .map_err(|err| err.to_string())
    }

    async fn call_toggle(&self, target_user_id: u16) -> Result<OkResponse, String> {
        LocalClientApi::call_toggle(self, target_user_id)
            .await
            .map_err(|err| err.to_string())
    }

    async fn reply_down(&self) -> Result<OkResponse, String> {
        LocalClientApi::reply_down(self)
            .await
            .map_err(|err| err.to_string())
    }

    async fn reply_up(&self) -> Result<OkResponse, String> {
        LocalClientApi::reply_up(self)
            .await
            .map_err(|err| err.to_string())
    }

    async fn reply_toggle(&self) -> Result<OkResponse, String> {
        LocalClientApi::reply_toggle(self)
            .await
            .map_err(|err| err.to_string())
    }

    async fn send_alert(&self, request: AlertRequest) -> Result<OkResponse, String> {
        LocalClientApi::send_alert(self, request)
            .await
            .map_err(|err| err.to_string())
    }

    async fn ack_alert(&self, alert_id: AlertId) -> Result<OkResponse, String> {
        LocalClientApi::ack_alert(self, alert_id)
            .await
            .map_err(|err| err.to_string())
    }

    async fn cancel_alert(&self, alert_id: AlertId) -> Result<OkResponse, String> {
        LocalClientApi::cancel_alert(self, alert_id)
            .await
            .map_err(|err| err.to_string())
    }
}

async fn run_local_ui(
    bind: SocketAddr,
    state: ApiState,
    auth: LocalHttpAuth,
) -> anyhow::Result<()> {
    let (listener, actual_bind) = bind_tcp_listener_with_port_fallback(bind).await?;
    if actual_bind != bind {
        tracing::warn!(
            requested = %bind,
            actual = %actual_bind,
            "desktop local UI bind address was in use; selected next available port"
        );
    }
    if auth.is_enabled() {
        tracing::info!(
            url = format!("http://{actual_bind}/"),
            "desktop local UI listening with HTTP authorization"
        );
    } else {
        tracing::warn!(
            url = format!("http://{actual_bind}/"),
            "desktop local UI listening without authentication"
        );
    }
    axum::serve(listener, local_ui_router(state, auth)).await?;
    Ok(())
}

fn local_ui_router(state: ApiState, auth: LocalHttpAuth) -> Router {
    Router::new()
        .route("/", get(local_ui_index))
        .route("/client-controls.js", get(local_ui_js))
        .route("/client-controls.css", get(local_ui_css))
        .route("/client-api.js", get(local_ui_api_js))
        .route("/client-api-http.js", get(local_ui_api_js))
        .route("/app.js", get(local_ui_js))
        .route("/style.css", get(local_ui_css))
        .route("/health", get(health_handler))
        .route("/state", get(state_handler))
        .route("/config", put(config_handler))
        .route("/talk-mode", post(talk_mode_handler))
        .route("/talk/down", post(talk_down_handler))
        .route("/talk/up", post(talk_up_handler))
        .route("/talk/toggle", post(talk_toggle_handler))
        .route("/mute", post(mute_handler))
        .route("/unmute", post(unmute_handler))
        .route("/codec", post(codec_handler))
        .route("/gain", post(gain_handler))
        .route("/buttons/:id/down", post(button_down_handler))
        .route("/buttons/:id/up", post(button_up_handler))
        .route("/buttons/:id/toggle", post(button_toggle_handler))
        .route("/calls/:id/down", post(call_down_handler))
        .route("/calls/:id/up", post(call_up_handler))
        .route("/calls/:id/toggle", post(call_toggle_handler))
        .route("/reply/down", post(reply_down_handler))
        .route("/reply/up", post(reply_up_handler))
        .route("/reply/toggle", post(reply_toggle_handler))
        .route("/alerts", post(alert_send_handler))
        .route("/alerts/:id/ack", post(alert_ack_handler))
        .route("/alerts/:id/cancel", post(alert_cancel_handler))
        .route(
            "/macos/microphone-modes",
            post(macos_microphone_modes_handler),
        )
        .layer(middleware::from_fn_with_state(auth, require_local_auth))
        .with_state(state)
}

async fn local_ui_index() -> Html<&'static str> {
    Html(LOCAL_UI_HTML)
}

async fn local_ui_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript")],
        LOCAL_UI_JS,
    )
}

async fn local_ui_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css")], LOCAL_UI_CSS)
}

async fn local_ui_api_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript")],
        LOCAL_UI_API_JS,
    )
}

async fn health_handler() -> Json<OkResponse> {
    Json(OkResponse { ok: true })
}

async fn macos_microphone_modes_handler() -> Result<Json<OkResponse>, ApiError> {
    #[cfg(target_os = "macos")]
    {
        macos_mic_mode::show_system_microphone_modes_ui()
            .map_err(|err| ApiError::BadRequest(err.to_string()))?;
        Ok(Json(OkResponse { ok: true }))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(ApiError::BadRequest(
            "macOS microphone modes are only available on macOS".to_string(),
        ))
    }
}

async fn state_handler(State(state): State<ApiState>) -> Json<StateResponse> {
    Json(state.state())
}

async fn config_handler(
    State(state): State<ApiState>,
    Json(request): Json<FullConfigRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    state.apply_config(request).await.map(Json)
}

async fn talk_mode_handler(
    State(state): State<ApiState>,
    Json(request): Json<TalkModeRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    state.set_talk_mode(request.mode).await.map(Json)
}

async fn mute_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    state.mute().await.map(Json)
}

async fn unmute_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    state.unmute().await.map(Json)
}

async fn talk_down_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    state.talk_down().await.map(Json)
}

async fn talk_up_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    state.talk_up().await.map(Json)
}

async fn talk_toggle_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    state.talk_toggle().await.map(Json)
}

async fn codec_handler(
    State(state): State<ApiState>,
    Json(request): Json<CodecRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    state.set_codec(request.codec).await.map(Json)
}

async fn gain_handler(
    State(state): State<ApiState>,
    Json(request): Json<GainRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    state.set_gain(request).map(Json)
}

async fn send_talk_mode(state: &ApiState, talk_mode: TalkMode) -> Result<OkResponse, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    forward_control_message(
        &state.control_tx,
        ControlMessage::TalkMode {
            user_id: snapshot.user_id,
            mode: talk_mode,
        },
    )
    .await?;
    Ok(OkResponse { ok: true })
}

async fn send_regular_talk(state: &ApiState, active: bool) -> Result<OkResponse, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    let user_id = snapshot.user_id;
    forward_control_message(&state.control_tx, ControlMessage::Talk { user_id, active }).await?;
    Ok(OkResponse { ok: true })
}

async fn button_down_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    state.button_down(id).await.map(Json)
}

async fn button_up_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    state.button_up(id).await.map(Json)
}

async fn button_toggle_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    state.button_toggle(id).await.map(Json)
}

async fn send_button(
    state: &ApiState,
    button_id: ButtonId,
    pressed: bool,
) -> Result<OkResponse, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    ensure_policy_allowed(
        &snapshot.lockout,
        |lockout| lockout.allow_buttons,
        "buttons",
    )?;
    let user_id = snapshot.user_id;
    forward_control_message(
        &state.control_tx,
        ControlMessage::Button {
            user_id,
            button_id,
            pressed,
        },
    )
    .await?;
    Ok(OkResponse { ok: true })
}

async fn call_down_handler(
    State(state): State<ApiState>,
    Path(id): Path<u16>,
) -> Result<Json<OkResponse>, ApiError> {
    state.call_down(id).await.map(Json)
}

async fn call_up_handler(
    State(state): State<ApiState>,
    Path(id): Path<u16>,
) -> Result<Json<OkResponse>, ApiError> {
    state.call_up(id).await.map(Json)
}

async fn call_toggle_handler(
    State(state): State<ApiState>,
    Path(id): Path<u16>,
) -> Result<Json<OkResponse>, ApiError> {
    state.call_toggle(id).await.map(Json)
}

async fn reply_down_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    state.reply_down().await.map(Json)
}

async fn reply_up_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    state.reply_up().await.map(Json)
}

async fn reply_toggle_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    state.reply_toggle().await.map(Json)
}

async fn send_direct_call(
    state: &ApiState,
    target_user_id: u16,
    active: bool,
    duck: bool,
) -> Result<OkResponse, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    forward_control_message(
        &state.control_tx,
        ControlMessage::DirectCall {
            user_id: snapshot.user_id,
            target_user_id,
            active,
            duck,
        },
    )
    .await?;
    Ok(OkResponse { ok: true })
}

async fn send_reply_call(
    state: &ApiState,
    active: bool,
    duck: bool,
) -> Result<OkResponse, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    forward_control_message(
        &state.control_tx,
        ControlMessage::ReplyCall {
            user_id: snapshot.user_id,
            active,
            duck,
        },
    )
    .await?;
    Ok(OkResponse { ok: true })
}

async fn alert_send_handler(
    State(state): State<ApiState>,
    Json(request): Json<AlertRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    state.send_alert(request).await.map(Json)
}

async fn alert_ack_handler(
    State(state): State<ApiState>,
    Path(id): Path<AlertId>,
) -> Result<Json<OkResponse>, ApiError> {
    state.ack_alert(id).await.map(Json)
}

async fn alert_cancel_handler(
    State(state): State<ApiState>,
    Path(id): Path<AlertId>,
) -> Result<Json<OkResponse>, ApiError> {
    state.cancel_alert(id).await.map(Json)
}

fn ensure_control_allowed(
    state: &ApiState,
    allowed: impl FnOnce(&ClientLockoutPolicy) -> bool,
    label: &str,
) -> Result<(), ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    ensure_policy_allowed(&snapshot.lockout, allowed, label)
}

fn ensure_local_api_allowed(config: &ClientConfig) -> Result<(), ApiError> {
    ensure_policy_allowed(
        &config.lockout,
        |lockout| lockout.allow_local_api,
        "local API controls",
    )
}

fn ensure_policy_allowed(
    lockout: &ClientLockoutPolicy,
    allowed: impl FnOnce(&ClientLockoutPolicy) -> bool,
    label: &str,
) -> Result<(), ApiError> {
    if allowed(lockout) {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!("{label} locked by admin")))
    }
}

fn validate_full_config_allowed(
    config: &ClientConfig,
    request: &FullConfigRequest,
) -> Result<(), ApiError> {
    let lockout = &config.lockout;
    if !lockout.allow_channels
        && (!same_channels(&request.listen, &config.listen)
            || !same_channels(&request.tx, &config.tx))
    {
        return Err(ApiError::BadRequest("channels locked by admin".to_string()));
    }
    if !lockout.allow_volumes
        && (request.vol != config.vol || request.talker_vol != config.talker_vol)
    {
        return Err(ApiError::BadRequest("volumes locked by admin".to_string()));
    }
    if !lockout.allow_codec && request.codec != config.codec {
        return Err(ApiError::BadRequest("codec locked by admin".to_string()));
    }
    if !lockout.allow_talk_mode && request.talk_mode != config.talk_mode {
        return Err(ApiError::BadRequest(
            "talk mode locked by admin".to_string(),
        ));
    }
    if !lockout.allow_priority
        && (request.priority != config.priority
            || !same_channels(&request.priority_channels, &config.priority_channels))
    {
        return Err(ApiError::BadRequest("priority locked by admin".to_string()));
    }
    if !lockout.allow_ifb && request.ifb != config.ifb {
        return Err(ApiError::BadRequest("IFB locked by admin".to_string()));
    }
    Ok(())
}

fn same_channels(left: &[u16], right: &[u16]) -> bool {
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort_unstable();
    right.sort_unstable();
    left == right
}

fn set_active_button(active_buttons: &mut Vec<ButtonId>, id: &ButtonId, active: bool) {
    if active {
        if !active_buttons.contains(id) {
            active_buttons.push(id.clone());
        }
    } else {
        active_buttons.retain(|button_id| button_id != id);
    }
}

fn set_active_direct_call(
    config: &mut ClientConfig,
    target_user_id: u16,
    active: bool,
    duck: bool,
) {
    if active {
        if let Some(call) = config
            .active_direct_calls
            .iter_mut()
            .find(|call| call.caller == config.user_id && call.target == target_user_id)
        {
            call.active = true;
            call.duck = duck;
        } else {
            config.active_direct_calls.push(DirectCallStatus {
                caller: config.user_id,
                caller_name: None,
                target: target_user_id,
                target_name: None,
                active: true,
                duck,
            });
        }
    } else {
        config
            .active_direct_calls
            .retain(|call| !(call.caller == config.user_id && call.target == target_user_id));
    }
}

async fn forward_control_message(
    control_tx: &mpsc::Sender<ControlRequest>,
    message: ControlMessage,
) -> Result<(), ApiError> {
    let (response_tx, response_rx) = oneshot::channel();
    control_tx
        .send(ControlRequest {
            message,
            response_tx,
        })
        .await
        .map_err(|_| ApiError::Unavailable("control connection is not available".to_string()))?;

    match response_rx
        .await
        .map_err(|_| ApiError::Unavailable("control response channel closed".to_string()))?
    {
        ControlResponse::Ack => Ok(()),
        ControlResponse::Error { message } => Err(ApiError::BadRequest(message)),
        other => Err(ApiError::BadRequest(format!(
            "unexpected control response: {other:?}"
        ))),
    }
}

const LOCAL_UI_HTML: &str = include_str!("../../shared-ui/talking/client-controls.html");
const LOCAL_UI_CSS: &str = include_str!("../../shared-ui/talking/client-controls.css");
const LOCAL_UI_JS: &str = include_str!("../../shared-ui/talking/client-controls.js");
const LOCAL_UI_API_JS: &str = include_str!("../../shared-ui/talking/client-api-http.js");

async fn run_command_loop(
    control_tx: mpsc::Sender<ControlRequest>,
    config: Arc<Mutex<ClientConfig>>,
) -> anyhow::Result<()> {
    eprintln!(
        "commands: tx <channels>, listen <channels>, vol <channel=gain,...>, talk-mode muted|ptt|open, talk on|off, mute, unmute, button <id> down|up|toggle, call <user> down|up|toggle [duck|no-duck], reply down|up|toggle [duck|no-duck], buttons, calls, show"
    );
    let mut lines = io::BufReader::new(io::stdin()).lines();

    while let Some(line) = lines.next_line().await? {
        let command = line.trim();
        if command.is_empty() {
            continue;
        }

        let update = {
            let mut config = config.lock().unwrap();
            match apply_runtime_command(&mut config, command) {
                Ok(update) => update,
                Err(err) => {
                    eprintln!("{err}");
                    continue;
                }
            }
        };

        match update {
            CommandUpdate::SendConfig => {
                let snapshot = config.lock().unwrap().clone();
                queue_control_message(&control_tx, snapshot.control_message()).await?;
                eprintln!(
                    "updated: listen={} tx={} vol={}",
                    format_channels(&snapshot.listen),
                    format_channels(&snapshot.tx),
                    format_volumes(&snapshot.vol)
                );
            }
            CommandUpdate::SendTalkMode => {
                let snapshot = config.lock().unwrap().clone();
                queue_control_message(
                    &control_tx,
                    ControlMessage::TalkMode {
                        user_id: snapshot.user_id,
                        mode: snapshot.talk_mode,
                    },
                )
                .await?;
                eprintln!("talk_mode={:?}", snapshot.talk_mode);
            }
            CommandUpdate::SendTalk { active } => {
                let snapshot = config.lock().unwrap().clone();
                queue_control_message(
                    &control_tx,
                    ControlMessage::Talk {
                        user_id: snapshot.user_id,
                        active,
                    },
                )
                .await?;
                eprintln!("regular_talk_active={active}");
            }
            CommandUpdate::SendButton { button_id, pressed } => {
                let snapshot = config.lock().unwrap().clone();
                queue_control_message(
                    &control_tx,
                    ControlMessage::Button {
                        user_id: snapshot.user_id,
                        button_id: button_id.clone(),
                        pressed,
                    },
                )
                .await?;
                eprintln!("button {button_id} pressed={pressed}");
            }
            CommandUpdate::SendDirectCall {
                target_user_id,
                active,
                duck,
            } => {
                let snapshot = config.lock().unwrap().clone();
                queue_control_message(
                    &control_tx,
                    ControlMessage::DirectCall {
                        user_id: snapshot.user_id,
                        target_user_id,
                        active,
                        duck,
                    },
                )
                .await?;
                eprintln!("direct_call target={target_user_id} active={active} duck={duck}");
            }
            CommandUpdate::SendReplyCall { active, duck } => {
                let snapshot = config.lock().unwrap().clone();
                queue_control_message(
                    &control_tx,
                    ControlMessage::ReplyCall {
                        user_id: snapshot.user_id,
                        active,
                        duck,
                    },
                )
                .await?;
                eprintln!("reply_call active={active} duck={duck}");
            }
            CommandUpdate::Show => {
                let snapshot = config.lock().unwrap().clone();
                eprintln!(
                    "listen={} tx={} buttons={} active_buttons={} active_calls={} last_caller={:?} codec={} talk_mode={:?} regular_talk={} priority={} vol={}",
                    format_channels(&snapshot.listen),
                    format_channels(&snapshot.tx),
                    format_buttons(&snapshot.buttons),
                    format_button_ids(&snapshot.active_buttons),
                    format_direct_calls(&snapshot.active_direct_calls),
                    snapshot.last_direct_caller,
                    format_codec(snapshot.codec),
                    snapshot.talk_mode,
                    snapshot.regular_talk_active,
                    snapshot.priority,
                    format_volumes(&snapshot.vol)
                );
            }
        }
    }

    Ok(())
}

async fn queue_control_message(
    control_tx: &mpsc::Sender<ControlRequest>,
    message: ControlMessage,
) -> anyhow::Result<()> {
    match send_control_request(control_tx, message).await? {
        ControlResponse::Ack => Ok(()),
        ControlResponse::Error { message } => bail!("{message}"),
        other => bail!("unexpected control response: {other:?}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CommandUpdate {
    SendConfig,
    SendTalkMode,
    SendTalk {
        active: bool,
    },
    SendButton {
        button_id: ButtonId,
        pressed: bool,
    },
    SendDirectCall {
        target_user_id: u16,
        active: bool,
        duck: bool,
    },
    SendReplyCall {
        active: bool,
        duck: bool,
    },
    Show,
}

fn apply_runtime_command(
    config: &mut ClientConfig,
    command: &str,
) -> anyhow::Result<CommandUpdate> {
    let mut parts = command.split_whitespace();
    let Some(name) = parts.next() else {
        return Ok(CommandUpdate::Show);
    };
    let rest = parts.collect::<Vec<_>>().join(" ");

    match name {
        "tx" => {
            config.tx = parse_channels(&rest)?;
            Ok(CommandUpdate::SendConfig)
        }
        "listen" => {
            config.listen = parse_channels(&rest)?;
            Ok(CommandUpdate::SendConfig)
        }
        "vol" => {
            config.vol = parse_volumes(&rest)?;
            Ok(CommandUpdate::SendConfig)
        }
        "mute" => {
            config.set_talk_mode(TalkMode::Muted);
            config.regular_talk_active = false;
            Ok(CommandUpdate::SendTalkMode)
        }
        "unmute" => {
            let talk_mode = config.restored_unmute_talk_mode();
            config.set_talk_mode(talk_mode);
            Ok(CommandUpdate::SendTalkMode)
        }
        "talk-mode" | "mode" => {
            config.set_talk_mode(parse_talk_mode(&rest)?);
            Ok(CommandUpdate::SendTalkMode)
        }
        "talk" => {
            let active = parse_bool(&rest)?;
            config.regular_talk_active = active;
            Ok(CommandUpdate::SendTalk { active })
        }
        "button" => parse_button_command(config, &rest),
        "call" => parse_direct_call_command(&rest),
        "reply" => parse_reply_call_command(&rest),
        "buttons" => Ok(CommandUpdate::Show),
        "calls" => Ok(CommandUpdate::Show),
        "show" => Ok(CommandUpdate::Show),
        "help" => {
            eprintln!("commands: tx <channels>, listen <channels>, vol <channel=gain,...>, talk-mode muted|ptt|open, talk on|off, mute, unmute, button <id> down|up|toggle, call <user> down|up|toggle [duck|no-duck], reply down|up|toggle [duck|no-duck], buttons, calls, show");
            Ok(CommandUpdate::Show)
        }
        other => bail!("unknown command `{other}`"),
    }
}

fn parse_bool(value: &str) -> anyhow::Result<bool> {
    match value.trim() {
        "on" | "true" | "1" => Ok(true),
        "off" | "false" | "0" => Ok(false),
        other => bail!("invalid boolean `{other}`, expected on/off"),
    }
}

fn parse_talk_mode(value: &str) -> anyhow::Result<TalkMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "muted" | "mute" => Ok(TalkMode::Muted),
        "ptt" => Ok(TalkMode::Ptt),
        "open" => Ok(TalkMode::Open),
        other => bail!("invalid talk mode `{other}`, expected muted, ptt, or open"),
    }
}

fn parse_button_command(config: &ClientConfig, rest: &str) -> anyhow::Result<CommandUpdate> {
    let mut parts = rest.split_whitespace();
    let button_id = parts
        .next()
        .context("button command requires a button id")?
        .to_string();
    let action = parts
        .next()
        .context("button command requires down, up, or toggle")?;
    if parts.next().is_some() {
        bail!("button command accepts exactly: button <id> down|up|toggle");
    }
    if !config.button_known(&button_id) {
        bail!("unknown button `{button_id}`");
    }
    match action {
        "down" | "press" | "on" => Ok(CommandUpdate::SendButton {
            button_id,
            pressed: true,
        }),
        "up" | "release" | "off" => Ok(CommandUpdate::SendButton {
            button_id,
            pressed: false,
        }),
        "toggle" => Ok(CommandUpdate::SendButton {
            button_id,
            pressed: true,
        }),
        other => bail!("invalid button action `{other}`, expected down/up/toggle"),
    }
}

fn parse_direct_call_command(rest: &str) -> anyhow::Result<CommandUpdate> {
    let mut parts = rest.split_whitespace();
    let target_user_id = parts
        .next()
        .context("call command requires a target user id")?
        .parse::<u16>()
        .context("call target must be a user id")?;
    let action = parts
        .next()
        .context("call command requires down, up, or toggle")?;
    let duck = parse_duck_flag(parts.next())?;
    if parts.next().is_some() {
        bail!("call command accepts exactly: call <user> down|up|toggle [duck|no-duck]");
    }
    Ok(CommandUpdate::SendDirectCall {
        target_user_id,
        active: parse_action_active(action)?,
        duck,
    })
}

fn parse_reply_call_command(rest: &str) -> anyhow::Result<CommandUpdate> {
    let mut parts = rest.split_whitespace();
    let action = parts
        .next()
        .context("reply command requires down, up, or toggle")?;
    let duck = parse_duck_flag(parts.next())?;
    if parts.next().is_some() {
        bail!("reply command accepts exactly: reply down|up|toggle [duck|no-duck]");
    }
    Ok(CommandUpdate::SendReplyCall {
        active: parse_action_active(action)?,
        duck,
    })
}

fn parse_action_active(action: &str) -> anyhow::Result<bool> {
    match action {
        "down" | "press" | "on" | "toggle" => Ok(true),
        "up" | "release" | "off" => Ok(false),
        other => bail!("invalid action `{other}`, expected down/up/toggle"),
    }
}

fn parse_duck_flag(value: Option<&str>) -> anyhow::Result<bool> {
    match value.unwrap_or("no-duck") {
        "duck" => Ok(true),
        "no-duck" => Ok(false),
        other => bail!("invalid duck flag `{other}`, expected duck or no-duck"),
    }
}

fn format_direct_calls(calls: &[DirectCallStatus]) -> String {
    calls
        .iter()
        .filter(|call| call.active)
        .map(|call| {
            format!(
                "{}->{}{}",
                call.caller,
                call.target,
                if call.duck { ":duck" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

async fn run_hotkey_loop(
    button_keys: Vec<ButtonKeyArg>,
    control_tx: mpsc::Sender<ControlRequest>,
    config: Arc<Mutex<ClientConfig>>,
) -> anyhow::Result<()> {
    eprintln!("focused hotkeys enabled; line commands are disabled while hotkeys are active");
    let key_map = button_keys
        .into_iter()
        .map(|binding| (binding.key, binding.button_id))
        .collect::<HashMap<_, _>>();
    tokio::task::spawn_blocking(move || run_blocking_hotkey_loop(key_map, control_tx, config))
        .await
        .context("hotkey task panicked")?
}

fn run_blocking_hotkey_loop(
    key_map: HashMap<char, ButtonId>,
    control_tx: mpsc::Sender<ControlRequest>,
    config: Arc<Mutex<ClientConfig>>,
) -> anyhow::Result<()> {
    crossterm::terminal::enable_raw_mode().context("enable terminal raw mode")?;
    let _raw_guard = RawModeGuard;
    loop {
        let Event::Key(key_event) = event::read().context("read terminal event")? else {
            continue;
        };
        let KeyCode::Char(key) = key_event.code else {
            continue;
        };
        let Some(button_id) = key_map.get(&key).cloned() else {
            continue;
        };
        let pressed = match key_event.kind {
            KeyEventKind::Press => true,
            KeyEventKind::Release => false,
            KeyEventKind::Repeat => continue,
        };
        let user_id = config.lock().unwrap().user_id;
        control_tx
            .blocking_send(ControlRequest {
                message: ControlMessage::Button {
                    user_id,
                    button_id,
                    pressed,
                },
                response_tx: oneshot::channel().0,
            })
            .context("queue hotkey button event")?;
    }
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn list_audio_devices() -> anyhow::Result<()> {
    let host = cpal::default_host();

    println!("Input devices:");
    for device in host.input_devices().context("list input devices")? {
        println!(
            "  {}",
            device.name().unwrap_or_else(|_| "<unknown>".to_string())
        );
    }

    println!();
    println!("Output devices:");
    for device in host.output_devices().context("list output devices")? {
        println!(
            "  {}",
            device.name().unwrap_or_else(|_| "<unknown>".to_string())
        );
    }

    Ok(())
}

fn select_input_device(host: &cpal::Host, pattern: Option<&str>) -> anyhow::Result<cpal::Device> {
    select_device(
        host.input_devices().context("list input devices")?,
        pattern,
        "input",
        || host.default_input_device(),
    )
}

fn select_output_device(host: &cpal::Host, pattern: Option<&str>) -> anyhow::Result<cpal::Device> {
    select_device(
        host.output_devices().context("list output devices")?,
        pattern,
        "output",
        || host.default_output_device(),
    )
}

fn select_device<I, F>(
    devices: I,
    pattern: Option<&str>,
    kind: &str,
    default_device: F,
) -> anyhow::Result<cpal::Device>
where
    I: IntoIterator<Item = cpal::Device>,
    F: FnOnce() -> Option<cpal::Device>,
{
    let Some(pattern) = pattern else {
        return default_device().ok_or_else(|| anyhow!("no default {kind} device"));
    };

    for device in devices {
        let name = device.name().unwrap_or_default();
        if device_name_matches(&name, pattern) {
            return Ok(device);
        }
    }

    bail!("no {kind} device matched `{pattern}`; run with --list-devices to inspect devices")
}

fn device_name_matches(name: &str, pattern: &str) -> bool {
    name.to_lowercase().contains(&pattern.to_lowercase())
}

enum InputStream {
    Raw(cpal::Stream),
    #[cfg(target_os = "macos")]
    VoiceProcessing(macos_voice::VoiceProcessingInputStream),
    #[cfg(target_os = "ios")]
    IosVoiceProcessing(ios_voice::VoiceProcessingInputStream),
}

impl InputStream {
    fn play(&mut self) -> anyhow::Result<()> {
        match self {
            Self::Raw(stream) => stream.play().context("start input stream"),
            #[cfg(target_os = "macos")]
            Self::VoiceProcessing(stream) => stream.play(),
            #[cfg(target_os = "ios")]
            Self::IosVoiceProcessing(stream) => stream.play(),
        }
    }
}

fn build_input_stream(
    tx: mpsc::Sender<Vec<i16>>,
    audio_settings: Arc<AudioSettings>,
    input_device: Option<&str>,
    input_backend: AudioInputBackend,
    capture_options: CapturePipelineOptions,
) -> anyhow::Result<(InputStream, InputBackendStatus)> {
    match input_backend {
        AudioInputBackend::Raw => Ok((
            build_cpal_input_stream(tx, audio_settings, input_device, capture_options)?,
            InputBackendStatus::active(AudioInputBackend::Raw, AudioInputBackend::Raw, None),
        )),
        AudioInputBackend::VoiceProcessing => build_preferred_voice_processing_input_stream(
            tx,
            audio_settings,
            input_device,
            capture_options,
            AudioInputBackend::VoiceProcessing,
        ),
        AudioInputBackend::Auto => {
            build_auto_input_stream(tx, audio_settings, input_device, capture_options)
        }
    }
}

#[cfg(target_os = "macos")]
fn build_auto_input_stream(
    tx: mpsc::Sender<Vec<i16>>,
    audio_settings: Arc<AudioSettings>,
    input_device: Option<&str>,
    capture_options: CapturePipelineOptions,
) -> anyhow::Result<(InputStream, InputBackendStatus)> {
    build_preferred_voice_processing_input_stream(
        tx,
        audio_settings,
        input_device,
        capture_options,
        AudioInputBackend::Auto,
    )
}

#[cfg(target_os = "macos")]
fn build_preferred_voice_processing_input_stream(
    tx: mpsc::Sender<Vec<i16>>,
    audio_settings: Arc<AudioSettings>,
    input_device: Option<&str>,
    capture_options: CapturePipelineOptions,
    requested: AudioInputBackend,
) -> anyhow::Result<(InputStream, InputBackendStatus)> {
    if let Some(input_device) = input_device {
        return Ok((
            build_cpal_input_stream(
                tx,
                audio_settings,
                Some(input_device),
                capture_options,
            )?,
            InputBackendStatus::active(
                requested,
                AudioInputBackend::Raw,
                Some(
                    "selected input devices use the raw backend; macOS voice processing requires the default input device".to_string(),
                ),
            ),
        ));
    }

    match macos_voice::VoiceProcessingInputStream::new(
        tx.clone(),
        Arc::clone(&audio_settings),
        capture_options.clone(),
    ) {
        Ok(stream) => {
            tracing::info!("using macOS VoiceProcessingIO input backend");
            let note = macos_mic_mode::context_note().unwrap_or_else(|err| Some(err.to_string()));
            Ok((
                InputStream::VoiceProcessing(stream),
                InputBackendStatus::active(requested, AudioInputBackend::VoiceProcessing, note),
            ))
        }
        Err(err) => {
            tracing::warn!(%err, "macOS VoiceProcessingIO input unavailable; falling back to raw cpal input");
            let note = format!("macOS voice processing unavailable; using raw input: {err:#}");
            let stream = build_cpal_input_stream(tx, audio_settings, input_device, capture_options)
                .with_context(|| {
                    format!("VoiceProcessingIO failed ({err:#}) and raw fallback failed")
                })?;
            Ok((
                stream,
                InputBackendStatus::active(requested, AudioInputBackend::Raw, Some(note)),
            ))
        }
    }
}

#[cfg(target_os = "ios")]
fn build_auto_input_stream(
    tx: mpsc::Sender<Vec<i16>>,
    audio_settings: Arc<AudioSettings>,
    input_device: Option<&str>,
    capture_options: CapturePipelineOptions,
) -> anyhow::Result<(InputStream, InputBackendStatus)> {
    build_preferred_voice_processing_input_stream(
        tx,
        audio_settings,
        input_device,
        capture_options,
        AudioInputBackend::Auto,
    )
}

#[cfg(target_os = "ios")]
fn build_preferred_voice_processing_input_stream(
    tx: mpsc::Sender<Vec<i16>>,
    audio_settings: Arc<AudioSettings>,
    input_device: Option<&str>,
    capture_options: CapturePipelineOptions,
    requested: AudioInputBackend,
) -> anyhow::Result<(InputStream, InputBackendStatus)> {
    if let Some(input_device) = input_device {
        return Ok((
            build_cpal_input_stream(
                tx,
                audio_settings,
                Some(input_device),
                capture_options,
            )?,
            InputBackendStatus::active(
                requested,
                AudioInputBackend::Raw,
                Some(
                    "selected input devices use the raw backend; iOS VoiceProcessingIO requires the default input route".to_string(),
                ),
            ),
        ));
    }

    match ios_voice::VoiceProcessingInputStream::new(
        tx.clone(),
        Arc::clone(&audio_settings),
        capture_options.clone(),
    ) {
        Ok(stream) => {
            tracing::info!("using iOS VoiceProcessingIO input backend");
            Ok((
                InputStream::IosVoiceProcessing(stream),
                InputBackendStatus::active(
                    requested,
                    AudioInputBackend::VoiceProcessing,
                    Some(
                        "using iOS VoiceProcessingIO input with automatic gain control".to_string(),
                    ),
                ),
            ))
        }
        Err(err) => {
            tracing::warn!(%err, "iOS VoiceProcessingIO input unavailable; falling back to raw cpal input");
            let note = format!("iOS voice processing unavailable; using raw input: {err:#}");
            let stream = build_cpal_input_stream(tx, audio_settings, input_device, capture_options)
                .with_context(|| {
                    format!("iOS VoiceProcessingIO failed ({err:#}) and raw fallback failed")
                })?;
            Ok((
                stream,
                InputBackendStatus::active(requested, AudioInputBackend::Raw, Some(note)),
            ))
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn build_auto_input_stream(
    tx: mpsc::Sender<Vec<i16>>,
    audio_settings: Arc<AudioSettings>,
    input_device: Option<&str>,
    capture_options: CapturePipelineOptions,
) -> anyhow::Result<(InputStream, InputBackendStatus)> {
    Ok((
        build_cpal_input_stream(tx, audio_settings, input_device, capture_options)?,
        InputBackendStatus::active(
            AudioInputBackend::Auto,
            AudioInputBackend::Raw,
            Some("auto uses the raw input backend on this platform".to_string()),
        ),
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn build_preferred_voice_processing_input_stream(
    tx: mpsc::Sender<Vec<i16>>,
    audio_settings: Arc<AudioSettings>,
    input_device: Option<&str>,
    capture_options: CapturePipelineOptions,
    requested: AudioInputBackend,
) -> anyhow::Result<(InputStream, InputBackendStatus)> {
    Ok((
        build_cpal_input_stream(tx, audio_settings, input_device, capture_options)?,
        InputBackendStatus::active(
            requested,
            AudioInputBackend::Raw,
            Some(
                "voice-processing input backend is only available on macOS; using raw input"
                    .to_string(),
            ),
        ),
    ))
}

fn build_cpal_input_stream(
    tx: mpsc::Sender<Vec<i16>>,
    audio_settings: Arc<AudioSettings>,
    input_device: Option<&str>,
    capture_options: CapturePipelineOptions,
) -> anyhow::Result<InputStream> {
    let host = cpal::default_host();
    let device = select_input_device(&host, input_device)?;
    let device_name = device.name()?;
    let supported = device.default_input_config()?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate.0;
    let input_channel = capture_options.channel_mode;
    tracing::info!(device = %device_name, ?config, ?sample_format, input_channel = input_channel.as_str(), "opened input device");
    if let Some(diagnostics) = &capture_options.diagnostics {
        diagnostics.set_source(CaptureSourceInfo {
            backend: AudioInputBackend::Raw,
            device: device_name,
            sample_format: format!("{sample_format:?}"),
            sample_rate_hz: sample_rate,
            channels: channels as u16,
            channel_mode: input_channel,
        });
    }

    let mut capture =
        CaptureAdapter::new(tx, sample_rate, channels, audio_settings, capture_options);
    let err_fn = |err| tracing::error!(%err, "input stream error");

    let stream = match sample_format {
        SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| capture.push_interleaved_i16(data.iter().copied()),
            err_fn,
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _| capture.push_interleaved_u16(data.iter().copied()),
            err_fn,
            None,
        ),
        SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| capture.push_interleaved_f32(data.iter().copied()),
            err_fn,
            None,
        ),
        other => return Err(anyhow!("unsupported input sample format: {other:?}")),
    }
    .context("build input stream")?;
    Ok(InputStream::Raw(stream))
}

fn build_output_stream(
    playback: Arc<Mutex<PlaybackBuffer>>,
    audio_settings: Arc<AudioSettings>,
    output_device: Option<&str>,
) -> anyhow::Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = select_output_device(&host, output_device)?;
    let supported = device.default_output_config()?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate.0;
    tracing::info!(device = %device.name()?, ?config, ?sample_format, "opened output device");

    let err_fn = |err| tracing::error!(%err, "output stream error");

    match sample_format {
        SampleFormat::I16 => {
            let mut output =
                OutputAdapter::new(playback, sample_rate, channels, Arc::clone(&audio_settings));
            device.build_output_stream(
                &config,
                move |data: &mut [i16], _| output.fill(data, |sample| sample),
                err_fn,
                None,
            )
        }
        SampleFormat::U16 => {
            let mut output =
                OutputAdapter::new(playback, sample_rate, channels, Arc::clone(&audio_settings));
            device.build_output_stream(
                &config,
                move |data: &mut [u16], _| {
                    output.fill(data, |sample| (sample as i32 + 32768) as u16)
                },
                err_fn,
                None,
            )
        }
        SampleFormat::F32 => {
            let mut output = OutputAdapter::new(playback, sample_rate, channels, audio_settings);
            device.build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    output.fill(data, |sample| sample as f32 / i16::MAX as f32)
                },
                err_fn,
                None,
            )
        }
        other => return Err(anyhow!("unsupported output sample format: {other:?}")),
    }
    .context("build output stream")
}

#[derive(Clone)]
struct CapturePipelineOptions {
    channel_mode: InputChannelMode,
    input_limiter: bool,
    input_transient_suppression: bool,
    input_silence_gate: bool,
    processing_settings: Arc<CaptureProcessingSettings>,
    diagnostics: Option<CaptureDiagnostics>,
    telemetry: Option<ClientTelemetryCounters>,
    debug_audio_tap: Option<DesktopDebugAudioTap>,
}

#[derive(Debug)]
struct CaptureProcessingSettings {
    profile: AtomicU8,
    mode: AtomicU8,
    vad: AtomicU8,
    transient_suppression: AtomicU8,
    native_voice_processing: AtomicU8,
}

#[derive(Debug, Clone, Copy)]
struct CaptureProcessingSnapshot {
    profile: ProcessingProfile,
    mode: ProcessingMode,
    vad: bool,
    transient_suppression: bool,
    native_voice_processing: bool,
}

impl CaptureProcessingSettings {
    fn new(config: &ProcessingConfig) -> Self {
        let settings = Self {
            profile: AtomicU8::new(0),
            mode: AtomicU8::new(0),
            vad: AtomicU8::new(0),
            transient_suppression: AtomicU8::new(0),
            native_voice_processing: AtomicU8::new(0),
        };
        settings.store(config);
        settings
    }

    fn store(&self, config: &ProcessingConfig) {
        self.profile
            .store(processing_profile_to_u8(config.profile), Ordering::Relaxed);
        self.mode
            .store(processing_mode_to_u8(config.mode), Ordering::Relaxed);
        self.vad.store(u8::from(config.vad), Ordering::Relaxed);
        self.transient_suppression
            .store(u8::from(config.transient_suppression), Ordering::Relaxed);
        self.native_voice_processing
            .store(u8::from(config.native_voice_processing), Ordering::Relaxed);
    }

    fn snapshot(&self) -> CaptureProcessingSnapshot {
        CaptureProcessingSnapshot {
            profile: u8_to_processing_profile(self.profile.load(Ordering::Relaxed)),
            mode: u8_to_processing_mode(self.mode.load(Ordering::Relaxed)),
            vad: self.vad.load(Ordering::Relaxed) != 0,
            transient_suppression: self.transient_suppression.load(Ordering::Relaxed) != 0,
            native_voice_processing: self.native_voice_processing.load(Ordering::Relaxed) != 0,
        }
    }
}

fn processing_profile_to_u8(profile: ProcessingProfile) -> u8 {
    match profile {
        ProcessingProfile::Raw => 0,
        ProcessingProfile::Voice => 1,
        ProcessingProfile::VoiceIsolation => 2,
        ProcessingProfile::Broadcast => 3,
    }
}

fn u8_to_processing_profile(value: u8) -> ProcessingProfile {
    match value {
        0 => ProcessingProfile::Raw,
        2 => ProcessingProfile::VoiceIsolation,
        3 => ProcessingProfile::Broadcast,
        _ => ProcessingProfile::Voice,
    }
}

fn processing_mode_to_u8(mode: ProcessingMode) -> u8 {
    match mode {
        ProcessingMode::Auto => 0,
        ProcessingMode::Enabled => 1,
        ProcessingMode::Disabled => 2,
    }
}

fn u8_to_processing_mode(value: u8) -> ProcessingMode {
    match value {
        1 => ProcessingMode::Enabled,
        2 => ProcessingMode::Disabled,
        _ => ProcessingMode::Auto,
    }
}

#[derive(Clone)]
struct CaptureDiagnostics {
    state: Arc<Mutex<CaptureDiagnosticsState>>,
}

#[derive(Debug)]
struct CaptureDiagnosticsState {
    source: CaptureSourceInfo,
    mic_gain: f32,
    pre_gain: CaptureStats,
    post_gain: CaptureStats,
    dropped_frames: u32,
}

#[derive(Debug, Clone)]
struct CaptureSourceInfo {
    backend: AudioInputBackend,
    device: String,
    sample_format: String,
    sample_rate_hz: u32,
    channels: u16,
    channel_mode: InputChannelMode,
}

impl Default for CaptureSourceInfo {
    fn default() -> Self {
        Self {
            backend: AudioInputBackend::Raw,
            device: "unknown".to_string(),
            sample_format: "unknown".to_string(),
            sample_rate_hz: MIX_SAMPLE_RATE,
            channels: 1,
            channel_mode: InputChannelMode::Average,
        }
    }
}

impl CaptureDiagnostics {
    fn new(channel_mode: InputChannelMode) -> Self {
        Self {
            state: Arc::new(Mutex::new(CaptureDiagnosticsState {
                source: CaptureSourceInfo {
                    channel_mode,
                    ..CaptureSourceInfo::default()
                },
                mic_gain: 1.0,
                pre_gain: CaptureStats::default(),
                post_gain: CaptureStats::default(),
                dropped_frames: 0,
            })),
        }
    }

    fn set_source(&self, source: CaptureSourceInfo) {
        self.state.lock().unwrap().source = source;
    }

    fn record_frame(
        &self,
        pre_gain: CaptureStats,
        post_gain: CaptureStats,
        dropped: bool,
        mic_gain: f32,
    ) {
        let mut state = self.state.lock().unwrap();
        state.mic_gain = mic_gain;
        state.pre_gain.merge(pre_gain);
        state.post_gain.merge(post_gain);
        if dropped {
            state.dropped_frames = state.dropped_frames.saturating_add(1);
        }
    }

    fn snapshot(&self) -> CaptureHealthStatus {
        let mut state = self.state.lock().unwrap();
        let source = state.source.clone();
        let pre_gain = state.pre_gain.take();
        let post_gain = state.post_gain.take();
        let dropped_frames = std::mem::take(&mut state.dropped_frames);
        let selected = post_gain.channel_health();
        let mic_gain = state.mic_gain;
        let software_gain_percent = (mic_gain * 100.0).round().clamp(0.0, u16::MAX as f32) as u16;

        CaptureHealthStatus {
            runtime: None,
            audio: None,
            playback: None,
            client_transport: None,
            codec_config: None,
            desktop: Some(DesktopCaptureHealthStatus {
                backend: format!("{:?}", source.backend),
                device: source.device,
                sample_format: source.sample_format,
                sample_rate_hz: source.sample_rate_hz,
                channels: source.channels,
                channel_mode: source.channel_mode.as_str().to_string(),
                mic_gain,
                pre_gain: pre_gain.channel_health(),
                post_gain: selected.clone(),
                pre_gain_clipped_samples: pre_gain.clipped_samples,
                post_gain_clipped_samples: post_gain.clipped_samples,
                dropped_frames,
            }),
            uptime_ms: 0,
            wifi: None,
            transport: None,
            memory: None,
            task_stack_high_water_bytes: None,
            display: None,
            battery: None,
            playback_queue_depth: 0,
            playback_underflows: 0,
            playback_overflows: 0,
            playback_i2s_gap_warnings: 0,
            playback_i2s_slow_warnings: 0,
            playback_i2s_short_warnings: 0,
            free_heap_bytes: 0,
            min_free_heap_bytes: 0,
            tx_target_count: 0,
            tx_packets_sent: 0,
            tx_send_failures: 0,
            adc_input: "desktop".to_string(),
            mic_pga_gain_db: 0,
            capture_channel: source.channel_mode.as_str().to_string(),
            software_gain_percent,
            high_pass_enabled: false,
            alc_enabled: false,
            noise_gate_enabled: false,
            left: CaptureChannelHealth::default(),
            right: CaptureChannelHealth::default(),
            selected,
            raw_clipped_samples: pre_gain.clipped_samples,
            software_clipped_samples: post_gain.clipped_samples,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct CaptureStats {
    samples: u64,
    sum: f64,
    sum_squares: f64,
    peak: f32,
    clipped_samples: u32,
}

impl CaptureStats {
    fn observe(&mut self, sample: f32) {
        self.samples += 1;
        self.sum += sample as f64;
        self.sum_squares += f64::from(sample * sample);
        self.peak = self.peak.max(sample.abs());
        if sample.abs() > 1.0 {
            self.clipped_samples = self.clipped_samples.saturating_add(1);
        }
    }

    fn merge(&mut self, other: Self) {
        self.samples += other.samples;
        self.sum += other.sum;
        self.sum_squares += other.sum_squares;
        self.peak = self.peak.max(other.peak);
        self.clipped_samples = self.clipped_samples.saturating_add(other.clipped_samples);
    }

    fn take(&mut self) -> Self {
        std::mem::take(self)
    }

    fn channel_health(&self) -> CaptureChannelHealth {
        if self.samples == 0 {
            return CaptureChannelHealth::default();
        }
        CaptureChannelHealth {
            rms: (self.sum_squares / self.samples as f64).sqrt() as f32,
            peak: self.peak,
            dc_offset: (self.sum / self.samples as f64) as f32,
        }
    }
}

#[derive(Clone)]
struct DesktopDebugAudioTap {
    tx: mpsc::Sender<DesktopDebugAudioFrame>,
}

struct DesktopDebugAudioFrame {
    pre_gain: Vec<i16>,
    post_gain: Vec<i16>,
}

fn spawn_desktop_debug_audio_writer(dir: PathBuf) -> anyhow::Result<DesktopDebugAudioTap> {
    fs::create_dir_all(&dir)
        .with_context(|| format!("create desktop debug audio directory {}", dir.display()))?;
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        if let Err(err) = run_desktop_debug_audio_writer(dir, rx).await {
            tracing::warn!(%err, "desktop debug audio writer stopped");
        }
    });
    Ok(DesktopDebugAudioTap { tx })
}

async fn run_desktop_debug_audio_writer(
    dir: PathBuf,
    mut rx: mpsc::Receiver<DesktopDebugAudioFrame>,
) -> anyhow::Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: MIX_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let pre_path = dir.join("desktop-pre-gain.wav");
    let post_path = dir.join("desktop-post-gain.wav");
    let mut pre_writer = hound::WavWriter::create(&pre_path, spec)
        .with_context(|| format!("create WAV writer {}", pre_path.display()))?;
    let mut post_writer = hound::WavWriter::create(&post_path, spec)
        .with_context(|| format!("create WAV writer {}", post_path.display()))?;

    while let Some(frame) = rx.recv().await {
        for sample in frame.pre_gain {
            pre_writer.write_sample(sample)?;
        }
        for sample in frame.post_gain {
            post_writer.write_sample(sample)?;
        }
    }

    pre_writer.finalize()?;
    post_writer.finalize()?;
    Ok(())
}

struct CaptureAdapter {
    tx: mpsc::Sender<Vec<i16>>,
    frame: Vec<i16>,
    pre_gain_frame: Vec<i16>,
    channels: usize,
    channel_index: usize,
    channel_samples: Vec<f32>,
    ratio: f32,
    phase: f32,
    previous_mono: f32,
    previous_raw_mono: f32,
    smoothed_mono: f32,
    transient_floor: f32,
    transient_gain: f32,
    transient_hold_samples: usize,
    silence_gate_envelope: f32,
    silence_gate_gain: f32,
    silence_gate_open: bool,
    silence_gate_hold_samples: usize,
    has_previous: bool,
    audio_settings: Arc<AudioSettings>,
    channel_mode: InputChannelMode,
    input_limiter: bool,
    input_transient_suppression: bool,
    input_silence_gate: bool,
    processing_settings: Arc<CaptureProcessingSettings>,
    speech_gate: CaptureSpeechGate,
    frame_pre_gain_stats: CaptureStats,
    frame_post_gain_stats: CaptureStats,
    diagnostics: Option<CaptureDiagnostics>,
    telemetry: Option<ClientTelemetryCounters>,
    debug_audio_tap: Option<DesktopDebugAudioTap>,
}

impl CaptureAdapter {
    fn new(
        tx: mpsc::Sender<Vec<i16>>,
        input_rate: u32,
        channels: usize,
        audio_settings: Arc<AudioSettings>,
        options: CapturePipelineOptions,
    ) -> Self {
        let channels = channels.max(1);
        Self {
            tx,
            frame: Vec::with_capacity(MIX_SAMPLES_PER_FRAME),
            pre_gain_frame: Vec::with_capacity(MIX_SAMPLES_PER_FRAME),
            channels,
            channel_index: 0,
            channel_samples: vec![0.0; channels],
            ratio: MIX_SAMPLE_RATE as f32 / input_rate as f32,
            phase: 0.0,
            previous_mono: 0.0,
            previous_raw_mono: 0.0,
            smoothed_mono: 0.0,
            transient_floor: 0.01,
            transient_gain: 1.0,
            transient_hold_samples: 0,
            silence_gate_envelope: 0.0,
            silence_gate_gain: 0.0,
            silence_gate_open: false,
            silence_gate_hold_samples: 0,
            has_previous: false,
            audio_settings,
            channel_mode: options.channel_mode,
            input_limiter: options.input_limiter,
            input_transient_suppression: options.input_transient_suppression,
            input_silence_gate: options.input_silence_gate,
            processing_settings: options.processing_settings,
            speech_gate: CaptureSpeechGate::default(),
            frame_pre_gain_stats: CaptureStats::default(),
            frame_post_gain_stats: CaptureStats::default(),
            diagnostics: options.diagnostics,
            telemetry: options.telemetry,
            debug_audio_tap: options.debug_audio_tap,
        }
    }

    fn push_interleaved_i16<I>(&mut self, samples: I)
    where
        I: IntoIterator<Item = i16>,
    {
        self.push_interleaved_f32(samples.into_iter().map(i16_to_unit_f32));
    }

    fn push_interleaved_u16<I>(&mut self, samples: I)
    where
        I: IntoIterator<Item = u16>,
    {
        self.push_interleaved_f32(samples.into_iter().map(u16_to_unit_f32));
    }

    fn push_interleaved_f32<I>(&mut self, samples: I)
    where
        I: IntoIterator<Item = f32>,
    {
        for sample in samples {
            if self.channel_index < self.channel_samples.len() {
                self.channel_samples[self.channel_index] = sample;
            }
            self.channel_index += 1;

            if self.channel_index == self.channels {
                let mono = self.channel_mode.select(&self.channel_samples);
                self.push_mono(mono);
                self.channel_index = 0;
            }
        }
    }

    fn push_mono(&mut self, mono: f32) {
        let mono = self.suppress_keyboard_transient(mono);
        if !self.has_previous {
            self.previous_mono = mono;
            self.smoothed_mono = mono;
            self.previous_raw_mono = mono;
            self.has_previous = true;
        }

        self.smoothed_mono = self.smoothed_mono * 0.5 + mono * 0.5;
        self.phase += self.ratio;

        while self.phase >= 1.0 {
            let fraction = 1.0 - ((self.phase - 1.0) / self.ratio).clamp(0.0, 1.0);
            let interpolated =
                self.previous_mono + (self.smoothed_mono - self.previous_mono) * fraction;
            let mic_gain = self.audio_settings.mic_gain();
            let post_gain = interpolated * mic_gain;
            let output_sample = if self.input_limiter {
                soft_limit_unit(post_gain)
            } else {
                post_gain
            };
            let output_sample = self.apply_silence_gate(output_sample);
            self.push_wire_sample(interpolated, post_gain, output_sample);
            self.phase -= 1.0;
        }

        self.previous_mono = self.smoothed_mono;
        self.previous_raw_mono = mono;
    }

    fn suppress_keyboard_transient(&mut self, mono: f32) -> f32 {
        let processing = self.processing_settings.snapshot();
        if !self.input_transient_suppression
            || !processing.transient_suppression
            || !capture_processing_active(processing)
        {
            return mono;
        }

        let abs = mono.abs();
        let delta = (mono - self.previous_raw_mono).abs();
        let floor = self.transient_floor.max(0.01);
        let threshold = (floor * 12.0).clamp(0.18, 0.65);
        let sudden = self.has_previous && abs > threshold && delta > threshold * 0.75;

        if sudden {
            let target_gain = (threshold / abs).clamp(0.08, 1.0);
            self.transient_gain = self.transient_gain.min(target_gain);
            self.transient_hold_samples = MIX_SAMPLE_RATE as usize / 20;
        } else if self.transient_hold_samples > 0 {
            self.transient_hold_samples -= 1;
        } else {
            self.transient_gain += (1.0 - self.transient_gain) * 0.002;
        }

        let output = mono * self.transient_gain;
        let learned_abs = output.abs().min(0.25);
        if learned_abs > self.transient_floor {
            self.transient_floor = self.transient_floor * 0.999 + learned_abs * 0.001;
        } else {
            self.transient_floor = self.transient_floor * 0.9995 + learned_abs * 0.0005;
        }
        output
    }

    fn apply_silence_gate(&mut self, sample: f32) -> f32 {
        let processing = self.processing_settings.snapshot();
        if !self.input_silence_gate || !capture_processing_active(processing) {
            return sample;
        }

        const OPEN_THRESHOLD: f32 = 0.012;
        const CLOSE_THRESHOLD: f32 = 0.004;
        const HOLD_SAMPLES: usize = MIX_SAMPLE_RATE as usize / 12;
        const ATTACK: f32 = 0.08;
        const RELEASE: f32 = 0.015;

        let abs = sample.abs();
        let coeff = if abs > self.silence_gate_envelope {
            0.25
        } else {
            0.002
        };
        self.silence_gate_envelope += (abs - self.silence_gate_envelope) * coeff;

        if self.silence_gate_envelope >= OPEN_THRESHOLD {
            self.silence_gate_open = true;
            self.silence_gate_hold_samples = HOLD_SAMPLES;
        } else if self.silence_gate_hold_samples > 0 {
            self.silence_gate_hold_samples -= 1;
        } else if self.silence_gate_envelope <= CLOSE_THRESHOLD {
            self.silence_gate_open = false;
        }

        let target = if self.silence_gate_open { 1.0 } else { 0.0 };
        let step = if target > self.silence_gate_gain {
            ATTACK
        } else {
            RELEASE
        };
        self.silence_gate_gain += (target - self.silence_gate_gain) * step;
        sample * self.silence_gate_gain
    }

    fn process_capture_frame(&mut self, frame: &mut [i16]) {
        let processing = self.processing_settings.snapshot();
        if !capture_processing_active(processing) {
            return;
        }
        if processing.transient_suppression {
            suppress_capture_transient_frame(processing.profile, frame);
        }
        if processing.vad {
            self.speech_gate.process(processing.profile, frame);
        }
    }

    fn push_wire_sample(&mut self, pre_gain: f32, post_gain: f32, output_sample: f32) {
        self.frame_pre_gain_stats.observe(pre_gain);
        self.frame_post_gain_stats.observe(post_gain);
        self.pre_gain_frame.push(unit_f32_to_i16(pre_gain));
        self.frame.push(unit_f32_to_i16(output_sample));

        if self.frame.len() == MIX_SAMPLES_PER_FRAME {
            let mut full_frame =
                std::mem::replace(&mut self.frame, Vec::with_capacity(MIX_SAMPLES_PER_FRAME));
            let pre_gain_frame = std::mem::replace(
                &mut self.pre_gain_frame,
                Vec::with_capacity(MIX_SAMPLES_PER_FRAME),
            );
            self.process_capture_frame(&mut full_frame);
            let dropped = self.tx.try_send(full_frame.clone()).is_err();
            if dropped {
                tracing::warn!("dropped mic frame because network queue is full");
                if let Some(telemetry) = &self.telemetry {
                    telemetry.record_tx_queue_drop();
                }
            }
            if let Some(tap) = &self.debug_audio_tap {
                let _ = tap.tx.try_send(DesktopDebugAudioFrame {
                    pre_gain: pre_gain_frame,
                    post_gain: full_frame,
                });
            }
            if let Some(diagnostics) = &self.diagnostics {
                diagnostics.record_frame(
                    self.frame_pre_gain_stats.take(),
                    self.frame_post_gain_stats.take(),
                    dropped,
                    self.audio_settings.mic_gain(),
                );
            }
        }
    }
}

#[derive(Debug, Default)]
struct CaptureSpeechGate {
    open: bool,
    speech_frames: usize,
    hold_frames: usize,
    gain: f32,
}

impl CaptureSpeechGate {
    fn process(&mut self, profile: ProcessingProfile, frame: &mut [i16]) -> bool {
        let meter = capture_frame_meter(frame);
        let features = capture_voice_features(frame, meter);
        let params = capture_gate_params(profile);
        let speech_like = meter.rms >= params.open_rms
            && features.zero_crossing_rate <= params.max_zero_crossing_rate
            && features.crest_factor <= params.max_crest_factor;

        if speech_like {
            self.speech_frames = self.speech_frames.saturating_add(1);
            if self.speech_frames >= params.open_frames {
                self.open = true;
                self.hold_frames = params.hold_frames;
            }
        } else {
            self.speech_frames = 0;
            if self.hold_frames > 0 && meter.rms >= params.close_rms {
                self.hold_frames -= 1;
            } else if meter.rms < params.close_rms {
                self.open = false;
            }
        }

        let target = if self.open { 1.0 } else { params.closed_gain };
        let step = if target > self.gain {
            params.attack
        } else {
            params.release
        };
        self.gain += (target - self.gain) * step;
        let gain = self.gain;
        for sample in frame {
            *sample = ((*sample as f32) * gain)
                .clamp(i16::MIN as f32, i16::MAX as f32)
                .round() as i16;
        }
        self.open
    }
}

fn capture_processing_active(processing: CaptureProcessingSnapshot) -> bool {
    let _native_voice_processing_requested = processing.native_voice_processing;
    match processing.mode {
        ProcessingMode::Enabled => !matches!(processing.profile, ProcessingProfile::Raw),
        ProcessingMode::Disabled => false,
        ProcessingMode::Auto => !matches!(processing.profile, ProcessingProfile::Raw),
    }
}

#[derive(Debug, Clone, Copy)]
struct CaptureFrameMeter {
    peak: f32,
    rms: f32,
}

#[derive(Debug, Clone, Copy)]
struct CaptureVoiceFeatures {
    zero_crossing_rate: f32,
    crest_factor: f32,
}

#[derive(Debug, Clone, Copy)]
struct CaptureGateParams {
    open_rms: f32,
    close_rms: f32,
    open_frames: usize,
    hold_frames: usize,
    closed_gain: f32,
    attack: f32,
    release: f32,
    max_zero_crossing_rate: f32,
    max_crest_factor: f32,
    transient_crest_factor: f32,
}

fn capture_frame_meter(samples: &[i16]) -> CaptureFrameMeter {
    if samples.is_empty() {
        return CaptureFrameMeter {
            peak: 0.0,
            rms: 0.0,
        };
    }
    let mut peak = 0.0_f32;
    let mut sum_squares = 0.0_f64;
    for sample in samples {
        let normalized = (*sample as f32 / i16::MAX as f32).abs().min(1.0);
        peak = peak.max(normalized);
        sum_squares += f64::from(normalized * normalized);
    }
    CaptureFrameMeter {
        peak,
        rms: (sum_squares / samples.len() as f64).sqrt() as f32,
    }
}

fn capture_voice_features(samples: &[i16], meter: CaptureFrameMeter) -> CaptureVoiceFeatures {
    if samples.len() < 2 || meter.rms <= f32::EPSILON {
        return CaptureVoiceFeatures {
            zero_crossing_rate: 0.0,
            crest_factor: 0.0,
        };
    }
    let mut crossings = 0usize;
    for pair in samples.windows(2) {
        if (pair[0] >= 0 && pair[1] < 0) || (pair[0] < 0 && pair[1] >= 0) {
            crossings += 1;
        }
    }
    CaptureVoiceFeatures {
        zero_crossing_rate: crossings as f32 / (samples.len() - 1) as f32,
        crest_factor: meter.peak / meter.rms.max(0.000_001),
    }
}

fn suppress_capture_transient_frame(profile: ProcessingProfile, frame: &mut [i16]) -> f32 {
    let meter = capture_frame_meter(frame);
    let features = capture_voice_features(frame, meter);
    let params = capture_gate_params(profile);
    if meter.rms < params.open_rms || features.crest_factor <= params.transient_crest_factor {
        return 0.0;
    }
    let reduction = match profile {
        ProcessingProfile::VoiceIsolation => 0.12,
        ProcessingProfile::Voice => 0.28,
        ProcessingProfile::Broadcast => 0.65,
        ProcessingProfile::Raw => 1.0,
    };
    if reduction >= 1.0 {
        return 0.0;
    }
    for sample in frame {
        *sample = ((*sample as f32) * reduction)
            .clamp(i16::MIN as f32, i16::MAX as f32)
            .round() as i16;
    }
    -20.0 * reduction.log10()
}

fn capture_gate_params(profile: ProcessingProfile) -> CaptureGateParams {
    match profile {
        ProcessingProfile::Raw => CaptureGateParams {
            open_rms: 0.0,
            close_rms: 0.0,
            open_frames: 1,
            hold_frames: 0,
            closed_gain: 1.0,
            attack: 1.0,
            release: 1.0,
            max_zero_crossing_rate: 1.0,
            max_crest_factor: f32::MAX,
            transient_crest_factor: f32::MAX,
        },
        ProcessingProfile::Broadcast => CaptureGateParams {
            open_rms: 0.005,
            close_rms: 0.002,
            open_frames: 1,
            hold_frames: 20,
            closed_gain: 0.22,
            attack: 0.45,
            release: 0.07,
            max_zero_crossing_rate: 0.45,
            max_crest_factor: 16.0,
            transient_crest_factor: 18.0,
        },
        ProcessingProfile::Voice => CaptureGateParams {
            open_rms: 0.009,
            close_rms: 0.0035,
            open_frames: 2,
            hold_frames: 14,
            closed_gain: 0.08,
            attack: 0.55,
            release: 0.11,
            max_zero_crossing_rate: 0.36,
            max_crest_factor: 11.0,
            transient_crest_factor: 10.0,
        },
        ProcessingProfile::VoiceIsolation => CaptureGateParams {
            open_rms: 0.013,
            close_rms: 0.0055,
            open_frames: 3,
            hold_frames: 10,
            closed_gain: 0.008,
            attack: 0.65,
            release: 0.18,
            max_zero_crossing_rate: 0.30,
            max_crest_factor: 8.0,
            transient_crest_factor: 7.0,
        },
    }
}

struct OutputAdapter {
    playback: Arc<Mutex<PlaybackBuffer>>,
    channels: usize,
    ratio: f32,
    phase: f32,
    previous_left: f32,
    previous_right: f32,
    current_left: i16,
    current_right: i16,
    next_left: i16,
    next_right: i16,
    initialized: bool,
    audio_settings: Arc<AudioSettings>,
}

impl OutputAdapter {
    fn new(
        playback: Arc<Mutex<PlaybackBuffer>>,
        output_rate: u32,
        channels: usize,
        audio_settings: Arc<AudioSettings>,
    ) -> Self {
        Self {
            playback,
            channels: channels.max(1),
            ratio: MIX_SAMPLE_RATE as f32 / output_rate as f32,
            phase: 0.0,
            previous_left: 0.0,
            previous_right: 0.0,
            current_left: 0,
            current_right: 0,
            next_left: 0,
            next_right: 0,
            initialized: false,
            audio_settings,
        }
    }

    fn fill<T, F>(&mut self, data: &mut [T], convert: F)
    where
        T: Copy,
        F: Fn(i16) -> T,
    {
        for frame in data.chunks_mut(self.channels) {
            let (left, right) = self.next_stereo();
            if self.channels == 1 {
                frame[0] = convert(((left as i32 + right as i32) / 2) as i16);
            } else {
                frame[0] = convert(left);
                frame[1] = convert(right);
                for output in &mut frame[2..] {
                    *output = convert(0);
                }
            }
        }
    }

    fn next_stereo(&mut self) -> (i16, i16) {
        let mut playback = self.playback.lock().unwrap();

        if !self.initialized {
            let (left, right) = playback.pop_stereo().unwrap_or((0, 0));
            let (next_left, next_right) = playback.pop_stereo().unwrap_or((left, right));
            self.current_left = left;
            self.current_right = right;
            self.next_left = next_left;
            self.next_right = next_right;
            self.previous_left = self.current_left as f32;
            self.previous_right = self.current_right as f32;
            self.initialized = true;
        }

        while self.phase >= 1.0 {
            self.current_left = self.next_left;
            self.current_right = self.next_right;
            let (next_left, next_right) = playback
                .pop_stereo()
                .unwrap_or((self.current_left, self.current_right));
            self.next_left = next_left;
            self.next_right = next_right;
            self.phase -= 1.0;
        }

        let left = self.current_left as f32
            + (self.next_left as f32 - self.current_left as f32) * self.phase;
        let right = self.current_right as f32
            + (self.next_right as f32 - self.current_right as f32) * self.phase;
        self.phase += self.ratio;
        let gain = self.audio_settings.speaker_gain();
        let left = apply_gain(left, gain);
        let right = apply_gain(right, gain);
        self.previous_left = left as f32;
        self.previous_right = right as f32;
        (left, right)
    }
}

fn apply_gain(sample: f32, gain: f32) -> i16 {
    (sample * gain)
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

fn i16_to_unit_f32(sample: i16) -> f32 {
    (sample as f32 / i16::MAX as f32).clamp(-1.0, 1.0)
}

fn u16_to_unit_f32(sample: u16) -> f32 {
    ((sample as f32 - 32768.0) / 32768.0).clamp(-1.0, 1.0)
}

fn unit_f32_to_i16(sample: f32) -> i16 {
    (sample * i16::MAX as f32)
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

fn soft_limit_unit(sample: f32) -> f32 {
    const KNEE: f32 = 0.85;
    let magnitude = sample.abs();
    if magnitude <= KNEE {
        return sample;
    }
    let over = (magnitude - KNEE) / (1.0 - KNEE);
    let limited = KNEE + (1.0 - KNEE) * (over / (1.0 + over));
    sample.signum() * limited.min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::Duration;

    use client_core::{
        apply_control_event, fail_pending_control_responses, next_reconnect_delay,
        CONTROL_RECONNECT_INITIAL, CONTROL_RECONNECT_MAX,
    };
    use common::{AlertTarget, ControlEvent, TalkButtonConfig, SAMPLES_PER_FRAME};

    fn config() -> ClientConfig {
        ClientConfig {
            user_id: 1,
            client_uid: "test-client".to_string(),
            role: ClientRole::Client,
            name: String::new(),
            listen: vec![1],
            tx: vec![1],
            codec: Codec::Pcm16,
            opus_profile: OpusProfile::default(),
            talk_mode: TalkMode::Ptt,
            last_non_muted_talk_mode: TalkMode::Ptt,
            regular_talk_active: true,
            priority: false,
            priority_channels: Vec::new(),
            processing: ProcessingConfig::default(),
            channel_rosters: Vec::new(),
            emergency: None,
            vol: HashMap::new(),
            talker_vol: HashMap::new(),
            buttons: Vec::new(),
            active_buttons: Vec::new(),
            active_direct_calls: Vec::new(),
            last_direct_caller: None,
            direct_call_history: Vec::new(),
            active_alerts: Vec::new(),
            recent_alerts: Vec::new(),
            advertised_buttons: Vec::new(),
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: Esp32AudioConfig::default(),
        }
    }

    fn test_capture_processing_settings() -> Arc<CaptureProcessingSettings> {
        Arc::new(CaptureProcessingSettings::new(&ProcessingConfig {
            mode: ProcessingMode::Disabled,
            ..ProcessingConfig::default()
        }))
    }

    fn api_state() -> (ApiState, mpsc::Receiver<ControlRequest>) {
        let (control_tx, control_rx) = mpsc::channel(8);
        (
            ApiState {
                config: Arc::new(Mutex::new(config())),
                control_tx,
                audio_settings: Arc::new(AudioSettings::new(1.0, 1.0)),
                input_backend_status: Arc::new(Mutex::new(InputBackendStatus::active(
                    AudioInputBackend::Auto,
                    AudioInputBackend::Raw,
                    Some("test backend".to_string()),
                ))),
                playback: Arc::new(Mutex::new(PlaybackBuffer::new(
                    SAMPLES_PER_FRAME * 4,
                    SAMPLES_PER_FRAME,
                ))),
                latest_telemetry: Arc::new(Mutex::new(None)),
                connection_status: Arc::new(Mutex::new(ClientConnectionEvent::Connected)),
            },
            control_rx,
        )
    }

    #[test]
    fn user_id_is_optional_for_server_assigned_enrollment() {
        let args = Args::try_parse_from(["desktop"]).unwrap();
        assert_eq!(args.user_id, None);
    }

    #[test]
    fn command_updates_tx_channel() {
        let mut config = config();

        let update = apply_runtime_command(&mut config, "tx 2,3").unwrap();

        assert_eq!(update, CommandUpdate::SendConfig);
        assert_eq!(config.tx, vec![2, 3]);
        assert_eq!(config.active_tx_channels(), vec![2, 3]);
    }

    #[test]
    fn mute_and_unmute_change_talk_mode_without_changing_tx() {
        let mut config = config();
        apply_runtime_command(&mut config, "tx 4").unwrap();
        apply_runtime_command(&mut config, "mute").unwrap();
        assert_eq!(config.tx, vec![4]);
        assert!(config.active_tx_channels().is_empty());

        apply_runtime_command(&mut config, "unmute").unwrap();

        assert_eq!(config.tx, vec![4]);
        assert!(config.active_tx_channels().is_empty());
        apply_runtime_command(&mut config, "talk on").unwrap();
        assert_eq!(config.active_tx_channels(), vec![4]);

        apply_runtime_command(&mut config, "talk-mode open").unwrap();
        apply_runtime_command(&mut config, "talk off").unwrap();
        assert_eq!(config.active_tx_channels(), vec![4]);
        apply_runtime_command(&mut config, "mute").unwrap();
        assert!(config.active_tx_channels().is_empty());
        apply_runtime_command(&mut config, "unmute").unwrap();
        assert_eq!(config.talk_mode, TalkMode::Open);
        assert_eq!(config.active_tx_channels(), vec![4]);
    }

    #[test]
    fn talk_commands_control_mode_and_transient_talk() {
        let mut config = config();

        apply_runtime_command(&mut config, "talk off").unwrap();
        assert!(!config.regular_talk_active);
        assert!(config.active_tx_channels().is_empty());

        apply_runtime_command(&mut config, "talk on").unwrap();
        assert!(config.regular_talk_active);
        assert_eq!(config.active_tx_channels(), vec![1]);

        apply_runtime_command(&mut config, "talk-mode open").unwrap();
        assert_eq!(config.talk_mode, TalkMode::Open);
        apply_runtime_command(&mut config, "talk off").unwrap();
        assert_eq!(config.active_tx_channels(), vec![1]);
    }

    #[test]
    fn command_updates_listen_and_volumes() {
        let mut config = config();

        apply_runtime_command(&mut config, "listen 1,2").unwrap();
        apply_runtime_command(&mut config, "vol 2=0.6,3=0.25").unwrap();

        assert_eq!(config.listen, vec![1, 2]);
        assert_eq!(config.vol.get(&2), Some(&0.6));
        assert_eq!(config.vol.get(&3), Some(&0.25));
    }

    #[test]
    fn button_argument_parses_id_and_label() {
        let button = "director=Director".parse::<ButtonArg>().unwrap();
        assert_eq!(button.id, "director");
        assert_eq!(button.label, "Director");

        let capability = ButtonCapability::from(button);
        assert_eq!(capability.id, "director");
        assert_eq!(capability.label, "Director");
    }

    #[test]
    fn button_command_sends_button_events() {
        let mut config = config();
        config.advertised_buttons = vec![ButtonCapability {
            id: "director".to_string(),
            label: "Director".to_string(),
        }];

        assert_eq!(
            apply_runtime_command(&mut config, "button director down").unwrap(),
            CommandUpdate::SendButton {
                button_id: "director".to_string(),
                pressed: true
            }
        );
        assert!(apply_runtime_command(&mut config, "button unknown down").is_err());
    }

    #[test]
    fn active_buttons_union_with_default_ptt_route() {
        let mut config = config();
        config.tx = vec![1, 2];
        config.buttons = vec![
            TalkButtonConfig {
                id: "director".to_string(),
                label: "Director".to_string(),
                color: None,
                mode: common::TalkButtonMode::Momentary,
                actions: vec![common::TalkButtonAction::Transmit {
                    channels: vec![2, 3],
                    users: Vec::new(),
                    duck: false,
                }],
            },
            TalkButtonConfig {
                id: "pa".to_string(),
                label: "PA".to_string(),
                color: None,
                mode: common::TalkButtonMode::Latching,
                actions: vec![common::TalkButtonAction::Transmit {
                    channels: vec![9],
                    users: Vec::new(),
                    duck: false,
                }],
            },
        ];
        config.active_buttons = vec!["director".to_string(), "pa".to_string()];

        assert_eq!(config.active_tx_channels(), vec![1, 2, 3, 9]);

        config.talk_mode = TalkMode::Muted;
        config.regular_talk_active = false;
        assert_eq!(config.active_tx_channels(), vec![2, 3, 9]);
    }

    #[test]
    fn device_name_matching_is_case_insensitive_substring() {
        assert!(device_name_matches("MacBook Pro Microphone", "micro"));
        assert!(device_name_matches("External USB Headset", "usb head"));
        assert!(!device_name_matches("External USB Headset", "studio"));
    }

    #[test]
    fn audio_input_backend_json_uses_operator_names() {
        assert_eq!(
            serde_json::to_string(&AudioInputBackend::VoiceProcessing).unwrap(),
            "\"voice_processing\""
        );
        assert_eq!(
            serde_json::from_str::<AudioInputBackend>("\"raw\"").unwrap(),
            AudioInputBackend::Raw
        );
    }

    #[tokio::test]
    async fn capture_applies_gain_before_float_clamp() {
        let (tx, mut rx) = mpsc::channel(1);
        let audio_settings = Arc::new(AudioSettings::new(0.5, 1.0));
        let diagnostics = CaptureDiagnostics::new(InputChannelMode::Average);
        let mut capture = CaptureAdapter::new(
            tx,
            MIX_SAMPLE_RATE,
            1,
            audio_settings,
            CapturePipelineOptions {
                channel_mode: InputChannelMode::Average,
                input_limiter: false,
                input_transient_suppression: false,
                input_silence_gate: false,
                processing_settings: test_capture_processing_settings(),
                diagnostics: Some(diagnostics.clone()),
                telemetry: None,
                debug_audio_tap: None,
            },
        );

        capture.push_interleaved_f32(std::iter::repeat_n(1.5, MIX_SAMPLES_PER_FRAME));
        let frame = rx.recv().await.unwrap();

        let normalized = frame[0] as f32 / i16::MAX as f32;
        assert!((normalized - 0.75).abs() < 0.01);
        let desktop = diagnostics.snapshot().desktop.unwrap();
        assert!(desktop.pre_gain_clipped_samples > 0);
        assert_eq!(desktop.post_gain_clipped_samples, 0);
    }

    #[tokio::test]
    async fn optional_input_limiter_reduces_output_but_reports_post_gain_clip() {
        let (tx, mut rx) = mpsc::channel(1);
        let audio_settings = Arc::new(AudioSettings::new(1.0, 1.0));
        let diagnostics = CaptureDiagnostics::new(InputChannelMode::Average);
        let mut capture = CaptureAdapter::new(
            tx,
            MIX_SAMPLE_RATE,
            1,
            audio_settings,
            CapturePipelineOptions {
                channel_mode: InputChannelMode::Average,
                input_limiter: true,
                input_transient_suppression: false,
                input_silence_gate: false,
                processing_settings: test_capture_processing_settings(),
                diagnostics: Some(diagnostics.clone()),
                telemetry: None,
                debug_audio_tap: None,
            },
        );

        capture.push_interleaved_f32(std::iter::repeat_n(1.4, MIX_SAMPLES_PER_FRAME));
        let frame = rx.recv().await.unwrap();

        assert!(frame[0] < i16::MAX);
        let desktop = diagnostics.snapshot().desktop.unwrap();
        assert!(desktop.post_gain_clipped_samples > 0);
    }

    #[tokio::test]
    async fn input_transient_suppression_reduces_sudden_keyboard_like_impulse() {
        let (tx, mut rx) = mpsc::channel(1);
        let audio_settings = Arc::new(AudioSettings::new(1.0, 1.0));
        let mut capture = CaptureAdapter::new(
            tx,
            MIX_SAMPLE_RATE,
            1,
            audio_settings,
            CapturePipelineOptions {
                channel_mode: InputChannelMode::Average,
                input_limiter: false,
                input_transient_suppression: true,
                input_silence_gate: false,
                processing_settings: Arc::new(CaptureProcessingSettings::new(&ProcessingConfig {
                    mode: ProcessingMode::Enabled,
                    profile: ProcessingProfile::VoiceIsolation,
                    ..ProcessingConfig::default()
                })),
                diagnostics: None,
                telemetry: None,
                debug_audio_tap: None,
            },
        );

        let mut samples = vec![0.01; MIX_SAMPLES_PER_FRAME];
        samples[120] = 1.0;
        capture.push_interleaved_f32(samples);
        let frame = rx.recv().await.unwrap();

        let peak = frame.iter().map(|sample| sample.abs()).max().unwrap();
        assert!(peak < (i16::MAX as f32 * 0.25) as i16);
    }

    #[tokio::test]
    async fn input_silence_gate_reduces_low_level_residue_and_opens_for_speech() {
        let (tx, mut rx) = mpsc::channel(4);
        let audio_settings = Arc::new(AudioSettings::new(1.0, 1.0));
        let mut capture = CaptureAdapter::new(
            tx,
            MIX_SAMPLE_RATE,
            1,
            audio_settings,
            CapturePipelineOptions {
                channel_mode: InputChannelMode::Average,
                input_limiter: false,
                input_transient_suppression: false,
                input_silence_gate: true,
                processing_settings: Arc::new(CaptureProcessingSettings::new(&ProcessingConfig {
                    mode: ProcessingMode::Enabled,
                    profile: ProcessingProfile::Voice,
                    vad: false,
                    transient_suppression: false,
                    ..ProcessingConfig::default()
                })),
                diagnostics: None,
                telemetry: None,
                debug_audio_tap: None,
            },
        );

        capture.push_interleaved_f32(std::iter::repeat_n(0.003, MIX_SAMPLES_PER_FRAME));
        let quiet = rx.recv().await.unwrap();
        let quiet_peak = quiet.iter().map(|sample| sample.abs()).max().unwrap();
        assert!(quiet_peak < 100);

        capture.push_interleaved_f32(std::iter::repeat_n(0.06, MIX_SAMPLES_PER_FRAME * 2));
        let speech = rx.recv().await.unwrap();
        let speech_peak = speech.iter().map(|sample| sample.abs()).max().unwrap();
        assert!(speech_peak > 1000);
    }

    #[test]
    fn playback_buffer_waits_for_prebuffer() {
        let mut playback = PlaybackBuffer::new(SAMPLES_PER_FRAME * 4, SAMPLES_PER_FRAME * 2);

        playback.push(&vec![1; SAMPLES_PER_FRAME]);
        assert_eq!(playback.pop(), None);

        playback.push(&vec![2; SAMPLES_PER_FRAME]);
        assert_eq!(playback.pop(), Some(1));
    }

    #[test]
    fn playback_buffer_resets_after_underrun() {
        let mut playback = PlaybackBuffer::new(SAMPLES_PER_FRAME * 4, SAMPLES_PER_FRAME);

        playback.push(&vec![1; SAMPLES_PER_FRAME]);
        for _ in 0..SAMPLES_PER_FRAME {
            assert_eq!(playback.pop(), Some(1));
        }
        assert_eq!(playback.pop(), None);

        playback.push(&vec![2; SAMPLES_PER_FRAME - 1]);
        assert_eq!(playback.pop(), None);
        playback.push(&[2]);
        assert_eq!(playback.pop(), Some(2));
    }

    #[test]
    fn jitter_zero_disables_prebuffer() {
        assert_eq!(samples_for_ms(0), 0);
        let mut playback = PlaybackBuffer::new(SAMPLES_PER_FRAME, samples_for_ms(0));

        playback.push(&[7]);

        assert_eq!(playback.pop(), Some(7));
    }

    #[test]
    fn reconnect_delay_backs_off_to_cap() {
        assert_eq!(
            next_reconnect_delay(CONTROL_RECONNECT_INITIAL),
            Duration::from_secs(1)
        );
        assert_eq!(
            next_reconnect_delay(CONTROL_RECONNECT_MAX),
            CONTROL_RECONNECT_MAX
        );
    }

    #[test]
    fn pending_control_responses_fail_on_disconnect() {
        let (response_tx, mut response_rx) = oneshot::channel();
        let mut pending = VecDeque::from([response_tx]);

        fail_pending_control_responses(&mut pending);

        assert!(pending.is_empty());
        assert!(matches!(
            response_rx.try_recv().unwrap(),
            ControlResponse::Error { .. }
        ));
    }

    #[tokio::test]
    async fn local_ui_state_returns_current_snapshot() {
        let (state, _control_rx) = api_state();

        let Json(response) = state_handler(State(state)).await;

        assert_eq!(response.user_id, 1);
        assert_eq!(response.listen, vec![1]);
        assert_eq!(response.tx, vec![1]);
        assert_eq!(response.codec, Codec::Pcm16);
        assert_eq!(response.mic_gain, 1.0);
        assert_eq!(response.speaker_gain, 1.0);
        assert_eq!(
            response.requested_input_backend,
            ClientAudioBackendKind::Auto
        );
        assert_eq!(
            response.active_input_backend,
            Some(ClientAudioBackendKind::Raw)
        );
        assert_eq!(response.input_backend_note.as_deref(), Some("test backend"));
        assert_eq!(response.playback.available_samples, 0);
        assert_eq!(response.playback.underflows, 0);
        assert_eq!(response.lockout, ClientLockoutPolicy::default());
        assert_eq!(
            response.supported_codecs,
            vec![Codec::Pcm16, Codec::Pcm24, Codec::Pcm48, Codec::Opus]
        );
    }

    #[tokio::test]
    async fn local_ui_rejects_locked_codec_control() {
        let (state, mut control_rx) = api_state();
        state.config.lock().unwrap().lockout.allow_codec = false;

        let result = codec_handler(State(state), Json(CodecRequest { codec: Codec::Opus })).await;

        assert!(matches!(
            result,
            Err(ApiError::BadRequest(message)) if message.contains("codec")
        ));
        assert!(control_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn local_ui_mute_enqueues_muted_talk_mode() {
        let (state, mut control_rx) = api_state();
        let handle = tokio::spawn(mute_handler(State(state)));

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::TalkMode {
                user_id: 1,
                mode: TalkMode::Muted
            }
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();

        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn local_ui_unmute_restores_previous_non_muted_talk_mode() {
        let (state, mut control_rx) = api_state();
        {
            let mut config = state.config.lock().unwrap();
            config.set_talk_mode(TalkMode::Open);
            config.set_talk_mode(TalkMode::Muted);
        }
        let handle = tokio::spawn(unmute_handler(State(state)));

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::TalkMode {
                user_id: 1,
                mode: TalkMode::Open
            }
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();

        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn local_ui_talk_down_enqueues_regular_talk_active() {
        let (state, mut control_rx) = api_state();
        let handle = tokio::spawn(talk_down_handler(State(state)));

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::Talk {
                user_id: 1,
                active: true
            }
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();

        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn no_wait_talk_queue_updates_local_state_without_control_ack() {
        let (state, mut control_rx) = api_state();

        assert_eq!(state.queue_talk(true).unwrap(), OkResponse { ok: true });
        assert!(state.config.lock().unwrap().regular_talk_active);

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::Talk {
                user_id: 1,
                active: true
            }
        ));
    }

    #[tokio::test]
    async fn no_wait_button_queue_updates_local_state_without_control_ack() {
        let (state, mut control_rx) = api_state();
        state.config.lock().unwrap().buttons = vec![TalkButtonConfig {
            id: "director".to_string(),
            label: "Director".to_string(),
            color: None,
            mode: TalkButtonMode::Latching,
            actions: Vec::new(),
        }];

        assert_eq!(
            state.queue_button("director".to_string(), true).unwrap(),
            OkResponse { ok: true }
        );
        assert_eq!(
            state.config.lock().unwrap().active_buttons,
            vec!["director".to_string()]
        );

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::Button {
                user_id: 1,
                ref button_id,
                pressed: true
            } if button_id == "director"
        ));
    }

    #[tokio::test]
    async fn local_ui_button_down_enqueues_button_press() {
        let (state, mut control_rx) = api_state();
        let handle = tokio::spawn(button_down_handler(
            State(state),
            Path("director".to_string()),
        ));

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::Button {
                user_id: 1,
                ref button_id,
                pressed: true
            } if button_id == "director"
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();

        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn local_ui_alert_endpoints_enqueue_control_messages() {
        let (state, mut control_rx) = api_state();
        let handle = tokio::spawn(alert_send_handler(
            State(state.clone()),
            Json(AlertRequest {
                target: AlertTarget::User(2),
                message: Some("Call me".to_string()),
            }),
        ));
        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::SendAlert {
                user_id: 1,
                target: AlertTarget::User(2),
                ref message,
            } if message.as_deref() == Some("Call me")
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();
        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });

        let handle = tokio::spawn(alert_ack_handler(State(state.clone()), Path(42)));
        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::AckAlert {
                user_id: 1,
                alert_id: 42
            }
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();
        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });

        let handle = tokio::spawn(alert_cancel_handler(State(state), Path(42)));
        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::CancelAlert {
                user_id: 1,
                alert_id: 42
            }
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();
        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn local_ui_config_sends_config_and_priority_in_order() {
        let (state, mut control_rx) = api_state();
        let handle = tokio::spawn(config_handler(
            State(state),
            Json(FullConfigRequest {
                listen: vec![1, 2],
                tx: vec![2],
                vol: [(2, 0.6)].into(),
                talker_vol: HashMap::new(),
                codec: Codec::Pcm48,
                opus_profile: OpusProfile::default(),
                talk_mode: TalkMode::Muted,
                priority: true,
                priority_channels: Vec::new(),
                ifb: IfbConfig {
                    enabled: true,
                    program: vec![1],
                    interrupt: vec![9],
                    duck_gain: 0.125,
                },
            }),
        ));

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::Config {
                user_id: 1,
                ref listen,
                ref tx,
                codec: Some(Codec::Pcm48),
                opus_profile: Some(OpusProfile::Speech24Standard),
                talk_mode: Some(TalkMode::Muted),
                priority: Some(true),
                ..
            } if listen == &vec![1, 2] && tx == &vec![2]
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::Priority {
                user_id: 1,
                active: true
            }
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();

        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn local_ui_gain_updates_runtime_audio_settings() {
        let (state, _control_rx) = api_state();
        let settings = Arc::clone(&state.audio_settings);

        let Json(response) = gain_handler(
            State(state),
            Json(GainRequest {
                mic_gain: Some(1.75),
                speaker_gain: Some(1.9),
            }),
        )
        .await
        .unwrap();

        assert_eq!(response, OkResponse { ok: true });
        assert_eq!(settings.mic_gain(), 1.75);
        assert_eq!(settings.speaker_gain(), 1.9);
    }

    #[test]
    fn runtime_gain_is_clamped() {
        let settings = AudioSettings::new(f32::NAN, 99.0);

        assert_eq!(settings.mic_gain(), 1.0);
        assert_eq!(settings.speaker_gain(), 2.0);
    }

    #[test]
    fn local_ui_contains_full_config_controls() {
        assert!(LOCAL_UI_HTML.contains("Operator Console"));
        assert!(LOCAL_UI_HTML.contains("client-api.js"));
        assert!(LOCAL_UI_HTML.contains("id=\"client-title\" hidden"));
        assert!(LOCAL_UI_HTML.contains(">Runtime</button>"));
        assert!(LOCAL_UI_HTML.contains("Runtime Controls"));
        assert!(LOCAL_UI_HTML.contains("show-all-channels"));
        assert!(LOCAL_UI_HTML.contains("channel-view-toggle"));
        assert!(LOCAL_UI_HTML.contains("identity-card"));
        assert!(LOCAL_UI_HTML.contains("identity-card-value"));
        assert!(LOCAL_UI_HTML.contains("max=\"2\""));
        assert!(!LOCAL_UI_HTML.contains("Client Settings"));
        assert!(LOCAL_UI_API_JS.contains("fetch("));
        assert!(LOCAL_UI_API_JS.contains("runtimeSettings: !mobileShell()"));
        assert!(!LOCAL_UI_JS.contains("fetch("));
        assert!(!LOCAL_UI_JS.contains("clientApi?.name || 'client'} controls"));
        assert!(LOCAL_UI_HTML.contains("dock-status"));
        assert!(LOCAL_UI_JS.contains("Server connected"));
        assert!(LOCAL_UI_JS.contains("'Hold Talk'"));
        assert!(LOCAL_UI_CSS.contains(".tag.connected"));
        assert!(LOCAL_UI_HTML.contains("route-editor"));
        assert!(LOCAL_UI_HTML.contains("talker-gain-editor"));
        assert!(LOCAL_UI_HTML.contains("ifb-editor"));
        assert!(LOCAL_UI_HTML.contains("route-add-listen"));
        assert!(LOCAL_UI_HTML.contains("mic-gain-input"));
        assert!(LOCAL_UI_JS.contains("function renderRouteEditor()"));
        assert!(LOCAL_UI_JS.contains("function renderTalkerGainEditor()"));
        assert!(LOCAL_UI_JS.contains("function renderIfbEditor()"));
        assert!(LOCAL_UI_JS.contains("function saveConfig()"));
        assert!(LOCAL_UI_JS.contains("const buttons = state.buttons || []"));
        assert!(LOCAL_UI_JS.contains("mode === 'latching'"));
        assert!(LOCAL_UI_JS.contains("Hold to run this action; release to stop transmit actions."));
        assert!(LOCAL_UI_HTML.contains("id=\"opus-profile-field\""));
        assert!(LOCAL_UI_JS.contains("function updateOpusProfileVisibility()"));
        assert!(LOCAL_UI_JS.contains("addEventListener('change', updateOpusProfileVisibility)"));
        assert!(LOCAL_UI_JS.contains("heldButtons"));
        assert!(LOCAL_UI_HTML.contains("alerts-panel"));
        assert!(LOCAL_UI_JS.contains("function renderAlerts()"));
        assert!(LOCAL_UI_JS.contains("ackAlert"));
        assert!(LOCAL_UI_HTML.contains("cue-panel"));
        assert!(LOCAL_UI_JS.contains("function activeIfbSources()"));
        assert!(LOCAL_UI_JS.contains("function renderCuePanel()"));
        assert!(LOCAL_UI_JS.contains("data-reply-direct-call"));
        assert!(LOCAL_UI_JS.contains("Call from"));
        assert!(LOCAL_UI_JS.contains("Alert from"));
        assert!(LOCAL_UI_HTML.contains("bottom-special"));
        assert!(
            LOCAL_UI_HTML.find("id=\"buttons\"").unwrap()
                < LOCAL_UI_HTML.find("id=\"mute\"").unwrap()
        );
        assert!(LOCAL_UI_JS.contains("expandedChannels"));
        assert!(LOCAL_UI_JS.contains("function rosterForChannel"));
        assert!(LOCAL_UI_JS.contains("aria-expanded"));
        assert!(LOCAL_UI_HTML.contains("channel-settings-modal"));
        assert!(LOCAL_UI_HTML.contains("channel-listen-toggle"));
        assert!(LOCAL_UI_HTML.contains("channel-tx-toggle"));
        assert!(LOCAL_UI_JS.contains("function bindChannelSettingsGesture"));
        assert!(LOCAL_UI_JS.contains("function saveChannelSettings()"));
        assert!(LOCAL_UI_JS.contains("showAllChannels"));
        assert!(LOCAL_UI_JS.contains("function allConfiguredChannels()"));
        assert!(LOCAL_UI_JS.contains("function setShowAllChannels"));
        assert!(LOCAL_UI_JS.contains("function assignedClientLabel()"));
        assert!(LOCAL_UI_JS.contains("User ID"));
        assert!(LOCAL_UI_JS.contains("function channelIconTag"));
        assert!(LOCAL_UI_JS.contains("function channelStatusTags"));
        assert!(LOCAL_UI_CSS.contains(".tag.icon-tag"));
        assert!(LOCAL_UI_CSS.contains(".tag.tx.talking"));
        assert!(!LOCAL_UI_JS.contains(">listening<"));
        assert!(!LOCAL_UI_JS.contains(">regular tx<"));
        assert!(!LOCAL_UI_JS.contains(">talking<"));
        assert!(LOCAL_UI_JS.contains("contextmenu"));
        assert!(LOCAL_UI_HTML.contains("macos-mic-mode-open"));
        assert!(LOCAL_UI_JS.contains("macos_microphone_mode"));
        assert!(LOCAL_UI_JS.contains("/macos/microphone-modes"));
        assert!(LOCAL_UI_HTML.contains("setup-open"));
        assert!(LOCAL_UI_JS.contains("function mobileShell()"));
        assert!(LOCAL_UI_JS.contains("history.back()"));
        assert!(!LOCAL_UI_HTML.contains("special-panel"));
    }

    #[test]
    fn config_update_from_server_replaces_local_route_and_talk_mode() {
        let config = Arc::new(Mutex::new(config()));
        apply_control_event(
            &config,
            ControlEvent::ConfigUpdate {
                user_id: 1,
                client_uid: "test-client".to_string(),
                name: "Ref 1".to_string(),
                listen: vec![2, 3],
                tx: vec![3],
                vol: [(2, 0.5)].into(),
                talker_vol: [(4, 0.75)].into(),
                codec: Codec::Pcm16,
                opus_profile: OpusProfile::default(),
                talk_mode: TalkMode::Muted,
                regular_talk_active: false,
                priority: true,
                priority_channels: Vec::new(),
                processing: ProcessingConfig::default(),
                buttons: vec![TalkButtonConfig {
                    id: "director".to_string(),
                    label: "Director".to_string(),
                    color: None,
                    mode: common::TalkButtonMode::Momentary,
                    actions: vec![common::TalkButtonAction::Transmit {
                        channels: vec![5],
                        users: Vec::new(),
                        duck: false,
                    }],
                }],
                active_buttons: vec!["director".to_string()],
                active_direct_calls: Vec::new(),
                last_direct_caller: None,
                direct_call_history: Vec::new(),
                active_alerts: Vec::new(),
                recent_alerts: Vec::new(),
                emergency: None,
                ifb: IfbConfig::default(),
                lockout: ClientLockoutPolicy {
                    allow_codec: false,
                    ..ClientLockoutPolicy::default()
                },
                stereo: StereoConfig::default(),
                esp32_audio: Esp32AudioConfig::default(),
            },
        );

        let config = config.lock().unwrap();
        assert_eq!(config.listen, vec![2, 3]);
        assert_eq!(config.name, "Ref 1");
        assert_eq!(config.tx, vec![3]);
        assert_eq!(config.active_tx_channels(), vec![5]);
        assert_eq!(config.vol.get(&2), Some(&0.5));
        assert_eq!(config.codec, Codec::Pcm16);
        assert_eq!(config.talk_mode, TalkMode::Muted);
        assert!(!config.regular_talk_active);
        assert!(config.priority);
        assert_eq!(config.buttons[0].id, "director");
        assert_eq!(config.active_buttons, vec!["director"]);
        assert!(!config.lockout.allow_codec);
    }
}
