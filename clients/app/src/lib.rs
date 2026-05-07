use std::fs;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use clap::{Parser, ValueEnum};
use client_core::{
    default_audio_addr, derive_admin_url_from_control, ClientAudioBackendKind,
    ClientEndpointOverrides, ClientInputChannelMode, ClientRuntimeConfig, ClientServerEndpoint,
    DEFAULT_CLIENT_BUTTON_COUNT, DEFAULT_SERVER_HOST, MAX_CLIENT_BUTTON_COUNT,
};
use common::{Codec, OpusProfile};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
pub struct AppArgs {
    #[arg(long, default_value = "intercom-app-settings.json")]
    pub config_file: PathBuf,
    #[arg(long)]
    pub write_config: bool,
    #[arg(long)]
    pub init_config: bool,
    #[arg(long)]
    pub print_config: bool,
    #[arg(long)]
    pub print_launch_plan: bool,
    #[arg(long)]
    pub server_host: Option<String>,
    #[arg(long)]
    pub server: Option<SocketAddr>,
    #[arg(long)]
    pub control: Option<String>,
    #[arg(long)]
    pub user_id: Option<u16>,
    #[arg(long)]
    pub client_uid: Option<String>,
    #[arg(long)]
    pub identity_file: Option<PathBuf>,
    #[arg(long)]
    pub tx_channel: Option<u16>,
    #[arg(long)]
    pub listen_channel: Option<u16>,
    #[arg(long, value_enum)]
    pub codec: Option<AppCodec>,
    #[arg(long, value_enum)]
    pub opus_profile: Option<AppOpusProfile>,
    #[arg(long)]
    pub mic_gain: Option<f32>,
    #[arg(long)]
    pub input_limiter: bool,
    #[arg(long)]
    pub disable_input_transient_suppression: bool,
    #[arg(long)]
    pub speaker_gain: Option<f32>,
    #[arg(long, value_parser = clap::value_parser!(u32).range(0..=250))]
    pub jitter_ms: Option<u32>,
    #[arg(long)]
    pub input_device: Option<String>,
    #[arg(long, value_enum)]
    pub input_backend: Option<desktop::AudioInputBackend>,
    #[arg(long, value_enum)]
    pub input_channel: Option<desktop::InputChannelMode>,
    #[arg(long)]
    pub output_device: Option<String>,
    #[arg(long)]
    pub debug_audio_dir: Option<PathBuf>,
    #[arg(long, value_parser = clap::value_parser!(u16).range(0..=MAX_CLIENT_BUTTON_COUNT as i64))]
    pub button_count: Option<u16>,
    #[arg(long = "button", value_name = "ID[=LABEL]")]
    pub buttons: Vec<String>,
    #[arg(long = "button-key", value_name = "ID=KEY")]
    pub button_keys: Vec<String>,
    #[arg(long)]
    pub local_ui_bind: Option<SocketAddr>,
    #[arg(long, env = "INTERCOM_LOCAL_UI_TOKEN")]
    pub local_ui_token: Option<String>,
    #[arg(long)]
    pub disable_local_ui: bool,
    #[arg(long)]
    pub enable_local_ui: bool,
    #[arg(long, value_enum)]
    pub window_mode: Option<AppWindowMode>,
    #[arg(long)]
    pub app_title: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(0..=30_000))]
    pub ui_open_delay_ms: Option<u64>,
    #[arg(long)]
    pub list_devices: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AppCodec {
    Pcm16,
    Pcm24,
    Pcm48,
    Opus,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AppOpusProfile {
    #[value(name = "speech-16-low", alias = "speech-low")]
    Speech16Low,
    #[value(name = "speech-24-standard", alias = "speech-standard")]
    Speech24Standard,
    #[value(name = "speech-48-high", alias = "speech-high")]
    Speech48High,
    #[value(name = "music-48", alias = "music-high")]
    Music48,
}

impl From<AppOpusProfile> for OpusProfile {
    fn from(value: AppOpusProfile) -> Self {
        match value {
            AppOpusProfile::Speech16Low => Self::Speech16Low,
            AppOpusProfile::Speech24Standard => Self::Speech24Standard,
            AppOpusProfile::Speech48High => Self::Speech48High,
            AppOpusProfile::Music48 => Self::Music48,
        }
    }
}

impl From<AppCodec> for Codec {
    fn from(value: AppCodec) -> Self {
        match value {
            AppCodec::Pcm16 => Self::Pcm16,
            AppCodec::Pcm24 => Self::Pcm24,
            AppCodec::Pcm48 => Self::Pcm48,
            AppCodec::Opus => Self::Opus,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AppWindowMode {
    SystemBrowser,
    Native,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AppSettings {
    pub app_title: String,
    pub server_profiles: Vec<MobileServerProfile>,
    pub server_host: String,
    pub server: SocketAddr,
    pub control: String,
    pub admin: Option<String>,
    pub advanced_endpoints: bool,
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
    pub input_backend: desktop::AudioInputBackend,
    pub input_channel: desktop::InputChannelMode,
    pub output_device: Option<String>,
    pub debug_audio_dir: Option<PathBuf>,
    pub button_count: u16,
    pub buttons: Vec<String>,
    pub button_keys: Vec<String>,
    pub local_ui_bind: SocketAddr,
    pub local_ui_token: Option<String>,
    pub disable_local_ui: bool,
    pub window_mode: AppWindowMode,
    pub ui_open_delay_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MobileServerProfile {
    pub id: String,
    pub name: String,
    pub server_host: String,
    pub server: String,
    pub control: String,
    pub admin: Option<String>,
    pub auth: Option<String>,
    pub version: Option<String>,
    pub last_connected_ms: Option<u64>,
    pub discovered: bool,
}

impl Default for MobileServerProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            server_host: String::new(),
            server: String::new(),
            control: String::new(),
            admin: None,
            auth: None,
            version: None,
            last_connected_ms: None,
            discovered: false,
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            app_title: "Intercom Suite".to_string(),
            server_profiles: Vec::new(),
            server_host: DEFAULT_SERVER_HOST.to_string(),
            server: default_audio_addr(),
            control: ClientServerEndpoint::default().control_url(),
            admin: None,
            advanced_endpoints: false,
            user_id: Some(1),
            client_uid: None,
            identity_file: None,
            tx_channel: 1,
            listen_channel: 1,
            codec: Codec::Pcm16,
            opus_profile: OpusProfile::default(),
            mic_gain: 1.0,
            input_limiter: false,
            input_transient_suppression: true,
            speaker_gain: 1.0,
            jitter_ms: 40,
            input_device: None,
            input_backend: desktop::AudioInputBackend::Auto,
            input_channel: desktop::InputChannelMode::Average,
            output_device: None,
            debug_audio_dir: None,
            button_count: DEFAULT_CLIENT_BUTTON_COUNT,
            buttons: Vec::new(),
            button_keys: Vec::new(),
            local_ui_bind: "127.0.0.1:41002".parse().expect("valid default address"),
            local_ui_token: None,
            disable_local_ui: false,
            window_mode: AppWindowMode::SystemBrowser,
            ui_open_delay_ms: 750,
        }
    }
}

impl AppSettings {
    pub fn endpoint_overrides(&self) -> ClientEndpointOverrides {
        ClientEndpointOverrides::normalized(
            &self.server_host,
            self.server,
            &self.control,
            self.admin.clone(),
            self.advanced_endpoints,
        )
    }

    pub fn normalize_endpoints(&mut self) {
        let endpoints = self.endpoint_overrides();
        self.server_host = endpoints.server_host;
        self.server = endpoints.server;
        self.control = endpoints.control;
        self.admin = endpoints.admin;
        self.advanced_endpoints = endpoints.advanced_endpoints;
        self.server_profiles = self
            .server_profiles
            .drain(..)
            .map(normalize_mobile_profile)
            .collect();
    }

    pub fn effective_server(&self) -> anyhow::Result<SocketAddr> {
        self.endpoint_overrides().effective_server()
    }

    pub fn effective_control(&self) -> anyhow::Result<String> {
        self.endpoint_overrides().effective_control()
    }

    pub fn effective_admin(&self) -> anyhow::Result<String> {
        self.endpoint_overrides().effective_admin()
    }

    pub fn merge_cli(&mut self, args: &AppArgs) {
        if let Some(server_host) = &args.server_host {
            self.server_host = server_host.clone();
            self.advanced_endpoints = false;
        }
        if let Some(server) = args.server {
            self.server = server;
            self.advanced_endpoints = true;
        }
        if let Some(control) = &args.control {
            self.control = control.clone();
            self.advanced_endpoints = true;
        }
        if let Some(user_id) = args.user_id {
            self.user_id = Some(user_id);
        }
        if let Some(client_uid) = &args.client_uid {
            self.client_uid = Some(client_uid.clone());
        }
        if let Some(identity_file) = &args.identity_file {
            self.identity_file = Some(identity_file.clone());
        }
        if let Some(tx_channel) = args.tx_channel {
            self.tx_channel = tx_channel;
        }
        if let Some(listen_channel) = args.listen_channel {
            self.listen_channel = listen_channel;
        }
        if let Some(codec) = args.codec {
            self.codec = codec.into();
        }
        if let Some(opus_profile) = args.opus_profile {
            self.opus_profile = opus_profile.into();
        }
        if let Some(mic_gain) = args.mic_gain {
            self.mic_gain = mic_gain;
        }
        if args.input_limiter {
            self.input_limiter = true;
        }
        if args.disable_input_transient_suppression {
            self.input_transient_suppression = false;
        }
        if let Some(speaker_gain) = args.speaker_gain {
            self.speaker_gain = speaker_gain;
        }
        if let Some(jitter_ms) = args.jitter_ms {
            self.jitter_ms = jitter_ms;
        }
        if let Some(input_device) = &args.input_device {
            self.input_device = Some(input_device.clone());
        }
        if let Some(input_backend) = args.input_backend {
            self.input_backend = input_backend;
        }
        if let Some(input_channel) = args.input_channel {
            self.input_channel = input_channel;
        }
        if let Some(output_device) = &args.output_device {
            self.output_device = Some(output_device.clone());
        }
        if let Some(debug_audio_dir) = &args.debug_audio_dir {
            self.debug_audio_dir = Some(debug_audio_dir.clone());
        }
        if let Some(button_count) = args.button_count {
            self.button_count = button_count;
        }
        if !args.buttons.is_empty() {
            self.buttons = args.buttons.clone();
        }
        if !args.button_keys.is_empty() {
            self.button_keys = args.button_keys.clone();
        }
        if let Some(local_ui_bind) = args.local_ui_bind {
            self.local_ui_bind = local_ui_bind;
        }
        if let Some(local_ui_token) = &args.local_ui_token {
            self.local_ui_token = Some(local_ui_token.clone());
        }
        if args.disable_local_ui {
            self.disable_local_ui = true;
        }
        if args.enable_local_ui {
            self.disable_local_ui = false;
        }
        if let Some(window_mode) = args.window_mode {
            self.window_mode = window_mode;
        }
        if let Some(app_title) = &args.app_title {
            self.app_title = app_title.clone();
        }
        if let Some(ui_open_delay_ms) = args.ui_open_delay_ms {
            self.ui_open_delay_ms = ui_open_delay_ms;
        }
        self.normalize_endpoints();
    }

    pub fn client_runtime_config(&self, list_devices: bool) -> anyhow::Result<ClientRuntimeConfig> {
        self.validate()?;
        let endpoints = self.endpoint_overrides();
        Ok(ClientRuntimeConfig {
            server_host: endpoints.server_host.clone(),
            server: endpoints.effective_server()?,
            control: endpoints.effective_control()?,
            user_id: self.user_id,
            client_uid: self.client_uid.clone(),
            identity_file: self.identity_file.clone(),
            tx_channel: self.tx_channel,
            listen_channel: self.listen_channel,
            codec: self.codec,
            opus_profile: self.opus_profile,
            mic_gain: self.mic_gain,
            input_limiter: self.input_limiter,
            input_transient_suppression: self.input_transient_suppression,
            speaker_gain: self.speaker_gain,
            jitter_ms: self.jitter_ms,
            input_device: self.input_device.clone(),
            input_backend: app_input_backend_to_core(self.input_backend),
            input_channel: app_input_channel_to_core(self.input_channel),
            output_device: self.output_device.clone(),
            debug_audio_dir: self.debug_audio_dir.clone(),
            button_count: self.button_count,
            buttons: self.buttons.clone(),
            button_keys: self.button_keys.clone(),
            local_ui_bind: self.local_ui_bind,
            local_ui_token: self.local_ui_token.clone(),
            disable_local_ui: self.disable_local_ui,
            list_devices,
        })
    }

    pub fn desktop_args(&self, list_devices: bool) -> anyhow::Result<desktop::Args> {
        let runtime = self.client_runtime_config(list_devices)?;
        Ok(desktop::Args {
            server_host: Some(runtime.server_host),
            server: runtime.server,
            control: runtime.control,
            user_id: runtime.user_id,
            client_uid: runtime.client_uid,
            identity_file: runtime.identity_file,
            tx_channel: runtime.tx_channel,
            listen_channel: runtime.listen_channel,
            codec: codec_to_wire(runtime.codec)?,
            opus_profile: opus_profile_to_wire(runtime.opus_profile),
            mic_gain: runtime.mic_gain,
            input_limiter: runtime.input_limiter,
            disable_input_transient_suppression: !runtime.input_transient_suppression,
            speaker_gain: runtime.speaker_gain,
            jitter_ms: runtime.jitter_ms,
            input_device: runtime.input_device,
            input_backend: core_input_backend_to_desktop(runtime.input_backend),
            input_channel: core_input_channel_to_desktop(runtime.input_channel),
            output_device: runtime.output_device,
            debug_audio_dir: runtime.debug_audio_dir,
            button_count: runtime.button_count,
            buttons: runtime
                .buttons
                .iter()
                .map(|button| button.parse())
                .collect::<Result<Vec<_>, _>>()
                .map_err(anyhow::Error::msg)?,
            button_keys: runtime
                .button_keys
                .iter()
                .map(|binding| binding.parse())
                .collect::<Result<Vec<_>, _>>()
                .map_err(anyhow::Error::msg)?,
            local_ui_bind: runtime.local_ui_bind,
            local_ui_token: runtime.local_ui_token,
            disable_local_ui: runtime.disable_local_ui,
            list_devices,
        })
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        self.endpoint_overrides().validate()?;
        if self.app_title.trim().is_empty() {
            bail!("app_title cannot be empty");
        }
        if self.jitter_ms > 250 {
            bail!("jitter_ms must be between 0 and 250");
        }
        if self.ui_open_delay_ms > 30_000 {
            bail!("ui_open_delay_ms must be between 0 and 30000");
        }
        if self.button_count > MAX_CLIENT_BUTTON_COUNT {
            bail!("button_count must be between 0 and {MAX_CLIENT_BUTTON_COUNT}");
        }
        validate_gain("mic_gain", self.mic_gain)?;
        validate_gain("speaker_gain", self.speaker_gain)?;
        codec_to_wire(self.codec)?;

        for button in &self.buttons {
            button
                .parse::<desktop::ButtonArg>()
                .map_err(anyhow::Error::msg)
                .with_context(|| format!("invalid button `{button}`"))?;
        }
        for binding in &self.button_keys {
            binding
                .parse::<desktop::ButtonKeyArg>()
                .map_err(anyhow::Error::msg)
                .with_context(|| format!("invalid button key `{binding}`"))?;
        }

        Ok(())
    }
}

pub fn load_settings(path: &Path) -> anyhow::Result<AppSettings> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let mut settings: AppSettings = serde_json::from_str(&text)
                .with_context(|| format!("parse app settings from {}", path.display()))?;
            settings.normalize_endpoints();
            Ok(settings)
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {
            let mut settings = AppSettings::default();
            settings.normalize_endpoints();
            Ok(settings)
        }
        Err(err) => Err(err).with_context(|| format!("read app settings from {}", path.display())),
    }
}

pub fn save_settings(path: &Path, settings: &AppSettings) -> anyhow::Result<()> {
    let mut settings = settings.clone();
    settings.normalize_endpoints();
    settings.validate()?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("create app settings directory {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&settings)?;
    let tmp_path = temp_settings_path(path);
    fs::write(&tmp_path, format!("{json}\n"))
        .with_context(|| format!("write app settings to {}", tmp_path.display()))?;
    fs::rename(&tmp_path, path)
        .with_context(|| format!("replace app settings at {}", path.display()))?;
    Ok(())
}

#[allow(dead_code)]
fn mobile_profile_id(name: &str, control: &str) -> String {
    let name = name.trim();
    let control = control.trim();
    if name.is_empty() {
        control.to_string()
    } else {
        format!("{name}|{control}")
    }
}

#[allow(dead_code)]
fn normalize_mobile_profile(mut profile: MobileServerProfile) -> MobileServerProfile {
    profile.name = profile.name.trim().to_string();
    profile.server_host = profile.server_host.trim().to_string();
    profile.server = profile.server.trim().to_string();
    profile.control = profile.control.trim().to_string();
    profile.admin = profile
        .admin
        .map(|admin| admin.trim().to_string())
        .filter(|admin| !admin.is_empty());
    profile.auth = profile
        .auth
        .map(|auth| auth.trim().to_string())
        .filter(|auth| !auth.is_empty());
    profile.version = profile
        .version
        .map(|version| version.trim().to_string())
        .filter(|version| !version.is_empty());
    if profile.server_host.is_empty() {
        if let Ok(server) = profile.server.parse::<SocketAddr>() {
            let endpoints = ClientEndpointOverrides::normalized(
                "",
                server,
                &profile.control,
                profile.admin.clone(),
                false,
            );
            if !endpoints.advanced_endpoints {
                let admin = endpoints.effective_admin().unwrap_or_else(|_| {
                    derive_admin_url_from_control(&endpoints.control).unwrap_or_default()
                });
                profile.server_host = endpoints.server_host;
                profile.server = endpoints.server.to_string();
                profile.control = endpoints.control;
                profile.admin = Some(admin).filter(|admin| !admin.is_empty());
            } else {
                profile.server_host = endpoints.server_host;
            }
        }
    } else if let Ok(endpoint) = ClientServerEndpoint::new(&profile.server_host) {
        if profile.server.is_empty() {
            if let Ok(server) = endpoint.resolve_audio_addr() {
                profile.server = server.to_string();
            }
        }
        if profile.control.is_empty() {
            profile.control = endpoint.control_url();
        }
        if profile.admin.is_none() {
            profile.admin = Some(endpoint.admin_url());
        }
    }
    if profile.id.trim().is_empty() {
        profile.id = mobile_profile_id(&profile.name, &profile.control);
    }
    profile
}

#[allow(dead_code)]
fn remember_mobile_profile(settings: &mut AppSettings, profile: MobileServerProfile) {
    let profile = normalize_mobile_profile(profile);
    if profile.server.is_empty() || profile.control.is_empty() {
        return;
    }
    settings
        .server_profiles
        .retain(|existing| existing.id != profile.id && existing.control != profile.control);
    settings.server_profiles.insert(0, profile);
    settings.server_profiles.truncate(20);
}

#[allow(dead_code)]
fn mobile_connected_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppLaunchPlan {
    pub app_title: String,
    pub window_mode: AppWindowMode,
    pub local_ui_url: Option<String>,
    pub opens_window: bool,
}

pub fn build_launch_plan(
    settings: &mut AppSettings,
    list_devices: bool,
) -> anyhow::Result<AppLaunchPlan> {
    let opens_window = !list_devices
        && !settings.disable_local_ui
        && settings.window_mode != AppWindowMode::Disabled;
    let local_ui_url = if opens_window {
        settings.local_ui_bind = choose_available_bind(settings.local_ui_bind)?;
        Some(format!("http://{}/", settings.local_ui_bind))
    } else {
        None
    };

    Ok(AppLaunchPlan {
        app_title: settings.app_title.clone(),
        window_mode: settings.window_mode,
        local_ui_url,
        opens_window,
    })
}

fn choose_available_bind(requested: SocketAddr) -> anyhow::Result<SocketAddr> {
    let mut addr = requested;
    let mut last_error = None;
    for _ in 0..=client_core::LOCAL_BIND_PORT_FALLBACK_ATTEMPTS {
        match std::net::TcpListener::bind(addr) {
            Ok(listener) => {
                let actual = listener.local_addr()?;
                return Ok(actual);
            }
            Err(err)
                if err.kind() == ErrorKind::AddrInUse
                    && requested.port() != 0
                    && addr.port() < u16::MAX =>
            {
                last_error = Some(err);
                addr.set_port(addr.port() + 1);
            }
            Err(err) => return Err(err).with_context(|| format!("check local UI bind at {addr}")),
        }
    }

    Err(last_error.expect("fallback loop records the address-in-use error"))
        .with_context(|| format!("find available local UI bind near {requested}"))
}

fn spawn_window_opener(plan: &AppLaunchPlan, delay_ms: u64) {
    if plan.window_mode != AppWindowMode::SystemBrowser {
        return;
    }
    let Some(url) = plan.local_ui_url.clone() else {
        return;
    };
    tokio::spawn(async move {
        if delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
        if let Err(err) = open_url(&url) {
            tracing::warn!(%url, %err, "failed to open app window URL");
        }
    });
}

fn open_url(url: &str) -> anyhow::Result<()> {
    let command = open_command_for_platform(url, HostPlatform::current());
    Command::new(&command.program)
        .args(&command.args)
        .spawn()
        .with_context(|| format!("launch {}", command.program))?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostPlatform {
    Macos,
    Windows,
    Linux,
}

impl HostPlatform {
    fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenCommand {
    program: String,
    args: Vec<String>,
}

fn open_command_for_platform(url: &str, platform: HostPlatform) -> OpenCommand {
    match platform {
        HostPlatform::Macos => OpenCommand {
            program: "open".to_string(),
            args: vec![url.to_string()],
        },
        HostPlatform::Windows => OpenCommand {
            program: "cmd".to_string(),
            args: vec![
                "/C".to_string(),
                "start".to_string(),
                "".to_string(),
                url.to_string(),
            ],
        },
        HostPlatform::Linux => OpenCommand {
            program: "xdg-open".to_string(),
            args: vec![url.to_string()],
        },
    }
}

pub async fn run_app(args: AppArgs) -> anyhow::Result<()> {
    let mut settings = load_settings(&args.config_file)?;
    settings.merge_cli(&args);
    settings.validate()?;

    if args.print_config {
        println!("{}", serde_json::to_string_pretty(&settings)?);
        return Ok(());
    }

    if args.init_config || args.write_config {
        save_settings(&args.config_file, &settings)?;
        eprintln!("wrote app settings to {}", args.config_file.display());
    }
    if args.init_config {
        eprintln!("settings initialized; start the app with `cargo run -p app`");
        return Ok(());
    }

    let mut runtime_settings = settings.clone();
    let launch_plan = build_launch_plan(&mut runtime_settings, args.list_devices)?;
    if args.print_launch_plan {
        println!("{}", serde_json::to_string_pretty(&launch_plan)?);
        return Ok(());
    }

    spawn_window_opener(&launch_plan, runtime_settings.ui_open_delay_ms);
    let desktop_args = runtime_settings.desktop_args(args.list_devices)?;
    desktop::run(desktop_args).await
}

#[cfg(all(feature = "native", any(target_os = "ios", target_os = "android")))]
#[tauri::mobile_entry_point]
pub fn run_mobile() {
    mobile::run();
}

#[cfg(all(feature = "native", any(target_os = "ios", target_os = "android")))]
mod mobile_audio;

#[cfg(all(feature = "native", any(target_os = "ios", target_os = "android")))]
mod mobile {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::thread;
    use std::thread::JoinHandle;

    use anyhow::Context;
    use client_core::{ClientAudioBackend, ClientEndpointOverrides, ClientRuntimePhase};
    use common::{
        AlertId, ButtonId, CaptureHealthStatus, ClientTelemetryRuntimeStatus, Codec, TalkMode,
    };
    use serde::Serialize;
    use tauri::Manager;
    use tokio::sync::oneshot;

    use super::{
        load_settings, mobile_connected_timestamp_ms, mobile_profile_id, normalize_mobile_profile,
        remember_mobile_profile, save_settings, AppSettings, AppWindowMode, MobileServerProfile,
    };
    use crate::mobile_audio::DefaultMobileAudioPlatform;

    #[derive(Default)]
    struct MobileAppState {
        runtime: Mutex<MobileRuntimeState>,
    }

    struct MobileRuntimeState {
        runtime: Option<MobileRuntimeHandle>,
        phase: ClientRuntimePhase,
        last_error: Option<String>,
    }

    impl Default for MobileRuntimeState {
        fn default() -> Self {
            Self {
                runtime: None,
                phase: ClientRuntimePhase::Stopped,
                last_error: None,
            }
        }
    }

    struct MobileRuntimeHandle {
        controls_url: String,
        api: desktop::LocalClientApi,
        shutdown_tx: Option<oneshot::Sender<()>>,
        join: Option<JoinHandle<anyhow::Result<()>>>,
    }

    impl MobileRuntimeHandle {
        fn finish_if_done(&mut self) -> Option<anyhow::Result<()>> {
            if !self.join.as_ref().is_some_and(JoinHandle::is_finished) {
                return None;
            }
            let join = self.join.take().expect("join handle checked above");
            Some(
                join.join()
                    .map_err(|_| anyhow::anyhow!("mobile client runtime thread panicked"))
                    .and_then(|result| result),
            )
        }

        fn shutdown(mut self) -> anyhow::Result<()> {
            if let Some(shutdown_tx) = self.shutdown_tx.take() {
                let _ = shutdown_tx.send(());
            }
            if let Some(join) = self.join.take() {
                join.join()
                    .map_err(|_| anyhow::anyhow!("mobile client runtime thread panicked"))??;
            }
            Ok(())
        }
    }

    #[derive(Debug, Clone, Serialize)]
    struct MobileStartResponse {
        build: common::BuildInfo,
        running: bool,
        phase: ClientRuntimePhase,
        local_ui_url: String,
        last_error: Option<String>,
    }

    #[derive(Debug, Clone, Serialize)]
    struct MobileStatusResponse {
        build: common::BuildInfo,
        running: bool,
        phase: ClientRuntimePhase,
        local_ui_url: Option<String>,
        last_error: Option<String>,
    }

    pub fn run() {
        tauri::Builder::default()
            .manage(MobileAppState::default())
            .invoke_handler(tauri::generate_handler![
                mobile_default_settings,
                mobile_discover_servers,
                mobile_forget_server,
                mobile_load_settings,
                mobile_open_controls,
                mobile_save_settings,
                mobile_select_server,
                mobile_start_client,
                mobile_stop_client,
                mobile_status,
                client_ack_alert,
                client_button_down,
                client_button_toggle,
                client_button_up,
                client_call_down,
                client_call_toggle,
                client_call_up,
                client_cancel_alert,
                client_codec,
                client_config,
                client_gain,
                client_mute,
                client_reply_down,
                client_reply_toggle,
                client_reply_up,
                client_send_alert,
                client_state,
                client_talk_down,
                client_talk_mode,
                client_talk_toggle,
                client_talk_up,
                client_unmute
            ])
            .setup(|app| {
                tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    tauri::WebviewUrl::App("mobile.html".into()),
                )
                .build()?;
                Ok(())
            })
            .run(tauri::generate_context!("tauri.conf.json"))
            .expect("run Tauri mobile app");
    }

    #[tauri::command]
    fn mobile_default_settings() -> AppSettings {
        let mut settings = AppSettings::default();
        settings.window_mode = AppWindowMode::Native;
        settings.disable_local_ui = true;
        settings.normalize_endpoints();
        settings
    }

    #[tauri::command]
    fn mobile_load_settings(app: tauri::AppHandle) -> std::result::Result<AppSettings, String> {
        let path = mobile_settings_path(&app)?;
        load_settings(&path).map_err(|err| err.to_string())
    }

    #[tauri::command]
    fn mobile_save_settings(
        app: tauri::AppHandle,
        mut settings: AppSettings,
    ) -> std::result::Result<(), String> {
        prepare_mobile_settings(&mut settings);
        let profile = mobile_profile_for_settings(&settings, None);
        remember_mobile_profile(&mut settings, profile);
        let path = mobile_settings_path(&app)?;
        save_settings(&path, &settings).map_err(|err| err.to_string())
    }

    #[tauri::command]
    fn mobile_discover_servers(
        app: tauri::AppHandle,
    ) -> std::result::Result<Vec<MobileServerProfile>, String> {
        let path = mobile_settings_path(&app)?;
        let settings = load_settings(&path).map_err(|err| err.to_string())?;
        let discovered = discover_mobile_servers().map_err(|err| err.to_string())?;
        Ok(merge_mobile_profiles(settings.server_profiles, discovered))
    }

    #[tauri::command]
    fn mobile_select_server(
        app: tauri::AppHandle,
        profile: MobileServerProfile,
    ) -> std::result::Result<AppSettings, String> {
        let mut profile = normalize_mobile_profile(profile);
        let server = profile
            .server
            .parse()
            .map_err(|err| format!("parse server audio address {}: {err}", profile.server))?;
        if profile.control.is_empty() {
            return Err("control WebSocket URL cannot be empty".to_string());
        }
        if profile.name.is_empty() {
            profile.name = profile.control.clone();
        }
        if profile.id.is_empty() {
            profile.id = mobile_profile_id(&profile.name, &profile.control);
        }

        let path = mobile_settings_path(&app)?;
        let mut settings = load_settings(&path).map_err(|err| err.to_string())?;
        let endpoints = ClientEndpointOverrides::normalized(
            &profile.server_host,
            server,
            &profile.control,
            profile.admin.clone(),
            false,
        );
        settings.server_host = endpoints.server_host;
        settings.server = server;
        settings.control = profile.control.clone();
        settings.admin = profile.admin.clone();
        settings.advanced_endpoints = endpoints.advanced_endpoints;
        remember_mobile_profile(&mut settings, profile);
        prepare_mobile_settings(&mut settings);
        save_settings(&path, &settings).map_err(|err| err.to_string())?;
        Ok(settings)
    }

    #[tauri::command]
    fn mobile_forget_server(
        app: tauri::AppHandle,
        id: String,
    ) -> std::result::Result<AppSettings, String> {
        let path = mobile_settings_path(&app)?;
        let mut settings = load_settings(&path).map_err(|err| err.to_string())?;
        settings.server_profiles.retain(|profile| profile.id != id);
        save_settings(&path, &settings).map_err(|err| err.to_string())?;
        Ok(settings)
    }

    #[tauri::command]
    fn mobile_start_client(
        app: tauri::AppHandle,
        state: tauri::State<'_, MobileAppState>,
        mut settings: AppSettings,
    ) -> std::result::Result<MobileStartResponse, String> {
        prepare_mobile_settings(&mut settings);
        let path = mobile_settings_path(&app)?;
        save_settings(&path, &settings).map_err(|err| err.to_string())?;

        {
            let mut runtime_state = state
                .runtime
                .lock()
                .map_err(|_| "mobile runtime state is poisoned".to_string())?;
            refresh_mobile_runtime_state(&mut runtime_state);
            if let Some(runtime) = runtime_state.runtime.as_ref() {
                let local_ui_url = runtime.controls_url.clone();
                return Ok(MobileStartResponse {
                    build: common::current_build_info(),
                    running: true,
                    phase: runtime_state.phase,
                    local_ui_url,
                    last_error: runtime_state.last_error.clone(),
                });
            }
            runtime_state.phase = ClientRuntimePhase::Starting;
            runtime_state.last_error = None;
        }

        if let Err(err) = DefaultMobileAudioPlatform.prepare() {
            set_mobile_runtime_failure(&state, &err);
            return Err(err);
        }

        let start_result = (|| {
            let runtime = spawn_mobile_runtime(settings.clone()).map_err(|err| err.to_string())?;
            let controls_url = runtime.controls_url.clone();
            Ok::<_, String>((runtime, controls_url))
        })();
        let (runtime, controls_url) = match start_result {
            Ok(result) => result,
            Err(err) => {
                set_mobile_runtime_failure(&state, &err);
                return Err(err);
            }
        };

        let mut connected_settings = settings.clone();
        let connected_profile =
            mobile_profile_for_settings(&connected_settings, Some(mobile_connected_timestamp_ms()));
        remember_mobile_profile(&mut connected_settings, connected_profile);
        if let Err(err) = save_settings(&path, &connected_settings) {
            tracing::warn!(%err, "could not update mobile server profile after connect");
        }

        let mut runtime_state = state
            .runtime
            .lock()
            .map_err(|_| "mobile runtime state is poisoned".to_string())?;
        runtime_state.runtime = Some(runtime);
        runtime_state.phase = ClientRuntimePhase::Running;
        runtime_state.last_error = None;
        Ok(MobileStartResponse {
            build: common::current_build_info(),
            running: true,
            phase: ClientRuntimePhase::Running,
            local_ui_url: controls_url,
            last_error: None,
        })
    }

    #[tauri::command]
    fn mobile_stop_client(
        state: tauri::State<'_, MobileAppState>,
    ) -> std::result::Result<(), String> {
        let runtime = {
            let mut runtime_state = state
                .runtime
                .lock()
                .map_err(|_| "mobile runtime state is poisoned".to_string())?;
            let runtime = runtime_state.runtime.take();
            runtime_state.phase = ClientRuntimePhase::Stopped;
            runtime_state.last_error = None;
            runtime
        };
        if let Some(runtime) = runtime {
            runtime.shutdown().map_err(|err| err.to_string())?;
        }
        Ok(())
    }

    #[tauri::command]
    fn mobile_open_controls(
        state: tauri::State<'_, MobileAppState>,
    ) -> std::result::Result<(), String> {
        mobile_runtime_api(&state).map(|_| ())
    }

    #[tauri::command]
    fn mobile_status(
        state: tauri::State<'_, MobileAppState>,
    ) -> std::result::Result<MobileStatusResponse, String> {
        let mut runtime_state = state
            .runtime
            .lock()
            .map_err(|_| "mobile runtime state is poisoned".to_string())?;
        refresh_mobile_runtime_state(&mut runtime_state);
        Ok(MobileStatusResponse {
            build: common::current_build_info(),
            running: runtime_state.runtime.is_some(),
            phase: runtime_state.phase,
            local_ui_url: runtime_state
                .runtime
                .as_ref()
                .map(|runtime| runtime.controls_url.clone()),
            last_error: runtime_state.last_error.clone(),
        })
    }

    fn mobile_runtime_api(
        state: &tauri::State<'_, MobileAppState>,
    ) -> std::result::Result<desktop::LocalClientApi, String> {
        let mut runtime_state = state
            .runtime
            .lock()
            .map_err(|_| "mobile runtime state is poisoned".to_string())?;
        refresh_mobile_runtime_state(&mut runtime_state);
        runtime_state
            .runtime
            .as_ref()
            .map(|runtime| runtime.api.clone())
            .ok_or_else(|| {
                runtime_state
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "mobile client is not running".to_string())
            })
    }

    fn mobile_runtime_phase_name(phase: ClientRuntimePhase) -> &'static str {
        match phase {
            ClientRuntimePhase::Stopped => "stopped",
            ClientRuntimePhase::Starting => "starting",
            ClientRuntimePhase::Running => "running",
            ClientRuntimePhase::Failed => "failed",
        }
    }

    #[tauri::command]
    fn client_state(
        state: tauri::State<'_, MobileAppState>,
    ) -> std::result::Result<client_core::StateResponse, String> {
        let (api, phase, last_error) = {
            let mut runtime_state = state
                .runtime
                .lock()
                .map_err(|_| "mobile runtime state is poisoned".to_string())?;
            refresh_mobile_runtime_state(&mut runtime_state);
            let api = runtime_state
                .runtime
                .as_ref()
                .map(|runtime| runtime.api.clone())
                .ok_or_else(|| {
                    runtime_state
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "mobile client is not running".to_string())
                })?;
            (api, runtime_state.phase, runtime_state.last_error.clone())
        };
        let mut response = api.state();
        let runtime = ClientTelemetryRuntimeStatus {
            client_kind: "mobile".to_string(),
            phase: mobile_runtime_phase_name(phase).to_string(),
            last_error,
        };
        if let Some(telemetry) = response.telemetry.as_mut() {
            telemetry.runtime = Some(runtime);
        } else {
            response.telemetry = Some(CaptureHealthStatus {
                runtime: Some(runtime),
                adc_input: "mobile".to_string(),
                capture_channel: "mobile".to_string(),
                ..CaptureHealthStatus::default()
            });
        }
        Ok(response)
    }

    #[tauri::command]
    async fn client_config(
        state: tauri::State<'_, MobileAppState>,
        request: client_core::FullConfigRequest,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .apply_config(request)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_talk_mode(
        state: tauri::State<'_, MobileAppState>,
        mode: TalkMode,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .set_talk_mode(mode)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_mute(
        state: tauri::State<'_, MobileAppState>,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .mute()
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_unmute(
        state: tauri::State<'_, MobileAppState>,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .unmute()
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_talk_down(
        state: tauri::State<'_, MobileAppState>,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .talk_down()
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_talk_up(
        state: tauri::State<'_, MobileAppState>,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .talk_up()
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_talk_toggle(
        state: tauri::State<'_, MobileAppState>,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .talk_toggle()
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_codec(
        state: tauri::State<'_, MobileAppState>,
        codec: Codec,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .set_codec(codec)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    fn client_gain(
        state: tauri::State<'_, MobileAppState>,
        request: client_core::GainRequest,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .set_gain(request)
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_button_down(
        state: tauri::State<'_, MobileAppState>,
        id: ButtonId,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .button_down(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_button_up(
        state: tauri::State<'_, MobileAppState>,
        id: ButtonId,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .button_up(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_button_toggle(
        state: tauri::State<'_, MobileAppState>,
        id: ButtonId,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .button_toggle(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_call_down(
        state: tauri::State<'_, MobileAppState>,
        id: u16,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .call_down(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_call_up(
        state: tauri::State<'_, MobileAppState>,
        id: u16,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .call_up(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_call_toggle(
        state: tauri::State<'_, MobileAppState>,
        id: u16,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .call_toggle(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_reply_down(
        state: tauri::State<'_, MobileAppState>,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .reply_down()
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_reply_up(
        state: tauri::State<'_, MobileAppState>,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .reply_up()
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_reply_toggle(
        state: tauri::State<'_, MobileAppState>,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .reply_toggle()
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_send_alert(
        state: tauri::State<'_, MobileAppState>,
        request: client_core::AlertRequest,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .send_alert(request)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_ack_alert(
        state: tauri::State<'_, MobileAppState>,
        id: AlertId,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .ack_alert(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_cancel_alert(
        state: tauri::State<'_, MobileAppState>,
        id: AlertId,
    ) -> std::result::Result<client_core::OkResponse, String> {
        mobile_runtime_api(&state)?
            .cancel_alert(id)
            .await
            .map_err(|err| err.to_string())
    }

    fn refresh_mobile_runtime_state(runtime_state: &mut MobileRuntimeState) {
        let Some(runtime) = runtime_state.runtime.as_mut() else {
            if runtime_state.phase != ClientRuntimePhase::Failed {
                runtime_state.phase = ClientRuntimePhase::Stopped;
            }
            return;
        };
        let Some(result) = runtime.finish_if_done() else {
            runtime_state.phase = ClientRuntimePhase::Running;
            return;
        };
        runtime_state.runtime = None;
        match result {
            Ok(()) => {
                runtime_state.phase = ClientRuntimePhase::Stopped;
                runtime_state.last_error = Some("mobile client runtime exited".to_string());
            }
            Err(err) => {
                runtime_state.phase = ClientRuntimePhase::Failed;
                runtime_state.last_error = Some(err.to_string());
            }
        }
    }

    fn set_mobile_runtime_failure(state: &tauri::State<'_, MobileAppState>, err: &str) {
        if let Ok(mut runtime_state) = state.runtime.lock() {
            runtime_state.runtime = None;
            runtime_state.phase = ClientRuntimePhase::Failed;
            runtime_state.last_error = Some(err.to_string());
        }
    }

    fn mobile_profile_for_settings(
        settings: &AppSettings,
        last_connected_ms: Option<u64>,
    ) -> MobileServerProfile {
        let endpoints = settings.endpoint_overrides();
        let control = endpoints
            .effective_control()
            .unwrap_or_else(|_| settings.control.clone());
        let server = endpoints
            .effective_server()
            .map(|server| server.to_string())
            .unwrap_or_else(|_| settings.server.to_string());
        let admin = endpoints.effective_admin().ok();
        let existing = settings
            .server_profiles
            .iter()
            .find(|profile| profile.control == control);
        let name = existing
            .map(|profile| profile.name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "Manual server".to_string());
        let id = existing
            .map(|profile| profile.id.clone())
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| mobile_profile_id(&name, &control));
        MobileServerProfile {
            id,
            name,
            server_host: endpoints.server_host,
            server,
            control,
            admin: existing.and_then(|profile| profile.admin.clone()).or(admin),
            auth: existing.and_then(|profile| profile.auth.clone()),
            version: existing.and_then(|profile| profile.version.clone()),
            last_connected_ms,
            discovered: false,
        }
    }

    fn merge_mobile_profiles(
        mut saved: Vec<MobileServerProfile>,
        discovered: Vec<MobileServerProfile>,
    ) -> Vec<MobileServerProfile> {
        for mut profile in discovered {
            profile = normalize_mobile_profile(profile);
            profile.discovered = true;
            if let Some(existing) = saved
                .iter_mut()
                .find(|existing| existing.id == profile.id || existing.control == profile.control)
            {
                let last_connected_ms = existing.last_connected_ms;
                *existing = profile;
                existing.last_connected_ms = last_connected_ms;
            } else {
                saved.push(profile);
            }
        }
        saved.sort_by(|a, b| {
            b.last_connected_ms
                .cmp(&a.last_connected_ms)
                .then_with(|| b.discovered.cmp(&a.discovered))
                .then_with(|| a.name.cmp(&b.name))
        });
        saved
    }

    fn discover_mobile_servers() -> anyhow::Result<Vec<MobileServerProfile>> {
        discover_mobile_servers_for_platform()
    }

    #[cfg(target_os = "ios")]
    fn discover_mobile_servers_for_platform() -> anyhow::Result<Vec<MobileServerProfile>> {
        ios_discover_intercom_servers()
    }

    #[cfg(not(target_os = "ios"))]
    fn discover_mobile_servers_for_platform() -> anyhow::Result<Vec<MobileServerProfile>> {
        Ok(Vec::new())
    }

    #[cfg(target_os = "ios")]
    fn ios_discover_intercom_servers() -> anyhow::Result<Vec<MobileServerProfile>> {
        use std::ffi::CStr;
        use std::os::raw::c_char;

        unsafe extern "C" {
            fn intercom_ios_browse_intercom_services(timeout_seconds: f64) -> *mut c_char;
            fn intercom_ios_free_string(value: *mut c_char);
        }

        let raw = unsafe { intercom_ios_browse_intercom_services(2.0) };
        if raw.is_null() {
            return Ok(Vec::new());
        }
        let text = unsafe { CStr::from_ptr(raw).to_string_lossy().into_owned() };
        unsafe { intercom_ios_free_string(raw) };
        serde_json::from_str(&text).context("parse iOS Bonjour discovery results")
    }

    fn spawn_mobile_runtime(settings: AppSettings) -> anyhow::Result<MobileRuntimeHandle> {
        let desktop_args = settings.desktop_args(false)?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (api_tx, api_rx) = std::sync::mpsc::channel::<desktop::LocalClientApi>();
        let join = thread::Builder::new()
            .name("intercom-mobile-client-runtime".to_string())
            .spawn(move || {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .context("build mobile client runtime")
                    .and_then(|runtime| {
                        runtime.block_on(desktop::run_until_shutdown_with_local_api(
                            desktop_args,
                            async move {
                                let _ = shutdown_rx.await;
                            },
                            Some(api_tx),
                        ))
                    })
            })
            .context("spawn mobile client runtime")?;
        let api = match api_rx.recv_timeout(std::time::Duration::from_secs(15)) {
            Ok(api) => api,
            Err(err) => {
                let _ = shutdown_tx.send(());
                return Err(anyhow::anyhow!(
                    "mobile client runtime did not become ready: {err}"
                ));
            }
        };
        Ok(MobileRuntimeHandle {
            controls_url: mobile_controls_url(),
            api,
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
        })
    }

    fn prepare_mobile_settings(settings: &mut AppSettings) {
        settings.window_mode = AppWindowMode::Native;
        settings.disable_local_ui = true;
        settings.normalize_endpoints();
    }

    fn mobile_controls_url() -> String {
        "client-controls.html".to_string()
    }

    fn mobile_settings_path(app: &tauri::AppHandle) -> std::result::Result<PathBuf, String> {
        let dir = app
            .path()
            .app_config_dir()
            .map_err(|err| format!("resolve mobile app config directory: {err}"))?;
        Ok(dir.join("intercom-app-settings.json"))
    }
}

fn validate_gain(name: &str, gain: f32) -> anyhow::Result<()> {
    if !gain.is_finite() || !(0.0..=8.0).contains(&gain) {
        bail!("{name} must be a finite value between 0 and 8");
    }
    Ok(())
}

fn temp_settings_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "intercom-app-settings.json".into());
    file_name.push(format!(".{}.tmp", std::process::id()));
    path.with_file_name(file_name)
}

fn app_input_backend_to_core(backend: desktop::AudioInputBackend) -> ClientAudioBackendKind {
    match backend {
        desktop::AudioInputBackend::Auto => ClientAudioBackendKind::Auto,
        desktop::AudioInputBackend::Raw => ClientAudioBackendKind::Raw,
        desktop::AudioInputBackend::VoiceProcessing => ClientAudioBackendKind::VoiceProcessing,
    }
}

fn core_input_backend_to_desktop(backend: ClientAudioBackendKind) -> desktop::AudioInputBackend {
    match backend {
        ClientAudioBackendKind::Auto
        | ClientAudioBackendKind::IosAvAudioSession
        | ClientAudioBackendKind::IosVoiceProcessingIo
        | ClientAudioBackendKind::IosRemoteIo => desktop::AudioInputBackend::Auto,
        ClientAudioBackendKind::Raw => desktop::AudioInputBackend::Raw,
        ClientAudioBackendKind::VoiceProcessing => desktop::AudioInputBackend::VoiceProcessing,
    }
}

fn app_input_channel_to_core(channel: desktop::InputChannelMode) -> ClientInputChannelMode {
    match channel {
        desktop::InputChannelMode::Average => ClientInputChannelMode::Average,
        desktop::InputChannelMode::Left => ClientInputChannelMode::Left,
        desktop::InputChannelMode::Right => ClientInputChannelMode::Right,
    }
}

fn core_input_channel_to_desktop(channel: ClientInputChannelMode) -> desktop::InputChannelMode {
    match channel {
        ClientInputChannelMode::Average => desktop::InputChannelMode::Average,
        ClientInputChannelMode::Left => desktop::InputChannelMode::Left,
        ClientInputChannelMode::Right => desktop::InputChannelMode::Right,
    }
}

fn codec_to_wire(codec: Codec) -> anyhow::Result<desktop::WireCodec> {
    match codec {
        Codec::Pcm16 => Ok(desktop::WireCodec::Pcm16),
        Codec::Pcm24 => Ok(desktop::WireCodec::Pcm24),
        Codec::Pcm48 => Ok(desktop::WireCodec::Pcm48),
        Codec::Opus => Ok(desktop::WireCodec::Opus),
        Codec::Adpcm => bail!("ADPCM is not supported by the app client"),
    }
}

fn opus_profile_to_wire(profile: OpusProfile) -> desktop::WireOpusProfile {
    match profile {
        OpusProfile::Speech16Low => desktop::WireOpusProfile::Speech16Low,
        OpusProfile::Speech24Standard => desktop::WireOpusProfile::Speech24Standard,
        OpusProfile::Speech48High => desktop::WireOpusProfile::Speech48High,
        OpusProfile::Music48 => desktop::WireOpusProfile::Music48,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn args() -> AppArgs {
        AppArgs {
            config_file: PathBuf::from("intercom-app-settings.json"),
            write_config: false,
            init_config: false,
            print_config: false,
            print_launch_plan: false,
            server_host: None,
            server: None,
            control: None,
            user_id: None,
            client_uid: None,
            identity_file: None,
            tx_channel: None,
            listen_channel: None,
            codec: None,
            opus_profile: None,
            mic_gain: None,
            input_limiter: false,
            disable_input_transient_suppression: false,
            speaker_gain: None,
            jitter_ms: None,
            input_device: None,
            input_backend: None,
            input_channel: None,
            output_device: None,
            debug_audio_dir: None,
            button_count: None,
            buttons: Vec::new(),
            button_keys: Vec::new(),
            local_ui_bind: None,
            local_ui_token: None,
            disable_local_ui: false,
            enable_local_ui: false,
            window_mode: None,
            app_title: None,
            ui_open_delay_ms: None,
            list_devices: false,
        }
    }

    #[test]
    fn defaults_match_desktop_launcher_defaults() {
        let settings = AppSettings::default();

        assert_eq!(settings.user_id, Some(1));
        assert_eq!(settings.server_host, "127.0.0.1");
        assert_eq!(settings.server, "127.0.0.1:40000".parse().unwrap());
        assert_eq!(settings.control, "ws://127.0.0.1:40001");
        assert!(!settings.advanced_endpoints);
        assert_eq!(settings.tx_channel, 1);
        assert_eq!(settings.listen_channel, 1);
        assert_eq!(settings.codec, Codec::Pcm16);
        assert_eq!(settings.opus_profile, OpusProfile::Speech24Standard);
        assert!(!settings.input_limiter);
        assert!(settings.input_transient_suppression);
        assert_eq!(settings.input_backend, desktop::AudioInputBackend::Auto);
        assert_eq!(settings.input_channel, desktop::InputChannelMode::Average);
        assert_eq!(settings.local_ui_bind, "127.0.0.1:41002".parse().unwrap());
        assert_eq!(settings.local_ui_token, None);
        assert_eq!(settings.button_count, DEFAULT_CLIENT_BUTTON_COUNT);
        assert_eq!(settings.app_title, "Intercom Suite");
        assert_eq!(settings.window_mode, AppWindowMode::SystemBrowser);
        assert_eq!(settings.ui_open_delay_ms, 750);
        assert!(settings.server_profiles.is_empty());
    }

    #[test]
    fn server_host_derives_runtime_endpoints() {
        let settings = AppSettings {
            server_host: "192.168.12.84".to_string(),
            ..AppSettings::default()
        };

        let runtime = settings.client_runtime_config(false).unwrap();
        assert_eq!(runtime.server_host, "192.168.12.84");
        assert_eq!(runtime.server, "192.168.12.84:40000".parse().unwrap());
        assert_eq!(runtime.control, "ws://192.168.12.84:40001");
    }

    #[test]
    fn legacy_default_port_settings_migrate_to_server_host() {
        let mut settings = AppSettings {
            server_host: String::new(),
            server: "192.168.1.20:40000".parse().unwrap(),
            control: "ws://192.168.1.20:40001".to_string(),
            admin: Some("http://192.168.1.20:40002".to_string()),
            ..AppSettings::default()
        };

        settings.normalize_endpoints();
        assert_eq!(settings.server_host, "192.168.1.20");
        assert!(!settings.advanced_endpoints);
        assert_eq!(settings.server, "192.168.1.20:40000".parse().unwrap());
        assert_eq!(settings.control, "ws://192.168.1.20:40001");
        assert_eq!(settings.admin, None);
    }

    #[test]
    fn settings_build_platform_neutral_runtime_config() {
        let settings = AppSettings {
            codec: Codec::Opus,
            input_backend: desktop::AudioInputBackend::VoiceProcessing,
            input_channel: desktop::InputChannelMode::Left,
            button_count: 8,
            buttons: vec!["director=Director".to_string()],
            ..AppSettings::default()
        };

        let runtime = settings.client_runtime_config(false).unwrap();

        assert_eq!(runtime.codec, Codec::Opus);
        assert_eq!(
            runtime.input_backend,
            ClientAudioBackendKind::VoiceProcessing
        );
        assert_eq!(runtime.input_channel, ClientInputChannelMode::Left);
        assert_eq!(runtime.button_count, 8);
        assert_eq!(runtime.buttons, vec!["director=Director"]);
        assert!(!runtime.list_devices);
    }

    #[test]
    fn cli_values_override_file_settings() {
        let mut settings = AppSettings {
            user_id: Some(7),
            tx_channel: 2,
            disable_local_ui: true,
            buttons: vec!["old=Old".to_string()],
            ..AppSettings::default()
        };
        let mut args = args();
        args.user_id = Some(9);
        args.tx_channel = Some(5);
        args.codec = Some(AppCodec::Pcm48);
        args.opus_profile = Some(AppOpusProfile::Speech48High);
        args.input_backend = Some(desktop::AudioInputBackend::Raw);
        args.button_count = Some(4);
        args.buttons = vec!["director=Director".to_string()];
        args.local_ui_token = Some("secret".to_string());
        args.enable_local_ui = true;
        args.window_mode = Some(AppWindowMode::Disabled);
        args.app_title = Some("Bench Client".to_string());
        args.ui_open_delay_ms = Some(25);

        settings.merge_cli(&args);

        assert_eq!(settings.user_id, Some(9));
        assert_eq!(settings.tx_channel, 5);
        assert_eq!(settings.codec, Codec::Pcm48);
        assert_eq!(settings.opus_profile, OpusProfile::Speech48High);
        assert_eq!(settings.input_backend, desktop::AudioInputBackend::Raw);
        assert_eq!(settings.button_count, 4);
        assert_eq!(settings.buttons, vec!["director=Director"]);
        assert_eq!(settings.local_ui_token.as_deref(), Some("secret"));
        assert!(!settings.disable_local_ui);
        assert_eq!(settings.window_mode, AppWindowMode::Disabled);
        assert_eq!(settings.app_title, "Bench Client");
        assert_eq!(settings.ui_open_delay_ms, 25);
    }

    #[test]
    fn settings_json_uses_protocol_codec_names() {
        let settings = AppSettings {
            codec: Codec::Pcm48,
            ..AppSettings::default()
        };

        let json = serde_json::to_string(&settings).unwrap();

        assert!(json.contains("\"codec\":\"pcm48\""));
        assert!(json.contains("\"opus_profile\":\"speech_24_standard\""));
        assert!(json.contains("\"input_backend\":\"auto\""));
        assert!(json.contains("\"window_mode\":\"system_browser\""));
        assert!(json.contains("\"server_profiles\":[]"));
        assert_eq!(
            serde_json::from_str::<AppSettings>(&json).unwrap().codec,
            Codec::Pcm48
        );
    }

    #[test]
    fn mobile_server_profiles_are_normalized_deduplicated_and_capped() {
        let mut settings = AppSettings::default();
        let first = MobileServerProfile {
            name: "  Studio A  ".to_string(),
            server: "  192.168.1.20:40000  ".to_string(),
            control: "  ws://192.168.1.20:40001  ".to_string(),
            admin: Some("  http://192.168.1.20:40002  ".to_string()),
            auth: Some(" required ".to_string()),
            version: Some(" 0.1.0 ".to_string()),
            discovered: true,
            ..MobileServerProfile::default()
        };

        remember_mobile_profile(&mut settings, first);

        assert_eq!(settings.server_profiles.len(), 1);
        assert_eq!(
            settings.server_profiles[0].id,
            "Studio A|ws://192.168.1.20:40001"
        );
        assert_eq!(settings.server_profiles[0].name, "Studio A");
        assert_eq!(settings.server_profiles[0].server, "192.168.1.20:40000");
        assert_eq!(
            settings.server_profiles[0].control,
            "ws://192.168.1.20:40001"
        );
        assert_eq!(
            settings.server_profiles[0].admin.as_deref(),
            Some("http://192.168.1.20:40002")
        );
        assert_eq!(
            settings.server_profiles[0].auth.as_deref(),
            Some("required")
        );

        remember_mobile_profile(
            &mut settings,
            MobileServerProfile {
                name: "Studio A renamed".to_string(),
                server: "192.168.1.21:40000".to_string(),
                control: "ws://192.168.1.20:40001".to_string(),
                ..MobileServerProfile::default()
            },
        );

        assert_eq!(settings.server_profiles.len(), 1);
        assert_eq!(settings.server_profiles[0].name, "Studio A renamed");
        assert_eq!(settings.server_profiles[0].server, "192.168.1.21:40000");

        for index in 0..25 {
            remember_mobile_profile(
                &mut settings,
                MobileServerProfile {
                    name: format!("Server {index}"),
                    server: format!("192.168.1.{index}:40000"),
                    control: format!("ws://192.168.1.{index}:40001"),
                    ..MobileServerProfile::default()
                },
            );
        }

        assert_eq!(settings.server_profiles.len(), 20);
        assert_eq!(settings.server_profiles[0].name, "Server 24");
    }

    #[test]
    fn desktop_args_parse_button_strings() {
        let settings = AppSettings {
            user_id: Some(1),
            buttons: vec!["director=Director".to_string()],
            button_keys: vec!["director=d".to_string()],
            ..AppSettings::default()
        };

        let args = settings.desktop_args(false).unwrap();

        assert_eq!(args.user_id, Some(1));
        assert_eq!(args.server_host.as_deref(), Some("127.0.0.1"));
        assert_eq!(args.button_count, DEFAULT_CLIENT_BUTTON_COUNT);
        assert_eq!(args.buttons.len(), 1);
        assert_eq!(args.button_keys.len(), 1);
    }

    #[test]
    fn validates_settings_before_launch_or_save() {
        let mut settings = AppSettings {
            jitter_ms: 251,
            ..AppSettings::default()
        };
        assert!(settings.validate().is_err());

        settings.jitter_ms = 40;
        settings.mic_gain = 9.0;
        assert!(settings.validate().is_err());

        settings.mic_gain = 1.0;
        settings.buttons = vec!["=NoId".to_string()];
        assert!(settings.validate().is_err());

        settings.buttons.clear();
        settings.app_title = "   ".to_string();
        assert!(settings.validate().is_err());

        settings.app_title = "Intercom Suite".to_string();
        settings.ui_open_delay_ms = 30_001;
        assert!(settings.validate().is_err());

        settings.ui_open_delay_ms = 750;
        settings.button_count = MAX_CLIENT_BUTTON_COUNT + 1;
        assert!(settings.validate().is_err());

        settings.button_count = DEFAULT_CLIENT_BUTTON_COUNT;
        settings.server_host = "http://127.0.0.1".to_string();
        assert!(settings.validate().is_err());

        settings.server_host = "127.0.0.1".to_string();
        settings.advanced_endpoints = true;
        settings.control = "http://127.0.0.1:40001".to_string();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn launch_plan_opens_system_browser_when_ui_enabled() {
        let mut settings = AppSettings {
            local_ui_bind: "127.0.0.1:0".parse().unwrap(),
            ..AppSettings::default()
        };

        let plan = build_launch_plan(&mut settings, false).unwrap();

        assert!(plan.opens_window);
        assert_eq!(plan.window_mode, AppWindowMode::SystemBrowser);
        assert_eq!(plan.app_title, "Intercom Suite");
        assert!(plan.local_ui_url.unwrap().starts_with("http://127.0.0.1:"));
        assert_ne!(settings.local_ui_bind.port(), 0);
    }

    #[test]
    fn launch_plan_does_not_open_when_disabled_or_listing_devices() {
        let mut disabled_settings = AppSettings {
            disable_local_ui: true,
            ..AppSettings::default()
        };
        let disabled_plan = build_launch_plan(&mut disabled_settings, false).unwrap();
        assert!(!disabled_plan.opens_window);
        assert_eq!(disabled_plan.local_ui_url, None);

        let mut no_window_settings = AppSettings {
            window_mode: AppWindowMode::Disabled,
            ..AppSettings::default()
        };
        let no_window_plan = build_launch_plan(&mut no_window_settings, false).unwrap();
        assert!(!no_window_plan.opens_window);
        assert_eq!(no_window_plan.local_ui_url, None);

        let mut list_devices_settings = AppSettings::default();
        let list_devices_plan = build_launch_plan(&mut list_devices_settings, true).unwrap();
        assert!(!list_devices_plan.opens_window);
        assert_eq!(list_devices_plan.local_ui_url, None);
    }

    #[test]
    fn launch_plan_prepares_local_ui_for_native_window() {
        let mut settings = AppSettings {
            window_mode: AppWindowMode::Native,
            local_ui_bind: "127.0.0.1:0".parse().unwrap(),
            ..AppSettings::default()
        };

        let plan = build_launch_plan(&mut settings, false).unwrap();

        assert!(plan.opens_window);
        assert_eq!(plan.window_mode, AppWindowMode::Native);
        assert!(plan.local_ui_url.unwrap().starts_with("http://127.0.0.1:"));
    }

    #[test]
    fn choose_available_bind_uses_next_port_when_taken() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = listener.local_addr().unwrap();

        let available = choose_available_bind(taken).unwrap();

        assert_ne!(available.port(), taken.port());
        assert_eq!(available.ip(), taken.ip());
    }

    #[test]
    fn open_command_for_platform_uses_expected_commands() {
        let url = "http://127.0.0.1:41002/";

        let macos = open_command_for_platform(url, HostPlatform::Macos);
        assert_eq!(macos.program, "open");
        assert_eq!(macos.args, vec![url]);

        let linux = open_command_for_platform(url, HostPlatform::Linux);
        assert_eq!(linux.program, "xdg-open");
        assert_eq!(linux.args, vec![url]);

        let windows = open_command_for_platform(url, HostPlatform::Windows);
        assert_eq!(windows.program, "cmd");
        assert_eq!(windows.args, vec!["/C", "start", "", url]);
    }

    #[test]
    fn load_missing_settings_returns_defaults() {
        let path = unique_test_path("missing-settings.json");

        let settings = load_settings(&path).unwrap();

        assert_eq!(settings, AppSettings::default());
    }

    #[test]
    fn save_settings_creates_parent_and_round_trips() {
        let path = unique_test_path("nested/settings.json");
        let settings = AppSettings {
            user_id: Some(42),
            codec: Codec::Pcm48,
            buttons: vec!["director=Director".to_string()],
            ..AppSettings::default()
        };

        save_settings(&path, &settings).unwrap();

        assert_eq!(load_settings(&path).unwrap(), settings);
        let _ = fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn temp_settings_path_stays_next_to_target() {
        let path = PathBuf::from("config/app.json");

        let tmp = temp_settings_path(&path);

        assert_eq!(tmp.parent(), Some(Path::new("config")));
        assert!(tmp
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("app.json."));
    }

    #[test]
    fn tauri_assets_and_bundle_config_are_present() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let config_path = root.join("tauri.conf.json");
        let config: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(config_path).unwrap()).unwrap();

        assert_eq!(config["productName"], "Intercom Suite");
        assert_eq!(config["mainBinaryName"], "app-native");
        assert_eq!(config["build"]["frontendDist"], "tauri-assets");
        assert_eq!(config["app"]["windows"].as_array().unwrap().len(), 0);
        assert_eq!(config["bundle"]["iOS"]["minimumSystemVersion"], "15.0");
        assert_eq!(config["bundle"]["android"]["minSdkVersion"], 26);

        for asset in [
            "tauri-assets/index.html",
            "tauri-assets/mobile.html",
            "tauri-assets/mobile.css",
            "tauri-assets/mobile.js",
            "tauri-assets/client-controls.html",
            "tauri-assets/client-controls.css",
            "tauri-assets/client-controls.js",
            "tauri-assets/client-api.js",
            "tauri-assets/settings.html",
            "tauri-assets/settings.css",
            "tauri-assets/settings.js",
            "tauri.ios.conf.json",
            "icons/icon.png",
            "icons/icon.ico",
            "icons/icon.icns",
            "scripts/package-native.sh",
            "scripts/mobile-doctor.sh",
            "scripts/ios-dev.sh",
            "scripts/ios-clean-build-output.sh",
            "scripts/ios-build-sim.sh",
            "scripts/ios-device-build.sh",
            "scripts/ios-device-dev.sh",
            "Info.ios.plist",
            "src/ios_mobile.m",
            "../../docs/generated-artifacts.md",
            "../../tools/check-generated-artifacts.sh",
            "gen/android/app/src/main/AndroidManifest.xml",
            "gen/android/app/src/main/java/com/intercomsuite/client/MainActivity.kt",
            "gen/apple/project.yml",
            "gen/apple/app_iOS/Info.plist",
        ] {
            assert!(root.join(asset).is_file(), "missing {asset}");
        }

        let settings_html = fs::read_to_string(root.join("tauri-assets/settings.html")).unwrap();
        let settings_js = fs::read_to_string(root.join("tauri-assets/settings.js")).unwrap();
        let mobile_html = fs::read_to_string(root.join("tauri-assets/mobile.html")).unwrap();
        let mobile_js = fs::read_to_string(root.join("tauri-assets/mobile.js")).unwrap();
        let mobile_css = fs::read_to_string(root.join("tauri-assets/mobile.css")).unwrap();
        let controls_html =
            fs::read_to_string(root.join("tauri-assets/client-controls.html")).unwrap();
        let controls_js = fs::read_to_string(root.join("tauri-assets/client-controls.js")).unwrap();
        let controls_api = fs::read_to_string(root.join("tauri-assets/client-api.js")).unwrap();
        let controls_css =
            fs::read_to_string(root.join("tauri-assets/client-controls.css")).unwrap();
        let shared_controls_root = root.join("../shared-ui/talking");
        let shared_controls_html =
            fs::read_to_string(shared_controls_root.join("client-controls.html")).unwrap();
        let shared_controls_js =
            fs::read_to_string(shared_controls_root.join("client-controls.js")).unwrap();
        let shared_controls_css =
            fs::read_to_string(shared_controls_root.join("client-controls.css")).unwrap();
        let shared_tauri_api =
            fs::read_to_string(shared_controls_root.join("client-api-tauri.js")).unwrap();
        let ios_dev_script = fs::read_to_string(root.join("scripts/ios-dev.sh")).unwrap();
        let ios_config = fs::read_to_string(root.join("tauri.ios.conf.json")).unwrap();
        let ios_clean_script =
            fs::read_to_string(root.join("scripts/ios-clean-build-output.sh")).unwrap();
        let ios_build_script = fs::read_to_string(root.join("scripts/ios-build-sim.sh")).unwrap();
        let ios_device_build_script =
            fs::read_to_string(root.join("scripts/ios-device-build.sh")).unwrap();
        let ios_device_dev_script =
            fs::read_to_string(root.join("scripts/ios-device-dev.sh")).unwrap();
        let ios_info = fs::read_to_string(root.join("Info.ios.plist")).unwrap();
        let ios_bridge = fs::read_to_string(root.join("src/ios_mobile.m")).unwrap();
        let android_manifest =
            fs::read_to_string(root.join("gen/android/app/src/main/AndroidManifest.xml")).unwrap();
        assert!(settings_html.contains("input_backend"));
        assert!(settings_html.contains("button_count"));
        assert!(settings_html.contains("server_host"));
        assert!(settings_html.contains("advanced_endpoints"));
        assert!(settings_js.contains("input_backend"));
        assert!(settings_js.contains("button_count"));
        assert!(settings_js.contains("server_host"));
        assert!(settings_js.contains("advanced_endpoints"));
        assert!(settings_js.contains("invoke("));
        assert!(!settings_js.contains("fetch("));
        assert!(mobile_html.contains("mobile-form"));
        assert!(mobile_html.contains("server-picker"));
        assert!(mobile_html.contains("scan-servers"));
        assert!(mobile_html.contains("button_count"));
        assert!(mobile_html.contains("server_host"));
        assert!(mobile_html.contains("advanced_endpoints"));
        assert!(mobile_js.contains("invoke("));
        assert!(mobile_js.contains("button_count"));
        assert!(mobile_js.contains("server_host"));
        assert!(mobile_js.contains("advanced_endpoints"));
        assert!(mobile_js.contains("mobile_start_client"));
        assert!(mobile_js.contains("mobile_open_controls"));
        assert!(mobile_js.contains("mobile_discover_servers"));
        assert!(mobile_js.contains("mobile_select_server"));
        assert!(mobile_js.contains("mobile_forget_server"));
        assert!(mobile_js.contains("server_profiles"));
        assert!(mobile_js.contains("status.phase"));
        assert!(!mobile_js.contains("fetch("));
        assert!(mobile_css.contains(".phone-shell"));
        assert!(mobile_css.contains(".server-panel"));
        assert_eq!(controls_html, shared_controls_html);
        assert_eq!(controls_js, shared_controls_js);
        assert_eq!(controls_css, shared_controls_css);
        assert_eq!(controls_api, shared_tauri_api);
        assert!(controls_html.contains("client-api.js"));
        assert!(controls_html.contains("client-controls.js"));
        assert!(controls_html.contains("id=\"client-title\" hidden"));
        assert!(controls_api.contains("invoke("));
        assert!(controls_api.contains("client_state"));
        assert!(controls_api.contains("client_talk_down"));
        assert!(controls_api.contains("client_config"));
        assert!(controls_js.contains("applyButtonColor"));
        assert!(controls_html.contains("channel-settings-modal"));
        assert!(controls_html.contains("channel-listen-toggle"));
        assert!(controls_html.contains("channel-tx-toggle"));
        assert!(controls_js.contains("function bindChannelSettingsGesture"));
        assert!(controls_js.contains("function saveChannelSettings()"));
        assert!(controls_js.contains("function channelIconTag"));
        assert!(controls_css.contains(".tag.icon-tag"));
        assert!(!controls_js.contains(">listening<"));
        assert!(!controls_js.contains(">regular tx<"));
        assert!(!controls_js.contains("clientApi?.name || 'client'} controls"));
        assert!(controls_js.contains("'Hold Talk to transmit'"));
        assert!(!controls_js.contains("fetch("));
        assert!(!controls_api.contains("fetch("));
        assert!(controls_css.contains(".phone-shell"));
        assert!(controls_css.contains("--dock-height"));
        assert!(ios_dev_script.contains("simctl bootstatus"));
        assert!(ios_dev_script.contains("cargo tauri ios dev"));
        assert!(ios_dev_script.contains("--features=\"${TAURI_IOS_FEATURES:-native}\""));
        assert!(ios_config.contains("beforeBuildCommand"));
        assert!(ios_config.contains("scripts/ios-clean-build-output.sh"));
        assert!(ios_config.contains("app/scripts/ios-clean-build-output.sh"));
        assert!(ios_clean_script.contains("*.xcarchive"));
        assert!(ios_clean_script.contains("*.ipa"));
        assert!(ios_clean_script.contains("arm64*"));
        assert!(ios_clean_script.contains(".stale"));
        assert!(ios_build_script.contains("cargo tauri ios build"));
        assert!(ios_build_script.contains("--target=aarch64-sim"));
        assert!(ios_build_script.contains("--features=\"${TAURI_IOS_FEATURES:-native}\""));
        assert!(ios_build_script.contains("ios-clean-build-output.sh"));
        assert!(ios_device_build_script.contains("APPLE_DEVELOPMENT_TEAM"));
        assert!(ios_device_build_script.contains("DEVELOPMENT_TEAM"));
        assert!(ios_device_build_script.contains("CODE_SIGN_STYLE"));
        assert!(ios_device_build_script.contains("-allowProvisioningDeviceRegistration"));
        assert!(ios_device_build_script.contains("aarch64-apple-ios"));
        assert!(ios_device_build_script.contains("--target=aarch64"));
        assert!(ios_device_dev_script.contains("APPLE_DEVELOPMENT_TEAM"));
        assert!(ios_device_dev_script.contains("DEVELOPMENT_TEAM"));
        assert!(ios_device_dev_script.contains("INTERCOM_REAL_XCODEBUILD"));
        assert!(ios_device_dev_script.contains("-allowProvisioningDeviceRegistration"));
        assert!(ios_device_dev_script.contains("aarch64-apple-ios"));
        assert!(ios_device_dev_script.contains("iPhone|iPad|iPod"));
        assert!(ios_device_dev_script.contains("no connected physical iPhone/iPad/iPod"));
        assert!(ios_device_dev_script.contains("DEVICE_NAME"));
        assert!(ios_device_dev_script.contains("cargo tauri ios dev \"$DEVICE_NAME\""));
        assert!(ios_info.contains("NSMicrophoneUsageDescription"));
        assert!(ios_info.contains("NSLocalNetworkUsageDescription"));
        assert!(ios_info.contains("NSBonjourServices"));
        assert!(ios_info.contains("_intercom-suite._tcp"));
        assert!(ios_bridge.contains("AVAudioSessionCategoryPlayAndRecord"));
        assert!(ios_bridge.contains("AVAudioSessionModeVoiceChat"));
        assert!(ios_bridge.contains("intercom_ios_browse_intercom_services"));
        assert!(android_manifest.contains("android.permission.RECORD_AUDIO"));
        assert!(android_manifest.contains("android.permission.INTERNET"));
    }

    #[test]
    fn mobile_shell_uses_operator_control_structure() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mobile_html = fs::read_to_string(root.join("tauri-assets/mobile.html")).unwrap();
        let mobile_css = fs::read_to_string(root.join("tauri-assets/mobile.css")).unwrap();

        for token in [
            "phone-shell",
            "topbar",
            "channels-panel",
            "server-panel",
            "server-picker",
            "bottom-dock",
            "control-button",
            "talk-button-main",
        ] {
            assert!(
                mobile_html.contains(token) || mobile_css.contains(token),
                "mobile shell missing {token}"
            );
        }
        assert!(mobile_html.contains("Intercom Suite"));
        assert!(!mobile_html.contains(">Mobile Client<"));
    }

    #[test]
    fn mobile_shell_keeps_setup_reachable_after_start() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mobile_js = fs::read_to_string(root.join("tauri-assets/mobile.js")).unwrap();

        assert!(mobile_js.contains("mobile_status"));
        assert!(mobile_js.contains("Client is running. Open Controls"));
        assert!(mobile_js.contains("close-config"));
        assert!(mobile_js.contains("async function openControls"));
        assert!(mobile_js.contains("await invoke('mobile_open_controls')"));
        assert!(mobile_js.contains("client-controls.html"));
        assert!(mobile_js.contains("window.location.href = currentLocalUiUrl"));
        assert!(!mobile_js.contains("window.location.replace(status.local_ui_url)"));
        assert!(mobile_js.contains("setControlsUrl(response.local_ui_url)"));
    }

    fn unique_test_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!("intercom-app-test-{}-{nanos}", std::process::id()))
            .join(name)
    }
}
