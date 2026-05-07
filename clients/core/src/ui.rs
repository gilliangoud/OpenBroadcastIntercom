use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use common::{
    AlertId, AlertStatus, AlertTarget, BuildInfo, ButtonCapability, ButtonId,
    ChannelPresenceRoster, ClientLockoutPolicy, Codec, ControlMessage, DirectCallHistoryEntry,
    DirectCallStatus, EmergencyStatus, IfbConfig, OpusProfile, ProcessingConfig, StereoConfig,
    TalkButtonConfig, TalkMode,
};
use serde::{Deserialize, Serialize};

use crate::{supported_codecs, AudioSettings, ClientConfig, PlaybackStats};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientRuntimePhase {
    Stopped,
    Starting,
    Running,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientRuntimeStatus {
    pub phase: ClientRuntimePhase,
    pub running: bool,
    pub last_error: Option<String>,
}

impl ClientRuntimeStatus {
    pub fn stopped() -> Self {
        Self {
            phase: ClientRuntimePhase::Stopped,
            running: false,
            last_error: None,
        }
    }

    pub fn starting() -> Self {
        Self {
            phase: ClientRuntimePhase::Starting,
            running: false,
            last_error: None,
        }
    }

    pub fn running() -> Self {
        Self {
            phase: ClientRuntimePhase::Running,
            running: true,
            last_error: None,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            phase: ClientRuntimePhase::Failed,
            running: false,
            last_error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientAudioBackendKind {
    #[default]
    Auto,
    Raw,
    VoiceProcessing,
    IosAvAudioSession,
    IosVoiceProcessingIo,
    IosRemoteIo,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientInputChannelMode {
    #[default]
    Average,
    Left,
    Right,
}

pub trait ClientAudioBackend: Send + Sync {
    fn kind(&self) -> ClientAudioBackendKind;

    fn prepare(&self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClientRuntimeConfig {
    pub server_host: String,
    pub server: SocketAddr,
    pub control: String,
    pub user_id: Option<u16>,
    pub client_uid: Option<String>,
    pub identity_file: Option<PathBuf>,
    pub tx_channel: u16,
    pub listen_channel: u16,
    pub codec: Codec,
    pub opus_profile: OpusProfile,
    pub mic_gain: f32,
    pub input_limiter: bool,
    pub input_transient_suppression: bool,
    pub speaker_gain: f32,
    pub jitter_ms: u32,
    pub input_device: Option<String>,
    pub input_backend: ClientAudioBackendKind,
    pub input_channel: ClientInputChannelMode,
    pub output_device: Option<String>,
    pub debug_audio_dir: Option<PathBuf>,
    pub button_count: u16,
    pub buttons: Vec<String>,
    pub button_keys: Vec<String>,
    pub local_ui_bind: SocketAddr,
    pub local_ui_token: Option<String>,
    pub disable_local_ui: bool,
    pub list_devices: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InputBackendState {
    pub requested: ClientAudioBackendKind,
    pub active: Option<ClientAudioBackendKind>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MacosMicrophoneModeStatus {
    pub preferred: String,
    pub active: String,
    pub voice_isolation_active: bool,
    pub system_ui_available: bool,
    pub note: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct OkResponse {
    pub ok: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct StateResponse {
    pub build: BuildInfo,
    pub user_id: u16,
    pub client_uid: String,
    pub name: String,
    pub listen: Vec<u16>,
    pub tx: Vec<u16>,
    pub vol: HashMap<u16, f32>,
    pub talker_vol: HashMap<u16, f32>,
    pub codec: Codec,
    pub opus_profile: OpusProfile,
    pub talk_mode: TalkMode,
    pub regular_talk_active: bool,
    pub priority: bool,
    pub priority_channels: Vec<u16>,
    pub processing: ProcessingConfig,
    pub channel_rosters: Vec<ChannelPresenceRoster>,
    pub emergency: Option<EmergencyStatus>,
    pub supported_codecs: Vec<Codec>,
    pub buttons: Vec<TalkButtonConfig>,
    pub active_buttons: Vec<ButtonId>,
    pub active_direct_calls: Vec<DirectCallStatus>,
    pub last_direct_caller: Option<u16>,
    pub direct_call_history: Vec<DirectCallHistoryEntry>,
    pub active_alerts: Vec<AlertStatus>,
    pub recent_alerts: Vec<AlertStatus>,
    pub advertised_buttons: Vec<ButtonCapability>,
    pub ifb: IfbConfig,
    pub lockout: ClientLockoutPolicy,
    pub stereo: StereoConfig,
    pub mic_gain: f32,
    pub speaker_gain: f32,
    pub requested_input_backend: ClientAudioBackendKind,
    pub active_input_backend: Option<ClientAudioBackendKind>,
    pub input_backend_note: Option<String>,
    pub macos_microphone_mode: Option<MacosMicrophoneModeStatus>,
    pub playback: PlaybackStats,
}

impl StateResponse {
    pub fn from_runtime_state(
        config: &ClientConfig,
        audio_settings: &AudioSettings,
        input_backend: InputBackendState,
        macos_microphone_mode: Option<MacosMicrophoneModeStatus>,
        playback: PlaybackStats,
    ) -> Self {
        Self {
            build: common::current_build_info(),
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
            mic_gain: audio_settings.mic_gain(),
            speaker_gain: audio_settings.speaker_gain(),
            requested_input_backend: input_backend.requested,
            active_input_backend: input_backend.active,
            input_backend_note: input_backend.note,
            macos_microphone_mode,
            playback,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct FullConfigRequest {
    pub listen: Vec<u16>,
    pub tx: Vec<u16>,
    #[serde(default)]
    pub vol: HashMap<u16, f32>,
    #[serde(default)]
    pub talker_vol: HashMap<u16, f32>,
    pub codec: Codec,
    #[serde(default)]
    pub opus_profile: OpusProfile,
    pub talk_mode: TalkMode,
    pub priority: bool,
    #[serde(default)]
    pub priority_channels: Vec<u16>,
    #[serde(default)]
    pub ifb: IfbConfig,
}

impl FullConfigRequest {
    pub fn control_messages(&self, user_id: u16) -> Vec<ControlMessage> {
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
pub struct CodecRequest {
    pub codec: Codec,
}

#[derive(Debug, Deserialize)]
pub struct TalkModeRequest {
    pub mode: TalkMode,
}

#[derive(Debug, Deserialize)]
pub struct GainRequest {
    pub mic_gain: Option<f32>,
    pub speaker_gain: Option<f32>,
}

#[derive(Debug, Deserialize)]
pub struct AlertRequest {
    pub target: AlertTarget,
    #[serde(default)]
    pub message: Option<String>,
}

#[allow(async_fn_in_trait)]
pub trait ClientControlApi {
    fn state(&self) -> StateResponse;
    async fn apply_config(&self, request: FullConfigRequest) -> Result<OkResponse, String>;
    async fn set_talk_mode(&self, mode: TalkMode) -> Result<OkResponse, String>;
    async fn mute(&self) -> Result<OkResponse, String>;
    async fn unmute(&self) -> Result<OkResponse, String>;
    async fn talk_down(&self) -> Result<OkResponse, String>;
    async fn talk_up(&self) -> Result<OkResponse, String>;
    async fn talk_toggle(&self) -> Result<OkResponse, String>;
    async fn set_codec(&self, codec: Codec) -> Result<OkResponse, String>;
    fn set_gain(&self, request: GainRequest) -> Result<OkResponse, String>;
    async fn button_down(&self, id: ButtonId) -> Result<OkResponse, String>;
    async fn button_up(&self, id: ButtonId) -> Result<OkResponse, String>;
    async fn button_toggle(&self, id: ButtonId) -> Result<OkResponse, String>;
    async fn call_down(&self, target_user_id: u16) -> Result<OkResponse, String>;
    async fn call_up(&self, target_user_id: u16) -> Result<OkResponse, String>;
    async fn call_toggle(&self, target_user_id: u16) -> Result<OkResponse, String>;
    async fn reply_down(&self) -> Result<OkResponse, String>;
    async fn reply_up(&self) -> Result<OkResponse, String>;
    async fn reply_toggle(&self) -> Result<OkResponse, String>;
    async fn send_alert(&self, request: AlertRequest) -> Result<OkResponse, String>;
    async fn ack_alert(&self, alert_id: AlertId) -> Result<OkResponse, String>;
    async fn cancel_alert(&self, alert_id: AlertId) -> Result<OkResponse, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_status_constructors_set_phase_and_running_flag() {
        assert_eq!(
            ClientRuntimeStatus::stopped().phase,
            ClientRuntimePhase::Stopped
        );
        assert!(!ClientRuntimeStatus::starting().running);
        assert!(ClientRuntimeStatus::running().running);

        let failed = ClientRuntimeStatus::failed("boom");
        assert_eq!(failed.phase, ClientRuntimePhase::Failed);
        assert_eq!(failed.last_error.as_deref(), Some("boom"));
    }

    #[test]
    fn audio_backend_kind_uses_stable_json_names() {
        assert_eq!(
            serde_json::to_string(&ClientAudioBackendKind::VoiceProcessing).unwrap(),
            "\"voice_processing\""
        );
        assert_eq!(
            serde_json::from_str::<ClientAudioBackendKind>("\"ios_remote_io\"").unwrap(),
            ClientAudioBackendKind::IosRemoteIo
        );
    }
}
