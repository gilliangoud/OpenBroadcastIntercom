use std::collections::HashMap;

use anyhow::{bail, Context};
use clap::{ArgAction, Parser, Subcommand};
use common::{
    AppleComputeUnits, ChannelId, Codec, ControlMessage, ControlResponse, DeepFilterBackend,
    EmergencyTarget, IfbConfig, OpusProfile, ProcessingConfig, ProcessingEngine, ProcessingMode,
    ProcessingProfile, ProcessingStageConfig, TalkMode, UserId, DEFAULT_IFB_DUCK_GAIN,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, default_value = "ws://127.0.0.1:40001")]
    control: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Status,
    FieldReport {
        #[arg(long, default_value_t = 1)]
        min_sessions: usize,
        #[arg(long, default_value_t = 3)]
        max_queue_depth: usize,
        #[arg(long, default_value_t = 1_000)]
        max_age_ms: u64,
        #[arg(long)]
        require_audio: bool,
        #[arg(long)]
        json: bool,
    },
    Config {
        #[arg(long)]
        user_id: UserId,
        #[arg(long, value_parser = parse_channels)]
        listen: Vec<ChannelId>,
        #[arg(long, value_parser = parse_channels)]
        tx: Vec<ChannelId>,
        #[arg(long, value_parser = parse_volumes, default_value = "")]
        vol: HashMap<ChannelId, f32>,
        #[arg(long, value_parser = parse_codec)]
        codec: Option<Codec>,
        #[arg(long, value_parser = parse_opus_profile)]
        opus_profile: Option<OpusProfile>,
        #[arg(long, value_parser = parse_talk_mode)]
        talk_mode: Option<TalkMode>,
        #[arg(long)]
        ifb_enabled: Option<bool>,
        #[arg(long, value_parser = parse_channels)]
        ifb_program: Option<Vec<ChannelId>>,
        #[arg(long, value_parser = parse_channels)]
        ifb_interrupt: Option<Vec<ChannelId>>,
        #[arg(long)]
        ifb_duck_gain: Option<f32>,
        #[arg(long, value_parser = parse_channels)]
        priority_channels: Option<Vec<ChannelId>>,
        #[arg(long, value_parser = parse_processing_engine)]
        processing_engine: Option<ProcessingEngine>,
        #[arg(long, value_parser = parse_processing_mode)]
        processing_mode: Option<ProcessingMode>,
        #[arg(long, value_parser = parse_processing_profile)]
        processing_profile: Option<ProcessingProfile>,
        #[arg(long)]
        processing_fallback_to_builtin: Option<bool>,
        #[arg(long, value_parser = parse_processing_pipeline)]
        processing_pipeline: Option<Vec<ProcessingStageConfig>>,
        #[arg(long)]
        deep_filter_model: Option<String>,
        #[arg(long, value_parser = parse_deep_filter_backend)]
        deep_filter_backend: Option<DeepFilterBackend>,
        #[arg(long, value_parser = parse_apple_compute_units)]
        apple_compute_units: Option<AppleComputeUnits>,
    },
    Codec {
        #[arg(long)]
        user_id: UserId,
        #[arg(long, value_parser = parse_codec)]
        codec: Codec,
    },
    TalkMode {
        #[arg(long)]
        user_id: UserId,
        #[arg(long, value_parser = parse_talk_mode)]
        mode: TalkMode,
    },
    Talk {
        #[arg(long)]
        user_id: UserId,
        #[arg(long, action = ArgAction::Set)]
        active: bool,
    },
    Priority {
        #[arg(long)]
        user_id: UserId,
        #[arg(long, action = ArgAction::Set)]
        active: bool,
    },
    Emergency {
        #[arg(long)]
        user_id: UserId,
        #[arg(long, action = ArgAction::Set)]
        active: bool,
        #[arg(long, default_value = "all", value_parser = parse_emergency_target)]
        target: EmergencyTarget,
        #[arg(long, default_value_t = DEFAULT_IFB_DUCK_GAIN)]
        duck_gain: f32,
        #[arg(long, action = ArgAction::Set)]
        mute_others: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let message = match args.command {
        Command::Status => ControlMessage::Status,
        Command::FieldReport {
            min_sessions,
            max_queue_depth,
            max_age_ms,
            require_audio,
            json,
        } => {
            let response = send_control_message(&args.control, ControlMessage::Status).await?;
            print_field_report(
                response,
                min_sessions,
                max_queue_depth,
                max_age_ms,
                require_audio,
                json,
            )?;
            return Ok(());
        }
        Command::Config {
            user_id,
            listen,
            tx,
            vol,
            codec,
            opus_profile,
            talk_mode,
            ifb_enabled,
            ifb_program,
            ifb_interrupt,
            ifb_duck_gain,
            priority_channels,
            processing_engine,
            processing_mode,
            processing_profile,
            processing_fallback_to_builtin,
            processing_pipeline,
            deep_filter_model,
            deep_filter_backend,
            apple_compute_units,
        } => ControlMessage::Config {
            user_id,
            role: None,
            name: None,
            listen,
            tx,
            vol,
            talker_vol: None,
            codec,
            opus_profile,
            talk_mode,
            priority: None,
            priority_channels,
            processing: build_processing_config(ProcessingConfigFlags {
                engine: processing_engine,
                mode: processing_mode,
                profile: processing_profile,
                fallback_to_builtin: processing_fallback_to_builtin,
                pipeline: processing_pipeline,
                deep_filter_model,
                deep_filter_backend,
                apple_compute_units,
            }),
            buttons: None,
            ifb: build_ifb_config(ifb_enabled, ifb_program, ifb_interrupt, ifb_duck_gain),
            stereo: None,
            esp32_audio: None,
        },
        Command::Codec { user_id, codec } => ControlMessage::AudioCodec { user_id, codec },
        Command::TalkMode { user_id, mode } => ControlMessage::TalkMode { user_id, mode },
        Command::Talk { user_id, active } => ControlMessage::Talk { user_id, active },
        Command::Priority { user_id, active } => ControlMessage::Priority { user_id, active },
        Command::Emergency {
            user_id,
            active,
            target,
            duck_gain,
            mute_others,
        } => ControlMessage::Emergency {
            user_id,
            active,
            target,
            duck_gain,
            mute_others,
        },
    };

    let response = send_control_message(&args.control, message).await?;
    print_response(response)?;
    Ok(())
}

fn print_field_report(
    response: ControlResponse,
    min_sessions: usize,
    max_queue_depth: usize,
    max_age_ms: u64,
    require_audio: bool,
    json_output: bool,
) -> anyhow::Result<()> {
    let ControlResponse::Status { sessions, metrics } = response else {
        bail!("expected status response");
    };

    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    if sessions.len() < min_sessions {
        failures.push(format!(
            "expected at least {min_sessions} session(s), found {}",
            sessions.len()
        ));
    }
    if require_audio && metrics.audio_packets_received == 0 {
        failures.push("no audio packets received by server".to_string());
    }
    if metrics.malformed_packets_dropped > 0 {
        warnings.push(format!(
            "{} malformed packet(s) dropped",
            metrics.malformed_packets_dropped
        ));
    }
    if metrics.audio_decode_errors > 0 {
        warnings.push(format!(
            "{} audio decode error(s)",
            metrics.audio_decode_errors
        ));
    }
    if metrics.source_frames_dropped > 0 {
        warnings.push(format!(
            "{} source frame(s) dropped",
            metrics.source_frames_dropped
        ));
    }
    if metrics.audio_encode_errors > 0 {
        warnings.push(format!(
            "{} audio encode error(s)",
            metrics.audio_encode_errors
        ));
    }

    for session in &sessions {
        if session.queue_depth > max_queue_depth {
            failures.push(format!(
                "user {} queue depth {} exceeds {}",
                session.user_id, session.queue_depth, max_queue_depth
            ));
        }
        if session.age_ms > max_age_ms {
            failures.push(format!(
                "user {} last seen {} ms exceeds {} ms",
                session.user_id, session.age_ms, max_age_ms
            ));
        }
        if session.transport.source_frames_dropped > 0 {
            warnings.push(format!(
                "user {} dropped {} source frame(s)",
                session.user_id, session.transport.source_frames_dropped
            ));
        }
        if session.transport.decode_errors > 0 {
            warnings.push(format!(
                "user {} has {} decode error(s)",
                session.user_id, session.transport.decode_errors
            ));
        }
        if session.output.limiter_events > 0 {
            warnings.push(format!(
                "user {} limiter active {} time(s)",
                session.user_id, session.output.limiter_events
            ));
        }
    }

    let ok = failures.is_empty();
    if json_output {
        let sessions = sessions
            .iter()
            .map(|session| {
                serde_json::json!({
                    "user_id": session.user_id,
                    "addr": session.addr,
                    "codec": format_codec(session.codec),
                    "opus_profile": format_opus_profile(session.opus_profile),
                    "talk_mode": format!("{:?}", session.talk_mode),
                    "regular_talk_active": session.regular_talk_active,
                    "queue_depth": session.queue_depth,
                    "age_ms": session.age_ms,
                    "input_active": session.input.active,
                    "input_rms": session.input.rms,
                    "output_rms": session.output.rms,
                    "limiter_events": session.output.limiter_events,
                    "source_frames_dropped": session.transport.source_frames_dropped,
                    "decode_errors": session.transport.decode_errors,
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": ok,
                "failures": failures,
                "warnings": warnings,
                "metrics": metrics,
                "sessions": sessions,
            }))?
        );
    } else {
        println!(
            "field_report\tstatus={}\tsessions={}\taudio_rx={}\tmixed_tx={}\tqueue_drop={}\tdecode_err={}",
            if ok { "ok" } else { "fail" },
            sessions.len(),
            metrics.audio_packets_received,
            metrics.mixed_packets_sent,
            metrics.source_frames_dropped,
            metrics.audio_decode_errors
        );
        for session in &sessions {
            println!(
                "session\tuser={}\tcodec={}\topus_profile={}\ttalk_mode={:?}\tqueue={}\tage_ms={}\tin_active={}\tin_rms={:.3}\tout_rms={:.3}\tlimiter_events={}",
                session.user_id,
                format_codec(session.codec),
                format_opus_profile(session.opus_profile),
                session.talk_mode,
                session.queue_depth,
                session.age_ms,
                session.input.active,
                session.input.rms,
                session.output.rms,
                session.output.limiter_events
            );
        }
        for warning in &warnings {
            println!("warning\t{warning}");
        }
        for failure in &failures {
            println!("failure\t{failure}");
        }
    }

    if ok {
        Ok(())
    } else {
        bail!("field report failed")
    }
}

async fn send_control_message(
    control_url: &str,
    message: ControlMessage,
) -> anyhow::Result<ControlResponse> {
    let (mut ws, _) = tokio_tungstenite::connect_async(control_url)
        .await
        .with_context(|| format!("connect control WebSocket at {control_url}"))?;
    ws.send(Message::Text(serde_json::to_string(&message)?))
        .await?;

    let Some(reply) = ws.next().await else {
        bail!("control server closed without a response");
    };
    let reply = reply?;
    let Message::Text(text) = reply else {
        bail!("control server returned a non-text response");
    };

    Ok(serde_json::from_str(&text)?)
}

fn print_response(response: ControlResponse) -> anyhow::Result<()> {
    match response {
        ControlResponse::Hello {
            preconfigured,
            user_id,
            client_uid,
            enrollment,
        } => {
            println!(
                "hello user={user_id} client_uid={client_uid} enrollment={enrollment:?} preconfigured={preconfigured}"
            );
        }
        ControlResponse::Ack => {
            println!("ack");
        }
        ControlResponse::Error { message } => {
            bail!("{message}");
        }
        ControlResponse::Status { sessions, metrics } => {
            println!(
                "metrics\taudio_rx={}\tmalformed_drop={}\tdecode_err={}\tframes={}\tqueued={}\tqueue_drop={}\texpired_queues={}\tmixed_tx={}\tencode_err={}\tcontrol_rx={}",
                metrics.audio_packets_received,
                metrics.malformed_packets_dropped,
                metrics.audio_decode_errors,
                metrics.audio_frames_decoded,
                metrics.source_frames_enqueued,
                metrics.source_frames_dropped,
                metrics.expired_source_queues,
                metrics.mixed_packets_sent,
                metrics.audio_encode_errors,
                metrics.control_messages_received
            );

            if sessions.is_empty() {
                println!("no sessions");
                return Ok(());
            }

            println!("user\taddr\tcodec\topus_profile\tsupported\tprocessing\tprocessing_status\ttalk_mode\tregular_talk\tpriority\tpriority_channels\temergency\tifb\tifb_active\tqueue\tlisten\ttx\tage_ms");
            for session in sessions {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:?}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    session.user_id,
                    session.addr.unwrap_or_else(|| "-".to_string()),
                    format_codec(session.codec),
                    format_opus_profile(session.opus_profile),
                    format_codecs(&session.supported_codecs),
                    format_processing(&session.processing),
                    format_processing_status(&session.processing_status),
                    session.talk_mode,
                    session.regular_talk_active,
                    session.priority,
                    format_channels(&session.priority_channels),
                    format_emergency(session.emergency.as_ref()),
                    format_ifb(&session.ifb),
                    format_ifb_status(&session.ifb_status),
                    session.queue_depth,
                    format_channels(&session.listen),
                    format_channels(&session.tx),
                    session.age_ms
                );
            }
        }
    }

    Ok(())
}

fn build_ifb_config(
    enabled: Option<bool>,
    program: Option<Vec<ChannelId>>,
    interrupt: Option<Vec<ChannelId>>,
    duck_gain: Option<f32>,
) -> Option<IfbConfig> {
    if enabled.is_none() && program.is_none() && interrupt.is_none() && duck_gain.is_none() {
        return None;
    }

    Some(IfbConfig {
        enabled: enabled.unwrap_or(false),
        program: program.unwrap_or_default(),
        interrupt: interrupt.unwrap_or_default(),
        duck_gain: duck_gain.unwrap_or(DEFAULT_IFB_DUCK_GAIN),
    })
}

struct ProcessingConfigFlags {
    engine: Option<ProcessingEngine>,
    mode: Option<ProcessingMode>,
    profile: Option<ProcessingProfile>,
    fallback_to_builtin: Option<bool>,
    pipeline: Option<Vec<ProcessingStageConfig>>,
    deep_filter_model: Option<String>,
    deep_filter_backend: Option<DeepFilterBackend>,
    apple_compute_units: Option<AppleComputeUnits>,
}

fn build_processing_config(flags: ProcessingConfigFlags) -> Option<ProcessingConfig> {
    if flags.engine.is_none()
        && flags.mode.is_none()
        && flags.profile.is_none()
        && flags.fallback_to_builtin.is_none()
        && flags.pipeline.is_none()
        && flags.deep_filter_model.is_none()
        && flags.deep_filter_backend.is_none()
        && flags.apple_compute_units.is_none()
    {
        return None;
    }

    let mut config = ProcessingConfig::default();
    if let Some(engine) = flags.engine {
        config.engine = engine;
    }
    if let Some(mode) = flags.mode {
        config.mode = mode;
    }
    if let Some(profile) = flags.profile {
        config.profile = profile;
    }
    if let Some(fallback) = flags.fallback_to_builtin {
        config.fallback_to_builtin = fallback;
    }
    if let Some(pipeline) = flags.pipeline {
        config.pipeline = pipeline;
    }
    if let Some(model) = flags
        .deep_filter_model
        .filter(|value| !value.trim().is_empty())
    {
        config.deep_filter_model = Some(model);
    }
    if let Some(backend) = flags.deep_filter_backend {
        config.deep_filter_backend = backend;
    }
    if let Some(units) = flags.apple_compute_units {
        config.apple_compute_units = units;
    }
    Some(config)
}

fn parse_channels(value: &str) -> Result<Vec<ChannelId>, String> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }

    value
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<ChannelId>()
                .map_err(|err| format!("invalid channel `{part}`: {err}"))
        })
        .collect()
}

fn parse_users(value: &str) -> Result<Vec<UserId>, String> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }

    value
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<UserId>()
                .map_err(|err| format!("invalid user `{part}`: {err}"))
        })
        .collect()
}

fn parse_emergency_target(value: &str) -> Result<EmergencyTarget, String> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("all") {
        return Ok(EmergencyTarget::All);
    }
    let Some((kind, ids)) = value.split_once(':') else {
        return Err(
            "invalid emergency target, expected all, users:1,2, or channels:1,2".to_string(),
        );
    };
    match kind.trim().to_ascii_lowercase().as_str() {
        "user" | "users" => Ok(EmergencyTarget::Users {
            users: parse_users(ids)?,
        }),
        "channel" | "channels" => Ok(EmergencyTarget::Channels {
            channels: parse_channels(ids)?,
        }),
        other => Err(format!(
            "invalid emergency target `{other}`, expected all, users, or channels"
        )),
    }
}

fn parse_volumes(value: &str) -> Result<HashMap<ChannelId, f32>, String> {
    let mut volumes = HashMap::new();
    if value.trim().is_empty() {
        return Ok(volumes);
    }

    for entry in value.split(',') {
        let Some((channel, gain)) = entry.split_once('=') else {
            return Err(format!("invalid volume `{entry}`, expected channel=gain"));
        };
        let channel = channel
            .trim()
            .parse::<ChannelId>()
            .map_err(|err| format!("invalid channel `{channel}`: {err}"))?;
        let gain = gain
            .trim()
            .parse::<f32>()
            .map_err(|err| format!("invalid gain `{gain}`: {err}"))?;
        volumes.insert(channel, gain);
    }

    Ok(volumes)
}

fn parse_codec(value: &str) -> Result<Codec, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "pcm" | "pcm16" => Ok(Codec::Pcm16),
        "pcm24" | "pcm-24" => Ok(Codec::Pcm24),
        "pcm48" | "pcm-48" => Ok(Codec::Pcm48),
        "opus" => Ok(Codec::Opus),
        "adpcm" => Ok(Codec::Adpcm),
        other => Err(format!(
            "invalid codec `{other}`, expected pcm16, pcm24, pcm48, opus, or adpcm"
        )),
    }
}

fn parse_opus_profile(value: &str) -> Result<OpusProfile, String> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "speech_16_low" | "speech_low" | "low" => Ok(OpusProfile::Speech16Low),
        "speech_24_standard" | "speech_standard" | "standard" | "speech" => {
            Ok(OpusProfile::Speech24Standard)
        }
        "speech_48_high" | "speech_high" | "high" => Ok(OpusProfile::Speech48High),
        "music_48" | "music_high" | "music" => Ok(OpusProfile::Music48),
        other => Err(format!(
            "invalid opus profile `{other}`, expected speech-16-low, speech-24-standard, speech-48-high, or music-48"
        )),
    }
}

fn parse_talk_mode(value: &str) -> Result<TalkMode, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "muted" | "mute" => Ok(TalkMode::Muted),
        "ptt" => Ok(TalkMode::Ptt),
        "open" => Ok(TalkMode::Open),
        other => Err(format!(
            "invalid talk mode `{other}`, expected muted, ptt, or open"
        )),
    }
}

fn parse_processing_engine(value: &str) -> Result<ProcessingEngine, String> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "built_in" | "builtin" | "basic" => Ok(ProcessingEngine::BuiltIn),
        "webrtc" | "web_rtc" | "apm" => Ok(ProcessingEngine::WebRtc),
        "rnnoise" | "rn_noise" => Ok(ProcessingEngine::RnNoise),
        "deepfilternet" | "deep_filter_net" | "deep_filter" | "dfn" => {
            Ok(ProcessingEngine::DeepFilterNet)
        }
        other => Err(format!(
            "invalid processing engine `{other}`, expected built-in, webrtc, rnnoise, or deepfilternet"
        )),
    }
}

fn parse_processing_pipeline(value: &str) -> Result<Vec<ProcessingStageConfig>, String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            parse_processing_engine(part).map(|engine| ProcessingStageConfig {
                engine,
                enabled: true,
            })
        })
        .collect()
}

fn parse_deep_filter_backend(value: &str) -> Result<DeepFilterBackend, String> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "auto" => Ok(DeepFilterBackend::Auto),
        "tract" | "cpu" => Ok(DeepFilterBackend::Tract),
        "coreml" | "core_ml" | "apple" => Ok(DeepFilterBackend::CoreMl),
        other => Err(format!(
            "invalid DeepFilterNet backend `{other}`, expected auto, tract, or coreml"
        )),
    }
}

fn parse_apple_compute_units(value: &str) -> Result<AppleComputeUnits, String> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "all" => Ok(AppleComputeUnits::All),
        "cpu_only" | "cpu" => Ok(AppleComputeUnits::CpuOnly),
        "cpu_and_gpu" | "gpu" => Ok(AppleComputeUnits::CpuAndGpu),
        "cpu_and_neural_engine" | "neural_engine" | "ane" => {
            Ok(AppleComputeUnits::CpuAndNeuralEngine)
        }
        other => Err(format!(
            "invalid Apple compute units `{other}`, expected all, cpu-and-gpu, cpu-and-neural-engine, or cpu-only"
        )),
    }
}

fn parse_processing_mode(value: &str) -> Result<ProcessingMode, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(ProcessingMode::Auto),
        "enabled" | "enable" | "on" => Ok(ProcessingMode::Enabled),
        "disabled" | "disable" | "off" => Ok(ProcessingMode::Disabled),
        other => Err(format!(
            "invalid processing mode `{other}`, expected auto, enabled, or disabled"
        )),
    }
}

fn parse_processing_profile(value: &str) -> Result<ProcessingProfile, String> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "raw" => Ok(ProcessingProfile::Raw),
        "voice" => Ok(ProcessingProfile::Voice),
        "voice_isolation" | "isolation" => Ok(ProcessingProfile::VoiceIsolation),
        "broadcast" => Ok(ProcessingProfile::Broadcast),
        other => Err(format!(
            "invalid processing profile `{other}`, expected raw, voice, voice-isolation, or broadcast"
        )),
    }
}

fn format_channels(channels: &[ChannelId]) -> String {
    if channels.is_empty() {
        return "-".to_string();
    }

    channels
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn format_codec(codec: Codec) -> &'static str {
    match codec {
        Codec::Pcm16 => "pcm16",
        Codec::Pcm24 => "pcm24",
        Codec::Pcm48 => "pcm48",
        Codec::Adpcm => "adpcm",
        Codec::Opus => "opus",
    }
}

fn format_opus_profile(profile: OpusProfile) -> &'static str {
    match profile {
        OpusProfile::Speech16Low => "speech-16-low",
        OpusProfile::Speech24Standard => "speech-24-standard",
        OpusProfile::Speech48High => "speech-48-high",
        OpusProfile::Music48 => "music-48",
    }
}

fn format_codecs(codecs: &[Codec]) -> String {
    if codecs.is_empty() {
        return "-".to_string();
    }

    codecs
        .iter()
        .map(|codec| format_codec(*codec))
        .collect::<Vec<_>>()
        .join(",")
}

fn format_ifb(ifb: &IfbConfig) -> String {
    if !ifb.enabled {
        return "-".to_string();
    }
    format!(
        "program={} interrupt={} duck={}",
        format_channels(&ifb.program),
        format_channels(&ifb.interrupt),
        ifb.duck_gain
    )
}

fn format_ifb_status(status: &common::IfbStatus) -> String {
    if status.active {
        format!("active duck={}", status.duck_gain)
    } else {
        "inactive".to_string()
    }
}

fn format_processing(config: &ProcessingConfig) -> String {
    let pipeline = if config.pipeline.is_empty() {
        "-".to_string()
    } else {
        config
            .pipeline
            .iter()
            .filter(|stage| stage.enabled)
            .map(|stage| format_processing_engine(stage.engine))
            .collect::<Vec<_>>()
            .join(">")
    };
    format!(
        "engine={} mode={:?} profile={:?} dfn_backend={} apple_compute={} pipeline={}",
        format_processing_engine(config.engine),
        config.mode,
        config.profile,
        format_deep_filter_backend(config.deep_filter_backend),
        format_apple_compute_units(config.apple_compute_units),
        pipeline
    )
}

fn format_processing_engine(engine: ProcessingEngine) -> &'static str {
    match engine {
        ProcessingEngine::BuiltIn => "built-in",
        ProcessingEngine::WebRtc => "webrtc",
        ProcessingEngine::RnNoise => "rnnoise",
        ProcessingEngine::DeepFilterNet => "deepfilternet",
    }
}

fn format_deep_filter_backend(backend: DeepFilterBackend) -> &'static str {
    match backend {
        DeepFilterBackend::Auto => "auto",
        DeepFilterBackend::Tract => "tract",
        DeepFilterBackend::CoreMl => "coreml",
    }
}

fn format_apple_compute_units(units: AppleComputeUnits) -> &'static str {
    match units {
        AppleComputeUnits::All => "all",
        AppleComputeUnits::CpuAndGpu => "cpu-and-gpu",
        AppleComputeUnits::CpuAndNeuralEngine => "cpu-and-neural-engine",
        AppleComputeUnits::CpuOnly => "cpu-only",
    }
}

fn format_processing_status(status: &common::ProcessingStatus) -> String {
    let detail = status
        .engine_detail
        .as_deref()
        .filter(|detail| !detail.is_empty())
        .unwrap_or("-");
    format!(
        "{} {} active={} gate={} reduction={:.1}dB backend={} compute={} inference={} detail={}",
        format_processing_engine(status.engine),
        if status.engine_available {
            "available"
        } else {
            "unavailable"
        },
        status.active,
        status.gate_open,
        status.gain_reduction_db,
        status.backend.as_deref().unwrap_or("-"),
        status.compute_units.as_deref().unwrap_or("-"),
        status
            .inference_ms
            .map(|ms| format!("{ms:.1}ms"))
            .unwrap_or_else(|| "-".to_string()),
        detail
    )
}

fn format_emergency(status: Option<&common::EmergencyStatus>) -> String {
    let Some(status) = status.filter(|status| status.active) else {
        return "-".to_string();
    };
    format!(
        "source={} recipients={} {}",
        status.source,
        format_users(&status.recipients),
        if status.mute_others { "mute" } else { "duck" }
    )
}

fn format_users(users: &[UserId]) -> String {
    if users.is_empty() {
        return "-".to_string();
    }
    users
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        InputMeterStatus, OutputMeterStatus, SessionStatus, StatusMetrics, TransportHealthStatus,
    };

    #[test]
    fn ifb_config_flags_build_optional_config() {
        assert_eq!(build_ifb_config(None, None, None, None), None);

        assert_eq!(
            build_ifb_config(Some(true), Some(vec![1, 2]), Some(vec![9]), Some(0.2)),
            Some(IfbConfig {
                enabled: true,
                program: vec![1, 2],
                interrupt: vec![9],
                duck_gain: 0.2,
            })
        );
    }

    #[test]
    fn status_formats_ifb_columns() {
        let ifb = IfbConfig {
            enabled: true,
            program: vec![1],
            interrupt: vec![9],
            duck_gain: 0.125,
        };
        assert!(format_ifb(&ifb).contains("program=1"));
        assert_eq!(format_ifb_status(&common::IfbStatus::default()), "inactive");
    }

    #[test]
    fn field_report_passes_healthy_status() {
        let response = ControlResponse::Status {
            sessions: vec![session_status(1)],
            metrics: StatusMetrics {
                audio_packets_received: 20,
                mixed_packets_sent: 10,
                ..StatusMetrics::default()
            },
        };

        assert!(print_field_report(response, 1, 3, 1_000, true, false).is_ok());
    }

    #[test]
    fn field_report_fails_missing_audio_when_required() {
        let response = ControlResponse::Status {
            sessions: vec![session_status(1)],
            metrics: StatusMetrics::default(),
        };

        assert!(print_field_report(response, 1, 3, 1_000, true, false).is_err());
    }

    #[test]
    fn field_report_fails_stale_or_backed_up_sessions() {
        let mut session = session_status(1);
        session.queue_depth = 4;
        session.age_ms = 2_000;
        let response = ControlResponse::Status {
            sessions: vec![session],
            metrics: StatusMetrics {
                audio_packets_received: 20,
                ..StatusMetrics::default()
            },
        };

        assert!(print_field_report(response, 1, 3, 1_000, true, true).is_err());
    }

    fn session_status(user_id: UserId) -> SessionStatus {
        SessionStatus {
            user_id,
            client_uid: format!("test-{user_id}"),
            enrollment: common::EnrollmentStatus::Enrolled,
            role: common::ClientRole::Client,
            addr: Some("127.0.0.1:50000".to_string()),
            listen: vec![1],
            tx: vec![1],
            talker_vol: HashMap::new(),
            codec: Codec::Pcm48,
            opus_profile: OpusProfile::Speech24Standard,
            supported_codecs: vec![Codec::Pcm16, Codec::Pcm24, Codec::Pcm48, Codec::Opus],
            advertised_buttons: Vec::new(),
            capabilities: common::ClientCapabilities::default(),
            buttons: Vec::new(),
            active_buttons: Vec::new(),
            active_direct_calls: Vec::new(),
            last_direct_caller: None,
            direct_call_history: Vec::new(),
            active_alerts: Vec::new(),
            recent_alerts: Vec::new(),
            ifb: IfbConfig::default(),
            lockout: common::ClientLockoutPolicy::default(),
            ifb_status: common::IfbStatus::default(),
            stereo: common::StereoConfig::default(),
            esp32_audio: common::Esp32AudioConfig::default(),
            stereo_status: common::StereoStatus::default(),
            talk_mode: TalkMode::Open,
            regular_talk_active: false,
            priority: false,
            priority_channels: Vec::new(),
            processing: common::ProcessingConfig::default(),
            processing_status: common::ProcessingStatus::default(),
            emergency: None,
            queue_depth: 0,
            age_ms: 25,
            input: InputMeterStatus {
                active: true,
                peak: 0.4,
                rms: 0.2,
                last_channel: Some(1),
                last_packet_age_ms: Some(10),
            },
            output: OutputMeterStatus {
                peak: 0.3,
                rms: 0.15,
                limiter_gain: 1.0,
                limiter_reduction_db: 0.0,
                limiter_events: 0,
            },
            capture: None,
            bridge: None,
            tally: common::TallyStatus::default(),
            transport: TransportHealthStatus::default(),
            recording_enabled: false,
            transcription_enabled: false,
        }
    }
}
