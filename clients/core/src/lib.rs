use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::ErrorKind;
use std::net::{Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use common::{
    codec_pcm16_payload_bytes, codec_sample_rate, pcm16_le_bytes_to_samples,
    pcm16_samples_to_le_bytes, AlertStatus, AudioTarget, ButtonCapability, ButtonId,
    ChannelPresenceRoster, ClientLockoutPolicy, ClientRole, ClientUid, Codec, ControlEvent,
    ControlMessage, ControlResponse, DirectCallHistoryEntry, DirectCallStatus, EmergencyStatus,
    Esp32AudioConfig, IfbConfig, OpusBandwidth, OpusProfile, ProcessingConfig, StereoConfig,
    TalkButtonAction, TalkButtonConfig, TalkMode, MIX_SAMPLES_PER_FRAME, MIX_SAMPLE_RATE,
};
use futures_util::{SinkExt, StreamExt};
use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Async, FixedAsync, PolynomialDegree, Resampler};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

pub mod ui;
pub use ui::{
    AlertRequest, ClientAudioBackend, ClientAudioBackendKind, ClientControlApi,
    ClientInputChannelMode, ClientRuntimeConfig, ClientRuntimePhase, ClientRuntimeStatus,
    CodecRequest, FullConfigRequest, GainRequest, InputBackendState, MacosMicrophoneModeStatus,
    OkResponse, StateResponse, TalkModeRequest,
};

pub const CONTROL_RECONNECT_INITIAL: Duration = Duration::from_millis(500);
pub const CONTROL_RECONNECT_MAX: Duration = Duration::from_secs(5);
pub const DEFAULT_ADMIN_PORT: u16 = 40002;
pub const DEFAULT_AUDIO_PORT: u16 = 40000;
pub const DEFAULT_CLIENT_BUTTON_COUNT: u16 = 6;
pub const DEFAULT_CONTROL_PORT: u16 = 40001;
pub const DEFAULT_SERVER_HOST: &str = "127.0.0.1";
pub const LOCAL_BIND_PORT_FALLBACK_ATTEMPTS: u16 = 100;
pub const MAX_CLIENT_BUTTON_COUNT: u16 = 24;
const RECONNECTING_CUE_REPEAT: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientServerEndpoint {
    pub host: String,
}

impl Default for ClientServerEndpoint {
    fn default() -> Self {
        Self {
            host: DEFAULT_SERVER_HOST.to_string(),
        }
    }
}

impl ClientServerEndpoint {
    pub fn new(host: impl AsRef<str>) -> anyhow::Result<Self> {
        Ok(Self {
            host: validate_server_host(host.as_ref()).map_err(anyhow::Error::msg)?,
        })
    }

    pub fn audio_addr_string(&self) -> String {
        format!("{}:{DEFAULT_AUDIO_PORT}", host_for_socket(&self.host))
    }

    pub fn resolve_audio_addr(&self) -> anyhow::Result<SocketAddr> {
        resolve_server_audio_addr(&self.host)
    }

    pub fn control_url(&self) -> String {
        format!("ws://{}:{DEFAULT_CONTROL_PORT}", host_for_url(&self.host))
    }

    pub fn admin_url(&self) -> String {
        format!("http://{}:{DEFAULT_ADMIN_PORT}", host_for_url(&self.host))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientEndpointOverrides {
    pub server_host: String,
    pub server: SocketAddr,
    pub control: String,
    pub admin: Option<String>,
    pub advanced_endpoints: bool,
}

impl Default for ClientEndpointOverrides {
    fn default() -> Self {
        let endpoint = ClientServerEndpoint::default();
        Self {
            server_host: endpoint.host.clone(),
            server: default_audio_addr(),
            control: endpoint.control_url(),
            admin: None,
            advanced_endpoints: false,
        }
    }
}

impl ClientEndpointOverrides {
    pub fn normalized(
        server_host: impl AsRef<str>,
        server: SocketAddr,
        control: impl AsRef<str>,
        admin: Option<String>,
        advanced_endpoints: bool,
    ) -> Self {
        let mut config = Self {
            server_host: server_host.as_ref().trim().to_string(),
            server,
            control: control.as_ref().trim().to_string(),
            admin: admin
                .map(|admin| admin.trim().to_string())
                .filter(|admin| !admin.is_empty()),
            advanced_endpoints,
        };
        normalize_endpoint_overrides(&mut config);
        config
    }

    pub fn endpoint(&self) -> anyhow::Result<ClientServerEndpoint> {
        ClientServerEndpoint::new(&self.server_host)
    }

    pub fn effective_server(&self) -> anyhow::Result<SocketAddr> {
        if self.advanced_endpoints {
            Ok(self.server)
        } else {
            self.endpoint()?.resolve_audio_addr()
        }
    }

    pub fn effective_control(&self) -> anyhow::Result<String> {
        if self.advanced_endpoints {
            validate_control_url(&self.control)?;
            Ok(self.control.clone())
        } else {
            Ok(self.endpoint()?.control_url())
        }
    }

    pub fn effective_admin(&self) -> anyhow::Result<String> {
        if self.advanced_endpoints {
            if let Some(admin) = self
                .admin
                .as_deref()
                .filter(|admin| !admin.trim().is_empty())
            {
                return Ok(admin.trim().to_string());
            }
            derive_admin_url_from_control(&self.control)
                .ok_or_else(|| anyhow!("admin URL is empty and control URL is not derivable"))
        } else {
            Ok(self.endpoint()?.admin_url())
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let _ = self.endpoint()?;
        if self.advanced_endpoints {
            validate_control_url(&self.control)?;
            if let Some(admin) = self
                .admin
                .as_deref()
                .filter(|admin| !admin.trim().is_empty())
            {
                validate_http_url(admin)?;
            }
        }
        Ok(())
    }

    pub fn sync_default_endpoints(&mut self) -> anyhow::Result<()> {
        let endpoint = self.endpoint()?;
        self.server = endpoint.resolve_audio_addr()?;
        self.control = endpoint.control_url();
        if !self.advanced_endpoints {
            self.admin = None;
        }
        Ok(())
    }
}

pub fn default_audio_addr() -> SocketAddr {
    ([127, 0, 0, 1], DEFAULT_AUDIO_PORT).into()
}

pub fn validate_server_host(host: &str) -> Result<String, String> {
    let host = host.trim();
    if host.is_empty() {
        return Err("server host cannot be empty".to_string());
    }
    if host.contains("://") {
        return Err("server host must not include a scheme".to_string());
    }
    if host.contains('/') || host.contains('?') || host.contains('#') || host.contains('@') {
        return Err(
            "server host must not include a path, query, fragment, or userinfo".to_string(),
        );
    }
    if host.chars().any(char::is_whitespace) {
        return Err("server host must not contain whitespace".to_string());
    }
    if host.starts_with('[') || host.ends_with(']') {
        if !(host.starts_with('[') && host.ends_with(']')) {
            return Err("IPv6 server host brackets are not balanced".to_string());
        }
        let inner = &host[1..host.len() - 1];
        inner
            .parse::<Ipv6Addr>()
            .map_err(|_| "bracketed server host must be an IPv6 address".to_string())?;
        return Ok(inner.to_string());
    }
    if let Some((name, port)) = host.rsplit_once(':') {
        if !name.contains(':') && !name.is_empty() && port.parse::<u16>().is_ok() {
            return Err("server host must not include a port; use Advanced endpoints".to_string());
        }
    }
    if host.contains(':') {
        host.parse::<Ipv6Addr>()
            .map_err(|_| "server host contains ':' but is not a valid IPv6 address".to_string())?;
    }
    Ok(host.to_string())
}

pub fn resolve_server_audio_addr(host: &str) -> anyhow::Result<SocketAddr> {
    let host = validate_server_host(host).map_err(anyhow::Error::msg)?;
    (host.as_str(), DEFAULT_AUDIO_PORT)
        .to_socket_addrs()
        .with_context(|| format!("resolve server host `{host}`"))?
        .next()
        .ok_or_else(|| anyhow!("server host `{host}` did not resolve"))
}

pub fn host_for_url(host: &str) -> String {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

pub fn host_for_socket(host: &str) -> String {
    host_for_url(host)
}

pub fn derive_server_host(
    server: SocketAddr,
    control: Option<&str>,
    admin: Option<&str>,
) -> Option<String> {
    let server_host = server.ip().to_string();
    let server_default = server.port() == DEFAULT_AUDIO_PORT;
    let control_host = control.and_then(|control| parse_url_host_port(control, "ws"));
    let admin_host = admin.and_then(|admin| parse_url_host_port(admin, "http"));
    if server_default
        && control_host
            .as_ref()
            .is_none_or(|(host, port)| *port == DEFAULT_CONTROL_PORT && host == &server_host)
        && admin_host
            .as_ref()
            .is_none_or(|(host, port)| *port == DEFAULT_ADMIN_PORT && host == &server_host)
    {
        return Some(server_host);
    }
    None
}

pub fn endpoint_fields_are_default(server_host: &str, server: SocketAddr, control: &str) -> bool {
    let Ok(endpoint) = ClientServerEndpoint::new(server_host) else {
        return false;
    };
    server.port() == DEFAULT_AUDIO_PORT
        && server.ip().to_string() == endpoint.host
        && control.trim() == endpoint.control_url()
}

pub fn normalize_endpoint_overrides(config: &mut ClientEndpointOverrides) {
    let server_host_was_empty = config.server_host.trim().is_empty();
    config.control = config.control.trim().to_string();
    config.admin = config
        .admin
        .take()
        .map(|admin| admin.trim().to_string())
        .filter(|admin| !admin.is_empty());

    if server_host_was_empty {
        config.server_host = derive_server_host(
            config.server,
            Some(config.control.as_str()),
            config.admin.as_deref(),
        )
        .unwrap_or_else(|| DEFAULT_SERVER_HOST.to_string());
    }
    if let Ok(host) = validate_server_host(&config.server_host) {
        config.server_host = host;
    }
    let matches_default_ports =
        endpoint_fields_are_default(&config.server_host, config.server, config.control.as_str());
    let global_default_endpoints = config.server == default_audio_addr()
        && config.control == format!("ws://{DEFAULT_SERVER_HOST}:{DEFAULT_CONTROL_PORT}");
    if !config.advanced_endpoints {
        if server_host_was_empty
            || (config.server_host == DEFAULT_SERVER_HOST && !global_default_endpoints)
        {
            if let Some(host) = derive_server_host(
                config.server,
                Some(config.control.as_str()),
                config.admin.as_deref(),
            ) {
                config.server_host = host;
            } else if !matches_default_ports && !global_default_endpoints {
                config.advanced_endpoints = true;
            }
        } else if !matches_default_ports && !global_default_endpoints {
            config.advanced_endpoints = true;
        }
    }
    if !config.advanced_endpoints {
        if let Ok(endpoint) = ClientServerEndpoint::new(&config.server_host) {
            if let Ok(server) = endpoint.resolve_audio_addr() {
                config.server = server;
            }
            config.control = endpoint.control_url();
            config.admin = None;
        }
    }
}

pub fn validate_control_url(control: &str) -> anyhow::Result<()> {
    let control = control.trim();
    if control.is_empty() {
        bail!("control URL cannot be empty");
    }
    if !(control.starts_with("ws://") || control.starts_with("wss://")) {
        bail!("control URL must start with ws:// or wss://");
    }
    Ok(())
}

pub fn validate_http_url(url: &str) -> anyhow::Result<()> {
    let url = url.trim();
    if url.is_empty() {
        bail!("URL cannot be empty");
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        bail!("URL must start with http:// or https://");
    }
    Ok(())
}

pub fn derive_admin_url_from_control(control: &str) -> Option<String> {
    let (host, _) = parse_url_host_port(control, "ws")?;
    Some(format!(
        "http://{}:{DEFAULT_ADMIN_PORT}",
        host_for_url(&host)
    ))
}

fn parse_url_host_port(url: &str, scheme_prefix: &str) -> Option<(String, u16)> {
    let url = url.trim();
    let scheme = format!("{scheme_prefix}://");
    let rest = if let Some(rest) = url.strip_prefix(&scheme) {
        rest
    } else {
        let secure_scheme = format!("{scheme_prefix}s://");
        url.strip_prefix(&secure_scheme)?
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|authority| !authority.is_empty())?;
    if authority.starts_with('[') {
        let end = authority.find(']')?;
        let host = &authority[1..end];
        let port = authority
            .get(end + 1..)
            .and_then(|suffix| suffix.strip_prefix(':'))
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or_else(|| {
                if scheme_prefix == "ws" {
                    DEFAULT_CONTROL_PORT
                } else {
                    DEFAULT_ADMIN_PORT
                }
            });
        return Some((host.to_string(), port));
    }
    let (host, port) = authority.rsplit_once(':').map_or_else(
        || {
            (
                authority,
                if scheme_prefix == "ws" {
                    DEFAULT_CONTROL_PORT
                } else {
                    DEFAULT_ADMIN_PORT
                },
            )
        },
        |(host, port)| (host, port.parse::<u16>().ok().unwrap_or(0)),
    );
    if host.is_empty() || port == 0 {
        None
    } else {
        Some((host.to_string(), port))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientConnectionEvent {
    Connected,
    Disconnected,
    Reconnecting,
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub user_id: u16,
    pub client_uid: ClientUid,
    pub role: ClientRole,
    pub name: String,
    pub listen: Vec<u16>,
    pub tx: Vec<u16>,
    pub codec: Codec,
    pub opus_profile: OpusProfile,
    pub talk_mode: TalkMode,
    pub last_non_muted_talk_mode: TalkMode,
    pub regular_talk_active: bool,
    pub priority: bool,
    pub priority_channels: Vec<u16>,
    pub processing: ProcessingConfig,
    pub channel_rosters: Vec<ChannelPresenceRoster>,
    pub emergency: Option<EmergencyStatus>,
    pub vol: HashMap<u16, f32>,
    pub talker_vol: HashMap<u16, f32>,
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
    pub esp32_audio: Esp32AudioConfig,
}

impl ClientConfig {
    pub fn set_talk_mode(&mut self, talk_mode: TalkMode) {
        self.talk_mode = talk_mode;
        if talk_mode != TalkMode::Muted {
            self.last_non_muted_talk_mode = talk_mode;
        }
    }

    pub fn restored_unmute_talk_mode(&self) -> TalkMode {
        if self.last_non_muted_talk_mode == TalkMode::Muted {
            TalkMode::Ptt
        } else {
            self.last_non_muted_talk_mode
        }
    }

    pub fn active_tx_channels(&self) -> Vec<u16> {
        let mut channels = HashSet::new();
        if self.talk_mode == TalkMode::Open
            || (self.talk_mode == TalkMode::Ptt && self.regular_talk_active)
        {
            channels.extend(self.tx.iter().copied());
        }
        for button in &self.buttons {
            if self.active_buttons.contains(&button.id) {
                for action in &button.actions {
                    if let TalkButtonAction::Transmit { channels: tx, .. } = action {
                        channels.extend(tx.iter().copied());
                    }
                }
            }
        }
        let mut channels = channels.into_iter().collect::<Vec<_>>();
        channels.sort_unstable();
        channels
    }

    pub fn active_tx_targets(&self) -> Vec<AudioTarget> {
        let mut targets = self
            .active_tx_channels()
            .into_iter()
            .map(AudioTarget::Channel)
            .collect::<HashSet<_>>();
        targets.extend(
            self.active_direct_calls
                .iter()
                .filter(|call| call.caller == self.user_id && call.active)
                .map(|call| AudioTarget::Direct(call.target)),
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
        if self
            .emergency
            .as_ref()
            .is_some_and(|emergency| emergency.active && emergency.source == self.user_id)
        {
            targets.insert(AudioTarget::Mixed);
        }
        let mut targets = targets.into_iter().collect::<Vec<_>>();
        targets.sort_by_key(|target| match target {
            AudioTarget::Channel(id) => (0, *id),
            AudioTarget::Direct(id) => (1, *id),
            AudioTarget::Mixed => (2, 0),
        });
        targets
    }

    pub fn button_known(&self, button_id: &str) -> bool {
        self.buttons.iter().any(|button| button.id == button_id)
            || self
                .advertised_buttons
                .iter()
                .any(|button| button.id == button_id)
    }

    pub fn control_message(&self) -> ControlMessage {
        ControlMessage::Config {
            user_id: self.user_id,
            role: Some(self.role),
            name: (!self.name.trim().is_empty()).then(|| self.name.clone()),
            listen: self.listen.clone(),
            tx: self.tx.clone(),
            vol: self.vol.clone(),
            talker_vol: None,
            codec: Some(self.codec),
            opus_profile: Some(self.opus_profile),
            talk_mode: Some(self.talk_mode),
            priority: None,
            priority_channels: None,
            processing: None,
            buttons: None,
            ifb: None,
            stereo: None,
            esp32_audio: None,
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PersistedClientIdentity {
    client_uid: String,
}

pub fn load_or_create_client_uid(
    override_uid: Option<&str>,
    identity_file: Option<&std::path::Path>,
) -> anyhow::Result<String> {
    if let Some(uid) = override_uid.map(str::trim).filter(|uid| !uid.is_empty()) {
        return Ok(uid.to_string());
    }

    let path = identity_file
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(default_client_identity_path);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let identity: PersistedClientIdentity = serde_json::from_str(&text)
                .with_context(|| format!("parse client identity {}", path.display()))?;
            if identity.client_uid.trim().is_empty() {
                bail!("client identity {} has an empty client_uid", path.display());
            }
            Ok(identity.client_uid)
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("create client identity directory {}", parent.display())
                })?;
            }
            let identity = PersistedClientIdentity {
                client_uid: uuid::Uuid::new_v4().to_string(),
            };
            let json = serde_json::to_string_pretty(&identity)?;
            std::fs::write(&path, format!("{json}\n"))
                .with_context(|| format!("write client identity {}", path.display()))?;
            Ok(identity.client_uid)
        }
        Err(err) => Err(err).with_context(|| format!("read client identity {}", path.display())),
    }
}

pub fn default_client_identity_path() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return std::path::PathBuf::from(appdata)
                .join("Intercom Suite")
                .join("client-identity.json");
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Intercom Suite")
                .join("client-identity.json");
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return std::path::PathBuf::from(xdg)
                .join("intercom-suite")
                .join("client-identity.json");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home)
                .join(".config")
                .join("intercom-suite")
                .join("client-identity.json");
        }
    }

    std::path::PathBuf::from("intercom-client-identity.json")
}

#[derive(Debug)]
pub struct AudioSettings {
    mic_gain: AtomicU32,
    speaker_gain: AtomicU32,
}

impl AudioSettings {
    pub fn new(mic_gain: f32, speaker_gain: f32) -> Self {
        Self {
            mic_gain: AtomicU32::new(clamp_runtime_gain(mic_gain).to_bits()),
            speaker_gain: AtomicU32::new(clamp_runtime_gain(speaker_gain).to_bits()),
        }
    }

    pub fn mic_gain(&self) -> f32 {
        f32::from_bits(self.mic_gain.load(Ordering::Relaxed))
    }

    pub fn speaker_gain(&self) -> f32 {
        f32::from_bits(self.speaker_gain.load(Ordering::Relaxed))
    }

    pub fn set_mic_gain(&self, gain: f32) {
        self.mic_gain
            .store(clamp_runtime_gain(gain).to_bits(), Ordering::Relaxed);
    }

    pub fn set_speaker_gain(&self, gain: f32) {
        self.speaker_gain
            .store(clamp_runtime_gain(gain).to_bits(), Ordering::Relaxed);
    }
}

pub fn clamp_runtime_gain(gain: f32) -> f32 {
    if gain.is_finite() {
        gain.clamp(0.0, 8.0)
    } else {
        1.0
    }
}

pub async fn bind_tcp_listener_with_port_fallback(
    requested: SocketAddr,
) -> anyhow::Result<(TcpListener, SocketAddr)> {
    let mut addr = requested;
    let mut last_addr = requested;
    let mut last_error = None;

    for _ in 0..=LOCAL_BIND_PORT_FALLBACK_ATTEMPTS {
        match TcpListener::bind(addr).await {
            Ok(listener) => return Ok((listener, addr)),
            Err(err)
                if err.kind() == ErrorKind::AddrInUse
                    && requested.port() != 0
                    && addr.port() < u16::MAX =>
            {
                last_addr = addr;
                last_error = Some(err);
                addr.set_port(addr.port() + 1);
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("bind TCP listener at requested address {requested}")
                });
            }
        }
    }

    Err(last_error.expect("fallback loop records the address-in-use error")).with_context(|| {
        format!("bind TCP listener near {requested}; last attempted address was {last_addr}")
    })
}

pub struct AudioEncoder {
    codec: Codec,
    opus: Option<opus::Encoder>,
    edge_resampler: FrameResampler,
}

impl AudioEncoder {
    pub fn new(codec: Codec, opus_profile: OpusProfile) -> anyhow::Result<Self> {
        let edge_rate = if codec == Codec::Opus {
            opus_profile.sample_rate_hz()
        } else {
            codec_sample_rate(codec)
        };
        let edge_resampler = FrameResampler::new(MIX_SAMPLE_RATE, edge_rate)?;
        let opus = if codec == Codec::Opus {
            let mut encoder = opus::Encoder::new(
                opus_profile.sample_rate_hz(),
                opus::Channels::Mono,
                opus_application(opus_profile),
            )?;
            configure_opus_encoder(&mut encoder, opus_profile, 1)?;
            Some(encoder)
        } else {
            None
        };

        Ok(Self {
            codec,
            opus,
            edge_resampler,
        })
    }

    pub fn encode(&mut self, samples: &[i16]) -> anyhow::Result<Vec<u8>> {
        if samples.len() != MIX_SAMPLES_PER_FRAME {
            bail!(
                "unexpected sample count: got {}, expected {}",
                samples.len(),
                MIX_SAMPLES_PER_FRAME
            );
        }

        match self.codec {
            Codec::Pcm16 | Codec::Pcm24 | Codec::Pcm48 => {
                let output = self.edge_resampler.process(samples)?;
                Ok(pcm16_samples_to_le_bytes(&output))
            }
            Codec::Opus => self.encode_opus(samples),
            Codec::Adpcm => bail!("ADPCM is not implemented"),
        }
    }

    fn encode_opus(&mut self, samples: &[i16]) -> anyhow::Result<Vec<u8>> {
        let samples = self.edge_resampler.process(samples)?;
        let encoder = self
            .opus
            .as_mut()
            .context("Opus encoder was not initialized")?;
        let mut payload = vec![0_u8; common::OPUS_MAX_PAYLOAD_BYTES];
        let len = encoder.encode(&samples, &mut payload)?;
        payload.truncate(len);
        Ok(payload)
    }
}

fn opus_application(profile: OpusProfile) -> opus::Application {
    if profile.is_music() {
        opus::Application::Audio
    } else {
        opus::Application::Voip
    }
}

fn opus_channels(channels: usize) -> opus::Channels {
    if channels > 1 {
        opus::Channels::Stereo
    } else {
        opus::Channels::Mono
    }
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

#[derive(Default)]
pub struct AudioDecoder {
    opus_mono: Option<opus::Decoder>,
    opus_stereo: Option<opus::Decoder>,
    opus_mono_profile: Option<OpusProfile>,
    opus_stereo_profile: Option<OpusProfile>,
    resamplers: HashMap<u32, FrameResampler>,
    stereo_resamplers: HashMap<(u32, usize), Vec<FrameResampler>>,
}

impl AudioDecoder {
    pub fn decode(&mut self, codec: Codec, payload: &[u8]) -> anyhow::Result<Vec<i16>> {
        self.decode_with_channels(codec, OpusProfile::default(), payload, 1)
    }

    pub fn decode_with_channels(
        &mut self,
        codec: Codec,
        opus_profile: OpusProfile,
        payload: &[u8],
        channels: usize,
    ) -> anyhow::Result<Vec<i16>> {
        match codec {
            Codec::Pcm16 | Codec::Pcm24 | Codec::Pcm48 => {
                let channels = channels.max(1);
                if channels > 1 && codec != Codec::Pcm48 {
                    bail!("stereo receive is only supported for pcm48 and opus");
                }
                let expected_len = codec_pcm16_payload_bytes(codec) * channels;
                if payload.len() != expected_len {
                    bail!(
                        "unexpected PCM16 payload length: got {}, expected {}",
                        payload.len(),
                        expected_len
                    );
                }
                let samples = pcm16_le_bytes_to_samples(payload)?;
                if channels == 2 && codec == Codec::Pcm48 {
                    Ok(samples)
                } else {
                    self.resample_to_mix(codec_sample_rate(codec), &samples)
                }
            }
            Codec::Opus => self.decode_opus(opus_profile, payload, channels),
            Codec::Adpcm => bail!("ADPCM is not implemented"),
        }
    }

    fn decode_opus(
        &mut self,
        profile: OpusProfile,
        payload: &[u8],
        channels: usize,
    ) -> anyhow::Result<Vec<i16>> {
        let channels = channels.max(1);
        let needs_new = if channels > 1 {
            self.opus_stereo_profile != Some(profile) || self.opus_stereo.is_none()
        } else {
            self.opus_mono_profile != Some(profile) || self.opus_mono.is_none()
        };
        if needs_new {
            let decoder = opus::Decoder::new(profile.sample_rate_hz(), opus_channels(channels))?;
            if channels > 1 {
                self.opus_stereo = Some(decoder);
                self.opus_stereo_profile = Some(profile);
            } else {
                self.opus_mono = Some(decoder);
                self.opus_mono_profile = Some(profile);
            }
        }
        let decoder = if channels > 1 {
            self.opus_stereo
                .as_mut()
                .context("Opus stereo decoder was not initialized")?
        } else {
            self.opus_mono
                .as_mut()
                .context("Opus mono decoder was not initialized")?
        };
        let frame_samples = profile.samples_per_frame();
        let mut samples = vec![0_i16; frame_samples * channels];
        let decoded = decoder.decode(payload, &mut samples, false)?;
        samples.truncate(decoded * channels);

        if samples.len() != frame_samples * channels {
            bail!(
                "unexpected Opus frame sample count: got {}, expected {}",
                samples.len(),
                frame_samples * channels
            );
        }

        if channels > 1 {
            self.resample_interleaved_to_mix(profile.sample_rate_hz(), &samples, channels)
        } else {
            self.resample_to_mix(profile.sample_rate_hz(), &samples)
        }
    }

    fn resample_to_mix(&mut self, from_rate: u32, samples: &[i16]) -> anyhow::Result<Vec<i16>> {
        if let Entry::Vacant(entry) = self.resamplers.entry(from_rate) {
            entry.insert(FrameResampler::new(from_rate, MIX_SAMPLE_RATE)?);
        }
        self.resamplers
            .get_mut(&from_rate)
            .context("decoder resampler was not initialized")?
            .process(samples)
    }

    fn resample_interleaved_to_mix(
        &mut self,
        from_rate: u32,
        samples: &[i16],
        channels: usize,
    ) -> anyhow::Result<Vec<i16>> {
        if from_rate == MIX_SAMPLE_RATE {
            return Ok(samples.to_vec());
        }
        let source_frame_len = frame_len_for_rate(from_rate);
        if samples.len() != source_frame_len * channels {
            bail!(
                "unexpected interleaved sample count: got {}, expected {}",
                samples.len(),
                source_frame_len * channels
            );
        }
        let key = (from_rate, channels);
        if let Entry::Vacant(entry) = self.stereo_resamplers.entry(key) {
            let mut resamplers = Vec::with_capacity(channels);
            for _ in 0..channels {
                resamplers.push(FrameResampler::new(from_rate, MIX_SAMPLE_RATE)?);
            }
            entry.insert(resamplers);
        }
        let resamplers = self
            .stereo_resamplers
            .get_mut(&key)
            .context("stereo decoder resamplers were not initialized")?;
        let mut output_channels = Vec::with_capacity(channels);
        for channel in 0..channels {
            let channel_samples = samples
                .chunks_exact(channels)
                .map(|frame| frame[channel])
                .collect::<Vec<_>>();
            output_channels.push(resamplers[channel].process(&channel_samples)?);
        }
        let target_frame_len = frame_len_for_rate(MIX_SAMPLE_RATE);
        let mut output = Vec::with_capacity(target_frame_len * channels);
        for frame in 0..target_frame_len {
            for channel_samples in &output_channels {
                output.push(channel_samples[frame]);
            }
        }
        Ok(output)
    }
}

struct FrameResampler {
    from_rate: u32,
    to_rate: u32,
    target_len: usize,
    resampler: Option<Async<f32>>,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
}

impl FrameResampler {
    fn new(from_rate: u32, to_rate: u32) -> anyhow::Result<Self> {
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
                .context("create client audio resampler")?,
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

    fn process(&mut self, samples: &[i16]) -> anyhow::Result<Vec<i16>> {
        if self.from_rate == self.to_rate {
            return Ok(samples.to_vec());
        }
        if samples.len() != self.input[0].len() {
            bail!(
                "unexpected resampler input length: got {}, expected {}",
                samples.len(),
                self.input[0].len()
            );
        }

        for (slot, sample) in self.input[0].iter_mut().zip(samples) {
            *slot = *sample as f32 / i16::MAX as f32;
        }
        self.output[0].fill(0.0);

        let input_len = self.input[0].len();
        let output_len = self.output[0].len();
        let input = SequentialSliceOfVecs::new(&self.input, 1, input_len)
            .map_err(|err| anyhow!("create resampler input adapter: {err}"))?;
        let mut output = SequentialSliceOfVecs::new_mut(&mut self.output, 1, output_len)
            .map_err(|err| anyhow!("create resampler output adapter: {err}"))?;
        let (_, written) = self
            .resampler
            .as_mut()
            .context("resampler was not initialized")?
            .process_into_buffer(&input, &mut output, None)
            .map_err(|err| anyhow!("resample client audio frame: {err}"))?;

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

impl Default for FrameResampler {
    fn default() -> Self {
        Self::new(MIX_SAMPLE_RATE, MIX_SAMPLE_RATE).expect("valid identity resampler")
    }
}

fn frame_len_for_rate(rate: u32) -> usize {
    (rate as usize * common::FRAME_MS as usize) / 1_000
}

#[derive(Debug)]
pub struct ControlRequest {
    pub message: ControlMessage,
    pub response_tx: oneshot::Sender<ControlResponse>,
}

pub async fn send_control_message(
    control_tx: &mpsc::Sender<ControlRequest>,
    message: ControlMessage,
) -> anyhow::Result<()> {
    match send_control_request(control_tx, message).await? {
        ControlResponse::Ack => Ok(()),
        ControlResponse::Error { message } => bail!("{message}"),
        other => bail!("unexpected control response: {other:?}"),
    }
}

pub async fn send_control_request(
    control_tx: &mpsc::Sender<ControlRequest>,
    message: ControlMessage,
) -> anyhow::Result<ControlResponse> {
    let (response_tx, response_rx) = oneshot::channel();
    control_tx
        .send(ControlRequest {
            message,
            response_tx,
        })
        .await
        .context("queue control message")?;
    response_rx.await.context("control response channel closed")
}

pub async fn run_control_connection(
    control_url: String,
    mut control_rx: mpsc::Receiver<ControlRequest>,
    config: Arc<Mutex<ClientConfig>>,
    connection_events: Option<mpsc::Sender<ClientConnectionEvent>>,
) -> anyhow::Result<()> {
    let mut reconnect_delay = CONTROL_RECONNECT_INITIAL;
    let mut connected_once = false;

    loop {
        let (ws, _) = match tokio_tungstenite::connect_async(&control_url).await {
            Ok(connection) => connection,
            Err(err) => {
                tracing::warn!(%control_url, %err, "control WebSocket connect failed; retrying");
                notify_connection_event(&connection_events, ClientConnectionEvent::Reconnecting);
                if !wait_before_reconnect(reconnect_delay, &mut control_rx, connected_once).await {
                    return Ok(());
                }
                reconnect_delay = next_reconnect_delay(reconnect_delay);
                continue;
            }
        };
        let (mut write, mut read) = ws.split();
        let mut pending_responses = VecDeque::<oneshot::Sender<ControlResponse>>::new();
        tracing::info!(%control_url, "control WebSocket connected");
        notify_connection_event(&connection_events, ClientConnectionEvent::Connected);

        if connected_once {
            let snapshot = config.lock().unwrap().clone();
            let hello = ControlMessage::Hello {
                user_id: snapshot.user_id,
                requested_user_id: Some(snapshot.user_id),
                client_uid: snapshot.client_uid.clone(),
                codecs: supported_codecs(),
                buttons: snapshot.advertised_buttons,
                role: snapshot.role,
            };
            if let Err(err) = write
                .send(Message::Text(serde_json::to_string(&hello)?))
                .await
            {
                tracing::warn!(%err, "failed to send reconnect hello");
                notify_connection_event(&connection_events, ClientConnectionEvent::Reconnecting);
                if !wait_before_reconnect(reconnect_delay, &mut control_rx, true).await {
                    return Ok(());
                }
                reconnect_delay = next_reconnect_delay(reconnect_delay);
                continue;
            }
            tracing::info!(user_id = snapshot.user_id, "sent reconnect hello");
        }
        connected_once = true;
        reconnect_delay = CONTROL_RECONNECT_INITIAL;

        let session_result: anyhow::Result<()> = loop {
            tokio::select! {
                maybe_request = control_rx.recv() => {
                    let Some(request) = maybe_request else {
                        return Ok(());
                    };
                    let text = serde_json::to_string(&request.message)?;
                    if let Err(err) = write.send(Message::Text(text)).await {
                        let _ = request.response_tx.send(control_unavailable_response());
                        break Err(err.into());
                    }
                    pending_responses.push_back(request.response_tx);
                }
                maybe_reply = read.next() => {
                    let Some(reply) = maybe_reply else {
                        break Err(anyhow!("control WebSocket closed"));
                    };
                    match reply {
                        Ok(Message::Text(text)) => {
                            handle_control_text(&config, &mut pending_responses, &text);
                        }
                        Ok(Message::Ping(payload)) => {
                            if let Err(err) = write.send(Message::Pong(payload)).await {
                                break Err(err.into());
                            }
                        }
                        Ok(Message::Close(_)) => break Err(anyhow!("control WebSocket closed")),
                        Ok(_) => {}
                        Err(err) => break Err(err.into()),
                    }
                }
            }
        };

        fail_pending_control_responses(&mut pending_responses);
        if let Err(err) = session_result {
            config.lock().unwrap().active_buttons.clear();
            tracing::warn!(%err, "control WebSocket disconnected; reconnecting");
            notify_connection_event(&connection_events, ClientConnectionEvent::Disconnected);
            notify_connection_event(&connection_events, ClientConnectionEvent::Reconnecting);
        }
        if !wait_before_reconnect(reconnect_delay, &mut control_rx, true).await {
            return Ok(());
        }
        reconnect_delay = next_reconnect_delay(reconnect_delay);
    }
}

fn notify_connection_event(
    connection_events: &Option<mpsc::Sender<ClientConnectionEvent>>,
    event: ClientConnectionEvent,
) {
    if let Some(tx) = connection_events {
        let _ = tx.try_send(event);
    }
}

async fn wait_before_reconnect(
    delay: Duration,
    control_rx: &mut mpsc::Receiver<ControlRequest>,
    fail_control_requests: bool,
) -> bool {
    if !fail_control_requests {
        tokio::time::sleep(delay).await;
        return true;
    }

    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);

    loop {
        tokio::select! {
            _ = &mut sleep => return true,
            maybe_request = control_rx.recv() => {
                let Some(request) = maybe_request else {
                    return false;
                };
                let _ = request.response_tx.send(control_unavailable_response());
            }
        }
    }
}

pub fn next_reconnect_delay(current: Duration) -> Duration {
    (current * 2).min(CONTROL_RECONNECT_MAX)
}

pub fn control_unavailable_response() -> ControlResponse {
    ControlResponse::Error {
        message: "control connection unavailable; reconnecting".to_string(),
    }
}

pub fn fail_pending_control_responses(
    pending_responses: &mut VecDeque<oneshot::Sender<ControlResponse>>,
) {
    while let Some(response_tx) = pending_responses.pop_front() {
        let _ = response_tx.send(control_unavailable_response());
    }
}

fn handle_control_text(
    config: &Arc<Mutex<ClientConfig>>,
    pending_responses: &mut VecDeque<oneshot::Sender<ControlResponse>>,
    text: &str,
) {
    if let Ok(event) = serde_json::from_str::<ControlEvent>(text) {
        apply_control_event(config, event);
        return;
    }

    match serde_json::from_str::<ControlResponse>(text) {
        Ok(response) => {
            if let Some(response_tx) = pending_responses.pop_front() {
                let _ = response_tx.send(response);
            } else if matches!(response, ControlResponse::Hello { .. }) {
                tracing::debug!(?response, "received reconnect hello response");
            } else {
                tracing::warn!(?response, "received unexpected control response");
            }
        }
        Err(err) => tracing::warn!(%text, %err, "ignored unknown control message"),
    }
}

pub fn apply_control_event(config: &Arc<Mutex<ClientConfig>>, event: ControlEvent) {
    match event {
        ControlEvent::PresenceUpdate {
            user_id,
            client_uid: _,
            channels,
        } => {
            let mut config = config.lock().unwrap();
            if config.user_id != user_id {
                tracing::warn!(
                    local_user_id = config.user_id,
                    event_user_id = user_id,
                    "ignored presence update for a different user"
                );
                return;
            }
            config.channel_rosters = channels;
        }
        ControlEvent::ConfigUpdate {
            user_id,
            client_uid,
            name,
            listen,
            tx,
            vol,
            talker_vol,
            codec,
            opus_profile,
            talk_mode,
            regular_talk_active,
            priority,
            priority_channels,
            processing,
            buttons,
            active_buttons,
            active_direct_calls,
            last_direct_caller,
            direct_call_history,
            active_alerts,
            recent_alerts,
            emergency,
            ifb,
            lockout,
            stereo,
            esp32_audio,
        } => {
            let mut config = config.lock().unwrap();
            if config.user_id != user_id {
                tracing::warn!(
                    local_user_id = config.user_id,
                    event_user_id = user_id,
                    "ignored config update for a different user"
                );
                return;
            }

            if !client_uid.is_empty() {
                config.client_uid = client_uid;
            }
            config.listen = listen;
            config.name = name;
            config.tx = tx;
            config.vol = vol;
            config.talker_vol = talker_vol;
            if codec_supported(codec) {
                config.codec = codec;
                config.opus_profile = opus_profile;
            } else {
                tracing::warn!(codec = ?codec, "ignored unsupported pushed codec");
            }
            config.set_talk_mode(talk_mode);
            config.regular_talk_active = regular_talk_active;
            config.priority = priority;
            config.priority_channels = priority_channels;
            config.processing = processing;
            config.buttons = buttons;
            config.active_buttons = active_buttons;
            config.active_direct_calls = active_direct_calls;
            config.last_direct_caller = last_direct_caller;
            config.direct_call_history = direct_call_history;
            config.active_alerts = active_alerts;
            config.recent_alerts = recent_alerts;
            config.emergency = emergency;
            config.ifb = ifb;
            config.lockout = lockout;
            config.stereo = stereo;
            config.esp32_audio = esp32_audio;
            tracing::info!(
                listen = %format_channels(&config.listen),
                tx = %format_channels(&config.tx),
                buttons = %format_buttons(&config.buttons),
                active_buttons = %format_button_ids(&config.active_buttons),
                codec = format_codec(config.codec),
                opus_profile = ?config.opus_profile,
                talk_mode = ?config.talk_mode,
                regular_talk_active = config.regular_talk_active,
                priority = config.priority,
                priority_channels = %format_channels(&config.priority_channels),
                vol = %format_volumes(&config.vol),
                "applied server config"
            );
        }
    }
}

pub fn codec_supported(codec: Codec) -> bool {
    match codec {
        Codec::Pcm16 => true,
        Codec::Pcm24 => true,
        Codec::Pcm48 => true,
        Codec::Adpcm => false,
        Codec::Opus => true,
    }
}

pub fn supported_codecs() -> Vec<Codec> {
    vec![Codec::Pcm16, Codec::Pcm24, Codec::Pcm48, Codec::Opus]
}

pub fn parse_channels(value: &str) -> anyhow::Result<Vec<u16>> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }

    value
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<u16>()
                .with_context(|| format!("invalid channel `{part}`"))
        })
        .collect()
}

pub fn parse_volumes(value: &str) -> anyhow::Result<HashMap<u16, f32>> {
    let mut volumes = HashMap::new();
    if value.trim().is_empty() {
        return Ok(volumes);
    }

    for entry in value.split(',') {
        let Some((channel, gain)) = entry.split_once('=') else {
            bail!("invalid volume `{entry}`, expected channel=gain");
        };
        volumes.insert(
            channel
                .trim()
                .parse::<u16>()
                .with_context(|| format!("invalid channel `{channel}`"))?,
            gain.trim()
                .parse::<f32>()
                .with_context(|| format!("invalid gain `{gain}`"))?,
        );
    }

    Ok(volumes)
}

pub fn format_channels(channels: &[u16]) -> String {
    if channels.is_empty() {
        return "-".to_string();
    }

    channels
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

pub fn format_volumes(volumes: &HashMap<u16, f32>) -> String {
    if volumes.is_empty() {
        return "-".to_string();
    }

    let mut entries = volumes
        .iter()
        .map(|(channel, gain)| (*channel, *gain))
        .collect::<Vec<_>>();
    entries.sort_by_key(|(channel, _)| *channel);
    entries
        .into_iter()
        .map(|(channel, gain)| format!("{channel}={gain}"))
        .collect::<Vec<_>>()
        .join(",")
}

pub fn format_buttons(buttons: &[TalkButtonConfig]) -> String {
    if buttons.is_empty() {
        return "-".to_string();
    }

    buttons
        .iter()
        .map(|button| format!("{}:{}", button.id, format_button_actions(&button.actions)))
        .collect::<Vec<_>>()
        .join(",")
}

fn format_button_actions(actions: &[TalkButtonAction]) -> String {
    if actions.is_empty() {
        return "none".to_string();
    }
    actions
        .iter()
        .map(|action| match action {
            TalkButtonAction::Transmit {
                channels,
                users,
                duck,
            } => format!(
                "tx ch={} users={}{}",
                format_channels(channels),
                format_channels(users),
                if *duck { " duck" } else { "" }
            ),
            TalkButtonAction::Alert { targets, .. } => format!("alert {}", targets.len()),
            TalkButtonAction::ApplyPreset { preset_id } => format!("preset {preset_id}"),
            TalkButtonAction::SetTalkMode { users, mode } => {
                format!("talk-mode {:?} users={}", mode, format_channels(users))
            }
            TalkButtonAction::RouteEdit { users, .. } => {
                format!("route-edit users={}", format_channels(users))
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

pub fn format_button_ids(buttons: &[ButtonId]) -> String {
    if buttons.is_empty() {
        "-".to_string()
    } else {
        buttons.join(",")
    }
}

pub fn default_button_capabilities(count: u16) -> Vec<ButtonCapability> {
    (1..=count.min(MAX_CLIENT_BUTTON_COUNT))
        .map(|id| ButtonCapability {
            id: id.to_string(),
            label: format!("Button {id}"),
        })
        .collect()
}

pub fn merge_button_capabilities(
    mut defaults: Vec<ButtonCapability>,
    overrides: Vec<ButtonCapability>,
) -> Vec<ButtonCapability> {
    for override_button in overrides {
        if let Some(existing) = defaults
            .iter_mut()
            .find(|button| button.id.trim() == override_button.id.trim())
        {
            *existing = override_button;
        } else {
            defaults.push(override_button);
        }
    }
    normalize_button_capabilities(defaults)
}

fn compare_button_ids(left: &str, right: &str) -> std::cmp::Ordering {
    match (left.parse::<u16>(), right.parse::<u16>()) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => left.cmp(right),
    }
}

pub fn format_codec(codec: Codec) -> &'static str {
    match codec {
        Codec::Pcm16 => "pcm16",
        Codec::Pcm24 => "pcm24",
        Codec::Pcm48 => "pcm48",
        Codec::Adpcm => "adpcm",
        Codec::Opus => "opus",
    }
}

pub fn normalize_button_capabilities(mut buttons: Vec<ButtonCapability>) -> Vec<ButtonCapability> {
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

pub fn samples_for_ms(ms: u32) -> usize {
    if ms == 0 {
        return 0;
    }

    let frames = ms.div_ceil(common::FRAME_MS).max(1) as usize;
    frames * MIX_SAMPLES_PER_FRAME
}

#[derive(Debug)]
pub struct PlaybackBuffer {
    samples: Vec<i16>,
    read: usize,
    capacity: usize,
    prebuffer_samples: usize,
    channels: usize,
    started: bool,
    underflows: u64,
    overflows: u64,
    dropped_samples: u64,
}

impl PlaybackBuffer {
    pub fn new(capacity: usize, prebuffer_samples: usize) -> Self {
        Self {
            samples: Vec::with_capacity(capacity),
            read: 0,
            capacity,
            prebuffer_samples,
            channels: 1,
            started: prebuffer_samples == 0,
            underflows: 0,
            overflows: 0,
            dropped_samples: 0,
        }
    }

    pub fn push(&mut self, samples: &[i16]) {
        self.push_frame(samples, 1);
    }

    pub fn push_frame(&mut self, samples: &[i16], channels: usize) {
        let channels = channels.max(1);
        if self.channels != channels {
            self.clear();
            self.channels = channels;
        }
        self.compact_consumed();

        self.samples.extend_from_slice(samples);
        if self.samples.len() > self.capacity {
            let excess = self.samples.len() - self.capacity;
            self.samples.drain(..excess);
            self.overflows += 1;
            self.dropped_samples += excess as u64;
        }
    }

    pub fn set_channels(&mut self, channels: usize) {
        let channels = channels.max(1);
        if self.channels != channels {
            self.clear();
            self.channels = channels;
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.read = 0;
        self.started = self.prebuffer_samples == 0;
    }

    pub fn pop(&mut self) -> Option<i16> {
        self.pop_sample()
    }

    pub fn pop_stereo(&mut self) -> Option<(i16, i16)> {
        match self.channels {
            1 => self.pop_sample().map(|sample| (sample, sample)),
            2 => {
                let left = self.pop_sample()?;
                let right = self.pop_sample().unwrap_or(left);
                Some((left, right))
            }
            _ => self.pop_sample().map(|sample| (sample, sample)),
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    fn pop_sample(&mut self) -> Option<i16> {
        if !self.started {
            if self.available_samples() < self.prebuffer_samples {
                return None;
            }
            self.started = true;
        }

        let sample = self.samples.get(self.read).copied();
        if sample.is_some() {
            self.read += 1;
        } else {
            self.underflows += 1;
            self.started = self.prebuffer_samples == 0;
        }
        sample
    }

    pub fn available_samples(&self) -> usize {
        self.samples.len().saturating_sub(self.read)
    }

    pub fn stats(&self) -> PlaybackStats {
        PlaybackStats {
            available_samples: self.available_samples(),
            capacity_samples: self.capacity,
            prebuffer_samples: self.prebuffer_samples,
            channels: self.channels,
            started: self.started,
            underflows: self.underflows,
            overflows: self.overflows,
            dropped_samples: self.dropped_samples,
        }
    }

    fn compact_consumed(&mut self) {
        if self.read > 0 {
            self.samples.drain(..self.read);
            self.read = 0;
        }
    }
}

pub fn enqueue_connection_cue(playback: &mut PlaybackBuffer, event: ClientConnectionEvent) {
    let channels = playback.channels().clamp(1, 2);
    let cue = connection_cue_samples(event, channels);
    playback.push_frame(&cue, channels);
}

pub async fn run_connection_cue_task(
    playback: Arc<Mutex<PlaybackBuffer>>,
    mut events: mpsc::Receiver<ClientConnectionEvent>,
) {
    let mut reconnecting = false;
    loop {
        if reconnecting {
            tokio::select! {
                maybe_event = events.recv() => {
                    let Some(event) = maybe_event else {
                        return;
                    };
                    reconnecting = handle_connection_cue_event(&playback, event, reconnecting);
                }
                _ = tokio::time::sleep(RECONNECTING_CUE_REPEAT) => {
                    enqueue_connection_cue_locked(&playback, ClientConnectionEvent::Reconnecting);
                }
            }
        } else {
            let Some(event) = events.recv().await else {
                return;
            };
            reconnecting = handle_connection_cue_event(&playback, event, reconnecting);
        }
    }
}

fn handle_connection_cue_event(
    playback: &Arc<Mutex<PlaybackBuffer>>,
    event: ClientConnectionEvent,
    reconnecting: bool,
) -> bool {
    match event {
        ClientConnectionEvent::Connected => {
            enqueue_connection_cue_locked(playback, event);
            false
        }
        ClientConnectionEvent::Disconnected => {
            enqueue_connection_cue_locked(playback, event);
            false
        }
        ClientConnectionEvent::Reconnecting => {
            if !reconnecting {
                enqueue_connection_cue_locked(playback, event);
            }
            true
        }
    }
}

fn enqueue_connection_cue_locked(
    playback: &Arc<Mutex<PlaybackBuffer>>,
    event: ClientConnectionEvent,
) {
    let mut playback = playback.lock().unwrap();
    enqueue_connection_cue(&mut playback, event);
}

fn connection_cue_samples(event: ClientConnectionEvent, channels: usize) -> Vec<i16> {
    let segments: &[(u32, u32)] = match event {
        ClientConnectionEvent::Connected => &[(660, 70), (0, 30), (880, 90)],
        ClientConnectionEvent::Disconnected => &[(440, 90), (0, 30), (220, 130)],
        ClientConnectionEvent::Reconnecting => &[(523, 60), (0, 20), (659, 70)],
    };
    let amplitude = match event {
        ClientConnectionEvent::Reconnecting => 700.0,
        ClientConnectionEvent::Connected | ClientConnectionEvent::Disconnected => 1800.0,
    };
    let channels = channels.clamp(1, 2);
    let mut samples = Vec::new();
    for &(freq, ms) in segments {
        let frames = samples_for_ms(ms);
        for i in 0..frames {
            let mono = if freq == 0 {
                0
            } else {
                let phase =
                    2.0 * std::f32::consts::PI * freq as f32 * i as f32 / MIX_SAMPLE_RATE as f32;
                let envelope = cue_envelope(i, frames);
                (phase.sin() * amplitude * envelope).round() as i16
            };
            for _ in 0..channels {
                samples.push(mono);
            }
        }
    }
    samples
}

fn cue_envelope(sample: usize, total: usize) -> f32 {
    if total == 0 {
        return 0.0;
    }
    let fade = (total / 10).max(16);
    if sample < fade {
        sample as f32 / fade as f32
    } else if sample + fade >= total {
        total.saturating_sub(sample) as f32 / fade as f32
    } else {
        1.0
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct PlaybackStats {
    pub available_samples: usize,
    pub capacity_samples: usize,
    pub prebuffer_samples: usize,
    pub channels: usize,
    pub started: bool,
    pub underflows: u64,
    pub overflows: u64,
    pub dropped_samples: u64,
}

#[derive(Debug, Default, Clone)]
pub struct ClientTelemetryCounters {
    inner: Arc<ClientTelemetryCountersInner>,
}

#[derive(Debug, Default)]
struct ClientTelemetryCountersInner {
    udp_rx_packets: AtomicU64,
    malformed_packets: AtomicU64,
    decode_errors: AtomicU64,
    codec_drops: AtomicU64,
    payload_decode_errors: AtomicU64,
    tx_packets: AtomicU64,
    tx_send_failures: AtomicU64,
    tx_queue_drops: AtomicU64,
}

impl ClientTelemetryCounters {
    pub fn record_udp_rx_packet(&self) {
        self.inner.udp_rx_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_malformed_packet(&self) {
        self.inner.malformed_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_decode_error(&self) {
        self.inner.decode_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_codec_drop(&self) {
        self.inner.codec_drops.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_payload_decode_error(&self) {
        self.inner
            .payload_decode_errors
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_tx_packet(&self) {
        self.inner.tx_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_tx_send_failure(&self) {
        self.inner.tx_send_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_tx_queue_drop(&self) {
        self.inner.tx_queue_drops.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> common::ClientTelemetryTransportStatus {
        common::ClientTelemetryTransportStatus {
            udp_rx_packets: self.inner.udp_rx_packets.load(Ordering::Relaxed),
            malformed_packets: self.inner.malformed_packets.load(Ordering::Relaxed),
            decode_errors: self.inner.decode_errors.load(Ordering::Relaxed),
            codec_drops: self.inner.codec_drops.load(Ordering::Relaxed),
            payload_decode_errors: self.inner.payload_decode_errors.load(Ordering::Relaxed),
            tx_packets: self.inner.tx_packets.load(Ordering::Relaxed),
            tx_send_failures: self.inner.tx_send_failures.load(Ordering::Relaxed),
            tx_queue_drops: self.inner.tx_queue_drops.load(Ordering::Relaxed),
        }
    }
}

pub fn playback_telemetry_from_stats(
    stats: PlaybackStats,
) -> common::ClientTelemetryPlaybackStatus {
    common::ClientTelemetryPlaybackStatus {
        available_samples: stats.available_samples as u64,
        capacity_samples: stats.capacity_samples as u64,
        prebuffer_samples: stats.prebuffer_samples as u64,
        queue_depth: stats.available_samples as u64,
        channels: stats.channels.try_into().unwrap_or(u16::MAX),
        started: stats.started,
        underflows: stats.underflows,
        overflows: stats.overflows,
        dropped_samples: stats.dropped_samples,
    }
}

pub fn capture_health_with_client_telemetry(
    mut health: common::CaptureHealthStatus,
    client_kind: impl Into<String>,
    playback: PlaybackStats,
    transport: common::ClientTelemetryTransportStatus,
    phase: impl Into<String>,
    last_error: Option<String>,
) -> common::CaptureHealthStatus {
    let client_kind = client_kind.into();
    let phase = phase.into();
    let playback_telemetry = playback_telemetry_from_stats(playback);
    health.runtime = Some(common::ClientTelemetryRuntimeStatus {
        client_kind,
        phase,
        last_error,
    });
    health.playback_queue_depth = playback_telemetry
        .queue_depth
        .try_into()
        .unwrap_or(u16::MAX);
    health.playback_underflows = playback_telemetry.underflows.try_into().unwrap_or(u32::MAX);
    health.playback_overflows = playback_telemetry.overflows.try_into().unwrap_or(u32::MAX);
    health.playback = Some(playback_telemetry);
    health.client_transport = Some(transport);

    if health.audio.is_none() {
        health.audio = health
            .desktop
            .as_ref()
            .map(desktop_capture_audio_telemetry)
            .or_else(|| {
                Some(common::ClientTelemetryAudioStatus {
                    backend: health
                        .codec_config
                        .as_ref()
                        .map(|config| config.audio_backend.clone())
                        .filter(|backend| !backend.is_empty())
                        .unwrap_or_else(|| health.adc_input.clone()),
                    input_device: None,
                    output_device: None,
                    sample_format: String::new(),
                    sample_rate_hz: health
                        .codec_config
                        .as_ref()
                        .map_or(0, |config| config.hardware_sample_rate_hz),
                    channels: health
                        .codec_config
                        .as_ref()
                        .map_or(0, |config| u16::from(config.hardware_channels)),
                    channel_mode: health.capture_channel.clone(),
                    mic_gain: Some(f32::from(health.software_gain_percent) / 100.0),
                    speaker_gain: None,
                    input: health.selected.clone(),
                    pre_gain: health.selected.clone(),
                    post_gain: health.selected.clone(),
                    pre_gain_clipped_samples: health.raw_clipped_samples,
                    post_gain_clipped_samples: health.software_clipped_samples,
                    dropped_frames: 0,
                })
            });
    }

    health
}

pub fn basic_client_telemetry(
    client_kind: impl Into<String>,
    playback: PlaybackStats,
    transport: common::ClientTelemetryTransportStatus,
) -> common::CaptureHealthStatus {
    let client_kind = client_kind.into();
    capture_health_with_client_telemetry(
        common::CaptureHealthStatus {
            adc_input: client_kind.clone(),
            capture_channel: "mono".to_string(),
            ..common::CaptureHealthStatus::default()
        },
        client_kind,
        playback,
        transport,
        "running",
        None,
    )
}

fn desktop_capture_audio_telemetry(
    desktop: &common::DesktopCaptureHealthStatus,
) -> common::ClientTelemetryAudioStatus {
    common::ClientTelemetryAudioStatus {
        backend: desktop.backend.clone(),
        input_device: Some(desktop.device.clone()).filter(|device| !device.is_empty()),
        output_device: None,
        sample_format: desktop.sample_format.clone(),
        sample_rate_hz: desktop.sample_rate_hz,
        channels: desktop.channels,
        channel_mode: desktop.channel_mode.clone(),
        mic_gain: Some(desktop.mic_gain),
        speaker_gain: None,
        input: desktop.post_gain.clone(),
        pre_gain: desktop.pre_gain.clone(),
        post_gain: desktop.post_gain.clone(),
        pre_gain_clipped_samples: desktop.pre_gain_clipped_samples,
        post_gain_clipped_samples: desktop.post_gain_clipped_samples,
        dropped_frames: desktop.dropped_frames,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_button_capabilities_are_numeric_and_bounded() {
        let buttons = default_button_capabilities(DEFAULT_CLIENT_BUTTON_COUNT);
        assert_eq!(buttons.len(), 6);
        assert_eq!(buttons[0].id, "1");
        assert_eq!(buttons[5].label, "Button 6");

        let capped = default_button_capabilities(MAX_CLIENT_BUTTON_COUNT + 5);
        assert_eq!(capped.len(), usize::from(MAX_CLIENT_BUTTON_COUNT));
        assert_eq!(
            capped.last().unwrap().id,
            MAX_CLIENT_BUTTON_COUNT.to_string()
        );
    }

    #[test]
    fn server_endpoint_derives_default_addresses() {
        let endpoint = ClientServerEndpoint::new("192.168.12.84").unwrap();
        assert_eq!(endpoint.audio_addr_string(), "192.168.12.84:40000");
        assert_eq!(endpoint.control_url(), "ws://192.168.12.84:40001");
        assert_eq!(endpoint.admin_url(), "http://192.168.12.84:40002");
    }

    #[test]
    fn server_endpoint_formats_ipv6_for_urls_and_sockets() {
        let endpoint = ClientServerEndpoint::new("::1").unwrap();
        assert_eq!(endpoint.host, "::1");
        assert_eq!(endpoint.audio_addr_string(), "[::1]:40000");
        assert_eq!(endpoint.control_url(), "ws://[::1]:40001");

        let bracketed = ClientServerEndpoint::new("[::1]").unwrap();
        assert_eq!(bracketed.host, "::1");
    }

    #[test]
    fn server_host_validation_rejects_non_host_inputs() {
        for host in [
            "",
            "http://127.0.0.1",
            "127.0.0.1:40000",
            "server.local/path",
            "server local",
            "[::1]:40000",
        ] {
            assert!(validate_server_host(host).is_err(), "{host} should fail");
        }
        assert_eq!(
            validate_server_host("server.local").unwrap(),
            "server.local"
        );
        assert_eq!(validate_server_host("localhost").unwrap(), "localhost");
    }

    #[test]
    fn endpoint_overrides_migrate_legacy_default_ports_to_host() {
        let mut endpoints = ClientEndpointOverrides::normalized(
            "",
            "192.168.1.20:40000".parse().unwrap(),
            "ws://192.168.1.20:40001",
            Some("http://192.168.1.20:40002".to_string()),
            false,
        );
        assert_eq!(endpoints.server_host, "192.168.1.20");
        assert!(!endpoints.advanced_endpoints);
        assert_eq!(endpoints.control, "ws://192.168.1.20:40001");
        assert_eq!(endpoints.admin, None);

        endpoints.server = "192.168.1.20:41000".parse().unwrap();
        endpoints.control = "ws://192.168.1.20:41001".to_string();
        endpoints.advanced_endpoints = false;
        normalize_endpoint_overrides(&mut endpoints);
        assert!(endpoints.advanced_endpoints);
    }

    #[test]
    fn telemetry_builder_maps_playback_and_transport_counters() {
        let counters = ClientTelemetryCounters::default();
        counters.record_udp_rx_packet();
        counters.record_malformed_packet();
        counters.record_decode_error();
        counters.record_tx_packet();
        counters.record_tx_send_failure();
        counters.record_tx_queue_drop();

        let playback = PlaybackStats {
            available_samples: 120,
            capacity_samples: 960,
            prebuffer_samples: 240,
            channels: 2,
            started: true,
            underflows: 3,
            overflows: 4,
            dropped_samples: 5,
        };
        let health = basic_client_telemetry("pi", playback, counters.snapshot());

        assert_eq!(health.runtime.unwrap().client_kind, "pi");
        let playback = health.playback.unwrap();
        assert_eq!(playback.available_samples, 120);
        assert_eq!(playback.capacity_samples, 960);
        assert_eq!(playback.underflows, 3);
        assert_eq!(health.playback_queue_depth, 120);
        assert_eq!(health.playback_underflows, 3);
        let transport = health.client_transport.unwrap();
        assert_eq!(transport.udp_rx_packets, 1);
        assert_eq!(transport.malformed_packets, 1);
        assert_eq!(transport.decode_errors, 1);
        assert_eq!(transport.tx_packets, 1);
        assert_eq!(transport.tx_send_failures, 1);
        assert_eq!(transport.tx_queue_drops, 1);
    }

    #[test]
    fn merge_button_capabilities_overrides_default_labels() {
        let buttons = merge_button_capabilities(
            default_button_capabilities(12),
            vec![ButtonCapability {
                id: "2".to_string(),
                label: "Director".to_string(),
            }],
        );
        assert_eq!(buttons[1].id, "2");
        assert_eq!(buttons[1].label, "Director");
        assert_eq!(buttons[9].id, "10");
    }

    #[test]
    fn active_buttons_union_with_default_ptt_route() {
        let mut config = ClientConfig {
            user_id: 1,
            client_uid: "test-client".to_string(),
            role: ClientRole::Client,
            name: String::new(),
            listen: vec![1],
            tx: vec![1, 2],
            codec: Codec::Pcm16,
            opus_profile: OpusProfile::default(),
            talk_mode: TalkMode::Ptt,
            last_non_muted_talk_mode: TalkMode::Ptt,
            regular_talk_active: true,
            priority: false,
            priority_channels: Vec::new(),
            emergency: None,
            vol: HashMap::new(),
            talker_vol: HashMap::new(),
            buttons: vec![TalkButtonConfig {
                id: "director".to_string(),
                label: "Director".to_string(),
                color: None,
                mode: common::TalkButtonMode::Momentary,
                actions: vec![TalkButtonAction::Transmit {
                    channels: vec![2, 3],
                    users: vec![4],
                    duck: false,
                }],
            }],
            active_buttons: vec!["director".to_string()],
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
            processing: ProcessingConfig::default(),
            channel_rosters: Vec::new(),
        };

        assert_eq!(config.active_tx_channels(), vec![1, 2, 3]);
        assert_eq!(
            config.active_tx_targets(),
            vec![
                AudioTarget::Channel(1),
                AudioTarget::Channel(2),
                AudioTarget::Channel(3),
                AudioTarget::Direct(4)
            ]
        );
        config.talk_mode = TalkMode::Muted;
        config.regular_talk_active = false;
        assert_eq!(config.active_tx_channels(), vec![2, 3]);
        config.talk_mode = TalkMode::Open;
        assert_eq!(config.active_tx_channels(), vec![1, 2, 3]);
    }

    #[test]
    fn presence_update_replaces_channel_rosters() {
        let config = Arc::new(Mutex::new(ClientConfig {
            user_id: 1,
            client_uid: "test-client".to_string(),
            role: ClientRole::Client,
            name: String::new(),
            listen: vec![1],
            tx: Vec::new(),
            codec: Codec::Pcm16,
            opus_profile: OpusProfile::default(),
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
            advertised_buttons: Vec::new(),
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: Esp32AudioConfig::default(),
        }));

        apply_control_event(
            &config,
            ControlEvent::PresenceUpdate {
                user_id: 1,
                client_uid: "test-client".to_string(),
                channels: vec![ChannelPresenceRoster {
                    channel_id: 7,
                    name: Some("Production PL".to_string()),
                    members: vec![common::ChannelPresenceMember {
                        user_id: 2,
                        name: "Director".to_string(),
                        present: true,
                        transmitting: true,
                    }],
                }],
            },
        );

        let config = config.lock().unwrap();
        assert_eq!(config.channel_rosters[0].channel_id, 7);
        assert_eq!(
            config.channel_rosters[0].name.as_deref(),
            Some("Production PL")
        );
        assert!(config.channel_rosters[0].members[0].transmitting);
    }

    #[test]
    fn playback_buffer_waits_for_prebuffer() {
        let mut playback =
            PlaybackBuffer::new(common::SAMPLES_PER_FRAME * 4, common::SAMPLES_PER_FRAME * 2);

        playback.push(&vec![1; common::SAMPLES_PER_FRAME]);
        assert_eq!(playback.pop(), None);
        playback.push(&vec![2; common::SAMPLES_PER_FRAME]);
        assert_eq!(playback.pop(), Some(1));
    }

    #[test]
    fn playback_buffer_tracks_underflow_and_overflow() {
        let mut playback = PlaybackBuffer::new(4, 0);

        assert_eq!(playback.pop(), None);
        playback.push(&[1, 2, 3, 4, 5, 6]);

        let stats = playback.stats();
        assert_eq!(stats.underflows, 1);
        assert_eq!(stats.overflows, 1);
        assert_eq!(stats.dropped_samples, 2);
        assert_eq!(stats.available_samples, 4);
    }

    #[test]
    fn playback_buffer_tracks_stereo_layout_and_clears_on_change() {
        let mut playback = PlaybackBuffer::new(MIX_SAMPLES_PER_FRAME * 4, 0);

        playback.push_frame(&[1, 2, 3, 4], 2);
        assert_eq!(playback.channels(), 2);
        assert_eq!(playback.pop_stereo(), Some((1, 2)));
        assert_eq!(playback.pop_stereo(), Some((3, 4)));

        playback.push_frame(&[9, 10], 2);
        playback.push_frame(&[11], 1);
        assert_eq!(playback.channels(), 1);
        assert_eq!(playback.available_samples(), 1);
        assert_eq!(playback.pop_stereo(), Some((11, 11)));
    }

    #[test]
    fn connection_cues_enqueue_without_changing_existing_layout() {
        let mut playback = PlaybackBuffer::new(samples_for_ms(2500) * 2, 0);
        playback.set_channels(2);
        enqueue_connection_cue(&mut playback, ClientConnectionEvent::Connected);

        assert_eq!(playback.channels(), 2);
        assert!(playback.available_samples() > samples_for_ms(150) * 2);
        assert!(playback.pop_stereo().is_some());

        let before = playback.available_samples();
        enqueue_connection_cue(&mut playback, ClientConnectionEvent::Reconnecting);
        assert_eq!(playback.channels(), 2);
        assert!(playback.available_samples() > before + samples_for_ms(120) * 2);
        assert_eq!(RECONNECTING_CUE_REPEAT, Duration::from_secs(4));
    }

    #[test]
    fn reconnecting_cue_remains_audible_in_small_playback_buffer() {
        let mut playback = PlaybackBuffer::new(samples_for_ms(120), 0);
        enqueue_connection_cue(&mut playback, ClientConnectionEvent::Reconnecting);

        let mut non_zero_samples = 0;
        while let Some(sample) = playback.pop() {
            if sample != 0 {
                non_zero_samples += 1;
            }
        }

        assert!(non_zero_samples > 0);
    }

    #[tokio::test]
    async fn initial_control_connect_failure_emits_reconnecting_event() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let (_control_tx, control_rx) = mpsc::channel::<ControlRequest>(1);
        let (event_tx, mut event_rx) = mpsc::channel::<ClientConnectionEvent>(1);
        let config = Arc::new(Mutex::new(ClientConfig {
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
        }));
        let task = tokio::spawn(run_control_connection(
            format!("ws://{addr}"),
            control_rx,
            config,
            Some(event_tx),
        ));

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap();
        task.abort();

        assert_eq!(event, Some(ClientConnectionEvent::Reconnecting));
    }

    #[test]
    fn runtime_gain_is_clamped() {
        let settings = AudioSettings::new(f32::NAN, 99.0);

        assert_eq!(settings.mic_gain(), 1.0);
        assert_eq!(settings.speaker_gain(), 8.0);
    }

    #[test]
    fn client_identity_file_is_created_and_reused() {
        let path = std::env::temp_dir().join(format!(
            "intercom-client-identity-test-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let first = load_or_create_client_uid(None, Some(&path)).unwrap();
        let second = load_or_create_client_uid(None, Some(&path)).unwrap();

        assert!(!first.is_empty());
        assert_eq!(first, second);
        assert_eq!(
            load_or_create_client_uid(Some("override-uid"), Some(&path)).unwrap(),
            "override-uid"
        );
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn tcp_bind_fallback_uses_next_available_port() {
        let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let occupied_addr = occupied.local_addr().unwrap();
        if occupied_addr.port() == u16::MAX {
            return;
        }

        let (_listener, actual_addr) = bind_tcp_listener_with_port_fallback(occupied_addr)
            .await
            .unwrap();

        assert_eq!(actual_addr.ip(), occupied_addr.ip());
        assert_ne!(actual_addr.port(), occupied_addr.port());
        assert!(actual_addr.port() > occupied_addr.port());
    }

    #[test]
    fn frame_resampler_produces_expected_client_frame_lengths() {
        let mut down = FrameResampler::new(MIX_SAMPLE_RATE, common::SAMPLE_RATE).unwrap();
        let downsampled = down.process(&vec![100; MIX_SAMPLES_PER_FRAME]).unwrap();
        assert_eq!(downsampled.len(), common::SAMPLES_PER_FRAME);

        let mut up = FrameResampler::new(common::SAMPLE_RATE, MIX_SAMPLE_RATE).unwrap();
        let upsampled = up.process(&vec![100; common::SAMPLES_PER_FRAME]).unwrap();
        assert_eq!(upsampled.len(), MIX_SAMPLES_PER_FRAME);
    }

    #[test]
    fn codec_paths_round_trip_to_mixer_domain() {
        let mixer_frame = (0..MIX_SAMPLES_PER_FRAME)
            .map(|index| ((index as f32 * 0.05).sin() * 10_000.0) as i16)
            .collect::<Vec<_>>();

        for codec in [Codec::Pcm16, Codec::Pcm24, Codec::Pcm48, Codec::Opus] {
            let mut encoder = AudioEncoder::new(codec, OpusProfile::default()).unwrap();
            let payload = encoder.encode(&mixer_frame).unwrap();
            if codec == Codec::Opus {
                assert!(!payload.is_empty());
            } else {
                assert_eq!(payload.len(), common::codec_pcm16_payload_bytes(codec));
            }

            let mut decoder = AudioDecoder::default();
            let decoded = decoder.decode(codec, &payload).unwrap();
            assert_eq!(decoded.len(), MIX_SAMPLES_PER_FRAME);
        }
    }

    #[test]
    fn opus_encoder_uses_intercom_profile() {
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
}
