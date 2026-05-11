use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::future;
use std::io::{BufWriter, ErrorKind, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(feature = "processing-deepfilternet")]
use std::sync::mpsc as std_mpsc;

use anyhow::{bail, Context};
use axum::extract::Request;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use common::{
    codec_pcm16_payload_bytes, codec_sample_rate, codec_samples_per_frame,
    pcm16_le_bytes_to_samples, pcm16_samples_to_le_bytes, resample_linear, AlertId,
    AlertRecipientStatus, AlertStatus, AlertTarget, AppleComputeUnits, AudioPacket, AudioTarget,
    BridgeStatus, ButtonCapability, ButtonId, CaptureHealthStatus, ChannelId,
    ChannelPresenceMember, ChannelPresenceRoster, ClientLockoutPolicy, ClientRole, ClientUid,
    Codec, ControlEvent, ControlMessage, ControlResponse, DeepFilterBackend,
    DirectCallHistoryEntry, DirectCallStatus, EmergencyStatus, EmergencyTarget, EnrollmentStatus,
    Esp32AudioConfig, IfbConfig, IfbStatus, InputMeterStatus, LevelNormalizationConfig,
    LevelNormalizationStatus, OpusBandwidth, OpusProfile, OutputMeterStatus, PcmFrameResampler,
    ProcessingConfig, ProcessingEngine, ProcessingMode, ProcessingProfile, ProcessingStageConfig,
    ProcessingStageStatus, ProcessingStatus, SessionStatus, StatusMetrics, StereoConfig,
    StereoStatus, TalkButtonAction, TalkButtonConfig, TalkButtonMode, TalkMode,
    TransportHealthStatus, UserId, DEFAULT_IFB_DUCK_GAIN, MAX_PACKET_BYTES, MIX_SAMPLES_PER_FRAME,
    MIX_SAMPLE_RATE, PCM48_STEREO_PAYLOAD_BYTES, SERVER_USER_ID,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, UdpSocket};
use tokio::process::Command;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::{error::ProtocolError, Error as WsError, Message};

#[cfg(feature = "processing-deepfilternet")]
use df::tract::{DfParams, DfTract, RuntimeParams};
#[cfg(feature = "processing-deepfilternet")]
use ndarray::Array2;
#[cfg(feature = "processing-webrtc")]
use webrtc_audio_processing::config::{
    Config as WebRtcConfig, GainController, GainController1, GainControllerMode, HighPassFilter,
    NoiseSuppression, NoiseSuppressionLevel, Pipeline, PipelineProcessingRate,
};
#[cfg(feature = "transcription-whisper")]
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

#[cfg(all(feature = "processing-deepfilternet-coreml", target_os = "macos"))]
mod deepfilternet_coreml;
mod discovery;
#[cfg(feature = "tts-supertonic")]
mod supertonic_tts;

#[cfg(test)]
pub(crate) use discovery::discovery_command;
pub use discovery::{
    default_discovery_name, start_discovery_advertisement, DiscoveryAdvertisement,
    DiscoveryAdvertisementHandle, DISCOVERY_SERVICE_TYPE,
};

const MIX_INTERVAL: Duration = Duration::from_millis(common::FRAME_MS as u64);
const ACTIVE_SOURCE_WINDOW: Duration = Duration::from_millis(80);
const MAX_SOURCE_QUEUE_FRAMES: usize = 6;
const PRIORITY_DUCK_GAIN: f32 = 0.25;
const LIMITER_THRESHOLD: f32 = 0.95;
const ACTIVE_TALKER_RMS_THRESHOLD: f32 = 0.02;
const ACTIVE_TALKER_HOLD: Duration = Duration::from_millis(350);
const LIVE_TRANSCRIPTION_SILENCE_FRAMES: usize = 70;
const LIVE_TRANSCRIPTION_MAX_FRAMES: usize = 600;
const LIVE_TRANSCRIPTION_OVERLAP_FRAMES: usize = 50;
const LIVE_TRANSCRIPTION_MIN_SPEECH_FRAMES: usize = 20;
const LIVE_TRANSCRIPTION_QUEUE_LIMIT: usize = 4;
const LIVE_TRANSCRIPTION_RMS_THRESHOLD: f32 = 0.018;
const TTS_SOURCE_USER_BASE: UserId = 64_000;
const TTS_SOURCE_USER_COUNT: UserId = 1_000;
const TTS_MAX_MESSAGE_CHARS: usize = 240;
const TTS_DEFAULT_GAIN: f32 = 0.18;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum TranscriptionEngineMode {
    #[default]
    Disabled,
    BuiltinWhisper,
    ExternalWhisper,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum EnrollmentPolicy {
    #[default]
    Auto,
    Approval,
    PreconfiguredOnly,
}

#[derive(Clone, Debug, Default)]
pub struct HttpAuthConfig {
    token: Option<Arc<str>>,
    realm: &'static str,
}

impl HttpAuthConfig {
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

    pub fn is_enabled(&self) -> bool {
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

async fn require_http_auth(
    State(auth): State<HttpAuthConfig>,
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

pub struct ServerState {
    sessions: RwLock<HashMap<UserId, Session>>,
    sources: RwLock<HashMap<UserId, SourceQueue>>,
    virtual_sources: RwLock<Vec<VirtualAudioSource>>,
    output_seq: RwLock<HashMap<UserId, u16>>,
    control_clients: RwLock<HashMap<UserId, mpsc::UnboundedSender<ControlEvent>>>,
    health: RwLock<HashMap<UserId, UserHealth>>,
    alerts: RwLock<Vec<RuntimeAlert>>,
    emergency: RwLock<Option<RuntimeEmergency>>,
    recording: RwLock<RecordingState>,
    transcription: RwLock<LiveTranscriptionState>,
    deepfilternet_model_dir: RwLock<PathBuf>,
    debug_audio_tx: RwLock<Option<mpsc::Sender<ServerDebugAudioFrame>>>,
    next_alert_id: AtomicU64,
    next_tts_id: AtomicU64,
    admin_state: RwLock<PersistedAdminState>,
    admin_state_file: Option<PathBuf>,
    enrollment_policy: EnrollmentPolicy,
    metrics: Metrics,
}

impl Default for ServerState {
    fn default() -> Self {
        Self::new_with_admin_state(PersistedAdminState::default(), None, EnrollmentPolicy::Auto)
    }
}

impl ServerState {
    fn new_with_admin_state(
        admin_state: PersistedAdminState,
        admin_state_file: Option<PathBuf>,
        enrollment_policy: EnrollmentPolicy,
    ) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            sources: RwLock::new(HashMap::new()),
            virtual_sources: RwLock::new(Vec::new()),
            output_seq: RwLock::new(HashMap::new()),
            control_clients: RwLock::new(HashMap::new()),
            health: RwLock::new(HashMap::new()),
            alerts: RwLock::new(Vec::new()),
            emergency: RwLock::new(None),
            recording: RwLock::new(RecordingState::default()),
            transcription: RwLock::new(LiveTranscriptionState::default()),
            deepfilternet_model_dir: RwLock::new(PathBuf::from("deepfilternet-models")),
            debug_audio_tx: RwLock::new(None),
            next_alert_id: AtomicU64::new(1),
            next_tts_id: AtomicU64::new(1),
            admin_state: RwLock::new(admin_state),
            admin_state_file,
            enrollment_policy,
            metrics: Metrics::default(),
        }
    }

    async fn load(
        admin_state_file: Option<PathBuf>,
        enrollment_policy: EnrollmentPolicy,
    ) -> anyhow::Result<Self> {
        let admin_state = match admin_state_file.as_deref() {
            Some(path) => load_admin_state(path).await?,
            None => PersistedAdminState::default(),
        };
        Ok(Self::new_with_admin_state(
            admin_state,
            admin_state_file,
            enrollment_policy,
        ))
    }
}

#[derive(Debug, Clone)]
struct UserHealth {
    input: InputMeterStatus,
    output: OutputMeterStatus,
    capture: Option<CaptureHealthStatus>,
    transport: TransportHealthStatus,
    processing: ProcessingStatus,
    last_packet_seen: Option<Instant>,
    active_until: Instant,
}

impl Default for UserHealth {
    fn default() -> Self {
        Self {
            input: InputMeterStatus::default(),
            output: OutputMeterStatus {
                limiter_gain: 1.0,
                ..OutputMeterStatus::default()
            },
            capture: None,
            transport: TransportHealthStatus::default(),
            processing: ProcessingStatus {
                bypassed: true,
                ..ProcessingStatus::default()
            },
            last_packet_seen: None,
            active_until: Instant::now() - ACTIVE_TALKER_HOLD,
        }
    }
}

#[derive(Debug, Default)]
struct Metrics {
    audio_packets_received: AtomicU64,
    malformed_packets_dropped: AtomicU64,
    audio_decode_errors: AtomicU64,
    audio_frames_decoded: AtomicU64,
    source_frames_enqueued: AtomicU64,
    source_frames_dropped: AtomicU64,
    expired_source_queues: AtomicU64,
    mixed_packets_sent: AtomicU64,
    audio_encode_errors: AtomicU64,
    audio_send_errors: AtomicU64,
    control_messages_received: AtomicU64,
}

impl Metrics {
    fn snapshot(&self) -> StatusMetrics {
        StatusMetrics {
            audio_packets_received: self.audio_packets_received.load(Ordering::Relaxed),
            malformed_packets_dropped: self.malformed_packets_dropped.load(Ordering::Relaxed),
            audio_decode_errors: self.audio_decode_errors.load(Ordering::Relaxed),
            audio_frames_decoded: self.audio_frames_decoded.load(Ordering::Relaxed),
            source_frames_enqueued: self.source_frames_enqueued.load(Ordering::Relaxed),
            source_frames_dropped: self.source_frames_dropped.load(Ordering::Relaxed),
            expired_source_queues: self.expired_source_queues.load(Ordering::Relaxed),
            mixed_packets_sent: self.mixed_packets_sent.load(Ordering::Relaxed),
            audio_encode_errors: self.audio_encode_errors.load(Ordering::Relaxed),
            audio_send_errors: self.audio_send_errors.load(Ordering::Relaxed),
            control_messages_received: self.control_messages_received.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone)]
struct Session {
    client_uid: ClientUid,
    enrollment: EnrollmentStatus,
    addr: Option<SocketAddr>,
    role: ClientRole,
    listen_channels: HashSet<ChannelId>,
    tx_channels: HashSet<ChannelId>,
    channel_volumes: HashMap<ChannelId, f32>,
    talker_volumes: HashMap<UserId, f32>,
    output_codec: Codec,
    opus_profile: OpusProfile,
    supported_codecs: HashSet<Codec>,
    advertised_buttons: Vec<ButtonCapability>,
    buttons: Vec<TalkButtonConfig>,
    active_buttons: HashSet<ButtonId>,
    active_direct_calls: HashMap<UserId, ActiveDirectCall>,
    last_direct_caller: Option<UserId>,
    direct_call_history: Vec<DirectCallHistory>,
    ifb: IfbConfig,
    ifb_status: IfbStatus,
    lockout: ClientLockoutPolicy,
    stereo: StereoConfig,
    esp32_audio: Esp32AudioConfig,
    processing: ProcessingConfig,
    bridge: Option<BridgeStatus>,
    name: String,
    talk_mode: TalkMode,
    regular_talk_active: bool,
    priority: bool,
    priority_channels: HashSet<ChannelId>,
    last_seen: Instant,
}

#[derive(Debug, Clone)]
struct ActiveDirectCall {
    duck: bool,
}

#[derive(Debug, Clone)]
struct DirectCallHistory {
    caller: UserId,
    target: UserId,
    started: Instant,
    ended: Option<Instant>,
    duck: bool,
    source_button: Option<ButtonId>,
}

#[derive(Debug, Clone)]
struct RuntimeAlert {
    id: AlertId,
    sender: UserId,
    target: AlertTarget,
    message: Option<String>,
    created_at_ms: u64,
    recipients: Vec<AlertRecipientStatus>,
    cancelled: bool,
    cancelled_at_ms: Option<u64>,
}

#[derive(Debug, Clone)]
struct RuntimeEmergency {
    source: UserId,
    target: EmergencyTarget,
    duck_gain: f32,
    mute_others: bool,
}

impl Session {
    fn new() -> Self {
        Self {
            addr: None,
            client_uid: String::new(),
            enrollment: EnrollmentStatus::Enrolled,
            role: ClientRole::Client,
            listen_channels: HashSet::new(),
            tx_channels: HashSet::new(),
            channel_volumes: HashMap::new(),
            talker_volumes: HashMap::new(),
            output_codec: Codec::Pcm16,
            opus_profile: OpusProfile::default(),
            supported_codecs: [Codec::Pcm16].into(),
            advertised_buttons: Vec::new(),
            buttons: Vec::new(),
            active_buttons: HashSet::new(),
            active_direct_calls: HashMap::new(),
            last_direct_caller: None,
            direct_call_history: Vec::new(),
            ifb: IfbConfig::default(),
            ifb_status: IfbStatus::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: Esp32AudioConfig::default(),
            processing: ProcessingConfig::default(),
            bridge: None,
            name: String::new(),
            talk_mode: TalkMode::Ptt,
            regular_talk_active: false,
            priority: false,
            priority_channels: HashSet::new(),
            last_seen: Instant::now(),
        }
    }

    fn control_event(
        &self,
        user_id: UserId,
        active_direct_calls: Vec<DirectCallStatus>,
        active_alerts: Vec<AlertStatus>,
        recent_alerts: Vec<AlertStatus>,
        emergency: Option<EmergencyStatus>,
    ) -> ControlEvent {
        let mut listen = self.listen_channels.iter().copied().collect::<Vec<_>>();
        listen.sort_unstable();
        let mut tx = self.tx_channels.iter().copied().collect::<Vec<_>>();
        tx.sort_unstable();
        let mut active_buttons = self.active_buttons.iter().cloned().collect::<Vec<_>>();
        active_buttons.sort();
        let mut priority_channels = self.priority_channels.iter().copied().collect::<Vec<_>>();
        priority_channels.sort_unstable();
        let direct_call_history = direct_call_history_entries(&self.direct_call_history);

        ControlEvent::ConfigUpdate {
            user_id,
            client_uid: self.client_uid.clone(),
            name: self.name.clone(),
            listen,
            tx,
            vol: self.channel_volumes.clone(),
            talker_vol: self.talker_volumes.clone(),
            codec: self.output_codec,
            opus_profile: self.opus_profile,
            talk_mode: self.talk_mode,
            regular_talk_active: self.regular_talk_active,
            priority: self.priority,
            priority_channels,
            buttons: sorted_buttons(&self.buttons),
            active_buttons,
            active_direct_calls,
            last_direct_caller: self.last_direct_caller,
            direct_call_history,
            active_alerts,
            recent_alerts,
            emergency,
            ifb: self.ifb.clone(),
            lockout: self.lockout.clone(),
            stereo: self.stereo.clone(),
            esp32_audio: self.esp32_audio.clone(),
            processing: self.processing.clone(),
        }
    }

    fn effective_tx_channels(&self) -> HashSet<ChannelId> {
        let mut tx = HashSet::new();
        if self.talk_mode == TalkMode::Open
            || (self.talk_mode == TalkMode::Ptt && self.regular_talk_active)
        {
            tx.extend(self.tx_channels.iter().copied());
        }
        for button in &self.buttons {
            if self.active_buttons.contains(&button.id) {
                for action in &button.actions {
                    if let TalkButtonAction::Transmit { channels, .. } = action {
                        tx.extend(channels.iter().copied());
                    }
                }
            }
        }
        tx
    }

    fn active_tx_targets(&self) -> HashSet<AudioTarget> {
        let mut targets = self
            .effective_tx_channels()
            .into_iter()
            .map(AudioTarget::Channel)
            .collect::<HashSet<_>>();
        targets.extend(
            self.active_direct_calls
                .keys()
                .copied()
                .map(AudioTarget::Direct),
        );
        for button in &self.buttons {
            if !self.active_buttons.contains(&button.id) {
                continue;
            }
            for action in &button.actions {
                if let TalkButtonAction::Transmit { users, .. } = action {
                    targets.extend(users.iter().copied().map(AudioTarget::Direct));
                }
            }
        }
        targets
    }
}

fn millis_since(started: Instant, instant: Instant) -> u64 {
    instant
        .saturating_duration_since(started)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

impl RuntimeAlert {
    fn status(&self) -> AlertStatus {
        AlertStatus {
            id: self.id,
            sender: self.sender,
            sender_name: None,
            target: self.target,
            message: self.message.clone(),
            created_at_ms: self.created_at_ms,
            recipients: self.recipients.clone(),
            cancelled: self.cancelled,
            cancelled_at_ms: self.cancelled_at_ms,
        }
    }

    fn active_for(&self, user_id: UserId) -> bool {
        !self.cancelled
            && self
                .recipients
                .iter()
                .any(|recipient| recipient.user_id == user_id && recipient.acked_at_ms.is_none())
    }

    fn relevant_for(&self, user_id: UserId) -> bool {
        self.sender == user_id
            || self
                .recipients
                .iter()
                .any(|recipient| recipient.user_id == user_id)
    }

    fn active(&self) -> bool {
        !self.cancelled
            && self
                .recipients
                .iter()
                .any(|recipient| recipient.acked_at_ms.is_none())
    }
}

fn session_display_name(sessions: &HashMap<UserId, Session>, user_id: UserId) -> Option<String> {
    sessions
        .get(&user_id)
        .map(|session| session.name.trim())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn alert_status_with_sessions(
    alert: &RuntimeAlert,
    sessions: &HashMap<UserId, Session>,
) -> AlertStatus {
    let mut status = alert.status();
    status.sender_name = session_display_name(sessions, alert.sender);
    status
}

#[cfg(test)]
fn alert_statuses_for_user(
    alerts: &[RuntimeAlert],
    user_id: UserId,
) -> (Vec<AlertStatus>, Vec<AlertStatus>) {
    let active = alerts
        .iter()
        .filter(|alert| alert.active_for(user_id))
        .map(RuntimeAlert::status)
        .collect::<Vec<_>>();
    let recent = alerts
        .iter()
        .filter(|alert| alert.relevant_for(user_id) && !alert.active_for(user_id))
        .rev()
        .take(20)
        .map(RuntimeAlert::status)
        .collect::<Vec<_>>();
    (active, recent)
}

fn alert_statuses_for_user_with_sessions(
    alerts: &[RuntimeAlert],
    user_id: UserId,
    sessions: &HashMap<UserId, Session>,
) -> (Vec<AlertStatus>, Vec<AlertStatus>) {
    let active = alerts
        .iter()
        .filter(|alert| alert.active_for(user_id))
        .map(|alert| alert_status_with_sessions(alert, sessions))
        .collect::<Vec<_>>();
    let recent = alerts
        .iter()
        .filter(|alert| alert.relevant_for(user_id) && !alert.active_for(user_id))
        .rev()
        .take(20)
        .map(|alert| alert_status_with_sessions(alert, sessions))
        .collect::<Vec<_>>();
    (active, recent)
}

fn admin_alert_statuses_with_sessions(
    alerts: &[RuntimeAlert],
    sessions: &HashMap<UserId, Session>,
) -> (Vec<AlertStatus>, Vec<AlertStatus>) {
    let active = alerts
        .iter()
        .filter(|alert| alert.active())
        .map(|alert| alert_status_with_sessions(alert, sessions))
        .collect::<Vec<_>>();
    let recent = alerts
        .iter()
        .filter(|alert| !alert.active())
        .rev()
        .take(50)
        .map(|alert| alert_status_with_sessions(alert, sessions))
        .collect::<Vec<_>>();
    (active, recent)
}

impl RuntimeEmergency {
    fn status(&self, recipients: Vec<UserId>) -> EmergencyStatus {
        EmergencyStatus {
            active: true,
            source: self.source,
            target: self.target.clone(),
            duck_gain: self.duck_gain,
            mute_others: self.mute_others,
            recipients,
        }
    }
}

fn emergency_recipients_for_sessions(
    emergency: &RuntimeEmergency,
    sessions: &HashMap<UserId, Session>,
) -> Vec<UserId> {
    let mut recipients = match &emergency.target {
        EmergencyTarget::All => sessions
            .keys()
            .copied()
            .filter(|user_id| *user_id != emergency.source)
            .collect::<Vec<_>>(),
        EmergencyTarget::Users { users } => users
            .iter()
            .copied()
            .filter(|user_id| *user_id != emergency.source && sessions.contains_key(user_id))
            .collect::<Vec<_>>(),
        EmergencyTarget::Channels { channels } => sessions
            .iter()
            .filter_map(|(&user_id, session)| {
                if user_id != emergency.source
                    && channels
                        .iter()
                        .any(|channel| session.listen_channels.contains(channel))
                {
                    Some(user_id)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>(),
    };
    recipients.sort_unstable();
    recipients.dedup();
    recipients
}

fn emergency_status_for_user(
    emergency: Option<&RuntimeEmergency>,
    sessions: &HashMap<UserId, Session>,
    user_id: UserId,
) -> Option<EmergencyStatus> {
    let emergency = emergency?;
    let recipients = emergency_recipients_for_sessions(emergency, sessions);
    if emergency.source == user_id || recipients.contains(&user_id) {
        Some(emergency.status(recipients))
    } else {
        None
    }
}

fn active_direct_call_statuses_for_user(
    user_id: UserId,
    sessions: &HashMap<UserId, Session>,
) -> Vec<DirectCallStatus> {
    let mut calls = Vec::new();
    for (&caller, session) in sessions {
        for (&target, call) in &session.active_direct_calls {
            if caller == user_id || target == user_id {
                calls.push(DirectCallStatus {
                    caller,
                    caller_name: session_display_name(sessions, caller),
                    target,
                    target_name: session_display_name(sessions, target),
                    active: true,
                    duck: call.duck,
                });
            }
        }
        for button in &session.buttons {
            if !session.active_buttons.contains(&button.id) {
                continue;
            }
            for action in &button.actions {
                if let TalkButtonAction::Transmit { users, duck, .. } = action {
                    for &target in users {
                        if caller == user_id || target == user_id {
                            calls.push(DirectCallStatus {
                                caller,
                                caller_name: session_display_name(sessions, caller),
                                target,
                                target_name: session_display_name(sessions, target),
                                active: true,
                                duck: *duck,
                            });
                        }
                    }
                }
            }
        }
    }
    calls.sort_by_key(|call| (call.caller, call.target, call.duck));
    calls.dedup_by_key(|call| (call.caller, call.target, call.duck));
    calls
}

fn direct_call_history_entries(history: &[DirectCallHistory]) -> Vec<DirectCallHistoryEntry> {
    history
        .iter()
        .map(|entry| DirectCallHistoryEntry {
            caller: entry.caller,
            target: entry.target,
            started_at_ms: millis_since(entry.started, entry.started),
            ended_at_ms: entry.ended.map(|ended| millis_since(entry.started, ended)),
            duck: entry.duck,
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct PersistedAdminState {
    #[serde(default)]
    channels: Vec<ChannelConfig>,
    #[serde(default)]
    devices: Vec<DeviceEnrollment>,
    #[serde(default)]
    clients: Vec<DesiredClientConfig>,
    #[serde(default)]
    presets: Vec<PresetConfig>,
    #[serde(default)]
    templates: Vec<ClientTemplateConfig>,
}

impl Default for PersistedAdminState {
    fn default() -> Self {
        let mut state = Self {
            channels: default_workflow_channels(),
            devices: Vec::new(),
            clients: Vec::new(),
            presets: default_workflow_presets(),
            templates: default_workflow_templates(),
        };
        normalize_admin_state(&mut state);
        state
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ChannelConfig {
    id: ChannelId,
    name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct DeviceEnrollment {
    client_uid: ClientUid,
    user_id: UserId,
    #[serde(default)]
    status: EnrollmentStatus,
    #[serde(default)]
    name: String,
    #[serde(default)]
    role: ClientRole,
    #[serde(default)]
    first_seen_ms: u64,
    #[serde(default)]
    last_seen_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hardware_fingerprint: Option<String>,
    #[serde(default)]
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct PresetConfig {
    id: String,
    name: String,
    #[serde(default)]
    clients: Vec<DesiredClientConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ClientTemplateConfig {
    id: String,
    name: String,
    #[serde(default)]
    client: ClientTemplateClientConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ClientTemplateClientConfig {
    #[serde(default)]
    role: ClientRole,
    #[serde(default)]
    name: String,
    #[serde(default)]
    listen: Vec<ChannelId>,
    #[serde(default)]
    tx: Vec<ChannelId>,
    #[serde(default)]
    vol: HashMap<ChannelId, f32>,
    #[serde(default, with = "common::user_gain_map")]
    talker_vol: HashMap<UserId, f32>,
    #[serde(default = "default_codec")]
    codec: Codec,
    #[serde(default)]
    opus_profile: OpusProfile,
    #[serde(default)]
    talk_mode: TalkMode,
    #[serde(default)]
    priority: bool,
    #[serde(default)]
    priority_channels: Vec<ChannelId>,
    #[serde(default)]
    buttons: Vec<TalkButtonConfig>,
    #[serde(default)]
    ifb: IfbConfig,
    #[serde(default)]
    lockout: ClientLockoutPolicy,
    #[serde(default)]
    stereo: StereoConfig,
    #[serde(default)]
    esp32_audio: Esp32AudioConfig,
    #[serde(default)]
    processing: ProcessingConfig,
}

impl Default for ClientTemplateClientConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            role: ClientRole::Client,
            listen: Vec::new(),
            tx: Vec::new(),
            vol: HashMap::new(),
            talker_vol: HashMap::new(),
            codec: Codec::Pcm16,
            opus_profile: OpusProfile::default(),
            talk_mode: TalkMode::Ptt,
            priority: false,
            priority_channels: Vec::new(),
            buttons: Vec::new(),
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: Esp32AudioConfig::default(),
            processing: ProcessingConfig::default(),
        }
    }
}

impl ClientTemplateClientConfig {
    fn to_desired(&self, user_id: UserId) -> DesiredClientConfig {
        DesiredClientConfig {
            user_id,
            client_uid: None,
            role: self.role,
            name: self.name.clone(),
            listen: self.listen.clone(),
            tx: self.tx.clone(),
            vol: self.vol.clone(),
            talker_vol: normalize_talker_volumes(self.talker_vol.clone(), user_id),
            codec: self.codec,
            opus_profile: self.opus_profile,
            talk_mode: self.talk_mode,
            priority: self.priority,
            priority_channels: sorted_unique_channels(self.priority_channels.clone()),
            buttons: normalize_button_configs(self.buttons.clone()),
            ifb: normalize_ifb_config(self.ifb.clone()),
            lockout: self.lockout.clone(),
            stereo: normalize_stereo_config(self.stereo.clone()),
            esp32_audio: normalize_esp32_audio_config(self.esp32_audio.clone()),
            processing: self.processing.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct DesiredClientConfig {
    user_id: UserId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_uid: Option<ClientUid>,
    #[serde(default)]
    role: ClientRole,
    #[serde(default)]
    name: String,
    #[serde(default)]
    listen: Vec<ChannelId>,
    #[serde(default)]
    tx: Vec<ChannelId>,
    #[serde(default)]
    vol: HashMap<ChannelId, f32>,
    #[serde(default, with = "common::user_gain_map")]
    talker_vol: HashMap<UserId, f32>,
    #[serde(default = "default_codec")]
    codec: Codec,
    #[serde(default)]
    opus_profile: OpusProfile,
    #[serde(default)]
    talk_mode: TalkMode,
    #[serde(default)]
    priority: bool,
    #[serde(default)]
    priority_channels: Vec<ChannelId>,
    #[serde(default)]
    buttons: Vec<TalkButtonConfig>,
    #[serde(default)]
    ifb: IfbConfig,
    #[serde(default)]
    lockout: ClientLockoutPolicy,
    #[serde(default)]
    stereo: StereoConfig,
    #[serde(default)]
    esp32_audio: Esp32AudioConfig,
    #[serde(default)]
    processing: ProcessingConfig,
}

impl DesiredClientConfig {
    fn new(user_id: UserId) -> Self {
        Self {
            user_id,
            client_uid: None,
            role: ClientRole::Client,
            name: String::new(),
            listen: vec![CHANNEL_OPEN],
            tx: vec![CHANNEL_OPEN],
            vol: HashMap::new(),
            talker_vol: HashMap::new(),
            codec: Codec::Pcm16,
            opus_profile: OpusProfile::default(),
            talk_mode: TalkMode::Ptt,
            priority: false,
            priority_channels: Vec::new(),
            buttons: Vec::new(),
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: Esp32AudioConfig::default(),
            processing: ProcessingConfig::default(),
        }
    }
}

const CHANNEL_OPEN: ChannelId = 0;
const CHANNEL_PROGRAM: ChannelId = 1;
const CHANNEL_PRODUCTION_PL: ChannelId = 2;
const CHANNEL_REFEREE_PL: ChannelId = 3;
const CHANNEL_DIRECTOR_IFB: ChannelId = 4;
const CHANNEL_PRODUCER_CUE: ChannelId = 5;
const CHANNEL_PA: ChannelId = 6;
const CHANNEL_UTILITY: ChannelId = 7;

const USER_DIRECTOR: UserId = 1;
const USER_PRODUCER: UserId = 2;
const USER_TALENT: UserId = 10;
const USER_REF_1: UserId = 20;
const USER_REF_2: UserId = 21;
const USER_PROGRAM_BRIDGE: UserId = 90;
const USER_PA_BRIDGE: UserId = 91;

fn default_workflow_channels() -> Vec<ChannelConfig> {
    vec![
        channel_config(CHANNEL_OPEN, "open"),
        channel_config(CHANNEL_PROGRAM, "Program"),
        channel_config(CHANNEL_PRODUCTION_PL, "Production PL"),
        channel_config(CHANNEL_REFEREE_PL, "Referee PL"),
        channel_config(CHANNEL_DIRECTOR_IFB, "Director IFB"),
        channel_config(CHANNEL_PRODUCER_CUE, "Producer Cue"),
        channel_config(CHANNEL_PA, "PA"),
        channel_config(CHANNEL_UTILITY, "Utility"),
    ]
}

fn channel_config(id: ChannelId, name: &str) -> ChannelConfig {
    ChannelConfig {
        id,
        name: name.to_string(),
    }
}

fn default_workflow_templates() -> Vec<ClientTemplateConfig> {
    vec![
        ClientTemplateConfig {
            id: "director-show-control".to_string(),
            name: "Director Show Control".to_string(),
            client: director_template_client(),
        },
        ClientTemplateConfig {
            id: "producer-cue".to_string(),
            name: "Producer Cue".to_string(),
            client: producer_template_client(),
        },
        ClientTemplateConfig {
            id: "talent-ifb-listen-only".to_string(),
            name: "Talent IFB Listen-Only".to_string(),
            client: talent_template_client(),
        },
        ClientTemplateConfig {
            id: "referee-field".to_string(),
            name: "Referee Field".to_string(),
            client: referee_template_client(),
        },
        ClientTemplateConfig {
            id: "program-bridge-input".to_string(),
            name: "Program Bridge Input".to_string(),
            client: program_bridge_template_client(),
        },
        ClientTemplateConfig {
            id: "pa-bridge-output".to_string(),
            name: "PA Bridge Output".to_string(),
            client: pa_bridge_template_client(),
        },
    ]
}

fn default_workflow_presets() -> Vec<PresetConfig> {
    vec![PresetConfig {
        id: "small-show-ifb".to_string(),
        name: "Small Show IFB".to_string(),
        clients: vec![
            desired_from_template(USER_DIRECTOR, "Director", director_template_client()),
            desired_from_template(USER_PRODUCER, "Producer", producer_template_client()),
            desired_from_template(USER_TALENT, "Talent", talent_template_client()),
            desired_from_template(USER_REF_1, "Ref 1", referee_template_client()),
            desired_from_template(USER_REF_2, "Ref 2", referee_template_client()),
            desired_from_template(
                USER_PROGRAM_BRIDGE,
                "Program Bridge",
                program_bridge_template_client(),
            ),
            desired_from_template(USER_PA_BRIDGE, "PA Bridge", pa_bridge_template_client()),
        ],
    }]
}

fn desired_from_template(
    user_id: UserId,
    name: &str,
    mut client: ClientTemplateClientConfig,
) -> DesiredClientConfig {
    client.name = name.to_string();
    client.to_desired(user_id)
}

fn director_template_client() -> ClientTemplateClientConfig {
    ClientTemplateClientConfig {
        name: "Director".to_string(),
        listen: vec![
            CHANNEL_PROGRAM,
            CHANNEL_PRODUCTION_PL,
            CHANNEL_REFEREE_PL,
            CHANNEL_PRODUCER_CUE,
        ],
        tx: vec![CHANNEL_PRODUCTION_PL],
        vol: [
            (CHANNEL_PROGRAM, 0.55),
            (CHANNEL_PRODUCTION_PL, 1.0),
            (CHANNEL_REFEREE_PL, 0.8),
            (CHANNEL_PRODUCER_CUE, 0.8),
        ]
        .into(),
        codec: Codec::Pcm48,
        talk_mode: TalkMode::Ptt,
        priority_channels: vec![CHANNEL_PRODUCTION_PL, CHANNEL_DIRECTOR_IFB],
        buttons: vec![
            transmit_button(
                "talent",
                "Talent",
                vec![CHANNEL_DIRECTOR_IFB],
                Vec::new(),
                true,
            ),
            transmit_button(
                "producer",
                "Producer",
                vec![CHANNEL_PRODUCER_CUE],
                Vec::new(),
                false,
            ),
            transmit_button("pa", "PA", vec![CHANNEL_PA], Vec::new(), false),
        ],
        lockout: show_control_lockout(),
        ..ClientTemplateClientConfig::default()
    }
}

fn producer_template_client() -> ClientTemplateClientConfig {
    ClientTemplateClientConfig {
        name: "Producer".to_string(),
        listen: vec![CHANNEL_PROGRAM, CHANNEL_PRODUCTION_PL, CHANNEL_PRODUCER_CUE],
        tx: vec![CHANNEL_PRODUCTION_PL],
        vol: [
            (CHANNEL_PROGRAM, 0.5),
            (CHANNEL_PRODUCTION_PL, 1.0),
            (CHANNEL_PRODUCER_CUE, 0.9),
        ]
        .into(),
        codec: Codec::Pcm48,
        talk_mode: TalkMode::Ptt,
        buttons: vec![
            transmit_button("cue", "Cue", vec![CHANNEL_PRODUCER_CUE], Vec::new(), true),
            transmit_button(
                "director",
                "Director",
                Vec::new(),
                vec![USER_DIRECTOR],
                false,
            ),
            TalkButtonConfig {
                id: "alert-talent".to_string(),
                label: "Alert Talent".to_string(),
                color: None,
                mode: TalkButtonMode::Momentary,
                actions: vec![TalkButtonAction::Alert {
                    targets: vec![AlertTarget::User(USER_TALENT)],
                    message: Some("Producer cue".to_string()),
                }],
            },
        ],
        lockout: show_control_lockout(),
        ..ClientTemplateClientConfig::default()
    }
}

fn talent_template_client() -> ClientTemplateClientConfig {
    ClientTemplateClientConfig {
        name: "Talent".to_string(),
        listen: vec![CHANNEL_PROGRAM, CHANNEL_DIRECTOR_IFB, CHANNEL_PRODUCER_CUE],
        tx: Vec::new(),
        vol: [
            (CHANNEL_PROGRAM, 1.0),
            (CHANNEL_DIRECTOR_IFB, 1.0),
            (CHANNEL_PRODUCER_CUE, 0.9),
        ]
        .into(),
        codec: Codec::Pcm48,
        talk_mode: TalkMode::Muted,
        ifb: IfbConfig {
            enabled: true,
            program: vec![CHANNEL_PROGRAM],
            interrupt: vec![CHANNEL_DIRECTOR_IFB, CHANNEL_PRODUCER_CUE],
            duck_gain: DEFAULT_IFB_DUCK_GAIN,
        },
        stereo: StereoConfig {
            enabled: true,
            channel_pan: [
                (CHANNEL_PROGRAM, -0.6),
                (CHANNEL_DIRECTOR_IFB, 0.7),
                (CHANNEL_PRODUCER_CUE, 0.7),
            ]
            .into(),
        },
        lockout: listen_only_lockout(),
        ..ClientTemplateClientConfig::default()
    }
}

fn referee_template_client() -> ClientTemplateClientConfig {
    ClientTemplateClientConfig {
        name: "Ref".to_string(),
        listen: vec![CHANNEL_REFEREE_PL, CHANNEL_DIRECTOR_IFB],
        tx: vec![CHANNEL_REFEREE_PL],
        codec: Codec::Pcm48,
        talk_mode: TalkMode::Ptt,
        buttons: vec![
            transmit_button(
                "director",
                "Director",
                Vec::new(),
                vec![USER_DIRECTOR],
                false,
            ),
            transmit_button("pa", "PA", vec![CHANNEL_PA], Vec::new(), false),
            TalkButtonConfig {
                id: "alert-director".to_string(),
                label: "Alert Director".to_string(),
                color: None,
                mode: TalkButtonMode::Momentary,
                actions: vec![TalkButtonAction::Alert {
                    targets: vec![AlertTarget::User(USER_DIRECTOR)],
                    message: Some("Referee calling".to_string()),
                }],
            },
        ],
        lockout: route_locked_lockout(),
        ..ClientTemplateClientConfig::default()
    }
}

fn program_bridge_template_client() -> ClientTemplateClientConfig {
    ClientTemplateClientConfig {
        role: ClientRole::Bridge,
        name: "Program Bridge".to_string(),
        listen: Vec::new(),
        tx: vec![CHANNEL_PROGRAM],
        codec: Codec::Pcm48,
        talk_mode: TalkMode::Open,
        lockout: fully_locked_lockout(),
        ..ClientTemplateClientConfig::default()
    }
}

fn pa_bridge_template_client() -> ClientTemplateClientConfig {
    ClientTemplateClientConfig {
        role: ClientRole::Bridge,
        name: "PA Bridge".to_string(),
        listen: vec![CHANNEL_PA],
        tx: Vec::new(),
        codec: Codec::Pcm48,
        talk_mode: TalkMode::Muted,
        lockout: fully_locked_lockout(),
        ..ClientTemplateClientConfig::default()
    }
}

fn transmit_button(
    id: &str,
    label: &str,
    channels: Vec<ChannelId>,
    users: Vec<UserId>,
    duck: bool,
) -> TalkButtonConfig {
    TalkButtonConfig {
        id: id.to_string(),
        label: label.to_string(),
        color: None,
        mode: TalkButtonMode::Momentary,
        actions: vec![TalkButtonAction::Transmit {
            channels,
            users,
            duck,
        }],
    }
}

fn show_control_lockout() -> ClientLockoutPolicy {
    ClientLockoutPolicy {
        allow_channels: false,
        allow_volumes: true,
        allow_codec: false,
        allow_talk_mode: true,
        allow_priority: false,
        allow_buttons: false,
        allow_ifb: false,
        allow_device_selection: true,
        allow_local_api: false,
    }
}

fn route_locked_lockout() -> ClientLockoutPolicy {
    ClientLockoutPolicy {
        allow_channels: false,
        allow_volumes: true,
        allow_codec: false,
        allow_talk_mode: true,
        allow_priority: false,
        allow_buttons: false,
        allow_ifb: false,
        allow_device_selection: true,
        allow_local_api: false,
    }
}

fn listen_only_lockout() -> ClientLockoutPolicy {
    ClientLockoutPolicy {
        allow_channels: false,
        allow_volumes: true,
        allow_codec: false,
        allow_talk_mode: false,
        allow_priority: false,
        allow_buttons: false,
        allow_ifb: false,
        allow_device_selection: true,
        allow_local_api: false,
    }
}

fn fully_locked_lockout() -> ClientLockoutPolicy {
    ClientLockoutPolicy {
        allow_channels: false,
        allow_volumes: false,
        allow_codec: false,
        allow_talk_mode: false,
        allow_priority: false,
        allow_buttons: false,
        allow_ifb: false,
        allow_device_selection: false,
        allow_local_api: false,
    }
}

fn default_codec() -> Codec {
    Codec::Pcm16
}

fn default_true() -> bool {
    true
}

fn default_tts_gain() -> f32 {
    TTS_DEFAULT_GAIN
}

#[derive(Debug, Serialize)]
struct AdminStateResponse {
    build: common::BuildInfo,
    sessions: Vec<SessionStatus>,
    metrics: StatusMetrics,
    enrollment_policy: EnrollmentPolicy,
    recording: RecordingStatusResponse,
    transcription: LiveTranscriptionStatusResponse,
    deepfilternet: DeepFilterNetStatusResponse,
    channels: Vec<ChannelConfig>,
    devices: Vec<DeviceEnrollment>,
    clients: Vec<DesiredClientConfig>,
    presets: Vec<PresetConfig>,
    templates: Vec<ClientTemplateConfig>,
    active_alerts: Vec<AlertStatus>,
    recent_alerts: Vec<AlertStatus>,
    emergency: Option<EmergencyStatus>,
    warnings: Vec<AdminWarning>,
}

#[derive(Debug, Clone, Serialize)]
struct RecordingStatusResponse {
    active: bool,
    session_id: Option<String>,
    session_dir: Option<String>,
    started_at_ms: Option<u64>,
    transcribe: bool,
    recorded_users: Vec<UserId>,
    frames_recorded: u64,
    transcript_segments: usize,
    engine: TranscriptionEngineStatus,
    recent_sessions: Vec<RecordingSessionSummary>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct TranscriptionEngineStatus {
    available: bool,
    mode: TranscriptionEngineMode,
    acceleration: WhisperAccelerationStatus,
    command: Option<String>,
    model: Option<String>,
    model_dir: Option<String>,
    models: Vec<WhisperModelInfo>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct WhisperModelInfo {
    name: String,
    path: String,
    selected: bool,
}

#[derive(Debug, Clone, Serialize)]
struct DeepFilterNetStatusResponse {
    backend_available: bool,
    supported_backends: Vec<String>,
    preferred_backend: String,
    apple_compute_units: Vec<String>,
    model_dir: String,
    models: Vec<DeepFilterNetModelInfo>,
    coreml_packages: Vec<DeepFilterNetCoreMlPackageInfo>,
    coreml_compiled: bool,
    detail: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct WhisperAccelerationStatus {
    active_backend: String,
    metal_compiled: bool,
    coreml_compiled: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DeepFilterNetModelInfo {
    name: String,
    path: String,
    runtime: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DeepFilterNetCoreMlPackageInfo {
    name: String,
    path: String,
    complete: bool,
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum TranscriptSource {
    Live,
    Recording,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct TranscriptSegment {
    id: u64,
    user_id: UserId,
    #[serde(default)]
    user_name: String,
    started_at_ms: u64,
    ended_at_ms: u64,
    #[serde(default)]
    contexts: Vec<AudioTarget>,
    text: String,
    confidence: Option<f32>,
    engine: String,
    #[serde(default = "default_transcript_source")]
    source: TranscriptSource,
    #[serde(default = "default_final_transcript_segment")]
    final_segment: bool,
}

fn default_transcript_source() -> TranscriptSource {
    TranscriptSource::Recording
}

fn default_final_transcript_segment() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct StartRecordingRequest {
    #[serde(default)]
    transcribe: bool,
    #[serde(default)]
    users: Option<Vec<UserId>>,
}

#[derive(Debug, Default, Deserialize)]
struct TranscriptQuery {
    user_id: Option<UserId>,
    channel_id: Option<ChannelId>,
    user_ids: Option<String>,
    channel_ids: Option<String>,
    direct_user_id: Option<UserId>,
    source: Option<TranscriptSource>,
    since_ms: Option<u64>,
    until_ms: Option<u64>,
    q: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AddTranscriptRequest {
    user_id: UserId,
    text: String,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    contexts: Vec<AudioTarget>,
}

struct TranscriptAppend {
    user_id: UserId,
    contexts: Vec<AudioTarget>,
    started_at_ms: Option<u64>,
    ended_at_ms: Option<u64>,
    text: String,
    confidence: Option<f32>,
    engine: String,
    source: TranscriptSource,
    final_segment: bool,
}

struct RecordingState {
    base_dir: PathBuf,
    whisper_command: Option<PathBuf>,
    whisper_model: Option<PathBuf>,
    active: Option<ActiveRecordingSession>,
    recent: Vec<RecordingSessionSummary>,
    transcripts: Vec<TranscriptSegment>,
    next_transcript_id: u64,
    last_engine_error: Option<String>,
}

struct LiveTranscriptionState {
    engine_mode: TranscriptionEngineMode,
    whisper_command: Option<PathBuf>,
    whisper_model: Option<PathBuf>,
    whisper_model_dir: PathBuf,
    #[cfg(feature = "transcription-whisper")]
    whisper_context: Option<Arc<WhisperContext>>,
    active: bool,
    started_at_ms: Option<u64>,
    users: Option<HashSet<UserId>>,
    per_user: HashMap<UserId, LiveTranscriptionUserRuntime>,
    last_error: Option<String>,
}

impl Default for LiveTranscriptionState {
    fn default() -> Self {
        Self {
            engine_mode: TranscriptionEngineMode::Disabled,
            whisper_command: None,
            whisper_model: None,
            whisper_model_dir: PathBuf::from("intercom-models"),
            #[cfg(feature = "transcription-whisper")]
            whisper_context: None,
            active: false,
            started_at_ms: None,
            users: None,
            per_user: HashMap::new(),
            last_error: None,
        }
    }
}

#[derive(Default)]
struct LiveTranscriptionUserRuntime {
    chunker: LiveTranscriptChunker,
    pending: VecDeque<LiveTranscriptJob>,
    worker_running: bool,
    queued_jobs: usize,
    dropped_jobs: u64,
    dropped_frames: u64,
    completed_segments: u64,
    last_contexts: Vec<AudioTarget>,
}

#[derive(Debug, Clone, Default)]
struct LiveTranscriptChunker {
    buffer: Vec<i16>,
    contexts: Vec<AudioTarget>,
    started_at_ms: Option<u64>,
    voiced_frames: usize,
    silence_frames: usize,
    total_frames: usize,
}

#[cfg_attr(not(feature = "transcription-whisper"), allow(dead_code))]
#[derive(Debug, Clone)]
struct LiveTranscriptJob {
    user_id: UserId,
    started_at_ms: u64,
    ended_at_ms: u64,
    contexts: Vec<AudioTarget>,
    samples_16khz: Vec<i16>,
}

#[derive(Debug, Clone, Serialize)]
struct LiveTranscriptionStatusResponse {
    active: bool,
    available: bool,
    engine: TranscriptionEngineMode,
    acceleration: WhisperAccelerationStatus,
    model: Option<String>,
    model_dir: String,
    models: Vec<WhisperModelInfo>,
    command: Option<String>,
    started_at_ms: Option<u64>,
    users: Vec<LiveTranscriptionUserStatus>,
    queued_jobs: usize,
    dropped_jobs: u64,
    dropped_frames: u64,
    completed_segments: u64,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct LiveTranscriptionUserStatus {
    user_id: UserId,
    queued_jobs: usize,
    dropped_jobs: u64,
    dropped_frames: u64,
    completed_segments: u64,
    active_chunk: bool,
    worker_running: bool,
    contexts: Vec<AudioTarget>,
}

#[derive(Debug, Deserialize)]
struct StartLiveTranscriptionRequest {
    #[serde(default)]
    users: Option<Vec<UserId>>,
}

#[derive(Debug, Deserialize)]
struct SelectWhisperModelRequest {
    model: String,
}

impl Default for RecordingState {
    fn default() -> Self {
        Self {
            base_dir: PathBuf::from("intercom-recordings"),
            whisper_command: None,
            whisper_model: None,
            active: None,
            recent: Vec::new(),
            transcripts: Vec::new(),
            next_transcript_id: 1,
            last_engine_error: None,
        }
    }
}

struct ActiveRecordingSession {
    id: String,
    dir: PathBuf,
    started_at_ms: u64,
    transcribe: bool,
    users: Option<HashSet<UserId>>,
    writers: HashMap<UserId, hound::WavWriter<BufWriter<File>>>,
    contexts: HashMap<UserId, Vec<AudioTarget>>,
    metadata_writer: BufWriter<File>,
    frames_recorded: u64,
}

#[derive(Debug, Clone, Serialize)]
struct RecordingSessionSummary {
    id: String,
    dir: String,
    started_at_ms: u64,
    stopped_at_ms: Option<u64>,
    transcribe: bool,
    recorded_users: Vec<UserId>,
    frames_recorded: u64,
}

#[derive(Debug, Serialize)]
struct RecordingMetadataEvent {
    kind: &'static str,
    timestamp_ms: u64,
    session_id: String,
    frame_index: u64,
    user_id: UserId,
    user_name: String,
    target: AudioTarget,
    codec: Codec,
    talk_mode: TalkMode,
    peak: f32,
    rms: f32,
}

#[derive(Debug, Clone)]
struct StoppedRecordingForTranscription {
    dir: PathBuf,
    users: Vec<UserId>,
}

#[derive(Debug, Serialize)]
struct AdminWarning {
    user_id: UserId,
    message: String,
}

#[derive(Debug, Deserialize)]
struct DevicePatch {
    #[serde(default)]
    user_id: Option<UserId>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<EnrollmentStatus>,
}

#[derive(Debug, Deserialize)]
struct ClientConfigPut {
    #[serde(default)]
    client_uid: Option<ClientUid>,
    #[serde(default)]
    role: ClientRole,
    #[serde(default)]
    name: String,
    listen: Vec<ChannelId>,
    tx: Vec<ChannelId>,
    #[serde(default)]
    vol: HashMap<ChannelId, f32>,
    #[serde(default, with = "common::user_gain_map")]
    talker_vol: HashMap<UserId, f32>,
    codec: Codec,
    #[serde(default)]
    opus_profile: OpusProfile,
    talk_mode: TalkMode,
    priority: bool,
    #[serde(default)]
    priority_channels: Vec<ChannelId>,
    #[serde(default)]
    buttons: Vec<TalkButtonConfig>,
    #[serde(default)]
    ifb: IfbConfig,
    #[serde(default)]
    lockout: ClientLockoutPolicy,
    #[serde(default)]
    stereo: StereoConfig,
    #[serde(default)]
    esp32_audio: Esp32AudioConfig,
    #[serde(default)]
    processing: ProcessingConfig,
}

#[derive(Debug, Default, Deserialize)]
struct ClientConfigPatch {
    client_uid: Option<ClientUid>,
    role: Option<ClientRole>,
    name: Option<String>,
    listen: Option<Vec<ChannelId>>,
    tx: Option<Vec<ChannelId>>,
    vol: Option<HashMap<ChannelId, f32>>,
    #[serde(default, with = "common::optional_user_gain_map")]
    talker_vol: Option<HashMap<UserId, f32>>,
    codec: Option<Codec>,
    opus_profile: Option<OpusProfile>,
    talk_mode: Option<TalkMode>,
    priority: Option<bool>,
    priority_channels: Option<Vec<ChannelId>>,
    buttons: Option<Vec<TalkButtonConfig>>,
    ifb: Option<IfbConfig>,
    lockout: Option<ClientLockoutPolicy>,
    stereo: Option<StereoConfig>,
    esp32_audio: Option<Esp32AudioConfig>,
    processing: Option<ProcessingConfig>,
}

#[derive(Debug, Deserialize)]
struct ChannelPut {
    name: String,
}

#[derive(Debug, Deserialize)]
struct PresetPut {
    name: String,
    #[serde(default)]
    clients: Vec<DesiredClientConfig>,
}

#[derive(Debug, Deserialize)]
struct ClientTemplatePut {
    name: String,
    #[serde(default)]
    client: ClientTemplateClientConfig,
}

#[derive(Debug, Deserialize)]
struct ApplyTemplateRequest {
    user_id: UserId,
}

#[derive(Debug, Deserialize)]
struct DirectCallRequest {
    caller: UserId,
    target: UserId,
    active: bool,
    #[serde(default)]
    duck: bool,
}

#[derive(Debug, Deserialize)]
struct EmergencyRequest {
    source: UserId,
    active: bool,
    #[serde(default)]
    target: EmergencyTarget,
    #[serde(default = "common::default_ifb_duck_gain")]
    duck_gain: f32,
    #[serde(default)]
    mute_others: bool,
}

#[derive(Debug, Deserialize)]
struct AdminAlertRequest {
    sender: UserId,
    target: AlertTarget,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AdminTtsRequest {
    #[serde(default)]
    sender: UserId,
    #[serde(default)]
    targets: Vec<AlertTarget>,
    #[serde(default)]
    target: Option<AlertTarget>,
    message: String,
    #[serde(default = "default_true")]
    priority: bool,
    #[serde(default)]
    duck: bool,
    #[serde(default = "default_tts_gain")]
    gain: f32,
}

#[derive(Debug, Deserialize)]
struct AdminAnnouncementRequest {
    #[serde(default)]
    sender: UserId,
    #[serde(default)]
    targets: Vec<AlertTarget>,
    #[serde(default)]
    target: Option<AlertTarget>,
    message: String,
    #[serde(default)]
    text_alert: bool,
    #[serde(default)]
    tts: bool,
    #[serde(default = "default_true")]
    priority: bool,
    #[serde(default)]
    duck: bool,
    #[serde(default = "default_tts_gain")]
    gain: f32,
}

#[derive(Debug, Clone, Serialize)]
struct TtsAnnouncementResponse {
    ok: bool,
    id: u64,
    sender: UserId,
    targets: Vec<AlertTarget>,
    recipients: Vec<UserId>,
    message: String,
    engine: Option<&'static str>,
    duration_ms: Option<u64>,
    alert_ids: Vec<AlertId>,
}

#[derive(Debug, Clone)]
struct TtsSynthesis {
    engine: &'static str,
    frames: Vec<Vec<i16>>,
}

#[derive(Debug, Deserialize)]
struct CancelAlertRequest {
    user_id: Option<UserId>,
}

#[derive(Debug, Clone)]
struct SourceQueue {
    frames: VecDeque<QueuedFrame>,
    last_seen: Instant,
}

impl SourceQueue {
    fn new() -> Self {
        Self {
            frames: VecDeque::with_capacity(MAX_SOURCE_QUEUE_FRAMES),
            last_seen: Instant::now(),
        }
    }

    fn push(&mut self, target: AudioTarget, samples: Vec<i16>) -> bool {
        self.push_with_options(target, samples, false, false)
    }

    fn push_with_options(
        &mut self,
        target: AudioTarget,
        samples: Vec<i16>,
        priority: bool,
        duck: bool,
    ) -> bool {
        self.last_seen = Instant::now();

        let dropped = if self.frames.len() == MAX_SOURCE_QUEUE_FRAMES {
            self.frames.pop_front();
            tracing::warn!(?target, "dropped oldest source frame because queue is full");
            true
        } else {
            false
        };

        self.frames.push_back(QueuedFrame {
            target,
            samples,
            priority,
            duck,
        });
        dropped
    }

    fn pop(&mut self) -> Option<QueuedFrame> {
        self.frames.pop_front()
    }
}

#[derive(Debug, Clone)]
struct QueuedFrame {
    target: AudioTarget,
    samples: Vec<i16>,
    priority: bool,
    duck: bool,
}

#[derive(Debug, Clone)]
struct VirtualAudioSource {
    user_id: UserId,
    target: AudioTarget,
    frames: Vec<Vec<i16>>,
    frame_index: usize,
    priority: bool,
    duck: bool,
}

impl VirtualAudioSource {
    fn new(
        user_id: UserId,
        target: AudioTarget,
        frames: Vec<Vec<i16>>,
        priority: bool,
        duck: bool,
    ) -> Self {
        Self {
            user_id,
            target,
            frames,
            frame_index: 0,
            priority,
            duck,
        }
    }

    fn next_frame(&mut self) -> Option<ActiveSourceFrame> {
        let samples = self.frames.get(self.frame_index)?.clone();
        self.frame_index += 1;
        Some(ActiveSourceFrame {
            user_id: self.user_id,
            target: self.target,
            samples,
            priority: self.priority,
            duck: self.duck,
        })
    }

    fn is_finished(&self) -> bool {
        self.frame_index >= self.frames.len()
    }
}

pub async fn run(
    audio_socket: Arc<UdpSocket>,
    control_listener: TcpListener,
) -> anyhow::Result<()> {
    run_with_options(audio_socket, control_listener, RunOptions::default()).await
}

#[derive(Debug, Clone)]
pub struct ServerRuntimeConfig {
    pub audio_bind: SocketAddr,
    pub control_bind: SocketAddr,
    pub admin_bind: Option<SocketAddr>,
    pub admin_state_file: Option<PathBuf>,
    pub admin_auth: HttpAuthConfig,
    pub enrollment_policy: EnrollmentPolicy,
    pub advertise_name: Option<String>,
    pub disable_discovery: bool,
    pub recordings_dir: PathBuf,
    pub debug_audio_dir: Option<PathBuf>,
    pub whisper_command: Option<PathBuf>,
    pub whisper_model: Option<PathBuf>,
    pub whisper_model_dir: PathBuf,
    pub deepfilternet_model_dir: PathBuf,
    pub transcription_engine: TranscriptionEngineMode,
}

impl Default for ServerRuntimeConfig {
    fn default() -> Self {
        Self {
            audio_bind: SocketAddr::from(([0, 0, 0, 0], 40000)),
            control_bind: SocketAddr::from(([0, 0, 0, 0], 40001)),
            admin_bind: Some(SocketAddr::from(([0, 0, 0, 0], 40002))),
            admin_state_file: Some(PathBuf::from("intercom-state.json")),
            admin_auth: HttpAuthConfig::disabled(),
            enrollment_policy: EnrollmentPolicy::Auto,
            advertise_name: None,
            disable_discovery: false,
            recordings_dir: PathBuf::from("intercom-recordings"),
            debug_audio_dir: None,
            whisper_command: None,
            whisper_model: None,
            whisper_model_dir: PathBuf::from("intercom-models"),
            deepfilternet_model_dir: PathBuf::from("deepfilternet-models"),
            transcription_engine: TranscriptionEngineMode::Disabled,
        }
    }
}

impl ServerRuntimeConfig {
    fn run_options(&self, admin_listener: Option<TcpListener>) -> RunOptions {
        RunOptions {
            admin_listener,
            admin_state_file: self.admin_state_file.clone(),
            admin_auth: self.admin_auth.clone(),
            enrollment_policy: self.enrollment_policy,
            recordings_dir: self.recordings_dir.clone(),
            debug_audio_dir: self.debug_audio_dir.clone(),
            whisper_command: self.whisper_command.clone(),
            whisper_model: self.whisper_model.clone(),
            whisper_model_dir: self.whisper_model_dir.clone(),
            deepfilternet_model_dir: self.deepfilternet_model_dir.clone(),
            transcription_engine: self.transcription_engine,
        }
    }
}

#[derive(Default)]
pub struct RunOptions {
    pub admin_listener: Option<TcpListener>,
    pub admin_state_file: Option<PathBuf>,
    pub admin_auth: HttpAuthConfig,
    pub enrollment_policy: EnrollmentPolicy,
    pub recordings_dir: PathBuf,
    pub debug_audio_dir: Option<PathBuf>,
    pub whisper_command: Option<PathBuf>,
    pub whisper_model: Option<PathBuf>,
    pub whisper_model_dir: PathBuf,
    pub deepfilternet_model_dir: PathBuf,
    pub transcription_engine: TranscriptionEngineMode,
}

pub struct ServerRuntimeHandle {
    pub audio_addr: SocketAddr,
    pub control_addr: SocketAddr,
    pub admin_addr: Option<SocketAddr>,
    _discovery_handle: Option<DiscoveryAdvertisementHandle>,
    tasks: ServerRuntimeTasks,
}

struct ServerRuntimeTasks {
    receiver: JoinHandle<anyhow::Result<()>>,
    mixer: JoinHandle<anyhow::Result<()>>,
    presence: JoinHandle<anyhow::Result<()>>,
    control: JoinHandle<anyhow::Result<()>>,
    admin: Option<JoinHandle<anyhow::Result<()>>>,
}

impl ServerRuntimeTasks {
    fn abort(&self) {
        self.receiver.abort();
        self.mixer.abort();
        self.presence.abort();
        self.control.abort();
        if let Some(admin) = &self.admin {
            admin.abort();
        }
    }
}

impl ServerRuntimeHandle {
    pub async fn wait(&mut self) -> anyhow::Result<()> {
        let tasks = &mut self.tasks;
        tokio::select! {
            result = &mut tasks.receiver => result.context("audio receiver task panicked")??,
            result = &mut tasks.mixer => result.context("audio mixer task panicked")??,
            result = &mut tasks.presence => result.context("presence task panicked")??,
            result = &mut tasks.control => result.context("control task panicked")??,
            result = wait_for_optional_task(tasks.admin.as_mut()) => result.context("admin task panicked")??,
        }

        Ok(())
    }

    pub fn shutdown(&self) {
        self.tasks.abort();
    }
}

impl Drop for ServerRuntimeHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub async fn start_runtime(config: ServerRuntimeConfig) -> anyhow::Result<ServerRuntimeHandle> {
    let audio_socket = Arc::new(
        UdpSocket::bind(config.audio_bind)
            .await
            .with_context(|| format!("bind UDP audio socket at {}", config.audio_bind))?,
    );
    let control_listener = TcpListener::bind(config.control_bind)
        .await
        .with_context(|| format!("bind WebSocket control listener at {}", config.control_bind))?;
    let admin_listener = match config.admin_bind {
        Some(admin_bind) => Some(
            TcpListener::bind(admin_bind)
                .await
                .with_context(|| format!("bind admin HTTP listener at {}", admin_bind))?,
        ),
        None => None,
    };

    let actual_audio_addr = audio_socket.local_addr()?;
    let actual_control_addr = control_listener.local_addr()?;
    let actual_admin_addr = admin_listener
        .as_ref()
        .map(|listener| listener.local_addr())
        .transpose()?;
    let discovery_handle = if config.disable_discovery {
        None
    } else {
        let advertisement = DiscoveryAdvertisement {
            name: config
                .advertise_name
                .clone()
                .unwrap_or_else(default_discovery_name),
            control_port: actual_control_addr.port(),
            audio_port: actual_audio_addr.port(),
            admin_port: discovery_admin_port(actual_admin_addr),
            auth_required: config.admin_auth.is_enabled(),
            version: common::current_build_info().version,
        };
        match start_discovery_advertisement(&advertisement) {
            Ok(handle) => {
                tracing::info!(
                    name = %advertisement.name,
                    service = DISCOVERY_SERVICE_TYPE,
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

    start_runtime_from_bound(
        audio_socket,
        control_listener,
        config.run_options(admin_listener),
        actual_audio_addr,
        actual_control_addr,
        actual_admin_addr,
        discovery_handle,
    )
    .await
}

pub fn discovery_admin_port(admin_addr: Option<SocketAddr>) -> Option<u16> {
    admin_addr
        .filter(|addr| !addr.ip().is_loopback())
        .map(|addr| addr.port())
}

pub async fn run_with_options(
    audio_socket: Arc<UdpSocket>,
    control_listener: TcpListener,
    options: RunOptions,
) -> anyhow::Result<()> {
    let audio_addr = audio_socket.local_addr()?;
    let control_addr = control_listener.local_addr()?;
    let admin_addr = options
        .admin_listener
        .as_ref()
        .map(|listener| listener.local_addr())
        .transpose()?;
    let mut handle = start_runtime_from_bound(
        audio_socket,
        control_listener,
        options,
        audio_addr,
        control_addr,
        admin_addr,
        None,
    )
    .await?;
    handle.wait().await
}

async fn start_runtime_from_bound(
    audio_socket: Arc<UdpSocket>,
    control_listener: TcpListener,
    options: RunOptions,
    audio_addr: SocketAddr,
    control_addr: SocketAddr,
    admin_addr: Option<SocketAddr>,
    discovery_handle: Option<DiscoveryAdvertisementHandle>,
) -> anyhow::Result<ServerRuntimeHandle> {
    let admin_listener = options.admin_listener;
    let admin_auth = options.admin_auth;
    let whisper_command = options.whisper_command;
    let whisper_model = options.whisper_model;
    let whisper_model_dir = if options.whisper_model_dir.as_os_str().is_empty() {
        PathBuf::from("intercom-models")
    } else {
        options.whisper_model_dir
    };
    let deepfilternet_model_dir = if options.deepfilternet_model_dir.as_os_str().is_empty() {
        PathBuf::from("deepfilternet-models")
    } else {
        options.deepfilternet_model_dir
    };
    let state =
        Arc::new(ServerState::load(options.admin_state_file, options.enrollment_policy).await?);
    if let Some(debug_audio_dir) = options.debug_audio_dir {
        let debug_audio_tx = spawn_server_debug_audio_writer(debug_audio_dir)?;
        *state.debug_audio_tx.write().await = Some(debug_audio_tx);
    }
    {
        let mut recording = state.recording.write().await;
        recording.base_dir = options.recordings_dir;
        recording.whisper_command = whisper_command.clone();
        recording.whisper_model = whisper_model.clone();
    }
    {
        let mut transcription = state.transcription.write().await;
        transcription.engine_mode = options.transcription_engine;
        transcription.whisper_command = whisper_command;
        transcription.whisper_model = whisper_model;
        transcription.whisper_model_dir = whisper_model_dir;
    }
    *state.deepfilternet_model_dir.write().await = deepfilternet_model_dir;
    let receiver_state = Arc::clone(&state);
    let mixer_state = Arc::clone(&state);
    let control_state = Arc::clone(&state);
    let admin_state = Arc::clone(&state);
    let presence_state = Arc::clone(&state);
    let receiver_socket = Arc::clone(&audio_socket);
    let mixer_socket = Arc::clone(&audio_socket);
    let receiver_task =
        tokio::spawn(async move { run_audio_receiver(receiver_socket, receiver_state).await });
    let mixer_task = tokio::spawn(async move { run_audio_mixer(mixer_socket, mixer_state).await });
    let presence_task = tokio::spawn(async move { run_presence_updates(presence_state).await });
    let control_task =
        tokio::spawn(async move { run_control(control_listener, control_state).await });
    let admin_task = admin_listener.map(|listener| {
        tokio::spawn(async move { run_admin(listener, admin_state, admin_auth).await })
    });

    Ok(ServerRuntimeHandle {
        audio_addr,
        control_addr,
        admin_addr,
        _discovery_handle: discovery_handle,
        tasks: ServerRuntimeTasks {
            receiver: receiver_task,
            mixer: mixer_task,
            presence: presence_task,
            control: control_task,
            admin: admin_task,
        },
    })
}

async fn wait_for_optional_task(
    task: Option<&mut tokio::task::JoinHandle<anyhow::Result<()>>>,
) -> Result<anyhow::Result<()>, tokio::task::JoinError> {
    match task {
        Some(task) => task.await,
        None => future::pending().await,
    }
}

pub async fn run_audio_receiver(
    socket: Arc<UdpSocket>,
    state: Arc<ServerState>,
) -> anyhow::Result<()> {
    let mut buf = vec![0_u8; MAX_PACKET_BYTES];
    let mut decoders = AudioDecoderBank::default();
    let mut processors = AudioProcessorBank::default();

    loop {
        let (len, from) = socket.recv_from(&mut buf).await?;
        let packet = match AudioPacket::decode(&buf[..len]) {
            Ok(packet) => packet,
            Err(err) => {
                state
                    .metrics
                    .malformed_packets_dropped
                    .fetch_add(1, Ordering::Relaxed);
                tracing::warn!(%from, %err, "dropped malformed audio packet");
                continue;
            }
        };
        state
            .metrics
            .audio_packets_received
            .fetch_add(1, Ordering::Relaxed);

        if !audio_user_is_enrolled(&state, packet.user_id).await {
            tracing::warn!(
                %from,
                user_id = packet.user_id,
                "dropped audio packet for non-enrolled client"
            );
            continue;
        }

        if packet.is_registration() {
            register_audio_endpoint(&state, packet.user_id, packet.codec, from).await;
            continue;
        }

        let opus_profile = opus_profile_for_user(&state, packet.user_id).await;
        let mut samples =
            match decoders.decode(packet.user_id, packet.codec, opus_profile, &packet.payload) {
                Ok(samples) => samples,
                Err(err) => {
                    state
                        .metrics
                        .audio_decode_errors
                        .fetch_add(1, Ordering::Relaxed);
                    record_decode_error(&state, packet.user_id).await;
                    tracing::warn!(
                        %from,
                        user_id = packet.user_id,
                        target = ?packet.target,
                        codec = ?packet.codec,
                        %err,
                        "dropped invalid audio payload"
                    );
                    continue;
                }
            };
        state
            .metrics
            .audio_frames_decoded
            .fetch_add(1, Ordering::Relaxed);

        tap_debug_audio(
            &state,
            ServerDebugAudioKind::DecodedInput,
            packet.user_id,
            1,
            &samples,
        )
        .await;
        register_audio_source(&state, packet.user_id, packet.target, packet.codec, from).await;
        let processing = processing_config_for_user(&state, packet.user_id).await;
        let processing_status =
            processors.process(packet.user_id, packet.codec, &processing, &mut samples);
        record_processing_status(&state, packet.user_id, processing_status).await;
        record_input_health(&state, packet.user_id, packet.target, &samples).await;
        record_ingest_frame(
            &state,
            packet.user_id,
            packet.target,
            Some(packet.codec),
            &samples,
        )
        .await;
        transcribe_ingest_frame(&state, packet.user_id, packet.target, &samples).await;
        store_source_frame(&state, packet.user_id, packet.target, samples).await;
    }
}

pub async fn run_audio_mixer(
    socket: Arc<UdpSocket>,
    state: Arc<ServerState>,
) -> anyhow::Result<()> {
    let mut interval = tokio::time::interval(MIX_INTERVAL);
    let mut encoded = Vec::with_capacity(common::HEADER_LEN + PCM48_STEREO_PAYLOAD_BYTES);
    let mut encoders = AudioEncoderBank::default();

    loop {
        interval.tick().await;
        let outputs = build_mixes(&state).await;

        for output in outputs {
            let seq = next_output_seq(&state, output.user_id).await;
            tap_debug_audio(
                &state,
                ServerDebugAudioKind::MixedOutput,
                output.user_id,
                output.channels as u16,
                &output.samples,
            )
            .await;
            let packet = AudioPacket {
                user_id: SERVER_USER_ID,
                target: AudioTarget::Mixed,
                codec: output.codec,
                seq,
                timestamp: seq as u32
                    * if output.codec == Codec::Opus {
                        output.opus_profile.samples_per_frame() as u32
                    } else {
                        codec_samples_per_frame(output.codec) as u32
                    },
                payload: match encoders.encode(
                    output.user_id,
                    output.codec,
                    output.opus_profile,
                    &output.samples,
                    output.channels,
                ) {
                    Ok(payload) => payload,
                    Err(err) => {
                        state
                            .metrics
                            .audio_encode_errors
                            .fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            user_id = output.user_id,
                            codec = ?output.codec,
                            %err,
                            "failed to encode mixed audio"
                        );
                        continue;
                    }
                },
            };
            if let Err(err) = packet.encode(&mut encoded) {
                state
                    .metrics
                    .audio_encode_errors
                    .fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    user_id = output.user_id,
                    codec = ?output.codec,
                    %err,
                    "failed to encode mixed audio packet"
                );
                continue;
            }
            if let Err(err) = socket.send_to(&encoded, output.addr).await {
                record_audio_send_error(&state, output.user_id, output.addr, &err).await;
                continue;
            }
            record_output_health(&state, output.user_id, &output.meter, output.limiter_event).await;
            state
                .metrics
                .mixed_packets_sent
                .fetch_add(1, Ordering::Relaxed);
            tracing::trace!(
                user_id = output.user_id,
                %output.addr,
                active_sources = output.active_sources,
                bytes = encoded.len(),
                "sent mixed audio packet"
            );
        }
    }
}

async fn record_audio_send_error(
    state: &ServerState,
    user_id: UserId,
    addr: SocketAddr,
    err: &std::io::Error,
) {
    state
        .metrics
        .audio_send_errors
        .fetch_add(1, Ordering::Relaxed);

    let endpoint_cleared = clear_audio_endpoint_if_matches(state, user_id, addr).await;
    tracing::warn!(
        user_id,
        %addr,
        %err,
        endpoint_cleared,
        "failed to send mixed audio packet"
    );
}

pub async fn run_control(listener: TcpListener, state: Arc<ServerState>) -> anyhow::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let state = Arc::clone(&state);

        tokio::spawn(async move {
            match handle_control_connection(stream, state).await {
                Ok(()) => {}
                Err(err) if is_normal_control_disconnect(&err) => {
                    tracing::debug!(%peer, %err, "control connection closed");
                }
                Err(err) => {
                    tracing::warn!(%peer, %err, "control connection ended with error");
                }
            }
        });
    }
}

fn is_normal_control_disconnect(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<WsError>()
            .is_some_and(is_normal_control_websocket_disconnect)
            || cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(is_normal_control_io_disconnect)
    })
}

fn is_normal_control_websocket_disconnect(err: &WsError) -> bool {
    match err {
        WsError::ConnectionClosed | WsError::AlreadyClosed => true,
        WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake) => true,
        WsError::Io(err) => is_normal_control_io_disconnect(err),
        _ => false,
    }
}

fn is_normal_control_io_disconnect(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        ErrorKind::ConnectionReset | ErrorKind::BrokenPipe | ErrorKind::UnexpectedEof
    )
}

async fn run_presence_updates(state: Arc<ServerState>) -> anyhow::Result<()> {
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    loop {
        interval.tick().await;
        push_presence_updates(&state).await;
    }
}

pub async fn run_admin(
    listener: TcpListener,
    state: Arc<ServerState>,
    auth: HttpAuthConfig,
) -> anyhow::Result<()> {
    axum::serve(listener, admin_router(state, auth))
        .await
        .context("serve admin HTTP UI")
}

fn admin_router(state: Arc<ServerState>, auth: HttpAuthConfig) -> Router {
    Router::new()
        .route("/admin", get(admin_index))
        .route("/admin/", get(admin_index))
        .route("/admin/clients", get(admin_clients_index))
        .route("/admin/clients/", get(admin_clients_index))
        .route("/admin/routing", get(admin_routing_index))
        .route("/admin/routing/", get(admin_routing_index))
        .route("/admin/presets", get(admin_presets_index))
        .route("/admin/presets/", get(admin_presets_index))
        .route("/admin/calls", get(admin_calls_index))
        .route("/admin/calls/", get(admin_calls_index))
        .route("/admin/recording", get(admin_recording_index))
        .route("/admin/recording/", get(admin_recording_index))
        .route("/admin/system", get(admin_system_index))
        .route("/admin/system/", get(admin_system_index))
        .route("/admin/app.js", get(admin_js))
        .route("/admin/style.css", get(admin_css))
        .route("/admin/branding/redline-logo.png", get(admin_logo_png))
        .route("/admin/api/state", get(admin_state_handler))
        .route(
            "/admin/api/clients/:user_id",
            put(put_client_handler)
                .patch(patch_client_handler)
                .delete(delete_client_handler),
        )
        .route(
            "/admin/api/devices/:client_uid/approve",
            post(approve_device_handler),
        )
        .route(
            "/admin/api/devices/:client_uid/reject",
            post(reject_device_handler),
        )
        .route(
            "/admin/api/devices/:client_uid",
            put(patch_device_handler)
                .patch(patch_device_handler)
                .delete(delete_device_handler),
        )
        .route(
            "/admin/api/channels/:channel_id",
            put(put_channel_handler).delete(delete_channel_handler),
        )
        .route(
            "/admin/api/presets/:preset_id",
            put(put_preset_handler)
                .delete(delete_preset_handler)
                .post(apply_preset_handler),
        )
        .route(
            "/admin/api/templates/:template_id",
            put(put_template_handler).delete(delete_template_handler),
        )
        .route(
            "/admin/api/templates/:template_id/apply",
            post(apply_template_handler),
        )
        .route("/admin/api/direct-call", post(admin_direct_call_handler))
        .route("/admin/api/emergency", post(admin_emergency_handler))
        .route("/admin/api/alerts", post(admin_send_alert_handler))
        .route("/admin/api/announcements", post(admin_announcement_handler))
        .route("/admin/api/tts", post(admin_tts_handler))
        .route(
            "/admin/api/alerts/:alert_id/cancel",
            post(admin_cancel_alert_handler),
        )
        .route(
            "/admin/api/recording/start",
            post(admin_recording_start_handler),
        )
        .route(
            "/admin/api/recording/stop",
            post(admin_recording_stop_handler),
        )
        .route(
            "/admin/api/recording/status",
            get(admin_recording_status_handler),
        )
        .route(
            "/admin/api/recording/sessions",
            get(admin_recording_sessions_handler),
        )
        .route(
            "/admin/api/transcription/live/start",
            post(admin_live_transcription_start_handler),
        )
        .route(
            "/admin/api/transcription/live/stop",
            post(admin_live_transcription_stop_handler),
        )
        .route(
            "/admin/api/transcription/live/status",
            get(admin_live_transcription_status_handler),
        )
        .route(
            "/admin/api/transcription/models",
            get(admin_transcription_models_handler),
        )
        .route(
            "/admin/api/transcription/model",
            put(admin_select_transcription_model_handler),
        )
        .route(
            "/admin/api/processing/deepfilternet/models",
            get(admin_deepfilternet_models_handler),
        )
        .route("/admin/api/transcripts", get(admin_transcripts_handler))
        .route("/admin/api/transcripts", post(admin_add_transcript_handler))
        .layer(middleware::from_fn_with_state(auth, require_http_auth))
        .with_state(state)
}

async fn admin_index() -> Html<&'static str> {
    Html(ADMIN_DASHBOARD_HTML)
}

async fn admin_clients_index() -> Html<&'static str> {
    Html(ADMIN_CLIENTS_HTML)
}

async fn admin_routing_index() -> Html<&'static str> {
    Html(ADMIN_ROUTING_HTML)
}

async fn admin_presets_index() -> Html<&'static str> {
    Html(ADMIN_PRESETS_HTML)
}

async fn admin_calls_index() -> Html<&'static str> {
    Html(ADMIN_CALLS_HTML)
}

async fn admin_recording_index() -> Html<&'static str> {
    Html(ADMIN_RECORDING_HTML)
}

async fn admin_system_index() -> Html<&'static str> {
    Html(ADMIN_SYSTEM_HTML)
}

async fn admin_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/javascript")], ADMIN_JS)
}

async fn admin_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css")], ADMIN_CSS)
}

async fn admin_logo_png() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], ADMIN_LOGO_PNG)
}

async fn admin_state_handler(State(state): State<Arc<ServerState>>) -> Json<AdminStateResponse> {
    Json(admin_state_snapshot(&state).await)
}

async fn put_client_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(user_id): AxumPath<UserId>,
    Json(body): Json<ClientConfigPut>,
) -> Result<Json<DesiredClientConfig>, AdminApiError> {
    let desired = DesiredClientConfig {
        user_id,
        client_uid: body
            .client_uid
            .map(|uid| normalize_client_uid(&uid, Some(user_id))),
        role: body.role,
        name: body.name,
        listen: body.listen,
        tx: body.tx,
        vol: body.vol,
        talker_vol: body.talker_vol,
        codec: body.codec,
        opus_profile: body.opus_profile,
        talk_mode: body.talk_mode,
        priority: body.priority,
        priority_channels: sorted_unique_channels(body.priority_channels),
        buttons: normalize_button_configs(body.buttons),
        ifb: normalize_ifb_config(body.ifb),
        lockout: body.lockout,
        stereo: normalize_stereo_config(body.stereo),
        esp32_audio: normalize_esp32_audio_config(body.esp32_audio),
        processing: body.processing,
    };
    upsert_admin_client(&state, desired.clone()).await?;
    Ok(Json(
        desired_client(&state, user_id).await.unwrap_or(desired),
    ))
}

async fn patch_client_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(user_id): AxumPath<UserId>,
    Json(body): Json<ClientConfigPatch>,
) -> Result<Json<DesiredClientConfig>, AdminApiError> {
    let mut desired = desired_baseline_for_user(&state, user_id).await;
    if let Some(client_uid) = body.client_uid {
        desired.client_uid = Some(normalize_client_uid(&client_uid, Some(user_id)));
    }
    if let Some(role) = body.role {
        desired.role = role;
    }
    if let Some(name) = body.name {
        desired.name = name;
    }
    if let Some(listen) = body.listen {
        desired.listen = listen;
    }
    if let Some(tx) = body.tx {
        desired.tx = tx;
    }
    if let Some(vol) = body.vol {
        desired.vol = vol;
    }
    if let Some(talker_vol) = body.talker_vol {
        desired.talker_vol = talker_vol;
    }
    if let Some(codec) = body.codec {
        desired.codec = codec;
    }
    if let Some(opus_profile) = body.opus_profile {
        desired.opus_profile = opus_profile;
    }
    if let Some(talk_mode) = body.talk_mode {
        desired.talk_mode = talk_mode;
    }
    if let Some(priority) = body.priority {
        desired.priority = priority;
    }
    if let Some(priority_channels) = body.priority_channels {
        desired.priority_channels = sorted_unique_channels(priority_channels);
    }
    if let Some(buttons) = body.buttons {
        desired.buttons = normalize_button_configs(buttons);
    }
    if let Some(ifb) = body.ifb {
        desired.ifb = normalize_ifb_config(ifb);
    }
    if let Some(lockout) = body.lockout {
        desired.lockout = lockout;
    }
    if let Some(stereo) = body.stereo {
        desired.stereo = normalize_stereo_config(stereo);
    }
    if let Some(esp32_audio) = body.esp32_audio {
        desired.esp32_audio = normalize_esp32_audio_config(esp32_audio);
    }
    if let Some(processing) = body.processing {
        desired.processing = processing;
    }

    upsert_admin_client(&state, desired.clone()).await?;
    Ok(Json(
        desired_client(&state, user_id).await.unwrap_or(desired),
    ))
}

async fn delete_client_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(user_id): AxumPath<UserId>,
) -> Result<Json<OkResponse>, AdminApiError> {
    let admin_snapshot = {
        let mut admin_state = state.admin_state.write().await;
        admin_state
            .clients
            .retain(|client| client.user_id != user_id);
        admin_state.clone()
    };
    save_admin_state(&state, &admin_snapshot).await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn approve_device_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(client_uid): AxumPath<String>,
    Json(body): Json<DevicePatch>,
) -> Result<Json<DeviceEnrollment>, AdminApiError> {
    update_device_enrollment(&state, client_uid, body, Some(EnrollmentStatus::Enrolled)).await
}

async fn reject_device_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(client_uid): AxumPath<String>,
    Json(body): Json<DevicePatch>,
) -> Result<Json<DeviceEnrollment>, AdminApiError> {
    update_device_enrollment(&state, client_uid, body, Some(EnrollmentStatus::Rejected)).await
}

async fn patch_device_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(client_uid): AxumPath<String>,
    Json(body): Json<DevicePatch>,
) -> Result<Json<DeviceEnrollment>, AdminApiError> {
    update_device_enrollment(&state, client_uid, body, None).await
}

async fn delete_device_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(client_uid): AxumPath<String>,
) -> Result<Json<OkResponse>, AdminApiError> {
    let client_uid = client_uid.trim().to_string();
    if client_uid.is_empty() {
        return Err(AdminApiError::BadRequest(
            "device UID cannot be empty".to_string(),
        ));
    }
    let admin_snapshot = {
        let mut admin_state = state.admin_state.write().await;
        admin_state
            .devices
            .retain(|device| device.client_uid != client_uid);
        for client in &mut admin_state.clients {
            if client.client_uid.as_deref() == Some(client_uid.as_str()) {
                client.client_uid = None;
            }
        }
        admin_state.clone()
    };
    save_admin_state(&state, &admin_snapshot).await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn update_device_enrollment(
    state: &ServerState,
    client_uid: String,
    body: DevicePatch,
    forced_status: Option<EnrollmentStatus>,
) -> Result<Json<DeviceEnrollment>, AdminApiError> {
    let client_uid = normalize_client_uid(&client_uid, body.user_id);
    let now = unix_time_ms();
    let updated = {
        let mut admin_state = state.admin_state.write().await;
        let user_id = body
            .user_id
            .filter(|user_id| *user_id > 0)
            .or_else(|| {
                admin_state
                    .devices
                    .iter()
                    .find(|device| device.client_uid == client_uid)
                    .map(|device| device.user_id)
            })
            .unwrap_or_else(|| next_available_user_id(&admin_state, None));
        if admin_state
            .devices
            .iter()
            .any(|device| device.client_uid != client_uid && device.user_id == user_id)
        {
            return Err(AdminApiError::BadRequest(format!(
                "user_id {user_id} is already assigned to another device"
            )));
        }

        let device = admin_state
            .devices
            .iter_mut()
            .find(|device| device.client_uid == client_uid);
        let updated = if let Some(device) = device {
            device.user_id = user_id;
            if let Some(name) = body.name.clone() {
                device.name = name.trim().to_string();
            }
            if let Some(status) = forced_status.or(body.status) {
                device.status = status;
            }
            device.last_seen_ms = now;
            device.clone()
        } else {
            let device = DeviceEnrollment {
                client_uid: client_uid.clone(),
                user_id,
                status: forced_status
                    .or(body.status)
                    .unwrap_or(EnrollmentStatus::Enrolled),
                name: body.name.unwrap_or_default().trim().to_string(),
                role: ClientRole::Client,
                first_seen_ms: now,
                last_seen_ms: now,
                hardware_fingerprint: None,
                warnings: Vec::new(),
            };
            admin_state.devices.push(device.clone());
            device
        };
        normalize_device_enrollments(&mut admin_state.devices);
        if updated.status == EnrollmentStatus::Enrolled {
            if let Some(desired) = admin_state
                .clients
                .iter_mut()
                .find(|client| client.user_id == updated.user_id)
            {
                desired.client_uid = Some(updated.client_uid.clone());
                if desired.name.is_empty() && !updated.name.is_empty() {
                    desired.name = updated.name.clone();
                }
            }
        }
        let snapshot = admin_state.clone();
        drop(admin_state);
        save_admin_state(state, &snapshot).await?;
        updated
    };
    if updated.status == EnrollmentStatus::Enrolled {
        apply_desired_to_live_session_if_any(state, updated.user_id).await;
        push_config_update(state, updated.user_id).await;
    }
    Ok(Json(updated))
}

async fn put_channel_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(channel_id): AxumPath<ChannelId>,
    Json(body): Json<ChannelPut>,
) -> Result<Json<ChannelConfig>, AdminApiError> {
    let channel = ChannelConfig {
        id: channel_id,
        name: body.name,
    };
    let admin_snapshot = {
        let mut admin_state = state.admin_state.write().await;
        if let Some(existing) = admin_state
            .channels
            .iter_mut()
            .find(|existing| existing.id == channel_id)
        {
            *existing = channel.clone();
        } else {
            admin_state.channels.push(channel.clone());
        }
        admin_state.channels.sort_by_key(|channel| channel.id);
        admin_state.clone()
    };
    save_admin_state(&state, &admin_snapshot).await?;
    Ok(Json(channel))
}

async fn delete_channel_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(channel_id): AxumPath<ChannelId>,
) -> Result<Json<OkResponse>, AdminApiError> {
    let admin_snapshot = {
        let mut admin_state = state.admin_state.write().await;
        admin_state
            .channels
            .retain(|channel| channel.id != channel_id);
        admin_state.clone()
    };
    save_admin_state(&state, &admin_snapshot).await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn put_preset_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(preset_id): AxumPath<String>,
    Json(body): Json<PresetPut>,
) -> Result<Json<PresetConfig>, AdminApiError> {
    let preset = PresetConfig {
        id: normalize_preset_id(&preset_id)?,
        name: body.name.trim().to_string(),
        clients: normalize_preset_clients(body.clients),
    };
    if preset.name.is_empty() {
        return Err(AdminApiError::BadRequest(
            "preset name cannot be empty".to_string(),
        ));
    }

    let admin_snapshot = {
        let mut admin_state = state.admin_state.write().await;
        upsert_preset(&mut admin_state.presets, preset.clone());
        admin_state.clone()
    };
    save_admin_state(&state, &admin_snapshot).await?;
    Ok(Json(preset))
}

async fn delete_preset_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(preset_id): AxumPath<String>,
) -> Result<Json<OkResponse>, AdminApiError> {
    let preset_id = normalize_preset_id(&preset_id)?;
    let admin_snapshot = {
        let mut admin_state = state.admin_state.write().await;
        admin_state.presets.retain(|preset| preset.id != preset_id);
        admin_state.clone()
    };
    save_admin_state(&state, &admin_snapshot).await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn apply_preset_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(preset_id): AxumPath<String>,
) -> Result<Json<OkResponse>, AdminApiError> {
    let preset_id = normalize_preset_id(&preset_id)?;
    let preset = {
        let admin_state = state.admin_state.read().await;
        admin_state
            .presets
            .iter()
            .find(|preset| preset.id == preset_id)
            .cloned()
    };
    let Some(preset) = preset else {
        return Err(AdminApiError::BadRequest(format!(
            "unknown preset `{preset_id}`"
        )));
    };

    apply_preset(&state, &preset).await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn put_template_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(template_id): AxumPath<String>,
    Json(body): Json<ClientTemplatePut>,
) -> Result<Json<ClientTemplateConfig>, AdminApiError> {
    let template = ClientTemplateConfig {
        id: normalize_config_id(&template_id, "template")?,
        name: body.name.trim().to_string(),
        client: normalize_template_client(body.client),
    };
    if template.name.is_empty() {
        return Err(AdminApiError::BadRequest(
            "template name cannot be empty".to_string(),
        ));
    }

    let admin_snapshot = {
        let mut admin_state = state.admin_state.write().await;
        upsert_template(&mut admin_state.templates, template.clone());
        admin_state.clone()
    };
    save_admin_state(&state, &admin_snapshot).await?;
    Ok(Json(template))
}

async fn delete_template_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(template_id): AxumPath<String>,
) -> Result<Json<OkResponse>, AdminApiError> {
    let template_id = normalize_config_id(&template_id, "template")?;
    let admin_snapshot = {
        let mut admin_state = state.admin_state.write().await;
        admin_state
            .templates
            .retain(|template| template.id != template_id);
        admin_state.clone()
    };
    save_admin_state(&state, &admin_snapshot).await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn apply_template_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(template_id): AxumPath<String>,
    Json(body): Json<ApplyTemplateRequest>,
) -> Result<Json<DesiredClientConfig>, AdminApiError> {
    let desired = template_desired_for_user(&state, &template_id, body.user_id).await?;
    upsert_admin_client(&state, desired.clone()).await?;
    Ok(Json(
        desired_client(&state, body.user_id)
            .await
            .unwrap_or(desired),
    ))
}

async fn admin_direct_call_handler(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<DirectCallRequest>,
) -> Result<Json<OkResponse>, AdminApiError> {
    match apply_direct_call_event(&state, body.caller, body.target, body.active, body.duck).await {
        ControlResponse::Ack => Ok(Json(OkResponse { ok: true })),
        ControlResponse::Error { message } => Err(AdminApiError::BadRequest(message)),
        other => Err(AdminApiError::BadRequest(format!(
            "unexpected control response: {other:?}"
        ))),
    }
}

async fn admin_emergency_handler(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<EmergencyRequest>,
) -> Result<Json<OkResponse>, AdminApiError> {
    match apply_emergency_event(
        &state,
        body.source,
        body.active,
        body.target,
        body.duck_gain,
        body.mute_others,
    )
    .await
    {
        ControlResponse::Ack => Ok(Json(OkResponse { ok: true })),
        ControlResponse::Error { message } => Err(AdminApiError::BadRequest(message)),
        other => Err(AdminApiError::BadRequest(format!(
            "unexpected control response: {other:?}"
        ))),
    }
}

async fn admin_send_alert_handler(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<AdminAlertRequest>,
) -> Result<Json<AlertStatus>, AdminApiError> {
    match apply_send_alert_event(&state, body.sender, body.target, body.message).await {
        ControlResponse::Ack => {
            let alerts = state.alerts.read().await;
            let Some(alert) = alerts.last() else {
                return Err(AdminApiError::Internal("alert was not created".to_string()));
            };
            let sessions = state.sessions.read().await;
            Ok(Json(alert_status_with_sessions(alert, &sessions)))
        }
        ControlResponse::Error { message } => Err(AdminApiError::BadRequest(message)),
        other => Err(AdminApiError::BadRequest(format!(
            "unexpected control response: {other:?}"
        ))),
    }
}

async fn admin_tts_handler(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<AdminTtsRequest>,
) -> Result<Json<TtsAnnouncementResponse>, AdminApiError> {
    Ok(Json(apply_tts_announcement_event(&state, body).await?))
}

async fn admin_announcement_handler(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<AdminAnnouncementRequest>,
) -> Result<Json<TtsAnnouncementResponse>, AdminApiError> {
    Ok(Json(apply_announcement_event(&state, body).await?))
}

async fn admin_cancel_alert_handler(
    State(state): State<Arc<ServerState>>,
    AxumPath(alert_id): AxumPath<AlertId>,
    Json(body): Json<CancelAlertRequest>,
) -> Result<Json<OkResponse>, AdminApiError> {
    let user_id = body.user_id.unwrap_or(SERVER_USER_ID);
    match apply_cancel_alert_event(&state, user_id, alert_id).await {
        ControlResponse::Ack => Ok(Json(OkResponse { ok: true })),
        ControlResponse::Error { message } => Err(AdminApiError::BadRequest(message)),
        other => Err(AdminApiError::BadRequest(format!(
            "unexpected control response: {other:?}"
        ))),
    }
}

async fn upsert_admin_client(
    state: &ServerState,
    desired: DesiredClientConfig,
) -> Result<(), AdminApiError> {
    match apply_desired_client(state, desired).await {
        ControlResponse::Ack => Ok(()),
        ControlResponse::Error { message } => Err(AdminApiError::BadRequest(message)),
        other => Err(AdminApiError::Internal(format!(
            "unexpected control response: {other:?}"
        ))),
    }
}

async fn admin_state_snapshot(state: &ServerState) -> AdminStateResponse {
    let (sessions, metrics) = status_snapshot(state).await;
    let admin_state = state.admin_state.read().await.clone();
    let sessions_guard = state.sessions.read().await;
    let alerts = state.alerts.read().await;
    let (active_alerts, recent_alerts) =
        admin_alert_statuses_with_sessions(&alerts, &sessions_guard);
    let emergency = state.emergency.read().await;
    let emergency = {
        emergency.as_ref().map(|emergency| {
            emergency.status(emergency_recipients_for_sessions(
                emergency,
                &sessions_guard,
            ))
        })
    };
    let warnings = admin_warnings(&admin_state, &sessions);
    let recording = recording_status_snapshot(state).await;
    let transcription = live_transcription_status_snapshot(state).await;
    let deepfilternet = deepfilternet_status_snapshot(state).await;

    AdminStateResponse {
        build: common::current_build_info(),
        sessions,
        metrics,
        enrollment_policy: state.enrollment_policy,
        recording,
        transcription,
        deepfilternet,
        channels: admin_state.channels,
        devices: admin_state.devices,
        clients: admin_state.clients,
        presets: admin_state.presets,
        templates: admin_state.templates,
        active_alerts,
        recent_alerts,
        emergency,
        warnings,
    }
}

fn admin_warnings(
    admin_state: &PersistedAdminState,
    sessions: &[SessionStatus],
) -> Vec<AdminWarning> {
    let mut warnings = admin_state
        .clients
        .iter()
        .filter_map(|client| {
            let session = sessions
                .iter()
                .find(|session| session.user_id == client.user_id)?;
            if session.codec != client.codec && !session.supported_codecs.contains(&client.codec) {
                Some(AdminWarning {
                    user_id: client.user_id,
                    message: format!(
                        "desired codec {:?} is not supported by this connected client; using {:?}",
                        client.codec, session.codec
                    ),
                })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    warnings.extend(workflow_validation_warnings(admin_state));

    for client in &admin_state.clients {
        let Some(session) = sessions
            .iter()
            .find(|session| session.user_id == client.user_id)
        else {
            continue;
        };
        if client.stereo.enabled && !session.stereo_status.active {
            warnings.push(AdminWarning {
                user_id: client.user_id,
                message: session
                    .stereo_status
                    .warning
                    .clone()
                    .unwrap_or_else(|| "stereo receive is configured but inactive".to_string()),
            });
        }
    }

    for device in &admin_state.devices {
        for message in &device.warnings {
            warnings.push(AdminWarning {
                user_id: device.user_id,
                message: format!("device {}: {message}", device.client_uid),
            });
        }
        let live_count = sessions
            .iter()
            .filter(|session| session.client_uid == device.client_uid)
            .count();
        if live_count > 1 {
            warnings.push(AdminWarning {
                user_id: device.user_id,
                message: format!("device {} has multiple live sessions", device.client_uid),
            });
        }
    }

    for client in &admin_state.clients {
        let Some(session) = sessions
            .iter()
            .find(|session| session.user_id == client.user_id)
        else {
            continue;
        };
        if client.esp32_audio.enabled {
            match session
                .capture
                .as_ref()
                .and_then(|capture| capture.codec_config.as_ref())
            {
                Some(codec_config) if !codec_config.server_control_enabled => {
                    warnings.push(AdminWarning {
                        user_id: client.user_id,
                        message: "ESP32 audio override is configured, but the client reports local menuconfig audio settings".to_string(),
                    });
                }
                None => warnings.push(AdminWarning {
                    user_id: client.user_id,
                    message:
                        "ESP32 audio override is configured, but this client has not reported ESP32 capture health".to_string(),
                }),
                _ => {}
            }
            if let Some(codec_config) = session
                .capture
                .as_ref()
                .and_then(|capture| capture.codec_config.as_ref())
            {
                let reported_hardware_rate = if codec_config.hardware_sample_rate_hz > 0 {
                    codec_config.hardware_sample_rate_hz
                } else {
                    codec_config.i2s_sample_rate_hz
                };
                if reported_hardware_rate != 0 && reported_hardware_rate != 48_000 {
                    warnings.push(AdminWarning {
                        user_id: client.user_id,
                        message: format!(
                            "ESP32 audio hardware reports {} Hz; the Ai-Thinker ES8388 firmware baseline expects a fixed 48000 Hz hardware path with software conversion for pcm16/pcm24/pcm48",
                            reported_hardware_rate
                        ),
                    });
                }
                if let Some(active_codec) = codec_config.active_codec {
                    if active_codec != session.codec {
                        warnings.push(AdminWarning {
                            user_id: client.user_id,
                            message: format!(
                                "ESP32 reports active codec {:?} but the server is sending {:?}",
                                active_codec, session.codec
                            ),
                        });
                    }
                }
            }
        }
    }

    for client in &admin_state.clients {
        let Some(session) = sessions
            .iter()
            .find(|session| session.user_id == client.user_id)
        else {
            continue;
        };
        let advertised = session
            .advertised_buttons
            .iter()
            .map(|button| button.id.as_str())
            .collect::<HashSet<_>>();
        for button in &client.buttons {
            if !advertised.contains(button.id.as_str()) {
                warnings.push(AdminWarning {
                    user_id: client.user_id,
                    message: format!(
                        "configured button `{}` is not advertised by this connected client",
                        button.id
                    ),
                });
            }
        }
    }

    for session in sessions {
        if let Some(capture) = &session.capture {
            warnings.extend(capture_health_warnings(session.user_id, capture));
        }
        warnings.extend(bridge_status_warnings(session));
    }

    warnings
}

fn bridge_status_warnings(session: &SessionStatus) -> Vec<AdminWarning> {
    if session.role != ClientRole::Bridge {
        return Vec::new();
    }

    let mut warnings = Vec::new();
    if let Some(bridge) = &session.bridge {
        let tx = bridge.tx.iter().copied().collect::<HashSet<_>>();
        let listen = bridge.listen.iter().copied().collect::<HashSet<_>>();
        let mut overlap = tx.intersection(&listen).copied().collect::<Vec<_>>();
        overlap.sort_unstable();
        if !overlap.is_empty() {
            warnings.push(AdminWarning {
                user_id: session.user_id,
                message: format!(
                    "bridge transmits and listens on channel(s) {}; separate bridge input and output routes to avoid feedback",
                    overlap
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            });
        }
        match bridge.mode {
            common::BridgeMode::Input if !bridge.listen.is_empty() => warnings.push(AdminWarning {
                user_id: session.user_id,
                message:
                    "input bridge reports listen channels; input-only bridges should only transmit production audio into intercom"
                        .to_string(),
            }),
            common::BridgeMode::Output if !bridge.tx.is_empty() => warnings.push(AdminWarning {
                user_id: session.user_id,
                message:
                    "output bridge reports TX channels; output-only bridges should only receive an intercom mix"
                        .to_string(),
            }),
            _ => {}
        }
    }

    warnings
}

fn workflow_validation_warnings(admin_state: &PersistedAdminState) -> Vec<AdminWarning> {
    let mut warnings = Vec::new();
    for client in &admin_state.clients {
        let lower_name = client.name.to_ascii_lowercase();
        let is_bridge = client.role == ClientRole::Bridge;
        let is_program_bridge =
            is_bridge && (lower_name.contains("program") || client.tx.contains(&CHANNEL_PROGRAM));
        let is_pa_bridge =
            is_bridge && (lower_name.contains("pa") || client.listen.contains(&CHANNEL_PA));
        let is_listen_only_ifb = client.ifb.enabled
            && client.tx.is_empty()
            && matches!(client.talk_mode, TalkMode::Muted);

        if client.tx.contains(&CHANNEL_PA) && !is_pa_bridge {
            warnings.push(AdminWarning {
                user_id: client.user_id,
                message: "PA is configured as a regular TX channel; use a dedicated PA button route instead".to_string(),
            });
        }

        if is_bridge {
            let tx = client.tx.iter().copied().collect::<HashSet<_>>();
            let listen = client.listen.iter().copied().collect::<HashSet<_>>();
            let mut overlap = tx.intersection(&listen).copied().collect::<Vec<_>>();
            overlap.sort_unstable();
            if !overlap.is_empty() {
                warnings.push(AdminWarning {
                    user_id: client.user_id,
                    message: format!(
                        "bridge desired config both listens and transmits on channel(s) {}; split bridge input/output aliases or use different channels to prevent feedback",
                        overlap
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                });
            }
        }

        if client.ifb.enabled {
            if client.ifb.program.is_empty() {
                warnings.push(AdminWarning {
                    user_id: client.user_id,
                    message: "IFB is enabled but has no program channels".to_string(),
                });
            }
            if client.ifb.interrupt.is_empty() {
                warnings.push(AdminWarning {
                    user_id: client.user_id,
                    message: "IFB is enabled but has no interrupt channels".to_string(),
                });
            }
            for channel in &client.ifb.interrupt {
                if !client.listen.contains(channel) {
                    warnings.push(AdminWarning {
                        user_id: client.user_id,
                        message: format!(
                            "IFB interrupt channel {channel} is not in this client's listen channels"
                        ),
                    });
                }
            }
        }

        if is_program_bridge {
            if !client.tx.contains(&CHANNEL_PROGRAM) {
                warnings.push(AdminWarning {
                    user_id: client.user_id,
                    message: "program bridge should transmit into the Program channel".to_string(),
                });
            }
            if !matches!(client.talk_mode, TalkMode::Open) {
                warnings.push(AdminWarning {
                    user_id: client.user_id,
                    message: "program bridge should use open talk mode so program audio is continuously available".to_string(),
                });
            }
        }

        if is_pa_bridge {
            if !client.tx.is_empty() {
                warnings.push(AdminWarning {
                    user_id: client.user_id,
                    message: "PA bridge should not have TX channels".to_string(),
                });
            }
            if !matches!(client.talk_mode, TalkMode::Muted) {
                warnings.push(AdminWarning {
                    user_id: client.user_id,
                    message: "PA bridge should stay muted; it should only receive PA channel audio"
                        .to_string(),
                });
            }
        }

        if is_listen_only_ifb
            && (client.lockout.allow_channels
                || client.lockout.allow_talk_mode
                || client.lockout.allow_buttons
                || client.lockout.allow_ifb)
        {
            warnings.push(AdminWarning {
                user_id: client.user_id,
                message:
                    "listen-only IFB client still allows local route, talk-mode, button, or IFB edits"
                        .to_string(),
            });
        }
    }
    warnings
}

fn capture_health_warnings(user_id: UserId, capture: &CaptureHealthStatus) -> Vec<AdminWarning> {
    let mut warnings = Vec::new();
    if let Some(desktop) = &capture.desktop {
        if desktop.post_gain_clipped_samples > 0 {
            warnings.push(AdminWarning {
                user_id,
                message: format!(
                    "Desktop capture is clipping after mic gain ({} samples in last report); lower mic gain or OS input level",
                    desktop.post_gain_clipped_samples
                ),
            });
        }
        if desktop.pre_gain_clipped_samples > 0 {
            warnings.push(AdminWarning {
                user_id,
                message: format!(
                    "Desktop capture reached full scale before local gain ({} samples in last report); lower the operating system input level or move the mic farther away",
                    desktop.pre_gain_clipped_samples
                ),
            });
        }
        if desktop.post_gain.peak > 0.92 && desktop.post_gain_clipped_samples == 0 {
            warnings.push(AdminWarning {
                user_id,
                message: "Desktop capture is close to clipping after mic gain".to_string(),
            });
        }
        if desktop.post_gain.rms < 0.003 && desktop.post_gain.peak < 0.02 {
            warnings.push(AdminWarning {
                user_id,
                message: "Desktop capture is nearly silent; check selected input device, input channel, and mic permissions".to_string(),
            });
        }
        if desktop.dropped_frames > 0 {
            warnings.push(AdminWarning {
                user_id,
                message: format!(
                    "Desktop capture dropped {} local mic frames before network send",
                    desktop.dropped_frames
                ),
            });
        }
    }
    let is_esp32 = capture.codec_config.is_some()
        || capture.wifi.is_some()
        || capture.memory.is_some()
        || capture.task_stack_high_water_bytes.is_some()
        || capture.display.is_some()
        || capture.battery.is_some()
        || (capture.runtime.is_none() && capture.desktop.is_none());
    if is_esp32 && (capture.raw_clipped_samples > 0 || capture.software_clipped_samples > 0) {
        warnings.push(AdminWarning {
            user_id,
            message: format!(
                "ESP32 capture is clipping (raw {}, software {} samples in last report)",
                capture.raw_clipped_samples, capture.software_clipped_samples
            ),
        });
    }
    if is_esp32 && capture.playback_underflows > 0 {
        warnings.push(AdminWarning {
            user_id,
            message: format!(
                "ESP32 playback jitter buffer underflowed {} times; increase playback jitter/prefill or check Wi-Fi jitter",
                capture.playback_underflows
            ),
        });
    }
    if is_esp32 && capture.playback_overflows > 0 {
        warnings.push(AdminWarning {
            user_id,
            message: format!(
                "ESP32 playback jitter buffer overflowed {} times; reduce jitter buffer delay or check bursty server delivery",
                capture.playback_overflows
            ),
        });
    }
    if is_esp32
        && (capture.playback_i2s_gap_warnings > 0
            || capture.playback_i2s_slow_warnings > 0
            || capture.playback_i2s_short_warnings > 0)
    {
        warnings.push(AdminWarning {
            user_id,
            message: format!(
                "ESP32 playback I2S timing warnings: gaps {}, slow writes {}, short writes {}; audio task or codec writes are missing deadlines",
                capture.playback_i2s_gap_warnings,
                capture.playback_i2s_slow_warnings,
                capture.playback_i2s_short_warnings
            ),
        });
    }
    if is_esp32 && capture.min_free_heap_bytes > 0 && capture.min_free_heap_bytes < 16 * 1024 {
        warnings.push(AdminWarning {
            user_id,
            message: format!(
                "ESP32 heap low-water mark is {} bytes; reduce buffers/features if this keeps dropping",
                capture.min_free_heap_bytes
            ),
        });
    }
    if is_esp32 && capture.selected.peak > 0.92 && capture.raw_clipped_samples == 0 {
        warnings.push(AdminWarning {
            user_id,
            message: "ESP32 capture is close to clipping; lower ES8388 PGA gain before using software gain".to_string(),
        });
    }
    if is_esp32 && capture.selected.rms < 0.003 && capture.selected.peak < 0.02 {
        warnings.push(AdminWarning {
            user_id,
            message:
                "ESP32 capture is nearly silent; check ADC input, mic wiring, or selected channel"
                    .to_string(),
        });
    }
    if is_esp32 && capture.adc_input == "difference" && capture.capture_channel == "average" {
        warnings.push(AdminWarning {
            user_id,
            message: "ESP32 differential mic is using average capture; this can cancel voice. Use left or right, then compare capture health.".to_string(),
        });
    }
    if is_esp32
        && !capture.alc_enabled
        && capture.selected.rms < 0.02
        && capture.selected.peak < 0.12
    {
        warnings.push(AdminWarning {
            user_id,
            message:
                "ESP32 mic is low and ES8388 ALC is off; enable ALC before raising software gain"
                    .to_string(),
        });
    }
    if is_esp32 && capture.selected.dc_offset.abs() > 0.08 {
        warnings.push(AdminWarning {
            user_id,
            message: format!(
                "ESP32 capture has high DC offset ({:.1}%); keep the capture high-pass enabled and check analog input bias",
                capture.selected.dc_offset * 100.0
            ),
        });
    }
    if is_esp32
        && capture.capture_channel == "left"
        && capture.right.rms > 0.02
        && capture.left.rms < capture.right.rms * 0.35
    {
        warnings.push(AdminWarning {
            user_id,
            message: "ESP32 right input is much stronger than selected left channel; try capture channel `right` or `average`".to_string(),
        });
    }
    if is_esp32
        && capture.capture_channel == "right"
        && capture.left.rms > 0.02
        && capture.right.rms < capture.left.rms * 0.35
    {
        warnings.push(AdminWarning {
            user_id,
            message: "ESP32 left input is much stronger than selected right channel; try capture channel `left` or `average`".to_string(),
        });
    }
    if let Some(playback) = &capture.playback {
        if playback.underflows > 0 {
            warnings.push(AdminWarning {
                user_id,
                message: format!(
                    "Client playback underflowed {} times; increase jitter/prefill or check network jitter",
                    playback.underflows
                ),
            });
        }
        if playback.overflows > 0 {
            warnings.push(AdminWarning {
                user_id,
                message: format!(
                    "Client playback overflowed {} times; reduce jitter buffer delay or check bursty delivery",
                    playback.overflows
                ),
            });
        }
    }
    if let Some(transport) = &capture.client_transport {
        if transport.malformed_packets > 0 || transport.decode_errors > 0 {
            warnings.push(AdminWarning {
                user_id,
                message: format!(
                    "Client dropped audio packets: malformed {}, decode errors {}",
                    transport.malformed_packets, transport.decode_errors
                ),
            });
        }
        if transport.tx_queue_drops > 0 || transport.tx_send_failures > 0 {
            warnings.push(AdminWarning {
                user_id,
                message: format!(
                    "Client transmit path dropped audio: queue drops {}, send failures {}",
                    transport.tx_queue_drops, transport.tx_send_failures
                ),
            });
        }
    }
    warnings
}

#[derive(Debug, Serialize)]
struct OkResponse {
    ok: bool,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug)]
enum AdminApiError {
    BadRequest(String),
    Internal(String),
}

impl From<anyhow::Error> for AdminApiError {
    fn from(value: anyhow::Error) -> Self {
        Self::Internal(value.to_string())
    }
}

impl IntoResponse for AdminApiError {
    fn into_response(self) -> Response {
        #[derive(Serialize)]
        struct ErrorBody {
            error: String,
        }

        let (status, error) = match self {
            Self::BadRequest(error) => (StatusCode::BAD_REQUEST, error),
            Self::Internal(error) => (StatusCode::INTERNAL_SERVER_ERROR, error),
        };
        (status, Json(ErrorBody { error })).into_response()
    }
}

async fn admin_recording_start_handler(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<StartRecordingRequest>,
) -> Result<Json<RecordingStatusResponse>, AdminApiError> {
    start_recording_session(&state, body).await?;
    Ok(Json(recording_status_snapshot(&state).await))
}

async fn admin_recording_stop_handler(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<RecordingStatusResponse>, AdminApiError> {
    if let Some(stopped) = stop_recording_session(&state).await? {
        let transcription_state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(err) = run_recording_transcription(transcription_state, stopped).await {
                tracing::warn!(%err, "recording transcription failed");
            }
        });
    }
    Ok(Json(recording_status_snapshot(&state).await))
}

async fn admin_recording_status_handler(
    State(state): State<Arc<ServerState>>,
) -> Json<RecordingStatusResponse> {
    Json(recording_status_snapshot(&state).await)
}

async fn admin_recording_sessions_handler(
    State(state): State<Arc<ServerState>>,
) -> Json<Vec<RecordingSessionSummary>> {
    Json(recording_sessions_snapshot(&state).await)
}

async fn admin_live_transcription_start_handler(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<StartLiveTranscriptionRequest>,
) -> Result<Json<LiveTranscriptionStatusResponse>, AdminApiError> {
    start_live_transcription(&state, body).await?;
    Ok(Json(live_transcription_status_snapshot(&state).await))
}

async fn admin_live_transcription_stop_handler(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<LiveTranscriptionStatusResponse>, AdminApiError> {
    stop_live_transcription(&state).await?;
    Ok(Json(live_transcription_status_snapshot(&state).await))
}

async fn admin_live_transcription_status_handler(
    State(state): State<Arc<ServerState>>,
) -> Json<LiveTranscriptionStatusResponse> {
    Json(live_transcription_status_snapshot(&state).await)
}

async fn admin_transcription_models_handler(
    State(state): State<Arc<ServerState>>,
) -> Json<Vec<WhisperModelInfo>> {
    Json(list_whisper_models(&state).await)
}

async fn admin_select_transcription_model_handler(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<SelectWhisperModelRequest>,
) -> Result<Json<LiveTranscriptionStatusResponse>, AdminApiError> {
    select_whisper_model(&state, body).await?;
    Ok(Json(live_transcription_status_snapshot(&state).await))
}

async fn admin_deepfilternet_models_handler(
    State(state): State<Arc<ServerState>>,
) -> Json<DeepFilterNetStatusResponse> {
    Json(deepfilternet_status_snapshot(&state).await)
}

async fn admin_transcripts_handler(
    State(state): State<Arc<ServerState>>,
    axum::extract::Query(query): axum::extract::Query<TranscriptQuery>,
) -> Json<Vec<TranscriptSegment>> {
    Json(query_transcripts(&state, query).await)
}

async fn admin_add_transcript_handler(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<AddTranscriptRequest>,
) -> Result<Json<TranscriptSegment>, AdminApiError> {
    if body.text.trim().is_empty() {
        return Err(AdminApiError::BadRequest(
            "transcript text cannot be empty".to_string(),
        ));
    }
    Ok(Json(
        append_transcript_segment(
            &state,
            TranscriptAppend {
                user_id: body.user_id,
                contexts: body.contexts,
                started_at_ms: None,
                ended_at_ms: None,
                text: body.text,
                confidence: body.confidence,
                engine: "manual".to_string(),
                source: TranscriptSource::Manual,
                final_segment: true,
            },
        )
        .await?,
    ))
}

async fn handle_control_connection(
    stream: tokio::net::TcpStream,
    state: Arc<ServerState>,
) -> anyhow::Result<()> {
    let ws = tokio_tungstenite::accept_async(stream).await?;
    let (mut write, mut read) = ws.split();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<ControlEvent>();
    let mut registered_user = None;

    let result = loop {
        tokio::select! {
            maybe_message = read.next() => {
                let Some(message) = maybe_message else {
                    break Ok(());
                };
                let message = match message {
                    Ok(message) => message,
                    Err(err) => break Err(err.into()),
                };
                match message {
                    Message::Text(text) => {
                        state
                            .metrics
                            .control_messages_received
                            .fetch_add(1, Ordering::Relaxed);
                        let response = match serde_json::from_str::<ControlMessage>(&text) {
                            Ok(control) => {
                                if matches!(control, ControlMessage::Hello { .. }) {
                                    let response = apply_control(&state, control).await;
                                    if let ControlResponse::Hello {
                                        user_id,
                                        preconfigured,
                                        enrollment,
                                        ..
                                    } = &response
                                    {
                                        if *enrollment == EnrollmentStatus::Enrolled {
                                            register_control_client(&state, *user_id, event_tx.clone()).await;
                                            registered_user = Some(*user_id);
                                            if *preconfigured {
                                                if let Some(event) = config_event_snapshot(&state, *user_id).await {
                                                    let _ = event_tx.send(event);
                                                }
                                            }
                                        }
                                    }
                                    response
                                } else {
                                    apply_control(&state, control).await
                                }
                            }
                            Err(err) => ControlResponse::Error {
                                message: format!("parse control message: {err}"),
                            },
                        };
                        if let Err(err) = write.send(Message::Text(serde_json::to_string(&response)?)).await {
                            break Err(err.into());
                        }
                    }
                    Message::Ping(payload) => {
                        if let Err(err) = write.send(Message::Pong(payload)).await {
                            break Err(err.into());
                        }
                    }
                    Message::Close(_) => break Ok(()),
                    _ => {}
                }
            }
            maybe_event = event_rx.recv() => {
                let Some(event) = maybe_event else {
                    break Ok(());
                };
                if let Err(err) = write.send(Message::Text(serde_json::to_string(&event)?)).await {
                    break Err(err.into());
                }
            }
        }
    };

    if let Some(user_id) = registered_user {
        unregister_control_client(&state, user_id, &event_tx).await;
        clear_active_buttons(&state, user_id).await;
        clear_emergency_for_source(&state, user_id).await;
    }

    result
}

async fn clear_audio_endpoint_if_matches(
    state: &ServerState,
    user_id: UserId,
    addr: SocketAddr,
) -> bool {
    let mut sessions = state.sessions.write().await;
    let Some(session) = sessions.get_mut(&user_id) else {
        return false;
    };
    if session.addr != Some(addr) {
        return false;
    }
    session.addr = None;
    true
}

async fn apply_control(state: &ServerState, control: ControlMessage) -> ControlResponse {
    match control {
        ControlMessage::Hello {
            user_id,
            requested_user_id,
            client_uid,
            codecs,
            buttons,
            role,
        } => {
            let requested_user_id = requested_user_id.or((user_id > 0).then_some(user_id));
            let enrollment =
                match resolve_client_enrollment(state, client_uid, requested_user_id, role).await {
                    Ok(enrollment) => enrollment,
                    Err(message) => return ControlResponse::Error { message },
                };
            if enrollment.status != EnrollmentStatus::Enrolled {
                return ControlResponse::Hello {
                    preconfigured: enrollment.preconfigured,
                    user_id: enrollment.user_id,
                    client_uid: enrollment.client_uid,
                    enrollment: enrollment.status,
                };
            }

            let user_id = enrollment.user_id;
            let desired = desired_client(state, user_id).await;
            let preconfigured = enrollment.preconfigured || desired.is_some();
            let mut sessions = state.sessions.write().await;
            let session = sessions.entry(user_id).or_insert_with(Session::new);
            session.client_uid = enrollment.client_uid.clone();
            session.enrollment = enrollment.status;
            session.role = role;
            session.supported_codecs = normalize_supported_codecs(codecs);
            session.advertised_buttons = normalize_button_capabilities(buttons);
            if let Some(desired) = desired.as_ref() {
                apply_desired_to_session_fields(desired, session);
            } else {
                apply_default_operator_channels_to_session(session);
                if !session.supported_codecs.contains(&session.output_codec) {
                    session.output_codec = Codec::Pcm16;
                }
            }
            session.last_seen = Instant::now();
            tracing::info!(
                user_id,
                client_uid = %session.client_uid,
                codecs = ?session.supported_codecs,
                "registered control client"
            );
            ControlResponse::Hello {
                preconfigured,
                user_id,
                client_uid: enrollment.client_uid,
                enrollment: enrollment.status,
            }
        }
        ControlMessage::Config {
            user_id,
            role,
            name,
            listen,
            tx,
            vol,
            talker_vol,
            codec,
            opus_profile,
            talk_mode,
            priority,
            priority_channels,
            processing,
            buttons,
            ifb,
            stereo,
            esp32_audio,
        } => {
            let baseline = desired_baseline_for_user(state, user_id).await;
            if let Err(message) = validate_config_lockout(
                &baseline,
                &listen,
                &tx,
                &vol,
                talker_vol.as_ref(),
                codec,
                opus_profile,
                talk_mode,
                priority,
                priority_channels.as_ref(),
                processing.as_ref(),
                buttons.as_ref(),
                ifb.as_ref(),
            ) {
                return ControlResponse::Error { message };
            }
            if let Some(codec) = codec {
                if let Err(message) = codec_available_for_user(state, user_id, codec).await {
                    return ControlResponse::Error { message };
                }
            }
            let mut desired = baseline;
            if let Some(role) = role {
                desired.role = role;
            }
            if let Some(name) = name {
                desired.name = name;
            }
            desired.listen = listen;
            desired.tx = tx;
            desired.vol = vol;
            if let Some(talker_vol) = talker_vol {
                desired.talker_vol = talker_vol;
            }
            if let Some(codec) = codec {
                desired.codec = codec;
            }
            if let Some(opus_profile) = opus_profile {
                desired.opus_profile = opus_profile;
            }
            if let Some(talk_mode) = talk_mode {
                desired.talk_mode = talk_mode;
            }
            if let Some(priority) = priority {
                desired.priority = priority;
            }
            if let Some(priority_channels) = priority_channels {
                desired.priority_channels = sorted_unique_channels(priority_channels);
            }
            if let Some(processing) = processing {
                desired.processing = processing;
            }
            if let Some(buttons) = buttons {
                desired.buttons = normalize_button_configs(buttons);
            }
            if let Some(ifb) = ifb {
                desired.ifb = normalize_ifb_config(ifb);
            }
            if let Some(stereo) = stereo {
                desired.stereo = normalize_stereo_config(stereo);
            }
            if let Some(esp32_audio) = esp32_audio {
                desired.esp32_audio = normalize_esp32_audio_config(esp32_audio);
            }
            tracing::info!(
                user_id,
                listen = ?desired.listen,
                tx = ?desired.tx,
                codec = ?desired.codec,
                "applied client config"
            );
            apply_desired_client(state, desired).await
        }
        ControlMessage::AudioCodec { user_id, codec } => {
            if let Err(message) =
                ensure_control_allowed(state, user_id, |lockout| lockout.allow_codec, "codec").await
            {
                return ControlResponse::Error { message };
            }
            if let Err(message) = codec_available_for_user(state, user_id, codec).await {
                return ControlResponse::Error { message };
            }
            let mut desired = desired_baseline_for_user(state, user_id).await;
            desired.codec = codec;
            tracing::info!(user_id, codec = ?desired.codec, "updated edge codec");
            apply_desired_client(state, desired).await
        }
        ControlMessage::TalkMode { user_id, mode } => {
            if let Err(message) = ensure_control_allowed(
                state,
                user_id,
                |lockout| lockout.allow_talk_mode,
                "talk mode",
            )
            .await
            {
                return ControlResponse::Error { message };
            }
            let mut desired = desired_baseline_for_user(state, user_id).await;
            desired.talk_mode = mode;
            tracing::info!(user_id, talk_mode = ?desired.talk_mode, "updated talk mode");
            apply_desired_client(state, desired).await
        }
        ControlMessage::Ping { user_id } => {
            let mut sessions = state.sessions.write().await;
            let session = sessions.entry(user_id).or_insert_with(Session::new);
            session.last_seen = Instant::now();
            ControlResponse::Ack
        }
        ControlMessage::CaptureHealth { user_id, health } => {
            record_capture_health(state, user_id, health).await;
            ControlResponse::Ack
        }
        ControlMessage::BridgeStatus { user_id, status } => {
            record_bridge_status(state, user_id, status).await;
            ControlResponse::Ack
        }
        ControlMessage::Talk { user_id, active } => {
            apply_regular_talk_event(state, user_id, active).await
        }
        ControlMessage::Priority { user_id, active } => {
            if let Err(message) =
                ensure_control_allowed(state, user_id, |lockout| lockout.allow_priority, "priority")
                    .await
            {
                return ControlResponse::Error { message };
            }
            let mut desired = desired_baseline_for_user(state, user_id).await;
            desired.priority = active;
            if active && desired.priority_channels.is_empty() {
                desired.priority_channels = sorted_unique_channels(desired.tx.clone());
            }
            tracing::info!(user_id, active, "updated priority state");
            apply_desired_client(state, desired).await
        }
        ControlMessage::Emergency {
            user_id,
            active,
            target,
            duck_gain,
            mute_others,
        } => apply_emergency_event(state, user_id, active, target, duck_gain, mute_others).await,
        ControlMessage::Button {
            user_id,
            button_id,
            pressed,
        } => {
            if let Err(message) =
                ensure_control_allowed(state, user_id, |lockout| lockout.allow_buttons, "buttons")
                    .await
            {
                return ControlResponse::Error { message };
            }
            apply_button_event(state, user_id, button_id, pressed).await
        }
        ControlMessage::SendAlert {
            user_id,
            target,
            message,
        } => apply_send_alert_event(state, user_id, target, message).await,
        ControlMessage::AckAlert { user_id, alert_id } => {
            apply_ack_alert_event(state, user_id, alert_id).await
        }
        ControlMessage::CancelAlert { user_id, alert_id } => {
            apply_cancel_alert_event(state, user_id, alert_id).await
        }
        ControlMessage::DirectCall {
            user_id,
            target_user_id,
            active,
            duck,
        } => apply_direct_call_event(state, user_id, target_user_id, active, duck).await,
        ControlMessage::ReplyCall {
            user_id,
            active,
            duck,
        } => apply_reply_call_event(state, user_id, active, duck).await,
        ControlMessage::Status => {
            let (sessions, metrics) = status_snapshot(state).await;
            ControlResponse::Status { sessions, metrics }
        }
    }
}

async fn desired_client(state: &ServerState, user_id: UserId) -> Option<DesiredClientConfig> {
    state
        .admin_state
        .read()
        .await
        .clients
        .iter()
        .find(|client| client.user_id == user_id)
        .cloned()
}

async fn desired_baseline_for_user(state: &ServerState, user_id: UserId) -> DesiredClientConfig {
    if let Some(desired) = desired_client(state, user_id).await {
        return desired;
    }

    let sessions = state.sessions.read().await;
    let Some(session) = sessions.get(&user_id) else {
        return DesiredClientConfig::new(user_id);
    };

    let mut listen = session.listen_channels.iter().copied().collect::<Vec<_>>();
    listen.sort_unstable();
    let mut tx = session.tx_channels.iter().copied().collect::<Vec<_>>();
    tx.sort_unstable();

    DesiredClientConfig {
        user_id,
        client_uid: (!session.client_uid.is_empty()).then(|| session.client_uid.clone()),
        role: session.role,
        name: String::new(),
        listen,
        tx,
        vol: session.channel_volumes.clone(),
        talker_vol: session.talker_volumes.clone(),
        codec: session.output_codec,
        opus_profile: session.opus_profile,
        talk_mode: session.talk_mode,
        priority: session.priority,
        priority_channels: {
            let mut channels = session
                .priority_channels
                .iter()
                .copied()
                .collect::<Vec<_>>();
            channels.sort_unstable();
            channels
        },
        buttons: sorted_buttons(&session.buttons),
        ifb: session.ifb.clone(),
        lockout: session.lockout.clone(),
        stereo: session.stereo.clone(),
        esp32_audio: session.esp32_audio.clone(),
        processing: session.processing.clone(),
    }
}

async fn ensure_control_allowed(
    state: &ServerState,
    user_id: UserId,
    allowed: impl FnOnce(&ClientLockoutPolicy) -> bool,
    label: &str,
) -> Result<(), String> {
    let desired = desired_baseline_for_user(state, user_id).await;
    if allowed(&desired.lockout) {
        Ok(())
    } else {
        Err(format!("{label} is locked by admin"))
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_config_lockout(
    baseline: &DesiredClientConfig,
    listen: &[ChannelId],
    tx: &[ChannelId],
    vol: &HashMap<ChannelId, f32>,
    talker_vol: Option<&HashMap<UserId, f32>>,
    codec: Option<Codec>,
    opus_profile: Option<OpusProfile>,
    talk_mode: Option<TalkMode>,
    priority: Option<bool>,
    priority_channels: Option<&Vec<ChannelId>>,
    _processing: Option<&ProcessingConfig>,
    buttons: Option<&Vec<TalkButtonConfig>>,
    ifb: Option<&IfbConfig>,
) -> Result<(), String> {
    let lockout = &baseline.lockout;
    if !lockout.allow_channels
        && (!same_channels(listen, &baseline.listen) || !same_channels(tx, &baseline.tx))
    {
        return Err("channels are locked by admin".to_string());
    }
    if !lockout.allow_volumes
        && (vol != &baseline.vol || talker_vol.is_some_and(|value| value != &baseline.talker_vol))
    {
        return Err("volumes are locked by admin".to_string());
    }
    if !lockout.allow_codec
        && (codec.is_some_and(|value| value != baseline.codec)
            || opus_profile.is_some_and(|value| value != baseline.opus_profile))
    {
        return Err("codec is locked by admin".to_string());
    }
    if !lockout.allow_talk_mode && talk_mode.is_some_and(|value| value != baseline.talk_mode) {
        return Err("talk mode is locked by admin".to_string());
    }
    if !lockout.allow_priority && priority.is_some_and(|value| value != baseline.priority) {
        return Err("priority is locked by admin".to_string());
    }
    if !lockout.allow_priority
        && priority_channels.is_some_and(|value| !same_channels(value, &baseline.priority_channels))
    {
        return Err("priority is locked by admin".to_string());
    }
    if !lockout.allow_buttons
        && buttons.is_some_and(|value| normalize_button_configs(value.clone()) != baseline.buttons)
    {
        return Err("buttons are locked by admin".to_string());
    }
    if !lockout.allow_ifb
        && ifb.is_some_and(|value| normalize_ifb_config(value.clone()) != baseline.ifb)
    {
        return Err("IFB is locked by admin".to_string());
    }
    Ok(())
}

fn same_channels(left: &[ChannelId], right: &[ChannelId]) -> bool {
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort_unstable();
    right.sort_unstable();
    left == right
}

async fn apply_desired_client(
    state: &ServerState,
    mut desired: DesiredClientConfig,
) -> ControlResponse {
    desired.client_uid = desired
        .client_uid
        .map(|uid| normalize_client_uid(&uid, Some(desired.user_id)));
    desired.priority_channels = sorted_unique_channels(desired.priority_channels);

    let admin_snapshot = {
        let mut admin_state = state.admin_state.write().await;
        if let Some(client_uid) = &desired.client_uid {
            let now = unix_time_ms();
            let client_uid = normalize_client_uid(client_uid, Some(desired.user_id));
            if let Some(device) = admin_state
                .devices
                .iter_mut()
                .find(|device| device.client_uid == client_uid)
            {
                device.user_id = desired.user_id;
                device.status = EnrollmentStatus::Enrolled;
                device.role = desired.role;
                if !desired.name.is_empty() {
                    device.name = desired.name.clone();
                }
                device.last_seen_ms = now;
            } else {
                admin_state.devices.push(DeviceEnrollment {
                    client_uid,
                    user_id: desired.user_id,
                    status: EnrollmentStatus::Enrolled,
                    name: desired.name.clone(),
                    role: desired.role,
                    first_seen_ms: now,
                    last_seen_ms: now,
                    hardware_fingerprint: None,
                    warnings: Vec::new(),
                });
            }
            normalize_device_enrollments(&mut admin_state.devices);
        }
        upsert_desired_client(&mut admin_state.clients, desired.clone());
        admin_state.clone()
    };

    if let Err(err) = save_admin_state(state, &admin_snapshot).await {
        return ControlResponse::Error {
            message: format!("save admin state: {err}"),
        };
    }

    apply_desired_to_live_session(state, &desired).await;
    push_config_update(state, desired.user_id).await;
    ControlResponse::Ack
}

fn upsert_desired_client(clients: &mut Vec<DesiredClientConfig>, desired: DesiredClientConfig) {
    if let Some(existing) = clients
        .iter_mut()
        .find(|client| client.user_id == desired.user_id)
    {
        *existing = desired;
    } else {
        clients.push(desired);
    }
    clients.sort_by_key(|client| client.user_id);
}

fn normalize_preset_id(preset_id: &str) -> Result<String, AdminApiError> {
    normalize_config_id(preset_id, "preset")
}

fn normalize_config_id(id: &str, kind: &str) -> Result<String, AdminApiError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(AdminApiError::BadRequest(format!(
            "{kind} id cannot be empty"
        )));
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(AdminApiError::BadRequest(format!(
            "{kind} id may only contain letters, numbers, dashes, and underscores"
        )));
    }
    Ok(id.to_string())
}

fn normalize_preset_clients(clients: Vec<DesiredClientConfig>) -> Vec<DesiredClientConfig> {
    let mut normalized = clients
        .into_iter()
        .map(|mut client| {
            client.client_uid = client
                .client_uid
                .map(|uid| normalize_client_uid(&uid, Some(client.user_id)));
            client.buttons = normalize_button_configs(client.buttons);
            client.ifb = normalize_ifb_config(client.ifb);
            client.talker_vol = normalize_talker_volumes(client.talker_vol, client.user_id);
            client.stereo = normalize_stereo_config(client.stereo);
            client.esp32_audio = normalize_esp32_audio_config(client.esp32_audio);
            client.priority_channels = sorted_unique_channels(client.priority_channels);
            client
        })
        .collect::<Vec<_>>();
    normalized.sort_by_key(|client| client.user_id);
    normalized
}

fn normalize_template_client(mut client: ClientTemplateClientConfig) -> ClientTemplateClientConfig {
    client.buttons = normalize_button_configs(client.buttons);
    client.ifb = normalize_ifb_config(client.ifb);
    client.talker_vol = normalize_template_talker_volumes(client.talker_vol);
    client.stereo = normalize_stereo_config(client.stereo);
    client.esp32_audio = normalize_esp32_audio_config(client.esp32_audio);
    client.priority_channels = sorted_unique_channels(client.priority_channels);
    client
}

fn normalize_template_talker_volumes(volumes: HashMap<UserId, f32>) -> HashMap<UserId, f32> {
    volumes
        .into_iter()
        .filter_map(|(talker_id, gain)| {
            if !gain.is_finite() {
                return None;
            }
            Some((talker_id, gain.clamp(0.0, 4.0)))
        })
        .collect()
}

fn upsert_preset(presets: &mut Vec<PresetConfig>, preset: PresetConfig) {
    if let Some(existing) = presets.iter_mut().find(|existing| existing.id == preset.id) {
        *existing = preset;
    } else {
        presets.push(preset);
    }
    presets.sort_by(|a, b| a.id.cmp(&b.id));
}

fn upsert_template(templates: &mut Vec<ClientTemplateConfig>, template: ClientTemplateConfig) {
    if let Some(existing) = templates
        .iter_mut()
        .find(|existing| existing.id == template.id)
    {
        *existing = template;
    } else {
        templates.push(template);
    }
    templates.sort_by(|a, b| a.id.cmp(&b.id));
}

async fn apply_preset(state: &ServerState, preset: &PresetConfig) -> Result<(), AdminApiError> {
    let clients = normalize_preset_clients(preset.clients.clone());
    let admin_snapshot = {
        let mut admin_state = state.admin_state.write().await;
        for desired in &clients {
            upsert_desired_client(&mut admin_state.clients, desired.clone());
        }
        admin_state.clone()
    };
    save_admin_state(state, &admin_snapshot).await?;

    for desired in &clients {
        apply_desired_to_live_session(state, desired).await;
        push_config_update(state, desired.user_id).await;
    }

    Ok(())
}

async fn template_desired_for_user(
    state: &ServerState,
    template_id: &str,
    user_id: UserId,
) -> Result<DesiredClientConfig, AdminApiError> {
    let template_id = normalize_config_id(template_id, "template")?;
    let template = {
        let admin_state = state.admin_state.read().await;
        admin_state
            .templates
            .iter()
            .find(|template| template.id == template_id)
            .cloned()
    };
    let Some(template) = template else {
        return Err(AdminApiError::BadRequest(format!(
            "unknown template `{template_id}`"
        )));
    };
    Ok(template.client.to_desired(user_id))
}

async fn apply_desired_to_live_session(state: &ServerState, desired: &DesiredClientConfig) {
    let mut sessions = state.sessions.write().await;
    let session = sessions.entry(desired.user_id).or_insert_with(Session::new);
    apply_desired_to_session_fields(desired, session);
    session.last_seen = Instant::now();
}

async fn apply_desired_to_live_session_if_any(state: &ServerState, user_id: UserId) {
    if let Some(desired) = desired_client(state, user_id).await {
        apply_desired_to_live_session(state, &desired).await;
    }
}

fn apply_desired_to_session_fields(desired: &DesiredClientConfig, session: &mut Session) {
    if let Some(client_uid) = &desired.client_uid {
        session.client_uid = client_uid.clone();
    }
    session.role = desired.role;
    session.listen_channels = desired.listen.iter().copied().collect();
    session.tx_channels = desired.tx.iter().copied().collect();
    session.channel_volumes = desired.vol.clone();
    session.talker_volumes = normalize_talker_volumes(desired.talker_vol.clone(), desired.user_id);
    session.buttons = normalize_button_configs(desired.buttons.clone());
    session.ifb = normalize_ifb_config(desired.ifb.clone());
    session.lockout = desired.lockout.clone();
    session.stereo = normalize_stereo_config(desired.stereo.clone());
    session.esp32_audio = normalize_esp32_audio_config(desired.esp32_audio.clone());
    session.processing = desired.processing.clone();
    session.name = desired.name.clone();
    let configured_ids = session
        .buttons
        .iter()
        .map(|button| button.id.as_str())
        .collect::<HashSet<_>>();
    session
        .active_buttons
        .retain(|button_id| configured_ids.contains(button_id.as_str()));
    session.output_codec = if session.supported_codecs.contains(&desired.codec) {
        desired.codec
    } else {
        Codec::Pcm16
    };
    session.opus_profile = desired.opus_profile;
    session.talk_mode = desired.talk_mode;
    session.priority = desired.priority;
    session.priority_channels = desired.priority_channels.iter().copied().collect();
}

fn apply_default_operator_channels_to_session(session: &mut Session) {
    if session.role != ClientRole::Client {
        return;
    }
    if session.listen_channels.is_empty() {
        session.listen_channels.insert(CHANNEL_OPEN);
    }
    if session.tx_channels.is_empty() {
        session.tx_channels.insert(CHANNEL_OPEN);
    }
}

async fn apply_regular_talk_event(
    state: &ServerState,
    user_id: UserId,
    active: bool,
) -> ControlResponse {
    let mut sessions = state.sessions.write().await;
    let Some(session) = sessions.get_mut(&user_id) else {
        return ControlResponse::Error {
            message: format!("unknown client {user_id}"),
        };
    };
    session.regular_talk_active = active;
    session.last_seen = Instant::now();
    drop(sessions);
    push_config_update(state, user_id).await;
    ControlResponse::Ack
}

async fn apply_emergency_event(
    state: &ServerState,
    user_id: UserId,
    active: bool,
    target: EmergencyTarget,
    duck_gain: f32,
    mute_others: bool,
) -> ControlResponse {
    if active && !state.sessions.read().await.contains_key(&user_id) {
        return ControlResponse::Error {
            message: format!("unknown client {user_id}"),
        };
    }
    let mut affected = {
        let sessions = state.sessions.read().await;
        sessions.keys().copied().collect::<Vec<_>>()
    };
    {
        let mut emergency = state.emergency.write().await;
        if active {
            *emergency = Some(RuntimeEmergency {
                source: user_id,
                target: normalize_emergency_target(target),
                duck_gain: normalize_duck_gain(duck_gain),
                mute_others,
            });
        } else if emergency
            .as_ref()
            .is_some_and(|current| current.source == user_id || user_id == SERVER_USER_ID)
        {
            *emergency = None;
        } else if emergency.is_some() {
            return ControlResponse::Error {
                message: format!("client {user_id} cannot clear the active emergency"),
            };
        }
    }
    affected.push(user_id);
    affected.sort_unstable();
    affected.dedup();
    for user_id in affected {
        push_config_update(state, user_id).await;
    }
    ControlResponse::Ack
}

async fn apply_button_event(
    state: &ServerState,
    user_id: UserId,
    button_id: ButtonId,
    pressed: bool,
) -> ControlResponse {
    let (button, became_active, became_inactive) = {
        let mut sessions = state.sessions.write().await;
        let Some(session) = sessions.get_mut(&user_id) else {
            return ControlResponse::Error {
                message: format!("unknown client {user_id}"),
            };
        };
        let Some(button) = session
            .buttons
            .iter()
            .find(|button| button.id == button_id)
            .cloned()
        else {
            return ControlResponse::Error {
                message: format!("unknown button `{button_id}` for client {user_id}"),
            };
        };
        let was_active = session.active_buttons.contains(&button_id);
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

        if now_active {
            session.active_buttons.insert(button_id.clone());
        } else {
            session.active_buttons.remove(&button_id);
        }
        session.last_seen = Instant::now();
        (button, !was_active && now_active, was_active && !now_active)
    };

    if became_active {
        start_button_transmit_routes(state, user_id, &button).await;
        if let ControlResponse::Error { message } =
            execute_button_activation_actions(state, user_id, &button).await
        {
            return ControlResponse::Error { message };
        }
    } else if became_inactive {
        stop_button_transmit_routes(state, user_id, &button).await;
    }

    push_config_update(state, user_id).await;
    ControlResponse::Ack
}

async fn start_button_transmit_routes(
    state: &ServerState,
    user_id: UserId,
    button: &TalkButtonConfig,
) {
    let now = Instant::now();
    let mut targets = Vec::new();
    {
        let mut sessions = state.sessions.write().await;
        for action in &button.actions {
            let TalkButtonAction::Transmit { users, duck, .. } = action else {
                continue;
            };
            for &target_user_id in users {
                if user_id == target_user_id || !sessions.contains_key(&target_user_id) {
                    continue;
                }
                if let Some(caller) = sessions.get_mut(&user_id) {
                    caller.direct_call_history.push(DirectCallHistory {
                        caller: user_id,
                        target: target_user_id,
                        started: now,
                        ended: None,
                        duck: *duck,
                        source_button: Some(button.id.clone()),
                    });
                }
                if let Some(target) = sessions.get_mut(&target_user_id) {
                    target.last_direct_caller = Some(user_id);
                    target.direct_call_history.push(DirectCallHistory {
                        caller: user_id,
                        target: target_user_id,
                        started: now,
                        ended: None,
                        duck: *duck,
                        source_button: Some(button.id.clone()),
                    });
                }
                targets.push(target_user_id);
            }
        }
        trim_direct_history(&mut sessions, user_id);
        for target in &targets {
            trim_direct_history(&mut sessions, *target);
        }
    }
    for target in targets {
        push_config_update(state, target).await;
    }
}

async fn stop_button_transmit_routes(
    state: &ServerState,
    user_id: UserId,
    button: &TalkButtonConfig,
) {
    let now = Instant::now();
    let mut targets = Vec::new();
    {
        let mut sessions = state.sessions.write().await;
        for action in &button.actions {
            let TalkButtonAction::Transmit { users, .. } = action else {
                continue;
            };
            for &target_user_id in users {
                close_button_direct_history(
                    &mut sessions,
                    user_id,
                    target_user_id,
                    &button.id,
                    now,
                );
                targets.push(target_user_id);
            }
        }
    }
    for target in targets {
        push_config_update(state, target).await;
    }
}

fn close_button_direct_history(
    sessions: &mut HashMap<UserId, Session>,
    user_id: UserId,
    target_user_id: UserId,
    button_id: &str,
    now: Instant,
) {
    for session_id in [user_id, target_user_id] {
        if let Some(session) = sessions.get_mut(&session_id) {
            if let Some(entry) = session.direct_call_history.iter_mut().rev().find(|entry| {
                entry.caller == user_id
                    && entry.target == target_user_id
                    && entry.source_button.as_deref() == Some(button_id)
                    && entry.ended.is_none()
            }) {
                entry.ended = Some(now);
            }
        }
    }
}

async fn execute_button_activation_actions(
    state: &ServerState,
    user_id: UserId,
    button: &TalkButtonConfig,
) -> ControlResponse {
    for action in &button.actions {
        match action {
            TalkButtonAction::Transmit { .. } => {}
            TalkButtonAction::Alert { targets, message } => {
                for target in targets {
                    if let ControlResponse::Error { message } =
                        apply_send_alert_event(state, user_id, *target, message.clone()).await
                    {
                        return ControlResponse::Error { message };
                    }
                }
            }
            TalkButtonAction::ApplyPreset { preset_id } => {
                let preset = {
                    let admin_state = state.admin_state.read().await;
                    admin_state
                        .presets
                        .iter()
                        .find(|preset| preset.id == *preset_id)
                        .cloned()
                };
                let Some(preset) = preset else {
                    return ControlResponse::Error {
                        message: format!("unknown preset `{preset_id}`"),
                    };
                };
                if let Err(err) = apply_preset(state, &preset).await {
                    let message = match err {
                        AdminApiError::BadRequest(message) | AdminApiError::Internal(message) => {
                            message
                        }
                    };
                    return ControlResponse::Error { message };
                }
            }
            TalkButtonAction::SetTalkMode { users, mode } => {
                let targets = action_users_or_self(users, user_id);
                for target_user_id in targets {
                    let mut desired = desired_baseline_for_user(state, target_user_id).await;
                    desired.talk_mode = *mode;
                    if let ControlResponse::Error { message } =
                        apply_desired_client(state, desired).await
                    {
                        return ControlResponse::Error { message };
                    }
                }
            }
            TalkButtonAction::RouteEdit {
                users,
                listen_add,
                listen_remove,
                listen_toggle,
                tx_add,
                tx_remove,
                tx_toggle,
            } => {
                let targets = action_users_or_self(users, user_id);
                for target_user_id in targets {
                    let mut desired = desired_baseline_for_user(state, target_user_id).await;
                    apply_route_edits(
                        &mut desired.listen,
                        listen_add,
                        listen_remove,
                        listen_toggle,
                    );
                    apply_route_edits(&mut desired.tx, tx_add, tx_remove, tx_toggle);
                    if let ControlResponse::Error { message } =
                        apply_desired_client(state, desired).await
                    {
                        return ControlResponse::Error { message };
                    }
                }
            }
        }
    }
    ControlResponse::Ack
}

fn action_users_or_self(users: &[UserId], user_id: UserId) -> Vec<UserId> {
    if users.is_empty() {
        vec![user_id]
    } else {
        users.to_vec()
    }
}

fn apply_route_edits(
    routes: &mut Vec<ChannelId>,
    add: &[ChannelId],
    remove: &[ChannelId],
    toggle: &[ChannelId],
) {
    let mut set = routes.iter().copied().collect::<HashSet<_>>();
    set.extend(add.iter().copied());
    for channel in remove {
        set.remove(channel);
    }
    for channel in toggle {
        if !set.remove(channel) {
            set.insert(*channel);
        }
    }
    *routes = set.into_iter().collect::<Vec<_>>();
    routes.sort_unstable();
}

async fn apply_send_alert_event(
    state: &ServerState,
    user_id: UserId,
    target: AlertTarget,
    message: Option<String>,
) -> ControlResponse {
    let recipients = resolve_alert_recipients(state, user_id, target).await;
    if recipients.is_empty() {
        return ControlResponse::Error {
            message: "alert has no live recipients".to_string(),
        };
    }

    let alert = create_runtime_alert(state, user_id, target, message, &recipients).await;
    for recipient in recipients {
        push_config_update(state, recipient).await;
    }
    push_config_update(state, user_id).await;
    tracing::info!(
        alert_id = alert.id,
        sender = user_id,
        target = ?target,
        "created alert"
    );
    ControlResponse::Ack
}

async fn create_runtime_alert(
    state: &ServerState,
    user_id: UserId,
    target: AlertTarget,
    message: Option<String>,
    recipients: &[UserId],
) -> AlertStatus {
    let alert_id = state.next_alert_id.fetch_add(1, Ordering::Relaxed);
    let recipient_statuses = recipients
        .iter()
        .copied()
        .map(|recipient| AlertRecipientStatus {
            user_id: recipient,
            acked_at_ms: None,
        })
        .collect::<Vec<_>>();
    let alert = {
        let sessions = state.sessions.read().await;
        let mut alerts = state.alerts.write().await;
        let alert = RuntimeAlert {
            id: alert_id,
            sender: user_id,
            target,
            message: normalize_optional_message(message),
            created_at_ms: unix_time_ms(),
            recipients: recipient_statuses,
            cancelled: false,
            cancelled_at_ms: None,
        };
        let status = alert_status_with_sessions(&alert, &sessions);
        alerts.push(alert);
        trim_alert_history(&mut alerts);
        status
    };
    alert
}

async fn resolve_alert_recipients(
    state: &ServerState,
    sender: UserId,
    target: AlertTarget,
) -> Vec<UserId> {
    let sessions = state.sessions.read().await;
    let control_clients = state.control_clients.read().await;
    let mut recipients = match target {
        AlertTarget::User(user_id) => {
            if user_id != sender
                && sessions.contains_key(&user_id)
                && (control_clients.contains_key(&user_id)
                    || sessions
                        .get(&user_id)
                        .is_some_and(|session| session.addr.is_some()))
            {
                vec![user_id]
            } else {
                Vec::new()
            }
        }
        AlertTarget::Channel(channel_id) => sessions
            .iter()
            .filter_map(|(&user_id, session)| {
                if user_id != sender
                    && session.listen_channels.contains(&channel_id)
                    && (control_clients.contains_key(&user_id) || session.addr.is_some())
                {
                    Some(user_id)
                } else {
                    None
                }
            })
            .collect(),
    };
    recipients.sort_unstable();
    recipients.dedup();
    recipients
}

async fn apply_tts_announcement_event(
    state: &Arc<ServerState>,
    request: AdminTtsRequest,
) -> Result<TtsAnnouncementResponse, AdminApiError> {
    apply_announcement_event(
        state,
        AdminAnnouncementRequest {
            sender: request.sender,
            targets: request.targets,
            target: request.target,
            message: request.message,
            text_alert: true,
            tts: true,
            priority: request.priority,
            duck: request.duck,
            gain: request.gain,
        },
    )
    .await
}

async fn apply_announcement_event(
    state: &Arc<ServerState>,
    request: AdminAnnouncementRequest,
) -> Result<TtsAnnouncementResponse, AdminApiError> {
    let sender = request.sender;
    let message = normalize_tts_message(&request.message)?;
    if !request.text_alert && !request.tts {
        return Err(AdminApiError::BadRequest(
            "enable at least text alert or spoken announcement".to_string(),
        ));
    }
    let mut targets = request.targets;
    if let Some(target) = request.target {
        targets.push(target);
    }
    let targets = sorted_unique_alert_targets(targets);
    if targets.is_empty() {
        return Err(AdminApiError::BadRequest(
            "announcement requires at least one live user or channel target".to_string(),
        ));
    }

    let mut recipients = Vec::new();
    let mut target_recipients = Vec::new();
    for target in &targets {
        let resolved = resolve_alert_recipients(state, sender, *target).await;
        if resolved.is_empty() {
            return Err(AdminApiError::BadRequest(format!(
                "announcement target {} has no live recipients",
                alert_target_label(*target)
            )));
        }
        recipients.extend(resolved.iter().copied());
        target_recipients.push((*target, resolved));
    }
    recipients.sort_unstable();
    recipients.dedup();

    let announcement_id = state.next_tts_id.fetch_add(1, Ordering::Relaxed);
    let mut alert_ids = Vec::new();

    if request.text_alert {
        for (target, resolved) in &target_recipients {
            let alert =
                create_runtime_alert(state, sender, *target, Some(message.clone()), resolved).await;
            alert_ids.push(alert.id);
        }
    }

    let mut engine = None;
    let mut duration_ms = None;
    if request.tts {
        let gain = request.gain.clamp(0.02, 1.0);
        let synthesis = synthesize_tts_audio(announcement_id, &message, gain).await?;
        duration_ms = Some(synthesis.frames.len() as u64 * common::FRAME_MS as u64);
        engine = Some(synthesis.engine);
        for (target_index, target) in targets.iter().enumerate() {
            let source_id = tts_source_user_id(announcement_id, target_index);
            let audio_target = match target {
                AlertTarget::User(user_id) => AudioTarget::Direct(*user_id),
                AlertTarget::Channel(channel_id) => AudioTarget::Channel(*channel_id),
            };
            start_tts_source(
                Arc::clone(state),
                source_id,
                audio_target,
                synthesis.frames.clone(),
                request.priority,
                request.duck,
            )
            .await;
        }
    }

    let mut notify_users = recipients.clone();
    notify_users.push(sender);
    notify_users.sort_unstable();
    notify_users.dedup();
    for user_id in notify_users {
        push_config_update(state, user_id).await;
    }

    tracing::info!(
        announcement_id,
        sender,
        targets = ?targets,
        recipients = ?recipients,
        ?duration_ms,
        ?engine,
        text_alert = request.text_alert,
        tts = request.tts,
        "queued announcement"
    );

    Ok(TtsAnnouncementResponse {
        ok: true,
        id: announcement_id,
        sender,
        targets,
        recipients,
        message,
        engine,
        duration_ms,
        alert_ids,
    })
}

fn normalize_tts_message(message: &str) -> Result<String, AdminApiError> {
    let message = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if message.is_empty() {
        return Err(AdminApiError::BadRequest(
            "announcement message cannot be empty".to_string(),
        ));
    }
    let mut normalized = message
        .chars()
        .take(TTS_MAX_MESSAGE_CHARS)
        .collect::<String>();
    if message.chars().count() > TTS_MAX_MESSAGE_CHARS {
        normalized.push_str("...");
    }
    Ok(normalized)
}

fn alert_target_label(target: AlertTarget) -> String {
    match target {
        AlertTarget::User(user_id) => format!("user {user_id}"),
        AlertTarget::Channel(channel_id) => format!("channel {channel_id}"),
    }
}

fn tts_source_user_id(announcement_id: u64, target_index: usize) -> UserId {
    TTS_SOURCE_USER_BASE
        + (((announcement_id as usize * 31 + target_index) % TTS_SOURCE_USER_COUNT as usize)
            as UserId)
}

#[cfg(all(not(test), feature = "tts-supertonic"))]
async fn synthesize_tts_audio(
    _announcement_id: u64,
    message: &str,
    gain: f32,
) -> Result<TtsSynthesis, AdminApiError> {
    let frames = supertonic_tts::synthesize(message, gain)
        .await
        .map_err(|err| AdminApiError::Internal(format!("Supertonic TTS failed: {err:#}")))?;
    Ok(TtsSynthesis {
        engine: supertonic_tts::ENGINE_NAME,
        frames,
    })
}

#[cfg(all(not(test), not(feature = "tts-supertonic")))]
async fn synthesize_tts_audio(
    _announcement_id: u64,
    _message: &str,
    _gain: f32,
) -> Result<TtsSynthesis, AdminApiError> {
    Err(AdminApiError::BadRequest(
        "server was built without tts-supertonic".to_string(),
    ))
}

#[cfg(test)]
async fn synthesize_tts_audio(
    _announcement_id: u64,
    _message: &str,
    _gain: f32,
) -> Result<TtsSynthesis, AdminApiError> {
    Ok(TtsSynthesis {
        engine: "supertonic",
        frames: vec![vec![1_000; MIX_SAMPLES_PER_FRAME]; 12],
    })
}

async fn start_tts_source(
    state: Arc<ServerState>,
    source_id: UserId,
    target: AudioTarget,
    frames: Vec<Vec<i16>>,
    priority: bool,
    duck: bool,
) {
    if frames.is_empty() {
        return;
    }
    state
        .virtual_sources
        .write()
        .await
        .push(VirtualAudioSource::new(
            source_id, target, frames, priority, duck,
        ));
}

async fn apply_ack_alert_event(
    state: &ServerState,
    user_id: UserId,
    alert_id: AlertId,
) -> ControlResponse {
    let now = unix_time_ms();
    let sender = {
        let mut alerts = state.alerts.write().await;
        let Some(alert) = alerts.iter_mut().find(|alert| alert.id == alert_id) else {
            return ControlResponse::Error {
                message: format!("unknown alert {alert_id}"),
            };
        };
        let Some(recipient) = alert
            .recipients
            .iter_mut()
            .find(|recipient| recipient.user_id == user_id)
        else {
            return ControlResponse::Error {
                message: format!("alert {alert_id} is not assigned to client {user_id}"),
            };
        };
        recipient.acked_at_ms = Some(now);
        let sender = alert.sender;
        trim_alert_history(&mut alerts);
        sender
    };
    push_config_update(state, user_id).await;
    push_config_update(state, sender).await;
    ControlResponse::Ack
}

async fn apply_cancel_alert_event(
    state: &ServerState,
    user_id: UserId,
    alert_id: AlertId,
) -> ControlResponse {
    let now = unix_time_ms();
    let mut affected = {
        let mut alerts = state.alerts.write().await;
        let Some(alert) = alerts.iter_mut().find(|alert| alert.id == alert_id) else {
            return ControlResponse::Error {
                message: format!("unknown alert {alert_id}"),
            };
        };
        if user_id != SERVER_USER_ID && alert.sender != user_id {
            return ControlResponse::Error {
                message: format!("client {user_id} cannot cancel alert {alert_id}"),
            };
        }
        alert.cancelled = true;
        alert.cancelled_at_ms = Some(now);
        let mut affected = vec![alert.sender];
        affected.extend(alert.recipients.iter().map(|recipient| recipient.user_id));
        trim_alert_history(&mut alerts);
        affected
    };
    affected.sort_unstable();
    affected.dedup();
    for user_id in affected {
        push_config_update(state, user_id).await;
    }
    ControlResponse::Ack
}

fn trim_alert_history(alerts: &mut Vec<RuntimeAlert>) {
    const MAX_INACTIVE_ALERTS: usize = 50;
    let inactive = alerts.iter().filter(|alert| !alert.active()).count();
    if inactive <= MAX_INACTIVE_ALERTS {
        return;
    }
    let mut remove_count = inactive - MAX_INACTIVE_ALERTS;
    alerts.retain(|alert| {
        if remove_count > 0 && !alert.active() {
            remove_count -= 1;
            false
        } else {
            true
        }
    });
}

async fn apply_direct_call_event(
    state: &ServerState,
    user_id: UserId,
    target_user_id: UserId,
    active: bool,
    duck: bool,
) -> ControlResponse {
    if user_id == target_user_id {
        return ControlResponse::Error {
            message: "cannot direct-call yourself".to_string(),
        };
    }

    let now = Instant::now();
    let mut sessions = state.sessions.write().await;
    if !sessions.contains_key(&target_user_id) {
        return ControlResponse::Error {
            message: format!("unknown target client {target_user_id}"),
        };
    }

    let caller = sessions.entry(user_id).or_insert_with(Session::new);
    if active {
        caller
            .active_direct_calls
            .insert(target_user_id, ActiveDirectCall { duck });
        caller.direct_call_history.push(DirectCallHistory {
            caller: user_id,
            target: target_user_id,
            started: now,
            ended: None,
            duck,
            source_button: None,
        });
    } else {
        caller.active_direct_calls.remove(&target_user_id);
        if let Some(entry) = caller.direct_call_history.iter_mut().rev().find(|entry| {
            entry.caller == user_id && entry.target == target_user_id && entry.ended.is_none()
        }) {
            entry.ended = Some(now);
        }
    }
    caller.last_seen = now;

    if let Some(target) = sessions.get_mut(&target_user_id) {
        if active {
            target.last_direct_caller = Some(user_id);
            target.direct_call_history.push(DirectCallHistory {
                caller: user_id,
                target: target_user_id,
                started: now,
                ended: None,
                duck,
                source_button: None,
            });
        } else if let Some(entry) = target.direct_call_history.iter_mut().rev().find(|entry| {
            entry.caller == user_id && entry.target == target_user_id && entry.ended.is_none()
        }) {
            entry.ended = Some(now);
        }
    }
    trim_direct_history(&mut sessions, user_id);
    trim_direct_history(&mut sessions, target_user_id);
    drop(sessions);

    push_config_update(state, user_id).await;
    push_config_update(state, target_user_id).await;
    ControlResponse::Ack
}

async fn apply_reply_call_event(
    state: &ServerState,
    user_id: UserId,
    active: bool,
    duck: bool,
) -> ControlResponse {
    let target_user_id = {
        let sessions = state.sessions.read().await;
        let Some(session) = sessions.get(&user_id) else {
            return ControlResponse::Error {
                message: format!("unknown client {user_id}"),
            };
        };
        let Some(last_direct_caller) = session.last_direct_caller else {
            return ControlResponse::Error {
                message: "no last direct caller to reply to".to_string(),
            };
        };
        last_direct_caller
    };

    apply_direct_call_event(state, user_id, target_user_id, active, duck).await
}

fn trim_direct_history(sessions: &mut HashMap<UserId, Session>, user_id: UserId) {
    const MAX_DIRECT_CALL_HISTORY: usize = 20;
    if let Some(session) = sessions.get_mut(&user_id) {
        let excess = session
            .direct_call_history
            .len()
            .saturating_sub(MAX_DIRECT_CALL_HISTORY);
        if excess > 0 {
            session.direct_call_history.drain(0..excess);
        }
    }
}

async fn clear_active_buttons(state: &ServerState, user_id: UserId) {
    let now = Instant::now();
    let mut sessions = state.sessions.write().await;
    let mut targets = sessions
        .get(&user_id)
        .map(|session| {
            session
                .active_direct_calls
                .keys()
                .copied()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let direct_call_targets = targets.clone();
    let active_buttons = sessions
        .get(&user_id)
        .map(|session| {
            session
                .buttons
                .iter()
                .filter(|button| session.active_buttons.contains(&button.id))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for button in &active_buttons {
        for action in &button.actions {
            let TalkButtonAction::Transmit { users, .. } = action else {
                continue;
            };
            for &target_user_id in users {
                close_button_direct_history(
                    &mut sessions,
                    user_id,
                    target_user_id,
                    &button.id,
                    now,
                );
                targets.push(target_user_id);
            }
        }
    }
    for target_user_id in &direct_call_targets {
        close_direct_call_history(&mut sessions, user_id, *target_user_id, now);
    }
    if let Some(session) = sessions.get_mut(&user_id) {
        session.active_buttons.clear();
        session.regular_talk_active = false;
        session.active_direct_calls.clear();
    }
    drop(sessions);
    targets.sort_unstable();
    targets.dedup();
    for target in targets {
        push_config_update(state, target).await;
    }
    push_config_update(state, user_id).await;
}

fn close_direct_call_history(
    sessions: &mut HashMap<UserId, Session>,
    user_id: UserId,
    target_user_id: UserId,
    now: Instant,
) {
    for session_id in [user_id, target_user_id] {
        if let Some(session) = sessions.get_mut(&session_id) {
            if let Some(entry) = session.direct_call_history.iter_mut().rev().find(|entry| {
                entry.caller == user_id
                    && entry.target == target_user_id
                    && entry.source_button.is_none()
                    && entry.ended.is_none()
            }) {
                entry.ended = Some(now);
            }
        }
    }
}

async fn clear_emergency_for_source(state: &ServerState, user_id: UserId) {
    let cleared = {
        let mut emergency = state.emergency.write().await;
        if emergency
            .as_ref()
            .is_none_or(|current| current.source != user_id)
        {
            return;
        }
        *emergency = None;
        true
    };
    if cleared {
        let sessions = state.sessions.read().await;
        let affected = sessions.keys().copied().collect::<Vec<_>>();
        drop(sessions);
        for affected_user in affected {
            push_config_update(state, affected_user).await;
        }
    }
}

fn normalize_button_capabilities(mut buttons: Vec<ButtonCapability>) -> Vec<ButtonCapability> {
    buttons.retain(|button| !button.id.trim().is_empty());
    for button in &mut buttons {
        button.id = button.id.trim().to_string();
        if button.label.trim().is_empty() {
            button.label = button.id.clone();
        } else {
            button.label = button.label.trim().to_string();
        }
    }
    buttons.sort_by(|left, right| compare_button_ids(&left.id, &right.id));
    buttons.dedup_by(|left, right| left.id == right.id);
    buttons
}

fn compare_button_ids(left: &str, right: &str) -> std::cmp::Ordering {
    match (left.parse::<u16>(), right.parse::<u16>()) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => left.cmp(right),
    }
}

fn normalize_talker_volumes(
    volumes: HashMap<UserId, f32>,
    listener_user_id: UserId,
) -> HashMap<UserId, f32> {
    volumes
        .into_iter()
        .filter_map(|(talker_id, gain)| {
            if talker_id == listener_user_id || !gain.is_finite() {
                return None;
            }
            Some((talker_id, gain.clamp(0.0, 4.0)))
        })
        .collect()
}

fn normalize_button_configs(mut buttons: Vec<TalkButtonConfig>) -> Vec<TalkButtonConfig> {
    buttons.retain(|button| !button.id.trim().is_empty());
    for button in &mut buttons {
        button.id = button.id.trim().to_string();
        if button.label.trim().is_empty() {
            button.label = button.id.clone();
        } else {
            button.label = button.label.trim().to_string();
        }
        button.color = normalize_button_color(button.color.take());
        button.actions = normalize_button_actions(std::mem::take(&mut button.actions));
    }
    buttons.sort_by(|left, right| compare_button_ids(&left.id, &right.id));
    buttons.dedup_by(|left, right| left.id == right.id);
    buttons
}

fn normalize_button_color(color: Option<String>) -> Option<String> {
    let color = color?.trim().to_string();
    if is_hex_button_color(&color) {
        Some(color)
    } else {
        None
    }
}

fn is_hex_button_color(color: &str) -> bool {
    let bytes = color.as_bytes();
    if bytes.first() != Some(&b'#') || (bytes.len() != 4 && bytes.len() != 7) {
        return false;
    }
    bytes[1..].iter().all(u8::is_ascii_hexdigit)
}

fn sorted_buttons(buttons: &[TalkButtonConfig]) -> Vec<TalkButtonConfig> {
    normalize_button_configs(buttons.to_vec())
}

fn normalize_button_actions(actions: Vec<TalkButtonAction>) -> Vec<TalkButtonAction> {
    actions
        .into_iter()
        .map(|action| match action {
            TalkButtonAction::Transmit {
                channels,
                users,
                duck,
            } => TalkButtonAction::Transmit {
                channels: sorted_unique_channels(channels),
                users: sorted_unique_users(users),
                duck,
            },
            TalkButtonAction::Alert { targets, message } => TalkButtonAction::Alert {
                targets: sorted_unique_alert_targets(targets),
                message: normalize_optional_message(message),
            },
            TalkButtonAction::ApplyPreset { preset_id } => TalkButtonAction::ApplyPreset {
                preset_id: preset_id.trim().to_string(),
            },
            TalkButtonAction::SetTalkMode { users, mode } => TalkButtonAction::SetTalkMode {
                users: sorted_unique_users(users),
                mode,
            },
            TalkButtonAction::RouteEdit {
                users,
                listen_add,
                listen_remove,
                listen_toggle,
                tx_add,
                tx_remove,
                tx_toggle,
            } => TalkButtonAction::RouteEdit {
                users: sorted_unique_users(users),
                listen_add: sorted_unique_channels(listen_add),
                listen_remove: sorted_unique_channels(listen_remove),
                listen_toggle: sorted_unique_channels(listen_toggle),
                tx_add: sorted_unique_channels(tx_add),
                tx_remove: sorted_unique_channels(tx_remove),
                tx_toggle: sorted_unique_channels(tx_toggle),
            },
        })
        .collect()
}

fn normalize_optional_message(message: Option<String>) -> Option<String> {
    message
        .map(|message| message.trim().to_string())
        .filter(|message| !message.is_empty())
}

fn sorted_unique_channels(mut channels: Vec<ChannelId>) -> Vec<ChannelId> {
    channels.sort_unstable();
    channels.dedup();
    channels
}

fn sorted_unique_users(mut users: Vec<UserId>) -> Vec<UserId> {
    users.retain(|user| *user > 0);
    users.sort_unstable();
    users.dedup();
    users
}

fn sorted_unique_alert_targets(mut targets: Vec<AlertTarget>) -> Vec<AlertTarget> {
    targets.retain(|target| match target {
        AlertTarget::User(user_id) => *user_id > 0,
        AlertTarget::Channel(_) => true,
    });
    targets.sort_by_key(|target| match target {
        AlertTarget::User(user_id) => (0, *user_id),
        AlertTarget::Channel(channel_id) => (1, *channel_id),
    });
    targets.dedup();
    targets
}

fn normalize_emergency_target(target: EmergencyTarget) -> EmergencyTarget {
    match target {
        EmergencyTarget::All => EmergencyTarget::All,
        EmergencyTarget::Users { users } => EmergencyTarget::Users {
            users: sorted_unique_users(users),
        },
        EmergencyTarget::Channels { channels } => EmergencyTarget::Channels {
            channels: sorted_unique_channels(channels),
        },
    }
}

fn normalize_duck_gain(gain: f32) -> f32 {
    if gain.is_finite() {
        gain.clamp(0.0, 1.0)
    } else {
        DEFAULT_IFB_DUCK_GAIN
    }
}

fn normalize_ifb_config(mut ifb: IfbConfig) -> IfbConfig {
    ifb.program.sort_unstable();
    ifb.program.dedup();
    ifb.interrupt.sort_unstable();
    ifb.interrupt.dedup();
    let interrupt = ifb.interrupt.iter().copied().collect::<HashSet<_>>();
    ifb.program.retain(|channel| !interrupt.contains(channel));
    ifb.duck_gain = if ifb.duck_gain.is_finite() {
        ifb.duck_gain.clamp(0.0, 1.0)
    } else {
        DEFAULT_IFB_DUCK_GAIN
    };
    ifb
}

fn normalize_stereo_config(mut stereo: StereoConfig) -> StereoConfig {
    stereo.channel_pan.retain(|_, pan| pan.is_finite());
    for pan in stereo.channel_pan.values_mut() {
        *pan = pan.clamp(-1.0, 1.0);
    }
    stereo
}

fn normalize_esp32_audio_config(mut config: Esp32AudioConfig) -> Esp32AudioConfig {
    config.mic_pga_gain_db = (config.mic_pga_gain_db / 3).min(8) * 3;
    config.mic_software_gain_percent = config.mic_software_gain_percent.min(400);
    config.speaker_software_gain_percent = config.speaker_software_gain_percent.min(400);
    config.notification_gain_percent = config.notification_gain_percent.min(200);
    config.sidetone.firmware_gain_percent = config.sidetone.firmware_gain_percent.min(200);
    config.sidetone.codec_bypass_gain_percent = config.sidetone.codec_bypass_gain_percent.min(200);
    config.sidetone.mic_bypass_gain_percent = config.sidetone.mic_bypass_gain_percent.min(400);
    config
}

async fn load_admin_state(path: &Path) -> anyhow::Result<PersistedAdminState> {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => {
            let mut state: PersistedAdminState = serde_json::from_str(&contents)
                .with_context(|| format!("parse admin state file {}", path.display()))?;
            normalize_admin_state(&mut state);
            Ok(state)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(PersistedAdminState::default())
        }
        Err(err) => Err(err).with_context(|| format!("read admin state file {}", path.display())),
    }
}

fn normalize_admin_state(state: &mut PersistedAdminState) {
    if state.channels.is_empty() {
        state.channels = default_workflow_channels();
    }
    ensure_open_channel(&mut state.channels);
    if state.presets.is_empty() {
        state.presets = default_workflow_presets();
    }
    if state.templates.is_empty() {
        state.templates = default_workflow_templates();
    }
    normalize_channels(&mut state.channels);
    normalize_device_enrollments(&mut state.devices);
    state.clients = normalize_preset_clients(std::mem::take(&mut state.clients));
    for preset in &mut state.presets {
        preset.id = preset.id.trim().to_string();
        preset.name = preset.name.trim().to_string();
        preset.clients = normalize_preset_clients(std::mem::take(&mut preset.clients));
    }
    state.presets.retain(|preset| !preset.id.is_empty());
    state.presets.sort_by(|left, right| left.id.cmp(&right.id));
    state.presets.dedup_by(|left, right| left.id == right.id);
    for template in &mut state.templates {
        template.id = template.id.trim().to_string();
        template.name = template.name.trim().to_string();
        template.client = normalize_template_client(std::mem::take(&mut template.client));
    }
    state.templates.retain(|template| !template.id.is_empty());
    state
        .templates
        .sort_by(|left, right| left.id.cmp(&right.id));
    state.templates.dedup_by(|left, right| left.id == right.id);
}

fn normalize_channels(channels: &mut Vec<ChannelConfig>) {
    for channel in channels.iter_mut() {
        channel.name = channel.name.trim().to_string();
    }
    channels.sort_by_key(|channel| channel.id);
    channels.dedup_by(|left, right| left.id == right.id);
}

fn ensure_open_channel(channels: &mut Vec<ChannelConfig>) {
    if let Some(channel) = channels
        .iter_mut()
        .find(|channel| channel.id == CHANNEL_OPEN)
    {
        if channel.name.trim().is_empty() {
            channel.name = "open".to_string();
        }
    } else {
        channels.push(channel_config(CHANNEL_OPEN, "open"));
    }
}

fn normalize_device_enrollments(devices: &mut Vec<DeviceEnrollment>) {
    devices.retain(|device| device.user_id > 0 && !device.client_uid.trim().is_empty());
    for device in devices.iter_mut() {
        device.client_uid = normalize_client_uid(&device.client_uid, Some(device.user_id));
        device.name = device.name.trim().to_string();
        device.warnings.sort();
        device.warnings.dedup();
    }
    devices.sort_by_key(|device| (device.user_id, device.client_uid.clone()));
    devices.dedup_by(|left, right| left.client_uid == right.client_uid);
}

fn normalize_client_uid(client_uid: &str, requested_user_id: Option<UserId>) -> ClientUid {
    let trimmed = client_uid.trim();
    if !trimmed.is_empty() {
        trimmed.to_string()
    } else if let Some(user_id) = requested_user_id.filter(|user_id| *user_id > 0) {
        format!("legacy-{user_id}")
    } else {
        "legacy-unassigned".to_string()
    }
}

fn next_available_user_id(admin_state: &PersistedAdminState, requested: Option<UserId>) -> UserId {
    let used = admin_state
        .devices
        .iter()
        .map(|device| device.user_id)
        .chain(admin_state.clients.iter().map(|client| client.user_id))
        .collect::<HashSet<_>>();
    if let Some(user_id) = requested.filter(|user_id| *user_id > 0) {
        if !used.contains(&user_id) {
            return user_id;
        }
    }
    (1..=UserId::MAX)
        .find(|user_id| !used.contains(user_id))
        .unwrap_or(UserId::MAX)
}

#[derive(Debug, Clone)]
struct EnrollmentDecision {
    user_id: UserId,
    client_uid: ClientUid,
    status: EnrollmentStatus,
    preconfigured: bool,
}

async fn resolve_client_enrollment(
    state: &ServerState,
    client_uid: String,
    requested_user_id: Option<UserId>,
    role: ClientRole,
) -> Result<EnrollmentDecision, String> {
    let now = unix_time_ms();
    let mut admin_state = state.admin_state.write().await;
    let client_uid = normalize_client_uid(&client_uid, requested_user_id);

    if let Some(device) = admin_state
        .devices
        .iter_mut()
        .find(|device| device.client_uid == client_uid)
    {
        device.last_seen_ms = now;
        device.role = role;
        let user_id = device.user_id;
        let status = device.status;
        let preconfigured = admin_state.clients.iter().any(|client| {
            client.user_id == user_id
                || client
                    .client_uid
                    .as_deref()
                    .is_some_and(|uid| uid == client_uid)
        });
        let snapshot = admin_state.clone();
        drop(admin_state);
        save_admin_state(state, &snapshot)
            .await
            .map_err(|err| format!("save admin state: {err}"))?;
        return Ok(EnrollmentDecision {
            user_id,
            client_uid,
            status,
            preconfigured,
        });
    }

    if let Some(client) = admin_state
        .clients
        .iter()
        .find(|client| client.client_uid.as_deref() == Some(client_uid.as_str()))
        .cloned()
    {
        let user_id = client.user_id;
        admin_state.devices.push(DeviceEnrollment {
            client_uid: client_uid.clone(),
            user_id,
            status: EnrollmentStatus::Enrolled,
            name: client.name,
            role,
            first_seen_ms: now,
            last_seen_ms: now,
            hardware_fingerprint: None,
            warnings: Vec::new(),
        });
        normalize_device_enrollments(&mut admin_state.devices);
        let snapshot = admin_state.clone();
        drop(admin_state);
        save_admin_state(state, &snapshot)
            .await
            .map_err(|err| format!("save admin state: {err}"))?;
        return Ok(EnrollmentDecision {
            user_id,
            client_uid,
            status: EnrollmentStatus::Enrolled,
            preconfigured: true,
        });
    }

    if let Some(requested) = requested_user_id.filter(|user_id| *user_id > 0) {
        let client_name = if let Some(client) = admin_state
            .clients
            .iter_mut()
            .find(|client| client.user_id == requested && client.client_uid.is_none())
        {
            client.client_uid = Some(client_uid.clone());
            Some(client.name.clone())
        } else {
            None
        };
        if let Some(client_name) = client_name {
            admin_state.devices.push(DeviceEnrollment {
                client_uid: client_uid.clone(),
                user_id: requested,
                status: EnrollmentStatus::Enrolled,
                name: client_name,
                role,
                first_seen_ms: now,
                last_seen_ms: now,
                hardware_fingerprint: None,
                warnings: Vec::new(),
            });
            normalize_device_enrollments(&mut admin_state.devices);
            let snapshot = admin_state.clone();
            drop(admin_state);
            save_admin_state(state, &snapshot)
                .await
                .map_err(|err| format!("save admin state: {err}"))?;
            return Ok(EnrollmentDecision {
                user_id: requested,
                client_uid,
                status: EnrollmentStatus::Enrolled,
                preconfigured: true,
            });
        }
    }

    if state.enrollment_policy == EnrollmentPolicy::PreconfiguredOnly {
        return Err(format!(
            "client `{client_uid}` is not preconfigured and enrollment policy is preconfigured-only"
        ));
    }

    let requested_conflicts = requested_user_id.filter(|requested| {
        *requested > 0
            && admin_state
                .devices
                .iter()
                .any(|device| device.user_id == *requested)
    });
    let user_id = next_available_user_id(&admin_state, requested_user_id);
    let status = match state.enrollment_policy {
        EnrollmentPolicy::Auto => EnrollmentStatus::Enrolled,
        EnrollmentPolicy::Approval => EnrollmentStatus::Pending,
        EnrollmentPolicy::PreconfiguredOnly => EnrollmentStatus::Rejected,
    };
    let mut warnings = Vec::new();
    if let Some(conflict) = requested_conflicts {
        warnings.push(format!(
            "requested user_id {conflict} is already assigned; server assigned {user_id}"
        ));
    }
    admin_state.devices.push(DeviceEnrollment {
        client_uid: client_uid.clone(),
        user_id,
        status,
        name: String::new(),
        role,
        first_seen_ms: now,
        last_seen_ms: now,
        hardware_fingerprint: None,
        warnings,
    });
    normalize_device_enrollments(&mut admin_state.devices);
    let snapshot = admin_state.clone();
    drop(admin_state);
    save_admin_state(state, &snapshot)
        .await
        .map_err(|err| format!("save admin state: {err}"))?;

    Ok(EnrollmentDecision {
        user_id,
        client_uid,
        status,
        preconfigured: false,
    })
}

async fn save_admin_state(
    state: &ServerState,
    admin_state: &PersistedAdminState,
) -> anyhow::Result<()> {
    let Some(path) = state.admin_state_file.as_deref() else {
        return Ok(());
    };
    save_admin_state_to_path(path, admin_state).await
}

async fn save_admin_state_to_path(
    path: &Path,
    admin_state: &PersistedAdminState,
) -> anyhow::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create admin state directory {}", parent.display()))?;
    }

    let contents = serde_json::to_string_pretty(admin_state)?;
    let tmp_path = path.with_extension("tmp");
    tokio::fs::write(&tmp_path, contents)
        .await
        .with_context(|| format!("write temp admin state file {}", tmp_path.display()))?;
    tokio::fs::rename(&tmp_path, path)
        .await
        .with_context(|| format!("replace admin state file {}", path.display()))?;
    Ok(())
}

fn codec_supported(codec: Codec) -> bool {
    match codec {
        Codec::Pcm16 => true,
        Codec::Pcm24 => true,
        Codec::Pcm48 => true,
        Codec::Adpcm => false,
        Codec::Opus => true,
    }
}

async fn codec_available_for_user(
    state: &ServerState,
    user_id: UserId,
    codec: Codec,
) -> Result<(), String> {
    if !codec_supported(codec) {
        return Err(format!("server was built without support for {codec:?}"));
    }

    let sessions = state.sessions.read().await;
    let Some(session) = sessions.get(&user_id) else {
        return Ok(());
    };

    let live = session.addr.is_some() || state.control_clients.read().await.contains_key(&user_id);
    if !live {
        return Ok(());
    }

    if session.supported_codecs.contains(&codec) {
        Ok(())
    } else {
        Err(format!(
            "client {user_id} does not advertise support for {codec:?}"
        ))
    }
}

fn normalize_supported_codecs(codecs: Vec<Codec>) -> HashSet<Codec> {
    let mut supported = codecs
        .into_iter()
        .filter(|codec| codec_supported(*codec))
        .collect::<HashSet<_>>();
    supported.insert(Codec::Pcm16);
    supported
}

fn sorted_codecs(codecs: &HashSet<Codec>) -> Vec<Codec> {
    let mut codecs = codecs.iter().copied().collect::<Vec<_>>();
    codecs.sort_by_key(|codec| match codec {
        Codec::Pcm16 => 0,
        Codec::Pcm24 => 1,
        Codec::Pcm48 => 2,
        Codec::Opus => 3,
        Codec::Adpcm => 4,
    });
    codecs
}

fn stereo_status_for_session(session: &Session) -> StereoStatus {
    if session.stereo.active_for_codec(session.output_codec) {
        StereoStatus {
            active: true,
            channels: 2,
            warning: None,
        }
    } else if session.stereo.enabled {
        StereoStatus {
            active: false,
            channels: common::CHANNELS,
            warning: Some("stereo receive requires pcm48 or opus".to_string()),
        }
    } else {
        StereoStatus::default()
    }
}

async fn register_control_client(
    state: &ServerState,
    user_id: UserId,
    sender: mpsc::UnboundedSender<ControlEvent>,
) {
    state.control_clients.write().await.insert(user_id, sender);
}

async fn unregister_control_client(
    state: &ServerState,
    user_id: UserId,
    sender: &mpsc::UnboundedSender<ControlEvent>,
) {
    let mut clients = state.control_clients.write().await;
    if clients
        .get(&user_id)
        .is_some_and(|current| current.same_channel(sender))
    {
        clients.remove(&user_id);
    }
}

async fn push_config_update(state: &ServerState, user_id: UserId) {
    let Some(event) = config_event_snapshot(state, user_id).await else {
        return;
    };
    let Some(sender) = state.control_clients.read().await.get(&user_id).cloned() else {
        return;
    };

    if sender.send(event).is_err() {
        state.control_clients.write().await.remove(&user_id);
    }
}

async fn push_presence_updates(state: &ServerState) {
    let user_ids = state
        .control_clients
        .read()
        .await
        .keys()
        .copied()
        .collect::<Vec<_>>();
    for user_id in user_ids {
        let channels = channel_rosters_for_user(state, user_id).await;
        let client_uid = state
            .sessions
            .read()
            .await
            .get(&user_id)
            .map(|session| session.client_uid.clone())
            .unwrap_or_default();
        let event = ControlEvent::PresenceUpdate {
            user_id,
            client_uid,
            channels,
        };
        let Some(sender) = state.control_clients.read().await.get(&user_id).cloned() else {
            continue;
        };
        if sender.send(event).is_err() {
            state.control_clients.write().await.remove(&user_id);
        }
    }
}

async fn config_event_snapshot(state: &ServerState, user_id: UserId) -> Option<ControlEvent> {
    let emergency = state.emergency.read().await.clone();
    let sessions = state.sessions.read().await;
    let session = sessions.get(&user_id)?;
    let alerts = state.alerts.read().await;
    let (active_alerts, recent_alerts) =
        alert_statuses_for_user_with_sessions(&alerts, user_id, &sessions);
    let emergency = emergency_status_for_user(emergency.as_ref(), &sessions, user_id);
    Some(session.control_event(
        user_id,
        active_direct_call_statuses_for_user(user_id, &sessions),
        active_alerts,
        recent_alerts,
        emergency,
    ))
}

async fn recording_status_snapshot(state: &ServerState) -> RecordingStatusResponse {
    let recording = state.recording.read().await;
    let (engine_mode, whisper_command, whisper_model, whisper_model_dir, last_error) = {
        let transcription = state.transcription.read().await;
        (
            transcription.engine_mode,
            transcription.whisper_command.clone(),
            transcription.whisper_model.clone(),
            transcription.whisper_model_dir.clone(),
            transcription.last_error.clone(),
        )
    };
    let models = list_whisper_models_from_paths(&whisper_model_dir, whisper_model.as_deref()).await;
    let engine = TranscriptionEngineStatus {
        available: transcription_engine_available_parts(
            engine_mode,
            whisper_command.as_ref(),
            whisper_model.as_ref(),
        ),
        mode: engine_mode,
        acceleration: whisper_acceleration_status(),
        command: recording
            .whisper_command
            .as_ref()
            .map(|path| path.display().to_string()),
        model: whisper_model
            .as_ref()
            .map(|path| path.display().to_string()),
        model_dir: Some(whisper_model_dir.display().to_string()),
        models,
        last_error: last_error.or_else(|| recording.last_engine_error.clone()),
    };
    let recorded_users = recording
        .active
        .as_ref()
        .map(|session| {
            let mut users = session.writers.keys().copied().collect::<Vec<_>>();
            users.sort_unstable();
            users
        })
        .unwrap_or_default();

    RecordingStatusResponse {
        active: recording.active.is_some(),
        session_id: recording.active.as_ref().map(|session| session.id.clone()),
        session_dir: recording
            .active
            .as_ref()
            .map(|session| session.dir.display().to_string()),
        started_at_ms: recording
            .active
            .as_ref()
            .map(|session| session.started_at_ms),
        transcribe: recording
            .active
            .as_ref()
            .is_some_and(|session| session.transcribe),
        recorded_users,
        frames_recorded: recording
            .active
            .as_ref()
            .map_or(0, |session| session.frames_recorded),
        transcript_segments: recording.transcripts.len(),
        engine,
        recent_sessions: recording.recent.clone(),
    }
}

async fn recording_sessions_snapshot(state: &ServerState) -> Vec<RecordingSessionSummary> {
    let recording = state.recording.read().await;
    recording.recent.clone()
}

fn transcription_engine_available(transcription: &LiveTranscriptionState) -> bool {
    transcription_engine_available_parts(
        transcription.engine_mode,
        transcription.whisper_command.as_ref(),
        transcription.whisper_model.as_ref(),
    )
}

fn transcription_engine_available_parts(
    mode: TranscriptionEngineMode,
    command: Option<&PathBuf>,
    model: Option<&PathBuf>,
) -> bool {
    match mode {
        TranscriptionEngineMode::Disabled => false,
        TranscriptionEngineMode::BuiltinWhisper => model.is_some(),
        TranscriptionEngineMode::ExternalWhisper => command.is_some() && model.is_some(),
    }
}

fn whisper_acceleration_status() -> WhisperAccelerationStatus {
    WhisperAccelerationStatus {
        active_backend: whisper_acceleration_backend().to_string(),
        metal_compiled: cfg!(feature = "macos-metal"),
        coreml_compiled: cfg!(feature = "macos-coreml"),
    }
}

fn whisper_acceleration_backend() -> &'static str {
    if cfg!(feature = "macos-metal") {
        "metal"
    } else if cfg!(feature = "macos-coreml") {
        "coreml"
    } else {
        "cpu"
    }
}

async fn live_transcription_status_snapshot(
    state: &ServerState,
) -> LiveTranscriptionStatusResponse {
    let (active, available, engine, model, model_dir, command, started_at_ms, last_error, users) = {
        let transcription = state.transcription.read().await;
        let mut users = transcription
            .per_user
            .iter()
            .map(|(&user_id, runtime)| LiveTranscriptionUserStatus {
                user_id,
                queued_jobs: runtime.pending.len(),
                dropped_jobs: runtime.dropped_jobs,
                dropped_frames: runtime.dropped_frames,
                completed_segments: runtime.completed_segments,
                active_chunk: !runtime.chunker.buffer.is_empty(),
                worker_running: runtime.worker_running,
                contexts: runtime.last_contexts.clone(),
            })
            .collect::<Vec<_>>();
        users.sort_by_key(|status| status.user_id);
        (
            transcription.active,
            transcription_engine_available(&transcription),
            transcription.engine_mode,
            transcription.whisper_model.clone(),
            transcription.whisper_model_dir.clone(),
            transcription.whisper_command.clone(),
            transcription.started_at_ms,
            transcription.last_error.clone(),
            users,
        )
    };
    let models = list_whisper_models_from_paths(&model_dir, model.as_deref()).await;
    let queued_jobs = users.iter().map(|user| user.queued_jobs).sum();
    let dropped_jobs = users.iter().map(|user| user.dropped_jobs).sum();
    let dropped_frames = users.iter().map(|user| user.dropped_frames).sum();
    let completed_segments = users.iter().map(|user| user.completed_segments).sum();

    LiveTranscriptionStatusResponse {
        active,
        available,
        engine,
        acceleration: whisper_acceleration_status(),
        model: model.as_ref().map(|path| path.display().to_string()),
        model_dir: model_dir.display().to_string(),
        models,
        command: command.as_ref().map(|path| path.display().to_string()),
        started_at_ms,
        users,
        queued_jobs,
        dropped_jobs,
        dropped_frames,
        completed_segments,
        last_error,
    }
}

async fn list_whisper_models(state: &ServerState) -> Vec<WhisperModelInfo> {
    let (model_dir, selected) = {
        let transcription = state.transcription.read().await;
        (
            transcription.whisper_model_dir.clone(),
            transcription.whisper_model.clone(),
        )
    };
    list_whisper_models_from_paths(&model_dir, selected.as_deref()).await
}

async fn deepfilternet_status_snapshot(state: &ServerState) -> DeepFilterNetStatusResponse {
    let model_dir = state.deepfilternet_model_dir.read().await.clone();
    let models = list_deepfilternet_models_from_dir(&model_dir).await;
    let coreml_packages = list_deepfilternet_coreml_packages_from_dir(&model_dir).await;
    let backend_available = processing_engine_available(ProcessingEngine::DeepFilterNet);
    let no_models = models.is_empty();
    let has_coreml_packages = !coreml_packages.is_empty();
    DeepFilterNetStatusResponse {
        backend_available,
        supported_backends: deepfilternet_supported_backend_names(),
        preferred_backend: deepfilternet_auto_backend().to_string(),
        apple_compute_units: apple_compute_unit_names(),
        model_dir: model_dir.display().to_string(),
        models,
        coreml_compiled: cfg!(all(
            target_os = "macos",
            feature = "processing-deepfilternet-coreml"
        )),
        coreml_packages,
        detail: if !backend_available {
            Some(unavailable_processing_engine_detail(
                ProcessingEngine::DeepFilterNet,
            ))
        } else if no_models {
            Some(if has_coreml_packages {
                "Core ML packages are present, but no complete runtime model is selectable. Use a complete Core ML package on macOS with the Core ML backend, or add a compatible ONNX .tar.gz model for Tract.".to_string()
            } else {
                "No compatible DeepFilterNet ONNX .tar.gz models or complete Core ML package directories found".to_string()
            })
        } else {
            None
        },
    }
}

fn deepfilternet_supported_backend_names() -> Vec<String> {
    vec![
        "auto".to_string(),
        "tract".to_string(),
        "coreml".to_string(),
    ]
}

fn apple_compute_unit_names() -> Vec<String> {
    vec![
        "all".to_string(),
        "cpu_and_gpu".to_string(),
        "cpu_and_neural_engine".to_string(),
        "cpu_only".to_string(),
    ]
}

fn deepfilternet_auto_backend() -> &'static str {
    if deepfilternet_coreml_runtime_available() {
        "coreml"
    } else {
        "tract"
    }
}

async fn list_deepfilternet_models_from_dir(model_dir: &Path) -> Vec<DeepFilterNetModelInfo> {
    let mut models = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(model_dir).await else {
        return models;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !is_supported_deepfilternet_model_path(&path) {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let runtime = if is_supported_deepfilternet_coreml_package_path(&path) {
            "coreml"
        } else {
            "tract"
        };
        models.push(DeepFilterNetModelInfo {
            name,
            path: path.display().to_string(),
            runtime: runtime.to_string(),
        });
    }
    models.sort_by(|a, b| a.name.cmp(&b.name));
    models
}

async fn list_deepfilternet_coreml_packages_from_dir(
    model_dir: &Path,
) -> Vec<DeepFilterNetCoreMlPackageInfo> {
    let mut packages = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(model_dir).await else {
        return packages;
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !is_supported_deepfilternet_coreml_package_path(&path) {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let (complete, detail) = deepfilternet_coreml_package_status(&path);
        packages.push(DeepFilterNetCoreMlPackageInfo {
            name,
            path: path.display().to_string(),
            complete,
            detail,
        });
    }

    packages.sort_by(|a, b| a.name.cmp(&b.name));
    packages
}

async fn list_whisper_models_from_paths(
    model_dir: &Path,
    selected: Option<&Path>,
) -> Vec<WhisperModelInfo> {
    let mut models = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(model_dir).await else {
        return models;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !is_supported_whisper_model_path(&path) {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let selected = selected.is_some_and(|selected| selected == path);
        models.push(WhisperModelInfo {
            name,
            path: path.display().to_string(),
            selected,
        });
    }
    models.sort_by(|a, b| a.name.cmp(&b.name));
    models
}

fn is_supported_whisper_model_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "bin" | "gguf"))
}

fn is_supported_deepfilternet_model_path(path: &Path) -> bool {
    is_supported_deepfilternet_runtime_model_path(path)
}

fn is_supported_deepfilternet_runtime_model_path(path: &Path) -> bool {
    is_supported_deepfilternet_onnx_archive_path(path)
        || (is_supported_deepfilternet_coreml_package_path(path)
            && deepfilternet_coreml_package_status(path).0)
}

fn is_supported_deepfilternet_onnx_archive_path(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let file_name = file_name.to_ascii_lowercase();
    file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz")
}

fn is_supported_deepfilternet_coreml_package_path(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let file_name = file_name.to_ascii_lowercase();
    path.is_dir() && file_name.contains("coreml")
}

fn deepfilternet_coreml_package_status(path: &Path) -> (bool, Option<String>) {
    let required_dirs = ["enc.mlmodelc", "erb_dec.mlmodelc", "df_dec.mlmodelc"];
    let required_files = ["config.ini", "metadata.json"];
    let mut missing = required_dirs
        .iter()
        .filter(|name| !path.join(name).is_dir())
        .copied()
        .collect::<Vec<_>>();
    missing.extend(
        required_files
            .iter()
            .filter(|name| !path.join(name).is_file())
            .copied(),
    );

    if missing.is_empty() {
        missing.extend(deepfilternet_coreml_missing_config_keys(path));
    }

    if missing.is_empty() {
        return (true, None);
    }

    (
        false,
        Some(format!(
            "Incomplete Core ML package; missing required entries: {}",
            missing.join(", ")
        )),
    )
}

fn deepfilternet_coreml_missing_config_keys(path: &Path) -> Vec<&'static str> {
    let Ok(contents) = std::fs::read_to_string(path.join("config.ini")) else {
        return vec!["config.ini readable"];
    };
    let mut section = "";
    let mut keys = std::collections::HashSet::<(&str, &str)>::new();
    for raw_line in contents.lines() {
        let line = raw_line.split(['#', ';']).next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim();
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        keys.insert((section, key.trim()));
    }

    let mut missing = [
        ("df", "sr", "df.sr"),
        ("df", "hop_size", "df.hop_size"),
        ("df", "fft_size", "df.fft_size"),
        ("df", "min_nb_erb_freqs", "df.min_nb_erb_freqs"),
        ("df", "nb_erb", "df.nb_erb"),
        ("df", "nb_df", "df.nb_df"),
        (
            "deepfilternet",
            "conv_lookahead",
            "deepfilternet.conv_lookahead",
        ),
    ]
    .into_iter()
    .filter_map(|(section, key, label)| (!keys.contains(&(section, key))).then_some(label))
    .collect::<Vec<_>>();

    for key in ["df_order", "df_lookahead"] {
        if !keys.contains(&("df", key)) && !keys.contains(&("deepfilternet", key)) {
            missing.push(match key {
                "df_order" => "df.df_order or deepfilternet.df_order",
                _ => "df.df_lookahead or deepfilternet.df_lookahead",
            });
        }
    }

    missing
}

async fn select_whisper_model(
    state: &Arc<ServerState>,
    request: SelectWhisperModelRequest,
) -> Result<(), AdminApiError> {
    let model_name = request.model.trim();
    if model_name.is_empty() || Path::new(model_name).components().count() != 1 {
        return Err(AdminApiError::BadRequest(
            "model must be the filename of a model in the configured model folder".to_string(),
        ));
    }
    let model_path = {
        let transcription = state.transcription.read().await;
        transcription.whisper_model_dir.join(model_name)
    };
    if !is_supported_whisper_model_path(&model_path) {
        return Err(AdminApiError::BadRequest(
            "model must end with .bin or .gguf".to_string(),
        ));
    }
    if tokio::fs::metadata(&model_path).await.is_err() {
        return Err(AdminApiError::BadRequest(format!(
            "model `{model_name}` was not found in the configured model folder"
        )));
    }
    let mut transcription = state.transcription.write().await;
    transcription.whisper_model = Some(model_path);
    clear_live_whisper_context(&mut transcription);
    transcription.last_error = None;
    if matches!(transcription.engine_mode, TranscriptionEngineMode::Disabled) {
        transcription.engine_mode = TranscriptionEngineMode::BuiltinWhisper;
    }
    Ok(())
}

#[cfg(feature = "transcription-whisper")]
fn clear_live_whisper_context(transcription: &mut LiveTranscriptionState) {
    transcription.whisper_context = None;
}

#[cfg(not(feature = "transcription-whisper"))]
fn clear_live_whisper_context(_transcription: &mut LiveTranscriptionState) {}

async fn start_live_transcription(
    state: &Arc<ServerState>,
    request: StartLiveTranscriptionRequest,
) -> Result<(), AdminApiError> {
    ensure_live_transcription_engine(state).await?;
    let mut transcription = state.transcription.write().await;
    transcription.active = true;
    transcription.started_at_ms = Some(unix_time_ms());
    transcription.users = request
        .users
        .map(|users| users.into_iter().filter(|user| *user > 0).collect());
    transcription.last_error = None;
    Ok(())
}

async fn stop_live_transcription(state: &Arc<ServerState>) -> Result<(), AdminApiError> {
    let mut jobs_to_spawn = Vec::new();
    {
        let mut transcription = state.transcription.write().await;
        transcription.active = false;
        transcription.started_at_ms = None;
        transcription.users = None;
        for (&user_id, runtime) in &mut transcription.per_user {
            if let Some(job) = runtime.chunker.flush(user_id) {
                if runtime.pending.len() >= LIVE_TRANSCRIPTION_QUEUE_LIMIT {
                    runtime.dropped_jobs += 1;
                    runtime.dropped_frames += frames_in_live_job(&job) as u64;
                } else {
                    runtime.pending.push_back(job);
                    if !runtime.worker_running {
                        runtime.worker_running = true;
                        jobs_to_spawn.push(user_id);
                    }
                }
            }
        }
    }
    for user_id in jobs_to_spawn {
        tokio::spawn(run_live_transcription_worker(Arc::clone(state), user_id));
    }
    Ok(())
}

#[cfg(feature = "transcription-whisper")]
async fn ensure_live_transcription_engine(state: &Arc<ServerState>) -> Result<(), AdminApiError> {
    let (mode, model, needs_builtin_context) = {
        let transcription = state.transcription.read().await;
        let model = transcription.whisper_model.clone();
        let needs_builtin_context = matches!(
            transcription.engine_mode,
            TranscriptionEngineMode::BuiltinWhisper
        ) && transcription.whisper_context.is_none();
        (transcription.engine_mode, model, needs_builtin_context)
    };

    match mode {
        TranscriptionEngineMode::Disabled => {
            return Err(AdminApiError::BadRequest(
                "live transcription engine is disabled".to_string(),
            ));
        }
        TranscriptionEngineMode::ExternalWhisper => {
            return Err(AdminApiError::BadRequest(
                "live transcription requires builtin-whisper; external-whisper is only used for recording stop transcription".to_string(),
            ));
        }
        TranscriptionEngineMode::BuiltinWhisper => {}
    }

    let Some(model) = model else {
        return Err(AdminApiError::BadRequest(
            "live transcription requires --whisper-model".to_string(),
        ));
    };

    if needs_builtin_context {
        let model_for_task = model.clone();
        let context = tokio::task::spawn_blocking(move || {
            WhisperContext::new_with_params(&model_for_task, WhisperContextParameters::default())
                .with_context(|| format!("load Whisper model {}", model_for_task.display()))
        })
        .await
        .map_err(|err| AdminApiError::Internal(err.to_string()))?
        .map_err(|err| AdminApiError::BadRequest(err.to_string()))?;

        let mut transcription = state.transcription.write().await;
        if transcription.whisper_context.is_none() {
            transcription.whisper_context = Some(Arc::new(context));
        }
    }

    Ok(())
}

#[cfg(not(feature = "transcription-whisper"))]
async fn ensure_live_transcription_engine(_state: &Arc<ServerState>) -> Result<(), AdminApiError> {
    Err(AdminApiError::BadRequest(
        "server was built without transcription-whisper".to_string(),
    ))
}

async fn start_recording_session(
    state: &ServerState,
    request: StartRecordingRequest,
) -> Result<(), AdminApiError> {
    let session_id = format!("session-{}", unix_time_ms());
    let (base_dir, session_dir) = {
        let recording = state.recording.read().await;
        let base_dir = if recording.base_dir.as_os_str().is_empty() {
            PathBuf::from("intercom-recordings")
        } else {
            recording.base_dir.clone()
        };
        let session_dir = base_dir.join(&session_id);
        (base_dir, session_dir)
    };
    tokio::fs::create_dir_all(&session_dir)
        .await
        .with_context(|| format!("create recording directory {}", session_dir.display()))?;
    let metadata_path = session_dir.join("metadata.jsonl");
    let metadata_writer = BufWriter::new(
        File::create(&metadata_path)
            .with_context(|| format!("create recording metadata {}", metadata_path.display()))?,
    );
    let mut recording = state.recording.write().await;
    if recording.active.is_some() {
        return Err(AdminApiError::BadRequest(
            "a recording session is already active".to_string(),
        ));
    }
    recording.base_dir = base_dir;
    recording.active = Some(ActiveRecordingSession {
        id: session_id,
        dir: session_dir,
        started_at_ms: unix_time_ms(),
        transcribe: request.transcribe,
        users: request
            .users
            .map(|users| users.into_iter().filter(|user| *user > 0).collect()),
        writers: HashMap::new(),
        contexts: HashMap::new(),
        metadata_writer,
        frames_recorded: 0,
    });
    Ok(())
}

async fn stop_recording_session(
    state: &ServerState,
) -> Result<Option<StoppedRecordingForTranscription>, AdminApiError> {
    let mut recording = state.recording.write().await;
    let Some(mut active) = recording.active.take() else {
        return Err(AdminApiError::BadRequest(
            "no recording session is active".to_string(),
        ));
    };
    let mut writer_errors = Vec::new();
    for (user_id, writer) in active.writers.drain() {
        if let Err(err) = writer.finalize() {
            writer_errors.push(format!("user {user_id}: {err}"));
        }
    }
    if let Err(err) = active.metadata_writer.flush() {
        writer_errors.push(format!("metadata: {err}"));
    }
    if !writer_errors.is_empty() {
        recording.last_engine_error = Some(writer_errors.join("; "));
    }
    let mut recorded_users = active.contexts.keys().copied().collect::<Vec<_>>();
    recorded_users.sort_unstable();
    let transcribe = active.transcribe;
    let stopped = transcribe.then(|| StoppedRecordingForTranscription {
        dir: active.dir.clone(),
        users: recorded_users.clone(),
    });
    recording.recent.push(RecordingSessionSummary {
        id: active.id,
        dir: active.dir.display().to_string(),
        started_at_ms: active.started_at_ms,
        stopped_at_ms: Some(unix_time_ms()),
        transcribe: active.transcribe,
        recorded_users,
        frames_recorded: active.frames_recorded,
    });
    if recording.recent.len() > 20 {
        let excess = recording.recent.len() - 20;
        recording.recent.drain(0..excess);
    }
    Ok(stopped)
}

async fn query_transcripts(state: &ServerState, query: TranscriptQuery) -> Vec<TranscriptSegment> {
    let users = transcript_query_ids(query.user_id, query.user_ids.as_deref());
    let channels = transcript_query_ids(query.channel_id, query.channel_ids.as_deref());
    let text_filter = query.q.as_ref().map(|q| q.trim().to_ascii_lowercase());
    let recording = state.recording.read().await;
    recording
        .transcripts
        .iter()
        .filter(|segment| {
            (users.is_empty() || users.contains(&segment.user_id))
                && (channels.is_empty()
                    || segment.contexts.iter().any(|context| {
                        context
                            .channel_id()
                            .is_some_and(|channel_id| channels.contains(&channel_id))
                    }))
                && query.direct_user_id.is_none_or(|direct_user_id| {
                    segment
                        .contexts
                        .contains(&AudioTarget::Direct(direct_user_id))
                })
                && query
                    .source
                    .as_ref()
                    .is_none_or(|source| &segment.source == source)
                && query
                    .since_ms
                    .is_none_or(|since_ms| segment.ended_at_ms >= since_ms)
                && query
                    .until_ms
                    .is_none_or(|until_ms| segment.started_at_ms <= until_ms)
                && text_filter
                    .as_ref()
                    .is_none_or(|q| q.is_empty() || segment.text.to_ascii_lowercase().contains(q))
        })
        .cloned()
        .collect()
}

fn transcript_query_ids(single: Option<UserId>, csv: Option<&str>) -> HashSet<UserId> {
    let mut ids = HashSet::new();
    if let Some(single) = single.filter(|id| *id > 0) {
        ids.insert(single);
    }
    if let Some(csv) = csv {
        ids.extend(
            csv.split(',')
                .filter_map(|item| item.trim().parse::<UserId>().ok())
                .filter(|id| *id > 0),
        );
    }
    ids
}

async fn append_transcript_segment(
    state: &ServerState,
    append: TranscriptAppend,
) -> Result<TranscriptSegment, AdminApiError> {
    let name = state
        .sessions
        .read()
        .await
        .get(&append.user_id)
        .map(|session| session.name.clone())
        .unwrap_or_default();
    let mut recording = state.recording.write().await;
    let id = recording.next_transcript_id;
    recording.next_transcript_id += 1;
    let now = unix_time_ms();
    let segment = TranscriptSegment {
        id,
        user_id: append.user_id,
        user_name: name,
        started_at_ms: append.started_at_ms.unwrap_or(now),
        ended_at_ms: append.ended_at_ms.unwrap_or(now),
        contexts: append.contexts,
        text: append.text.trim().to_string(),
        confidence: append.confidence,
        engine: append.engine,
        source: append.source,
        final_segment: append.final_segment,
    };
    if let Some(active) = recording.active.as_ref() {
        let path = active.dir.join("transcripts.jsonl");
        let line = serde_json::to_string(&segment)
            .map_err(|err| AdminApiError::Internal(err.to_string()))?
            + "\n";
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| std::io::Write::write_all(&mut file, line.as_bytes()))
            .with_context(|| format!("append transcript {}", path.display()))?;
    }
    recording.transcripts.push(segment.clone());
    if recording.transcripts.len() > 2_000 {
        let excess = recording.transcripts.len() - 2_000;
        recording.transcripts.drain(0..excess);
    }
    Ok(segment)
}

async fn run_recording_transcription(
    state: Arc<ServerState>,
    stopped: StoppedRecordingForTranscription,
) -> anyhow::Result<()> {
    let mode = state.transcription.read().await.engine_mode;
    match mode {
        TranscriptionEngineMode::BuiltinWhisper => {
            ensure_live_transcription_engine(&state)
                .await
                .map_err(|err| anyhow::anyhow!("{err:?}"))?;
            run_builtin_recording_transcription(state, stopped).await
        }
        TranscriptionEngineMode::ExternalWhisper => {
            run_external_whisper_recording_transcription(state, stopped).await
        }
        TranscriptionEngineMode::Disabled => {
            let message = "transcription engine is disabled".to_string();
            state.recording.write().await.last_engine_error = Some(message.clone());
            bail!("{message}");
        }
    }
}

async fn run_external_whisper_recording_transcription(
    state: Arc<ServerState>,
    stopped: StoppedRecordingForTranscription,
) -> anyhow::Result<()> {
    let (command, model) = {
        let transcription = state.transcription.read().await;
        let command = transcription
            .whisper_command
            .clone()
            .context("Whisper command is not configured")?;
        let model = transcription
            .whisper_model
            .clone()
            .context("Whisper model is not configured")?;
        (command, model)
    };

    for user_id in stopped.users {
        let wav = stopped.dir.join(format!("user-{user_id}.wav"));
        if tokio::fs::metadata(&wav).await.is_err() {
            continue;
        }
        let output_base = stopped.dir.join(format!("user-{user_id}-transcript"));
        let status = Command::new(&command)
            .arg("-m")
            .arg(&model)
            .arg("-f")
            .arg(&wav)
            .arg("-otxt")
            .arg("-of")
            .arg(&output_base)
            .status()
            .await
            .with_context(|| format!("run Whisper command {}", command.display()))?;
        if !status.success() {
            let message = format!("Whisper exited with status {status}");
            state.recording.write().await.last_engine_error = Some(message.clone());
            bail!("{message}");
        }
        let txt_path = output_base.with_extension("txt");
        let text = tokio::fs::read_to_string(&txt_path)
            .await
            .with_context(|| format!("read Whisper transcript {}", txt_path.display()))?;
        if !text.trim().is_empty() {
            append_transcript_segment(
                &state,
                TranscriptAppend {
                    user_id,
                    contexts: Vec::new(),
                    started_at_ms: None,
                    ended_at_ms: None,
                    text: text.trim().to_string(),
                    confidence: None,
                    engine: "local_whisper".to_string(),
                    source: TranscriptSource::Recording,
                    final_segment: true,
                },
            )
            .await
            .map_err(|err| anyhow::anyhow!("{err:?}"))?;
        }
    }
    Ok(())
}

#[cfg(feature = "transcription-whisper")]
async fn run_builtin_recording_transcription(
    state: Arc<ServerState>,
    stopped: StoppedRecordingForTranscription,
) -> anyhow::Result<()> {
    let context = state
        .transcription
        .read()
        .await
        .whisper_context
        .clone()
        .context("builtin Whisper context is not loaded")?;

    for user_id in stopped.users {
        let wav = stopped.dir.join(format!("user-{user_id}.wav"));
        if tokio::fs::metadata(&wav).await.is_err() {
            continue;
        }
        let samples_16khz = read_wav_as_16khz_mono(&wav).await?;
        let job = LiveTranscriptJob {
            user_id,
            started_at_ms: unix_time_ms(),
            ended_at_ms: unix_time_ms(),
            contexts: Vec::new(),
            samples_16khz,
        };
        if let Some(completed) = transcribe_live_job_with_builtin(Arc::clone(&context), job).await?
        {
            append_transcript_segment(
                &state,
                TranscriptAppend {
                    user_id: completed.user_id,
                    contexts: completed.contexts,
                    started_at_ms: Some(completed.started_at_ms),
                    ended_at_ms: Some(completed.ended_at_ms),
                    text: completed.text,
                    confidence: None,
                    engine: "builtin_whisper".to_string(),
                    source: TranscriptSource::Recording,
                    final_segment: true,
                },
            )
            .await
            .map_err(|err| anyhow::anyhow!("{err:?}"))?;
        }
    }
    Ok(())
}

#[cfg(not(feature = "transcription-whisper"))]
async fn run_builtin_recording_transcription(
    state: Arc<ServerState>,
    _stopped: StoppedRecordingForTranscription,
) -> anyhow::Result<()> {
    let message = "server was built without transcription-whisper".to_string();
    state.recording.write().await.last_engine_error = Some(message.clone());
    bail!("{message}")
}

#[cfg_attr(not(feature = "transcription-whisper"), allow(dead_code))]
async fn read_wav_as_16khz_mono(path: &Path) -> anyhow::Result<Vec<i16>> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut reader = hound::WavReader::open(&path)
            .with_context(|| format!("open WAV {}", path.display()))?;
        let spec = reader.spec();
        if spec.channels != 1 {
            bail!(
                "expected mono WAV for transcription, got {} channels",
                spec.channels
            );
        }
        let samples = reader
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("read WAV samples {}", path.display()))?;
        Ok(resample_linear(
            &samples,
            spec.sample_rate,
            common::SAMPLE_RATE,
        ))
    })
    .await
    .context("join WAV read task")?
}

async fn status_snapshot(state: &ServerState) -> (Vec<SessionStatus>, StatusMetrics) {
    let now = Instant::now();
    let emergency = state.emergency.read().await.clone();
    let sessions = state.sessions.read().await;
    let sources = state.sources.read().await;
    let health = state.health.read().await;
    let alerts = state.alerts.read().await;
    let recording = state.recording.read().await;
    let recording_active = recording.active.as_ref();
    let transcription = state.transcription.read().await;
    let live_transcription_active = transcription.active;
    let live_transcription_users = transcription.users.clone();
    let mut statuses = sessions
        .iter()
        .map(|(&user_id, session)| {
            let mut listen = session.listen_channels.iter().copied().collect::<Vec<_>>();
            listen.sort_unstable();
            let mut tx = session.tx_channels.iter().copied().collect::<Vec<_>>();
            tx.sort_unstable();
            let mut active_buttons = session.active_buttons.iter().cloned().collect::<Vec<_>>();
            active_buttons.sort();
            let active_direct_calls = active_direct_call_statuses_for_user(user_id, &sessions);
            let direct_call_history = direct_call_history_entries(&session.direct_call_history);
            let (active_alerts, recent_alerts) =
                alert_statuses_for_user_with_sessions(&alerts, user_id, &sessions);
            let emergency = emergency_status_for_user(emergency.as_ref(), &sessions, user_id);
            let mut priority_channels = session
                .priority_channels
                .iter()
                .copied()
                .collect::<Vec<_>>();
            priority_channels.sort_unstable();

            let queue_depth = sources
                .get(&user_id)
                .map_or(0, |source| source.frames.len());
            let mut input = health
                .get(&user_id)
                .map_or_else(InputMeterStatus::default, |health| health.input.clone());
            let output = health
                .get(&user_id)
                .map_or_else(OutputMeterStatus::default, |health| health.output.clone());
            let capture = health
                .get(&user_id)
                .and_then(|health| health.capture.clone());
            let mut transport = health
                .get(&user_id)
                .map_or_else(TransportHealthStatus::default, |health| {
                    health.transport.clone()
                });
            let processing_status = health
                .get(&user_id)
                .map_or_else(ProcessingStatus::default, |health| {
                    health.processing.clone()
                });
            transport.source_queue_depth = queue_depth;
            if let Some(user_health) = health.get(&user_id) {
                input.active = now <= user_health.active_until;
                input.last_packet_age_ms = user_health.last_packet_seen.map(|seen| {
                    now.duration_since(seen)
                        .as_millis()
                        .try_into()
                        .unwrap_or(u64::MAX)
                });
            }

            SessionStatus {
                user_id,
                client_uid: session.client_uid.clone(),
                enrollment: session.enrollment,
                role: session.role,
                addr: session.addr.map(|addr| addr.to_string()),
                listen,
                tx,
                talker_vol: session.talker_volumes.clone(),
                codec: session.output_codec,
                opus_profile: session.opus_profile,
                supported_codecs: sorted_codecs(&session.supported_codecs),
                advertised_buttons: session.advertised_buttons.clone(),
                buttons: sorted_buttons(&session.buttons),
                active_buttons,
                active_direct_calls,
                last_direct_caller: session.last_direct_caller,
                direct_call_history,
                active_alerts,
                recent_alerts,
                emergency,
                ifb: session.ifb.clone(),
                lockout: session.lockout.clone(),
                ifb_status: session.ifb_status.clone(),
                stereo: session.stereo.clone(),
                esp32_audio: session.esp32_audio.clone(),
                stereo_status: stereo_status_for_session(session),
                talk_mode: session.talk_mode,
                regular_talk_active: session.regular_talk_active,
                priority: session.priority,
                priority_channels,
                processing: session.processing.clone(),
                processing_status,
                queue_depth,
                age_ms: now
                    .duration_since(session.last_seen)
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
                input,
                output,
                capture,
                bridge: session.bridge.clone(),
                transport,
                recording_enabled: recording_active.is_some_and(|active| {
                    active
                        .users
                        .as_ref()
                        .is_none_or(|users| users.contains(&user_id))
                }),
                transcription_enabled: (live_transcription_active
                    && live_transcription_users
                        .as_ref()
                        .is_none_or(|users| users.contains(&user_id)))
                    || recording_active.is_some_and(|active| active.transcribe),
            }
        })
        .collect::<Vec<_>>();
    statuses.sort_by_key(|status| status.user_id);
    (statuses, state.metrics.snapshot())
}

async fn channel_rosters_for_user(
    state: &ServerState,
    user_id: UserId,
) -> Vec<ChannelPresenceRoster> {
    let now = Instant::now();
    let channel_names = state
        .admin_state
        .read()
        .await
        .channels
        .iter()
        .map(|channel| (channel.id, channel.name.clone()))
        .collect::<HashMap<_, _>>();
    let sessions = state.sessions.read().await;
    let health = state.health.read().await;
    let Some(viewer) = sessions.get(&user_id) else {
        return Vec::new();
    };

    let configured_channels = channel_names.keys().copied().collect::<HashSet<_>>();
    let mut visible_channels = configured_channels.clone();
    visible_channels.extend(configured_presence_channels(viewer));
    visible_channels.extend(viewer.effective_tx_channels());
    let mut rosters = visible_channels
        .into_iter()
        .filter_map(|channel_id| {
            let mut members = sessions
                .iter()
                .filter_map(|(&member_id, session)| {
                    if member_id == 0 {
                        return None;
                    }
                    let mut member_channels = configured_presence_channels(session);
                    member_channels.extend(session.effective_tx_channels());
                    if !member_channels.contains(&channel_id) {
                        return None;
                    }
                    let active = health
                        .get(&member_id)
                        .is_some_and(|status| now <= status.active_until);
                    Some(ChannelPresenceMember {
                        user_id: member_id,
                        name: session.name.clone(),
                        present: session.addr.is_some(),
                        transmitting: active
                            && session
                                .active_tx_targets()
                                .contains(&AudioTarget::Channel(channel_id)),
                    })
                })
                .collect::<Vec<_>>();
            members.sort_by_key(|member| member.user_id);
            if members.is_empty() && !configured_channels.contains(&channel_id) {
                None
            } else {
                Some(ChannelPresenceRoster {
                    channel_id,
                    name: channel_names
                        .get(&channel_id)
                        .map(|name| name.trim().to_string())
                        .filter(|name| !name.is_empty()),
                    members,
                })
            }
        })
        .collect::<Vec<_>>();
    rosters.sort_by_key(|roster| roster.channel_id);
    rosters
}

fn configured_presence_channels(session: &Session) -> HashSet<ChannelId> {
    let mut channels = session.listen_channels.clone();
    channels.extend(session.tx_channels.iter().copied());
    channels.extend(session.ifb.program.iter().copied());
    channels.extend(session.ifb.interrupt.iter().copied());
    for button in &session.buttons {
        for action in &button.actions {
            if let TalkButtonAction::Transmit { channels: tx, .. } = action {
                channels.extend(tx.iter().copied());
            }
        }
    }
    channels
}

async fn register_audio_source(
    state: &ServerState,
    user_id: UserId,
    target: AudioTarget,
    codec: Codec,
    addr: SocketAddr,
) {
    let desired = register_audio_endpoint(state, user_id, codec, addr).await;
    let mut sessions = state.sessions.write().await;
    let session = sessions.entry(user_id).or_insert_with(Session::new);

    if let AudioTarget::Channel(channel_id) = target {
        if session.listen_channels.is_empty() {
            session.listen_channels.insert(channel_id);
        }
        if session.tx_channels.is_empty() && desired.is_none() {
            session.tx_channels.insert(channel_id);
        }
    }
}

async fn audio_user_is_enrolled(state: &ServerState, user_id: UserId) -> bool {
    if user_id == 0 {
        return false;
    }

    {
        let sessions = state.sessions.read().await;
        if let Some(session) = sessions.get(&user_id) {
            return session.enrollment == EnrollmentStatus::Enrolled;
        }
    }

    let admin_state = state.admin_state.read().await;
    if admin_state
        .devices
        .iter()
        .any(|device| device.user_id == user_id && device.status != EnrollmentStatus::Enrolled)
    {
        return false;
    }

    state.enrollment_policy == EnrollmentPolicy::Auto
}

async fn register_audio_endpoint(
    state: &ServerState,
    user_id: UserId,
    codec: Codec,
    addr: SocketAddr,
) -> Option<DesiredClientConfig> {
    let desired = desired_client(state, user_id).await;
    let mut sessions = state.sessions.write().await;
    let session = sessions.entry(user_id).or_insert_with(Session::new);
    let was_new_addr = session.addr != Some(addr);
    session.addr = Some(addr);
    if codec_supported(codec) {
        session.supported_codecs.insert(codec);
    }
    if let Some(desired) = desired.as_ref() {
        apply_desired_to_session_fields(desired, session);
    } else {
        session.output_codec = codec;
    }
    session.last_seen = Instant::now();

    if was_new_addr {
        tracing::info!(user_id, %addr, "registered audio endpoint");
    }
    desired
}

async fn store_source_frame(
    state: &ServerState,
    user_id: UserId,
    target: AudioTarget,
    samples: Vec<i16>,
) {
    store_source_frame_with_options(state, user_id, target, samples, false, false).await;
}

async fn store_source_frame_with_options(
    state: &ServerState,
    user_id: UserId,
    target: AudioTarget,
    samples: Vec<i16>,
    priority: bool,
    duck: bool,
) {
    let mut sources = state.sources.write().await;
    let queue = sources.entry(user_id).or_insert_with(SourceQueue::new);
    let dropped = if priority || duck {
        queue.push_with_options(target, samples, priority, duck)
    } else {
        queue.push(target, samples)
    };
    state
        .metrics
        .source_frames_enqueued
        .fetch_add(1, Ordering::Relaxed);
    if dropped {
        state
            .metrics
            .source_frames_dropped
            .fetch_add(1, Ordering::Relaxed);
    }
    drop(sources);
    if dropped {
        record_source_drop(state, user_id).await;
    }
}

async fn record_decode_error(state: &ServerState, user_id: UserId) {
    let mut health = state.health.write().await;
    health.entry(user_id).or_default().transport.decode_errors += 1;
}

async fn record_source_drop(state: &ServerState, user_id: UserId) {
    let mut health = state.health.write().await;
    health
        .entry(user_id)
        .or_default()
        .transport
        .source_frames_dropped += 1;
}

async fn processing_config_for_user(state: &ServerState, user_id: UserId) -> ProcessingConfig {
    state
        .sessions
        .read()
        .await
        .get(&user_id)
        .map_or_else(ProcessingConfig::default, |session| {
            session.processing.clone()
        })
}

async fn opus_profile_for_user(state: &ServerState, user_id: UserId) -> OpusProfile {
    if let Some(profile) = state
        .sessions
        .read()
        .await
        .get(&user_id)
        .map(|session| session.opus_profile)
    {
        return profile;
    }
    desired_client(state, user_id)
        .await
        .map_or_else(OpusProfile::default, |desired| desired.opus_profile)
}

async fn record_processing_status(state: &ServerState, user_id: UserId, status: ProcessingStatus) {
    let mut health = state.health.write().await;
    health.entry(user_id).or_default().processing = status;
}

async fn record_capture_health(state: &ServerState, user_id: UserId, capture: CaptureHealthStatus) {
    {
        let mut sessions = state.sessions.write().await;
        sessions
            .entry(user_id)
            .or_insert_with(Session::new)
            .last_seen = Instant::now();
    }
    let mut health = state.health.write().await;
    health.entry(user_id).or_default().capture = Some(capture);
}

fn normalize_bridge_status(mut status: BridgeStatus) -> BridgeStatus {
    status.input_device = status
        .input_device
        .map(|device| device.trim().to_string())
        .filter(|device| !device.is_empty());
    status.output_device = status
        .output_device
        .map(|device| device.trim().to_string())
        .filter(|device| !device.is_empty());
    status.input_gain = if status.input_gain.is_finite() {
        status.input_gain.clamp(0.0, 8.0)
    } else {
        1.0
    };
    status.output_gain = if status.output_gain.is_finite() {
        status.output_gain.clamp(0.0, 8.0)
    } else {
        1.0
    };
    status.tx = sorted_unique_channels(status.tx);
    status.listen = sorted_unique_channels(status.listen);
    status.note = status.note.trim().to_string();
    status
}

async fn record_bridge_status(state: &ServerState, user_id: UserId, status: BridgeStatus) {
    let mut sessions = state.sessions.write().await;
    let session = sessions.entry(user_id).or_insert_with(Session::new);
    session.role = ClientRole::Bridge;
    session.bridge = Some(normalize_bridge_status(status));
    session.last_seen = Instant::now();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ServerDebugAudioKind {
    DecodedInput,
    MixedOutput,
}

impl ServerDebugAudioKind {
    fn file_stem(self) -> &'static str {
        match self {
            Self::DecodedInput => "server-decoded-input",
            Self::MixedOutput => "server-mixed-output",
        }
    }
}

#[derive(Debug)]
struct ServerDebugAudioFrame {
    kind: ServerDebugAudioKind,
    user_id: UserId,
    channels: u16,
    samples: Vec<i16>,
}

fn spawn_server_debug_audio_writer(
    dir: PathBuf,
) -> anyhow::Result<mpsc::Sender<ServerDebugAudioFrame>> {
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create server debug audio directory {}", dir.display()))?;
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(async move {
        if let Err(err) = run_server_debug_audio_writer(dir, rx).await {
            tracing::warn!(%err, "server debug audio writer stopped");
        }
    });
    Ok(tx)
}

async fn run_server_debug_audio_writer(
    dir: PathBuf,
    mut rx: mpsc::Receiver<ServerDebugAudioFrame>,
) -> anyhow::Result<()> {
    let mut writers: HashMap<
        (ServerDebugAudioKind, UserId, u16),
        hound::WavWriter<BufWriter<File>>,
    > = HashMap::new();

    while let Some(frame) = rx.recv().await {
        let key = (frame.kind, frame.user_id, frame.channels.max(1));
        let writer = match writers.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let path = dir.join(format!(
                    "{}-user-{}-{}ch.wav",
                    frame.kind.file_stem(),
                    frame.user_id,
                    frame.channels.max(1)
                ));
                let spec = hound::WavSpec {
                    channels: frame.channels.max(1),
                    sample_rate: MIX_SAMPLE_RATE,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                };
                let writer = hound::WavWriter::create(&path, spec)
                    .with_context(|| format!("create WAV writer {}", path.display()))?;
                entry.insert(writer)
            }
        };
        for sample in frame.samples {
            writer.write_sample(sample)?;
        }
    }

    for (_, writer) in writers {
        writer.finalize()?;
    }
    Ok(())
}

async fn tap_debug_audio(
    state: &ServerState,
    kind: ServerDebugAudioKind,
    user_id: UserId,
    channels: u16,
    samples: &[i16],
) {
    let tx = state.debug_audio_tx.read().await.clone();
    let Some(tx) = tx else {
        return;
    };
    let _ = tx.try_send(ServerDebugAudioFrame {
        kind,
        user_id,
        channels: channels.max(1),
        samples: samples.to_vec(),
    });
}

async fn record_input_health(
    state: &ServerState,
    user_id: UserId,
    target: AudioTarget,
    samples: &[i16],
) {
    let now = Instant::now();
    let meter = measure_i16(samples);
    let mut health = state.health.write().await;
    let user_health = health.entry(user_id).or_default();
    user_health.input.peak = meter.peak;
    user_health.input.rms = meter.rms;
    user_health.input.last_channel = target.channel_id();
    user_health.input.last_packet_age_ms = Some(0);
    user_health.input.active = meter.rms >= ACTIVE_TALKER_RMS_THRESHOLD;
    user_health.last_packet_seen = Some(now);
    if meter.rms >= ACTIVE_TALKER_RMS_THRESHOLD {
        user_health.active_until = now + ACTIVE_TALKER_HOLD;
    }
}

async fn transcribe_ingest_frame(
    state: &Arc<ServerState>,
    user_id: UserId,
    target: AudioTarget,
    samples: &[i16],
) {
    let mut should_spawn = false;
    {
        let mut transcription = state.transcription.write().await;
        if !transcription.active
            || transcription
                .users
                .as_ref()
                .is_some_and(|users| !users.contains(&user_id))
        {
            return;
        }

        let runtime = transcription.per_user.entry(user_id).or_default();
        if let Some(job) = runtime.chunker.push_frame(user_id, target, samples) {
            runtime.last_contexts = job.contexts.clone();
            if runtime.pending.len() >= LIVE_TRANSCRIPTION_QUEUE_LIMIT {
                runtime.dropped_jobs += 1;
                runtime.dropped_frames += frames_in_live_job(&job) as u64;
                transcription.last_error = Some(format!(
                    "live transcription queue full for user {user_id}; dropped chunk"
                ));
            } else {
                runtime.pending.push_back(job);
                runtime.queued_jobs = runtime.pending.len();
                if !runtime.worker_running {
                    runtime.worker_running = true;
                    should_spawn = true;
                }
            }
        }
    }

    if should_spawn {
        tokio::spawn(run_live_transcription_worker(Arc::clone(state), user_id));
    }
}

async fn record_ingest_frame(
    state: &ServerState,
    user_id: UserId,
    target: AudioTarget,
    codec: Option<Codec>,
    samples: &[i16],
) {
    let (user_name, talk_mode, session_codec) = {
        let sessions = state.sessions.read().await;
        sessions
            .get(&user_id)
            .map_or((String::new(), TalkMode::Ptt, Codec::Pcm16), |session| {
                (
                    session.name.clone(),
                    session.talk_mode,
                    session.output_codec,
                )
            })
    };
    let codec = codec.unwrap_or(session_codec);
    let meter = measure_i16(samples);
    let mut recording = state.recording.write().await;
    let Some(active) = recording.active.as_mut() else {
        return;
    };
    if active
        .users
        .as_ref()
        .is_some_and(|users| !users.contains(&user_id))
    {
        return;
    }
    active.contexts.entry(user_id).or_default().push(target);
    if active
        .contexts
        .get(&user_id)
        .is_some_and(|contexts| contexts.len() > 64)
    {
        active.contexts.get_mut(&user_id).unwrap().drain(0..32);
    }

    if let Err(err) = ensure_recording_writer(active, user_id).and_then(|writer| {
        for sample in samples {
            writer.write_sample(*sample)?;
        }
        Ok(())
    }) {
        recording.last_engine_error = Some(format!("record user {user_id}: {err}"));
        tracing::warn!(user_id, %err, "failed to record ingest frame");
        return;
    }
    let metadata = RecordingMetadataEvent {
        kind: "ingest_frame",
        timestamp_ms: unix_time_ms(),
        session_id: active.id.clone(),
        frame_index: active.frames_recorded,
        user_id,
        user_name,
        target,
        codec,
        talk_mode,
        peak: meter.peak,
        rms: meter.rms,
    };
    let metadata_error = match serde_json::to_string(&metadata) {
        Ok(line) => {
            if let Err(err) = writeln!(active.metadata_writer, "{line}") {
                tracing::warn!(user_id, %err, "failed to record metadata event");
                Some(format!("record metadata user {user_id}: {err}"))
            } else {
                None
            }
        }
        Err(err) => {
            tracing::warn!(user_id, %err, "failed to serialize recording metadata event");
            Some(format!("serialize metadata user {user_id}: {err}"))
        }
    };
    active.frames_recorded += 1;
    if let Some(err) = metadata_error {
        recording.last_engine_error = Some(err);
    }
}

fn ensure_recording_writer(
    active: &mut ActiveRecordingSession,
    user_id: UserId,
) -> anyhow::Result<&mut hound::WavWriter<BufWriter<File>>> {
    if !active.writers.contains_key(&user_id) {
        let path = active.dir.join(format!("user-{user_id}.wav"));
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: MIX_SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(&path, spec)
            .with_context(|| format!("create WAV writer {}", path.display()))?;
        active.writers.insert(user_id, writer);
    }
    active
        .writers
        .get_mut(&user_id)
        .context("recording writer was not initialized")
}

impl LiveTranscriptChunker {
    fn push_frame(
        &mut self,
        user_id: UserId,
        target: AudioTarget,
        samples: &[i16],
    ) -> Option<LiveTranscriptJob> {
        let samples_16khz = resample_linear(samples, MIX_SAMPLE_RATE, common::SAMPLE_RATE);
        let meter = measure_i16(&samples_16khz);
        let voiced = meter.rms >= LIVE_TRANSCRIPTION_RMS_THRESHOLD;

        if self.buffer.is_empty() && !voiced {
            return None;
        }

        let frame_start = unix_time_ms();
        if self.started_at_ms.is_none() {
            self.started_at_ms = Some(frame_start);
        }
        self.buffer.extend_from_slice(&samples_16khz);
        if !self.contexts.contains(&target) {
            self.contexts.push(target);
        }
        if self.contexts.len() > 16 {
            self.contexts.drain(0..8);
        }

        self.total_frames += 1;
        if voiced {
            self.voiced_frames += 1;
            self.silence_frames = 0;
        } else if self.voiced_frames > 0 {
            self.silence_frames += 1;
        }

        if self.voiced_frames == 0 {
            return None;
        }

        let end_for_silence = self.silence_frames >= LIVE_TRANSCRIPTION_SILENCE_FRAMES;
        let end_for_length = self.total_frames >= LIVE_TRANSCRIPTION_MAX_FRAMES;
        if end_for_silence || end_for_length {
            return self.finalize(user_id, end_for_length);
        }

        None
    }

    fn flush(&mut self, user_id: UserId) -> Option<LiveTranscriptJob> {
        if self.voiced_frames == 0 {
            self.clear();
            return None;
        }
        self.finalize(user_id, false)
    }

    fn finalize(&mut self, user_id: UserId, keep_overlap: bool) -> Option<LiveTranscriptJob> {
        if self.voiced_frames < LIVE_TRANSCRIPTION_MIN_SPEECH_FRAMES {
            self.clear();
            return None;
        }

        let started_at_ms = self.started_at_ms.unwrap_or_else(unix_time_ms);
        let duration_ms = (self.total_frames as u64) * common::FRAME_MS as u64;
        let ended_at_ms = started_at_ms.saturating_add(duration_ms);
        let job = LiveTranscriptJob {
            user_id,
            started_at_ms,
            ended_at_ms,
            contexts: self.contexts.clone(),
            samples_16khz: self.buffer.clone(),
        };

        if keep_overlap {
            let overlap_samples = LIVE_TRANSCRIPTION_OVERLAP_FRAMES * common::SAMPLES_PER_FRAME;
            let keep = overlap_samples.min(self.buffer.len());
            let overlap = self.buffer[self.buffer.len().saturating_sub(keep)..].to_vec();
            let contexts = self.contexts.clone();
            self.buffer = overlap;
            self.contexts = contexts;
            self.started_at_ms = Some(ended_at_ms.saturating_sub(
                (LIVE_TRANSCRIPTION_OVERLAP_FRAMES as u64) * common::FRAME_MS as u64,
            ));
            self.voiced_frames = LIVE_TRANSCRIPTION_OVERLAP_FRAMES.min(self.total_frames);
            self.silence_frames = 0;
            self.total_frames = LIVE_TRANSCRIPTION_OVERLAP_FRAMES.min(self.total_frames);
        } else {
            self.clear();
        }

        Some(job)
    }

    fn clear(&mut self) {
        self.buffer.clear();
        self.contexts.clear();
        self.started_at_ms = None;
        self.voiced_frames = 0;
        self.silence_frames = 0;
        self.total_frames = 0;
    }
}

fn frames_in_live_job(job: &LiveTranscriptJob) -> usize {
    job.samples_16khz.len() / common::SAMPLES_PER_FRAME
}

#[cfg(feature = "transcription-whisper")]
async fn run_live_transcription_worker(state: Arc<ServerState>, user_id: UserId) {
    loop {
        let (job, context) = {
            let mut transcription = state.transcription.write().await;
            let context = transcription.whisper_context.clone();
            let Some(runtime) = transcription.per_user.get_mut(&user_id) else {
                return;
            };
            let Some(job) = runtime.pending.pop_front() else {
                runtime.worker_running = false;
                runtime.queued_jobs = 0;
                return;
            };
            runtime.queued_jobs = runtime.pending.len();
            let Some(context) = context else {
                runtime.worker_running = false;
                return;
            };
            (job, context)
        };

        let result = transcribe_live_job_with_builtin(context, job.clone()).await;
        match result {
            Ok(Some(completed)) => {
                if let Err(err) = append_transcript_segment(
                    &state,
                    TranscriptAppend {
                        user_id: completed.user_id,
                        contexts: completed.contexts,
                        started_at_ms: Some(completed.started_at_ms),
                        ended_at_ms: Some(completed.ended_at_ms),
                        text: completed.text,
                        confidence: None,
                        engine: "builtin_whisper".to_string(),
                        source: TranscriptSource::Live,
                        final_segment: true,
                    },
                )
                .await
                {
                    state.transcription.write().await.last_error = Some(format!("{err:?}"));
                } else if let Some(runtime) =
                    state.transcription.write().await.per_user.get_mut(&user_id)
                {
                    runtime.completed_segments += 1;
                }
            }
            Ok(None) => {}
            Err(err) => {
                let message = format!("live transcription user {user_id}: {err}");
                state.transcription.write().await.last_error = Some(message.clone());
                tracing::warn!(user_id, %err, "live transcription failed");
            }
        }
    }
}

#[cfg(not(feature = "transcription-whisper"))]
async fn run_live_transcription_worker(state: Arc<ServerState>, user_id: UserId) {
    let mut transcription = state.transcription.write().await;
    if let Some(runtime) = transcription.per_user.get_mut(&user_id) {
        runtime.worker_running = false;
    }
    transcription.last_error = Some("server was built without transcription-whisper".to_string());
}

#[cfg(feature = "transcription-whisper")]
#[derive(Debug, Clone)]
struct CompletedLiveTranscript {
    user_id: UserId,
    started_at_ms: u64,
    ended_at_ms: u64,
    contexts: Vec<AudioTarget>,
    text: String,
}

#[cfg(feature = "transcription-whisper")]
trait LiveTranscriptionEngine {
    fn transcribe(&mut self, samples_16khz: &[i16]) -> anyhow::Result<String>;
}

#[cfg(feature = "transcription-whisper")]
fn transcribe_live_job_with_engine(
    engine: &mut impl LiveTranscriptionEngine,
    job: LiveTranscriptJob,
) -> anyhow::Result<Option<CompletedLiveTranscript>> {
    let text = engine.transcribe(&job.samples_16khz)?;
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some(CompletedLiveTranscript {
        user_id: job.user_id,
        started_at_ms: job.started_at_ms,
        ended_at_ms: job.ended_at_ms,
        contexts: job.contexts,
        text: text.to_string(),
    }))
}

#[cfg(feature = "transcription-whisper")]
async fn transcribe_live_job_with_builtin(
    context: Arc<WhisperContext>,
    job: LiveTranscriptJob,
) -> anyhow::Result<Option<CompletedLiveTranscript>> {
    tokio::task::spawn_blocking(move || {
        let mut engine = BuiltinWhisperEngine { context };
        transcribe_live_job_with_engine(&mut engine, job)
    })
    .await
    .context("join live transcription task")?
}

#[cfg(feature = "transcription-whisper")]
struct BuiltinWhisperEngine {
    context: Arc<WhisperContext>,
}

#[cfg(feature = "transcription-whisper")]
impl LiveTranscriptionEngine for BuiltinWhisperEngine {
    fn transcribe(&mut self, samples_16khz: &[i16]) -> anyhow::Result<String> {
        let mut state = self
            .context
            .create_state()
            .context("create Whisper state")?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_translate(false);
        params.set_no_context(true);
        params.set_single_segment(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_n_threads(2);
        let audio = samples_16khz
            .iter()
            .map(|sample| *sample as f32 / i16::MAX as f32)
            .collect::<Vec<_>>();
        state.full(params, &audio).context("run Whisper")?;
        Ok(state
            .as_iter()
            .map(|segment| segment.to_string())
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string())
    }
}

async fn record_output_health(
    state: &ServerState,
    user_id: UserId,
    meter: &OutputMeterStatus,
    limiter_event: bool,
) {
    let mut health = state.health.write().await;
    let user_health = health.entry(user_id).or_default();
    let previous_events = user_health.output.limiter_events;
    user_health.output = meter.clone();
    user_health.output.limiter_events = previous_events + u64::from(limiter_event);
}

#[derive(Debug)]
struct MixOutput {
    user_id: UserId,
    addr: SocketAddr,
    codec: Codec,
    opus_profile: OpusProfile,
    samples: Vec<i16>,
    channels: usize,
    active_sources: usize,
    meter: OutputMeterStatus,
    limiter_event: bool,
}

#[derive(Debug, Clone, Copy)]
struct FrameMeter {
    peak: f32,
    rms: f32,
}

fn measure_i16(samples: &[i16]) -> FrameMeter {
    if samples.is_empty() {
        return FrameMeter {
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

    FrameMeter {
        peak,
        rms: (sum_squares / samples.len() as f64).sqrt() as f32,
    }
}

fn measure_f32(samples: &[f32]) -> FrameMeter {
    if samples.is_empty() {
        return FrameMeter {
            peak: 0.0,
            rms: 0.0,
        };
    }

    let mut peak = 0.0_f32;
    let mut sum_squares = 0.0_f64;
    for sample in samples {
        let normalized = sample.abs();
        peak = peak.max(normalized);
        sum_squares += f64::from(normalized * normalized);
    }

    FrameMeter {
        peak,
        rms: (sum_squares / samples.len() as f64).sqrt() as f32,
    }
}

fn limit_frame(samples: &[f32]) -> (Vec<i16>, OutputMeterStatus, bool) {
    let input = measure_f32(samples);
    let limiter_gain = if input.peak > LIMITER_THRESHOLD {
        LIMITER_THRESHOLD / input.peak
    } else {
        1.0
    };
    let limiter_event = limiter_gain < 1.0;
    let limited = samples
        .iter()
        .map(|sample| {
            (sample * limiter_gain * i16::MAX as f32)
                .round()
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16
        })
        .collect::<Vec<_>>();
    let output = measure_i16(&limited);
    let reduction_db = if limiter_gain < 1.0 {
        -20.0 * limiter_gain.log10()
    } else {
        0.0
    };

    (
        limited,
        OutputMeterStatus {
            peak: output.peak,
            rms: output.rms,
            limiter_gain,
            limiter_reduction_db: reduction_db,
            limiter_events: 0,
        },
        limiter_event,
    )
}

fn limit_stereo_frame(left: &[f32], right: &[f32]) -> (Vec<i16>, OutputMeterStatus, bool) {
    let peak = left
        .iter()
        .chain(right.iter())
        .map(|sample| sample.abs())
        .fold(0.0_f32, f32::max);
    let limiter_gain = if peak > LIMITER_THRESHOLD {
        LIMITER_THRESHOLD / peak
    } else {
        1.0
    };
    let limiter_event = limiter_gain < 1.0;
    let mut limited = Vec::with_capacity(left.len() * 2);
    for (left, right) in left.iter().zip(right) {
        limited.push(
            (left * limiter_gain * i16::MAX as f32)
                .round()
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16,
        );
        limited.push(
            (right * limiter_gain * i16::MAX as f32)
                .round()
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16,
        );
    }
    let output = measure_i16(&limited);
    let reduction_db = if limiter_gain < 1.0 {
        -20.0 * limiter_gain.log10()
    } else {
        0.0
    };

    (
        limited,
        OutputMeterStatus {
            peak: output.peak,
            rms: output.rms,
            limiter_gain,
            limiter_reduction_db: reduction_db,
            limiter_events: 0,
        },
        limiter_event,
    )
}

fn pan_gains(pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    if pan < 0.0 {
        (1.0, 1.0 + pan)
    } else {
        (1.0 - pan, 1.0)
    }
}

fn is_server_virtual_source(user_id: UserId) -> bool {
    user_id == SERVER_USER_ID || user_id >= TTS_SOURCE_USER_BASE
}

async fn build_mixes(state: &ServerState) -> Vec<MixOutput> {
    let now = Instant::now();
    let emergency = state.emergency.read().await.clone();
    let mut sessions = state.sessions.write().await;
    let mut sources = state.sources.write().await;
    let mut virtual_sources = state.virtual_sources.write().await;
    let mut source_frames = Vec::new();

    let mut expired_queues = 0_u64;
    sources.retain(|user_id, source| {
        let active = now.duration_since(source.last_seen) <= ACTIVE_SOURCE_WINDOW;
        if !active {
            expired_queues += 1;
            tracing::debug!(user_id, "expired inactive source queue");
        }
        active
    });
    if expired_queues > 0 {
        state
            .metrics
            .expired_source_queues
            .fetch_add(expired_queues, Ordering::Relaxed);
    }

    for (&user_id, source) in sources.iter_mut() {
        let Some(frame) = source.pop() else {
            continue;
        };
        source_frames.push(ActiveSourceFrame {
            user_id,
            target: frame.target,
            samples: frame.samples,
            priority: frame.priority,
            duck: frame.duck,
        });
    }
    for source in virtual_sources.iter_mut() {
        if let Some(frame) = source.next_frame() {
            source_frames.push(frame);
        }
    }
    virtual_sources.retain(|source| !source.is_finished());
    drop(virtual_sources);
    drop(sources);

    let mut sender_routes = sessions
        .iter()
        .map(|(&user_id, session)| {
            (
                user_id,
                (
                    session.priority,
                    session.priority_channels.clone(),
                    session.active_tx_targets(),
                ),
            )
        })
        .collect::<HashMap<_, _>>();
    for source in &source_frames {
        if !is_server_virtual_source(source.user_id) {
            continue;
        }
        let entry = sender_routes
            .entry(source.user_id)
            .or_insert_with(|| (false, HashSet::new(), HashSet::new()));
        entry.0 |= source.priority;
        if source.priority {
            if let AudioTarget::Channel(channel_id) = source.target {
                entry.1.insert(channel_id);
            }
        }
        entry.2.insert(source.target);
    }
    let emergency_recipients = emergency
        .as_ref()
        .map(|emergency| emergency_recipients_for_sessions(emergency, &sessions))
        .unwrap_or_default()
        .into_iter()
        .collect::<HashSet<_>>();
    let mut direct_duck_by_listener = HashSet::new();
    for source in &source_frames {
        if is_server_virtual_source(source.user_id) && source.duck {
            if let AudioTarget::Direct(target_user_id) = source.target {
                direct_duck_by_listener.insert(target_user_id);
            }
        }
    }
    for (&caller_id, session) in sessions.iter() {
        for (&target_user_id, call) in &session.active_direct_calls {
            if call.duck
                && source_frames.iter().any(|source| {
                    source.user_id == caller_id
                        && source.target == AudioTarget::Direct(target_user_id)
                })
            {
                direct_duck_by_listener.insert(target_user_id);
            }
        }
        for button in &session.buttons {
            if !session.active_buttons.contains(&button.id) {
                continue;
            }
            for action in &button.actions {
                let TalkButtonAction::Transmit { users, duck, .. } = action else {
                    continue;
                };
                if !duck {
                    continue;
                }
                for &target_user_id in users {
                    if source_frames.iter().any(|source| {
                        source.user_id == caller_id
                            && source.target == AudioTarget::Direct(target_user_id)
                    }) {
                        direct_duck_by_listener.insert(target_user_id);
                    }
                }
            }
        }
    }

    sessions
        .iter_mut()
        .filter_map(|(&listener_id, listener)| {
            let addr = listener.addr?;
            let active_priority_channels = source_frames
                .iter()
                .filter_map(|source| {
                    let AudioTarget::Channel(channel_id) = source.target else {
                        return None;
                    };
                    if source.user_id == listener_id
                        || !listener.listen_channels.contains(&channel_id)
                    {
                        return None;
                    }

                    if sender_routes.get(&source.user_id).is_some_and(|sender| {
                        sender.0
                            && sender.1.contains(&channel_id)
                            && sender.2.contains(&source.target)
                    }) {
                        Some(channel_id)
                    } else {
                        None
                    }
                })
                .collect::<HashSet<_>>();
            let ifb_active = listener.ifb.enabled
                && source_frames.iter().any(|source| {
                    let AudioTarget::Channel(channel_id) = source.target else {
                        return false;
                    };
                    if source.user_id == listener_id
                        || !listener.listen_channels.contains(&channel_id)
                        || !listener.ifb.interrupt.contains(&channel_id)
                    {
                        return false;
                    }

                    sender_routes
                        .get(&source.user_id)
                        .is_some_and(|sender| sender.2.contains(&source.target))
                });
            let direct_duck_active = direct_duck_by_listener.contains(&listener_id);
            let emergency_active_for_listener = emergency.as_ref().is_some_and(|emergency| {
                emergency_recipients.contains(&listener_id) && emergency.source != listener_id
            });
            listener.ifb_status = IfbStatus {
                active: ifb_active,
                duck_gain: if ifb_active {
                    listener.ifb.duck_gain
                } else {
                    1.0
                },
            };
            let stereo_active = listener.stereo.active_for_codec(listener.output_codec);
            let mut mix = [0.0_f32; MIX_SAMPLES_PER_FRAME];
            let mut mix_left = [0.0_f32; MIX_SAMPLES_PER_FRAME];
            let mut mix_right = [0.0_f32; MIX_SAMPLES_PER_FRAME];
            let mut active_sources = 0;

            for source in &source_frames {
                if source.user_id == listener_id || source.samples.len() != MIX_SAMPLES_PER_FRAME {
                    continue;
                }

                let sender = match sender_routes.get(&source.user_id) {
                    Some(sender) => sender,
                    None => continue,
                };

                let mut is_emergency_source = false;
                let channel_id = match source.target {
                    AudioTarget::Channel(channel_id) => {
                        if !listener.listen_channels.contains(&channel_id)
                            || !sender.2.contains(&source.target)
                        {
                            continue;
                        }
                        Some(channel_id)
                    }
                    AudioTarget::Direct(target_user_id) => {
                        if target_user_id != listener_id || !sender.2.contains(&source.target) {
                            continue;
                        }
                        None
                    }
                    AudioTarget::Mixed => {
                        let Some(emergency) = emergency.as_ref() else {
                            continue;
                        };
                        if emergency.source != source.user_id
                            || !emergency_recipients.contains(&listener_id)
                        {
                            continue;
                        }
                        is_emergency_source = true;
                        None
                    }
                };

                let priority_duck_gain = if channel_id
                    .is_some_and(|channel_id| active_priority_channels.contains(&channel_id))
                    && !channel_id
                        .is_some_and(|channel_id| sender.0 && sender.1.contains(&channel_id))
                {
                    PRIORITY_DUCK_GAIN
                } else {
                    1.0
                };
                let ifb_duck_gain = if channel_id.is_some_and(|channel_id| {
                    ifb_active && listener.ifb.program.contains(&channel_id)
                }) {
                    listener.ifb.duck_gain
                } else {
                    1.0
                };
                let direct_duck_gain =
                    if direct_duck_active && source.target != AudioTarget::Direct(listener_id) {
                        DEFAULT_IFB_DUCK_GAIN
                    } else {
                        1.0
                    };
                let emergency_duck_gain = if emergency_active_for_listener && !is_emergency_source {
                    if emergency
                        .as_ref()
                        .is_some_and(|emergency| emergency.mute_others)
                    {
                        0.0
                    } else {
                        emergency
                            .as_ref()
                            .map_or(1.0, |emergency| emergency.duck_gain)
                    }
                } else {
                    1.0
                };
                if emergency_duck_gain == 0.0 {
                    continue;
                }

                let gain = listener
                    .channel_volumes
                    .get(&channel_id.unwrap_or_default())
                    .copied()
                    .unwrap_or(1.0)
                    .max(0.0)
                    * listener
                        .talker_volumes
                        .get(&source.user_id)
                        .copied()
                        .unwrap_or(1.0)
                        .max(0.0)
                    * priority_duck_gain
                    * ifb_duck_gain
                    * direct_duck_gain
                    * emergency_duck_gain;

                if stereo_active {
                    let pan = channel_id
                        .and_then(|channel_id| {
                            listener.stereo.channel_pan.get(&channel_id).copied()
                        })
                        .unwrap_or(0.0);
                    let (left_gain, right_gain) = pan_gains(pan);
                    for ((mixed_left, mixed_right), sample) in mix_left
                        .iter_mut()
                        .zip(mix_right.iter_mut())
                        .zip(&source.samples)
                    {
                        let sample = (*sample as f32 / i16::MAX as f32) * gain;
                        *mixed_left += sample * left_gain;
                        *mixed_right += sample * right_gain;
                    }
                } else {
                    for (mixed, sample) in mix.iter_mut().zip(&source.samples) {
                        *mixed += (*sample as f32 / i16::MAX as f32) * gain;
                    }
                }
                active_sources += 1;
            }

            if active_sources == 0 {
                return None;
            }

            let (samples, meter, limiter_event, channels) = if stereo_active {
                let (samples, meter, limiter_event) = limit_stereo_frame(&mix_left, &mix_right);
                (samples, meter, limiter_event, 2)
            } else {
                let (samples, meter, limiter_event) = limit_frame(&mix);
                (samples, meter, limiter_event, 1)
            };

            Some(MixOutput {
                user_id: listener_id,
                addr,
                codec: listener.output_codec,
                opus_profile: listener.opus_profile,
                samples,
                channels,
                active_sources,
                meter,
                limiter_event,
            })
        })
        .collect()
}

#[derive(Debug)]
struct ActiveSourceFrame {
    user_id: UserId,
    target: AudioTarget,
    samples: Vec<i16>,
    priority: bool,
    duck: bool,
}

const ADMIN_DASHBOARD_HTML: &str = include_str!("../admin/dashboard.html");
const ADMIN_CLIENTS_HTML: &str = include_str!("../admin/clients.html");
const ADMIN_ROUTING_HTML: &str = include_str!("../admin/routing.html");
const ADMIN_PRESETS_HTML: &str = include_str!("../admin/presets.html");
const ADMIN_CALLS_HTML: &str = include_str!("../admin/calls.html");
const ADMIN_RECORDING_HTML: &str = include_str!("../admin/recording.html");
const ADMIN_SYSTEM_HTML: &str = include_str!("../admin/system.html");
const ADMIN_JS: &str = include_str!("../admin/app.js");
const ADMIN_CSS: &str = include_str!("../admin/style.css");
const ADMIN_LOGO_PNG: &[u8] = include_bytes!("../admin/branding/redline-logo.png");

async fn next_output_seq(state: &ServerState, user_id: UserId) -> u16 {
    let mut output_seq = state.output_seq.write().await;
    let seq = output_seq.entry(user_id).or_insert(0);
    let current = *seq;
    *seq = seq.wrapping_add(1);
    current
}

#[derive(Default)]
struct AudioProcessorBank {
    high_pass: HashMap<UserId, HighPassState>,
    vad: HashMap<UserId, ServerVadState>,
    normalizers: HashMap<UserId, LevelNormalizerState>,
    #[cfg(feature = "processing-rnnoise")]
    rnnoise: HashMap<UserId, RnNoiseProcessorState>,
    #[cfg(feature = "processing-webrtc")]
    webrtc: HashMap<UserId, WebRtcProcessorState>,
    #[cfg(feature = "processing-deepfilternet")]
    deepfilternet: HashMap<UserId, DeepFilterNetWorkerState>,
}

#[derive(Debug, Clone, Copy, Default)]
struct HighPassState {
    previous_input: f32,
    previous_output: f32,
}

#[derive(Debug, Clone, Copy, Default)]
struct ServerVadState {
    open: bool,
    speech_frames: usize,
    hold_frames: usize,
    gain: f32,
}

#[derive(Debug, Clone, Copy)]
struct LevelNormalizerState {
    gain: f32,
    initialized: bool,
}

impl Default for LevelNormalizerState {
    fn default() -> Self {
        Self {
            gain: 1.0,
            initialized: false,
        }
    }
}

#[cfg(feature = "processing-rnnoise")]
struct RnNoiseProcessorState {
    denoise: Box<nnnoiseless::DenoiseState<'static>>,
    input: [f32; MIX_SAMPLES_PER_FRAME],
    output: [f32; MIX_SAMPLES_PER_FRAME],
    warmed: bool,
}

#[cfg(feature = "processing-webrtc")]
struct WebRtcProcessorState {
    processor: webrtc_audio_processing::Processor,
    frame: [f32; MIX_SAMPLES_PER_FRAME],
    config_key: Option<WebRtcConfigKey>,
}

#[cfg(feature = "processing-deepfilternet")]
struct DeepFilterNetFrameProcessor {
    runtime: DeepFilterNetRuntime,
    input: Array2<f32>,
    output: Array2<f32>,
    key: DeepFilterNetConfigKey,
    active_backend: DeepFilterBackend,
    fallback_reason: Option<String>,
}

#[cfg(feature = "processing-deepfilternet")]
enum DeepFilterNetRuntime {
    Tract(DfTract),
    #[cfg(all(feature = "processing-deepfilternet-coreml", target_os = "macos"))]
    CoreMl(deepfilternet_coreml::CoreMlDeepFilterNet),
}

#[cfg(feature = "processing-deepfilternet")]
struct DeepFilterNetWorkerState {
    key: DeepFilterNetConfigKey,
    tx: std_mpsc::SyncSender<DeepFilterNetRequest>,
    rx: std_mpsc::Receiver<DeepFilterNetResponse>,
    next_id: u64,
}

#[cfg(feature = "processing-deepfilternet")]
struct DeepFilterNetRequest {
    id: u64,
    samples: [i16; MIX_SAMPLES_PER_FRAME],
}

#[cfg(feature = "processing-deepfilternet")]
struct DeepFilterNetResponse {
    id: u64,
    result: Result<DeepFilterNetFrameResult, String>,
}

#[cfg(feature = "processing-deepfilternet")]
struct DeepFilterNetFrameResult {
    samples: [i16; MIX_SAMPLES_PER_FRAME],
    lsnr_db: f32,
    lookahead_frames: usize,
    model_name: String,
    requested_backend: DeepFilterBackend,
    active_backend: DeepFilterBackend,
    compute_units: AppleComputeUnits,
    inference_ms: f32,
    fallback_reason: Option<String>,
}

#[cfg(feature = "processing-webrtc")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WebRtcConfigKey {
    profile: ProcessingProfile,
    high_pass: bool,
    noise_gate: bool,
    compressor: bool,
    vad: bool,
    transient_suppression: bool,
}

#[cfg(feature = "processing-deepfilternet")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct DeepFilterNetConfigKey {
    model_path: PathBuf,
    profile: ProcessingProfile,
    backend: DeepFilterBackend,
    compute_units: AppleComputeUnits,
}

struct BuiltInProcessingContext {
    user_id: UserId,
    codec: Codec,
    input: FrameMeter,
    engine: ProcessingEngine,
    engine_available: bool,
    engine_detail: Option<String>,
}

#[cfg(feature = "processing-rnnoise")]
impl RnNoiseProcessorState {
    fn new() -> Self {
        Self {
            denoise: nnnoiseless::DenoiseState::new(),
            input: [0.0; MIX_SAMPLES_PER_FRAME],
            output: [0.0; MIX_SAMPLES_PER_FRAME],
            warmed: false,
        }
    }
}

#[cfg(feature = "processing-webrtc")]
impl WebRtcProcessorState {
    fn new(config: &ProcessingConfig) -> Result<Self, webrtc_audio_processing::Error> {
        let processor = webrtc_audio_processing::Processor::new(MIX_SAMPLE_RATE)?;
        let mut state = Self {
            processor,
            frame: [0.0; MIX_SAMPLES_PER_FRAME],
            config_key: None,
        };
        state.apply_config(config);
        Ok(state)
    }

    fn apply_config(&mut self, config: &ProcessingConfig) {
        let key = WebRtcConfigKey::from(config);
        if self.config_key == Some(key) {
            return;
        }

        self.processor.set_config(webrtc_config(config));
        self.config_key = Some(key);
    }
}

#[cfg(feature = "processing-webrtc")]
impl From<&ProcessingConfig> for WebRtcConfigKey {
    fn from(config: &ProcessingConfig) -> Self {
        Self {
            profile: config.profile,
            high_pass: config.high_pass,
            noise_gate: config.noise_gate,
            compressor: config.compressor,
            vad: config.vad,
            transient_suppression: config.transient_suppression,
        }
    }
}

#[cfg(feature = "processing-deepfilternet")]
impl DeepFilterNetFrameProcessor {
    fn new(key: DeepFilterNetConfigKey) -> anyhow::Result<Self> {
        let runtime_params = deepfilternet_runtime_params(key.profile);
        let (active_backend, fallback_reason) =
            deepfilternet_select_runtime_backend(key.backend, &key.model_path);
        let runtime = match active_backend {
            #[cfg(all(feature = "processing-deepfilternet-coreml", target_os = "macos"))]
            DeepFilterBackend::CoreMl => {
                let params = deepfilternet_coreml::CoreMlRuntimeParams {
                    post_filter: runtime_params.post_filter,
                    post_filter_beta: runtime_params.post_filter_beta,
                    atten_lim_db: runtime_params.atten_lim_db,
                    min_db_thresh: runtime_params.min_db_thresh,
                    max_db_erb_thresh: runtime_params.max_db_erb_thresh,
                    max_db_df_thresh: runtime_params.max_db_df_thresh,
                };
                DeepFilterNetRuntime::CoreMl(deepfilternet_coreml::CoreMlDeepFilterNet::new(
                    &key.model_path,
                    key.compute_units,
                    &params,
                )?)
            }
            _ => {
                if is_supported_deepfilternet_coreml_package_path(&key.model_path) {
                    bail!(
                        "{}",
                        fallback_reason.clone().unwrap_or_else(|| {
                            "DeepFilterNet Core ML package selected, but Core ML runtime is unavailable"
                                .to_string()
                        })
                    );
                }
                let df_params = DfParams::new(key.model_path.clone())?;
                let model = DfTract::new(df_params, &runtime_params)?;
                if model.sr != MIX_SAMPLE_RATE as usize || model.hop_size != MIX_SAMPLES_PER_FRAME {
                    bail!(
                        "DeepFilterNet model must run at {} Hz with {}-sample frames; model is {} Hz / {} samples",
                        MIX_SAMPLE_RATE,
                        MIX_SAMPLES_PER_FRAME,
                        model.sr,
                        model.hop_size
                    );
                }
                DeepFilterNetRuntime::Tract(model)
            }
        };
        Ok(Self {
            runtime,
            input: Array2::zeros((1, MIX_SAMPLES_PER_FRAME)),
            output: Array2::zeros((1, MIX_SAMPLES_PER_FRAME)),
            key,
            active_backend,
            fallback_reason,
        })
    }

    fn process(
        &mut self,
        samples: [i16; MIX_SAMPLES_PER_FRAME],
    ) -> anyhow::Result<DeepFilterNetFrameResult> {
        #[cfg(all(feature = "processing-deepfilternet-coreml", target_os = "macos"))]
        if let DeepFilterNetRuntime::CoreMl(model) = &mut self.runtime {
            let started = Instant::now();
            let result = model.process(samples)?;
            let inference_ms = started.elapsed().as_secs_f32() * 1_000.0;
            return Ok(DeepFilterNetFrameResult {
                samples: result.samples,
                lsnr_db: result.lsnr_db,
                lookahead_frames: result.lookahead_frames,
                model_name: result.model_name,
                requested_backend: self.key.backend,
                active_backend: self.active_backend,
                compute_units: self.key.compute_units,
                inference_ms,
                fallback_reason: self.fallback_reason.clone(),
            });
        }

        let model = match &mut self.runtime {
            DeepFilterNetRuntime::Tract(model) => model,
            #[cfg(all(feature = "processing-deepfilternet-coreml", target_os = "macos"))]
            DeepFilterNetRuntime::CoreMl(_) => {
                bail!("DeepFilterNet Core ML runtime was not handled on this platform")
            }
        };
        let input_slice = self
            .input
            .as_slice_mut()
            .expect("DeepFilterNet input array is contiguous");
        for (dst, sample) in input_slice.iter_mut().zip(samples.iter()) {
            *dst = *sample as f32 / i16::MAX as f32;
        }
        let started = Instant::now();
        let lsnr_db = model.process(self.input.view(), self.output.view_mut())?;
        let inference_ms = started.elapsed().as_secs_f32() * 1_000.0;
        let output_slice = self
            .output
            .as_slice()
            .expect("DeepFilterNet output array is contiguous");
        let mut output = [0_i16; MIX_SAMPLES_PER_FRAME];
        for (sample, processed) in output.iter_mut().zip(output_slice.iter()) {
            *sample = (*processed * i16::MAX as f32)
                .round()
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }

        Ok(DeepFilterNetFrameResult {
            samples: output,
            lsnr_db,
            lookahead_frames: model.lookahead,
            model_name: self
                .key
                .model_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("model")
                .to_string(),
            requested_backend: self.key.backend,
            active_backend: self.active_backend,
            compute_units: self.key.compute_units,
            inference_ms,
            fallback_reason: self.fallback_reason.clone(),
        })
    }
}

#[cfg(feature = "processing-deepfilternet")]
impl DeepFilterNetWorkerState {
    fn spawn(key: DeepFilterNetConfigKey, queue_frames: usize) -> Self {
        let queue_frames = queue_frames.clamp(1, 200);
        let (tx, worker_rx) = std_mpsc::sync_channel::<DeepFilterNetRequest>(queue_frames);
        let (worker_tx, rx) = std_mpsc::sync_channel::<DeepFilterNetResponse>(queue_frames);
        let worker_key = key.clone();
        let _ = std::thread::Builder::new()
            .name(format!("ic-dfn-{}", std::process::id()))
            .spawn(move || run_deepfilternet_worker(worker_key, worker_rx, worker_tx));
        Self {
            key,
            tx,
            rx,
            next_id: 1,
        }
    }

    fn process(
        &mut self,
        samples: &[i16],
        timeout: Duration,
    ) -> Result<DeepFilterNetFrameResult, String> {
        while self.rx.try_recv().is_ok() {}

        let mut frame = [0_i16; MIX_SAMPLES_PER_FRAME];
        frame.copy_from_slice(samples);
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.tx
            .try_send(DeepFilterNetRequest { id, samples: frame })
            .map_err(|err| match err {
                std_mpsc::TrySendError::Full(_) => "DeepFilterNet worker queue is full".to_string(),
                std_mpsc::TrySendError::Disconnected(_) => {
                    "DeepFilterNet worker disconnected".to_string()
                }
            })?;

        let deadline = Instant::now() + timeout;
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err("DeepFilterNet worker timed out".to_string());
            };
            let response = self
                .rx
                .recv_timeout(remaining)
                .map_err(|_| "DeepFilterNet worker timed out".to_string())?;
            if response.id == id {
                return response.result;
            }
        }
    }
}

#[cfg(feature = "processing-deepfilternet")]
fn run_deepfilternet_worker(
    key: DeepFilterNetConfigKey,
    rx: std_mpsc::Receiver<DeepFilterNetRequest>,
    tx: std_mpsc::SyncSender<DeepFilterNetResponse>,
) {
    let mut processor = match DeepFilterNetFrameProcessor::new(key.clone()) {
        Ok(processor) => processor,
        Err(err) => {
            let detail = format!(
                "DeepFilterNet model `{}` failed to load: {err}",
                key.model_path.display()
            );
            for request in rx {
                let _ = tx.send(DeepFilterNetResponse {
                    id: request.id,
                    result: Err(detail.clone()),
                });
            }
            return;
        }
    };

    for request in rx {
        let result = processor
            .process(request.samples)
            .map_err(|err| format!("DeepFilterNet processing failed: {err}"));
        if tx
            .send(DeepFilterNetResponse {
                id: request.id,
                result,
            })
            .is_err()
        {
            break;
        }
    }
}

impl AudioProcessorBank {
    fn process(
        &mut self,
        user_id: UserId,
        codec: Codec,
        config: &ProcessingConfig,
        samples: &mut [i16],
    ) -> ProcessingStatus {
        let cleanup_active = processing_active(config, codec);
        let normalization_enabled = level_normalization_enabled(config);
        let input = measure_i16(samples);
        let pipeline = effective_processing_pipeline(config);
        if !cleanup_active && !normalization_enabled {
            return ProcessingStatus {
                active: false,
                bypassed: true,
                gate_open: input.rms >= ACTIVE_TALKER_RMS_THRESHOLD,
                engine: config.engine,
                engine_available: processing_engine_available(config.engine),
                engine_detail: inactive_processing_engine_detail(config.engine),
                backend: None,
                compute_units: None,
                inference_ms: None,
                input_rms: input.rms,
                output_rms: input.rms,
                gain_reduction_db: 0.0,
                stages: Vec::new(),
                normalization: LevelNormalizationStatus::default(),
            };
        }

        let mut stages = Vec::new();
        let mut gate_open = input.rms >= ACTIVE_TALKER_RMS_THRESHOLD;
        let mut gain_reduction_db = 0.0_f32;
        let mut engine_available = true;
        let mut details = Vec::new();
        let mut ran_stage = false;

        for stage in pipeline.iter().filter(|stage| stage.enabled) {
            let stage_input = measure_i16(samples);
            let stage_status = match stage.engine {
                ProcessingEngine::BuiltIn => self.process_builtin(
                    BuiltInProcessingContext {
                        user_id,
                        codec,
                        input: stage_input,
                        engine: ProcessingEngine::BuiltIn,
                        engine_available: true,
                        engine_detail: None,
                    },
                    config,
                    samples,
                ),
                ProcessingEngine::WebRtc => {
                    self.process_webrtc(user_id, codec, config, samples, stage_input)
                }
                ProcessingEngine::RnNoise => {
                    self.process_rnnoise(user_id, codec, config, samples, stage_input)
                }
                ProcessingEngine::DeepFilterNet => {
                    self.process_deepfilternet(user_id, codec, config, samples, stage_input)
                }
            };

            ran_stage |= stage_status.active && !stage_status.bypassed;
            gate_open = stage_status.gate_open;
            gain_reduction_db = gain_reduction_db.max(stage_status.gain_reduction_db);
            engine_available &= stage_status.engine_available;
            if let Some(detail) = stage_status.engine_detail.as_deref() {
                if !detail.is_empty() {
                    details.push(detail.to_string());
                }
            }
            stages.push(processing_stage_status(stage_status));
        }

        if !ran_stage && !normalization_enabled {
            return ProcessingStatus {
                active: false,
                bypassed: true,
                gate_open: input.rms >= ACTIVE_TALKER_RMS_THRESHOLD,
                engine: config.engine,
                engine_available,
                engine_detail: (!details.is_empty()).then(|| details.join("; ")),
                backend: None,
                compute_units: None,
                inference_ms: None,
                input_rms: input.rms,
                output_rms: input.rms,
                gain_reduction_db: 0.0,
                stages,
                normalization: LevelNormalizationStatus::default(),
            };
        }

        let normalization =
            self.normalize_input_level(user_id, &config.normalization, gate_open, samples);
        let normalization_reduction_db = gain_reduction_from_linear(normalization.applied_gain);
        let output = measure_i16(samples);
        ProcessingStatus {
            active: ran_stage || normalization.active,
            bypassed: !ran_stage && normalization.bypassed,
            gate_open,
            engine: pipeline.first().map_or(config.engine, |stage| stage.engine),
            engine_available,
            engine_detail: (!details.is_empty()).then(|| details.join("; ")),
            backend: None,
            compute_units: None,
            inference_ms: None,
            input_rms: input.rms,
            output_rms: output.rms,
            gain_reduction_db: gain_reduction_db.max(normalization_reduction_db),
            stages,
            normalization,
        }
    }

    fn normalize_input_level(
        &mut self,
        user_id: UserId,
        config: &LevelNormalizationConfig,
        gate_open: bool,
        samples: &mut [i16],
    ) -> LevelNormalizationStatus {
        let sanitized = sanitize_level_normalization_config(config);
        let input = measure_i16(samples);
        let mut status = LevelNormalizationStatus {
            input_rms: input.rms,
            output_rms: input.rms,
            target_rms: sanitized.target_rms,
            applied_gain: 1.0,
            desired_gain: 1.0,
            max_boost: sanitized.max_boost,
            max_attenuation: sanitized.max_attenuation,
            ..LevelNormalizationStatus::default()
        };

        if !sanitized.enabled {
            status.reason = "disabled".to_string();
            return status;
        }
        if !gate_open {
            status.reason = "gate_closed".to_string();
            return status;
        }
        if input.rms < sanitized.noise_floor_rms {
            status.reason = "below_noise_floor".to_string();
            return status;
        }

        let min_gain = 1.0 / sanitized.max_attenuation.max(1.0);
        let desired = (sanitized.target_rms / input.rms.max(0.000_001))
            .clamp(min_gain, sanitized.max_boost.max(1.0));
        let state = self.normalizers.entry(user_id).or_default();
        if !state.initialized {
            state.gain = desired;
            state.initialized = true;
        } else {
            let base_step =
                (common::FRAME_MS as f32 / sanitized.adaptation_ms.max(10) as f32).clamp(0.01, 1.0);
            let step = if desired < state.gain {
                (base_step * 2.0).min(1.0)
            } else {
                base_step
            };
            state.gain += (desired - state.gain) * step;
        }
        state.gain = state.gain.clamp(min_gain, sanitized.max_boost.max(1.0));

        let peak_safety_gain = if input.peak > 0.0 {
            (0.98 / input.peak).min(sanitized.max_boost.max(1.0))
        } else {
            1.0
        };
        let applied_gain = state.gain.min(peak_safety_gain).max(min_gain);
        let mut clipping_events = 0_u32;
        for sample in &mut *samples {
            let scaled = *sample as f32 * applied_gain;
            if scaled > i16::MAX as f32 || scaled < i16::MIN as f32 {
                clipping_events = clipping_events.saturating_add(1);
            }
            *sample = scaled.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }

        let output = measure_i16(samples);
        status.active = true;
        status.bypassed = false;
        status.output_rms = output.rms;
        status.applied_gain = applied_gain;
        status.desired_gain = desired;
        status.clipping_events = clipping_events;
        status.reason = if applied_gain < desired {
            "peak_limited".to_string()
        } else {
            "active".to_string()
        };
        status
    }

    #[cfg(feature = "processing-webrtc")]
    fn process_webrtc(
        &mut self,
        user_id: UserId,
        codec: Codec,
        config: &ProcessingConfig,
        samples: &mut [i16],
        input: FrameMeter,
    ) -> ProcessingStatus {
        if samples.len() != MIX_SAMPLES_PER_FRAME {
            return self.process_builtin(
                BuiltInProcessingContext {
                    user_id,
                    codec,
                    input,
                    engine: ProcessingEngine::WebRtc,
                    engine_available: false,
                    engine_detail: Some(format!(
                        "WebRTC APM requires {} samples at 48 kHz; got {}",
                        MIX_SAMPLES_PER_FRAME,
                        samples.len()
                    )),
                },
                config,
                samples,
            );
        }

        let state = match self.webrtc.entry(user_id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let state = match WebRtcProcessorState::new(config) {
                    Ok(state) => state,
                    Err(err) => {
                        let detail = format!("WebRTC APM init failed: {err}");
                        if config.fallback_to_builtin {
                            return self.process_builtin(
                                BuiltInProcessingContext {
                                    user_id,
                                    codec,
                                    input,
                                    engine: ProcessingEngine::WebRtc,
                                    engine_available: false,
                                    engine_detail: Some(format!(
                                        "{detail}; used built-in fallback"
                                    )),
                                },
                                config,
                                samples,
                            );
                        }

                        return ProcessingStatus {
                            active: false,
                            bypassed: true,
                            gate_open: input.rms >= ACTIVE_TALKER_RMS_THRESHOLD,
                            engine: ProcessingEngine::WebRtc,
                            engine_available: false,
                            engine_detail: Some(detail),
                            backend: None,
                            compute_units: None,
                            inference_ms: None,
                            input_rms: input.rms,
                            output_rms: input.rms,
                            gain_reduction_db: 0.0,
                            stages: Vec::new(),
                            normalization: LevelNormalizationStatus::default(),
                        };
                    }
                };
                entry.insert(state)
            }
        };
        state.apply_config(config);
        for (dst, sample) in state.frame.iter_mut().zip(samples.iter()) {
            *dst = *sample as f32 / i16::MAX as f32;
        }

        if let Err(err) = state
            .processor
            .process_capture_frame(std::slice::from_mut(&mut state.frame))
        {
            let detail = format!("WebRTC APM process failed: {err}");
            if config.fallback_to_builtin {
                return self.process_builtin(
                    BuiltInProcessingContext {
                        user_id,
                        codec,
                        input,
                        engine: ProcessingEngine::WebRtc,
                        engine_available: false,
                        engine_detail: Some(format!("{detail}; used built-in fallback")),
                    },
                    config,
                    samples,
                );
            }

            return ProcessingStatus {
                active: false,
                bypassed: true,
                gate_open: input.rms >= ACTIVE_TALKER_RMS_THRESHOLD,
                engine: ProcessingEngine::WebRtc,
                engine_available: false,
                engine_detail: Some(detail),
                backend: None,
                compute_units: None,
                inference_ms: None,
                input_rms: input.rms,
                output_rms: input.rms,
                gain_reduction_db: 0.0,
                stages: Vec::new(),
                normalization: LevelNormalizationStatus::default(),
            };
        }

        for (sample, processed) in samples.iter_mut().zip(state.frame.iter()) {
            *sample = (*processed * i16::MAX as f32)
                .round()
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }

        let output = measure_i16(samples);
        let stats = state.processor.get_stats();
        let detail = match stats.voice_detected {
            Some(true) => "WebRTC APM voice=true".to_string(),
            Some(false) => "WebRTC APM voice=false".to_string(),
            None => "WebRTC APM".to_string(),
        };
        ProcessingStatus {
            active: true,
            bypassed: false,
            gate_open: stats
                .voice_detected
                .unwrap_or(output.rms >= ACTIVE_TALKER_RMS_THRESHOLD),
            engine: ProcessingEngine::WebRtc,
            engine_available: true,
            engine_detail: Some(detail),
            backend: None,
            compute_units: None,
            inference_ms: None,
            input_rms: input.rms,
            output_rms: output.rms,
            gain_reduction_db: rms_gain_reduction_db(input, output),
            stages: Vec::new(),
            normalization: LevelNormalizationStatus::default(),
        }
    }

    #[cfg(not(feature = "processing-webrtc"))]
    fn process_webrtc(
        &mut self,
        user_id: UserId,
        codec: Codec,
        config: &ProcessingConfig,
        samples: &mut [i16],
        input: FrameMeter,
    ) -> ProcessingStatus {
        self.processing_engine_unavailable_or_fallback(
            user_id,
            codec,
            config,
            samples,
            input,
            ProcessingEngine::WebRtc,
            "server was built without processing-webrtc".to_string(),
        )
    }

    fn process_builtin(
        &mut self,
        context: BuiltInProcessingContext,
        config: &ProcessingConfig,
        samples: &mut [i16],
    ) -> ProcessingStatus {
        let cleanup_enabled = processing_cleanup_enabled(config);
        if cleanup_enabled && config.high_pass {
            high_pass_frame(self.high_pass.entry(context.user_id).or_default(), samples);
        }

        if cleanup_enabled && config.transient_suppression {
            suppress_transient_frame(config.profile, samples);
        }

        let gate_open = if cleanup_enabled && (config.noise_gate || config.vad) {
            gate_voice_frame(
                self.vad.entry(context.user_id).or_default(),
                config.profile,
                config.vad,
                samples,
            )
        } else {
            true
        };
        let gain_reduction_db = if cleanup_enabled && config.compressor {
            compress_frame(config.profile, samples)
        } else {
            0.0
        };
        if config.presence && matches!(context.codec, Codec::Pcm16 | Codec::Pcm24 | Codec::Opus) {
            enhance_low_rate_voice(samples);
        }

        let output = measure_i16(samples);
        ProcessingStatus {
            active: true,
            bypassed: false,
            gate_open,
            engine: context.engine,
            engine_available: context.engine_available,
            engine_detail: context.engine_detail,
            backend: None,
            compute_units: None,
            inference_ms: None,
            input_rms: context.input.rms,
            output_rms: output.rms,
            gain_reduction_db,
            stages: Vec::new(),
            normalization: LevelNormalizationStatus::default(),
        }
    }

    #[cfg(feature = "processing-rnnoise")]
    fn process_rnnoise(
        &mut self,
        user_id: UserId,
        codec: Codec,
        config: &ProcessingConfig,
        samples: &mut [i16],
        input: FrameMeter,
    ) -> ProcessingStatus {
        if samples.len() != nnnoiseless::DenoiseState::FRAME_SIZE {
            return self.process_builtin(
                BuiltInProcessingContext {
                    user_id,
                    codec,
                    input,
                    engine: ProcessingEngine::RnNoise,
                    engine_available: false,
                    engine_detail: Some(format!(
                        "RNNoise requires {} samples at 48 kHz; got {}",
                        nnnoiseless::DenoiseState::FRAME_SIZE,
                        samples.len()
                    )),
                },
                config,
                samples,
            );
        }

        let cleanup_enabled = processing_cleanup_enabled(config);
        if cleanup_enabled && config.high_pass {
            high_pass_frame(self.high_pass.entry(user_id).or_default(), samples);
        }

        let rnnoise = self
            .rnnoise
            .entry(user_id)
            .or_insert_with(RnNoiseProcessorState::new);
        for (dst, src) in rnnoise.input.iter_mut().zip(samples.iter()) {
            *dst = *src as f32;
        }
        let vad_probability = rnnoise
            .denoise
            .process_frame(&mut rnnoise.output, &rnnoise.input);
        if rnnoise.warmed {
            for (sample, processed) in samples.iter_mut().zip(rnnoise.output.iter()) {
                *sample = processed.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            }
        } else {
            rnnoise.warmed = true;
        }

        if cleanup_enabled && config.transient_suppression {
            suppress_transient_frame(config.profile, samples);
        }

        let gate_open = if cleanup_enabled && (config.noise_gate || config.vad) {
            let rnnoise_open = vad_probability >= rnnoise_vad_threshold(config.profile);
            if rnnoise_open {
                let state = self.vad.entry(user_id).or_default();
                state.open = true;
                state.hold_frames = vad_params(config.profile).hold_frames;
                state.gain = state.gain.max(0.8);
                true
            } else if config.noise_gate {
                gate_voice_frame(
                    self.vad.entry(user_id).or_default(),
                    config.profile,
                    config.vad,
                    samples,
                )
            } else {
                false
            }
        } else {
            true
        };

        let compressor_reduction_db = if cleanup_enabled && config.compressor {
            compress_frame(config.profile, samples)
        } else {
            0.0
        };
        if config.presence && matches!(codec, Codec::Pcm16 | Codec::Pcm24 | Codec::Opus) {
            enhance_low_rate_voice(samples);
        }

        let output = measure_i16(samples);
        ProcessingStatus {
            active: true,
            bypassed: false,
            gate_open,
            engine: ProcessingEngine::RnNoise,
            engine_available: true,
            engine_detail: Some(format!("RNNoise VAD {:.2}", vad_probability)),
            backend: None,
            compute_units: None,
            inference_ms: None,
            input_rms: input.rms,
            output_rms: output.rms,
            gain_reduction_db: compressor_reduction_db.max(rms_gain_reduction_db(input, output)),
            stages: Vec::new(),
            normalization: LevelNormalizationStatus::default(),
        }
    }

    #[cfg(not(feature = "processing-rnnoise"))]
    fn process_rnnoise(
        &mut self,
        user_id: UserId,
        codec: Codec,
        config: &ProcessingConfig,
        samples: &mut [i16],
        input: FrameMeter,
    ) -> ProcessingStatus {
        self.processing_engine_unavailable_or_fallback(
            user_id,
            codec,
            config,
            samples,
            input,
            ProcessingEngine::RnNoise,
            "server was built without processing-rnnoise".to_string(),
        )
    }

    #[cfg(feature = "processing-deepfilternet")]
    fn process_deepfilternet(
        &mut self,
        user_id: UserId,
        codec: Codec,
        config: &ProcessingConfig,
        samples: &mut [i16],
        input: FrameMeter,
    ) -> ProcessingStatus {
        if samples.len() != MIX_SAMPLES_PER_FRAME {
            return self.deepfilternet_unavailable_or_fallback(
                user_id,
                codec,
                config,
                samples,
                input,
                format!(
                    "DeepFilterNet requires {} samples at 48 kHz; got {}",
                    MIX_SAMPLES_PER_FRAME,
                    samples.len()
                ),
            );
        }

        let key = match deepfilternet_config_key(config) {
            Ok(key) => key,
            Err(detail) => {
                return self.deepfilternet_unavailable_or_fallback(
                    user_id, codec, config, samples, input, detail,
                );
            }
        };

        let reload = self
            .deepfilternet
            .get(&user_id)
            .is_none_or(|state| state.key != key);
        if reload {
            self.deepfilternet.insert(
                user_id,
                DeepFilterNetWorkerState::spawn(key, config.worker_queue_frames),
            );
        }

        let result = {
            let Some(state) = self.deepfilternet.get_mut(&user_id) else {
                unreachable!("DeepFilterNet worker was inserted above");
            };
            state.process(samples, deepfilternet_worker_timeout(config))
        };
        let result = match result {
            Ok(result) => result,
            Err(detail) => {
                return self.deepfilternet_unavailable_or_fallback(
                    user_id, codec, config, samples, input, detail,
                );
            }
        };
        samples.copy_from_slice(&result.samples);

        if config.presence && matches!(codec, Codec::Pcm16 | Codec::Pcm24 | Codec::Opus) {
            enhance_low_rate_voice(samples);
        }

        let output = measure_i16(samples);
        let fallback_note = result
            .fallback_reason
            .as_deref()
            .map(|reason| format!("; {reason}"))
            .unwrap_or_default();
        ProcessingStatus {
            active: true,
            bypassed: false,
            gate_open: output.rms >= ACTIVE_TALKER_RMS_THRESHOLD || result.lsnr_db > -10.0,
            engine: ProcessingEngine::DeepFilterNet,
            engine_available: true,
            engine_detail: Some(format!(
                "DeepFilterNet {model_name}; backend requested={} active={} compute={} inference={:.1} ms; LSNR {:.1} dB; lookahead {} frames{}",
                deep_filter_backend_name(result.requested_backend),
                deep_filter_backend_name(result.active_backend),
                apple_compute_units_name(result.compute_units),
                result.inference_ms,
                result.lsnr_db,
                result.lookahead_frames,
                fallback_note,
                model_name = result.model_name
            )),
            backend: Some(deep_filter_backend_name(result.active_backend).to_string()),
            compute_units: Some(apple_compute_units_name(result.compute_units).to_string()),
            inference_ms: Some(result.inference_ms),
            input_rms: input.rms,
            output_rms: output.rms,
            gain_reduction_db: rms_gain_reduction_db(input, output),
            stages: Vec::new(),
            normalization: LevelNormalizationStatus::default(),
        }
    }

    #[cfg(not(feature = "processing-deepfilternet"))]
    fn process_deepfilternet(
        &mut self,
        user_id: UserId,
        codec: Codec,
        config: &ProcessingConfig,
        samples: &mut [i16],
        input: FrameMeter,
    ) -> ProcessingStatus {
        self.deepfilternet_unavailable_or_fallback(
            user_id,
            codec,
            config,
            samples,
            input,
            "server was built without processing-deepfilternet".to_string(),
        )
    }

    #[allow(dead_code)]
    fn processing_engine_unavailable_or_fallback(
        &mut self,
        user_id: UserId,
        codec: Codec,
        config: &ProcessingConfig,
        samples: &mut [i16],
        input: FrameMeter,
        engine: ProcessingEngine,
        detail: String,
    ) -> ProcessingStatus {
        if config.fallback_to_builtin {
            return self.process_builtin(
                BuiltInProcessingContext {
                    user_id,
                    codec,
                    input,
                    engine,
                    engine_available: false,
                    engine_detail: Some(format!("{detail}; used built-in fallback")),
                },
                config,
                samples,
            );
        }

        ProcessingStatus {
            active: false,
            bypassed: true,
            gate_open: input.rms >= ACTIVE_TALKER_RMS_THRESHOLD,
            engine,
            engine_available: false,
            engine_detail: Some(detail),
            backend: None,
            compute_units: None,
            inference_ms: None,
            input_rms: input.rms,
            output_rms: input.rms,
            gain_reduction_db: 0.0,
            stages: Vec::new(),
            normalization: LevelNormalizationStatus::default(),
        }
    }

    fn deepfilternet_unavailable_or_fallback(
        &mut self,
        user_id: UserId,
        codec: Codec,
        config: &ProcessingConfig,
        samples: &mut [i16],
        input: FrameMeter,
        detail: String,
    ) -> ProcessingStatus {
        if config.fallback_to_builtin {
            return self.process_builtin(
                BuiltInProcessingContext {
                    user_id,
                    codec,
                    input,
                    engine: ProcessingEngine::DeepFilterNet,
                    engine_available: false,
                    engine_detail: Some(format!("{detail}; used built-in fallback")),
                },
                config,
                samples,
            );
        }

        ProcessingStatus {
            active: false,
            bypassed: true,
            gate_open: input.rms >= ACTIVE_TALKER_RMS_THRESHOLD,
            engine: ProcessingEngine::DeepFilterNet,
            engine_available: false,
            engine_detail: Some(detail),
            backend: Some(deep_filter_backend_name(config.deep_filter_backend).to_string()),
            compute_units: Some(apple_compute_units_name(config.apple_compute_units).to_string()),
            inference_ms: None,
            input_rms: input.rms,
            output_rms: input.rms,
            gain_reduction_db: 0.0,
            stages: Vec::new(),
            normalization: LevelNormalizationStatus::default(),
        }
    }
}

fn effective_processing_pipeline(config: &ProcessingConfig) -> Vec<ProcessingStageConfig> {
    let pipeline = if config.pipeline.is_empty() {
        vec![ProcessingStageConfig {
            engine: config.engine,
            enabled: true,
        }]
    } else {
        config.pipeline.clone()
    };

    pipeline.into_iter().filter(|stage| stage.enabled).collect()
}

#[cfg(feature = "processing-deepfilternet")]
fn deepfilternet_config_key(config: &ProcessingConfig) -> Result<DeepFilterNetConfigKey, String> {
    let model = config
        .deep_filter_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| "DeepFilterNet model is not configured".to_string())?;
    let model_path = PathBuf::from(model);
    if !is_supported_deepfilternet_runtime_model_path(&model_path) {
        return Err(format!(
            "DeepFilterNet runtime requires an ONNX .tar.gz/.tgz model archive or a complete Core ML package directory; selected `{}`",
            model_path.display()
        ));
    }
    Ok(DeepFilterNetConfigKey {
        model_path,
        profile: config.profile,
        backend: config.deep_filter_backend,
        compute_units: config.apple_compute_units,
    })
}

#[cfg_attr(not(feature = "processing-deepfilternet"), allow(dead_code))]
fn deepfilternet_select_runtime_backend(
    requested: DeepFilterBackend,
    model_path: &Path,
) -> (DeepFilterBackend, Option<String>) {
    let model_is_coreml = is_supported_deepfilternet_coreml_package_path(model_path);
    match requested {
        DeepFilterBackend::Tract => {
            if model_is_coreml {
                (
                    DeepFilterBackend::Tract,
                    Some("Tract backend requested, but selected model is a Core ML package; select an ONNX .tar.gz model or use the Core ML backend".to_string()),
                )
            } else {
                (DeepFilterBackend::Tract, None)
            }
        }
        DeepFilterBackend::Auto => {
            if model_is_coreml && deepfilternet_coreml_runtime_available() {
                (DeepFilterBackend::CoreMl, None)
            } else if model_is_coreml {
                (
                    DeepFilterBackend::Tract,
                    Some(deepfilternet_coreml_fallback_detail(model_path)),
                )
            } else {
                (DeepFilterBackend::Tract, None)
            }
        }
        DeepFilterBackend::CoreMl => {
            if model_is_coreml && deepfilternet_coreml_runtime_available() {
                (DeepFilterBackend::CoreMl, None)
            } else {
                (
                    DeepFilterBackend::Tract,
                    Some(deepfilternet_coreml_fallback_detail(model_path)),
                )
            }
        }
    }
}

fn deepfilternet_coreml_runtime_available() -> bool {
    cfg!(all(
        target_os = "macos",
        feature = "processing-deepfilternet-coreml"
    ))
}

fn deepfilternet_coreml_fallback_detail(model_path: &Path) -> String {
    if !cfg!(target_os = "macos") {
        return "Core ML backend requested, but Core ML packages only run on macOS; build on macOS with processing-deepfilternet-coreml or select an ONNX .tar.gz model for Tract".to_string();
    }
    if !cfg!(feature = "processing-deepfilternet-coreml") {
        return "Core ML backend requested, but server was built without processing-deepfilternet-coreml; rebuild with that feature or select an ONNX .tar.gz model for Tract".to_string();
    }
    if !is_supported_deepfilternet_coreml_package_path(model_path) {
        return "Core ML backend requested, but the selected DeepFilterNet model is not a Core ML package; select a complete Core ML package directory".to_string();
    }
    let (_, detail) = deepfilternet_coreml_package_status(model_path);
    detail.unwrap_or_else(|| {
        "Core ML backend requested, but the selected DeepFilterNet Core ML package is not usable; select a complete Core ML package directory or an ONNX .tar.gz model for Tract".to_string()
    })
}

fn deep_filter_backend_name(backend: DeepFilterBackend) -> &'static str {
    match backend {
        DeepFilterBackend::Auto => "auto",
        DeepFilterBackend::Tract => "tract",
        DeepFilterBackend::CoreMl => "coreml",
    }
}

fn apple_compute_units_name(units: AppleComputeUnits) -> &'static str {
    match units {
        AppleComputeUnits::CpuOnly => "cpu_only",
        AppleComputeUnits::CpuAndGpu => "cpu_and_gpu",
        AppleComputeUnits::CpuAndNeuralEngine => "cpu_and_neural_engine",
        AppleComputeUnits::All => "all",
    }
}

#[cfg(feature = "processing-deepfilternet")]
fn deepfilternet_runtime_params(profile: ProcessingProfile) -> RuntimeParams {
    let params = RuntimeParams::default_with_ch(1);
    match profile {
        ProcessingProfile::Raw => params.with_atten_lim(0.0),
        ProcessingProfile::Broadcast => params
            .with_atten_lim(18.0)
            .with_thresholds(-12.0, 35.0, 35.0),
        ProcessingProfile::Voice => params
            .with_atten_lim(36.0)
            .with_thresholds(-15.0, 35.0, 35.0),
        ProcessingProfile::VoiceIsolation => params
            .with_atten_lim(100.0)
            .with_thresholds(-18.0, 35.0, 35.0)
            .with_post_filter(0.02),
    }
}

#[cfg(feature = "processing-deepfilternet")]
fn deepfilternet_worker_timeout(_config: &ProcessingConfig) -> Duration {
    Duration::from_millis(common::FRAME_MS as u64 * 2)
}

fn processing_stage_status(status: ProcessingStatus) -> ProcessingStageStatus {
    ProcessingStageStatus {
        engine: status.engine,
        active: status.active,
        bypassed: status.bypassed,
        available: status.engine_available,
        detail: status.engine_detail,
        backend: status.backend,
        compute_units: status.compute_units,
        inference_ms: status.inference_ms,
        input_rms: status.input_rms,
        output_rms: status.output_rms,
        gain_reduction_db: status.gain_reduction_db,
    }
}

#[cfg(feature = "processing-webrtc")]
fn webrtc_config(config: &ProcessingConfig) -> WebRtcConfig {
    WebRtcConfig {
        pipeline: Pipeline {
            maximum_internal_processing_rate: PipelineProcessingRate::Max48000Hz,
            ..Pipeline::default()
        },
        high_pass_filter: config.high_pass.then_some(HighPassFilter::default()),
        noise_suppression: (config.noise_gate || config.vad).then_some(NoiseSuppression {
            level: webrtc_noise_suppression_level(config.profile),
            analyze_linear_aec_output: false,
        }),
        gain_controller: config.compressor.then_some(GainController::GainController1(
            GainController1 {
                mode: GainControllerMode::FixedDigital,
                target_level_dbfs: webrtc_target_level_dbfs(config.profile),
                compression_gain_db: webrtc_compression_gain_db(config.profile),
                enable_limiter: true,
                analog_gain_controller: None,
            },
        )),
        ..WebRtcConfig::default()
    }
}

#[cfg(feature = "processing-webrtc")]
fn webrtc_noise_suppression_level(profile: ProcessingProfile) -> NoiseSuppressionLevel {
    match profile {
        ProcessingProfile::Raw => NoiseSuppressionLevel::Low,
        ProcessingProfile::Broadcast => NoiseSuppressionLevel::Moderate,
        ProcessingProfile::Voice => NoiseSuppressionLevel::High,
        ProcessingProfile::VoiceIsolation => NoiseSuppressionLevel::VeryHigh,
    }
}

#[cfg(feature = "processing-webrtc")]
fn webrtc_target_level_dbfs(profile: ProcessingProfile) -> u8 {
    match profile {
        ProcessingProfile::Raw | ProcessingProfile::Broadcast => 9,
        ProcessingProfile::Voice => 6,
        ProcessingProfile::VoiceIsolation => 5,
    }
}

#[cfg(feature = "processing-webrtc")]
fn webrtc_compression_gain_db(profile: ProcessingProfile) -> u8 {
    match profile {
        ProcessingProfile::Raw => 0,
        ProcessingProfile::Broadcast => 6,
        ProcessingProfile::Voice => 9,
        ProcessingProfile::VoiceIsolation => 12,
    }
}

fn processing_engine_available(engine: ProcessingEngine) -> bool {
    match engine {
        ProcessingEngine::BuiltIn => true,
        ProcessingEngine::WebRtc => cfg!(feature = "processing-webrtc"),
        ProcessingEngine::RnNoise => cfg!(feature = "processing-rnnoise"),
        ProcessingEngine::DeepFilterNet => cfg!(feature = "processing-deepfilternet"),
    }
}

fn inactive_processing_engine_detail(engine: ProcessingEngine) -> Option<String> {
    (!processing_engine_available(engine)).then(|| unavailable_processing_engine_detail(engine))
}

fn unavailable_processing_engine_detail(engine: ProcessingEngine) -> String {
    match engine {
        ProcessingEngine::BuiltIn => String::new(),
        ProcessingEngine::WebRtc => "server was built without processing-webrtc".to_string(),
        ProcessingEngine::RnNoise => "server was built without processing-rnnoise".to_string(),
        ProcessingEngine::DeepFilterNet => {
            "server was built without processing-deepfilternet".to_string()
        }
    }
}

#[cfg(feature = "processing-rnnoise")]
fn rnnoise_vad_threshold(profile: ProcessingProfile) -> f32 {
    match profile {
        ProcessingProfile::Raw => 0.0,
        ProcessingProfile::Broadcast => 0.25,
        ProcessingProfile::Voice => 0.35,
        ProcessingProfile::VoiceIsolation => 0.45,
    }
}

#[cfg_attr(
    not(any(
        feature = "processing-webrtc",
        feature = "processing-rnnoise",
        feature = "processing-deepfilternet"
    )),
    allow(dead_code)
)]
fn rms_gain_reduction_db(input: FrameMeter, output: FrameMeter) -> f32 {
    if input.rms <= f32::EPSILON || output.rms >= input.rms {
        0.0
    } else {
        -20.0 * (output.rms / input.rms).max(0.000_001).log10()
    }
}

fn gain_reduction_from_linear(gain: f32) -> f32 {
    if gain >= 1.0 {
        0.0
    } else {
        -20.0 * gain.max(0.000_001).log10()
    }
}

fn level_normalization_enabled(config: &ProcessingConfig) -> bool {
    !matches!(config.mode, ProcessingMode::Disabled) && config.normalization.enabled
}

fn sanitize_level_normalization_config(
    config: &LevelNormalizationConfig,
) -> LevelNormalizationConfig {
    LevelNormalizationConfig {
        enabled: config.enabled,
        target_rms: config.target_rms.clamp(0.02, 0.40),
        max_boost: config.max_boost.clamp(1.0, 16.0),
        max_attenuation: config.max_attenuation.clamp(1.0, 32.0),
        adaptation_ms: config.adaptation_ms.clamp(20, 5_000),
        noise_floor_rms: config.noise_floor_rms.clamp(0.0, 0.20),
    }
}

fn processing_active(config: &ProcessingConfig, codec: Codec) -> bool {
    match config.mode {
        ProcessingMode::Enabled => true,
        ProcessingMode::Disabled => false,
        ProcessingMode::Auto => {
            !matches!(config.profile, ProcessingProfile::Raw)
                && (processing_pipeline_has_non_builtin(config)
                    || matches!(config.profile, ProcessingProfile::VoiceIsolation)
                    || !matches!(codec, Codec::Pcm48))
        }
    }
}

fn processing_cleanup_enabled(config: &ProcessingConfig) -> bool {
    match config.mode {
        ProcessingMode::Enabled => !matches!(config.profile, ProcessingProfile::Raw),
        ProcessingMode::Disabled => false,
        ProcessingMode::Auto => {
            !matches!(config.profile, ProcessingProfile::Raw)
                && (processing_pipeline_has_non_builtin(config)
                    || matches!(config.profile, ProcessingProfile::VoiceIsolation))
        }
    }
}

fn processing_pipeline_has_non_builtin(config: &ProcessingConfig) -> bool {
    effective_processing_pipeline(config)
        .iter()
        .any(|stage| !matches!(stage.engine, ProcessingEngine::BuiltIn))
}

fn high_pass_frame(state: &mut HighPassState, samples: &mut [i16]) {
    const ALPHA: f32 = 0.98;
    for sample in samples {
        let input = *sample as f32;
        let output = ALPHA * (state.previous_output + input - state.previous_input);
        state.previous_input = input;
        state.previous_output = output;
        *sample = output.clamp(i16::MIN as f32, i16::MAX as f32).round() as i16;
    }
}

fn gate_voice_frame(
    state: &mut ServerVadState,
    profile: ProcessingProfile,
    speech_vad: bool,
    samples: &mut [i16],
) -> bool {
    let meter = measure_i16(samples);
    let features = voice_features(samples, meter);
    let params = vad_params(profile);
    let speech_like = if speech_vad {
        meter.rms >= params.open_rms
            && features.zero_crossing_rate <= params.max_zero_crossing_rate
            && features.crest_factor <= params.max_crest_factor
    } else {
        meter.rms >= params.open_rms
    };

    if speech_like {
        state.speech_frames = state.speech_frames.saturating_add(1);
        if state.speech_frames >= params.open_frames {
            state.open = true;
            state.hold_frames = params.hold_frames;
        }
    } else {
        state.speech_frames = 0;
        if state.hold_frames > 0 && meter.rms >= params.close_rms {
            state.hold_frames -= 1;
        } else if meter.rms < params.close_rms {
            state.open = false;
        }
    }

    let target_gain = if state.open { 1.0 } else { params.closed_gain };
    let step = if target_gain > state.gain {
        params.attack
    } else {
        params.release
    };
    state.gain += (target_gain - state.gain) * step;
    let gain = state.gain;
    for sample in samples {
        *sample = ((*sample as f32) * gain)
            .clamp(i16::MIN as f32, i16::MAX as f32)
            .round() as i16;
    }
    state.open
}

fn suppress_transient_frame(profile: ProcessingProfile, samples: &mut [i16]) -> f32 {
    let meter = measure_i16(samples);
    let features = voice_features(samples, meter);
    let params = vad_params(profile);
    if meter.rms < params.open_rms || features.crest_factor <= params.transient_crest_factor {
        return 0.0;
    }
    let reduction = match profile {
        ProcessingProfile::VoiceIsolation => 0.18,
        ProcessingProfile::Voice => 0.35,
        ProcessingProfile::Broadcast => 0.7,
        ProcessingProfile::Raw => 1.0,
    };
    if reduction >= 1.0 {
        return 0.0;
    }
    for sample in samples {
        *sample = ((*sample as f32) * reduction)
            .clamp(i16::MIN as f32, i16::MAX as f32)
            .round() as i16;
    }
    -20.0 * reduction.log10()
}

fn compress_frame(profile: ProcessingProfile, samples: &mut [i16]) -> f32 {
    let meter = measure_i16(samples);
    let target = match profile {
        ProcessingProfile::VoiceIsolation => 0.14_f32,
        ProcessingProfile::Voice => 0.18_f32,
        ProcessingProfile::Broadcast => 0.24_f32,
        ProcessingProfile::Raw => return 0.0,
    };
    if meter.rms <= target {
        return 0.0;
    }
    let ratio = match profile {
        ProcessingProfile::VoiceIsolation => 4.0_f32,
        ProcessingProfile::Voice => 3.0_f32,
        ProcessingProfile::Broadcast => 2.0_f32,
        ProcessingProfile::Raw => 1.0_f32,
    };
    let compressed = target + (meter.rms - target) / ratio;
    let gain = (compressed / meter.rms).clamp(0.1, 1.0);
    for sample in samples {
        *sample = ((*sample as f32) * gain)
            .clamp(i16::MIN as f32, i16::MAX as f32)
            .round() as i16;
    }
    -20.0 * gain.log10()
}

#[derive(Debug, Clone, Copy)]
struct VoiceFeatures {
    zero_crossing_rate: f32,
    crest_factor: f32,
}

fn voice_features(samples: &[i16], meter: FrameMeter) -> VoiceFeatures {
    if samples.len() < 2 || meter.rms <= f32::EPSILON {
        return VoiceFeatures {
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
    VoiceFeatures {
        zero_crossing_rate: crossings as f32 / (samples.len() - 1) as f32,
        crest_factor: meter.peak / meter.rms.max(0.000_001),
    }
}

#[derive(Debug, Clone, Copy)]
struct VadParams {
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

fn vad_params(profile: ProcessingProfile) -> VadParams {
    match profile {
        ProcessingProfile::Raw => VadParams {
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
        ProcessingProfile::Broadcast => VadParams {
            open_rms: 0.006,
            close_rms: 0.0025,
            open_frames: 1,
            hold_frames: 18,
            closed_gain: 0.25,
            attack: 0.35,
            release: 0.08,
            max_zero_crossing_rate: 0.45,
            max_crest_factor: 16.0,
            transient_crest_factor: 18.0,
        },
        ProcessingProfile::Voice => VadParams {
            open_rms: 0.010,
            close_rms: 0.004,
            open_frames: 2,
            hold_frames: 14,
            closed_gain: 0.12,
            attack: 0.45,
            release: 0.10,
            max_zero_crossing_rate: 0.36,
            max_crest_factor: 12.0,
            transient_crest_factor: 11.0,
        },
        ProcessingProfile::VoiceIsolation => VadParams {
            open_rms: 0.014,
            close_rms: 0.006,
            open_frames: 3,
            hold_frames: 10,
            closed_gain: 0.015,
            attack: 0.55,
            release: 0.16,
            max_zero_crossing_rate: 0.30,
            max_crest_factor: 8.5,
            transient_crest_factor: 7.5,
        },
    }
}

#[derive(Default)]
struct AudioDecoderBank {
    opus: HashMap<OpusDecoderKey, opus::Decoder>,
    pcm_resamplers: HashMap<PcmDecoderKey, PcmFrameResampler>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct OpusDecoderKey {
    user_id: UserId,
    profile: OpusProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PcmDecoderKey {
    user_id: UserId,
    codec: Codec,
}

impl AudioDecoderBank {
    fn decode(
        &mut self,
        user_id: UserId,
        codec: Codec,
        opus_profile: OpusProfile,
        payload: &[u8],
    ) -> anyhow::Result<Vec<i16>> {
        match codec {
            Codec::Pcm16 | Codec::Pcm24 | Codec::Pcm48 => {
                let expected_len = codec_pcm16_payload_bytes(codec);
                if payload.len() != expected_len {
                    bail!(
                        "unexpected PCM16 payload length: got {}, expected {}",
                        payload.len(),
                        expected_len
                    );
                }
                let samples = pcm16_le_bytes_to_samples(payload)?;
                let key = PcmDecoderKey { user_id, codec };
                if let Entry::Vacant(entry) = self.pcm_resamplers.entry(key) {
                    entry.insert(PcmFrameResampler::new(
                        codec_sample_rate(codec),
                        MIX_SAMPLE_RATE,
                    )?);
                }
                self.pcm_resamplers
                    .get_mut(&key)
                    .context("PCM decoder resampler was not initialized")?
                    .process(&samples)
                    .map_err(Into::into)
            }
            Codec::Opus => self.decode_opus(user_id, opus_profile, payload),
            Codec::Adpcm => bail!("ADPCM is not implemented"),
        }
    }

    fn decode_opus(
        &mut self,
        user_id: UserId,
        profile: OpusProfile,
        payload: &[u8],
    ) -> anyhow::Result<Vec<i16>> {
        let key = OpusDecoderKey { user_id, profile };
        let decoder = match self.opus.get_mut(&key) {
            Some(decoder) => decoder,
            None => {
                self.opus.insert(
                    key,
                    opus::Decoder::new(profile.sample_rate_hz(), opus::Channels::Mono)?,
                );
                self.opus.get_mut(&key).unwrap()
            }
        };
        let frame_samples = profile.samples_per_frame();
        let mut samples = vec![0_i16; frame_samples];
        let decoded = decoder.decode(payload, &mut samples, false)?;
        samples.truncate(decoded);

        if samples.len() != frame_samples {
            bail!(
                "unexpected Opus frame sample count: got {}, expected {}",
                samples.len(),
                frame_samples
            );
        }

        Ok(resample_linear(
            &samples,
            profile.sample_rate_hz(),
            MIX_SAMPLE_RATE,
        ))
    }
}

#[derive(Default)]
struct AudioEncoderBank {
    opus: HashMap<OpusEncoderKey, opus::Encoder>,
    pcm_resamplers: HashMap<PcmEncoderKey, PcmFrameResampler>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct OpusEncoderKey {
    user_id: UserId,
    profile: OpusProfile,
    channels: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PcmEncoderKey {
    user_id: UserId,
    codec: Codec,
    channels: usize,
}

impl AudioEncoderBank {
    fn encode(
        &mut self,
        user_id: UserId,
        codec: Codec,
        opus_profile: OpusProfile,
        samples: &[i16],
        channels: usize,
    ) -> anyhow::Result<Vec<u8>> {
        let channels = channels.max(1);
        if channels == 2 && !matches!(codec, Codec::Pcm48 | Codec::Opus) {
            bail!("stereo output is only supported for pcm48 and opus");
        }
        if samples.len() != MIX_SAMPLES_PER_FRAME * channels {
            bail!(
                "unexpected sample count: got {}, expected {}",
                samples.len(),
                MIX_SAMPLES_PER_FRAME * channels
            );
        }

        match codec {
            Codec::Pcm48 if channels == 2 => Ok(pcm16_samples_to_le_bytes(samples)),
            Codec::Pcm16 | Codec::Pcm24 | Codec::Pcm48 => {
                let key = PcmEncoderKey {
                    user_id,
                    codec,
                    channels,
                };
                if let Entry::Vacant(entry) = self.pcm_resamplers.entry(key) {
                    entry.insert(PcmFrameResampler::new(
                        MIX_SAMPLE_RATE,
                        codec_sample_rate(codec),
                    )?);
                }
                let output = self
                    .pcm_resamplers
                    .get_mut(&key)
                    .context("PCM encoder resampler was not initialized")?
                    .process(samples)?;
                Ok(pcm16_samples_to_le_bytes(&output))
            }
            Codec::Opus => self.encode_opus(user_id, opus_profile, samples, channels),
            Codec::Adpcm => bail!("ADPCM is not implemented"),
        }
    }

    fn encode_opus(
        &mut self,
        user_id: UserId,
        profile: OpusProfile,
        samples: &[i16],
        channels: usize,
    ) -> anyhow::Result<Vec<u8>> {
        let samples = if channels == 1 {
            resample_linear(samples, MIX_SAMPLE_RATE, profile.sample_rate_hz())
        } else {
            resample_interleaved(samples, MIX_SAMPLE_RATE, profile.sample_rate_hz(), channels)?
        };
        let key = OpusEncoderKey {
            user_id,
            profile,
            channels,
        };
        let encoder = match self.opus.get_mut(&key) {
            Some(encoder) => encoder,
            None => {
                let mut encoder = opus::Encoder::new(
                    profile.sample_rate_hz(),
                    opus_channels(channels),
                    opus_application(profile),
                )?;
                configure_opus_encoder(&mut encoder, profile, channels)?;
                self.opus.insert(key, encoder);
                self.opus.get_mut(&key).unwrap()
            }
        };
        let mut payload = vec![0_u8; common::OPUS_MAX_PAYLOAD_BYTES];
        let len = encoder.encode(&samples, &mut payload)?;
        payload.truncate(len);
        Ok(payload)
    }
}

fn opus_channels(channels: usize) -> opus::Channels {
    if channels > 1 {
        opus::Channels::Stereo
    } else {
        opus::Channels::Mono
    }
}

fn opus_application(profile: OpusProfile) -> opus::Application {
    if profile.is_music() {
        opus::Application::Audio
    } else {
        opus::Application::Voip
    }
}

fn resample_interleaved(
    samples: &[i16],
    from_rate: u32,
    to_rate: u32,
    channels: usize,
) -> anyhow::Result<Vec<i16>> {
    if from_rate == to_rate {
        return Ok(samples.to_vec());
    }
    let channels = channels.max(1);
    if !samples.len().is_multiple_of(channels) {
        bail!(
            "interleaved sample count {} is not divisible by {} channels",
            samples.len(),
            channels
        );
    }
    let mut output_channels = Vec::with_capacity(channels);
    for channel in 0..channels {
        let channel_samples = samples
            .chunks_exact(channels)
            .map(|frame| frame[channel])
            .collect::<Vec<_>>();
        output_channels.push(resample_linear(&channel_samples, from_rate, to_rate));
    }
    let target_frame_len = output_channels.first().map_or(0, Vec::len);
    let mut output = Vec::with_capacity(target_frame_len * channels);
    for frame in 0..target_frame_len {
        for channel_samples in &output_channels {
            output.push(channel_samples[frame]);
        }
    }
    Ok(output)
}

fn configure_opus_encoder(
    encoder: &mut opus::Encoder,
    profile: OpusProfile,
    channels: usize,
) -> anyhow::Result<()> {
    encoder.set_bitrate(opus::Bitrate::Bits(profile.bitrate_bps(channels)))?;
    encoder.set_signal(if profile.is_music() {
        opus::Signal::Music
    } else {
        opus::Signal::Voice
    })?;
    encoder.set_max_bandwidth(opus_bandwidth(profile.max_bandwidth()))?;
    encoder.set_vbr(true)?;
    encoder.set_vbr_constraint(true)?;
    encoder.set_complexity(profile.complexity())?;
    encoder.set_inband_fec(true)?;
    encoder.set_packet_loss_perc(profile.packet_loss_percent())?;
    encoder.set_dtx(false)?;
    Ok(())
}

fn opus_bandwidth(bandwidth: OpusBandwidth) -> opus::Bandwidth {
    match bandwidth {
        OpusBandwidth::Wideband => opus::Bandwidth::Wideband,
        OpusBandwidth::Superwideband => opus::Bandwidth::Superwideband,
        OpusBandwidth::Fullband => opus::Bandwidth::Fullband,
    }
}

fn enhance_low_rate_voice(samples: &mut [i16]) {
    if samples.len() < 2 {
        return;
    }
    let average_delta = samples
        .windows(2)
        .map(|pair| (pair[1] as i32 - pair[0] as i32).unsigned_abs() as f32)
        .sum::<f32>()
        / (samples.len() - 1) as f32;
    if average_delta < 1.0 {
        return;
    }

    let mut previous = samples[0] as f32;
    for sample in samples.iter_mut().skip(1) {
        let input = *sample as f32;
        let presence = input - previous;
        previous = input;
        let output = (input + presence * 0.10).clamp(i16::MIN as f32, i16::MAX as f32);
        *sample = output.round() as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn http_auth_accepts_bearer_and_basic_tokens() {
        let auth = HttpAuthConfig::token("secret", "Test");
        let mut headers = HeaderMap::new();

        assert!(!auth.authorizes(&headers));

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret"),
        );
        assert!(auth.authorizes(&headers));

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!(
                "Basic {}",
                BASE64_STANDARD.encode("operator:secret")
            ))
            .unwrap(),
        );
        assert!(auth.authorizes(&headers));

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong"),
        );
        assert!(!auth.authorizes(&headers));
        assert!(HttpAuthConfig::disabled().authorizes(&HeaderMap::new()));
    }

    #[test]
    fn control_disconnect_classifier_accepts_mobile_reset_errors() {
        let websocket_reset = anyhow::Error::new(WsError::Protocol(
            ProtocolError::ResetWithoutClosingHandshake,
        ));
        assert!(is_normal_control_disconnect(&websocket_reset));

        let websocket_closed = anyhow::Error::new(WsError::ConnectionClosed);
        assert!(is_normal_control_disconnect(&websocket_closed));

        let io_reset = anyhow::Error::new(std::io::Error::new(ErrorKind::ConnectionReset, "reset"));
        assert!(is_normal_control_disconnect(&io_reset));

        let parse_error = anyhow::anyhow!("parse control message");
        assert!(!is_normal_control_disconnect(&parse_error));
    }

    #[test]
    fn esp32_audio_normalization_allows_mic_bypass_boost() {
        let config = Esp32AudioConfig {
            mic_pga_gain_db: 29,
            mic_software_gain_percent: 999,
            speaker_software_gain_percent: 999,
            notification_gain_percent: 999,
            sidetone: common::Esp32SidetoneControlConfig {
                firmware_gain_percent: 999,
                codec_bypass_gain_percent: 999,
                mic_bypass_gain_percent: 999,
                ..Default::default()
            },
            ..Default::default()
        };

        let normalized = normalize_esp32_audio_config(config);

        assert_eq!(normalized.mic_pga_gain_db, 24);
        assert_eq!(normalized.mic_software_gain_percent, 400);
        assert_eq!(normalized.speaker_software_gain_percent, 400);
        assert_eq!(normalized.notification_gain_percent, 200);
        assert_eq!(normalized.sidetone.firmware_gain_percent, 200);
        assert_eq!(normalized.sidetone.codec_bypass_gain_percent, 200);
        assert_eq!(normalized.sidetone.mic_bypass_gain_percent, 400);
    }

    #[test]
    fn processing_chain_bypasses_or_processes_by_mode_and_codec() {
        let mut bank = AudioProcessorBank::default();
        let mut pcm48 = vec![8_000; MIX_SAMPLES_PER_FRAME];
        let status = bank.process(1, Codec::Pcm48, &ProcessingConfig::default(), &mut pcm48);
        assert!(status.bypassed);
        assert!(!status.active);

        let mut loud = vec![20_000; MIX_SAMPLES_PER_FRAME];
        let config = ProcessingConfig {
            mode: ProcessingMode::Enabled,
            high_pass: false,
            noise_gate: false,
            compressor: true,
            presence: false,
            vad: false,
            transient_suppression: false,
            ..ProcessingConfig::default()
        };
        let status = bank.process(1, Codec::Pcm48, &config, &mut loud);
        assert!(status.active);
        assert!(status.gain_reduction_db > 0.0);
        assert!(measure_i16(&loud).rms < status.input_rms);

        let mut quiet = vec![50; MIX_SAMPLES_PER_FRAME];
        let config = ProcessingConfig {
            mode: ProcessingMode::Enabled,
            high_pass: false,
            noise_gate: true,
            compressor: false,
            presence: false,
            vad: false,
            transient_suppression: false,
            ..ProcessingConfig::default()
        };
        let status = bank.process(1, Codec::Pcm16, &config, &mut quiet);
        assert!(!status.gate_open);
        assert!(measure_i16(&quiet).rms < status.input_rms);
    }

    #[test]
    fn level_normalization_boosts_quiet_and_attenuates_loud_speech() {
        let mut bank = AudioProcessorBank::default();
        let config = ProcessingConfig {
            mode: ProcessingMode::Enabled,
            high_pass: false,
            noise_gate: false,
            compressor: false,
            presence: false,
            vad: false,
            transient_suppression: false,
            normalization: LevelNormalizationConfig {
                enabled: true,
                target_rms: 0.12,
                max_boost: 6.0,
                max_attenuation: 8.0,
                adaptation_ms: 20,
                noise_floor_rms: 0.005,
            },
            ..ProcessingConfig::default()
        };

        let mut quiet = vec![1_200; MIX_SAMPLES_PER_FRAME];
        let quiet_status = bank.process(1, Codec::Pcm48, &config, &mut quiet);
        assert!(quiet_status.normalization.active);
        assert!(quiet_status.normalization.applied_gain > 1.0);
        assert!(measure_i16(&quiet).rms > quiet_status.normalization.input_rms);

        let mut loud = vec![20_000; MIX_SAMPLES_PER_FRAME];
        let loud_status = bank.process(2, Codec::Pcm48, &config, &mut loud);
        assert!(loud_status.normalization.active);
        assert!(loud_status.normalization.applied_gain < 1.0);
        assert!(measure_i16(&loud).rms < loud_status.normalization.input_rms);
        assert!(loud_status.gain_reduction_db > 0.0);
    }

    #[test]
    fn level_normalization_respects_noise_floor_and_gain_limits() {
        let mut bank = AudioProcessorBank::default();
        let config = ProcessingConfig {
            mode: ProcessingMode::Enabled,
            high_pass: false,
            noise_gate: false,
            compressor: false,
            presence: false,
            vad: false,
            transient_suppression: false,
            normalization: LevelNormalizationConfig {
                enabled: true,
                target_rms: 0.20,
                max_boost: 2.0,
                max_attenuation: 4.0,
                adaptation_ms: 20,
                noise_floor_rms: 0.010,
            },
            ..ProcessingConfig::default()
        };

        let mut room = vec![100; MIX_SAMPLES_PER_FRAME];
        let room_status = bank.process(1, Codec::Pcm48, &config, &mut room);
        assert!(room_status.normalization.bypassed);
        assert!(matches!(
            room_status.normalization.reason.as_str(),
            "gate_closed" | "below_noise_floor"
        ));
        assert_eq!(room_status.normalization.applied_gain, 1.0);

        let mut whisper = vec![1_500; MIX_SAMPLES_PER_FRAME];
        let whisper_status = bank.process(2, Codec::Pcm48, &config, &mut whisper);
        assert!(whisper_status.normalization.active);
        assert!(whisper_status.normalization.applied_gain <= 2.0);
    }

    #[cfg(feature = "processing-rnnoise")]
    #[test]
    fn rnnoise_engine_processes_48khz_frames_and_reports_vad() {
        let mut bank = AudioProcessorBank::default();
        let mut frame = (0..MIX_SAMPLES_PER_FRAME)
            .map(|i| {
                let phase = i as f32 * 440.0 * std::f32::consts::TAU / MIX_SAMPLE_RATE as f32;
                (phase.sin() * 8_000.0).round() as i16
            })
            .collect::<Vec<_>>();
        let original = frame.clone();
        let config = ProcessingConfig {
            mode: ProcessingMode::Enabled,
            engine: ProcessingEngine::RnNoise,
            high_pass: false,
            noise_gate: false,
            compressor: false,
            presence: false,
            transient_suppression: false,
            ..ProcessingConfig::default()
        };

        let warmup = bank.process(1, Codec::Pcm48, &config, &mut frame);
        assert!(warmup.active);
        frame.clone_from(&original);
        let status = bank.process(1, Codec::Pcm48, &config, &mut frame);

        assert!(status.active);
        assert!(status.engine_available);
        assert_eq!(status.engine, ProcessingEngine::RnNoise);
        assert!(status
            .engine_detail
            .as_deref()
            .is_some_and(|detail| detail.contains("RNNoise VAD")));
        assert_ne!(frame, original);
    }

    #[cfg(feature = "processing-webrtc")]
    #[test]
    fn webrtc_engine_processes_48khz_frames_and_reports_stage() {
        let mut bank = AudioProcessorBank::default();
        let mut frame = (0..MIX_SAMPLES_PER_FRAME)
            .map(|i| {
                let phase = i as f32 * 440.0 * std::f32::consts::TAU / MIX_SAMPLE_RATE as f32;
                (phase.sin() * 8_000.0).round() as i16
            })
            .collect::<Vec<_>>();
        let config = ProcessingConfig {
            mode: ProcessingMode::Enabled,
            engine: ProcessingEngine::WebRtc,
            profile: ProcessingProfile::VoiceIsolation,
            high_pass: true,
            noise_gate: true,
            compressor: true,
            presence: false,
            transient_suppression: false,
            ..ProcessingConfig::default()
        };

        let status = bank.process(1, Codec::Pcm48, &config, &mut frame);

        assert!(status.active);
        assert!(status.engine_available);
        assert_eq!(status.engine, ProcessingEngine::WebRtc);
        assert_eq!(status.stages.len(), 1);
        assert_eq!(status.stages[0].engine, ProcessingEngine::WebRtc);
        assert!(status.stages[0].available);
        assert!(status
            .engine_detail
            .as_deref()
            .is_some_and(|detail| detail.contains("WebRTC APM")));
    }

    #[test]
    fn unavailable_processing_engine_reports_fallback_status() {
        let mut bank = AudioProcessorBank::default();
        let mut frame = vec![20_000; MIX_SAMPLES_PER_FRAME];
        let config = ProcessingConfig {
            mode: ProcessingMode::Enabled,
            engine: ProcessingEngine::DeepFilterNet,
            high_pass: false,
            noise_gate: false,
            compressor: true,
            presence: false,
            transient_suppression: false,
            fallback_to_builtin: true,
            ..ProcessingConfig::default()
        };

        let status = bank.process(1, Codec::Pcm48, &config, &mut frame);

        assert!(status.active);
        assert!(!status.engine_available);
        assert_eq!(status.engine, ProcessingEngine::DeepFilterNet);
        assert!(status
            .engine_detail
            .as_deref()
            .is_some_and(|detail| detail.contains("built-in fallback")));
        assert!(measure_i16(&frame).rms < status.input_rms);
    }

    #[test]
    fn deepfilternet_coreml_request_selects_safe_tract_fallback() {
        let (active, detail) = deepfilternet_select_runtime_backend(
            DeepFilterBackend::CoreMl,
            Path::new("deepfilternet-models/DeepFilterNet3_onnx.tar.gz"),
        );

        assert_eq!(active, DeepFilterBackend::Tract);
        assert!(detail
            .as_deref()
            .is_some_and(|message| message.contains("Core ML")));
    }

    #[tokio::test]
    async fn channel_roster_marks_present_and_transmitting_members() {
        let state = ServerState::default();
        let mut listener = Session::new();
        listener.addr = Some("127.0.0.1:10001".parse().unwrap());
        listener.listen_channels = [1].into();
        let mut talker = Session::new();
        talker.addr = Some("127.0.0.1:10002".parse().unwrap());
        talker.name = "Director".to_string();
        talker.tx_channels = [1].into();
        talker.talk_mode = TalkMode::Open;
        let mut unrelated = Session::new();
        unrelated.addr = Some("127.0.0.1:10003".parse().unwrap());
        unrelated.listen_channels = [9].into();
        let mut stale_zero = Session::new();
        stale_zero.addr = Some("127.0.0.1:10004".parse().unwrap());
        stale_zero.listen_channels = [1].into();
        stale_zero.tx_channels = [1].into();
        state.sessions.write().await.extend([
            (0, stale_zero),
            (1, listener),
            (2, talker),
            (3, unrelated),
        ]);
        record_input_health(
            &state,
            2,
            AudioTarget::Channel(1),
            &vec![i16::MAX / 2; MIX_SAMPLES_PER_FRAME],
        )
        .await;

        let rosters = channel_rosters_for_user(&state, 1).await;
        assert_eq!(rosters.len(), default_workflow_channels().len());
        assert!(rosters.iter().any(|roster| {
            roster.channel_id == CHANNEL_OPEN
                && roster.name.as_deref() == Some("open")
                && roster.members.is_empty()
        }));
        assert!(!rosters.iter().any(|roster| roster.channel_id == 9));
        let program = rosters
            .iter()
            .find(|roster| roster.channel_id == CHANNEL_PROGRAM)
            .unwrap();
        assert_eq!(program.name.as_deref(), Some("Program"));
        assert!(program
            .members
            .iter()
            .any(|member| member.user_id == 1 && member.present));
        assert!(program.members.iter().any(|member| {
            member.user_id == 2 && member.name == "Director" && member.transmitting
        }));
        assert!(!program.members.iter().any(|member| member.user_id == 0));
        assert!(!program.members.iter().any(|member| member.user_id == 3));
    }

    #[tokio::test]
    async fn audio_send_error_clears_stale_endpoint_and_increments_metric() {
        let state = ServerState::default();
        let addr = "127.0.0.1:50000".parse().unwrap();
        {
            let mut sessions = state.sessions.write().await;
            let mut session = Session::new();
            session.addr = Some(addr);
            sessions.insert(1, session);
        }

        let err = std::io::Error::new(std::io::ErrorKind::HostUnreachable, "no route");
        record_audio_send_error(&state, 1, addr, &err).await;

        assert_eq!(state.metrics.snapshot().audio_send_errors, 1);
        assert_eq!(state.sessions.read().await.get(&1).unwrap().addr, None);
    }

    #[test]
    fn opus_payload_round_trips_one_frame() {
        let samples = (0..MIX_SAMPLES_PER_FRAME)
            .map(|index| {
                let phase = index as f32 / MIX_SAMPLES_PER_FRAME as f32;
                (phase.sin() * 4_000.0) as i16
            })
            .collect::<Vec<_>>();

        let mut encoder = AudioEncoderBank::default();
        let mut decoder = AudioDecoderBank::default();

        let encoded = encoder
            .encode(1, Codec::Opus, OpusProfile::Speech24Standard, &samples, 1)
            .unwrap();
        let decoded = decoder
            .decode(1, Codec::Opus, OpusProfile::Speech24Standard, &encoded)
            .unwrap();

        assert!(!encoded.is_empty());
        assert!(encoded.len() < common::PCM16_PAYLOAD_BYTES);
        assert_eq!(decoded.len(), MIX_SAMPLES_PER_FRAME);
    }

    #[test]
    fn stereo_opus_payload_encodes_interleaved_frame() {
        let samples = (0..MIX_SAMPLES_PER_FRAME)
            .flat_map(|index| {
                let phase = index as f32 / MIX_SAMPLES_PER_FRAME as f32;
                [
                    (phase.sin() * 5_000.0) as i16,
                    (phase.cos() * 2_000.0) as i16,
                ]
            })
            .collect::<Vec<_>>();

        let mut encoder = AudioEncoderBank::default();
        let encoded = encoder
            .encode(1, Codec::Opus, OpusProfile::Speech48High, &samples, 2)
            .unwrap();
        let mut decoder = opus::Decoder::new(
            OpusProfile::Speech48High.sample_rate_hz(),
            opus::Channels::Stereo,
        )
        .unwrap();
        let mut decoded = vec![0_i16; MIX_SAMPLES_PER_FRAME * 2];
        let decoded_per_channel = decoder.decode(&encoded, &mut decoded, false).unwrap();

        assert!(!encoded.is_empty());
        assert_eq!(decoded_per_channel, MIX_SAMPLES_PER_FRAME);
        assert!(decoded.iter().any(|sample| *sample != 0));
    }

    #[test]
    fn server_opus_encoder_uses_intercom_profile() {
        let mut encoder = opus::Encoder::new(
            OpusProfile::Speech24Standard.sample_rate_hz(),
            opus::Channels::Mono,
            opus::Application::Voip,
        )
        .unwrap();

        configure_opus_encoder(&mut encoder, OpusProfile::Speech24Standard, 1).unwrap();

        assert_eq!(
            encoder.get_bitrate().unwrap(),
            opus::Bitrate::Bits(OpusProfile::Speech24Standard.bitrate_bps(1))
        );
        assert_eq!(encoder.get_signal().unwrap(), opus::Signal::Voice);
        assert_eq!(
            encoder.get_max_bandwidth().unwrap(),
            opus::Bandwidth::Superwideband
        );
        assert!(encoder.get_vbr().unwrap());
        assert!(encoder.get_vbr_constraint().unwrap());
        assert_eq!(
            encoder.get_complexity().unwrap(),
            OpusProfile::Speech24Standard.complexity()
        );
        assert!(encoder.get_inband_fec().unwrap());
        assert_eq!(
            encoder.get_packet_loss_perc().unwrap(),
            common::OPUS_PACKET_LOSS_PERCENT
        );
        assert!(!encoder.get_dtx().unwrap());
    }

    #[test]
    fn codec_paths_normalize_to_mixer_domain() {
        let mix_samples = vec![1_000; MIX_SAMPLES_PER_FRAME];
        let narrow_samples = vec![1_000; common::SAMPLES_PER_FRAME];
        let medium_samples = vec![1_000; common::PCM24_SAMPLES_PER_FRAME];

        let mut encoder = AudioEncoderBank::default();
        let mut decoder = AudioDecoderBank::default();

        let pcm16_payload = encoder
            .encode(1, Codec::Pcm16, OpusProfile::default(), &mix_samples, 1)
            .unwrap();
        let pcm24_payload = encoder
            .encode(1, Codec::Pcm24, OpusProfile::default(), &mix_samples, 1)
            .unwrap();
        let pcm48_payload = encoder
            .encode(1, Codec::Pcm48, OpusProfile::default(), &mix_samples, 1)
            .unwrap();
        let opus_payload = encoder
            .encode(1, Codec::Opus, OpusProfile::default(), &mix_samples, 1)
            .unwrap();

        assert_eq!(pcm16_payload.len(), common::PCM16_PAYLOAD_BYTES);
        assert_eq!(pcm24_payload.len(), common::PCM24_PAYLOAD_BYTES);
        assert_eq!(pcm48_payload.len(), common::PCM48_PAYLOAD_BYTES);
        assert!(!opus_payload.is_empty());

        let pcm16_decoded = decoder
            .decode(
                1,
                Codec::Pcm16,
                OpusProfile::default(),
                &pcm16_samples_to_le_bytes(&narrow_samples),
            )
            .unwrap();
        let pcm24_decoded = decoder
            .decode(
                1,
                Codec::Pcm24,
                OpusProfile::default(),
                &pcm16_samples_to_le_bytes(&medium_samples),
            )
            .unwrap();
        let pcm48_decoded = decoder
            .decode(1, Codec::Pcm48, OpusProfile::default(), &pcm48_payload)
            .unwrap();
        let opus_decoded = decoder
            .decode(1, Codec::Opus, OpusProfile::default(), &opus_payload)
            .unwrap();

        assert_eq!(pcm16_decoded.len(), MIX_SAMPLES_PER_FRAME);
        assert_eq!(pcm24_decoded.len(), MIX_SAMPLES_PER_FRAME);
        assert_eq!(pcm48_decoded.len(), MIX_SAMPLES_PER_FRAME);
        assert_eq!(opus_decoded.len(), MIX_SAMPLES_PER_FRAME);
    }

    #[test]
    fn low_rate_voice_enhancer_keeps_silence_and_bounds() {
        let mut silence = vec![0; MIX_SAMPLES_PER_FRAME];
        enhance_low_rate_voice(&mut silence);
        assert!(silence.iter().all(|sample| *sample == 0));

        let mut loud = vec![i16::MAX; MIX_SAMPLES_PER_FRAME];
        enhance_low_rate_voice(&mut loud);
        assert!(loud.iter().all(|sample| *sample >= 0));
    }

    #[test]
    fn limiter_leaves_quiet_frame_unchanged() {
        let input = vec![0.25; MIX_SAMPLES_PER_FRAME];
        let (limited, meter, event) = limit_frame(&input);

        assert!(!event);
        assert_eq!(meter.limiter_gain, 1.0);
        assert_eq!(limited[0], (0.25 * i16::MAX as f32).round() as i16);
    }

    #[test]
    fn limiter_reduces_hot_frame_without_clipping() {
        let input = vec![1.5; MIX_SAMPLES_PER_FRAME];
        let (limited, meter, event) = limit_frame(&input);

        assert!(event);
        assert!(meter.limiter_gain < 1.0);
        assert!(meter.limiter_reduction_db > 0.0);
        assert!(limited.iter().all(|sample| *sample < i16::MAX));
        assert!(meter.peak <= LIMITER_THRESHOLD + 0.01);
    }

    #[test]
    fn stereo_limiter_uses_linked_gain() {
        let left = vec![2.0; MIX_SAMPLES_PER_FRAME];
        let right = vec![0.5; MIX_SAMPLES_PER_FRAME];

        let (limited, meter, event) = limit_stereo_frame(&left, &right);

        assert!(event);
        assert_eq!(limited.len(), MIX_SAMPLES_PER_FRAME * 2);
        assert!(meter.peak <= LIMITER_THRESHOLD + 0.01);
        assert!(meter.limiter_gain < 1.0);
        assert!(limited[0] > limited[1]);
    }

    #[tokio::test]
    async fn stereo_pcm48_mix_pans_channels_per_listener() {
        let state = ServerState::default();
        {
            let mut sessions = state.sessions.write().await;
            let mut left_talker = Session::new();
            left_talker.talk_mode = TalkMode::Open;
            left_talker.tx_channels = [1].into();
            sessions.insert(1, left_talker);

            let mut right_talker = Session::new();
            right_talker.talk_mode = TalkMode::Open;
            right_talker.tx_channels = [2].into();
            sessions.insert(2, right_talker);

            let mut listener = Session::new();
            listener.addr = Some("127.0.0.1:50000".parse().unwrap());
            listener.listen_channels = [1, 2].into();
            listener.output_codec = Codec::Pcm48;
            listener.stereo = StereoConfig {
                enabled: true,
                channel_pan: [(1, -1.0), (2, 1.0)].into(),
            };
            sessions.insert(3, listener);
        }
        {
            let mut sources = state.sources.write().await;
            let mut left = SourceQueue::new();
            left.push(AudioTarget::Channel(1), vec![8_000; MIX_SAMPLES_PER_FRAME]);
            sources.insert(1, left);
            let mut right = SourceQueue::new();
            right.push(AudioTarget::Channel(2), vec![2_000; MIX_SAMPLES_PER_FRAME]);
            sources.insert(2, right);
        }

        let outputs = build_mixes(&state).await;

        assert_eq!(outputs.len(), 1);
        let output = &outputs[0];
        assert_eq!(output.user_id, 3);
        assert_eq!(output.codec, Codec::Pcm48);
        assert_eq!(output.channels, 2);
        assert_eq!(output.samples.len(), MIX_SAMPLES_PER_FRAME * 2);
        assert!(output.samples[0] > output.samples[1]);
    }

    #[tokio::test]
    async fn virtual_tts_source_emits_every_frame_without_live_queue_expiry() {
        let state = Arc::new(ServerState::default());
        let mut listener = Session::new();
        listener.addr = Some("127.0.0.1:50001".parse().unwrap());
        listener.listen_channels = [7].into();
        listener.output_codec = Codec::Pcm48;
        state.sessions.write().await.insert(2, listener);

        let frames = vec![
            vec![1_000; MIX_SAMPLES_PER_FRAME],
            vec![2_000; MIX_SAMPLES_PER_FRAME],
            vec![3_000; MIX_SAMPLES_PER_FRAME],
        ];
        start_tts_source(
            Arc::clone(&state),
            TTS_SOURCE_USER_BASE,
            AudioTarget::Channel(7),
            frames,
            false,
            false,
        )
        .await;

        tokio::time::sleep(ACTIVE_SOURCE_WINDOW + Duration::from_millis(20)).await;

        for expected in [1_000, 2_000, 3_000] {
            let outputs = build_mixes(&state).await;
            let output = outputs
                .iter()
                .find(|output| output.user_id == 2)
                .expect("listener should receive virtual TTS frame");
            assert_eq!(output.samples[0], expected);
        }

        assert!(build_mixes(&state).await.is_empty());
        assert!(state.virtual_sources.read().await.is_empty());
    }

    #[tokio::test]
    async fn virtual_tts_sources_for_multiple_targets_do_not_interfere() {
        let state = Arc::new(ServerState::default());
        {
            let mut sessions = state.sessions.write().await;
            let mut first = Session::new();
            first.addr = Some("127.0.0.1:50002".parse().unwrap());
            first.listen_channels = [7].into();
            first.output_codec = Codec::Pcm48;
            sessions.insert(2, first);

            let mut second = Session::new();
            second.addr = Some("127.0.0.1:50003".parse().unwrap());
            second.listen_channels = [8].into();
            second.output_codec = Codec::Pcm48;
            sessions.insert(3, second);
        }

        start_tts_source(
            Arc::clone(&state),
            TTS_SOURCE_USER_BASE,
            AudioTarget::Channel(7),
            vec![vec![1_000; MIX_SAMPLES_PER_FRAME]],
            false,
            false,
        )
        .await;
        start_tts_source(
            Arc::clone(&state),
            TTS_SOURCE_USER_BASE + 1,
            AudioTarget::Channel(8),
            vec![vec![2_000; MIX_SAMPLES_PER_FRAME]],
            false,
            false,
        )
        .await;

        let outputs = build_mixes(&state).await;
        let first = outputs
            .iter()
            .find(|output| output.user_id == 2)
            .expect("first listener should receive first TTS source");
        let second = outputs
            .iter()
            .find(|output| output.user_id == 3)
            .expect("second listener should receive second TTS source");
        assert_eq!(first.samples[0], 1_000);
        assert_eq!(second.samples[0], 2_000);
    }

    #[test]
    fn stereo_status_warns_until_supported_codec_is_active() {
        let mut session = Session::new();
        session.output_codec = Codec::Pcm16;
        session.stereo.enabled = true;

        let status = stereo_status_for_session(&session);

        assert!(!status.active);
        assert_eq!(status.channels, common::CHANNELS);
        assert_eq!(
            status.warning.as_deref(),
            Some("stereo receive requires pcm48 or opus")
        );

        session.output_codec = Codec::Opus;
        let status = stereo_status_for_session(&session);
        assert!(status.active);
        assert_eq!(status.channels, 2);
        assert!(status.warning.is_none());

        session.output_codec = Codec::Pcm48;
        let status = stereo_status_for_session(&session);
        assert!(status.active);
        assert_eq!(status.channels, 2);
    }

    #[test]
    fn meters_report_silence_and_signal() {
        let silence = vec![0; MIX_SAMPLES_PER_FRAME];
        let tone = vec![i16::MAX / 2; MIX_SAMPLES_PER_FRAME];

        let silence_meter = measure_i16(&silence);
        assert_eq!(silence_meter.peak, 0.0);
        assert_eq!(silence_meter.rms, 0.0);

        let tone_meter = measure_i16(&tone);
        assert!(tone_meter.peak > 0.49);
        assert!(tone_meter.rms > 0.49);
    }

    #[tokio::test]
    async fn active_talker_holds_then_decays() {
        let state = ServerState::default();
        let samples = vec![i16::MAX / 2; MIX_SAMPLES_PER_FRAME];
        record_input_health(&state, 1, AudioTarget::Channel(2), &samples).await;

        let (sessions, _) = status_snapshot_with_session(&state, 1).await;
        assert!(sessions[0].input.active);
        assert_eq!(sessions[0].input.last_channel, Some(2));

        {
            let mut health = state.health.write().await;
            health.get_mut(&1).unwrap().active_until = Instant::now() - Duration::from_millis(1);
        }

        let (sessions, _) = status_snapshot_with_session(&state, 1).await;
        assert!(!sessions[0].input.active);
    }

    #[tokio::test]
    async fn capture_health_updates_status_snapshot() {
        let state = ServerState::default();
        let capture = test_capture_health("left", 0, 0, 0.12, 0.32, 0.01);
        record_capture_health(&state, 1, capture.clone()).await;

        let (sessions, _) = status_snapshot(&state).await;
        assert_eq!(sessions[0].user_id, 1);
        assert_eq!(sessions[0].capture, Some(capture));
    }

    #[tokio::test]
    async fn bridge_status_updates_status_snapshot() {
        let state = ServerState::default();
        let status = BridgeStatus {
            mode: common::BridgeMode::Output,
            input_device: None,
            output_device: Some("USB Audio".to_string()),
            input_gain: 2.0,
            output_gain: 0.5,
            tx: vec![5, 5, 0],
            listen: vec![30, 10],
            note: " PA output ".to_string(),
        };

        record_bridge_status(&state, 90, status).await;

        let (sessions, _) = status_snapshot(&state).await;
        assert_eq!(sessions[0].user_id, 90);
        assert_eq!(sessions[0].role, ClientRole::Bridge);
        let bridge = sessions[0].bridge.as_ref().unwrap();
        assert_eq!(bridge.mode, common::BridgeMode::Output);
        assert_eq!(bridge.output_device.as_deref(), Some("USB Audio"));
        assert_eq!(bridge.tx, vec![0, 5]);
        assert_eq!(bridge.listen, vec![10, 30]);
        assert_eq!(bridge.note, "PA output");
    }

    #[tokio::test]
    async fn admin_warnings_report_live_bridge_feedback_overlap() {
        let state = ServerState::default();
        record_bridge_status(
            &state,
            91,
            BridgeStatus {
                mode: common::BridgeMode::Duplex,
                tx: vec![6],
                listen: vec![6],
                ..BridgeStatus::default()
            },
        )
        .await;

        let (sessions, _) = status_snapshot(&state).await;
        let admin_state = PersistedAdminState {
            channels: Vec::new(),
            devices: Vec::new(),
            clients: Vec::new(),
            presets: Vec::new(),
            templates: Vec::new(),
        };
        let warnings = admin_warnings(&admin_state, &sessions);

        assert!(warnings
            .iter()
            .any(|warning| warning.message.contains("avoid feedback")));
    }

    #[test]
    fn capture_health_generates_quality_warnings() {
        let clipping =
            capture_health_warnings(7, &test_capture_health("left", 3, 2, 0.2, 1.0, 0.0));
        assert!(clipping
            .iter()
            .any(|warning| warning.message.contains("clipping")));

        let silent =
            capture_health_warnings(7, &test_capture_health("left", 0, 0, 0.001, 0.01, 0.0));
        assert!(silent
            .iter()
            .any(|warning| warning.message.contains("nearly silent")));

        let dc = capture_health_warnings(7, &test_capture_health("left", 0, 0, 0.1, 0.2, 0.12));
        assert!(dc
            .iter()
            .any(|warning| warning.message.contains("DC offset")));
    }

    fn test_capture_health(
        capture_channel: &str,
        raw_clipped_samples: u32,
        software_clipped_samples: u32,
        selected_rms: f32,
        selected_peak: f32,
        selected_dc_offset: f32,
    ) -> CaptureHealthStatus {
        let selected = common::CaptureChannelHealth {
            rms: selected_rms,
            peak: selected_peak,
            dc_offset: selected_dc_offset,
        };
        CaptureHealthStatus {
            runtime: None,
            audio: None,
            playback: None,
            client_transport: None,
            codec_config: None,
            desktop: None,
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
            adc_input: "difference".to_string(),
            mic_pga_gain_db: 9,
            capture_channel: capture_channel.to_string(),
            software_gain_percent: 100,
            high_pass_enabled: true,
            alc_enabled: true,
            noise_gate_enabled: true,
            left: selected.clone(),
            right: common::CaptureChannelHealth {
                rms: 0.01,
                peak: 0.03,
                dc_offset: 0.0,
            },
            selected,
            raw_clipped_samples,
            software_clipped_samples,
        }
    }

    #[test]
    fn admin_ui_is_split_into_operator_pages() {
        assert!(ADMIN_DASHBOARD_HTML.contains("data-page=\"dashboard\""));
        assert!(ADMIN_CLIENTS_HTML.contains("data-page=\"clients\""));
        assert!(ADMIN_ROUTING_HTML.contains("data-page=\"routing\""));
        assert!(ADMIN_PRESETS_HTML.contains("data-page=\"presets\""));
        assert!(ADMIN_CALLS_HTML.contains("data-page=\"calls\""));
        assert!(ADMIN_RECORDING_HTML.contains("data-page=\"recording\""));
        assert!(ADMIN_SYSTEM_HTML.contains("data-page=\"system\""));

        for page in [
            ADMIN_DASHBOARD_HTML,
            ADMIN_CLIENTS_HTML,
            ADMIN_ROUTING_HTML,
            ADMIN_PRESETS_HTML,
            ADMIN_CALLS_HTML,
            ADMIN_RECORDING_HTML,
            ADMIN_SYSTEM_HTML,
        ] {
            assert!(page.contains("/admin/clients/"));
            assert!(page.contains("/admin/routing/"));
            assert!(page.contains("/admin/presets/"));
            assert!(page.contains("/admin/calls/"));
            assert!(page.contains("/admin/recording/"));
            assert!(page.contains("/admin/system/"));
            assert!(page.contains("/admin/app.js"));
            assert!(page.contains("/admin/style.css"));
            assert!(page.contains("/admin/branding/redline-logo.png"));
        }

        assert!(!ADMIN_LOGO_PNG.is_empty());
        assert!(ADMIN_JS.contains("function renderDashboardPage()"));
        assert!(ADMIN_JS.contains("function renderClientsPage()"));
        assert!(ADMIN_JS.contains("function clientCards()"));
        assert!(ADMIN_JS.contains("client-card-list"));
        assert!(ADMIN_JS.contains("function clientEditorName("));
        assert!(ADMIN_JS.contains("Stable Device UID<span class=\"readonly-value\""));
        assert!(ADMIN_JS.contains("id=\"client-uid\" type=\"hidden\""));
        assert!(!ADMIN_JS.contains("Stable Device UID<input id=\"client-uid\" type=\"text\""));
        assert!(ADMIN_JS.contains("data-delete-device"));
        assert!(ADMIN_JS.contains("async function deleteDevice("));
        assert!(ADMIN_JS.contains("method: 'DELETE'"));
        assert!(ADMIN_CSS.contains(".client-card-list"));
        assert!(ADMIN_CSS.contains(".readonly-value"));
        assert!(!ADMIN_JS.contains("<th>User</th><th>UID</th><th>Role"));
        assert!(ADMIN_JS.contains("function renderRoutingPage()"));
        assert!(ADMIN_JS.contains("function renderPresetsPage()"));
        assert!(ADMIN_JS.contains("function renderCallsPage()"));
        assert!(ADMIN_JS.contains("Alert / Announcement"));
        assert!(ADMIN_JS.contains("announcement-text-alert"));
        assert!(ADMIN_JS.contains("api('/announcements'"));
        assert!(ADMIN_JS.contains("function renderRecordingPage()"));
        assert!(ADMIN_JS.contains("function updateRecordingPage()"));
        assert!(ADMIN_JS.contains("recordingModelTouched"));
        assert!(ADMIN_JS.contains("function renderSystemPage()"));
        assert!(ADMIN_JS.contains("if (!modalRoot().innerHTML) refresh()"));
        assert!(ADMIN_JS.contains("data-editor-vol"));
        assert!(ADMIN_JS.contains("data-ifb-program"));
        assert!(ADMIN_JS.contains("data-button-tx-users"));
        assert!(ADMIN_JS.contains("data-button-color"));
        assert!(ADMIN_JS.contains("data-button-alert-id"));
        assert!(ADMIN_JS.contains("function clampPanUi"));
        assert!(ADMIN_JS.contains("ArrowLeft"));
        assert!(ADMIN_JS.contains("METER_FLOOR_DB"));
        assert!(ADMIN_JS.contains("dbfsText"));
        assert!(ADMIN_JS.contains("class=\"peak\""));
        assert!(ADMIN_JS.contains("function deepFilterNetModelOptions"));
        assert!(ADMIN_JS.contains("function deepFilterNetCoreMlStatusHtml"));
        assert!(ADMIN_JS.contains("processing-deep-filter-model"));
        assert!(ADMIN_JS.contains("coreml_packages"));
        assert!(ADMIN_CSS.contains(".pan-slider-wrap::after"));
    }

    #[tokio::test]
    async fn deepfilternet_model_folder_lists_supported_archives() {
        let dir = temp_state_path("deepfilternet-models");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("DeepFilterNet3_onnx.tar.gz"), b"model")
            .await
            .unwrap();
        tokio::fs::write(dir.join("DeepFilterNet3_ll_onnx.tar.gz"), b"model")
            .await
            .unwrap();
        tokio::fs::write(dir.join("DeepFilterNet3.zip"), b"model")
            .await
            .unwrap();
        tokio::fs::write(dir.join("notes.txt"), b"ignore")
            .await
            .unwrap();

        let models = list_deepfilternet_models_from_dir(&dir).await;
        let names = models
            .iter()
            .map(|model| model.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "DeepFilterNet3_ll_onnx.tar.gz",
                "DeepFilterNet3_onnx.tar.gz"
            ]
        );
        assert!(models
            .iter()
            .all(|model| model.path.contains("DeepFilterNet3")));

        let _ = tokio::fs::remove_dir_all(dir).await;
    }

    #[tokio::test]
    async fn deepfilternet_model_folder_lists_coreml_packages_separately() {
        let dir = temp_state_path("deepfilternet-coreml-models");
        let package = dir.join("DeepFilterNet3_ll_coreml");
        tokio::fs::create_dir_all(package.join("enc.mlmodelc"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(package.join("erb_dec.mlmodelc"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(package.join("df_dec.mlmodelc"))
            .await
            .unwrap();
        tokio::fs::write(
            package.join("config.ini"),
            b"[df]\nsr = 48000\nhop_size = 480\nfft_size = 960\nmin_nb_erb_freqs = 2\nnb_erb = 32\nnb_df = 96\ndf_order = 5\ndf_lookahead = 2\n[deepfilternet]\nconv_lookahead = 2\n",
        )
        .await
        .unwrap();
        tokio::fs::write(package.join("metadata.json"), b"{}")
            .await
            .unwrap();
        tokio::fs::create_dir_all(dir.join("DeepFilterNet3_broken_coreml"))
            .await
            .unwrap();
        tokio::fs::write(dir.join("DeepFilterNet3_onnx.tar.gz"), b"model")
            .await
            .unwrap();

        let runtime_models = list_deepfilternet_models_from_dir(&dir).await;
        assert_eq!(runtime_models.len(), 2);
        assert!(runtime_models
            .iter()
            .any(|model| model.name == "DeepFilterNet3_onnx.tar.gz" && model.runtime == "tract"));
        assert!(runtime_models
            .iter()
            .any(|model| model.name == "DeepFilterNet3_ll_coreml" && model.runtime == "coreml"));

        let packages = list_deepfilternet_coreml_packages_from_dir(&dir).await;
        assert_eq!(packages.len(), 2);
        let complete = packages
            .iter()
            .find(|package| package.name == "DeepFilterNet3_ll_coreml")
            .unwrap();
        assert!(complete.complete);
        assert!(complete.detail.is_none());
        let broken = packages
            .iter()
            .find(|package| package.name == "DeepFilterNet3_broken_coreml")
            .unwrap();
        assert!(!broken.complete);
        assert!(broken
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("enc.mlmodelc")));

        let _ = tokio::fs::remove_dir_all(dir).await;
    }

    #[cfg(feature = "processing-deepfilternet")]
    #[test]
    fn deepfilternet_worker_processes_local_model_when_present() {
        let model_path = PathBuf::from("deepfilternet-models/DeepFilterNet3_onnx.tar.gz");
        if !model_path.exists() {
            return;
        }

        let key = DeepFilterNetConfigKey {
            model_path,
            profile: ProcessingProfile::VoiceIsolation,
            backend: DeepFilterBackend::Auto,
            compute_units: AppleComputeUnits::All,
        };
        let mut worker = DeepFilterNetWorkerState::spawn(key, 2);
        let mut samples = [0_i16; MIX_SAMPLES_PER_FRAME];
        for (index, sample) in samples.iter_mut().enumerate() {
            let phase = index as f32 * 440.0 * std::f32::consts::TAU / MIX_SAMPLE_RATE as f32;
            *sample = (phase.sin() * 5000.0).round() as i16;
        }

        let result = worker.process(&samples, Duration::from_secs(10)).unwrap();
        assert_eq!(result.samples.len(), MIX_SAMPLES_PER_FRAME);
        assert!(result.model_name.contains("DeepFilterNet3"));
        assert!(result.lsnr_db.is_finite());
    }

    #[cfg(all(feature = "processing-deepfilternet-coreml", target_os = "macos"))]
    #[test]
    fn deepfilternet_coreml_processor_runs_local_package_when_present() {
        let model_path = PathBuf::from("deepfilternet-coreml-models/DeepFilterNet3_ll_coreml");
        if !model_path.exists() {
            return;
        }

        let key = DeepFilterNetConfigKey {
            model_path,
            profile: ProcessingProfile::VoiceIsolation,
            backend: DeepFilterBackend::CoreMl,
            compute_units: AppleComputeUnits::All,
        };
        let mut processor = DeepFilterNetFrameProcessor::new(key).unwrap();
        let mut samples = [0_i16; MIX_SAMPLES_PER_FRAME];
        for (index, sample) in samples.iter_mut().enumerate() {
            let phase = index as f32 * 440.0 * std::f32::consts::TAU / MIX_SAMPLE_RATE as f32;
            *sample = (phase.sin() * 5000.0).round() as i16;
        }

        let result = processor.process(samples).unwrap();
        assert_eq!(result.samples.len(), MIX_SAMPLES_PER_FRAME);
        assert_eq!(result.active_backend, DeepFilterBackend::CoreMl);
        assert!(result.model_name.contains("DeepFilterNet3"));
        assert!(result.lsnr_db.is_finite());
    }

    #[tokio::test]
    async fn button_events_update_effective_tx_routes() {
        let state = ServerState::default();
        let desired = DesiredClientConfig {
            user_id: 1,
            client_uid: None,
            role: ClientRole::Client,
            name: String::new(),
            listen: vec![1],
            tx: vec![1],
            vol: HashMap::new(),
            talker_vol: HashMap::new(),
            codec: Codec::Pcm16,
            opus_profile: OpusProfile::default(),
            talk_mode: TalkMode::Muted,
            priority: false,
            priority_channels: Vec::new(),
            buttons: vec![
                TalkButtonConfig {
                    id: "director".to_string(),
                    label: "Director".to_string(),
                    color: None,
                    mode: TalkButtonMode::Momentary,
                    actions: vec![TalkButtonAction::Transmit {
                        channels: vec![2, 3],
                        users: Vec::new(),
                        duck: false,
                    }],
                },
                TalkButtonConfig {
                    id: "pa".to_string(),
                    label: "PA".to_string(),
                    color: None,
                    mode: TalkButtonMode::Latching,
                    actions: vec![TalkButtonAction::Transmit {
                        channels: vec![3, 4],
                        users: Vec::new(),
                        duck: false,
                    }],
                },
            ],
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: Esp32AudioConfig::default(),
            processing: ProcessingConfig::default(),
        };
        assert!(matches!(
            apply_desired_client(&state, desired).await,
            ControlResponse::Ack
        ));

        assert!(matches!(
            apply_button_event(&state, 1, "director".to_string(), true).await,
            ControlResponse::Ack
        ));
        {
            let sessions = state.sessions.read().await;
            assert_eq!(
                sorted_channels(&sessions.get(&1).unwrap().effective_tx_channels()),
                vec![2, 3]
            );
        }

        assert!(matches!(
            apply_button_event(&state, 1, "pa".to_string(), true).await,
            ControlResponse::Ack
        ));
        {
            let sessions = state.sessions.read().await;
            assert_eq!(
                sorted_channels(&sessions.get(&1).unwrap().effective_tx_channels()),
                vec![2, 3, 4]
            );
        }

        assert!(matches!(
            apply_button_event(&state, 1, "director".to_string(), false).await,
            ControlResponse::Ack
        ));
        assert!(matches!(
            apply_button_event(&state, 1, "pa".to_string(), false).await,
            ControlResponse::Ack
        ));
        {
            let sessions = state.sessions.read().await;
            assert_eq!(
                sorted_channels(&sessions.get(&1).unwrap().effective_tx_channels()),
                vec![3, 4]
            );
        }

        assert!(matches!(
            apply_button_event(&state, 1, "pa".to_string(), true).await,
            ControlResponse::Ack
        ));
        {
            let sessions = state.sessions.read().await;
            assert!(sessions.get(&1).unwrap().effective_tx_channels().is_empty());
        }
    }

    #[test]
    fn normalize_button_configs_keeps_only_safe_hex_colors() {
        let buttons = normalize_button_configs(vec![
            TalkButtonConfig {
                id: " 10 ".to_string(),
                label: " ".to_string(),
                color: Some(" #f0a ".to_string()),
                mode: TalkButtonMode::Momentary,
                actions: Vec::new(),
            },
            TalkButtonConfig {
                id: "2".to_string(),
                label: "Two".to_string(),
                color: Some("url(javascript:bad)".to_string()),
                mode: TalkButtonMode::Momentary,
                actions: Vec::new(),
            },
        ]);

        assert_eq!(buttons[0].id, "2");
        assert_eq!(buttons[0].color, None);
        assert_eq!(buttons[1].id, "10");
        assert_eq!(buttons[1].label, "10");
        assert_eq!(buttons[1].color.as_deref(), Some("#f0a"));
    }

    #[tokio::test]
    async fn clear_active_buttons_removes_stuck_routes() {
        let state = ServerState::default();
        let mut session = Session::new();
        session.buttons = vec![TalkButtonConfig {
            id: "pa".to_string(),
            label: "PA".to_string(),
            color: None,
            mode: TalkButtonMode::Latching,
            actions: vec![TalkButtonAction::Transmit {
                channels: vec![9],
                users: Vec::new(),
                duck: false,
            }],
        }];
        session.active_buttons.insert("pa".to_string());
        session
            .active_direct_calls
            .insert(2, ActiveDirectCall { duck: false });
        session.direct_call_history.push(DirectCallHistory {
            caller: 1,
            target: 2,
            started: Instant::now(),
            ended: None,
            duck: false,
            source_button: None,
        });
        let mut target = Session::new();
        target.direct_call_history.push(DirectCallHistory {
            caller: 1,
            target: 2,
            started: Instant::now(),
            ended: None,
            duck: false,
            source_button: None,
        });
        state.sessions.write().await.insert(1, session);
        state.sessions.write().await.insert(2, target);

        clear_active_buttons(&state, 1).await;

        let sessions = state.sessions.read().await;
        let caller = sessions.get(&1).unwrap();
        let target = sessions.get(&2).unwrap();
        assert!(caller.active_buttons.is_empty());
        assert!(caller.active_direct_calls.is_empty());
        assert!(caller.direct_call_history.last().unwrap().ended.is_some());
        assert!(target.direct_call_history.last().unwrap().ended.is_some());
    }

    #[tokio::test]
    async fn alerts_route_ack_and_cancel() {
        let state = ServerState::default();
        let mut sender = Session::new();
        sender.addr = Some("127.0.0.1:10001".parse().unwrap());
        let mut listener = Session::new();
        listener.addr = Some("127.0.0.1:10002".parse().unwrap());
        listener.listen_channels = [7].into();
        let mut unrelated = Session::new();
        unrelated.addr = Some("127.0.0.1:10003".parse().unwrap());
        unrelated.listen_channels = [8].into();
        state
            .sessions
            .write()
            .await
            .extend([(1, sender), (2, listener), (3, unrelated)]);

        assert!(matches!(
            apply_send_alert_event(&state, 1, AlertTarget::User(2), Some("Call me".to_string()),)
                .await,
            ControlResponse::Ack
        ));
        {
            let alerts = state.alerts.read().await;
            let (active_for_2, _) = alert_statuses_for_user(&alerts, 2);
            let (active_for_3, _) = alert_statuses_for_user(&alerts, 3);
            assert_eq!(active_for_2.len(), 1);
            assert_eq!(active_for_2[0].message.as_deref(), Some("Call me"));
            assert!(active_for_3.is_empty());
        }

        assert!(matches!(
            apply_ack_alert_event(&state, 2, 1).await,
            ControlResponse::Ack
        ));
        {
            let alerts = state.alerts.read().await;
            let (active, recent) = alert_statuses_for_user(&alerts, 2);
            assert!(active.is_empty());
            assert_eq!(recent.len(), 1);
            assert!(recent[0].recipients[0].acked_at_ms.is_some());
        }

        assert!(matches!(
            apply_send_alert_event(&state, 1, AlertTarget::Channel(7), None).await,
            ControlResponse::Ack
        ));
        {
            let alerts = state.alerts.read().await;
            let channel_alert = alerts.iter().find(|alert| alert.id == 2).unwrap();
            assert_eq!(
                channel_alert
                    .recipients
                    .iter()
                    .map(|recipient| recipient.user_id)
                    .collect::<Vec<_>>(),
                vec![2]
            );
        }

        assert!(matches!(
            apply_cancel_alert_event(&state, 1, 2).await,
            ControlResponse::Ack
        ));
        {
            let alerts = state.alerts.read().await;
            let (_, recent) = alert_statuses_for_user(&alerts, 2);
            assert!(recent.iter().any(|alert| alert.cancelled));
        }
    }

    #[tokio::test]
    async fn tts_announcement_creates_visible_alert_and_routes_audio() {
        let state = Arc::new(ServerState::default());
        let mut listener = Session::new();
        listener.addr = Some("127.0.0.1:10002".parse().unwrap());
        listener.listen_channels = [7].into();
        listener.output_codec = Codec::Pcm48;
        state.sessions.write().await.insert(2, listener);

        let response = apply_tts_announcement_event(
            &state,
            AdminTtsRequest {
                sender: SERVER_USER_ID,
                targets: vec![AlertTarget::Channel(7)],
                target: None,
                message: "Stand by".to_string(),
                priority: true,
                duck: false,
                gain: 0.18,
            },
        )
        .await
        .unwrap();

        assert_eq!(response.recipients, vec![2]);
        assert_eq!(response.targets, vec![AlertTarget::Channel(7)]);
        assert_eq!(response.engine, Some("supertonic"));
        assert!(response.duration_ms.is_some_and(|duration| duration > 0));
        {
            let alerts = state.alerts.read().await;
            let (active, _) = alert_statuses_for_user(&alerts, 2);
            assert_eq!(active.len(), 1);
            assert_eq!(active[0].message.as_deref(), Some("Stand by"));
        }

        let outputs = build_mixes(&state).await;
        let output = outputs.iter().find(|output| output.user_id == 2).unwrap();
        assert_eq!(output.codec, Codec::Pcm48);
        assert!(output.samples.iter().any(|sample| *sample != 0));
    }

    #[tokio::test]
    async fn tts_announcement_rejects_empty_message_and_empty_targets() {
        let state = Arc::new(ServerState::default());

        let empty_message = apply_tts_announcement_event(
            &state,
            AdminTtsRequest {
                sender: SERVER_USER_ID,
                targets: vec![AlertTarget::User(2)],
                target: None,
                message: "  ".to_string(),
                priority: true,
                duck: false,
                gain: 0.18,
            },
        )
        .await;
        assert!(matches!(empty_message, Err(AdminApiError::BadRequest(_))));

        let empty_targets = apply_tts_announcement_event(
            &state,
            AdminTtsRequest {
                sender: SERVER_USER_ID,
                targets: Vec::new(),
                target: None,
                message: "Hello".to_string(),
                priority: true,
                duck: false,
                gain: 0.18,
            },
        )
        .await;
        assert!(matches!(empty_targets, Err(AdminApiError::BadRequest(_))));

        let nothing_enabled = apply_announcement_event(
            &state,
            AdminAnnouncementRequest {
                sender: SERVER_USER_ID,
                targets: vec![AlertTarget::User(2)],
                target: None,
                message: "Hello".to_string(),
                text_alert: false,
                tts: false,
                priority: true,
                duck: false,
                gain: 0.18,
            },
        )
        .await;
        assert!(matches!(nothing_enabled, Err(AdminApiError::BadRequest(_))));
    }

    #[tokio::test]
    async fn announcement_can_send_text_alert_without_audio() {
        let state = Arc::new(ServerState::default());
        let mut listener = Session::new();
        listener.addr = Some("127.0.0.1:10002".parse().unwrap());
        state.sessions.write().await.insert(2, listener);

        let response = apply_announcement_event(
            &state,
            AdminAnnouncementRequest {
                sender: SERVER_USER_ID,
                targets: vec![AlertTarget::User(2)],
                target: None,
                message: "Text only".to_string(),
                text_alert: true,
                tts: false,
                priority: true,
                duck: false,
                gain: 0.18,
            },
        )
        .await
        .unwrap();

        assert_eq!(response.alert_ids.len(), 1);
        assert_eq!(response.engine, None);
        assert_eq!(response.duration_ms, None);
        assert!(state.sources.read().await.is_empty());
    }

    #[tokio::test]
    async fn button_actions_transmit_alert_and_update_server_owned_config() {
        let state = ServerState::default();
        let desired_1 = DesiredClientConfig {
            user_id: 1,
            client_uid: None,
            role: ClientRole::Client,
            name: "Ref".to_string(),
            listen: vec![1],
            tx: Vec::new(),
            vol: HashMap::new(),
            talker_vol: HashMap::new(),
            codec: Codec::Pcm16,
            opus_profile: OpusProfile::default(),
            talk_mode: TalkMode::Muted,
            priority: false,
            priority_channels: Vec::new(),
            buttons: vec![TalkButtonConfig {
                id: "director".to_string(),
                label: "Director".to_string(),
                color: None,
                mode: TalkButtonMode::Momentary,
                actions: vec![
                    TalkButtonAction::Transmit {
                        channels: vec![5],
                        users: vec![2],
                        duck: true,
                    },
                    TalkButtonAction::Alert {
                        targets: vec![AlertTarget::User(2)],
                        message: Some("Director calling".to_string()),
                    },
                    TalkButtonAction::SetTalkMode {
                        users: vec![2],
                        mode: TalkMode::Muted,
                    },
                    TalkButtonAction::RouteEdit {
                        users: vec![2],
                        listen_add: vec![9],
                        listen_remove: Vec::new(),
                        listen_toggle: Vec::new(),
                        tx_add: vec![10],
                        tx_remove: Vec::new(),
                        tx_toggle: Vec::new(),
                    },
                ],
            }],
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: Esp32AudioConfig::default(),
            processing: ProcessingConfig::default(),
        };
        let desired_2 = DesiredClientConfig {
            user_id: 2,
            client_uid: None,
            role: ClientRole::Client,
            name: "Director".to_string(),
            listen: Vec::new(),
            tx: Vec::new(),
            vol: HashMap::new(),
            talker_vol: HashMap::new(),
            codec: Codec::Pcm16,
            opus_profile: OpusProfile::default(),
            talk_mode: TalkMode::Open,
            priority: false,
            priority_channels: Vec::new(),
            buttons: Vec::new(),
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: Esp32AudioConfig::default(),
            processing: ProcessingConfig::default(),
        };
        assert!(matches!(
            apply_desired_client(&state, desired_1).await,
            ControlResponse::Ack
        ));
        assert!(matches!(
            apply_desired_client(&state, desired_2).await,
            ControlResponse::Ack
        ));
        state.sessions.write().await.get_mut(&2).unwrap().addr =
            Some("127.0.0.1:10002".parse().unwrap());

        assert!(matches!(
            apply_button_event(&state, 1, "director".to_string(), true).await,
            ControlResponse::Ack
        ));

        {
            let sessions = state.sessions.read().await;
            let targets = sessions.get(&1).unwrap().active_tx_targets();
            assert!(targets.contains(&AudioTarget::Channel(5)));
            assert!(targets.contains(&AudioTarget::Direct(2)));
            assert_eq!(sessions.get(&2).unwrap().last_direct_caller, Some(1));
            let active_calls = active_direct_call_statuses_for_user(2, &sessions);
            assert_eq!(active_calls[0].caller_name.as_deref(), Some("Ref"));
            assert_eq!(active_calls[0].target_name.as_deref(), Some("Director"));
        }
        {
            let desired = desired_client(&state, 2).await.unwrap();
            assert_eq!(desired.talk_mode, TalkMode::Muted);
            assert_eq!(desired.listen, vec![9]);
            assert_eq!(desired.tx, vec![10]);
        }
        {
            let alerts = state.alerts.read().await;
            let sessions = state.sessions.read().await;
            let (active, _) = alert_statuses_for_user_with_sessions(&alerts, 2, &sessions);
            assert_eq!(active.len(), 1);
            assert_eq!(active[0].sender_name.as_deref(), Some("Ref"));
            assert_eq!(active[0].message.as_deref(), Some("Director calling"));
        }

        assert!(matches!(
            apply_button_event(&state, 1, "director".to_string(), false).await,
            ControlResponse::Ack
        ));
        let sessions = state.sessions.read().await;
        assert!(!sessions
            .get(&1)
            .unwrap()
            .active_tx_targets()
            .contains(&AudioTarget::Direct(2)));
        assert!(sessions
            .get(&2)
            .unwrap()
            .direct_call_history
            .iter()
            .any(
                |entry| entry.source_button.as_deref() == Some("director") && entry.ended.is_some()
            ));
    }

    #[tokio::test]
    async fn lockout_rejects_client_control_but_admin_can_update() {
        let state = ServerState::default();
        let desired = DesiredClientConfig {
            user_id: 22,
            client_uid: None,
            role: ClientRole::Client,
            name: "Locked".to_string(),
            listen: vec![1],
            tx: vec![1],
            vol: HashMap::new(),
            talker_vol: HashMap::new(),
            codec: Codec::Pcm16,
            opus_profile: OpusProfile::default(),
            talk_mode: TalkMode::Ptt,
            priority: false,
            priority_channels: Vec::new(),
            buttons: Vec::new(),
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy {
                allow_talk_mode: false,
                ..ClientLockoutPolicy::default()
            },
            stereo: StereoConfig::default(),
            esp32_audio: Esp32AudioConfig::default(),
            processing: ProcessingConfig::default(),
        };
        assert!(matches!(
            apply_desired_client(&state, desired).await,
            ControlResponse::Ack
        ));

        let rejected = apply_control(
            &state,
            ControlMessage::TalkMode {
                user_id: 22,
                mode: TalkMode::Muted,
            },
        )
        .await;
        assert!(matches!(
            rejected,
            ControlResponse::Error { message } if message.contains("talk mode")
        ));

        let mut admin_desired = desired_client(&state, 22).await.unwrap();
        admin_desired.talk_mode = TalkMode::Muted;
        assert!(matches!(
            apply_desired_client(&state, admin_desired).await,
            ControlResponse::Ack
        ));
        assert_eq!(
            desired_client(&state, 22).await.unwrap().talk_mode,
            TalkMode::Muted
        );
    }

    #[tokio::test]
    async fn admin_can_save_unsupported_live_codec_as_desired_with_session_fallback() {
        let state = ServerState::default();
        {
            let mut sessions = state.sessions.write().await;
            let mut session = Session::new();
            session.supported_codecs = [Codec::Pcm16].into();
            session.output_codec = Codec::Pcm16;
            session.last_seen = Instant::now();
            sessions.insert(55, session);
        }

        let desired = DesiredClientConfig {
            user_id: 55,
            client_uid: None,
            role: ClientRole::Client,
            name: "ESP32".to_string(),
            listen: vec![1],
            tx: vec![1],
            vol: HashMap::new(),
            talker_vol: HashMap::new(),
            codec: Codec::Opus,
            opus_profile: OpusProfile::Speech24Standard,
            talk_mode: TalkMode::Open,
            priority: false,
            priority_channels: Vec::new(),
            buttons: Vec::new(),
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: Esp32AudioConfig::default(),
            processing: ProcessingConfig::default(),
        };

        assert!(matches!(
            apply_desired_client(&state, desired).await,
            ControlResponse::Ack
        ));
        assert_eq!(desired_client(&state, 55).await.unwrap().codec, Codec::Opus);

        let sessions = state.sessions.read().await;
        let session = sessions.get(&55).unwrap();
        assert_eq!(session.output_codec, Codec::Pcm16);
        assert_eq!(session.talk_mode, TalkMode::Open);
        assert_eq!(session.tx_channels, [1].into());
    }

    #[tokio::test]
    async fn applying_preset_updates_desired_and_live_sessions() {
        let state = ServerState::default();
        let preset = PresetConfig {
            id: "refs".to_string(),
            name: "Refs".to_string(),
            clients: vec![DesiredClientConfig {
                user_id: 10,
                client_uid: None,
                role: ClientRole::Client,
                name: "Ref 1".to_string(),
                listen: vec![1, 2],
                tx: vec![1],
                vol: [(2, 0.5)].into(),
                talker_vol: [(12, 0.8)].into(),
                codec: Codec::Pcm48,
                opus_profile: OpusProfile::default(),
                talk_mode: TalkMode::Open,
                priority: false,
                priority_channels: Vec::new(),
                buttons: Vec::new(),
                ifb: IfbConfig::default(),
                lockout: ClientLockoutPolicy::default(),
                stereo: StereoConfig::default(),
                esp32_audio: Esp32AudioConfig::default(),
                processing: ProcessingConfig::default(),
            }],
        };

        apply_preset(&state, &preset).await.unwrap();

        let desired = desired_client(&state, 10).await.unwrap();
        assert_eq!(desired.name, "Ref 1");
        assert_eq!(desired.talker_vol.get(&12), Some(&0.8));

        let sessions = state.sessions.read().await;
        let session = sessions.get(&10).unwrap();
        assert_eq!(session.listen_channels, [1, 2].into());
        assert_eq!(session.talker_volumes.get(&12), Some(&0.8));
    }

    #[tokio::test]
    async fn applying_template_updates_target_desired_and_live_session() {
        let state = ServerState::default();
        {
            let mut admin_state = state.admin_state.write().await;
            admin_state.templates.push(ClientTemplateConfig {
                id: "referee".to_string(),
                name: "Referee".to_string(),
                client: ClientTemplateClientConfig {
                    role: ClientRole::Client,
                    name: "Ref".to_string(),
                    listen: vec![1, 2],
                    tx: vec![1],
                    vol: [(2, 0.6)].into(),
                    talker_vol: [(11, 0.4), (10, 0.2)].into(),
                    codec: Codec::Pcm48,
                    opus_profile: OpusProfile::default(),
                    talk_mode: TalkMode::Open,
                    priority: true,
                    priority_channels: vec![1],
                    buttons: vec![TalkButtonConfig {
                        id: "director".to_string(),
                        label: "Director".to_string(),
                        color: None,
                        mode: TalkButtonMode::Momentary,
                        actions: vec![TalkButtonAction::Transmit {
                            channels: vec![9],
                            users: Vec::new(),
                            duck: false,
                        }],
                    }],
                    ifb: IfbConfig {
                        enabled: true,
                        program: vec![2],
                        interrupt: vec![9],
                        duck_gain: 0.125,
                    },
                    lockout: ClientLockoutPolicy::default(),
                    stereo: StereoConfig::default(),
                    esp32_audio: Esp32AudioConfig::default(),
                    processing: ProcessingConfig::default(),
                },
            });
        }

        let desired = template_desired_for_user(&state, "referee", 10)
            .await
            .unwrap();
        assert_eq!(desired.user_id, 10);
        assert_eq!(desired.name, "Ref");
        assert_eq!(desired.talker_vol.get(&11), Some(&0.4));
        assert!(!desired.talker_vol.contains_key(&10));

        assert!(matches!(
            apply_desired_client(&state, desired).await,
            ControlResponse::Ack
        ));

        let desired = desired_client(&state, 10).await.unwrap();
        assert_eq!(desired.codec, Codec::Pcm48);
        assert_eq!(desired.buttons[0].id, "director");

        let sessions = state.sessions.read().await;
        let session = sessions.get(&10).unwrap();
        assert_eq!(session.listen_channels, [1, 2].into());
        assert_eq!(session.ifb.interrupt, vec![9]);
    }

    #[test]
    fn admin_warnings_report_unadvertised_buttons() {
        let admin_state = PersistedAdminState {
            channels: Vec::new(),
            devices: Vec::new(),
            clients: vec![DesiredClientConfig {
                user_id: 1,
                client_uid: None,
                role: ClientRole::Client,
                name: String::new(),
                listen: vec![5],
                tx: Vec::new(),
                vol: HashMap::new(),
                talker_vol: HashMap::new(),
                codec: Codec::Pcm16,
                opus_profile: OpusProfile::default(),
                talk_mode: TalkMode::Open,
                priority: false,
                priority_channels: Vec::new(),
                buttons: vec![TalkButtonConfig {
                    id: "director".to_string(),
                    label: "Director".to_string(),
                    color: None,
                    mode: TalkButtonMode::Momentary,
                    actions: vec![TalkButtonAction::Transmit {
                        channels: vec![2],
                        users: Vec::new(),
                        duck: false,
                    }],
                }],
                ifb: IfbConfig {
                    enabled: true,
                    program: vec![2],
                    interrupt: vec![5],
                    duck_gain: 0.125,
                },
                lockout: ClientLockoutPolicy::default(),
                stereo: StereoConfig::default(),
                esp32_audio: Esp32AudioConfig::default(),
                processing: ProcessingConfig::default(),
            }],
            presets: Vec::new(),
            templates: Vec::new(),
        };
        let mut status = SessionStatus {
            user_id: 1,
            client_uid: "test-client".to_string(),
            enrollment: EnrollmentStatus::Enrolled,
            role: ClientRole::Client,
            addr: Some("127.0.0.1:1".to_string()),
            listen: Vec::new(),
            tx: Vec::new(),
            talker_vol: HashMap::new(),
            codec: Codec::Pcm16,
            opus_profile: OpusProfile::default(),
            supported_codecs: vec![Codec::Pcm16],
            advertised_buttons: Vec::new(),
            buttons: Vec::new(),
            active_buttons: Vec::new(),
            active_direct_calls: Vec::new(),
            last_direct_caller: None,
            direct_call_history: Vec::new(),
            active_alerts: Vec::new(),
            recent_alerts: Vec::new(),
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            ifb_status: IfbStatus::default(),
            stereo: StereoConfig::default(),
            esp32_audio: Esp32AudioConfig::default(),
            stereo_status: StereoStatus::default(),
            talk_mode: TalkMode::Open,
            regular_talk_active: false,
            priority: false,
            priority_channels: Vec::new(),
            processing: ProcessingConfig::default(),
            processing_status: ProcessingStatus::default(),
            emergency: None,
            queue_depth: 0,
            age_ms: 0,
            input: InputMeterStatus::default(),
            output: OutputMeterStatus::default(),
            capture: None,
            bridge: None,
            transport: TransportHealthStatus::default(),
            recording_enabled: false,
            transcription_enabled: false,
        };

        let warnings = admin_warnings(&admin_state, &[status.clone()]);
        assert!(warnings
            .iter()
            .any(|warning| warning.message.contains("not advertised")));

        status.advertised_buttons = vec![ButtonCapability {
            id: "director".to_string(),
            label: "Director".to_string(),
        }];
        assert!(admin_warnings(&admin_state, &[status]).is_empty());
    }

    #[test]
    fn default_admin_state_seeds_ifb_workflow_defaults_without_emergency_channel() {
        let state = PersistedAdminState::default();

        let channels = state
            .channels
            .iter()
            .map(|channel| (channel.id, channel.name.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            channels,
            vec![
                (0, "open"),
                (1, "Program"),
                (2, "Production PL"),
                (3, "Referee PL"),
                (4, "Director IFB"),
                (5, "Producer Cue"),
                (6, "PA"),
                (7, "Utility"),
            ]
        );
        assert!(!state
            .channels
            .iter()
            .any(|channel| channel.name == "Emergency"));

        let template_ids = state
            .templates
            .iter()
            .map(|template| template.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            template_ids,
            vec![
                "director-show-control",
                "pa-bridge-output",
                "producer-cue",
                "program-bridge-input",
                "referee-field",
                "talent-ifb-listen-only",
            ]
        );

        let preset = state
            .presets
            .iter()
            .find(|preset| preset.id == "small-show-ifb")
            .unwrap();
        assert_eq!(preset.clients.len(), 7);
        let talent = preset
            .clients
            .iter()
            .find(|client| client.user_id == USER_TALENT)
            .unwrap();
        assert!(talent.ifb.enabled);
        assert_eq!(talent.ifb.program, vec![CHANNEL_PROGRAM]);
        assert_eq!(
            talent.ifb.interrupt,
            vec![CHANNEL_DIRECTOR_IFB, CHANNEL_PRODUCER_CUE]
        );
        assert_eq!(talent.talk_mode, TalkMode::Muted);

        let pa_bridge = preset
            .clients
            .iter()
            .find(|client| client.user_id == USER_PA_BRIDGE)
            .unwrap();
        assert_eq!(pa_bridge.role, ClientRole::Bridge);
        assert_eq!(pa_bridge.listen, vec![CHANNEL_PA]);
        assert!(pa_bridge.tx.is_empty());
    }

    #[test]
    fn new_desired_client_defaults_to_open_channel() {
        let desired = DesiredClientConfig::new(42);

        assert_eq!(desired.listen, vec![CHANNEL_OPEN]);
        assert_eq!(desired.tx, vec![CHANNEL_OPEN]);
    }

    #[test]
    fn live_client_without_desired_config_defaults_to_open_channel() {
        let mut session = Session::new();
        session.role = ClientRole::Client;

        apply_default_operator_channels_to_session(&mut session);

        assert_eq!(session.listen_channels, [CHANNEL_OPEN].into());
        assert_eq!(session.tx_channels, [CHANNEL_OPEN].into());
    }

    #[test]
    fn normalize_admin_state_preserves_open_channel_zero() {
        let mut state = PersistedAdminState {
            channels: vec![channel_config(CHANNEL_PROGRAM, "Program")],
            devices: Vec::new(),
            clients: Vec::new(),
            presets: Vec::new(),
            templates: Vec::new(),
        };

        normalize_admin_state(&mut state);

        assert_eq!(
            state
                .channels
                .iter()
                .map(|channel| (channel.id, channel.name.as_str()))
                .collect::<Vec<_>>(),
            vec![(CHANNEL_OPEN, "open"), (CHANNEL_PROGRAM, "Program"),]
        );
    }

    #[test]
    fn workflow_validation_warnings_report_common_ifb_and_pa_mistakes() {
        let mut pa_regular_tx = DesiredClientConfig::new(1);
        pa_regular_tx.name = "Ref".to_string();
        pa_regular_tx.tx = vec![CHANNEL_PA];

        let mut bad_talent = DesiredClientConfig::new(2);
        bad_talent.name = "Talent".to_string();
        bad_talent.tx = Vec::new();
        bad_talent.talk_mode = TalkMode::Muted;
        bad_talent.ifb = IfbConfig {
            enabled: true,
            program: Vec::new(),
            interrupt: vec![CHANNEL_DIRECTOR_IFB],
            duck_gain: DEFAULT_IFB_DUCK_GAIN,
        };

        let mut bad_program_bridge = DesiredClientConfig::new(3);
        bad_program_bridge.role = ClientRole::Bridge;
        bad_program_bridge.name = "Program Bridge".to_string();
        bad_program_bridge.talk_mode = TalkMode::Ptt;

        let mut bad_pa_bridge = DesiredClientConfig::new(4);
        bad_pa_bridge.role = ClientRole::Bridge;
        bad_pa_bridge.name = "PA Bridge".to_string();
        bad_pa_bridge.listen = vec![CHANNEL_PA];
        bad_pa_bridge.tx = vec![CHANNEL_PA];
        bad_pa_bridge.talk_mode = TalkMode::Open;

        let admin_state = PersistedAdminState {
            channels: default_workflow_channels(),
            devices: Vec::new(),
            clients: vec![pa_regular_tx, bad_talent, bad_program_bridge, bad_pa_bridge],
            presets: Vec::new(),
            templates: Vec::new(),
        };

        let messages = workflow_validation_warnings(&admin_state)
            .into_iter()
            .map(|warning| warning.message)
            .collect::<Vec<_>>();
        assert!(messages
            .iter()
            .any(|message| message.contains("PA is configured as a regular TX")));
        assert!(messages
            .iter()
            .any(|message| message.contains("IFB is enabled but has no program")));
        assert!(messages
            .iter()
            .any(|message| message.contains("IFB interrupt channel")));
        assert!(messages
            .iter()
            .any(|message| message.contains("program bridge should transmit")));
        assert!(messages
            .iter()
            .any(|message| message.contains("program bridge should use open")));
        assert!(messages
            .iter()
            .any(|message| message.contains("PA bridge should not have TX")));
        assert!(messages
            .iter()
            .any(|message| message.contains("PA bridge should stay muted")));
        assert!(messages
            .iter()
            .any(|message| message.contains("bridge desired config both listens and transmits")));
        assert!(messages
            .iter()
            .any(|message| message.contains("listen-only IFB client")));
    }

    #[tokio::test]
    async fn admin_state_loads_default_when_missing() {
        let path = temp_state_path("missing");
        let state = load_admin_state(&path).await.unwrap();
        assert_eq!(state, PersistedAdminState::default());
    }

    #[tokio::test]
    async fn admin_state_saves_and_loads_json() {
        let path = temp_state_path("roundtrip");
        let state = PersistedAdminState {
            channels: vec![
                ChannelConfig {
                    id: 0,
                    name: "open".to_string(),
                },
                ChannelConfig {
                    id: 2,
                    name: "Program".to_string(),
                },
            ],
            devices: Vec::new(),
            clients: vec![DesiredClientConfig {
                user_id: 7,
                client_uid: None,
                role: ClientRole::Client,
                name: "Beltpack".to_string(),
                listen: vec![1, 2],
                tx: vec![1],
                vol: [(2, 0.6)].into(),
                talker_vol: [(3, 0.75)].into(),
                codec: Codec::Opus,
                opus_profile: OpusProfile::Speech48High,
                talk_mode: TalkMode::Muted,
                priority: true,
                priority_channels: vec![1],
                buttons: vec![TalkButtonConfig {
                    id: "director".to_string(),
                    label: "Director".to_string(),
                    color: None,
                    mode: TalkButtonMode::Momentary,
                    actions: vec![TalkButtonAction::Transmit {
                        channels: vec![5],
                        users: Vec::new(),
                        duck: false,
                    }],
                }],
                ifb: IfbConfig {
                    enabled: true,
                    program: vec![2],
                    interrupt: vec![5],
                    duck_gain: 0.125,
                },
                lockout: ClientLockoutPolicy {
                    allow_codec: false,
                    ..ClientLockoutPolicy::default()
                },
                stereo: StereoConfig::default(),
                esp32_audio: Esp32AudioConfig::default(),
                processing: ProcessingConfig::default(),
            }],
            presets: vec![PresetConfig {
                id: "refs".to_string(),
                name: "Refs".to_string(),
                clients: vec![DesiredClientConfig {
                    user_id: 8,
                    client_uid: None,
                    role: ClientRole::Client,
                    name: "Ref 1".to_string(),
                    listen: vec![1],
                    tx: vec![1],
                    vol: HashMap::new(),
                    talker_vol: HashMap::new(),
                    codec: Codec::Pcm48,
                    opus_profile: OpusProfile::default(),
                    talk_mode: TalkMode::Open,
                    priority: false,
                    priority_channels: Vec::new(),
                    buttons: Vec::new(),
                    ifb: IfbConfig::default(),
                    lockout: ClientLockoutPolicy::default(),
                    stereo: StereoConfig::default(),
                    esp32_audio: Esp32AudioConfig::default(),
                    processing: ProcessingConfig::default(),
                }],
            }],
            templates: vec![ClientTemplateConfig {
                id: "beltpack".to_string(),
                name: "Beltpack".to_string(),
                client: ClientTemplateClientConfig {
                    role: ClientRole::Client,
                    name: "Template Beltpack".to_string(),
                    listen: vec![2],
                    tx: vec![1],
                    vol: [(2, 0.7)].into(),
                    talker_vol: [(3, 0.5)].into(),
                    codec: Codec::Pcm48,
                    opus_profile: OpusProfile::default(),
                    talk_mode: TalkMode::Ptt,
                    priority: false,
                    priority_channels: Vec::new(),
                    buttons: Vec::new(),
                    ifb: IfbConfig::default(),
                    lockout: ClientLockoutPolicy::default(),
                    stereo: StereoConfig::default(),
                    esp32_audio: Esp32AudioConfig::default(),
                    processing: ProcessingConfig::default(),
                },
            }],
        };

        save_admin_state_to_path(&path, &state).await.unwrap();
        let loaded = load_admin_state(&path).await.unwrap();

        assert_eq!(loaded, state);
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn auto_enrollment_assigns_next_alias_on_conflict() {
        let state = ServerState::new_with_admin_state(
            PersistedAdminState {
                devices: vec![DeviceEnrollment {
                    client_uid: "existing-device".to_string(),
                    user_id: 7,
                    status: EnrollmentStatus::Enrolled,
                    name: String::new(),
                    role: ClientRole::Client,
                    first_seen_ms: 1,
                    last_seen_ms: 1,
                    hardware_fingerprint: None,
                    warnings: Vec::new(),
                }],
                ..PersistedAdminState::default()
            },
            None,
            EnrollmentPolicy::Auto,
        );

        let enrollment = resolve_client_enrollment(
            &state,
            "new-device".to_string(),
            Some(7),
            ClientRole::Client,
        )
        .await
        .unwrap();

        assert_eq!(enrollment.status, EnrollmentStatus::Enrolled);
        assert_ne!(enrollment.user_id, 7);
        let admin = state.admin_state.read().await;
        let device = admin
            .devices
            .iter()
            .find(|device| device.client_uid == "new-device")
            .unwrap();
        assert_eq!(device.user_id, enrollment.user_id);
        assert!(device.warnings[0].contains("requested user_id 7"));
    }

    #[tokio::test]
    async fn approval_enrollment_creates_pending_device() {
        let state = ServerState::new_with_admin_state(
            PersistedAdminState::default(),
            None,
            EnrollmentPolicy::Approval,
        );

        let enrollment = resolve_client_enrollment(
            &state,
            "pending-device".to_string(),
            Some(12),
            ClientRole::Client,
        )
        .await
        .unwrap();

        assert_eq!(enrollment.status, EnrollmentStatus::Pending);
        assert_eq!(enrollment.user_id, 12);
        assert_eq!(
            state.admin_state.read().await.devices[0].status,
            EnrollmentStatus::Pending
        );
        assert!(!audio_user_is_enrolled(&state, 12).await);
    }

    #[tokio::test]
    async fn preconfigured_only_rejects_unknown_device() {
        let state = ServerState::new_with_admin_state(
            PersistedAdminState::default(),
            None,
            EnrollmentPolicy::PreconfiguredOnly,
        );

        let err = resolve_client_enrollment(
            &state,
            "unknown-device".to_string(),
            Some(20),
            ClientRole::Client,
        )
        .await
        .unwrap_err();

        assert!(err.contains("preconfigured-only"));
        assert!(state.admin_state.read().await.devices.is_empty());
        assert!(!audio_user_is_enrolled(&state, 20).await);
    }

    #[tokio::test]
    async fn audio_user_zero_is_never_enrolled_by_udp_path() {
        let state = ServerState::new_with_admin_state(
            PersistedAdminState::default(),
            None,
            EnrollmentPolicy::Auto,
        );

        assert!(!audio_user_is_enrolled(&state, 0).await);
    }

    #[tokio::test]
    async fn enrolled_control_session_allows_audio_alias() {
        let state = ServerState::new_with_admin_state(
            PersistedAdminState::default(),
            None,
            EnrollmentPolicy::Approval,
        );
        let response = apply_control(
            &state,
            ControlMessage::Hello {
                user_id: 12,
                requested_user_id: Some(12),
                client_uid: "approved-device".to_string(),
                codecs: vec![Codec::Pcm16],
                buttons: Vec::new(),
                role: ClientRole::Client,
            },
        )
        .await;

        assert!(matches!(
            response,
            ControlResponse::Hello {
                enrollment: EnrollmentStatus::Pending,
                ..
            }
        ));

        {
            let mut admin = state.admin_state.write().await;
            admin.devices[0].status = EnrollmentStatus::Enrolled;
        }

        let response = apply_control(
            &state,
            ControlMessage::Hello {
                user_id: 12,
                requested_user_id: Some(12),
                client_uid: "approved-device".to_string(),
                codecs: vec![Codec::Pcm16],
                buttons: Vec::new(),
                role: ClientRole::Client,
            },
        )
        .await;

        assert!(matches!(
            response,
            ControlResponse::Hello {
                enrollment: EnrollmentStatus::Enrolled,
                user_id: 12,
                ..
            }
        ));
        assert!(audio_user_is_enrolled(&state, 12).await);
    }

    #[tokio::test]
    async fn recording_session_writes_filtered_ingest_and_transcripts() {
        let state = ServerState::default();
        let dir = std::env::temp_dir().join(format!("intercom-recordings-test-{}", unix_time_ms()));
        state.recording.write().await.base_dir = dir.clone();

        start_recording_session(
            &state,
            StartRecordingRequest {
                transcribe: false,
                users: Some(vec![1]),
            },
        )
        .await
        .unwrap();
        record_ingest_frame(
            &state,
            1,
            AudioTarget::Channel(7),
            Some(Codec::Pcm48),
            &vec![100; MIX_SAMPLES_PER_FRAME],
        )
        .await;
        record_ingest_frame(
            &state,
            2,
            AudioTarget::Channel(7),
            Some(Codec::Pcm48),
            &vec![100; MIX_SAMPLES_PER_FRAME],
        )
        .await;

        let status = recording_status_snapshot(&state).await;
        assert!(status.active);
        assert_eq!(status.recorded_users, vec![1]);
        assert_eq!(status.frames_recorded, 1);

        assert!(stop_recording_session(&state).await.unwrap().is_none());
        let status = recording_status_snapshot(&state).await;
        assert!(!status.active);
        assert_eq!(status.recent_sessions.len(), 1);
        assert_eq!(recording_sessions_snapshot(&state).await.len(), 1);
        let session_dir = PathBuf::from(&status.recent_sessions[0].dir);
        let wav_path = session_dir.join("user-1.wav");
        let metadata_path = session_dir.join("metadata.jsonl");
        assert!(wav_path.exists());
        let metadata = tokio::fs::read_to_string(&metadata_path).await.unwrap();
        let metadata_event: serde_json::Value =
            serde_json::from_str(metadata.lines().next().unwrap()).unwrap();
        assert_eq!(metadata_event["kind"], "ingest_frame");
        assert_eq!(metadata_event["user_id"], 1);
        assert_eq!(metadata_event["target"]["kind"], "channel");
        assert_eq!(metadata_event["target"]["id"], 7);

        append_transcript_segment(
            &state,
            TranscriptAppend {
                user_id: 1,
                contexts: vec![AudioTarget::Channel(7)],
                started_at_ms: None,
                ended_at_ms: None,
                text: "Check one two".to_string(),
                confidence: Some(0.9),
                engine: "fake".to_string(),
                source: TranscriptSource::Manual,
                final_segment: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            query_transcripts(
                &state,
                TranscriptQuery {
                    user_id: Some(1),
                    channel_id: Some(7),
                    ..Default::default()
                },
            )
            .await
            .len(),
            1
        );
        append_transcript_segment(
            &state,
            TranscriptAppend {
                user_id: 1,
                contexts: vec![AudioTarget::Direct(3)],
                started_at_ms: Some(100),
                ended_at_ms: Some(150),
                text: "Direct reply".to_string(),
                confidence: None,
                engine: "fake".to_string(),
                source: TranscriptSource::Live,
                final_segment: true,
            },
        )
        .await
        .unwrap();
        assert!(query_transcripts(
            &state,
            TranscriptQuery {
                user_id: Some(2),
                channel_id: Some(7),
                ..Default::default()
            },
        )
        .await
        .is_empty());
        assert_eq!(
            query_transcripts(
                &state,
                TranscriptQuery {
                    user_ids: Some("1,3".to_string()),
                    direct_user_id: Some(3),
                    source: Some(TranscriptSource::Live),
                    q: Some("reply".to_string()),
                    since_ms: Some(90),
                    until_ms: Some(200),
                    ..Default::default()
                },
            )
            .await
            .len(),
            1
        );

        let _ = tokio::fs::remove_dir_all(dir).await;
    }

    #[test]
    fn live_transcription_chunker_ignores_silence_and_finalizes_after_speech() {
        let mut chunker = LiveTranscriptChunker::default();
        let silence = vec![0; MIX_SAMPLES_PER_FRAME];
        for _ in 0..100 {
            assert!(chunker
                .push_frame(1, AudioTarget::Channel(7), &silence)
                .is_none());
        }
        assert!(chunker.buffer.is_empty());

        let speech = vec![12_000; MIX_SAMPLES_PER_FRAME];
        for _ in 0..LIVE_TRANSCRIPTION_MIN_SPEECH_FRAMES {
            assert!(chunker
                .push_frame(1, AudioTarget::Channel(7), &speech)
                .is_none());
        }
        let mut job = None;
        for _ in 0..LIVE_TRANSCRIPTION_SILENCE_FRAMES {
            job = chunker.push_frame(1, AudioTarget::Channel(7), &silence);
            if job.is_some() {
                break;
            }
        }
        let job = job.expect("speech followed by silence should finalize a transcript chunk");
        assert_eq!(job.user_id, 1);
        assert!(job.contexts.contains(&AudioTarget::Channel(7)));
        assert!(
            job.samples_16khz.len()
                >= LIVE_TRANSCRIPTION_MIN_SPEECH_FRAMES * common::SAMPLES_PER_FRAME
        );
    }

    #[test]
    fn live_transcription_chunker_finalizes_long_speech_at_max_length() {
        let mut chunker = LiveTranscriptChunker::default();
        let speech = vec![12_000; MIX_SAMPLES_PER_FRAME];
        let mut job = None;
        for _ in 0..LIVE_TRANSCRIPTION_MAX_FRAMES {
            job = chunker.push_frame(2, AudioTarget::Direct(3), &speech);
            if job.is_some() {
                break;
            }
        }
        let job = job.expect("long speech should finalize at max chunk length");
        assert_eq!(job.user_id, 2);
        assert!(job.contexts.contains(&AudioTarget::Direct(3)));
        assert!(
            !chunker.buffer.is_empty(),
            "long chunks keep overlap context"
        );
    }

    #[cfg(feature = "transcription-whisper")]
    #[test]
    fn fake_live_transcription_engine_returns_completed_segment() {
        struct FakeEngine;
        impl LiveTranscriptionEngine for FakeEngine {
            fn transcribe(&mut self, _samples_16khz: &[i16]) -> anyhow::Result<String> {
                Ok(" check one ".to_string())
            }
        }

        let job = LiveTranscriptJob {
            user_id: 4,
            started_at_ms: 10,
            ended_at_ms: 20,
            contexts: vec![AudioTarget::Channel(9)],
            samples_16khz: vec![1; common::SAMPLES_PER_FRAME],
        };
        let completed = transcribe_live_job_with_engine(&mut FakeEngine, job)
            .unwrap()
            .unwrap();

        assert_eq!(completed.user_id, 4);
        assert_eq!(completed.text, "check one");
        assert_eq!(completed.contexts, vec![AudioTarget::Channel(9)]);
    }

    #[test]
    fn discovery_advertisement_txt_records_include_mobile_connection_metadata() {
        let advertisement = DiscoveryAdvertisement {
            name: "Truck A\nMain".to_string(),
            control_port: 40001,
            audio_port: 40000,
            admin_port: Some(40002),
            auth_required: true,
            version: "0.1.0".to_string(),
        };

        let records = advertisement.txt_records();

        assert!(records.contains(&"audio_port=40000".to_string()));
        assert!(records.contains(&"admin_port=40002".to_string()));
        assert!(records.contains(&"auth=required".to_string()));
        assert!(records.contains(&"version=0.1.0".to_string()));
        assert!(records.contains(&"name=Truck AMain".to_string()));
    }

    #[test]
    fn discovery_command_advertises_control_service() {
        let advertisement = DiscoveryAdvertisement {
            name: "RedLine".to_string(),
            control_port: 41001,
            audio_port: 41000,
            admin_port: None,
            auth_required: false,
            version: "0.1.0".to_string(),
        };

        let command = discovery_command(&advertisement);
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        assert_eq!(command.get_program().to_string_lossy(), "dns-sd");
        assert_eq!(args[0], "-R");
        assert_eq!(args[1], "RedLine");
        assert_eq!(args[2], DISCOVERY_SERVICE_TYPE);
        assert_eq!(args[3], "local.");
        assert_eq!(args[4], "41001");
        assert!(args.contains(&"audio_port=41000".to_string()));
        assert!(args.contains(&"auth=none".to_string()));
    }

    #[test]
    fn discovery_admin_port_omits_loopback_admin() {
        assert_eq!(
            discovery_admin_port(Some(SocketAddr::from(([127, 0, 0, 1], 40002)))),
            None
        );
        assert_eq!(
            discovery_admin_port(Some(SocketAddr::from(([0, 0, 0, 0], 40002)))),
            Some(40002)
        );
    }

    #[tokio::test]
    async fn live_transcription_queue_is_bounded_and_reports_drops() {
        let state = Arc::new(ServerState::default());
        {
            let mut transcription = state.transcription.write().await;
            transcription.active = true;
            transcription.per_user.insert(
                1,
                LiveTranscriptionUserRuntime {
                    worker_running: true,
                    ..Default::default()
                },
            );
        }
        let speech = vec![12_000; MIX_SAMPLES_PER_FRAME];
        let silence = vec![0; MIX_SAMPLES_PER_FRAME];

        for _ in 0..(LIVE_TRANSCRIPTION_QUEUE_LIMIT + 1) {
            for _ in 0..LIVE_TRANSCRIPTION_MIN_SPEECH_FRAMES {
                transcribe_ingest_frame(&state, 1, AudioTarget::Channel(2), &speech).await;
            }
            for _ in 0..LIVE_TRANSCRIPTION_SILENCE_FRAMES {
                transcribe_ingest_frame(&state, 1, AudioTarget::Channel(2), &silence).await;
            }
        }

        let status = live_transcription_status_snapshot(&state).await;
        let user = status.users.iter().find(|user| user.user_id == 1).unwrap();
        assert_eq!(user.queued_jobs, LIVE_TRANSCRIPTION_QUEUE_LIMIT);
        assert!(user.dropped_jobs > 0);
        assert!(user.dropped_frames > 0);
    }

    #[tokio::test]
    async fn admin_state_rejects_malformed_json() {
        let path = temp_state_path("malformed");
        tokio::fs::write(&path, "{not json").await.unwrap();

        let err = load_admin_state(&path).await.unwrap_err();

        assert!(err.to_string().contains("parse admin state file"));
        let _ = tokio::fs::remove_file(path).await;
    }

    fn temp_state_path(name: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "intercom-server-{name}-{}-{now}.json",
            std::process::id()
        ))
    }

    fn sorted_channels(channels: &HashSet<ChannelId>) -> Vec<ChannelId> {
        let mut channels = channels.iter().copied().collect::<Vec<_>>();
        channels.sort_unstable();
        channels
    }

    async fn status_snapshot_with_session(
        state: &ServerState,
        user_id: UserId,
    ) -> (Vec<SessionStatus>, StatusMetrics) {
        state
            .sessions
            .write()
            .await
            .entry(user_id)
            .or_insert_with(Session::new);
        status_snapshot(state).await
    }
}
