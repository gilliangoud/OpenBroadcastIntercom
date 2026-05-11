use std::collections::HashMap;
use std::future;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
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
    basic_client_telemetry, bind_tcp_listener_with_port_fallback, default_button_capabilities,
    load_or_create_client_uid, merge_button_capabilities, run_connection_cue_task,
    run_control_connection, samples_for_ms, send_control_message, send_control_request,
    supported_codecs, AudioDecoder, AudioEncoder, ClientConfig, ClientConnectionEvent,
    ClientServerEndpoint, ClientTelemetryCounters, ControlRequest, PlaybackBuffer, PlaybackStats,
    DEFAULT_CLIENT_BUTTON_COUNT, DEFAULT_CONTROL_PORT, DEFAULT_SERVER_HOST,
    MAX_CLIENT_BUTTON_COUNT,
};
use common::{
    codec_samples_per_frame, AlertId, AlertStatus, AlertTarget, AudioPacket, ButtonCapability,
    ButtonId, CaptureHealthStatus, ChannelPresenceRoster, ClientCapabilities, ClientLockoutPolicy,
    ClientRole, Codec, ControlMessage, ControlResponse, DirectCallHistoryEntry, DirectCallStatus,
    EmergencyStatus, Esp32AudioConfig, IfbConfig, OpusProfile, ProcessingConfig, StereoConfig,
    TalkButtonConfig, TalkMode, MIX_SAMPLES_PER_FRAME, MIX_SAMPLE_RATE,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long)]
    server_host: Option<String>,
    #[arg(long, default_value = "127.0.0.1:40000")]
    server: SocketAddr,
    #[arg(long, default_value = "ws://127.0.0.1:40001")]
    control: String,
    #[arg(long)]
    user_id: Option<u16>,
    #[arg(long)]
    client_uid: Option<String>,
    #[arg(long)]
    identity_file: Option<std::path::PathBuf>,
    #[arg(long, default_value_t = 0)]
    tx_channel: u16,
    #[arg(long, default_value_t = 0)]
    listen_channel: u16,
    #[arg(long, value_enum, default_value_t = WireCodec::Pcm16)]
    codec: WireCodec,
    #[arg(long, value_enum, default_value_t = WireOpusProfile::Speech24Standard)]
    opus_profile: WireOpusProfile,
    #[arg(long, default_value_t = 1.0)]
    mic_gain: f32,
    #[arg(long, default_value_t = 1.0)]
    speaker_gain: f32,
    #[arg(long, default_value_t = 40, value_parser = clap::value_parser!(u32).range(0..=250))]
    jitter_ms: u32,
    #[arg(long)]
    input_device: Option<String>,
    #[arg(long)]
    output_device: Option<String>,
    #[arg(long)]
    receive_only: bool,
    #[arg(long, default_value_t = DEFAULT_CLIENT_BUTTON_COUNT, value_parser = clap::value_parser!(u16).range(0..=MAX_CLIENT_BUTTON_COUNT as i64))]
    button_count: u16,
    #[arg(long = "button", value_name = "ID[=LABEL]")]
    buttons: Vec<ButtonArg>,
    #[arg(long, default_value = "0.0.0.0:41001")]
    local_api_bind: SocketAddr,
    #[arg(long, env = "INTERCOM_LOCAL_API_TOKEN")]
    local_api_token: Option<String>,
    #[arg(long)]
    disable_local_api: bool,
    #[arg(long)]
    list_devices: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum WireCodec {
    Pcm16,
    Pcm24,
    Pcm48,
    Opus,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum WireOpusProfile {
    #[value(name = "speech-16-low", alias = "speech-low")]
    Speech16Low,
    #[value(name = "speech-24-standard", alias = "speech-standard")]
    Speech24Standard,
    #[value(name = "speech-48-high", alias = "speech-high")]
    Speech48High,
    #[value(name = "music-48", alias = "music-high")]
    Music48,
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
struct LocalHttpAuth {
    token: Option<Arc<str>>,
    realm: &'static str,
}

impl LocalHttpAuth {
    fn disabled() -> Self {
        Self::default()
    }

    fn token(token: impl Into<String>, realm: &'static str) -> Self {
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

const LOCAL_UI_HTML: &str = include_str!("../../shared-ui/talking/client-controls.html");
const LOCAL_UI_CSS: &str = include_str!("../../shared-ui/talking/client-controls.css");
const LOCAL_UI_JS: &str = include_str!("../../shared-ui/talking/client-controls.js");
const LOCAL_UI_API_JS: &str = include_str!("../../shared-ui/talking/client-api-http.js");

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
struct ButtonArg {
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("pi=info".parse()?))
        .init();

    let mut args = Args::parse();
    resolve_endpoint_args(&mut args)?;
    if args.list_devices {
        list_audio_devices()?;
        return Ok(());
    }

    let user_id = args.user_id.unwrap_or(0);
    let client_uid =
        load_or_create_client_uid(args.client_uid.as_deref(), args.identity_file.as_deref())?;
    let advertised_buttons = merge_button_capabilities(
        default_button_capabilities(args.button_count),
        args.buttons
            .clone()
            .into_iter()
            .map(ButtonCapability::from)
            .collect(),
    );
    let capabilities = ClientCapabilities::pi();
    let runtime_config = Arc::new(Mutex::new(ClientConfig {
        user_id,
        client_uid,
        role: ClientRole::Client,
        listen: vec![args.listen_channel],
        tx: if args.receive_only {
            Vec::new()
        } else {
            vec![args.tx_channel]
        },
        codec: Codec::from(args.codec),
        opus_profile: OpusProfile::from(args.opus_profile),
        name: String::new(),
        talk_mode: if args.receive_only {
            TalkMode::Muted
        } else {
            TalkMode::Ptt
        },
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
        capabilities: capabilities.clone(),
        ifb: IfbConfig::default(),
        lockout: ClientLockoutPolicy::default(),
        stereo: StereoConfig::default(),
        esp32_audio: Esp32AudioConfig::default(),
    }));
    let jitter_samples = samples_for_ms(args.jitter_ms);
    let playback = Arc::new(Mutex::new(PlaybackBuffer::new(
        jitter_samples + MIX_SAMPLES_PER_FRAME * 12,
        jitter_samples,
    )));
    let telemetry_counters = ClientTelemetryCounters::default();
    let latest_telemetry = Arc::new(Mutex::new(None));

    let (control_tx, control_rx) = mpsc::channel::<ControlRequest>(16);
    let (connection_event_tx, connection_event_rx) = mpsc::channel::<ClientConnectionEvent>(8);
    let (connection_cue_tx, connection_cue_rx) = mpsc::channel::<ClientConnectionEvent>(8);
    let connection_status = Arc::new(Mutex::new(ClientConnectionEvent::Reconnecting));
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
            args.control,
            control_rx,
            control_config,
            Some(connection_event_tx),
        )
        .await
    });

    let initial_config = runtime_config.lock().unwrap().clone();
    let hello_response = send_control_request(
        &control_tx,
        ControlMessage::Hello {
            user_id: initial_config.user_id,
            requested_user_id: (initial_config.user_id > 0).then_some(initial_config.user_id),
            client_uid: initial_config.client_uid.clone(),
            codecs: supported_codecs(),
            buttons: advertised_buttons.clone(),
            capabilities,
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
        send_control_message(&control_tx, startup_config.control_message()).await?;
    } else {
        tracing::info!("server has preconfigured state; waiting for config_update");
    }

    let local_api_task = if args.disable_local_api {
        tokio::spawn(future::pending::<anyhow::Result<()>>())
    } else {
        let local_api_auth = args
            .local_api_token
            .clone()
            .map_or_else(LocalHttpAuth::disabled, |token| {
                LocalHttpAuth::token(token, "Intercom Pi")
            });
        let local_api_state = ApiState {
            config: Arc::clone(&runtime_config),
            control_tx: control_tx.clone(),
            playback: Arc::clone(&playback),
            latest_telemetry: Arc::clone(&latest_telemetry),
            connection_status: Arc::clone(&connection_status),
        };
        let local_api_bind = args.local_api_bind;
        tokio::spawn(async move {
            let (listener, actual_bind) =
                bind_tcp_listener_with_port_fallback(local_api_bind).await?;
            if actual_bind != local_api_bind {
                tracing::warn!(
                    requested = %local_api_bind,
                    actual = %actual_bind,
                    "Pi local API bind address was in use; selected next available port"
                );
            }
            if local_api_auth.is_enabled() {
                tracing::info!(
                    bind = %actual_bind,
                    "Pi local control API listening with HTTP authorization"
                );
            } else {
                tracing::warn!(
                    bind = %actual_bind,
                    "Pi local control API listening without authentication"
                );
            }
            axum::serve(listener, local_api_router(local_api_state, local_api_auth))
                .await
                .context("serve Pi local control API")
        })
    };

    let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    socket.connect(args.server).await?;
    tracing::info!(local = %socket.local_addr()?, server = %args.server, "pi client connected");

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

    let telemetry_control_tx = control_tx.clone();
    let telemetry_config = Arc::clone(&runtime_config);
    let telemetry_playback = Arc::clone(&playback);
    let telemetry_counters_task = telemetry_counters.clone();
    let telemetry_latest = Arc::clone(&latest_telemetry);
    let telemetry_started_at = Instant::now();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            interval.tick().await;
            let user_id = telemetry_config.lock().unwrap().user_id;
            if user_id == 0 {
                continue;
            }
            let mut health = basic_client_telemetry(
                "pi",
                telemetry_playback.lock().unwrap().stats(),
                telemetry_counters_task.snapshot(),
            );
            health.uptime_ms = telemetry_started_at
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX);
            *telemetry_latest.lock().unwrap() = Some(health.clone());
            if let Err(err) = send_control_message(
                &telemetry_control_tx,
                ControlMessage::CaptureHealth { user_id, health },
            )
            .await
            {
                tracing::debug!(%err, "could not report pi client telemetry");
            }
        }
    });

    let input_stream = build_input_stream(
        mic_tx,
        args.mic_gain,
        args.input_device.as_deref(),
        telemetry_counters.clone(),
    )?;
    let output_stream = build_output_stream(
        Arc::clone(&playback),
        args.speaker_gain,
        args.output_device.as_deref(),
    )?;
    input_stream.play()?;
    output_stream.play()?;

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
        result = local_api_task => result.context("local API task panicked")??,
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
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
struct ApiState {
    config: Arc<Mutex<ClientConfig>>,
    control_tx: mpsc::Sender<ControlRequest>,
    playback: Arc<Mutex<PlaybackBuffer>>,
    latest_telemetry: Arc<Mutex<Option<CaptureHealthStatus>>>,
    connection_status: Arc<Mutex<ClientConnectionEvent>>,
}

#[derive(Debug, Serialize, PartialEq)]
struct OkResponse {
    ok: bool,
}

#[derive(Debug, Serialize, PartialEq)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug, Serialize, PartialEq)]
struct StateResponse {
    server_connection: ClientConnectionEvent,
    user_id: u16,
    client_uid: String,
    name: String,
    listen: Vec<u16>,
    tx: Vec<u16>,
    vol: HashMap<u16, f32>,
    talker_vol: HashMap<u16, f32>,
    codec: Codec,
    opus_profile: OpusProfile,
    talk_mode: TalkMode,
    regular_talk_active: bool,
    priority: bool,
    priority_channels: Vec<u16>,
    processing: ProcessingConfig,
    channel_rosters: Vec<ChannelPresenceRoster>,
    emergency: Option<EmergencyStatus>,
    supported_codecs: Vec<Codec>,
    buttons: Vec<TalkButtonConfig>,
    active_buttons: Vec<ButtonId>,
    active_direct_calls: Vec<DirectCallStatus>,
    last_direct_caller: Option<u16>,
    direct_call_history: Vec<DirectCallHistoryEntry>,
    active_alerts: Vec<AlertStatus>,
    recent_alerts: Vec<AlertStatus>,
    advertised_buttons: Vec<ButtonCapability>,
    ifb: IfbConfig,
    lockout: ClientLockoutPolicy,
    stereo: StereoConfig,
    playback: PlaybackStats,
    telemetry: Option<CaptureHealthStatus>,
}

impl StateResponse {
    fn from_state(
        config: &ClientConfig,
        playback: PlaybackStats,
        telemetry: Option<CaptureHealthStatus>,
        server_connection: ClientConnectionEvent,
    ) -> Self {
        Self {
            server_connection,
            user_id: config.user_id,
            client_uid: config.client_uid.clone(),
            name: config.name.clone(),
            listen: config.listen.clone(),
            tx: config.tx.clone(),
            vol: config.vol.clone(),
            talker_vol: config.talker_vol.clone(),
            codec: config.codec,
            opus_profile: config.opus_profile,
            talk_mode: config.talk_mode,
            regular_talk_active: config.regular_talk_active,
            priority: config.priority,
            priority_channels: config.priority_channels.clone(),
            processing: config.processing.clone(),
            channel_rosters: config.channel_rosters.clone(),
            emergency: config.emergency.clone(),
            supported_codecs: supported_codecs(),
            buttons: config.buttons.clone(),
            active_buttons: config.active_buttons.clone(),
            active_direct_calls: config.active_direct_calls.clone(),
            last_direct_caller: config.last_direct_caller,
            direct_call_history: config.direct_call_history.clone(),
            active_alerts: config.active_alerts.clone(),
            recent_alerts: config.recent_alerts.clone(),
            advertised_buttons: config.advertised_buttons.clone(),
            ifb: config.ifb.clone(),
            lockout: config.lockout.clone(),
            stereo: config.stereo.clone(),
            playback,
            telemetry,
        }
    }
}

#[derive(Debug, Deserialize)]
struct FullConfigRequest {
    listen: Vec<u16>,
    tx: Vec<u16>,
    #[serde(default)]
    vol: HashMap<u16, f32>,
    #[serde(default)]
    talker_vol: HashMap<u16, f32>,
    codec: Codec,
    #[serde(default)]
    opus_profile: OpusProfile,
    talk_mode: TalkMode,
    priority: bool,
    #[serde(default)]
    priority_channels: Vec<u16>,
    #[serde(default)]
    ifb: IfbConfig,
}

impl FullConfigRequest {
    fn control_messages(&self, user_id: u16) -> Vec<ControlMessage> {
        vec![
            ControlMessage::Config {
                user_id,
                role: None,
                name: None,
                listen: self.listen.clone(),
                tx: self.tx.clone(),
                vol: self.vol.clone(),
                talker_vol: Some(self.talker_vol.clone()),
                codec: Some(self.codec),
                opus_profile: Some(self.opus_profile),
                talk_mode: Some(self.talk_mode),
                priority: Some(self.priority),
                priority_channels: Some(self.priority_channels.clone()),
                processing: None,
                buttons: None,
                ifb: Some(self.ifb.clone()),
                stereo: None,
                esp32_audio: None,
            },
            ControlMessage::Priority {
                user_id,
                active: self.priority,
            },
        ]
    }
}

#[derive(Debug, Deserialize)]
struct CodecRequest {
    codec: Codec,
}

#[derive(Debug, Deserialize)]
struct TalkModeRequest {
    mode: TalkMode,
}

#[derive(Debug, Deserialize)]
struct AlertRequest {
    target: AlertTarget,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug)]
enum ApiError {
    BadRequest(String),
    Unavailable(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            Self::BadRequest(error) => (StatusCode::BAD_REQUEST, error),
            Self::Unavailable(error) => (StatusCode::SERVICE_UNAVAILABLE, error),
        };
        (status, Json(ErrorResponse { error })).into_response()
    }
}

fn local_api_router(state: ApiState, auth: LocalHttpAuth) -> Router {
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

async fn state_handler(State(state): State<ApiState>) -> Json<StateResponse> {
    let snapshot = state.config.lock().unwrap().clone();
    let playback = state.playback.lock().unwrap().stats();
    let telemetry = state.latest_telemetry.lock().unwrap().clone();
    let server_connection = *state.connection_status.lock().unwrap();
    Json(StateResponse::from_state(
        &snapshot,
        playback,
        telemetry,
        server_connection,
    ))
}

async fn config_handler(
    State(state): State<ApiState>,
    Json(request): Json<FullConfigRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    validate_full_config_allowed(&snapshot, &request)?;
    let user_id = snapshot.user_id;
    for message in request.control_messages(user_id) {
        forward_control_message(&state.control_tx, message).await?;
    }
    Ok(Json(OkResponse { ok: true }))
}

async fn talk_mode_handler(
    State(state): State<ApiState>,
    Json(request): Json<TalkModeRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    ensure_control_allowed(&state, |lockout| lockout.allow_talk_mode, "talk mode")?;
    send_talk_mode(&state, request.mode).await
}

async fn mute_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    ensure_control_allowed(&state, |lockout| lockout.allow_talk_mode, "talk mode")?;
    send_talk_mode(&state, TalkMode::Muted).await
}

async fn unmute_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    ensure_policy_allowed(
        &snapshot.lockout,
        |lockout| lockout.allow_talk_mode,
        "talk mode",
    )?;
    let talk_mode = snapshot.restored_unmute_talk_mode();
    send_talk_mode(&state, talk_mode).await
}

async fn talk_down_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    ensure_control_allowed(&state, |_| true, "talk")?;
    send_regular_talk(&state, true).await
}

async fn talk_up_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    ensure_control_allowed(&state, |_| true, "talk")?;
    send_regular_talk(&state, false).await
}

async fn talk_toggle_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    let active = !snapshot.regular_talk_active;
    send_regular_talk(&state, active).await
}

async fn codec_handler(
    State(state): State<ApiState>,
    Json(request): Json<CodecRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    ensure_policy_allowed(&snapshot.lockout, |lockout| lockout.allow_codec, "codec")?;
    let user_id = snapshot.user_id;
    forward_control_message(
        &state.control_tx,
        ControlMessage::AudioCodec {
            user_id,
            codec: request.codec,
        },
    )
    .await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn send_talk_mode(
    state: &ApiState,
    talk_mode: TalkMode,
) -> Result<Json<OkResponse>, ApiError> {
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
    Ok(Json(OkResponse { ok: true }))
}

async fn send_regular_talk(state: &ApiState, active: bool) -> Result<Json<OkResponse>, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    let user_id = snapshot.user_id;
    forward_control_message(&state.control_tx, ControlMessage::Talk { user_id, active }).await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn button_down_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    send_button(&state, id, true).await
}

async fn button_up_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    send_button(&state, id, false).await
}

async fn button_toggle_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    send_button(&state, id, true).await
}

async fn send_button(
    state: &ApiState,
    button_id: ButtonId,
    pressed: bool,
) -> Result<Json<OkResponse>, ApiError> {
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
    Ok(Json(OkResponse { ok: true }))
}

async fn call_down_handler(
    State(state): State<ApiState>,
    Path(id): Path<u16>,
) -> Result<Json<OkResponse>, ApiError> {
    send_direct_call(&state, id, true, false).await
}

async fn call_up_handler(
    State(state): State<ApiState>,
    Path(id): Path<u16>,
) -> Result<Json<OkResponse>, ApiError> {
    send_direct_call(&state, id, false, false).await
}

async fn call_toggle_handler(
    State(state): State<ApiState>,
    Path(id): Path<u16>,
) -> Result<Json<OkResponse>, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    let active = !snapshot
        .active_direct_calls
        .iter()
        .any(|call| call.caller == snapshot.user_id && call.target == id && call.active);
    send_direct_call(&state, id, active, false).await
}

async fn reply_down_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    send_reply_call(&state, true, false).await
}

async fn reply_up_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    send_reply_call(&state, false, false).await
}

async fn reply_toggle_handler(State(state): State<ApiState>) -> Result<Json<OkResponse>, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
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
    send_reply_call(&state, active, false).await
}

async fn send_direct_call(
    state: &ApiState,
    target_user_id: u16,
    active: bool,
    duck: bool,
) -> Result<Json<OkResponse>, ApiError> {
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
    Ok(Json(OkResponse { ok: true }))
}

async fn send_reply_call(
    state: &ApiState,
    active: bool,
    duck: bool,
) -> Result<Json<OkResponse>, ApiError> {
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
    Ok(Json(OkResponse { ok: true }))
}

async fn alert_send_handler(
    State(state): State<ApiState>,
    Json(request): Json<AlertRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    forward_control_message(
        &state.control_tx,
        ControlMessage::SendAlert {
            user_id: snapshot.user_id,
            target: request.target,
            message: request.message,
        },
    )
    .await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn alert_ack_handler(
    State(state): State<ApiState>,
    Path(id): Path<AlertId>,
) -> Result<Json<OkResponse>, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    forward_control_message(
        &state.control_tx,
        ControlMessage::AckAlert {
            user_id: snapshot.user_id,
            alert_id: id,
        },
    )
    .await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn alert_cancel_handler(
    State(state): State<ApiState>,
    Path(id): Path<AlertId>,
) -> Result<Json<OkResponse>, ApiError> {
    let snapshot = state.config.lock().unwrap().clone();
    ensure_local_api_allowed(&snapshot)?;
    forward_control_message(
        &state.control_tx,
        ControlMessage::CancelAlert {
            user_id: snapshot.user_id,
            alert_id: id,
        },
    )
    .await?;
    Ok(Json(OkResponse { ok: true }))
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

fn build_input_stream(
    tx: mpsc::Sender<Vec<i16>>,
    mic_gain: f32,
    input_device: Option<&str>,
    telemetry: ClientTelemetryCounters,
) -> anyhow::Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = select_input_device(&host, input_device)?;
    let supported = device.default_input_config()?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate.0;
    tracing::info!(device = %device.name()?, ?config, ?sample_format, "opened input device");

    let mut capture = CaptureAdapter::new(tx, sample_rate, channels, mic_gain, telemetry);
    let err_fn = |err| tracing::error!(%err, "input stream error");

    match sample_format {
        SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| capture.push_interleaved(data.iter().copied()),
            err_fn,
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _| {
                capture.push_interleaved(data.iter().map(|sample| (*sample as i32 - 32768) as i16))
            },
            err_fn,
            None,
        ),
        SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                capture.push_interleaved(
                    data.iter()
                        .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16),
                )
            },
            err_fn,
            None,
        ),
        other => return Err(anyhow!("unsupported input sample format: {other:?}")),
    }
    .context("build input stream")
}

fn build_output_stream(
    playback: Arc<Mutex<PlaybackBuffer>>,
    speaker_gain: f32,
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
            let mut output = OutputAdapter::new(playback, sample_rate, channels, speaker_gain);
            device.build_output_stream(
                &config,
                move |data: &mut [i16], _| output.fill(data, |sample| sample),
                err_fn,
                None,
            )
        }
        SampleFormat::U16 => {
            let mut output = OutputAdapter::new(playback, sample_rate, channels, speaker_gain);
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
            let mut output = OutputAdapter::new(playback, sample_rate, channels, speaker_gain);
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

struct CaptureAdapter {
    tx: mpsc::Sender<Vec<i16>>,
    frame: Vec<i16>,
    channels: usize,
    channel_index: usize,
    channel_sum: i32,
    ratio: f32,
    phase: f32,
    previous_mono: f32,
    smoothed_mono: f32,
    has_previous: bool,
    mic_gain: f32,
    telemetry: ClientTelemetryCounters,
}

impl CaptureAdapter {
    fn new(
        tx: mpsc::Sender<Vec<i16>>,
        input_rate: u32,
        channels: usize,
        mic_gain: f32,
        telemetry: ClientTelemetryCounters,
    ) -> Self {
        Self {
            tx,
            frame: Vec::with_capacity(MIX_SAMPLES_PER_FRAME),
            channels: channels.max(1),
            channel_index: 0,
            channel_sum: 0,
            ratio: MIX_SAMPLE_RATE as f32 / input_rate as f32,
            phase: 0.0,
            previous_mono: 0.0,
            smoothed_mono: 0.0,
            has_previous: false,
            mic_gain: mic_gain.max(0.0),
            telemetry,
        }
    }

    fn push_interleaved<I>(&mut self, samples: I)
    where
        I: IntoIterator<Item = i16>,
    {
        for sample in samples {
            self.channel_sum += sample as i32;
            self.channel_index += 1;

            if self.channel_index == self.channels {
                let mono = (self.channel_sum / self.channels as i32) as i16;
                self.push_mono(mono);
                self.channel_index = 0;
                self.channel_sum = 0;
            }
        }
    }

    fn push_mono(&mut self, sample: i16) {
        let mono = sample as f32;
        if !self.has_previous {
            self.previous_mono = mono;
            self.smoothed_mono = mono;
            self.has_previous = true;
        }

        self.smoothed_mono = self.smoothed_mono * 0.5 + mono * 0.5;
        self.phase += self.ratio;

        while self.phase >= 1.0 {
            let fraction = 1.0 - ((self.phase - 1.0) / self.ratio).clamp(0.0, 1.0);
            let interpolated =
                self.previous_mono + (self.smoothed_mono - self.previous_mono) * fraction;
            self.push_wire_sample(apply_gain(interpolated, self.mic_gain));
            self.phase -= 1.0;
        }

        self.previous_mono = self.smoothed_mono;
    }

    fn push_wire_sample(&mut self, sample: i16) {
        self.frame.push(sample);

        if self.frame.len() == MIX_SAMPLES_PER_FRAME {
            let full_frame =
                std::mem::replace(&mut self.frame, Vec::with_capacity(MIX_SAMPLES_PER_FRAME));
            if self.tx.try_send(full_frame).is_err() {
                tracing::warn!("dropped mic frame because network queue is full");
                self.telemetry.record_tx_queue_drop();
            }
        }
    }
}

struct OutputAdapter {
    playback: Arc<Mutex<PlaybackBuffer>>,
    channels: usize,
    ratio: f32,
    phase: f32,
    current_left: i16,
    current_right: i16,
    next_left: i16,
    next_right: i16,
    initialized: bool,
    speaker_gain: f32,
}

impl OutputAdapter {
    fn new(
        playback: Arc<Mutex<PlaybackBuffer>>,
        output_rate: u32,
        channels: usize,
        speaker_gain: f32,
    ) -> Self {
        Self {
            playback,
            channels: channels.max(1),
            ratio: MIX_SAMPLE_RATE as f32 / output_rate as f32,
            phase: 0.0,
            current_left: 0,
            current_right: 0,
            next_left: 0,
            next_right: 0,
            initialized: false,
            speaker_gain: speaker_gain.max(0.0),
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
        (
            apply_gain(left, self.speaker_gain),
            apply_gain(right, self.speaker_gain),
        )
    }
}

fn apply_gain(sample: f32, gain: f32) -> i16 {
    (sample * gain)
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::Duration;

    use client_core::{
        fail_pending_control_responses, next_reconnect_delay, CONTROL_RECONNECT_INITIAL,
        CONTROL_RECONNECT_MAX,
    };
    use common::SAMPLES_PER_FRAME;

    fn config() -> ClientConfig {
        ClientConfig {
            user_id: 10,
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
            capabilities: ClientCapabilities::default(),
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: Esp32AudioConfig::default(),
        }
    }

    fn api_state() -> (ApiState, mpsc::Receiver<ControlRequest>) {
        let (control_tx, control_rx) = mpsc::channel(8);
        (
            ApiState {
                config: Arc::new(Mutex::new(config())),
                control_tx,
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
        let args = Args::try_parse_from(["pi"]).unwrap();
        assert_eq!(args.user_id, None);
    }

    #[test]
    fn device_name_matching_is_case_insensitive_substring() {
        assert!(device_name_matches("USB Audio Device", "audio"));
        assert!(device_name_matches("Headphones", "HEAD"));
        assert!(!device_name_matches("Headphones", "microphone"));
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

    #[test]
    fn button_argument_parses_id_and_label() {
        let button = "pa=PA".parse::<ButtonArg>().unwrap();
        assert_eq!(button.id, "pa");
        assert_eq!(button.label, "PA");

        let capability = ButtonCapability::from(button);
        assert_eq!(capability.id, "pa");
        assert_eq!(capability.label, "PA");
    }

    #[test]
    fn active_buttons_union_with_regular_talk_route() {
        let mut config = config();
        config.tx = vec![1];
        config.buttons = vec![TalkButtonConfig {
            id: "director".to_string(),
            label: "Director".to_string(),
            color: None,
            mode: common::TalkButtonMode::Momentary,
            actions: vec![common::TalkButtonAction::Transmit {
                channels: vec![2, 3],
                users: Vec::new(),
                duck: false,
            }],
        }];
        config.active_buttons = vec!["director".to_string()];

        assert_eq!(config.active_tx_channels(), vec![1, 2, 3]);

        config.talk_mode = TalkMode::Muted;
        config.regular_talk_active = false;
        assert_eq!(config.active_tx_channels(), vec![2, 3]);
    }

    #[test]
    fn full_config_request_builds_control_messages_in_order() {
        let request = FullConfigRequest {
            listen: vec![1, 2],
            tx: vec![2],
            vol: [(2, 0.5)].into(),
            talker_vol: HashMap::new(),
            codec: Codec::Opus,
            opus_profile: OpusProfile::default(),
            talk_mode: TalkMode::Muted,
            priority: true,
            priority_channels: Vec::new(),
            ifb: IfbConfig::default(),
        };

        let messages = request.control_messages(10);

        assert_eq!(messages.len(), 2);
        let ControlMessage::Config {
            user_id,
            listen,
            tx,
            vol,
            codec,
            talk_mode,
            ..
        } = &messages[0]
        else {
            panic!("expected config message");
        };
        assert_eq!(*user_id, 10);
        assert_eq!(listen, &vec![1, 2]);
        assert_eq!(tx, &vec![2]);
        assert_eq!(vol.get(&2), Some(&0.5));
        assert_eq!(*codec, Some(Codec::Opus));
        assert_eq!(*talk_mode, Some(TalkMode::Muted));
        assert!(matches!(
            messages[1],
            ControlMessage::Priority {
                user_id: 10,
                active: true
            }
        ));
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let Json(response) = health_handler().await;

        assert_eq!(response, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn state_returns_current_snapshot() {
        let (state, _control_rx) = api_state();

        let Json(response) = state_handler(State(state)).await;

        assert_eq!(response.user_id, 10);
        assert_eq!(response.listen, vec![1]);
        assert_eq!(response.tx, vec![1]);
        assert_eq!(response.codec, Codec::Pcm16);
        assert_eq!(response.talk_mode, TalkMode::Ptt);
        assert!(response.regular_talk_active);
        assert_eq!(
            response.supported_codecs,
            vec![Codec::Pcm16, Codec::Pcm24, Codec::Pcm48, Codec::Opus]
        );
        assert!(response.buttons.is_empty());
        assert!(response.active_buttons.is_empty());
        assert!(response.advertised_buttons.is_empty());
        assert_eq!(response.lockout, ClientLockoutPolicy::default());
        assert_eq!(response.playback.available_samples, 0);
        assert_eq!(response.playback.underflows, 0);
    }

    #[test]
    fn local_ui_uses_shared_operator_console_assets() {
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
        assert!(LOCAL_UI_HTML.contains("route-editor"));
        assert!(LOCAL_UI_HTML.contains("bottom-special"));
        assert!(LOCAL_UI_JS.contains("function renderButtons()"));
        assert!(LOCAL_UI_JS.contains("function renderCapabilityPanels()"));
        assert!(LOCAL_UI_JS.contains("data-requires-gain"));
        assert!(LOCAL_UI_HTML.contains("channel-settings-modal"));
        assert!(LOCAL_UI_JS.contains("function bindChannelSettingsGesture"));
        assert!(LOCAL_UI_JS.contains("showAllChannels"));
        assert!(LOCAL_UI_JS.contains("function allConfiguredChannels()"));
        assert!(LOCAL_UI_JS.contains("function setShowAllChannels"));
        assert!(LOCAL_UI_JS.contains("function assignedClientLabel()"));
        assert!(LOCAL_UI_JS.contains("User ID"));
        assert!(LOCAL_UI_JS.contains("function channelIconTag"));
        assert!(LOCAL_UI_JS.contains("function channelStatusTags"));
        assert!(LOCAL_UI_CSS.contains(".tag.icon-tag"));
        assert!(LOCAL_UI_CSS.contains(".tag.tx.talking"));
        assert!(LOCAL_UI_JS.contains("contextmenu"));
        assert!(!LOCAL_UI_JS.contains("clientApi?.name || 'client'} controls"));
        assert!(LOCAL_UI_HTML.contains("dock-status"));
        assert!(LOCAL_UI_JS.contains("Server connected"));
        assert!(LOCAL_UI_JS.contains("'Hold Talk'"));
        assert!(LOCAL_UI_CSS.contains(".tag.connected"));
        assert!(!LOCAL_UI_JS.contains("fetch("));
        assert!(LOCAL_UI_API_JS.contains("fetch("));
        assert!(LOCAL_UI_API_JS.contains("runtimeSettings: !mobileShell()"));
        assert!(LOCAL_UI_CSS.contains(".phone-shell"));
        assert!(LOCAL_UI_CSS.contains("--dock-height"));
    }

    #[tokio::test]
    async fn local_api_rejects_locked_codec_control() {
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
    async fn mute_enqueues_muted_talk_mode() {
        let (state, mut control_rx) = api_state();
        let handle = tokio::spawn(mute_handler(State(state)));

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::TalkMode {
                user_id: 10,
                mode: TalkMode::Muted
            }
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();

        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn unmute_restores_previous_non_muted_talk_mode() {
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
                user_id: 10,
                mode: TalkMode::Open
            }
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();

        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn codec_enqueues_audio_codec() {
        let (state, mut control_rx) = api_state();
        let handle = tokio::spawn(codec_handler(
            State(state),
            Json(CodecRequest { codec: Codec::Opus }),
        ));

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::AudioCodec {
                user_id: 10,
                codec: Codec::Opus
            }
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();

        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn button_down_enqueues_button_press() {
        let (state, mut control_rx) = api_state();
        let handle = tokio::spawn(button_down_handler(
            State(state),
            Path("director".to_string()),
        ));

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::Button {
                user_id: 10,
                ref button_id,
                pressed: true
            } if button_id == "director"
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();

        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn button_up_enqueues_button_release() {
        let (state, mut control_rx) = api_state();
        let handle = tokio::spawn(button_up_handler(State(state), Path("pa".to_string())));

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::Button {
                user_id: 10,
                ref button_id,
                pressed: false
            } if button_id == "pa"
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();

        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn alert_endpoints_enqueue_control_messages() {
        let (state, mut control_rx) = api_state();
        let handle = tokio::spawn(alert_send_handler(
            State(state.clone()),
            Json(AlertRequest {
                target: AlertTarget::Channel(4),
                message: Some("Ready".to_string()),
            }),
        ));
        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::SendAlert {
                user_id: 10,
                target: AlertTarget::Channel(4),
                ref message,
            } if message.as_deref() == Some("Ready")
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();
        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });

        let handle = tokio::spawn(alert_ack_handler(State(state.clone()), Path(7)));
        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::AckAlert {
                user_id: 10,
                alert_id: 7
            }
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();
        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });

        let handle = tokio::spawn(alert_cancel_handler(State(state), Path(7)));
        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::CancelAlert {
                user_id: 10,
                alert_id: 7
            }
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();
        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn talk_down_enqueues_regular_talk_active() {
        let (state, mut control_rx) = api_state();
        let handle = tokio::spawn(talk_down_handler(State(state)));

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::Talk {
                user_id: 10,
                active: true
            }
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();

        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn config_sends_config_and_priority_in_order() {
        let (state, mut control_rx) = api_state();
        let handle = tokio::spawn(config_handler(
            State(state),
            Json(FullConfigRequest {
                listen: vec![2, 3],
                tx: vec![3],
                vol: [(2, 0.7)].into(),
                talker_vol: HashMap::new(),
                codec: Codec::Opus,
                opus_profile: OpusProfile::default(),
                talk_mode: TalkMode::Muted,
                priority: true,
                priority_channels: Vec::new(),
                ifb: IfbConfig::default(),
            }),
        ));

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(request.message, ControlMessage::Config { .. }));
        request.response_tx.send(ControlResponse::Ack).unwrap();

        let request = control_rx.recv().await.unwrap();
        assert!(matches!(
            request.message,
            ControlMessage::Priority {
                user_id: 10,
                active: true
            }
        ));
        request.response_tx.send(ControlResponse::Ack).unwrap();

        assert_eq!(handle.await.unwrap().unwrap().0, OkResponse { ok: true });
    }

    #[tokio::test]
    async fn server_error_maps_to_bad_request() {
        let (state, mut control_rx) = api_state();
        let handle = tokio::spawn(mute_handler(State(state)));

        let request = control_rx.recv().await.unwrap();
        request
            .response_tx
            .send(ControlResponse::Error {
                message: "not allowed".to_string(),
            })
            .unwrap();

        let response = handle.await.unwrap().unwrap_err().into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
