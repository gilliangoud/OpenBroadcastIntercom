use std::collections::HashMap;

use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Async, FixedAsync, PolynomialDegree, Resampler};
use serde::{Deserialize, Serialize};

pub mod admin;
pub mod audio;
pub mod build_info;
pub mod device;
pub mod processing;
pub mod protocol;
pub mod status;
pub use build_info::{current_build_info, BuildInfo};

pub const MAGIC: [u8; 2] = *b"IC";
pub const VERSION: u8 = 2;
pub const SAMPLE_RATE: u32 = 16_000;
pub const PCM24_SAMPLE_RATE: u32 = 24_000;
pub const PCM48_SAMPLE_RATE: u32 = 48_000;
pub const MIX_SAMPLE_RATE: u32 = PCM48_SAMPLE_RATE;
pub const OPUS_SAMPLE_RATE: u32 = PCM48_SAMPLE_RATE;
pub const CHANNELS: u16 = 1;
pub const FRAME_MS: u32 = 10;
pub const SAMPLES_PER_FRAME: usize = (SAMPLE_RATE as usize * FRAME_MS as usize) / 1_000;
pub const PCM24_SAMPLES_PER_FRAME: usize = (PCM24_SAMPLE_RATE as usize * FRAME_MS as usize) / 1_000;
pub const PCM48_SAMPLES_PER_FRAME: usize = (PCM48_SAMPLE_RATE as usize * FRAME_MS as usize) / 1_000;
pub const MIX_SAMPLES_PER_FRAME: usize = PCM48_SAMPLES_PER_FRAME;
pub const PCM16_BYTES_PER_SAMPLE: usize = 2;
pub const PCM16_PAYLOAD_BYTES: usize = SAMPLES_PER_FRAME * PCM16_BYTES_PER_SAMPLE;
pub const PCM24_PAYLOAD_BYTES: usize = PCM24_SAMPLES_PER_FRAME * PCM16_BYTES_PER_SAMPLE;
pub const PCM48_PAYLOAD_BYTES: usize = PCM48_SAMPLES_PER_FRAME * PCM16_BYTES_PER_SAMPLE;
pub const PCM48_STEREO_PAYLOAD_BYTES: usize = PCM48_PAYLOAD_BYTES * 2;
pub const HEADER_LEN: usize = 17;
pub const OPUS_MAX_PAYLOAD_BYTES: usize = 1_275;
pub const MAX_PAYLOAD_LEN: usize = PCM48_STEREO_PAYLOAD_BYTES;
pub const MAX_PACKET_BYTES: usize = HEADER_LEN + MAX_PAYLOAD_LEN;
pub const OPUS_SPEECH_16_LOW_MONO_BITRATE_BPS: i32 = 20_000;
pub const OPUS_SPEECH_24_STANDARD_MONO_BITRATE_BPS: i32 = 32_000;
pub const OPUS_SPEECH_48_HIGH_MONO_BITRATE_BPS: i32 = 56_000;
pub const OPUS_MUSIC_48_MONO_BITRATE_BPS: i32 = 80_000;
pub const OPUS_STEREO_BITRATE_MULTIPLIER: i32 = 2;
pub const OPUS_PACKET_LOSS_PERCENT: i32 = 5;
pub const SERVER_USER_ID: UserId = 0;
pub const MIXED_CHANNEL_ID: ChannelId = 0;
pub const AUDIO_REGISTRATION_CHANNEL_ID: ChannelId = 0;
pub const DEFAULT_IFB_DUCK_GAIN: f32 = 0.125;

pub type UserId = u16;
pub type ChannelId = u16;
pub type AlertId = u64;
pub type ButtonId = String;
pub type ClientUid = String;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientRole {
    #[default]
    Client,
    Bridge,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeMode {
    Input,
    Output,
    #[default]
    Duplex,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeEndpointKind {
    #[default]
    AudioDevice,
    VmixBrowserSource,
    NdiSource,
    NdiOutput,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BridgeEndpointStatus {
    #[serde(default)]
    pub kind: BridgeEndpointKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_clients: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_level: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underflows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drops: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnects: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_audio_ms_ago: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BridgeStatus {
    #[serde(default)]
    pub mode: BridgeMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<BridgeEndpointStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<BridgeEndpointStatus>,
    #[serde(default = "default_unit_gain")]
    pub input_gain: f32,
    #[serde(default = "default_unit_gain")]
    pub output_gain: f32,
    #[serde(default)]
    pub tx: Vec<ChannelId>,
    #[serde(default)]
    pub listen: Vec<ChannelId>,
    #[serde(default)]
    pub note: String,
}

impl Default for BridgeStatus {
    fn default() -> Self {
        Self {
            mode: BridgeMode::Duplex,
            input_device: None,
            output_device: None,
            input: None,
            output: None,
            input_gain: 1.0,
            output_gain: 1.0,
            tx: Vec::new(),
            listen: Vec::new(),
            note: String::new(),
        }
    }
}

fn default_unit_gain() -> f32 {
    1.0
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TallyState {
    #[default]
    Off,
    Preview,
    Live,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TallyStatus {
    #[serde(default)]
    pub state: TallyState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_number: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_title: Option<String>,
    #[serde(default)]
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentStatus {
    #[default]
    Enrolled,
    Pending,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Codec {
    #[serde(rename = "pcm16", alias = "pcm")]
    Pcm16 = 0,
    #[serde(rename = "adpcm")]
    Adpcm = 1,
    #[serde(rename = "opus")]
    Opus = 2,
    #[serde(rename = "pcm48", alias = "pcm-48")]
    Pcm48 = 3,
    #[serde(rename = "pcm24", alias = "pcm-24")]
    Pcm24 = 4,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OpusProfile {
    #[serde(rename = "speech_16_low", alias = "speech_low")]
    Speech16Low,
    #[default]
    #[serde(rename = "speech_24_standard", alias = "speech_standard")]
    Speech24Standard,
    #[serde(rename = "speech_48_high", alias = "speech_high")]
    Speech48High,
    #[serde(rename = "music_48", alias = "music_high")]
    Music48,
}

impl OpusProfile {
    pub fn bitrate_bps(self, channels: usize) -> i32 {
        let mono = match self {
            Self::Speech16Low => OPUS_SPEECH_16_LOW_MONO_BITRATE_BPS,
            Self::Speech24Standard => OPUS_SPEECH_24_STANDARD_MONO_BITRATE_BPS,
            Self::Speech48High => OPUS_SPEECH_48_HIGH_MONO_BITRATE_BPS,
            Self::Music48 => OPUS_MUSIC_48_MONO_BITRATE_BPS,
        };
        if channels > 1 {
            mono * OPUS_STEREO_BITRATE_MULTIPLIER
        } else {
            mono
        }
    }

    pub fn complexity(self) -> i32 {
        match self {
            Self::Speech16Low => 3,
            Self::Speech24Standard => 5,
            Self::Speech48High => 8,
            Self::Music48 => 8,
        }
    }

    pub fn sample_rate_hz(self) -> u32 {
        match self {
            Self::Speech16Low => SAMPLE_RATE,
            Self::Speech24Standard => PCM24_SAMPLE_RATE,
            Self::Speech48High | Self::Music48 => PCM48_SAMPLE_RATE,
        }
    }

    pub fn samples_per_frame(self) -> usize {
        (self.sample_rate_hz() as usize * FRAME_MS as usize) / 1_000
    }

    pub fn max_bandwidth(self) -> OpusBandwidth {
        match self {
            Self::Speech16Low => OpusBandwidth::Wideband,
            Self::Speech24Standard => OpusBandwidth::Superwideband,
            Self::Speech48High | Self::Music48 => OpusBandwidth::Fullband,
        }
    }

    pub fn packet_loss_percent(self) -> i32 {
        OPUS_PACKET_LOSS_PERCENT
    }

    pub fn is_music(self) -> bool {
        matches!(self, Self::Music48)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpusBandwidth {
    Wideband,
    Superwideband,
    Fullband,
}

pub fn default_opus_profile() -> OpusProfile {
    OpusProfile::default()
}

impl TryFrom<u8> for Codec {
    type Error = PacketError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Pcm16),
            1 => Ok(Self::Adpcm),
            2 => Ok(Self::Opus),
            3 => Ok(Self::Pcm48),
            4 => Ok(Self::Pcm24),
            other => Err(PacketError::UnknownCodec(other)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum AudioTarget {
    Channel(ChannelId),
    Direct(UserId),
    Mixed,
}

impl AudioTarget {
    fn wire_parts(self) -> (u8, u16) {
        match self {
            Self::Channel(channel_id) => (1, channel_id),
            Self::Direct(user_id) => (2, user_id),
            Self::Mixed => (3, 0),
        }
    }

    fn from_wire(kind: u8, id: u16) -> Result<Self, PacketError> {
        match kind {
            1 => Ok(Self::Channel(id)),
            2 => Ok(Self::Direct(id)),
            3 if id == 0 => Ok(Self::Mixed),
            3 => Err(PacketError::InvalidTargetId { kind, id }),
            other => Err(PacketError::UnknownTargetKind(other)),
        }
    }

    pub fn channel_id(self) -> Option<ChannelId> {
        match self {
            Self::Channel(channel_id) => Some(channel_id),
            Self::Direct(_) | Self::Mixed => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPacket {
    pub user_id: UserId,
    pub target: AudioTarget,
    pub codec: Codec,
    pub seq: u16,
    pub timestamp: u32,
    pub payload: Vec<u8>,
}

impl AudioPacket {
    pub fn registration(user_id: UserId, codec: Codec, seq: u16) -> Self {
        Self {
            user_id,
            target: AudioTarget::Mixed,
            codec,
            seq,
            timestamp: 0,
            payload: Vec::new(),
        }
    }

    pub fn is_registration(&self) -> bool {
        self.target == AudioTarget::Mixed && self.payload.is_empty()
    }

    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), PacketError> {
        if self.payload.len() > u16::MAX as usize {
            return Err(PacketError::PayloadTooLarge(self.payload.len()));
        }

        out.clear();
        out.reserve(HEADER_LEN + self.payload.len());
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&self.user_id.to_be_bytes());
        let (target_kind, target_id) = self.target.wire_parts();
        out.push(target_kind);
        out.extend_from_slice(&target_id.to_be_bytes());
        out.push(self.codec as u8);
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out.extend_from_slice(&(self.payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.payload);
        Ok(())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PacketError> {
        if bytes.len() < HEADER_LEN {
            return Err(PacketError::TooShort(bytes.len()));
        }
        if bytes[0..2] != MAGIC {
            return Err(PacketError::InvalidMagic([bytes[0], bytes[1]]));
        }
        if bytes[2] != VERSION {
            return Err(PacketError::UnsupportedVersion(bytes[2]));
        }

        let user_id = u16::from_be_bytes([bytes[3], bytes[4]]);
        let target = AudioTarget::from_wire(bytes[5], u16::from_be_bytes([bytes[6], bytes[7]]))?;
        let codec = Codec::try_from(bytes[8])?;
        let seq = u16::from_be_bytes([bytes[9], bytes[10]]);
        let timestamp = u32::from_be_bytes([bytes[11], bytes[12], bytes[13], bytes[14]]);
        let payload_len = u16::from_be_bytes([bytes[15], bytes[16]]) as usize;
        let actual_len = bytes.len() - HEADER_LEN;

        if payload_len != actual_len {
            return Err(PacketError::PayloadLengthMismatch {
                declared: payload_len,
                actual: actual_len,
            });
        }

        Ok(Self {
            user_id,
            target,
            codec,
            seq,
            timestamp,
            payload: bytes[HEADER_LEN..].to_vec(),
        })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PacketError {
    #[error("packet too short: {0} bytes")]
    TooShort(usize),
    #[error("invalid magic: {0:?}")]
    InvalidMagic([u8; 2]),
    #[error("unsupported protocol version: {0}")]
    UnsupportedVersion(u8),
    #[error("unknown codec: {0}")]
    UnknownCodec(u8),
    #[error("unknown audio target kind: {0}")]
    UnknownTargetKind(u8),
    #[error("invalid audio target id {id} for kind {kind}")]
    InvalidTargetId { kind: u8, id: u16 },
    #[error("payload length mismatch: declared {declared}, actual {actual}")]
    PayloadLengthMismatch { declared: usize, actual: usize },
    #[error("payload too large: {0} bytes")]
    PayloadTooLarge(usize),
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    Hello {
        #[serde(default)]
        user_id: UserId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requested_user_id: Option<UserId>,
        #[serde(default)]
        client_uid: ClientUid,
        #[serde(default = "default_supported_codecs")]
        codecs: Vec<Codec>,
        #[serde(default)]
        buttons: Vec<ButtonCapability>,
        #[serde(default)]
        capabilities: ClientCapabilities,
        #[serde(default)]
        role: ClientRole,
    },
    Config {
        user_id: UserId,
        #[serde(default)]
        role: Option<ClientRole>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default)]
        listen: Vec<ChannelId>,
        #[serde(default)]
        tx: Vec<ChannelId>,
        #[serde(default, with = "channel_volume_map")]
        vol: HashMap<ChannelId, f32>,
        #[serde(
            default,
            with = "optional_user_gain_map",
            skip_serializing_if = "Option::is_none"
        )]
        talker_vol: Option<HashMap<UserId, f32>>,
        #[serde(default)]
        codec: Option<Codec>,
        #[serde(default)]
        opus_profile: Option<OpusProfile>,
        #[serde(default)]
        talk_mode: Option<TalkMode>,
        #[serde(default)]
        priority: Option<bool>,
        #[serde(default)]
        priority_channels: Option<Vec<ChannelId>>,
        #[serde(default)]
        processing: Option<ProcessingConfig>,
        #[serde(default)]
        buttons: Option<Vec<TalkButtonConfig>>,
        #[serde(default)]
        ifb: Option<IfbConfig>,
        #[serde(default)]
        stereo: Option<StereoConfig>,
        #[serde(default)]
        esp32_audio: Option<Esp32AudioConfig>,
    },
    AudioCodec {
        user_id: UserId,
        codec: Codec,
    },
    TalkMode {
        user_id: UserId,
        mode: TalkMode,
    },
    Ping {
        user_id: UserId,
    },
    CaptureHealth {
        user_id: UserId,
        health: CaptureHealthStatus,
    },
    BridgeStatus {
        user_id: UserId,
        status: BridgeStatus,
    },
    Talk {
        user_id: UserId,
        active: bool,
    },
    Priority {
        user_id: UserId,
        active: bool,
    },
    Emergency {
        user_id: UserId,
        active: bool,
        #[serde(default)]
        target: EmergencyTarget,
        #[serde(default = "default_ifb_duck_gain")]
        duck_gain: f32,
        #[serde(default)]
        mute_others: bool,
    },
    Button {
        user_id: UserId,
        button_id: ButtonId,
        pressed: bool,
    },
    SendAlert {
        user_id: UserId,
        target: AlertTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    AckAlert {
        user_id: UserId,
        alert_id: AlertId,
    },
    CancelAlert {
        user_id: UserId,
        alert_id: AlertId,
    },
    DirectCall {
        user_id: UserId,
        target_user_id: UserId,
        active: bool,
        #[serde(default)]
        duck: bool,
    },
    ReplyCall {
        user_id: UserId,
        active: bool,
        #[serde(default)]
        duck: bool,
    },
    Status,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ButtonCapability {
    pub id: ButtonId,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    #[default]
    Unknown,
    Desktop,
    Mobile,
    Pi,
    Esp32,
    Bridge,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientCapabilities {
    #[serde(default)]
    pub advertised: bool,
    #[serde(default)]
    pub client_kind: ClientKind,
    #[serde(default)]
    pub supports_processing: bool,
    #[serde(default)]
    pub supports_native_voice_processing: bool,
    #[serde(default)]
    pub supports_esp32_audio: bool,
    #[serde(default)]
    pub supports_stereo: bool,
    #[serde(default)]
    pub supports_ifb: bool,
    #[serde(default)]
    pub supports_local_api: bool,
    #[serde(default)]
    pub supports_device_selection: bool,
    #[serde(default)]
    pub button_action_types: Vec<String>,
}

impl Default for ClientCapabilities {
    fn default() -> Self {
        Self {
            advertised: false,
            client_kind: ClientKind::Unknown,
            supports_processing: false,
            supports_native_voice_processing: false,
            supports_esp32_audio: false,
            supports_stereo: false,
            supports_ifb: false,
            supports_local_api: false,
            supports_device_selection: false,
            button_action_types: Vec::new(),
        }
    }
}

impl ClientCapabilities {
    pub fn desktop() -> Self {
        Self {
            advertised: true,
            client_kind: ClientKind::Desktop,
            supports_processing: true,
            supports_native_voice_processing: true,
            supports_esp32_audio: false,
            supports_stereo: true,
            supports_ifb: true,
            supports_local_api: true,
            supports_device_selection: true,
            button_action_types: default_button_action_types(),
        }
    }

    pub fn mobile() -> Self {
        Self {
            advertised: true,
            client_kind: ClientKind::Mobile,
            supports_processing: false,
            supports_native_voice_processing: true,
            supports_esp32_audio: false,
            supports_stereo: true,
            supports_ifb: true,
            supports_local_api: false,
            supports_device_selection: false,
            button_action_types: default_button_action_types(),
        }
    }

    pub fn pi() -> Self {
        Self {
            advertised: true,
            client_kind: ClientKind::Pi,
            supports_processing: false,
            supports_native_voice_processing: false,
            supports_esp32_audio: false,
            supports_stereo: true,
            supports_ifb: true,
            supports_local_api: true,
            supports_device_selection: true,
            button_action_types: default_button_action_types(),
        }
    }

    pub fn esp32() -> Self {
        Self {
            advertised: true,
            client_kind: ClientKind::Esp32,
            supports_processing: false,
            supports_native_voice_processing: false,
            supports_esp32_audio: true,
            supports_stereo: false,
            supports_ifb: false,
            supports_local_api: false,
            supports_device_selection: false,
            button_action_types: default_button_action_types(),
        }
    }

    pub fn bridge() -> Self {
        Self {
            advertised: true,
            client_kind: ClientKind::Bridge,
            supports_processing: false,
            supports_native_voice_processing: false,
            supports_esp32_audio: false,
            supports_stereo: true,
            supports_ifb: false,
            supports_local_api: false,
            supports_device_selection: true,
            button_action_types: Vec::new(),
        }
    }
}

pub fn default_button_action_types() -> Vec<String> {
    [
        "transmit",
        "alert",
        "apply_preset",
        "set_talk_mode",
        "route_edit",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TalkButtonMode {
    #[default]
    Momentary,
    Latching,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TalkMode {
    Muted,
    #[default]
    Ptt,
    Open,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum AlertTarget {
    User(UserId),
    Channel(ChannelId),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EmergencyTarget {
    #[default]
    All,
    Users {
        #[serde(default)]
        users: Vec<UserId>,
    },
    Channels {
        #[serde(default)]
        channels: Vec<ChannelId>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmergencyStatus {
    pub active: bool,
    pub source: UserId,
    #[serde(default)]
    pub target: EmergencyTarget,
    pub duck_gain: f32,
    #[serde(default)]
    pub mute_others: bool,
    #[serde(default)]
    pub recipients: Vec<UserId>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingMode {
    #[default]
    Auto,
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingProfile {
    Raw,
    #[default]
    Voice,
    VoiceIsolation,
    Broadcast,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingEngine {
    #[serde(alias = "builtin")]
    #[default]
    BuiltIn,
    #[serde(rename = "webrtc", alias = "web_rtc")]
    WebRtc,
    #[serde(rename = "rnnoise", alias = "rn_noise")]
    RnNoise,
    #[serde(rename = "deepfilternet", alias = "deep_filter_net")]
    DeepFilterNet,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeepFilterBackend {
    #[default]
    Auto,
    Tract,
    #[serde(rename = "coreml", alias = "core_ml")]
    CoreMl,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppleComputeUnits {
    CpuOnly,
    CpuAndGpu,
    CpuAndNeuralEngine,
    #[default]
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessingConfig {
    #[serde(default)]
    pub mode: ProcessingMode,
    #[serde(default)]
    pub engine: ProcessingEngine,
    #[serde(default)]
    pub profile: ProcessingProfile,
    #[serde(default = "default_processing_module_enabled")]
    pub high_pass: bool,
    #[serde(default = "default_processing_module_enabled")]
    pub noise_gate: bool,
    #[serde(default = "default_processing_module_enabled")]
    pub compressor: bool,
    #[serde(default = "default_processing_module_enabled")]
    pub presence: bool,
    #[serde(default = "default_processing_module_enabled")]
    pub vad: bool,
    #[serde(default = "default_processing_module_enabled")]
    pub transient_suppression: bool,
    #[serde(default = "default_processing_module_enabled")]
    pub native_voice_processing: bool,
    #[serde(default = "default_processing_fallback_to_builtin")]
    pub fallback_to_builtin: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deep_filter_model: Option<String>,
    #[serde(default)]
    pub deep_filter_backend: DeepFilterBackend,
    #[serde(default)]
    pub apple_compute_units: AppleComputeUnits,
    #[serde(default = "default_processing_worker_queue_frames")]
    pub worker_queue_frames: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pipeline: Vec<ProcessingStageConfig>,
    #[serde(default)]
    pub normalization: LevelNormalizationConfig,
}

impl Default for ProcessingConfig {
    fn default() -> Self {
        Self {
            mode: ProcessingMode::Auto,
            engine: ProcessingEngine::BuiltIn,
            profile: ProcessingProfile::Voice,
            high_pass: true,
            noise_gate: true,
            compressor: true,
            presence: true,
            vad: true,
            transient_suppression: true,
            native_voice_processing: true,
            fallback_to_builtin: true,
            deep_filter_model: None,
            deep_filter_backend: DeepFilterBackend::Auto,
            apple_compute_units: AppleComputeUnits::All,
            worker_queue_frames: default_processing_worker_queue_frames(),
            pipeline: Vec::new(),
            normalization: LevelNormalizationConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LevelNormalizationConfig {
    pub enabled: bool,
    pub target_rms: f32,
    pub max_boost: f32,
    pub max_attenuation: f32,
    pub adaptation_ms: u32,
    pub noise_floor_rms: f32,
}

impl Default for LevelNormalizationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target_rms: 0.14,
            max_boost: 4.0,
            max_attenuation: 8.0,
            adaptation_ms: 250,
            noise_floor_rms: 0.012,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProcessingStageConfig {
    pub engine: ProcessingEngine,
    pub enabled: bool,
}

impl Default for ProcessingStageConfig {
    fn default() -> Self {
        Self {
            engine: ProcessingEngine::BuiltIn,
            enabled: true,
        }
    }
}

pub fn default_processing_module_enabled() -> bool {
    true
}

pub fn default_processing_fallback_to_builtin() -> bool {
    true
}

pub fn default_processing_worker_queue_frames() -> usize {
    12
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Esp32AdcInput {
    #[default]
    Difference,
    Mic1,
    Mic2,
    Line1,
    Line2,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Esp32CaptureChannel {
    #[default]
    Left,
    Right,
    Average,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Esp32SidetoneMode {
    #[default]
    Off,
    Firmware,
    CodecBypass,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Esp32SidetoneControlConfig {
    pub mode: Esp32SidetoneMode,
    pub firmware_gain_percent: u16,
    pub codec_bypass_gain_percent: u16,
    pub mic_bypass_gain_percent: u16,
}

impl Default for Esp32SidetoneControlConfig {
    fn default() -> Self {
        Self {
            mode: Esp32SidetoneMode::Off,
            firmware_gain_percent: 25,
            codec_bypass_gain_percent: 25,
            mic_bypass_gain_percent: 100,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Esp32AudioConfig {
    pub enabled: bool,
    pub adc_input: Esp32AdcInput,
    pub mic_pga_gain_db: u8,
    pub capture_channel: Esp32CaptureChannel,
    pub mic_software_gain_percent: u16,
    pub speaker_software_gain_percent: u16,
    pub notification_gain_percent: u16,
    pub high_pass_enabled: bool,
    pub alc_enabled: bool,
    pub noise_gate_enabled: bool,
    pub sidetone: Esp32SidetoneControlConfig,
}

impl Default for Esp32AudioConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            adc_input: Esp32AdcInput::Difference,
            mic_pga_gain_db: 9,
            capture_channel: Esp32CaptureChannel::Left,
            mic_software_gain_percent: 100,
            speaker_software_gain_percent: 100,
            notification_gain_percent: 50,
            high_pass_enabled: true,
            alc_enabled: true,
            noise_gate_enabled: true,
            sidetone: Esp32SidetoneControlConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CaptureChannelHealth {
    pub rms: f32,
    pub peak: f32,
    pub dc_offset: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Esp32SidetoneConfig {
    pub mode: String,
    pub firmware_gain_percent: u16,
    pub codec_bypass_gain_percent: u16,
    pub mic_bypass_gain_percent: u16,
    pub active_bypass_source: String,
    pub codec_bypass_preserves_dac: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Esp32CodecConfig {
    pub chip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_codec: Option<Codec>,
    #[serde(default)]
    pub server_control_enabled: bool,
    #[serde(default)]
    pub audio_backend: String,
    pub adc_input: String,
    pub mic_pga_gain_db: u8,
    pub capture_channel: String,
    pub mic_software_gain_percent: u16,
    pub speaker_software_gain_percent: u16,
    pub notification_gain_percent: u16,
    pub high_pass_enabled: bool,
    #[serde(default)]
    pub alc_enabled: bool,
    #[serde(default)]
    pub noise_gate_enabled: bool,
    #[serde(default)]
    pub hardware_sample_rate_hz: u32,
    #[serde(default)]
    pub hardware_channels: u8,
    #[serde(default)]
    pub hardware_bits_per_sample: u8,
    pub i2s_sample_rate_hz: u32,
    #[serde(default)]
    pub i2s_format: String,
    #[serde(default)]
    pub i2s_slot_width: String,
    pub sidetone: Esp32SidetoneConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CaptureHealthStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<ClientTelemetryRuntimeStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<ClientTelemetryAudioStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback: Option<ClientTelemetryPlaybackStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_transport: Option<ClientTelemetryTransportStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec_config: Option<Esp32CodecConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop: Option<DesktopCaptureHealthStatus>,
    #[serde(default)]
    pub uptime_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wifi: Option<Esp32WifiHealthStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Esp32TransportHealthStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<Esp32MemoryHealthStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_stack_high_water_bytes: Option<Esp32TaskStackHealthStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<Esp32DisplayHealthStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<Esp32BatteryHealthStatus>,
    #[serde(default)]
    pub playback_queue_depth: u16,
    #[serde(default)]
    pub playback_underflows: u32,
    #[serde(default)]
    pub playback_overflows: u32,
    #[serde(default)]
    pub playback_i2s_gap_warnings: u32,
    #[serde(default)]
    pub playback_i2s_slow_warnings: u32,
    #[serde(default)]
    pub playback_i2s_short_warnings: u32,
    #[serde(default)]
    pub free_heap_bytes: u32,
    #[serde(default)]
    pub min_free_heap_bytes: u32,
    #[serde(default)]
    pub tx_target_count: u16,
    #[serde(default)]
    pub tx_packets_sent: u32,
    #[serde(default)]
    pub tx_send_failures: u32,
    pub adc_input: String,
    pub mic_pga_gain_db: u8,
    pub capture_channel: String,
    pub software_gain_percent: u16,
    pub high_pass_enabled: bool,
    #[serde(default)]
    pub alc_enabled: bool,
    #[serde(default)]
    pub noise_gate_enabled: bool,
    pub left: CaptureChannelHealth,
    pub right: CaptureChannelHealth,
    pub selected: CaptureChannelHealth,
    pub raw_clipped_samples: u32,
    pub software_clipped_samples: u32,
}

pub type ClientTelemetryStatus = CaptureHealthStatus;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientTelemetryRuntimeStatus {
    #[serde(default)]
    pub client_kind: String,
    #[serde(default)]
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ClientTelemetryAudioStatus {
    #[serde(default)]
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_device: Option<String>,
    #[serde(default)]
    pub sample_format: String,
    #[serde(default)]
    pub sample_rate_hz: u32,
    #[serde(default)]
    pub channels: u16,
    #[serde(default)]
    pub channel_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_gain: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker_gain: Option<f32>,
    #[serde(default)]
    pub input: CaptureChannelHealth,
    #[serde(default)]
    pub pre_gain: CaptureChannelHealth,
    #[serde(default)]
    pub post_gain: CaptureChannelHealth,
    #[serde(default)]
    pub pre_gain_clipped_samples: u32,
    #[serde(default)]
    pub post_gain_clipped_samples: u32,
    #[serde(default)]
    pub dropped_frames: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientTelemetryPlaybackStatus {
    #[serde(default)]
    pub available_samples: u64,
    #[serde(default)]
    pub capacity_samples: u64,
    #[serde(default)]
    pub prebuffer_samples: u64,
    #[serde(default)]
    pub queue_depth: u64,
    #[serde(default)]
    pub channels: u16,
    #[serde(default)]
    pub started: bool,
    #[serde(default)]
    pub underflows: u64,
    #[serde(default)]
    pub overflows: u64,
    #[serde(default)]
    pub dropped_samples: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientTelemetryTransportStatus {
    #[serde(default)]
    pub udp_rx_packets: u64,
    #[serde(default)]
    pub malformed_packets: u64,
    #[serde(default)]
    pub decode_errors: u64,
    #[serde(default)]
    pub codec_drops: u64,
    #[serde(default)]
    pub payload_decode_errors: u64,
    #[serde(default)]
    pub packet_encode_errors: u64,
    #[serde(default)]
    pub tx_packets: u64,
    #[serde(default)]
    pub tx_send_failures: u64,
    #[serde(default)]
    pub tx_queue_drops: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Esp32WifiHealthStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rssi_dbm: Option<i16>,
    #[serde(default)]
    pub connect_count: u32,
    #[serde(default)]
    pub disconnect_count: u32,
    #[serde(default)]
    pub control_connect_count: u32,
    #[serde(default)]
    pub control_disconnect_count: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Esp32TransportHealthStatus {
    #[serde(default)]
    pub udp_rx_packets: u32,
    #[serde(default)]
    pub udp_decode_errors: u32,
    #[serde(default)]
    pub udp_codec_drops: u32,
    #[serde(default)]
    pub udp_sequence_gaps: u32,
    #[serde(default)]
    pub udp_payload_decode_errors: u32,
    #[serde(default)]
    pub udp_tx_send_failures: u32,
    #[serde(default)]
    pub audio_tx_queue_drops: u32,
    #[serde(default)]
    pub opus_encode_failures: u32,
    #[serde(default)]
    pub opus_decode_failures: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Esp32MemoryHealthStatus {
    #[serde(default)]
    pub free_heap_bytes: u32,
    #[serde(default)]
    pub min_free_heap_bytes: u32,
    #[serde(default)]
    pub internal_free_heap_bytes: u32,
    #[serde(default)]
    pub internal_largest_free_block_bytes: u32,
    #[serde(default)]
    pub spiram_free_heap_bytes: u32,
    #[serde(default)]
    pub spiram_largest_free_block_bytes: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Esp32TaskStackHealthStatus {
    #[serde(default)]
    pub udp: u32,
    #[serde(default)]
    pub registration: u32,
    #[serde(default)]
    pub playback: u32,
    #[serde(default)]
    pub capture: u32,
    #[serde(default)]
    pub buttons: u32,
    #[serde(default)]
    pub display: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Esp32DisplayHealthStatus {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub initialized: bool,
    #[serde(default)]
    pub framebuffer_in_psram: bool,
    #[serde(default)]
    pub framebuffer_bytes: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Esp32BatteryHealthStatus {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub millivolts: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DesktopCaptureHealthStatus {
    pub backend: String,
    pub device: String,
    pub sample_format: String,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub channel_mode: String,
    pub mic_gain: f32,
    pub pre_gain: CaptureChannelHealth,
    pub post_gain: CaptureChannelHealth,
    pub pre_gain_clipped_samples: u32,
    pub post_gain_clipped_samples: u32,
    pub dropped_frames: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessingStatus {
    pub active: bool,
    pub bypassed: bool,
    pub gate_open: bool,
    #[serde(default)]
    pub engine: ProcessingEngine,
    #[serde(default)]
    pub engine_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compute_units: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_ms: Option<f32>,
    pub input_rms: f32,
    pub output_rms: f32,
    pub gain_reduction_db: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<ProcessingStageStatus>,
    #[serde(default)]
    pub normalization: LevelNormalizationStatus,
}

impl Default for ProcessingStatus {
    fn default() -> Self {
        Self {
            active: false,
            bypassed: true,
            gate_open: false,
            engine: ProcessingEngine::BuiltIn,
            engine_available: true,
            engine_detail: None,
            backend: None,
            compute_units: None,
            inference_ms: None,
            input_rms: 0.0,
            output_rms: 0.0,
            gain_reduction_db: 0.0,
            stages: Vec::new(),
            normalization: LevelNormalizationStatus::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LevelNormalizationStatus {
    pub active: bool,
    pub bypassed: bool,
    pub input_rms: f32,
    pub output_rms: f32,
    pub target_rms: f32,
    pub applied_gain: f32,
    pub desired_gain: f32,
    pub max_boost: f32,
    pub max_attenuation: f32,
    pub clipping_events: u32,
    pub reason: String,
}

impl Default for LevelNormalizationStatus {
    fn default() -> Self {
        let config = LevelNormalizationConfig::default();
        Self {
            active: false,
            bypassed: true,
            input_rms: 0.0,
            output_rms: 0.0,
            target_rms: config.target_rms,
            applied_gain: 1.0,
            desired_gain: 1.0,
            max_boost: config.max_boost,
            max_attenuation: config.max_attenuation,
            clipping_events: 0,
            reason: "disabled".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessingStageStatus {
    pub engine: ProcessingEngine,
    pub active: bool,
    pub bypassed: bool,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compute_units: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_ms: Option<f32>,
    pub input_rms: f32,
    pub output_rms: f32,
    pub gain_reduction_db: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ChannelPresenceRoster {
    pub channel_id: ChannelId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub members: Vec<ChannelPresenceMember>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ChannelPresenceMember {
    pub user_id: UserId,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub present: bool,
    #[serde(default)]
    pub transmitting: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AlertRecipientStatus {
    pub user_id: UserId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acked_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AlertStatus {
    pub id: AlertId,
    pub sender: UserId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
    pub target: AlertTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub created_at_ms: u64,
    #[serde(default)]
    pub recipients: Vec<AlertRecipientStatus>,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at_ms: Option<u64>,
}

impl AlertStatus {
    pub fn active_for(&self, user_id: UserId) -> bool {
        !self.cancelled
            && self
                .recipients
                .iter()
                .any(|recipient| recipient.user_id == user_id && recipient.acked_at_ms.is_none())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TalkButtonAction {
    Transmit {
        #[serde(default)]
        channels: Vec<ChannelId>,
        #[serde(default)]
        users: Vec<UserId>,
        #[serde(default)]
        duck: bool,
    },
    Alert {
        #[serde(default)]
        targets: Vec<AlertTarget>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    ApplyPreset {
        preset_id: String,
    },
    SetTalkMode {
        #[serde(default)]
        users: Vec<UserId>,
        mode: TalkMode,
    },
    RouteEdit {
        #[serde(default)]
        users: Vec<UserId>,
        #[serde(default)]
        listen_add: Vec<ChannelId>,
        #[serde(default)]
        listen_remove: Vec<ChannelId>,
        #[serde(default)]
        listen_toggle: Vec<ChannelId>,
        #[serde(default)]
        tx_add: Vec<ChannelId>,
        #[serde(default)]
        tx_remove: Vec<ChannelId>,
        #[serde(default)]
        tx_toggle: Vec<ChannelId>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TalkButtonConfig {
    pub id: ButtonId,
    #[serde(default)]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub mode: TalkButtonMode,
    pub actions: Vec<TalkButtonAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IfbConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub program: Vec<ChannelId>,
    #[serde(default)]
    pub interrupt: Vec<ChannelId>,
    #[serde(default = "default_ifb_duck_gain")]
    pub duck_gain: f32,
}

impl Default for IfbConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            program: Vec::new(),
            interrupt: Vec::new(),
            duck_gain: DEFAULT_IFB_DUCK_GAIN,
        }
    }
}

pub fn default_ifb_duck_gain() -> f32 {
    DEFAULT_IFB_DUCK_GAIN
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StereoConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, with = "channel_pan_map")]
    pub channel_pan: HashMap<ChannelId, f32>,
}

impl StereoConfig {
    pub fn active_for_codec(&self, codec: Codec) -> bool {
        self.enabled && matches!(codec, Codec::Pcm48 | Codec::Opus)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectCallStatus {
    pub caller: UserId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_name: Option<String>,
    pub target: UserId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_name: Option<String>,
    pub active: bool,
    #[serde(default)]
    pub duck: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectCallHistoryEntry {
    pub caller: UserId,
    pub target: UserId,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    #[serde(default)]
    pub duck: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientLockoutPolicy {
    #[serde(default = "default_lockout_allowed")]
    pub allow_channels: bool,
    #[serde(default = "default_lockout_allowed")]
    pub allow_volumes: bool,
    #[serde(default = "default_lockout_allowed")]
    pub allow_codec: bool,
    #[serde(default = "default_lockout_allowed")]
    pub allow_talk_mode: bool,
    #[serde(default = "default_lockout_allowed")]
    pub allow_priority: bool,
    #[serde(default = "default_lockout_allowed")]
    pub allow_buttons: bool,
    #[serde(default = "default_lockout_allowed")]
    pub allow_ifb: bool,
    #[serde(default = "default_lockout_allowed")]
    pub allow_device_selection: bool,
    #[serde(default = "default_lockout_allowed")]
    pub allow_local_api: bool,
}

impl Default for ClientLockoutPolicy {
    fn default() -> Self {
        Self {
            allow_channels: true,
            allow_volumes: true,
            allow_codec: true,
            allow_talk_mode: true,
            allow_priority: true,
            allow_buttons: true,
            allow_ifb: true,
            allow_device_selection: true,
            allow_local_api: true,
        }
    }
}

pub fn default_lockout_allowed() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IfbStatus {
    pub active: bool,
    pub duck_gain: f32,
}

impl Default for IfbStatus {
    fn default() -> Self {
        Self {
            active: false,
            duck_gain: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StereoStatus {
    pub active: bool,
    pub channels: u16,
    pub warning: Option<String>,
}

impl Default for StereoStatus {
    fn default() -> Self {
        Self {
            active: false,
            channels: CHANNELS,
            warning: None,
        }
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlEvent {
    ConfigUpdate {
        user_id: UserId,
        #[serde(default)]
        client_uid: ClientUid,
        #[serde(default)]
        name: String,
        listen: Vec<ChannelId>,
        tx: Vec<ChannelId>,
        #[serde(with = "channel_volume_map")]
        vol: HashMap<ChannelId, f32>,
        #[serde(default, with = "user_gain_map")]
        talker_vol: HashMap<UserId, f32>,
        codec: Codec,
        #[serde(default)]
        opus_profile: OpusProfile,
        #[serde(default)]
        talk_mode: TalkMode,
        #[serde(default)]
        regular_talk_active: bool,
        priority: bool,
        #[serde(default)]
        priority_channels: Vec<ChannelId>,
        #[serde(default)]
        processing: ProcessingConfig,
        #[serde(default)]
        buttons: Vec<TalkButtonConfig>,
        #[serde(default)]
        active_buttons: Vec<ButtonId>,
        #[serde(default)]
        active_direct_calls: Vec<DirectCallStatus>,
        #[serde(default)]
        last_direct_caller: Option<UserId>,
        #[serde(default)]
        direct_call_history: Vec<DirectCallHistoryEntry>,
        #[serde(default)]
        active_alerts: Vec<AlertStatus>,
        #[serde(default)]
        recent_alerts: Vec<AlertStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emergency: Option<EmergencyStatus>,
        #[serde(default)]
        ifb: IfbConfig,
        #[serde(default)]
        lockout: ClientLockoutPolicy,
        #[serde(default)]
        stereo: StereoConfig,
        #[serde(default)]
        esp32_audio: Esp32AudioConfig,
    },
    PresenceUpdate {
        user_id: UserId,
        #[serde(default)]
        client_uid: ClientUid,
        #[serde(default)]
        channels: Vec<ChannelPresenceRoster>,
    },
    TallyUpdate {
        user_id: UserId,
        #[serde(default)]
        client_uid: ClientUid,
        #[serde(default)]
        tally: TallyStatus,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlResponse {
    Hello {
        preconfigured: bool,
        #[serde(default)]
        user_id: UserId,
        #[serde(default)]
        client_uid: ClientUid,
        #[serde(default)]
        enrollment: EnrollmentStatus,
    },
    Ack,
    Error {
        message: String,
    },
    Status {
        sessions: Vec<SessionStatus>,
        metrics: StatusMetrics,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionStatus {
    pub user_id: UserId,
    #[serde(default)]
    pub client_uid: ClientUid,
    #[serde(default)]
    pub enrollment: EnrollmentStatus,
    #[serde(default)]
    pub role: ClientRole,
    pub addr: Option<String>,
    pub listen: Vec<ChannelId>,
    pub tx: Vec<ChannelId>,
    #[serde(default, with = "user_gain_map")]
    pub talker_vol: HashMap<UserId, f32>,
    pub codec: Codec,
    #[serde(default)]
    pub opus_profile: OpusProfile,
    pub supported_codecs: Vec<Codec>,
    #[serde(default)]
    pub advertised_buttons: Vec<ButtonCapability>,
    #[serde(default)]
    pub capabilities: ClientCapabilities,
    #[serde(default)]
    pub buttons: Vec<TalkButtonConfig>,
    #[serde(default)]
    pub active_buttons: Vec<ButtonId>,
    #[serde(default)]
    pub active_direct_calls: Vec<DirectCallStatus>,
    #[serde(default)]
    pub last_direct_caller: Option<UserId>,
    #[serde(default)]
    pub direct_call_history: Vec<DirectCallHistoryEntry>,
    #[serde(default)]
    pub active_alerts: Vec<AlertStatus>,
    #[serde(default)]
    pub recent_alerts: Vec<AlertStatus>,
    #[serde(default)]
    pub ifb: IfbConfig,
    #[serde(default)]
    pub lockout: ClientLockoutPolicy,
    #[serde(default)]
    pub ifb_status: IfbStatus,
    #[serde(default)]
    pub stereo: StereoConfig,
    #[serde(default)]
    pub esp32_audio: Esp32AudioConfig,
    #[serde(default)]
    pub stereo_status: StereoStatus,
    #[serde(default)]
    pub talk_mode: TalkMode,
    #[serde(default)]
    pub regular_talk_active: bool,
    pub priority: bool,
    #[serde(default)]
    pub priority_channels: Vec<ChannelId>,
    #[serde(default)]
    pub processing: ProcessingConfig,
    #[serde(default)]
    pub processing_status: ProcessingStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emergency: Option<EmergencyStatus>,
    pub queue_depth: usize,
    pub age_ms: u64,
    #[serde(default)]
    pub input: InputMeterStatus,
    #[serde(default)]
    pub output: OutputMeterStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<CaptureHealthStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<BridgeStatus>,
    #[serde(default)]
    pub tally: TallyStatus,
    #[serde(default)]
    pub transport: TransportHealthStatus,
    #[serde(default)]
    pub recording_enabled: bool,
    #[serde(default)]
    pub transcription_enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InputMeterStatus {
    pub active: bool,
    pub peak: f32,
    pub rms: f32,
    pub last_channel: Option<ChannelId>,
    pub last_packet_age_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OutputMeterStatus {
    pub peak: f32,
    pub rms: f32,
    pub limiter_gain: f32,
    pub limiter_reduction_db: f32,
    pub limiter_events: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportHealthStatus {
    pub source_queue_depth: usize,
    pub source_frames_dropped: u64,
    pub decode_errors: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusMetrics {
    pub audio_packets_received: u64,
    pub malformed_packets_dropped: u64,
    pub audio_decode_errors: u64,
    pub audio_frames_decoded: u64,
    pub source_frames_enqueued: u64,
    pub source_frames_dropped: u64,
    pub expired_source_queues: u64,
    pub mixed_packets_sent: u64,
    pub audio_encode_errors: u64,
    pub audio_send_errors: u64,
    pub control_messages_received: u64,
}

pub fn pcm16_samples_to_le_bytes(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * PCM16_BYTES_PER_SAMPLE);
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

pub fn pcm16_le_bytes_to_samples(bytes: &[u8]) -> Result<Vec<i16>, PcmError> {
    if !bytes.len().is_multiple_of(PCM16_BYTES_PER_SAMPLE) {
        return Err(PcmError::OddByteLength(bytes.len()));
    }

    Ok(bytes
        .chunks_exact(PCM16_BYTES_PER_SAMPLE)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect())
}

pub fn codec_sample_rate(codec: Codec) -> u32 {
    match codec {
        Codec::Pcm16 | Codec::Adpcm => SAMPLE_RATE,
        Codec::Pcm24 => PCM24_SAMPLE_RATE,
        Codec::Opus => OpusProfile::default().sample_rate_hz(),
        Codec::Pcm48 => PCM48_SAMPLE_RATE,
    }
}

pub fn codec_samples_per_frame(codec: Codec) -> usize {
    frame_len_for_rate(codec_sample_rate(codec))
}

pub fn codec_pcm16_payload_bytes(codec: Codec) -> usize {
    codec_samples_per_frame(codec) * PCM16_BYTES_PER_SAMPLE
}

pub fn frame_len_for_rate(rate: u32) -> usize {
    (rate as usize * FRAME_MS as usize) / 1_000
}

#[derive(Debug)]
pub struct PcmFrameResampler {
    from_rate: u32,
    to_rate: u32,
    target_len: usize,
    resampler: Option<Async<f32>>,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
}

impl PcmFrameResampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Result<Self, PcmError> {
        let input_len = frame_len_for_rate(from_rate);
        let target_len = frame_len_for_rate(to_rate);
        let resampler = if from_rate == to_rate {
            None
        } else {
            Some(
                Async::<f32>::new_poly(
                    to_rate as f64 / from_rate as f64,
                    1.0,
                    PolynomialDegree::Cubic,
                    input_len,
                    1,
                    FixedAsync::Input,
                )
                .map_err(|err| PcmError::Resample(err.to_string()))?,
            )
        };
        let output_len = resampler.as_ref().map_or(target_len, |resampler| {
            resampler.output_frames_max().max(target_len)
        });

        Ok(Self {
            from_rate,
            to_rate,
            target_len,
            resampler,
            input: vec![vec![0.0; input_len]],
            output: vec![vec![0.0; output_len]],
        })
    }

    pub fn process(&mut self, samples: &[i16]) -> Result<Vec<i16>, PcmError> {
        if self.from_rate == self.to_rate {
            return Ok(samples.to_vec());
        }
        if samples.len() != self.input[0].len() {
            return Err(PcmError::Resample(format!(
                "unexpected resampler input length: got {}, expected {}",
                samples.len(),
                self.input[0].len()
            )));
        }

        for (slot, sample) in self.input[0].iter_mut().zip(samples) {
            *slot = *sample as f32 / i16::MAX as f32;
        }
        self.output[0].fill(0.0);

        let input_len = self.input[0].len();
        let output_len = self.output[0].len();
        let input = SequentialSliceOfVecs::new(&self.input, 1, input_len)
            .map_err(|err| PcmError::Resample(err.to_string()))?;
        let mut output = SequentialSliceOfVecs::new_mut(&mut self.output, 1, output_len)
            .map_err(|err| PcmError::Resample(err.to_string()))?;
        let (_, written) = self
            .resampler
            .as_mut()
            .ok_or_else(|| PcmError::Resample("resampler was not initialized".to_string()))?
            .process_into_buffer(&input, &mut output, None)
            .map_err(|err| PcmError::Resample(err.to_string()))?;

        let usable = written.min(self.target_len);
        let mut samples = self.output[0][..usable]
            .iter()
            .map(|sample| {
                (sample * i16::MAX as f32)
                    .round()
                    .clamp(i16::MIN as f32, i16::MAX as f32) as i16
            })
            .collect::<Vec<_>>();
        if samples.len() < self.target_len {
            let fill = samples.last().copied().unwrap_or(0);
            samples.resize(self.target_len, fill);
        }
        Ok(samples)
    }
}

pub fn resample_linear(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
    resample_audio(samples, from_rate, to_rate).unwrap_or_else(|_| {
        if from_rate == to_rate {
            return samples.to_vec();
        }

        let target_len = (samples.len() as u64 * to_rate as u64 / from_rate as u64) as usize;
        resample_linear_to_len(samples, target_len.max(1))
    })
}

pub fn resample_audio(samples: &[i16], from_rate: u32, to_rate: u32) -> Result<Vec<i16>, PcmError> {
    if from_rate == to_rate {
        return Ok(samples.to_vec());
    }
    if samples.is_empty() {
        return Ok(Vec::new());
    }

    let input = vec![samples
        .iter()
        .map(|sample| *sample as f32 / i16::MAX as f32)
        .collect::<Vec<_>>()];
    let input_adapter = SequentialSliceOfVecs::new(&input, 1, samples.len())
        .map_err(|err| PcmError::Resample(err.to_string()))?;
    let mut resampler = Async::<f32>::new_poly(
        to_rate as f64 / from_rate as f64,
        1.0,
        PolynomialDegree::Cubic,
        samples.len(),
        1,
        FixedAsync::Input,
    )
    .map_err(|err| PcmError::Resample(err.to_string()))?;
    let output = resampler
        .process(&input_adapter, 0, None)
        .map_err(|err| PcmError::Resample(err.to_string()))?;
    let output = output.take_data();

    let target_len = (samples.len() as u64 * to_rate as u64 / from_rate as u64) as usize;
    let mut output = output
        .iter()
        .map(|sample| {
            (sample * i16::MAX as f32)
                .round()
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16
        })
        .collect::<Vec<_>>();
    if output.len() > target_len {
        output.truncate(target_len);
    } else if output.len() < target_len {
        let fill = output.last().copied().unwrap_or(0);
        output.resize(target_len, fill);
    }
    Ok(output)
}

pub fn resample_linear_to_len(samples: &[i16], target_len: usize) -> Vec<i16> {
    if samples.is_empty() || target_len == 0 {
        return Vec::new();
    }
    if samples.len() == target_len {
        return samples.to_vec();
    }
    if target_len == 1 {
        return vec![samples[0]];
    }
    if samples.len() == 1 {
        return vec![samples[0]; target_len];
    }

    let source_span = (samples.len() - 1) as f32;
    let target_span = (target_len - 1) as f32;
    (0..target_len)
        .map(|index| {
            let pos = index as f32 * source_span / target_span;
            let left = pos.floor() as usize;
            let right = (left + 1).min(samples.len() - 1);
            let frac = pos - left as f32;
            let sample =
                samples[left] as f32 + (samples[right] as f32 - samples[left] as f32) * frac;
            sample.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16
        })
        .collect()
}

pub fn default_supported_codecs() -> Vec<Codec> {
    vec![Codec::Pcm16]
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PcmError {
    #[error("PCM16 payload has odd byte length: {0}")]
    OddByteLength(usize),
    #[error("resample failed: {0}")]
    Resample(String),
}

pub mod channel_volume_map {
    use std::collections::HashMap;

    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::ChannelId;

    pub fn serialize<S>(value: &HashMap<ChannelId, f32>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let string_keyed = value
            .iter()
            .map(|(channel, gain)| (channel.to_string(), *gain))
            .collect::<HashMap<_, _>>();
        string_keyed.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<HashMap<ChannelId, f32>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let string_keyed = HashMap::<String, f32>::deserialize(deserializer)?;
        string_keyed
            .into_iter()
            .map(|(channel, gain)| {
                let channel = channel.parse::<ChannelId>().map_err(|err| {
                    D::Error::custom(format!("invalid channel `{channel}`: {err}"))
                })?;
                Ok((channel, gain))
            })
            .collect()
    }
}

pub mod channel_pan_map {
    use std::collections::HashMap;

    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::ChannelId;

    pub fn serialize<S>(value: &HashMap<ChannelId, f32>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let string_keyed = value
            .iter()
            .map(|(channel, pan)| (channel.to_string(), *pan))
            .collect::<HashMap<_, _>>();
        string_keyed.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<HashMap<ChannelId, f32>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let string_keyed = HashMap::<String, f32>::deserialize(deserializer)?;
        string_keyed
            .into_iter()
            .map(|(channel, pan)| {
                let channel = channel.parse::<ChannelId>().map_err(|err| {
                    D::Error::custom(format!("invalid channel `{channel}`: {err}"))
                })?;
                Ok((channel, pan))
            })
            .collect()
    }
}

pub mod user_gain_map {
    use std::collections::HashMap;

    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::UserId;

    pub fn serialize<S>(value: &HashMap<UserId, f32>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let string_keyed = value
            .iter()
            .map(|(user_id, gain)| (user_id.to_string(), *gain))
            .collect::<HashMap<_, _>>();
        string_keyed.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<HashMap<UserId, f32>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let string_keyed = HashMap::<String, f32>::deserialize(deserializer)?;
        string_keyed
            .into_iter()
            .map(|(user_id, gain)| {
                let user_id = user_id.parse::<UserId>().map_err(|err| {
                    D::Error::custom(format!("invalid user id `{user_id}`: {err}"))
                })?;
                Ok((user_id, gain))
            })
            .collect()
    }
}

pub mod optional_user_gain_map {
    use std::collections::HashMap;

    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::UserId;

    pub fn serialize<S>(
        value: &Option<HashMap<UserId, f32>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) => super::user_gain_map::serialize(value, serializer),
            None => Option::<HashMap<String, f32>>::None.serialize(serializer),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<HashMap<UserId, f32>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<HashMap<String, f32>>::deserialize(deserializer)?;
        value
            .map(|value| {
                value
                    .into_iter()
                    .map(|(user_id, gain)| {
                        let user_id = user_id.parse::<UserId>().map_err(|err| {
                            D::Error::custom(format!("invalid user id `{user_id}`: {err}"))
                        })?;
                        Ok((user_id, gain))
                    })
                    .collect()
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet() -> AudioPacket {
        AudioPacket {
            user_id: 12,
            target: AudioTarget::Channel(7),
            codec: Codec::Pcm16,
            seq: 42,
            timestamp: 123_456,
            payload: vec![1, 2, 3, 4],
        }
    }

    #[test]
    fn packet_round_trips() {
        let packet = packet();
        let mut encoded = Vec::new();

        packet.encode(&mut encoded).unwrap();
        let decoded = AudioPacket::decode(&encoded).unwrap();

        assert_eq!(decoded, packet);
        assert_eq!(decoded.seq, 42);
    }

    #[test]
    fn direct_packet_round_trips() {
        let packet = AudioPacket {
            user_id: 12,
            target: AudioTarget::Direct(44),
            codec: Codec::Pcm48,
            seq: 9,
            timestamp: 480,
            payload: vec![8, 9],
        };
        let mut encoded = Vec::new();

        packet.encode(&mut encoded).unwrap();
        let decoded = AudioPacket::decode(&encoded).unwrap();

        assert_eq!(decoded, packet);
    }

    #[test]
    fn mixed_packet_round_trips() {
        let packet = AudioPacket {
            user_id: SERVER_USER_ID,
            target: AudioTarget::Mixed,
            codec: Codec::Pcm48,
            seq: 9,
            timestamp: 480,
            payload: vec![8, 9],
        };
        let mut encoded = Vec::new();

        packet.encode(&mut encoded).unwrap();
        let decoded = AudioPacket::decode(&encoded).unwrap();

        assert_eq!(decoded, packet);
    }

    #[test]
    fn registration_packet_round_trips() {
        let packet = AudioPacket::registration(12, Codec::Pcm48, 7);
        let mut encoded = Vec::new();

        packet.encode(&mut encoded).unwrap();
        let decoded = AudioPacket::decode(&encoded).unwrap();

        assert_eq!(decoded, packet);
        assert!(decoded.is_registration());
    }

    #[test]
    fn rejects_invalid_magic() {
        let mut encoded = Vec::new();
        packet().encode(&mut encoded).unwrap();
        encoded[0] = b'X';

        assert_eq!(
            AudioPacket::decode(&encoded),
            Err(PacketError::InvalidMagic([b'X', b'C']))
        );
    }

    #[test]
    fn rejects_invalid_version() {
        let mut encoded = Vec::new();
        packet().encode(&mut encoded).unwrap();
        encoded[2] = 99;

        assert_eq!(
            AudioPacket::decode(&encoded),
            Err(PacketError::UnsupportedVersion(99))
        );
    }

    #[test]
    fn rejects_payload_length_mismatch() {
        let mut encoded = Vec::new();
        packet().encode(&mut encoded).unwrap();
        encoded[16] = 99;

        assert_eq!(
            AudioPacket::decode(&encoded),
            Err(PacketError::PayloadLengthMismatch {
                declared: 99,
                actual: 4
            })
        );
    }

    #[test]
    fn rejects_invalid_target_kind() {
        let mut encoded = Vec::new();
        packet().encode(&mut encoded).unwrap();
        encoded[5] = 99;

        assert_eq!(
            AudioPacket::decode(&encoded),
            Err(PacketError::UnknownTargetKind(99))
        );
    }

    #[test]
    fn pcm16_samples_round_trip() {
        let samples = [-32768, -1, 0, 1, 32767];

        let encoded = pcm16_samples_to_le_bytes(&samples);
        let decoded = pcm16_le_bytes_to_samples(&encoded).unwrap();

        assert_eq!(decoded, samples);
    }

    #[test]
    fn rejects_odd_length_pcm16_payload() {
        assert_eq!(
            pcm16_le_bytes_to_samples(&[1, 2, 3]),
            Err(PcmError::OddByteLength(3))
        );
    }

    #[test]
    fn control_codec_json_uses_wire_names_and_pcm_alias() {
        assert_eq!(serde_json::to_string(&Codec::Pcm16).unwrap(), "\"pcm16\"");
        assert_eq!(serde_json::to_string(&Codec::Pcm24).unwrap(), "\"pcm24\"");
        assert_eq!(serde_json::to_string(&Codec::Pcm48).unwrap(), "\"pcm48\"");
        assert_eq!(serde_json::to_string(&Codec::Opus).unwrap(), "\"opus\"");
        assert_eq!(
            serde_json::from_str::<Codec>("\"pcm\"").unwrap(),
            Codec::Pcm16
        );
        assert_eq!(
            serde_json::from_str::<Codec>("\"pcm-24\"").unwrap(),
            Codec::Pcm24
        );
        assert_eq!(
            serde_json::from_str::<Codec>("\"pcm48\"").unwrap(),
            Codec::Pcm48
        );
    }

    #[test]
    fn control_button_json_round_trips_and_defaults() {
        assert_eq!(
            serde_json::to_string(&TalkMode::Muted).unwrap(),
            "\"muted\""
        );
        assert_eq!(serde_json::to_string(&TalkMode::Ptt).unwrap(), "\"ptt\"");
        assert_eq!(serde_json::to_string(&TalkMode::Open).unwrap(), "\"open\"");

        let message = ControlMessage::Button {
            user_id: 7,
            button_id: "director".to_string(),
            pressed: true,
        };
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlMessage>(&json).unwrap(),
            message
        );

        let health = CaptureHealthStatus {
            runtime: None,
            audio: None,
            playback: None,
            client_transport: None,
            codec_config: Some(Esp32CodecConfig {
                chip: "es8388".to_string(),
                active_codec: Some(Codec::Pcm16),
                server_control_enabled: true,
                audio_backend: "legacy_i2s_es8388".to_string(),
                adc_input: "difference".to_string(),
                mic_pga_gain_db: 9,
                capture_channel: "left".to_string(),
                mic_software_gain_percent: 100,
                speaker_software_gain_percent: 100,
                notification_gain_percent: 50,
                high_pass_enabled: true,
                alc_enabled: true,
                noise_gate_enabled: true,
                hardware_sample_rate_hz: 48_000,
                hardware_channels: 2,
                hardware_bits_per_sample: 16,
                i2s_sample_rate_hz: 48_000,
                i2s_format: "philips".to_string(),
                i2s_slot_width: "16".to_string(),
                sidetone: Esp32SidetoneConfig {
                    mode: "off".to_string(),
                    firmware_gain_percent: 25,
                    codec_bypass_gain_percent: 25,
                    mic_bypass_gain_percent: 100,
                    active_bypass_source: "adc_p_after_mic_amp".to_string(),
                    codec_bypass_preserves_dac: true,
                },
            }),
            desktop: None,
            uptime_ms: 1234,
            wifi: Some(Esp32WifiHealthStatus {
                rssi_dbm: Some(-55),
                connect_count: 1,
                disconnect_count: 0,
                control_connect_count: 1,
                control_disconnect_count: 0,
            }),
            transport: Some(Esp32TransportHealthStatus {
                udp_rx_packets: 10,
                udp_decode_errors: 0,
                udp_codec_drops: 1,
                udp_sequence_gaps: 0,
                udp_payload_decode_errors: 0,
                udp_tx_send_failures: 0,
                audio_tx_queue_drops: 0,
                opus_encode_failures: 0,
                opus_decode_failures: 0,
            }),
            memory: Some(Esp32MemoryHealthStatus {
                free_heap_bytes: 100_000,
                min_free_heap_bytes: 90_000,
                internal_free_heap_bytes: 40_000,
                internal_largest_free_block_bytes: 20_000,
                spiram_free_heap_bytes: 500_000,
                spiram_largest_free_block_bytes: 250_000,
            }),
            task_stack_high_water_bytes: Some(Esp32TaskStackHealthStatus {
                udp: 1000,
                registration: 1000,
                playback: 1000,
                capture: 1000,
                buttons: 1000,
                display: 0,
            }),
            display: Some(Esp32DisplayHealthStatus {
                enabled: false,
                initialized: false,
                framebuffer_in_psram: false,
                framebuffer_bytes: 0,
            }),
            battery: Some(Esp32BatteryHealthStatus {
                status: "unknown".to_string(),
                present: false,
                percent: None,
                millivolts: None,
                charging: None,
            }),
            playback_queue_depth: 2,
            playback_underflows: 3,
            playback_overflows: 4,
            playback_i2s_gap_warnings: 0,
            playback_i2s_slow_warnings: 0,
            playback_i2s_short_warnings: 0,
            free_heap_bytes: 0,
            min_free_heap_bytes: 0,
            tx_target_count: 1,
            tx_packets_sent: 2,
            tx_send_failures: 0,
            adc_input: "difference".to_string(),
            mic_pga_gain_db: 9,
            capture_channel: "left".to_string(),
            software_gain_percent: 100,
            high_pass_enabled: true,
            alc_enabled: true,
            noise_gate_enabled: true,
            left: CaptureChannelHealth {
                rms: 0.10,
                peak: 0.25,
                dc_offset: 0.01,
            },
            right: CaptureChannelHealth {
                rms: 0.02,
                peak: 0.05,
                dc_offset: 0.0,
            },
            selected: CaptureChannelHealth {
                rms: 0.10,
                peak: 0.25,
                dc_offset: 0.01,
            },
            raw_clipped_samples: 0,
            software_clipped_samples: 0,
        };
        let message = ControlMessage::CaptureHealth {
            user_id: 7,
            health: health.clone(),
        };
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlMessage>(&json).unwrap(),
            message
        );
        assert_eq!(
            serde_json::from_str::<CaptureHealthStatus>(&serde_json::to_string(&health).unwrap())
                .unwrap(),
            health
        );
        let generic = CaptureHealthStatus {
            runtime: Some(ClientTelemetryRuntimeStatus {
                client_kind: "pi".to_string(),
                phase: "running".to_string(),
                last_error: None,
            }),
            playback: Some(ClientTelemetryPlaybackStatus {
                available_samples: 120,
                capacity_samples: 960,
                prebuffer_samples: 240,
                queue_depth: 120,
                channels: 2,
                started: true,
                underflows: 1,
                overflows: 2,
                dropped_samples: 3,
            }),
            client_transport: Some(ClientTelemetryTransportStatus {
                udp_rx_packets: 4,
                malformed_packets: 5,
                decode_errors: 6,
                codec_drops: 7,
                payload_decode_errors: 8,
                packet_encode_errors: 9,
                tx_packets: 10,
                tx_send_failures: 11,
                tx_queue_drops: 12,
            }),
            adc_input: "pi".to_string(),
            capture_channel: "mono".to_string(),
            ..CaptureHealthStatus::default()
        };
        assert_eq!(
            serde_json::from_str::<ClientTelemetryStatus>(
                &serde_json::to_string(&generic).unwrap()
            )
            .unwrap(),
            generic
        );
        let legacy_codec_config = serde_json::from_str::<Esp32CodecConfig>(
            r#"{
                "chip":"es8388",
                "active_codec":"pcm16",
                "server_control_enabled":true,
                "adc_input":"difference",
                "mic_pga_gain_db":9,
                "capture_channel":"left",
                "mic_software_gain_percent":100,
                "speaker_software_gain_percent":100,
                "notification_gain_percent":50,
                "high_pass_enabled":true,
                "alc_enabled":false,
                "noise_gate_enabled":false,
                "i2s_sample_rate_hz":48000,
                "sidetone":{
                    "mode":"off",
                    "firmware_gain_percent":25,
                    "codec_bypass_gain_percent":25,
                    "mic_bypass_gain_percent":100,
                    "codec_bypass_available":false
                }
            }"#,
        )
        .unwrap();
        assert_eq!(legacy_codec_config.sidetone.active_bypass_source, "");
        assert!(!legacy_codec_config.sidetone.codec_bypass_preserves_dac);

        let bridge_status = BridgeStatus {
            mode: BridgeMode::Duplex,
            input_device: Some("BlackHole 2ch".to_string()),
            output_device: Some("USB Audio".to_string()),
            input: None,
            output: None,
            input_gain: 0.75,
            output_gain: 0.5,
            tx: vec![20],
            listen: vec![30],
            note: "vMix program in, PA return out".to_string(),
        };
        let bridge_message = ControlMessage::BridgeStatus {
            user_id: 90,
            status: bridge_status.clone(),
        };
        let json = serde_json::to_string(&bridge_message).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlMessage>(&json).unwrap(),
            bridge_message
        );
        assert_eq!(
            serde_json::from_str::<BridgeStatus>(&serde_json::to_string(&bridge_status).unwrap())
                .unwrap(),
            bridge_status
        );
        let defaulted = serde_json::from_str::<BridgeStatus>(r#"{}"#).unwrap();
        assert_eq!(defaulted.mode, BridgeMode::Duplex);
        assert_eq!(defaulted.input_gain, 1.0);
        assert_eq!(defaulted.output_gain, 1.0);

        let direct = ControlMessage::DirectCall {
            user_id: 7,
            target_user_id: 8,
            active: true,
            duck: true,
        };
        let json = serde_json::to_string(&direct).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlMessage>(&json).unwrap(),
            direct
        );

        let hello =
            serde_json::from_str::<ControlMessage>(r#"{"type":"hello","user_id":7}"#).unwrap();
        assert!(matches!(
            hello,
            ControlMessage::Hello {
                user_id: 7,
                buttons,
                capabilities,
                ..
            } if buttons.is_empty() && !capabilities.advertised
        ));
        let hello = ControlMessage::Hello {
            user_id: 8,
            requested_user_id: Some(8),
            client_uid: "desktop-8".to_string(),
            codecs: vec![Codec::Opus],
            buttons: Vec::new(),
            capabilities: ClientCapabilities::desktop(),
            role: ClientRole::Client,
        };
        let json = serde_json::to_string(&hello).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlMessage>(&json).unwrap(),
            hello
        );

        let button = TalkButtonConfig {
            id: "pa".to_string(),
            label: "PA".to_string(),
            color: Some("#123abc".to_string()),
            mode: TalkButtonMode::Latching,
            actions: vec![
                TalkButtonAction::Transmit {
                    channels: vec![9],
                    users: vec![12],
                    duck: true,
                },
                TalkButtonAction::Alert {
                    targets: vec![AlertTarget::Channel(9), AlertTarget::User(12)],
                    message: Some("PA cue".to_string()),
                },
                TalkButtonAction::ApplyPreset {
                    preset_id: "game".to_string(),
                },
                TalkButtonAction::SetTalkMode {
                    users: vec![12],
                    mode: TalkMode::Muted,
                },
                TalkButtonAction::RouteEdit {
                    users: vec![12],
                    listen_add: vec![1],
                    listen_remove: vec![2],
                    listen_toggle: vec![3],
                    tx_add: vec![4],
                    tx_remove: vec![5],
                    tx_toggle: vec![6],
                },
            ],
        };
        let json = serde_json::to_string(&button).unwrap();
        assert_eq!(
            serde_json::from_str::<TalkButtonConfig>(&json).unwrap(),
            button
        );
        assert!(serde_json::from_str::<TalkButtonConfig>(
            r#"{"id":"old","label":"Old","mode":"momentary","tx":[1]}"#
        )
        .is_err());
    }

    #[test]
    fn alert_control_json_round_trips() {
        let alert = ControlMessage::SendAlert {
            user_id: 4,
            target: AlertTarget::User(9),
            message: Some("Call me".to_string()),
        };
        let json = serde_json::to_string(&alert).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlMessage>(&json).unwrap(),
            alert
        );

        let status = AlertStatus {
            id: 11,
            sender: 4,
            sender_name: Some("Director".to_string()),
            target: AlertTarget::Channel(2),
            message: None,
            created_at_ms: 1234,
            recipients: vec![AlertRecipientStatus {
                user_id: 9,
                acked_at_ms: None,
            }],
            cancelled: false,
            cancelled_at_ms: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(serde_json::from_str::<AlertStatus>(&json).unwrap(), status);
        let legacy = serde_json::from_str::<AlertStatus>(
            r#"{"id":12,"sender":4,"target":{"kind":"channel","id":2},"created_at_ms":1234,"recipients":[],"cancelled":false}"#,
        )
        .unwrap();
        assert_eq!(legacy.sender_name, None);
        assert!(status.active_for(9));
        assert!(!status.active_for(10));
    }

    #[test]
    fn ifb_config_json_defaults_and_round_trips() {
        let defaulted = serde_json::from_str::<IfbConfig>(r#"{}"#).unwrap();
        assert_eq!(defaulted, IfbConfig::default());

        let ifb = IfbConfig {
            enabled: true,
            program: vec![1, 2],
            interrupt: vec![9],
            duck_gain: 0.125,
        };
        let json = serde_json::to_string(&ifb).unwrap();
        assert_eq!(serde_json::from_str::<IfbConfig>(&json).unwrap(), ifb);
    }

    #[test]
    fn lockout_policy_json_defaults_and_round_trips() {
        let defaulted = serde_json::from_str::<ClientLockoutPolicy>(r#"{}"#).unwrap();
        assert_eq!(defaulted, ClientLockoutPolicy::default());

        let policy = ClientLockoutPolicy {
            allow_codec: false,
            allow_local_api: false,
            ..ClientLockoutPolicy::default()
        };
        let json = serde_json::to_string(&policy).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientLockoutPolicy>(&json).unwrap(),
            policy
        );
    }

    #[test]
    fn stereo_config_json_defaults_and_round_trips() {
        let defaulted = serde_json::from_str::<StereoConfig>(r#"{}"#).unwrap();
        assert_eq!(defaulted, StereoConfig::default());

        let stereo = StereoConfig {
            enabled: true,
            channel_pan: [(1, -1.0), (2, 1.0)].into(),
        };
        let json = serde_json::to_string(&stereo).unwrap();
        assert_eq!(serde_json::from_str::<StereoConfig>(&json).unwrap(), stereo);
        assert!(stereo.active_for_codec(Codec::Pcm48));
        assert!(stereo.active_for_codec(Codec::Opus));
        assert!(!stereo.active_for_codec(Codec::Pcm16));
    }

    #[test]
    fn processing_and_presence_json_round_trip() {
        let processing = ProcessingConfig::default();
        let json = serde_json::to_string(&processing).unwrap();
        assert_eq!(
            serde_json::from_str::<ProcessingConfig>(&json).unwrap(),
            processing
        );
        assert!(json.contains("\"engine\":\"built_in\""));

        let webrtc = serde_json::from_str::<ProcessingConfig>(r#"{"engine":"webrtc"}"#).unwrap();
        assert_eq!(webrtc.engine, ProcessingEngine::WebRtc);
        let normalized = serde_json::from_str::<ProcessingConfig>(
            r#"{"normalization":{"enabled":true,"target_rms":0.12,"max_boost":6.0,"max_attenuation":10.0,"adaptation_ms":180,"noise_floor_rms":0.01}}"#,
        )
        .unwrap();
        assert!(normalized.normalization.enabled);
        assert_eq!(normalized.normalization.target_rms, 0.12);
        assert_eq!(normalized.normalization.max_boost, 6.0);
        assert_eq!(normalized.normalization.max_attenuation, 10.0);
        assert_eq!(normalized.normalization.adaptation_ms, 180);
        assert_eq!(normalized.normalization.noise_floor_rms, 0.01);
        let pipeline = serde_json::from_str::<ProcessingConfig>(
            r#"{"pipeline":[{"engine":"webrtc"},{"engine":"rnnoise","enabled":false}]}"#,
        )
        .unwrap();
        assert_eq!(
            pipeline.pipeline,
            vec![
                ProcessingStageConfig {
                    engine: ProcessingEngine::WebRtc,
                    enabled: true,
                },
                ProcessingStageConfig {
                    engine: ProcessingEngine::RnNoise,
                    enabled: false,
                }
            ]
        );
        let rnnoise = serde_json::from_str::<ProcessingConfig>(r#"{"engine":"rnnoise"}"#).unwrap();
        assert_eq!(rnnoise.engine, ProcessingEngine::RnNoise);
        let deep_filter = serde_json::from_str::<ProcessingConfig>(
            r#"{"engine":"deepfilternet","deep_filter_backend":"coreml","apple_compute_units":"cpu_and_gpu"}"#,
        )
        .unwrap();
        assert_eq!(deep_filter.engine, ProcessingEngine::DeepFilterNet);
        assert_eq!(deep_filter.deep_filter_backend, DeepFilterBackend::CoreMl);
        assert_eq!(
            deep_filter.apple_compute_units,
            AppleComputeUnits::CpuAndGpu
        );

        let esp32_audio = Esp32AudioConfig {
            enabled: true,
            adc_input: Esp32AdcInput::Difference,
            mic_pga_gain_db: 9,
            capture_channel: Esp32CaptureChannel::Left,
            mic_software_gain_percent: 90,
            speaker_software_gain_percent: 120,
            notification_gain_percent: 40,
            high_pass_enabled: true,
            alc_enabled: true,
            noise_gate_enabled: true,
            sidetone: Esp32SidetoneControlConfig {
                mode: Esp32SidetoneMode::Firmware,
                firmware_gain_percent: 20,
                codec_bypass_gain_percent: 25,
                mic_bypass_gain_percent: 100,
            },
        };
        let json = serde_json::to_string(&esp32_audio).unwrap();
        assert_eq!(
            serde_json::from_str::<Esp32AudioConfig>(&json).unwrap(),
            esp32_audio
        );

        let roster = ChannelPresenceRoster {
            channel_id: 2,
            name: Some("Program".to_string()),
            members: vec![ChannelPresenceMember {
                user_id: 7,
                name: "Ref".to_string(),
                present: true,
                transmitting: true,
            }],
        };
        let event = ControlEvent::PresenceUpdate {
            user_id: 1,
            client_uid: "test-client".to_string(),
            channels: vec![roster],
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<ControlEvent>(&json).unwrap(), event);
    }

    #[test]
    fn opus_profiles_have_distinct_operator_settings() {
        assert_eq!(OpusProfile::default(), OpusProfile::Speech24Standard);
        assert!(
            OpusProfile::Speech16Low.bitrate_bps(1) < OpusProfile::Speech24Standard.bitrate_bps(1)
        );
        assert!(
            OpusProfile::Speech48High.bitrate_bps(1) > OpusProfile::Speech24Standard.bitrate_bps(1)
        );
        assert_eq!(
            OpusProfile::Speech24Standard.bitrate_bps(2),
            OpusProfile::Speech24Standard.bitrate_bps(1) * OPUS_STEREO_BITRATE_MULTIPLIER
        );
        assert_eq!(OpusProfile::Speech16Low.sample_rate_hz(), SAMPLE_RATE);
        assert_eq!(
            OpusProfile::Speech24Standard.sample_rate_hz(),
            PCM24_SAMPLE_RATE
        );
        assert_eq!(
            OpusProfile::Speech48High.sample_rate_hz(),
            PCM48_SAMPLE_RATE
        );
        assert!(OpusProfile::Music48.is_music());
    }

    #[test]
    fn codec_frame_sizes_match_rates() {
        assert_eq!(codec_samples_per_frame(Codec::Pcm16), 160);
        assert_eq!(codec_samples_per_frame(Codec::Pcm24), 240);
        assert_eq!(OpusProfile::Speech16Low.samples_per_frame(), 160);
        assert_eq!(OpusProfile::Speech24Standard.samples_per_frame(), 240);
        assert_eq!(OpusProfile::Speech48High.samples_per_frame(), 480);
        assert_eq!(codec_samples_per_frame(Codec::Opus), 240);
        assert_eq!(codec_samples_per_frame(Codec::Pcm48), 480);
        assert_eq!(codec_pcm16_payload_bytes(Codec::Pcm24), 480);
        assert_eq!(codec_pcm16_payload_bytes(Codec::Pcm48), 960);
        let max_payload_len = MAX_PAYLOAD_LEN;
        let stereo_payload_bytes = PCM48_STEREO_PAYLOAD_BYTES;
        let max_packet_bytes = MAX_PACKET_BYTES;
        assert!(max_payload_len >= stereo_payload_bytes);
        assert_eq!(max_packet_bytes, HEADER_LEN + max_payload_len);
    }

    #[test]
    fn resample_linear_changes_length() {
        let samples = [0, 100, 200];
        let upsampled = resample_linear_to_len(&samples, 5);
        assert_eq!(upsampled, vec![0, 50, 100, 150, 200]);
    }

    #[test]
    fn resample_audio_produces_expected_frame_lengths() {
        let narrow = vec![1_000; SAMPLES_PER_FRAME];
        let wide = resample_audio(&narrow, SAMPLE_RATE, PCM48_SAMPLE_RATE).unwrap();
        assert_eq!(wide.len(), PCM48_SAMPLES_PER_FRAME);

        let narrow_again = resample_audio(&wide, PCM48_SAMPLE_RATE, SAMPLE_RATE).unwrap();
        assert_eq!(narrow_again.len(), SAMPLES_PER_FRAME);
    }
}
