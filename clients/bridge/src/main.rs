use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use clap::{Parser, ValueEnum};
use client_core::{
    load_or_create_client_uid, run_connection_cue_task, run_control_connection,
    send_control_request, supported_codecs, AudioDecoder, AudioEncoder, AudioSettings,
    ClientConfig, ClientConnectionEvent, ClientServerEndpoint, ControlRequest, PlaybackBuffer,
    DEFAULT_CONTROL_PORT, DEFAULT_SERVER_HOST,
};
use common::{
    codec_samples_per_frame, AudioPacket, BridgeStatus, ClientLockoutPolicy, ClientRole, Codec,
    ControlMessage, ControlResponse, Esp32AudioConfig, IfbConfig, OpusProfile, ProcessingConfig,
    StereoConfig, TalkMode, MIX_SAMPLES_PER_FRAME,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
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
    #[arg(long, required_unless_present = "list_devices")]
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
    #[arg(long, default_value_t = 1.0)]
    input_gain: f32,
    #[arg(long, default_value_t = 1.0)]
    output_gain: f32,
    #[arg(long, default_value = "")]
    note: String,
    #[arg(long)]
    list_devices: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum BridgeCliMode {
    Input,
    Output,
    Duplex,
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
    input_device: Option<String>,
    output_device: Option<String>,
    input_gain: f32,
    output_gain: f32,
    note: String,
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
    resolve_endpoint_args(&mut args)?;
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

async fn run(args: Args) -> anyhow::Result<()> {
    let user_id = args.user_id.context("--user-id is required")?;
    let client_uid =
        load_or_create_client_uid(args.client_uid.as_deref(), args.identity_file.as_deref())?;
    let codec = Codec::from(args.codec);
    let opus_profile = OpusProfile::from(args.opus_profile);
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
        ifb: IfbConfig::default(),
        lockout: ClientLockoutPolicy::default(),
        stereo: StereoConfig {
            enabled: args.stereo,
            channel_pan: HashMap::new(),
        },
        esp32_audio: Esp32AudioConfig::default(),
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
    let mut bridge_input_device = None;
    let mut bridge_output_device = None;
    let mut _input_stream = None;
    let capture_task = if args.mode.captures() {
        let (mic_tx, mic_rx) = mpsc::channel::<Vec<i16>>(8);
        let settings = Arc::new(AudioSettings::new(args.input_gain, 1.0));
        let (stream, device_name) =
            build_input_stream(mic_tx, settings, args.input_device.as_deref())?;
        bridge_input_device = Some(device_name);
        stream.play()?;
        _input_stream = Some(stream);
        Some(tokio::spawn(run_capture_sender(
            Arc::clone(&socket),
            Arc::clone(&runtime_config),
            mic_rx,
        )))
    } else {
        None
    };
    let mut _output_stream = None;
    let playback_task = if args.mode.plays() {
        let playback = Arc::new(Mutex::new(PlaybackBuffer::new(
            MIX_SAMPLES_PER_FRAME * 20,
            MIX_SAMPLES_PER_FRAME * 4,
        )));
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
        )))
    } else {
        None
    };
    let bridge_status_task = tokio::spawn(run_bridge_status(
        control_tx,
        Arc::clone(&runtime_config),
        BridgeStatusConfig {
            mode: args.mode,
            input_device: bridge_input_device,
            output_device: bridge_output_device,
            input_gain: args.input_gain,
            output_gain: args.output_gain,
            note: args.note,
        },
    ));

    tokio::select! {
        result = control_task => result.context("control task panicked")??,
        result = registration_task => result.context("registration task panicked")??,
        result = bridge_status_task => result.context("bridge status task panicked")??,
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
                bridge_status_snapshot(
                    &config,
                    BridgeStatusConfig {
                        input_device: bridge.input_device.clone(),
                        output_device: bridge.output_device.clone(),
                        note: bridge.note.clone(),
                        ..bridge
                    },
                ),
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

fn bridge_status_snapshot(config: &ClientConfig, bridge: BridgeStatusConfig) -> BridgeStatus {
    BridgeStatus {
        mode: bridge.mode.status_mode(),
        input_device: bridge.input_device,
        output_device: bridge.output_device,
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
        let payload = encoder.encode(&frame)?;
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
            packet.encode(&mut encoded)?;
            socket.send(&encoded).await?;
        }
    }
    Ok(())
}

async fn run_playback_receiver(
    socket: Arc<UdpSocket>,
    config: Arc<Mutex<ClientConfig>>,
    playback: Arc<Mutex<PlaybackBuffer>>,
) -> anyhow::Result<()> {
    let mut decoder = AudioDecoder::default();
    let mut buf = vec![0_u8; common::MAX_PACKET_BYTES];
    loop {
        let len = socket.recv(&mut buf).await?;
        let packet = match AudioPacket::decode(&buf[..len]) {
            Ok(packet) => packet,
            Err(err) => {
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
    let mut capture =
        CaptureAdapter::new(tx, config.sample_rate.0, config.channels as usize, settings);
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
    frame: Vec<i16>,
}

impl CaptureAdapter {
    fn new(
        tx: mpsc::Sender<Vec<i16>>,
        input_rate: u32,
        channels: usize,
        settings: Arc<AudioSettings>,
    ) -> Self {
        Self {
            tx,
            input_rate,
            channels: channels.max(1),
            settings,
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
                let _ = self.tx.try_send(output);
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
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: Esp32AudioConfig::default(),
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
                input_device: Some("BlackHole".to_string()),
                output_device: Some("USB Audio".to_string()),
                input_gain: 0.8,
                output_gain: 0.6,
                note: "vMix program bridge".to_string(),
            },
        );

        assert_eq!(status.mode, common::BridgeMode::Duplex);
        assert_eq!(status.input_device.as_deref(), Some("BlackHole"));
        assert_eq!(status.output_device.as_deref(), Some("USB Audio"));
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
        assert_eq!(args.note, "program feed");
    }
}
