use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use clap::{Parser, ValueEnum};
use client_core::{
    basic_client_telemetry, load_or_create_client_uid, run_connection_cue_task,
    run_control_connection, send_control_request, supported_codecs, AudioDecoder, AudioEncoder,
    AudioSettings, ClientConfig, ClientConnectionEvent, ClientServerEndpoint,
    ClientTelemetryCounters, ControlRequest, PlaybackBuffer, DEFAULT_CONTROL_PORT,
    DEFAULT_SERVER_HOST,
};
use common::{
    codec_samples_per_frame, AudioPacket, BridgeEndpointKind, BridgeEndpointStatus, BridgeStatus,
    ClientCapabilities, ClientLockoutPolicy, ClientRole, Codec, ControlMessage, ControlResponse,
    Esp32AudioConfig, IfbConfig, OpusProfile, ProcessingConfig, StereoConfig, TalkMode,
    TallyStatus, MIX_SAMPLES_PER_FRAME,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use ndi_runtime::NdiRuntime;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(about = "Generic audio-device bridge for PA, vMix, and production audio feeds")]
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
    #[arg(long, default_value = "Bridge")]
    name: String,
    #[arg(long, value_enum, default_value_t = BridgeCliMode::Duplex)]
    mode: BridgeCliMode,
    #[arg(long, value_delimiter = ',', default_value = "1")]
    tx_channels: Vec<u16>,
    #[arg(long, value_delimiter = ',', default_value = "1")]
    listen_channels: Vec<u16>,
    #[arg(long, value_enum, default_value_t = WireCodec::Pcm48)]
    codec: WireCodec,
    #[arg(long, value_enum, default_value_t = WireOpusProfile::Speech48High)]
    opus_profile: WireOpusProfile,
    #[arg(long)]
    stereo: bool,
    #[arg(long)]
    input_device: Option<String>,
    #[arg(long)]
    output_device: Option<String>,
    #[arg(long, value_enum, default_value_t = BridgeInputKind::AudioDevice)]
    input_kind: BridgeInputKind,
    #[arg(long, value_enum, default_value_t = BridgeOutputKind::AudioDevice)]
    output_kind: BridgeOutputKind,
    #[arg(long)]
    ndi_source: Option<String>,
    #[arg(long)]
    ndi_output_name: Option<String>,
    #[arg(long)]
    ndi_groups: Option<String>,
    #[arg(long)]
    vmix_source_url: Option<String>,
    #[arg(long, default_value_t = 1.0)]
    input_gain: f32,
    #[arg(long, default_value_t = 1.0)]
    output_gain: f32,
    #[arg(long, default_value = "")]
    note: String,
    #[arg(long)]
    list_devices: bool,
    #[arg(long)]
    list_ndi_sources: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum BridgeCliMode {
    Input,
    Output,
    Duplex,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum BridgeInputKind {
    #[value(name = "audio-device")]
    AudioDevice,
    #[value(name = "ndi-source")]
    NdiSource,
}

impl BridgeInputKind {
    fn status_kind(self) -> BridgeEndpointKind {
        match self {
            Self::AudioDevice => BridgeEndpointKind::AudioDevice,
            Self::NdiSource => BridgeEndpointKind::NdiSource,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum BridgeOutputKind {
    #[value(name = "audio-device")]
    AudioDevice,
    #[value(name = "vmix-browser-source")]
    VmixBrowserSource,
    #[value(name = "ndi-output")]
    NdiOutput,
}

impl BridgeOutputKind {
    fn status_kind(self) -> BridgeEndpointKind {
        match self {
            Self::AudioDevice => BridgeEndpointKind::AudioDevice,
            Self::VmixBrowserSource => BridgeEndpointKind::VmixBrowserSource,
            Self::NdiOutput => BridgeEndpointKind::NdiOutput,
        }
    }
}

impl BridgeCliMode {
    fn captures(self) -> bool {
        matches!(self, Self::Input | Self::Duplex)
    }

    fn plays(self) -> bool {
        matches!(self, Self::Output | Self::Duplex)
    }

    fn status_mode(self) -> common::BridgeMode {
        match self {
            Self::Input => common::BridgeMode::Input,
            Self::Output => common::BridgeMode::Output,
            Self::Duplex => common::BridgeMode::Duplex,
        }
    }
}

#[derive(Debug, Clone)]
struct BridgeStatusConfig {
    mode: BridgeCliMode,
    input_kind: BridgeInputKind,
    output_kind: BridgeOutputKind,
    input_device: Option<String>,
    output_device: Option<String>,
    ndi_source: Option<String>,
    ndi_output_name: Option<String>,
    vmix_source_url: Option<String>,
    input_gain: f32,
    output_gain: f32,
    note: String,
    telemetry: Arc<BridgeRuntimeTelemetry>,
}

#[derive(Debug)]
struct BridgeRuntimeTelemetry {
    started_at: Instant,
    input: EndpointRuntimeTelemetry,
    output: EndpointRuntimeTelemetry,
}

impl Default for BridgeRuntimeTelemetry {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
            input: EndpointRuntimeTelemetry::default(),
            output: EndpointRuntimeTelemetry::default(),
        }
    }
}

#[derive(Debug, Default)]
struct EndpointRuntimeTelemetry {
    audio_level_ppm: AtomicU32,
    frames: AtomicU64,
    underflows: AtomicU64,
    drops: AtomicU64,
    reconnects: AtomicU64,
    last_audio_ms: AtomicU64,
    runtime: Mutex<Option<String>>,
    warning: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Default)]
struct EndpointTelemetrySnapshot {
    audio_level: Option<f32>,
    frames: u64,
    underflows: u64,
    drops: u64,
    reconnects: u64,
    stale: bool,
    last_audio_ms_ago: Option<u64>,
    runtime: Option<String>,
    warning: Option<String>,
}

impl EndpointRuntimeTelemetry {
    fn record_audio_i16(&self, samples: &[i16], started_at: Instant) {
        self.record_audio_level(i16_peak_level(samples), started_at);
    }

    fn record_audio_f32(&self, samples: &[f32], started_at: Instant) {
        self.record_audio_level(f32_peak_level(samples), started_at);
    }

    fn record_audio_level(&self, level: f32, started_at: Instant) {
        self.audio_level_ppm.store(
            (level.clamp(0.0, 1.0) * 1_000_000.0).round() as u32,
            Ordering::Relaxed,
        );
        self.frames.fetch_add(1, Ordering::Relaxed);
        self.last_audio_ms
            .store(elapsed_ms(started_at), Ordering::Relaxed);
    }

    fn record_underflow(&self) {
        self.underflows.fetch_add(1, Ordering::Relaxed);
    }

    fn record_drop(&self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }

    fn record_reconnect(&self) {
        self.reconnects.fetch_add(1, Ordering::Relaxed);
    }

    fn set_runtime(&self, runtime: impl Into<String>) {
        *self.runtime.lock().unwrap() = Some(runtime.into());
    }

    fn set_warning(&self, warning: Option<String>) {
        *self.warning.lock().unwrap() = warning;
    }

    fn snapshot(&self, started_at: Instant) -> EndpointTelemetrySnapshot {
        let last_audio_ms = self.last_audio_ms.load(Ordering::Relaxed);
        let now = elapsed_ms(started_at);
        let last_audio_ms_ago = (last_audio_ms > 0).then_some(now.saturating_sub(last_audio_ms));
        let frames = self.frames.load(Ordering::Relaxed);
        let stale = frames == 0 || last_audio_ms_ago.is_some_and(|age| age > 2_000);
        EndpointTelemetrySnapshot {
            audio_level: (frames > 0)
                .then_some(self.audio_level_ppm.load(Ordering::Relaxed) as f32 / 1_000_000.0),
            frames,
            underflows: self.underflows.load(Ordering::Relaxed),
            drops: self.drops.load(Ordering::Relaxed),
            reconnects: self.reconnects.load(Ordering::Relaxed),
            stale,
            last_audio_ms_ago,
            runtime: self.runtime.lock().unwrap().clone(),
            warning: self.warning.lock().unwrap().clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum WireCodec {
    Pcm16,
    Pcm24,
    Pcm48,
    Opus,
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("bridge=info".parse()?))
        .init();
    let mut args = Args::parse();
    if args.list_devices {
        list_audio_devices()?;
        return Ok(());
    }
    if args.list_ndi_sources {
        list_ndi_sources()?;
        return Ok(());
    }
    resolve_endpoint_args(&mut args)?;
    validate_route_endpoint_kinds(&args)?;
    run(args).await
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

fn validate_route_endpoint_kinds(args: &Args) -> anyhow::Result<()> {
    if args.mode.captures() && args.input_kind == BridgeInputKind::NdiSource {
        if args
            .ndi_source
            .as_deref()
            .filter(|source| !source.trim().is_empty())
            .is_none()
        {
            bail!("NDI receive routes require --ndi-source");
        }
    }
    if args.mode.plays() && args.output_kind == BridgeOutputKind::NdiOutput {
        if args
            .ndi_output_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .is_none()
        {
            bail!("NDI output routes require --ndi-output-name");
        }
    }
    if args.mode.plays() && args.output_kind == BridgeOutputKind::VmixBrowserSource {
        bail!("vMix Browser Source output routes must be run by bridge-app so it can serve the local browser-source URL");
    }
    Ok(())
}

async fn run(args: Args) -> anyhow::Result<()> {
    let user_id = args.user_id.context("--user-id is required")?;
    let client_uid =
        load_or_create_client_uid(args.client_uid.as_deref(), args.identity_file.as_deref())?;
    let codec = Codec::from(args.codec);
    let opus_profile = OpusProfile::from(args.opus_profile);
    let capabilities = ClientCapabilities::bridge();
    let runtime_config = Arc::new(Mutex::new(ClientConfig {
        user_id,
        client_uid,
        role: ClientRole::Bridge,
        name: args.name.clone(),
        listen: if args.mode.plays() {
            args.listen_channels.clone()
        } else {
            Vec::new()
        },
        tx: if args.mode.captures() {
            args.tx_channels.clone()
        } else {
            Vec::new()
        },
        codec,
        opus_profile,
        talk_mode: if args.mode.captures() {
            TalkMode::Open
        } else {
            TalkMode::Muted
        },
        last_non_muted_talk_mode: TalkMode::Open,
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
        advertised_buttons: Vec::new(),
        capabilities: capabilities.clone(),
        ifb: IfbConfig::default(),
        lockout: ClientLockoutPolicy::default(),
        stereo: StereoConfig {
            enabled: args.stereo,
            channel_pan: HashMap::new(),
        },
        esp32_audio: Esp32AudioConfig::default(),
        tally: TallyStatus::default(),
    }));

    let (control_tx, control_rx) = mpsc::channel::<ControlRequest>(16);
    let (connection_event_tx, connection_event_rx) = mpsc::channel::<ClientConnectionEvent>(8);
    let mut connection_event_rx = Some(connection_event_rx);
    let control_task = tokio::spawn(run_control_connection(
        args.control.clone(),
        control_rx,
        Arc::clone(&runtime_config),
        Some(connection_event_tx),
    ));
    let initial = runtime_config.lock().unwrap().clone();
    let hello = send_control_request(
        &control_tx,
        ControlMessage::Hello {
            user_id,
            requested_user_id: Some(user_id),
            client_uid: initial.client_uid.clone(),
            codecs: supported_codecs(),
            buttons: Vec::new(),
            capabilities,
            role: ClientRole::Bridge,
        },
    )
    .await?;
    let preconfigured = match hello {
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
        match send_control_request(&control_tx, startup_config.control_message()).await? {
            ControlResponse::Ack => {}
            ControlResponse::Error { message } => bail!("{message}"),
            other => bail!("unexpected config response: {other:?}"),
        }
    }

    let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    socket.connect(args.server).await?;
    tracing::info!(local = %socket.local_addr()?, server = %args.server, mode = ?args.mode, "bridge connected");

    let registration_task = tokio::spawn(run_registration(
        Arc::clone(&socket),
        Arc::clone(&runtime_config),
    ));
    let telemetry_counters = ClientTelemetryCounters::default();
    let bridge_runtime_telemetry = Arc::new(BridgeRuntimeTelemetry::default());
    let mut telemetry_playback = Arc::new(Mutex::new(PlaybackBuffer::new(
        MIX_SAMPLES_PER_FRAME * 20,
        0,
    )));
    let mut bridge_input_device = None;
    let mut bridge_output_device = None;
    let mut _input_stream = None;
    let capture_task = if args.mode.captures() {
        match args.input_kind {
            BridgeInputKind::AudioDevice => {
                let (mic_tx, mic_rx) = mpsc::channel::<Vec<i16>>(8);
                let settings = Arc::new(AudioSettings::new(args.input_gain, 1.0));
                let (stream, device_name) = build_input_stream(
                    mic_tx,
                    settings,
                    args.input_device.as_deref(),
                    telemetry_counters.clone(),
                )?;
                bridge_input_device = Some(device_name);
                stream.play()?;
                _input_stream = Some(stream);
                Some(tokio::spawn(run_capture_sender(
                    Arc::clone(&socket),
                    Arc::clone(&runtime_config),
                    mic_rx,
                    telemetry_counters.clone(),
                )))
            }
            BridgeInputKind::NdiSource => {
                let source_name = args
                    .ndi_source
                    .as_deref()
                    .map(str::trim)
                    .filter(|source| !source.is_empty())
                    .context("NDI receive routes require --ndi-source")?
                    .to_string();
                bridge_input_device = Some(source_name.clone());
                let (mic_tx, mic_rx) = mpsc::channel::<Vec<i16>>(8);
                let capture_sender = tokio::spawn(run_capture_sender(
                    Arc::clone(&socket),
                    Arc::clone(&runtime_config),
                    mic_rx,
                    telemetry_counters.clone(),
                ));
                let telemetry = telemetry_counters.clone();
                let endpoint_telemetry = Arc::clone(&bridge_runtime_telemetry);
                let input_gain = args.input_gain;
                let ndi_capture = tokio::task::spawn_blocking(move || {
                    run_ndi_capture_producer(
                        source_name,
                        input_gain,
                        mic_tx,
                        telemetry,
                        endpoint_telemetry,
                    )
                });
                Some(tokio::spawn(async move {
                    tokio::select! {
                        result = ndi_capture => result.context("NDI capture task panicked")??,
                        result = capture_sender => result.context("capture sender task panicked")??,
                    }
                    Ok(())
                }))
            }
        }
    } else {
        None
    };
    let mut _output_stream = None;
    let playback_task = if args.mode.plays() {
        match args.output_kind {
            BridgeOutputKind::AudioDevice => {
                let playback = Arc::new(Mutex::new(PlaybackBuffer::new(
                    MIX_SAMPLES_PER_FRAME * 20,
                    MIX_SAMPLES_PER_FRAME * 4,
                )));
                telemetry_playback = Arc::clone(&playback);
                let settings = Arc::new(AudioSettings::new(1.0, args.output_gain));
                let (stream, device_name) = build_output_stream(
                    Arc::clone(&playback),
                    settings,
                    args.output_device.as_deref(),
                )?;
                bridge_output_device = Some(device_name);
                stream.play()?;
                _output_stream = Some(stream);
                if let Some(events) = connection_event_rx.take() {
                    tokio::spawn(run_connection_cue_task(Arc::clone(&playback), events));
                }
                Some(tokio::spawn(run_playback_receiver(
                    Arc::clone(&socket),
                    Arc::clone(&runtime_config),
                    playback,
                    telemetry_counters.clone(),
                )))
            }
            BridgeOutputKind::NdiOutput => {
                let output_name = args
                    .ndi_output_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .context("NDI output routes require --ndi-output-name")?
                    .to_string();
                bridge_output_device = Some(output_name.clone());
                Some(tokio::spawn(run_ndi_output_receiver(
                    Arc::clone(&socket),
                    Arc::clone(&runtime_config),
                    output_name,
                    args.ndi_groups.clone(),
                    args.output_gain,
                    telemetry_counters.clone(),
                    Arc::clone(&bridge_runtime_telemetry),
                )))
            }
            BridgeOutputKind::VmixBrowserSource => None,
        }
    } else {
        None
    };
    let bridge_status_task = tokio::spawn(run_bridge_status(
        control_tx.clone(),
        Arc::clone(&runtime_config),
        BridgeStatusConfig {
            mode: args.mode,
            input_kind: args.input_kind,
            output_kind: args.output_kind,
            input_device: bridge_input_device,
            output_device: bridge_output_device,
            ndi_source: args.ndi_source,
            ndi_output_name: args.ndi_output_name,
            vmix_source_url: args.vmix_source_url,
            input_gain: args.input_gain,
            output_gain: args.output_gain,
            note: args.note,
            telemetry: Arc::clone(&bridge_runtime_telemetry),
        },
    ));
    let bridge_telemetry_task = tokio::spawn(run_bridge_telemetry(
        control_tx,
        Arc::clone(&runtime_config),
        telemetry_playback,
        telemetry_counters.clone(),
        Instant::now(),
    ));

    tokio::select! {
        result = control_task => result.context("control task panicked")??,
        result = registration_task => result.context("registration task panicked")??,
        result = bridge_status_task => result.context("bridge status task panicked")??,
        result = bridge_telemetry_task => result.context("bridge telemetry task panicked")??,
        result = wait_optional(capture_task) => result.context("capture task panicked")??,
        result = wait_optional(playback_task) => result.context("playback task panicked")??,
        _ = tokio::signal::ctrl_c() => tracing::info!("bridge shutting down"),
    }
    Ok(())
}

async fn wait_optional(
    task: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
) -> Result<anyhow::Result<()>, tokio::task::JoinError> {
    match task {
        Some(task) => task.await,
        None => std::future::pending().await,
    }
}

async fn run_registration(
    socket: Arc<UdpSocket>,
    config: Arc<Mutex<ClientConfig>>,
) -> anyhow::Result<()> {
    let seq = AtomicU16::new(0);
    let mut encoded = Vec::new();
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        interval.tick().await;
        let (user_id, codec) = {
            let config = config.lock().unwrap();
            (config.user_id, config.codec)
        };
        let packet = AudioPacket::registration(user_id, codec, seq.fetch_add(1, Ordering::Relaxed));
        packet.encode(&mut encoded)?;
        socket.send(&encoded).await?;
    }
}

async fn run_bridge_status(
    control_tx: mpsc::Sender<ControlRequest>,
    config: Arc<Mutex<ClientConfig>>,
    bridge: BridgeStatusConfig,
) -> anyhow::Result<()> {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        let (user_id, snapshot) = {
            let config = config.lock().unwrap();
            (
                config.user_id,
                bridge_status_snapshot(&config, bridge.clone()),
            )
        };
        match send_control_request(
            &control_tx,
            ControlMessage::BridgeStatus {
                user_id,
                status: snapshot,
            },
        )
        .await?
        {
            ControlResponse::Ack => {}
            ControlResponse::Error { message }
                if message.contains("control connection unavailable") =>
            {
                tracing::debug!(%message, "bridge status deferred until control reconnects");
            }
            ControlResponse::Error { message } => {
                tracing::warn!(%message, "bridge status rejected by server");
            }
            other => tracing::warn!(?other, "unexpected bridge status response"),
        }
    }
}

async fn run_bridge_telemetry(
    control_tx: mpsc::Sender<ControlRequest>,
    config: Arc<Mutex<ClientConfig>>,
    playback: Arc<Mutex<PlaybackBuffer>>,
    telemetry: ClientTelemetryCounters,
    started_at: Instant,
) -> anyhow::Result<()> {
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        interval.tick().await;
        let user_id = config.lock().unwrap().user_id;
        let mut health = basic_client_telemetry(
            "bridge",
            playback.lock().unwrap().stats(),
            telemetry.snapshot(),
        );
        health.uptime_ms = started_at
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        match send_control_request(
            &control_tx,
            ControlMessage::CaptureHealth { user_id, health },
        )
        .await?
        {
            ControlResponse::Ack => {}
            ControlResponse::Error { message }
                if message.contains("control connection unavailable") =>
            {
                tracing::debug!(%message, "bridge telemetry deferred until control reconnects");
            }
            ControlResponse::Error { message } => {
                tracing::warn!(%message, "bridge telemetry rejected by server");
            }
            other => tracing::warn!(?other, "unexpected bridge telemetry response"),
        }
    }
}

fn bridge_status_snapshot(config: &ClientConfig, bridge: BridgeStatusConfig) -> BridgeStatus {
    let input_telemetry = bridge.telemetry.input.snapshot(bridge.telemetry.started_at);
    let output_telemetry = bridge
        .telemetry
        .output
        .snapshot(bridge.telemetry.started_at);
    let input = bridge.mode.captures().then(|| BridgeEndpointStatus {
        kind: bridge.input_kind.status_kind(),
        name: match bridge.input_kind {
            BridgeInputKind::AudioDevice => bridge.input_device.clone(),
            BridgeInputKind::NdiSource => bridge.ndi_source.clone(),
        },
        url: None,
        connected_clients: None,
        available: Some(true),
        warning: input_telemetry.warning.clone(),
        runtime: input_telemetry.runtime.clone(),
        audio_level: input_telemetry.audio_level,
        frames: (input_telemetry.frames > 0).then_some(input_telemetry.frames),
        underflows: (input_telemetry.underflows > 0).then_some(input_telemetry.underflows),
        drops: (input_telemetry.drops > 0).then_some(input_telemetry.drops),
        reconnects: (input_telemetry.reconnects > 0).then_some(input_telemetry.reconnects),
        stale: matches!(bridge.input_kind, BridgeInputKind::NdiSource)
            .then_some(input_telemetry.stale),
        last_audio_ms_ago: input_telemetry.last_audio_ms_ago,
    });
    let output = bridge.mode.plays().then(|| BridgeEndpointStatus {
        kind: bridge.output_kind.status_kind(),
        name: match bridge.output_kind {
            BridgeOutputKind::AudioDevice => bridge.output_device.clone(),
            BridgeOutputKind::VmixBrowserSource => Some(
                bridge
                    .ndi_output_name
                    .clone()
                    .unwrap_or_else(|| "vMix Browser Source".to_string()),
            ),
            BridgeOutputKind::NdiOutput => bridge.ndi_output_name.clone(),
        },
        url: bridge.vmix_source_url.clone(),
        connected_clients: None,
        available: Some(bridge.output_kind != BridgeOutputKind::VmixBrowserSource),
        warning: match bridge.output_kind {
            BridgeOutputKind::AudioDevice => output_telemetry.warning.clone(),
            BridgeOutputKind::VmixBrowserSource => {
                Some("vMix browser sources are served by bridge-app".to_string())
            }
            BridgeOutputKind::NdiOutput => output_telemetry.warning.clone(),
        },
        runtime: output_telemetry.runtime.clone(),
        audio_level: output_telemetry.audio_level,
        frames: (output_telemetry.frames > 0).then_some(output_telemetry.frames),
        underflows: (output_telemetry.underflows > 0).then_some(output_telemetry.underflows),
        drops: (output_telemetry.drops > 0).then_some(output_telemetry.drops),
        reconnects: (output_telemetry.reconnects > 0).then_some(output_telemetry.reconnects),
        stale: matches!(bridge.output_kind, BridgeOutputKind::NdiOutput)
            .then_some(output_telemetry.stale),
        last_audio_ms_ago: output_telemetry.last_audio_ms_ago,
    });
    BridgeStatus {
        mode: bridge.mode.status_mode(),
        input_device: bridge.input_device,
        output_device: bridge.output_device,
        input,
        output,
        input_gain: bridge.input_gain,
        output_gain: bridge.output_gain,
        tx: config.tx.clone(),
        listen: config.listen.clone(),
        note: bridge.note,
    }
}

async fn run_capture_sender(
    socket: Arc<UdpSocket>,
    config: Arc<Mutex<ClientConfig>>,
    mut mic_rx: mpsc::Receiver<Vec<i16>>,
    telemetry: ClientTelemetryCounters,
) -> anyhow::Result<()> {
    let seq = AtomicU16::new(0);
    let timestamp = AtomicU32::new(0);
    let mut encoded = Vec::new();
    let (mut current_codec, mut current_profile) = {
        let config = config.lock().unwrap();
        (config.codec, config.opus_profile)
    };
    let mut encoder = AudioEncoder::new(current_codec, current_profile)?;
    while let Some(frame) = mic_rx.recv().await {
        let (targets, codec, profile, user_id) = {
            let config = config.lock().unwrap();
            (
                config.active_tx_targets(),
                config.codec,
                config.opus_profile,
                config.user_id,
            )
        };
        if targets.is_empty() {
            continue;
        }
        if codec != current_codec || profile != current_profile {
            encoder = AudioEncoder::new(codec, profile)?;
            current_codec = codec;
            current_profile = profile;
        }
        let payload = match encoder.encode(&frame) {
            Ok(payload) => payload,
            Err(err) => {
                telemetry.record_codec_drop();
                return Err(err);
            }
        };
        let timestamp = timestamp.fetch_add(
            codec_samples_per_frame(current_codec) as u32,
            Ordering::Relaxed,
        );
        for target in targets {
            let packet = AudioPacket {
                user_id,
                target,
                codec: current_codec,
                seq: seq.fetch_add(1, Ordering::Relaxed),
                timestamp,
                payload: payload.clone(),
            };
            if let Err(err) = packet.encode(&mut encoded) {
                telemetry.record_packet_encode_error();
                return Err(err.into());
            }
            match socket.send(&encoded).await {
                Ok(_) => telemetry.record_tx_packet(),
                Err(err) => {
                    telemetry.record_tx_send_failure();
                    return Err(err.into());
                }
            }
        }
    }
    Ok(())
}

async fn run_playback_receiver(
    socket: Arc<UdpSocket>,
    config: Arc<Mutex<ClientConfig>>,
    playback: Arc<Mutex<PlaybackBuffer>>,
    telemetry: ClientTelemetryCounters,
) -> anyhow::Result<()> {
    let mut decoder = AudioDecoder::default();
    let mut buf = vec![0_u8; common::MAX_PACKET_BYTES];
    loop {
        let len = socket.recv(&mut buf).await?;
        let packet = match AudioPacket::decode(&buf[..len]) {
            Ok(packet) => {
                telemetry.record_udp_rx_packet();
                packet
            }
            Err(err) => {
                telemetry.record_malformed_packet();
                tracing::warn!(%err, "dropped malformed bridge receive packet");
                continue;
            }
        };
        let (receive_channels, opus_profile) = {
            let config = config.lock().unwrap();
            let channels = if config.stereo.active_for_codec(config.codec)
                && matches!(packet.codec, Codec::Pcm48 | Codec::Opus)
            {
                2
            } else {
                1
            };
            (channels, config.opus_profile)
        };
        let samples = match decoder.decode_with_channels(
            packet.codec,
            opus_profile,
            &packet.payload,
            receive_channels,
        ) {
            Ok(samples) => samples,
            Err(err) => {
                telemetry.record_decode_error();
                tracing::warn!(%err, codec = ?packet.codec, "dropped invalid bridge receive packet");
                continue;
            }
        };
        playback
            .lock()
            .unwrap()
            .push_frame(&samples, receive_channels);
    }
}

fn run_ndi_capture_producer(
    source_name: String,
    input_gain: f32,
    mic_tx: mpsc::Sender<Vec<i16>>,
    telemetry: ClientTelemetryCounters,
    runtime_telemetry: Arc<BridgeRuntimeTelemetry>,
) -> anyhow::Result<()> {
    let runtime = NdiRuntime::load().context("load system NDI runtime for receive")?;
    runtime_telemetry.input.set_runtime(
        runtime
            .library_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "system NDI runtime".to_string()),
    );
    tracing::info!(
        source = %source_name,
        library = ?runtime.library_path(),
        "starting NDI receive route"
    );
    let settings = Arc::new(AudioSettings::new(input_gain, 1.0));
    loop {
        let mut receiver = match runtime.receiver(&source_name) {
            Ok(receiver) => {
                runtime_telemetry.input.set_warning(None);
                receiver
            }
            Err(err) => {
                runtime_telemetry.input.record_reconnect();
                runtime_telemetry
                    .input
                    .set_warning(Some(format!("NDI source unavailable: {err}")));
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        let mut capture: Option<(u32, usize, CaptureAdapter)> = None;
        loop {
            let frame = match receiver.capture_audio(Duration::from_millis(500)) {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    runtime_telemetry.input.record_underflow();
                    continue;
                }
                Err(err) => {
                    runtime_telemetry.input.record_reconnect();
                    runtime_telemetry
                        .input
                        .set_warning(Some(format!("NDI receive reconnecting: {err}")));
                    break;
                }
            };
            runtime_telemetry
                .input
                .record_audio_f32(&frame.samples, runtime_telemetry.started_at);
            if capture.as_ref().is_none_or(|(rate, channels, _)| {
                *rate != frame.sample_rate_hz || *channels != frame.channels
            }) {
                tracing::info!(
                    source = %source_name,
                    sample_rate = frame.sample_rate_hz,
                    channels = frame.channels,
                    "NDI receive audio format changed"
                );
                capture = Some((
                    frame.sample_rate_hz,
                    frame.channels,
                    CaptureAdapter::new(
                        mic_tx.clone(),
                        frame.sample_rate_hz,
                        frame.channels,
                        Arc::clone(&settings),
                        telemetry.clone(),
                    ),
                ));
            }
            if let Some((_, _, adapter)) = capture.as_mut() {
                adapter
                    .push_interleaved(frame.samples.into_iter().map(|sample| float_to_i16(sample)));
            }
        }
    }
}

async fn run_ndi_output_receiver(
    socket: Arc<UdpSocket>,
    config: Arc<Mutex<ClientConfig>>,
    output_name: String,
    groups: Option<String>,
    output_gain: f32,
    telemetry: ClientTelemetryCounters,
    runtime_telemetry: Arc<BridgeRuntimeTelemetry>,
) -> anyhow::Result<()> {
    let runtime = NdiRuntime::load().context("load system NDI runtime for send")?;
    runtime_telemetry.output.set_runtime(
        runtime
            .library_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "system NDI runtime".to_string()),
    );
    tracing::info!(
        name = %output_name,
        groups = ?groups,
        library = ?runtime.library_path(),
        "starting NDI output route"
    );
    let mut sender = runtime.sender(&output_name, groups.as_deref())?;
    runtime_telemetry.output.set_warning(None);
    let mut decoder = AudioDecoder::default();
    let mut buf = vec![0_u8; common::MAX_PACKET_BYTES];
    let mut floats = Vec::<f32>::new();
    loop {
        let len = socket.recv(&mut buf).await?;
        let packet = match AudioPacket::decode(&buf[..len]) {
            Ok(packet) => {
                telemetry.record_udp_rx_packet();
                packet
            }
            Err(err) => {
                telemetry.record_malformed_packet();
                runtime_telemetry.output.record_drop();
                tracing::warn!(%err, "dropped malformed bridge NDI output packet");
                continue;
            }
        };
        let (receive_channels, opus_profile) = {
            let config = config.lock().unwrap();
            let channels = if config.stereo.active_for_codec(config.codec)
                && matches!(packet.codec, Codec::Pcm48 | Codec::Opus)
            {
                2
            } else {
                1
            };
            (channels, config.opus_profile)
        };
        let samples = match decoder.decode_with_channels(
            packet.codec,
            opus_profile,
            &packet.payload,
            receive_channels,
        ) {
            Ok(samples) => samples,
            Err(err) => {
                telemetry.record_decode_error();
                runtime_telemetry.output.record_drop();
                tracing::warn!(%err, codec = ?packet.codec, "dropped invalid bridge NDI output packet");
                continue;
            }
        };
        runtime_telemetry
            .output
            .record_audio_i16(&samples, runtime_telemetry.started_at);
        floats.clear();
        floats.extend(
            samples
                .iter()
                .map(|sample| ((*sample as f32 / i16::MAX as f32) * output_gain).clamp(-1.0, 1.0)),
        );
        if let Err(err) =
            sender.send_interleaved_f32(common::MIX_SAMPLE_RATE, receive_channels, &floats)
        {
            runtime_telemetry.output.record_reconnect();
            runtime_telemetry
                .output
                .set_warning(Some(format!("NDI output send failed: {err}")));
            sender = runtime.sender(&output_name, groups.as_deref())?;
            runtime_telemetry.output.set_warning(None);
        }
    }
}

fn float_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

fn elapsed_ms(started_at: Instant) -> u64 {
    started_at
        .elapsed()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn i16_peak_level(samples: &[i16]) -> f32 {
    samples
        .iter()
        .map(|sample| (*sample as f32).abs() / i16::MAX as f32)
        .fold(0.0, f32::max)
        .clamp(0.0, 1.0)
}

fn f32_peak_level(samples: &[f32]) -> f32 {
    samples
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0, f32::max)
        .clamp(0.0, 1.0)
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
    println!("\nOutput devices:");
    for device in host.output_devices().context("list output devices")? {
        println!(
            "  {}",
            device.name().unwrap_or_else(|_| "<unknown>".to_string())
        );
    }
    Ok(())
}

fn list_ndi_sources() -> anyhow::Result<()> {
    let runtime = NdiRuntime::load()?;
    println!(
        "NDI runtime: {}",
        runtime
            .library_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "system loader path".to_string())
    );
    println!("NDI sources:");
    let sources = runtime.find_sources(Duration::from_millis(2_000), None)?;
    if sources.is_empty() {
        println!("  <none found>");
    } else {
        for source in sources {
            match source.url {
                Some(url) => println!("  {} ({url})", source.name),
                None => println!("  {}", source.name),
            }
        }
    }
    Ok(())
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
        if name.to_lowercase().contains(&pattern.to_lowercase()) {
            return Ok(device);
        }
    }
    bail!("no {kind} device matched `{pattern}`; run --list-devices")
}

fn build_input_stream(
    tx: mpsc::Sender<Vec<i16>>,
    settings: Arc<AudioSettings>,
    input_device: Option<&str>,
    telemetry: ClientTelemetryCounters,
) -> anyhow::Result<(cpal::Stream, String)> {
    let host = cpal::default_host();
    let device = select_device(
        host.input_devices().context("list input devices")?,
        input_device,
        "input",
        || host.default_input_device(),
    )?;
    let device_name = device
        .name()
        .unwrap_or_else(|_| "default input".to_string());
    let supported = device.default_input_config()?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let mut capture = CaptureAdapter::new(
        tx,
        config.sample_rate.0,
        config.channels as usize,
        settings,
        telemetry,
    );
    let err_fn = |err| tracing::error!(%err, "bridge input stream error");
    let stream = match sample_format {
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
    .context("build input stream")?;
    Ok((stream, device_name))
}

fn build_output_stream(
    playback: Arc<Mutex<PlaybackBuffer>>,
    settings: Arc<AudioSettings>,
    output_device: Option<&str>,
) -> anyhow::Result<(cpal::Stream, String)> {
    let host = cpal::default_host();
    let device = select_device(
        host.output_devices().context("list output devices")?,
        output_device,
        "output",
        || host.default_output_device(),
    )?;
    let device_name = device
        .name()
        .unwrap_or_else(|_| "default output".to_string());
    let supported = device.default_output_config()?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let mut output = OutputAdapter::new(
        playback,
        config.sample_rate.0,
        config.channels as usize,
        settings,
    );
    let err_fn = |err| tracing::error!(%err, "bridge output stream error");
    let stream = match sample_format {
        SampleFormat::I16 => device.build_output_stream(
            &config,
            move |data: &mut [i16], _| output.fill(data, |sample| sample),
            err_fn,
            None,
        ),
        SampleFormat::U16 => device.build_output_stream(
            &config,
            move |data: &mut [u16], _| output.fill(data, |sample| (sample as i32 + 32768) as u16),
            err_fn,
            None,
        ),
        SampleFormat::F32 => device.build_output_stream(
            &config,
            move |data: &mut [f32], _| output.fill(data, |sample| sample as f32 / i16::MAX as f32),
            err_fn,
            None,
        ),
        other => return Err(anyhow!("unsupported output sample format: {other:?}")),
    }
    .context("build output stream")?;
    Ok((stream, device_name))
}

struct CaptureAdapter {
    tx: mpsc::Sender<Vec<i16>>,
    input_rate: u32,
    channels: usize,
    settings: Arc<AudioSettings>,
    telemetry: ClientTelemetryCounters,
    frame: Vec<i16>,
}

impl CaptureAdapter {
    fn new(
        tx: mpsc::Sender<Vec<i16>>,
        input_rate: u32,
        channels: usize,
        settings: Arc<AudioSettings>,
        telemetry: ClientTelemetryCounters,
    ) -> Self {
        Self {
            tx,
            input_rate,
            channels: channels.max(1),
            settings,
            telemetry,
            frame: Vec::new(),
        }
    }

    fn push_interleaved(&mut self, samples: impl IntoIterator<Item = i16>) {
        for (index, sample) in samples.into_iter().enumerate() {
            if index % self.channels == 0 {
                let sample = ((sample as f32) * self.settings.mic_gain())
                    .clamp(i16::MIN as f32, i16::MAX as f32)
                    .round() as i16;
                self.frame.push(sample);
            }
            let input_frame_len = (self.input_rate as usize * common::FRAME_MS as usize) / 1_000;
            if self.frame.len() >= input_frame_len {
                let input = self.frame.drain(..input_frame_len).collect::<Vec<_>>();
                let output =
                    common::resample_linear(&input, self.input_rate, common::MIX_SAMPLE_RATE);
                if self.tx.try_send(output).is_err() {
                    self.telemetry.record_tx_queue_drop();
                }
            }
        }
    }
}

struct OutputAdapter {
    playback: Arc<Mutex<PlaybackBuffer>>,
    output_rate: u32,
    channels: usize,
    settings: Arc<AudioSettings>,
    resampled: Vec<i16>,
    index: usize,
}

impl OutputAdapter {
    fn new(
        playback: Arc<Mutex<PlaybackBuffer>>,
        output_rate: u32,
        channels: usize,
        settings: Arc<AudioSettings>,
    ) -> Self {
        Self {
            playback,
            output_rate,
            channels: channels.max(1),
            settings,
            resampled: Vec::new(),
            index: 0,
        }
    }

    fn fill<T>(&mut self, data: &mut [T], convert: impl Fn(i16) -> T)
    where
        T: Copy,
    {
        for frame in data.chunks_mut(self.channels) {
            if self.index >= self.resampled.len() {
                let mut mix = Vec::with_capacity(MIX_SAMPLES_PER_FRAME);
                {
                    let mut playback = self.playback.lock().unwrap();
                    for _ in 0..MIX_SAMPLES_PER_FRAME {
                        let (left, right) = playback.pop_stereo().unwrap_or((0, 0));
                        mix.push(((left as i32 + right as i32) / 2) as i16);
                    }
                }
                self.resampled =
                    common::resample_linear(&mix, common::MIX_SAMPLE_RATE, self.output_rate);
                self.index = 0;
            }
            let sample = self.resampled.get(self.index).copied().unwrap_or(0);
            self.index += 1;
            let sample = ((sample as f32) * self.settings.speaker_gain())
                .clamp(i16::MIN as f32, i16::MAX as f32)
                .round() as i16;
            for channel in frame {
                *channel = convert(sample);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ClientConfig {
        ClientConfig {
            user_id: 90,
            client_uid: "bridge-test".to_string(),
            role: ClientRole::Bridge,
            name: "Program Bridge".to_string(),
            listen: vec![30],
            tx: vec![20],
            codec: Codec::Pcm48,
            opus_profile: OpusProfile::Speech48High,
            talk_mode: TalkMode::Open,
            last_non_muted_talk_mode: TalkMode::Open,
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
            advertised_buttons: Vec::new(),
            capabilities: ClientCapabilities::default(),
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: Esp32AudioConfig::default(),
            tally: TallyStatus::default(),
        }
    }

    #[test]
    fn bridge_cli_mode_maps_to_wire_status_mode() {
        assert_eq!(
            BridgeCliMode::Input.status_mode(),
            common::BridgeMode::Input
        );
        assert_eq!(
            BridgeCliMode::Output.status_mode(),
            common::BridgeMode::Output
        );
        assert_eq!(
            BridgeCliMode::Duplex.status_mode(),
            common::BridgeMode::Duplex
        );
    }

    #[test]
    fn bridge_status_snapshot_reports_routes_devices_and_levels() {
        let config = test_config();

        let status = bridge_status_snapshot(
            &config,
            BridgeStatusConfig {
                mode: BridgeCliMode::Duplex,
                input_kind: BridgeInputKind::AudioDevice,
                output_kind: BridgeOutputKind::AudioDevice,
                input_device: Some("BlackHole".to_string()),
                output_device: Some("USB Audio".to_string()),
                ndi_source: None,
                ndi_output_name: None,
                vmix_source_url: None,
                input_gain: 0.8,
                output_gain: 0.6,
                note: "vMix program bridge".to_string(),
                telemetry: Arc::new(BridgeRuntimeTelemetry::default()),
            },
        );

        assert_eq!(status.mode, common::BridgeMode::Duplex);
        assert_eq!(status.input_device.as_deref(), Some("BlackHole"));
        assert_eq!(status.output_device.as_deref(), Some("USB Audio"));
        assert_eq!(
            status.input.as_ref().unwrap().kind,
            BridgeEndpointKind::AudioDevice
        );
        assert_eq!(
            status.output.as_ref().unwrap().kind,
            BridgeEndpointKind::AudioDevice
        );
        assert_eq!(status.input_gain, 0.8);
        assert_eq!(status.output_gain, 0.6);
        assert_eq!(status.tx, vec![20]);
        assert_eq!(status.listen, vec![30]);
        assert_eq!(status.note, "vMix program bridge");
    }

    #[test]
    fn bridge_cli_parses_routes_and_note() {
        let args = Args::try_parse_from([
            "bridge",
            "--user-id",
            "90",
            "--mode",
            "input",
            "--tx-channels",
            "20,21",
            "--input-device",
            "BlackHole",
            "--note",
            "program feed",
        ])
        .unwrap();

        assert_eq!(args.mode, BridgeCliMode::Input);
        assert_eq!(args.tx_channels, vec![20, 21]);
        assert_eq!(args.listen_channels, vec![1]);
        assert_eq!(args.input_device.as_deref(), Some("BlackHole"));
        assert_eq!(args.input_kind, BridgeInputKind::AudioDevice);
        assert_eq!(args.output_kind, BridgeOutputKind::AudioDevice);
        assert_eq!(args.note, "program feed");
    }

    #[test]
    fn bridge_cli_parses_default_ndi_route_fields() {
        let args = Args::try_parse_from([
            "bridge",
            "--user-id",
            "95",
            "--mode",
            "output",
            "--output-kind",
            "ndi-output",
            "--ndi-output-name",
            "RedLine Program",
            "--ndi-groups",
            "arena",
        ])
        .unwrap();

        assert_eq!(args.output_kind, BridgeOutputKind::NdiOutput);
        assert_eq!(args.ndi_output_name.as_deref(), Some("RedLine Program"));
        assert_eq!(args.ndi_groups.as_deref(), Some("arena"));
        assert!(validate_route_endpoint_kinds(&args).is_ok());
    }

    #[test]
    fn bridge_status_snapshot_includes_endpoint_telemetry() {
        let config = test_config();
        let telemetry = Arc::new(BridgeRuntimeTelemetry::default());
        telemetry.input.set_runtime("/usr/local/lib/libndi.4.dylib");
        telemetry
            .input
            .record_audio_i16(&[0, i16::MAX], telemetry.started_at);
        telemetry.input.record_underflow();
        telemetry.input.record_reconnect();

        let status = bridge_status_snapshot(
            &config,
            BridgeStatusConfig {
                mode: BridgeCliMode::Input,
                input_kind: BridgeInputKind::NdiSource,
                output_kind: BridgeOutputKind::AudioDevice,
                input_device: None,
                output_device: None,
                ndi_source: Some("vMix Program".to_string()),
                ndi_output_name: None,
                vmix_source_url: None,
                input_gain: 1.0,
                output_gain: 1.0,
                note: String::new(),
                telemetry,
            },
        );

        let input = status.input.unwrap();
        assert_eq!(
            input.runtime.as_deref(),
            Some("/usr/local/lib/libndi.4.dylib")
        );
        assert_eq!(input.frames, Some(1));
        assert_eq!(input.underflows, Some(1));
        assert_eq!(input.reconnects, Some(1));
        assert!(input.audio_level.unwrap() > 0.9);
        assert_eq!(input.stale, Some(false));
    }

    #[test]
    fn bridge_cli_requires_ndi_route_names() {
        let args = Args::try_parse_from([
            "bridge",
            "--user-id",
            "95",
            "--mode",
            "output",
            "--output-kind",
            "ndi-output",
        ])
        .unwrap();

        assert!(validate_route_endpoint_kinds(&args).is_err());
    }
}
