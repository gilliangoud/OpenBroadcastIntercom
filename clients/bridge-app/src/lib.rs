use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use axum::extract::ws::{Message as WsMessage, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderValue};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use clap::Parser;
use client_core::{
    default_audio_addr, derive_admin_url_from_control, run_control_connection,
    send_control_request, supported_codecs, AudioDecoder, ClientConfig, ClientEndpointOverrides,
    ClientServerEndpoint, ControlRequest, DEFAULT_SERVER_HOST,
};
use common::{
    AudioPacket, BridgeMode, ClientCapabilities, ClientLockoutPolicy, ClientRole, Codec,
    ControlMessage, ControlResponse, Esp32AudioConfig, IfbConfig, OpusProfile, ProcessingConfig,
    StereoConfig, TalkMode, TallyStatus,
};
use cpal::traits::{DeviceTrait, HostTrait};
use ndi_runtime::NdiRuntime;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{interval, Duration};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(about = "Cross-platform multi-route bridge launcher for production audio PCs")]
pub struct Args {
    #[arg(long, default_value = "127.0.0.1:41012")]
    pub bind: SocketAddr,
    #[arg(long, default_value = "intercom-bridge-app.json")]
    pub config_file: PathBuf,
    #[arg(long)]
    pub bridge_bin: Option<PathBuf>,
    #[arg(long)]
    pub server_host: Option<String>,
    #[arg(long)]
    pub server: Option<SocketAddr>,
    #[arg(long)]
    pub control: Option<String>,
    #[arg(long)]
    pub admin: Option<String>,
    #[arg(long)]
    pub init_config: bool,
    #[arg(long)]
    pub print_config: bool,
    #[arg(long)]
    pub no_open: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct BridgeAppConfig {
    app_title: String,
    server_host: String,
    server: SocketAddr,
    control: String,
    admin: Option<String>,
    advanced_endpoints: bool,
    bridge_bin: Option<PathBuf>,
    routes: Vec<BridgeRouteConfig>,
}

impl Default for BridgeAppConfig {
    fn default() -> Self {
        Self {
            app_title: "RedLine Bridge".to_string(),
            server_host: DEFAULT_SERVER_HOST.to_string(),
            server: default_audio_addr(),
            control: ClientServerEndpoint::default().control_url(),
            admin: None,
            advanced_endpoints: false,
            bridge_bin: None,
            routes: vec![BridgeRouteConfig {
                id: "program-in".to_string(),
                name: "Program Input".to_string(),
                user_id: 90,
                mode: BridgeMode::Input,
                tx_channels: vec![1],
                listen_channels: Vec::new(),
                note: "vMix or virtual audio program feed into RedLine".to_string(),
                ..BridgeRouteConfig::default()
            }],
        }
    }
}

impl BridgeAppConfig {
    fn endpoint_overrides(&self) -> ClientEndpointOverrides {
        ClientEndpointOverrides::normalized(
            &self.server_host,
            self.server,
            &self.control,
            self.admin.clone(),
            self.advanced_endpoints,
        )
    }

    fn normalize_endpoints(&mut self) {
        let endpoints = self.endpoint_overrides();
        self.server_host = endpoints.server_host;
        self.server = endpoints.server;
        self.control = endpoints.control;
        self.admin = endpoints.admin;
        self.advanced_endpoints = endpoints.advanced_endpoints;
    }

    fn merge_cli(&mut self, args: &Args) {
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
        if let Some(admin) = &args.admin {
            self.admin = Some(admin.clone());
            self.advanced_endpoints = true;
        }
        if let Some(bridge_bin) = &args.bridge_bin {
            self.bridge_bin = Some(bridge_bin.clone());
        }
        self.normalize_endpoints();
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.app_title.trim().is_empty() {
            bail!("app_title cannot be empty");
        }
        self.endpoint_overrides().validate()?;
        let mut ids = std::collections::HashSet::new();
        let mut users = std::collections::HashSet::new();
        for route in &self.routes {
            route.validate()?;
            if !ids.insert(route.id.as_str()) {
                bail!("duplicate bridge route id `{}`", route.id);
            }
            if !users.insert(route.user_id) {
                bail!("duplicate bridge route user_id `{}`", route.user_id);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct BridgeRouteConfig {
    id: String,
    name: String,
    user_id: u16,
    client_uid: Option<String>,
    identity_file: Option<PathBuf>,
    mode: BridgeMode,
    tx_channels: Vec<u16>,
    listen_channels: Vec<u16>,
    codec: Codec,
    opus_profile: OpusProfile,
    stereo: bool,
    input_kind: RouteInputKind,
    output_kind: RouteOutputKind,
    input_device: Option<String>,
    output_device: Option<String>,
    ndi_source: Option<String>,
    ndi_output_name: Option<String>,
    ndi_groups: Option<String>,
    input_gain: f32,
    output_gain: f32,
    note: String,
    enabled: bool,
}

impl Default for BridgeRouteConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: "Bridge Route".to_string(),
            user_id: 90,
            client_uid: None,
            identity_file: None,
            mode: BridgeMode::Input,
            tx_channels: vec![1],
            listen_channels: Vec::new(),
            codec: Codec::Pcm48,
            opus_profile: OpusProfile::Speech48High,
            stereo: false,
            input_kind: RouteInputKind::AudioDevice,
            output_kind: RouteOutputKind::AudioDevice,
            input_device: None,
            output_device: None,
            ndi_source: None,
            ndi_output_name: None,
            ndi_groups: None,
            input_gain: 1.0,
            output_gain: 1.0,
            note: String::new(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RouteInputKind {
    #[default]
    AudioDevice,
    NdiSource,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RouteOutputKind {
    #[default]
    AudioDevice,
    VmixBrowserSource,
    NdiOutput,
}

impl BridgeRouteConfig {
    fn captures(&self) -> bool {
        matches!(self.mode, BridgeMode::Input | BridgeMode::Duplex)
    }

    fn plays(&self) -> bool {
        matches!(self.mode, BridgeMode::Output | BridgeMode::Duplex)
    }

    fn is_vmix_browser_source(&self) -> bool {
        self.plays() && self.output_kind == RouteOutputKind::VmixBrowserSource
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.id.trim().is_empty() {
            bail!("route id cannot be empty");
        }
        if self.user_id == 0 {
            bail!("route `{}` user_id must be 1..65535", self.id);
        }
        validate_gain("input_gain", self.input_gain)?;
        validate_gain("output_gain", self.output_gain)?;
        if self.captures() && self.tx_channels.is_empty() {
            bail!("route `{}` captures audio but has no TX channels", self.id);
        }
        if self.plays() && self.listen_channels.is_empty() {
            bail!("route `{}` plays audio but has no listen channels", self.id);
        }
        if self.input_kind == RouteInputKind::NdiSource && !matches!(self.mode, BridgeMode::Input) {
            bail!(
                "route `{}` uses an NDI source and must be an input route",
                self.id
            );
        }
        if self.output_kind == RouteOutputKind::VmixBrowserSource
            && !matches!(self.mode, BridgeMode::Output)
        {
            bail!(
                "route `{}` uses a vMix Browser Source and must be an output route",
                self.id
            );
        }
        if self.output_kind == RouteOutputKind::NdiOutput
            && !matches!(self.mode, BridgeMode::Output)
        {
            bail!(
                "route `{}` uses an NDI output and must be an output route",
                self.id
            );
        }
        if self.input_kind == RouteInputKind::NdiSource
            && self
                .ndi_source
                .as_deref()
                .is_none_or(|source| source.trim().is_empty())
        {
            bail!(
                "route `{}` uses NDI input but has no NDI source name",
                self.id
            );
        }
        if self.output_kind == RouteOutputKind::NdiOutput
            && self
                .ndi_output_name
                .as_deref()
                .is_none_or(|name| name.trim().is_empty())
        {
            bail!(
                "route `{}` uses NDI output but has no NDI output name",
                self.id
            );
        }
        if self
            .tx_channels
            .iter()
            .any(|channel| self.listen_channels.contains(channel))
        {
            bail!(
                "route `{}` listens and transmits on the same channel; split routes to avoid feedback",
                self.id
            );
        }
        if self.codec == Codec::Adpcm {
            bail!("route `{}` uses unsupported ADPCM codec", self.id);
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct BridgeAppStateResponse {
    config: BridgeAppConfig,
    bridge_bin: String,
    base_url: String,
    routes: Vec<RouteRuntimeStatus>,
    input_devices: Vec<String>,
    output_devices: Vec<String>,
    ndi_available: bool,
    ndi_sources: Vec<String>,
    ndi_error: Option<String>,
    channels: Vec<ChannelOption>,
    discovery_warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ChannelOption {
    id: u16,
    name: String,
}

#[derive(Debug, Clone, Serialize)]
struct RouteRuntimeStatus {
    id: String,
    running: bool,
    pid: Option<u32>,
    started_at_ms: Option<u64>,
    exit: Option<String>,
    source_url: Option<String>,
    connected_clients: u32,
    warning: Option<String>,
    audio_level: Option<f32>,
    audio_frames: u64,
    underflows: u64,
    drops: u64,
    stale: bool,
    last_audio_ms_ago: Option<u64>,
    runtime: Option<String>,
}

#[derive(Debug)]
struct RunningRoute {
    child: Child,
    started_at_ms: u64,
}

#[derive(Debug)]
struct BrowserSourceRuntime {
    started_at_ms: u64,
    audio_tx: broadcast::Sender<Vec<u8>>,
    telemetry: Arc<BrowserSourceTelemetry>,
    task: JoinHandle<anyhow::Result<()>>,
}

#[derive(Debug, Default)]
struct BrowserSourceTelemetry {
    audio_level_ppm: AtomicU32,
    audio_frames: AtomicU64,
    underflows: AtomicU64,
    drops: AtomicU64,
    last_audio_ms: AtomicU64,
}

#[derive(Debug, Clone, Default)]
struct BrowserSourceTelemetrySnapshot {
    audio_level: Option<f32>,
    audio_frames: u64,
    underflows: u64,
    drops: u64,
    stale: bool,
    last_audio_ms_ago: Option<u64>,
}

impl BrowserSourceTelemetry {
    fn record_audio(&self, level: f32) {
        self.audio_level_ppm.store(
            (level.clamp(0.0, 1.0) * 1_000_000.0).round() as u32,
            Ordering::Relaxed,
        );
        self.audio_frames.fetch_add(1, Ordering::Relaxed);
        self.last_audio_ms.store(unix_time_ms(), Ordering::Relaxed);
    }

    fn record_underflow(&self) {
        self.underflows.fetch_add(1, Ordering::Relaxed);
    }

    fn record_drop(&self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }

    fn record_drops(&self, count: u64) {
        self.drops.fetch_add(count, Ordering::Relaxed);
    }

    fn snapshot(&self) -> BrowserSourceTelemetrySnapshot {
        let frames = self.audio_frames.load(Ordering::Relaxed);
        let last_audio_ms = self.last_audio_ms.load(Ordering::Relaxed);
        let last_audio_ms_ago =
            (last_audio_ms > 0).then_some(unix_time_ms().saturating_sub(last_audio_ms));
        BrowserSourceTelemetrySnapshot {
            audio_level: (frames > 0)
                .then_some(self.audio_level_ppm.load(Ordering::Relaxed) as f32 / 1_000_000.0),
            audio_frames: frames,
            underflows: self.underflows.load(Ordering::Relaxed),
            drops: self.drops.load(Ordering::Relaxed),
            stale: frames == 0 || last_audio_ms_ago.is_some_and(|age| age > 2_000),
            last_audio_ms_ago,
        }
    }
}

#[derive(Debug)]
struct AppState {
    config_file: PathBuf,
    bridge_bin_override: Option<PathBuf>,
    base_url: String,
    config: RwLock<BridgeAppConfig>,
    children: Mutex<HashMap<String, RunningRoute>>,
    browser_sources: Mutex<HashMap<String, BrowserSourceRuntime>>,
    browser_connections: Mutex<HashMap<String, u32>>,
}

#[derive(Debug, Serialize)]
struct OkResponse {
    ok: bool,
}

const MIX_FRAME_FLOAT_BYTES: usize = 960 * std::mem::size_of::<f32>();

pub fn init_tracing() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("bridge_app=info".parse()?))
        .try_init();
    Ok(())
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    let mut config = load_config(&args.config_file)?;
    config.merge_cli(&args);
    config.validate()?;

    if args.print_config {
        println!("{}", serde_json::to_string_pretty(&config)?);
        return Ok(());
    }

    if args.init_config {
        save_config(&args.config_file, &config)?;
        eprintln!("wrote bridge app config to {}", args.config_file.display());
        return Ok(());
    }

    let listener = TcpListener::bind(args.bind)
        .await
        .with_context(|| format!("bind bridge app at {}", args.bind))?;
    let url = format!("http://{}", listener.local_addr()?);
    let state = Arc::new(AppState {
        config_file: args.config_file.clone(),
        bridge_bin_override: args.bridge_bin.clone(),
        base_url: url.clone(),
        config: RwLock::new(config),
        children: Mutex::new(HashMap::new()),
        browser_sources: Mutex::new(HashMap::new()),
        browser_connections: Mutex::new(HashMap::new()),
    });
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/app.js", get(js_handler))
        .route("/style.css", get(css_handler))
        .route("/api/state", get(state_handler))
        .route("/api/config", put(config_handler))
        .route("/api/routes/start-all", post(start_all_handler))
        .route("/api/routes/stop-all", post(stop_all_handler))
        .route("/api/routes/:route_id/start", post(start_route_handler))
        .route("/api/routes/:route_id/stop", post(stop_route_handler))
        .route("/vmix/source/:route_id", get(vmix_source_handler))
        .route("/vmix/source/:route_id/ws", get(vmix_source_ws_handler))
        .with_state(Arc::clone(&state));
    if !args.no_open {
        spawn_window_opener(url.clone());
    }
    tracing::info!(%url, "bridge app listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn load_config(path: &Path) -> anyhow::Result<BridgeAppConfig> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let mut config: BridgeAppConfig = serde_json::from_str(&text)
                .with_context(|| format!("parse bridge app config from {}", path.display()))?;
            config.normalize_endpoints();
            Ok(config)
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {
            let mut config = BridgeAppConfig::default();
            config.normalize_endpoints();
            Ok(config)
        }
        Err(err) => {
            Err(err).with_context(|| format!("read bridge app config from {}", path.display()))
        }
    }
}

fn save_config(path: &Path, config: &BridgeAppConfig) -> anyhow::Result<()> {
    let mut config = config.clone();
    config.normalize_endpoints();
    config.validate()?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("create bridge app config directory {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&config)?;
    let tmp_path = temp_config_path(path);
    fs::write(&tmp_path, format!("{json}\n"))
        .with_context(|| format!("write bridge app config to {}", tmp_path.display()))?;
    fs::rename(&tmp_path, path)
        .with_context(|| format!("replace bridge app config at {}", path.display()))?;
    Ok(())
}

fn temp_config_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "intercom-bridge-app.json".into());
    file_name.push(format!(".{}.tmp", std::process::id()));
    path.with_file_name(file_name)
}

async fn index_handler() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn js_handler() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/javascript"),
        )],
        APP_JS,
    )
        .into_response()
}

async fn css_handler() -> Response {
    (
        [(header::CONTENT_TYPE, HeaderValue::from_static("text/css"))],
        STYLE_CSS,
    )
        .into_response()
}

async fn state_handler(State(state): State<Arc<AppState>>) -> Json<BridgeAppStateResponse> {
    Json(state_snapshot(&state).await)
}

async fn vmix_source_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(route_id): AxumPath<String>,
) -> Result<Html<String>, BridgeAppError> {
    let config = state.config.read().await;
    let route = config
        .routes
        .iter()
        .find(|route| route.id == route_id)
        .ok_or_else(|| BridgeAppError::bad_request(format!("unknown bridge route `{route_id}`")))?;
    if !route.is_vmix_browser_source() {
        return Err(BridgeAppError::bad_request(format!(
            "route `{route_id}` is not a vMix Browser Source output"
        )));
    }
    Ok(Html(vmix_source_page(route)))
}

async fn vmix_source_ws_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(route_id): AxumPath<String>,
    ws: WebSocketUpgrade,
) -> Result<Response, BridgeAppError> {
    let config = state.config.read().await;
    let Some(route) = config.routes.iter().find(|route| route.id == route_id) else {
        return Err(BridgeAppError::bad_request(format!(
            "unknown bridge route `{route_id}`"
        )));
    };
    if !route.is_vmix_browser_source() {
        return Err(BridgeAppError::bad_request(format!(
            "route `{route_id}` is not a vMix Browser Source output"
        )));
    }
    drop(config);
    Ok(ws
        .on_upgrade(move |socket| run_vmix_source_socket(state, route_id, socket))
        .into_response())
}

async fn run_vmix_source_socket(
    state: Arc<AppState>,
    route_id: String,
    mut socket: axum::extract::ws::WebSocket,
) {
    let route_runtime = state
        .browser_sources
        .lock()
        .await
        .get(&route_id)
        .map(|runtime| (runtime.audio_tx.subscribe(), Arc::clone(&runtime.telemetry)));
    {
        let mut connections = state.browser_connections.lock().await;
        *connections.entry(route_id.clone()).or_insert(0) += 1;
    }
    let mut tick = interval(Duration::from_millis(20));
    let silence = vec![0_u8; MIX_FRAME_FLOAT_BYTES];
    if let Some((mut audio_rx, telemetry)) = route_runtime {
        loop {
            if !state.browser_sources.lock().await.contains_key(&route_id) {
                break;
            }
            let bytes = match tokio::time::timeout(Duration::from_millis(40), audio_rx.recv()).await
            {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(broadcast::error::RecvError::Lagged(skipped))) => {
                    telemetry.record_drops(skipped);
                    continue;
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => break,
                Err(_) => {
                    telemetry.record_underflow();
                    silence.clone()
                }
            };
            if socket.send(WsMessage::Binary(bytes.into())).await.is_err() {
                break;
            }
        }
        decrement_browser_connection(&state, &route_id).await;
        return;
    }
    loop {
        tick.tick().await;
        if !state.browser_sources.lock().await.contains_key(&route_id) {
            break;
        }
        if socket
            .send(WsMessage::Binary(silence.clone().into()))
            .await
            .is_err()
        {
            break;
        }
    }
    decrement_browser_connection(&state, &route_id).await;
}

async fn decrement_browser_connection(state: &AppState, route_id: &str) {
    let mut connections = state.browser_connections.lock().await;
    if let Some(count) = connections.get_mut(route_id) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            connections.remove(route_id);
        }
    }
}

async fn config_handler(
    State(state): State<Arc<AppState>>,
    Json(mut config): Json<BridgeAppConfig>,
) -> Result<Json<OkResponse>, BridgeAppError> {
    config.normalize_endpoints();
    config.validate().map_err(BridgeAppError::bad_request)?;
    save_config(&state.config_file, &config).map_err(BridgeAppError::internal)?;
    *state.config.write().await = config;
    Ok(Json(OkResponse { ok: true }))
}

async fn start_route_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(route_id): AxumPath<String>,
) -> Result<Json<OkResponse>, BridgeAppError> {
    start_route(&state, &route_id).await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn stop_route_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(route_id): AxumPath<String>,
) -> Result<Json<OkResponse>, BridgeAppError> {
    stop_route(&state, &route_id).await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn start_all_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<OkResponse>, BridgeAppError> {
    let route_ids = state
        .config
        .read()
        .await
        .routes
        .iter()
        .filter(|route| route.enabled)
        .map(|route| route.id.clone())
        .collect::<Vec<_>>();
    for route_id in route_ids {
        start_route(&state, &route_id).await?;
    }
    Ok(Json(OkResponse { ok: true }))
}

async fn stop_all_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<OkResponse>, BridgeAppError> {
    let route_ids = state
        .children
        .lock()
        .await
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for route_id in route_ids {
        stop_route(&state, &route_id).await?;
    }
    Ok(Json(OkResponse { ok: true }))
}

async fn state_snapshot(state: &AppState) -> BridgeAppStateResponse {
    let config = state.config.read().await.clone();
    let bridge_bin = bridge_bin_path(state, &config).display().to_string();
    let mut discovery_warnings = Vec::new();
    let input_devices = match audio_device_names(AudioDeviceKind::Input) {
        Ok(devices) => devices,
        Err(err) => {
            discovery_warnings.push(format!("input device discovery failed: {err}"));
            Vec::new()
        }
    };
    let output_devices = match audio_device_names(AudioDeviceKind::Output) {
        Ok(devices) => devices,
        Err(err) => {
            discovery_warnings.push(format!("output device discovery failed: {err}"));
            Vec::new()
        }
    };
    let (ndi_available, ndi_sources, ndi_error) = discover_ndi_sources();
    let uses_ndi_source = config
        .routes
        .iter()
        .any(|route| route.input_kind == RouteInputKind::NdiSource);
    let uses_ndi_output = config
        .routes
        .iter()
        .any(|route| route.output_kind == RouteOutputKind::NdiOutput);
    let uses_ndi = uses_ndi_source || uses_ndi_output;
    if uses_ndi {
        if let Some(err) = ndi_error.as_deref() {
            discovery_warnings.push(err.to_string());
        }
    }
    if ndi_available && uses_ndi_source && ndi_sources.is_empty() {
        discovery_warnings
            .push("NDI runtime is available but no sources were discovered".to_string());
    }
    let channels = match channel_options_for_config(&config).await {
        Ok(channels) => channels,
        Err(err) => {
            discovery_warnings.push(format!("server channel discovery failed: {err}"));
            fallback_channel_options(&config)
        }
    };
    let mut children = state.children.lock().await;
    let browser_sources = state.browser_sources.lock().await;
    let browser_connections = state.browser_connections.lock().await;
    let mut statuses = Vec::new();
    for route in &config.routes {
        let mut status = RouteRuntimeStatus {
            id: route.id.clone(),
            running: false,
            pid: None,
            started_at_ms: None,
            exit: None,
            source_url: route
                .is_vmix_browser_source()
                .then(|| vmix_source_url(state, route)),
            connected_clients: browser_connections.get(&route.id).copied().unwrap_or(0),
            warning: None,
            audio_level: None,
            audio_frames: 0,
            underflows: 0,
            drops: 0,
            stale: false,
            last_audio_ms_ago: None,
            runtime: None,
        };
        if let Some(running) = browser_sources.get(&route.id) {
            let telemetry = running.telemetry.snapshot();
            status.running = !running.task.is_finished();
            status.started_at_ms = Some(running.started_at_ms);
            status.audio_level = telemetry.audio_level;
            status.audio_frames = telemetry.audio_frames;
            status.underflows = telemetry.underflows;
            status.drops = telemetry.drops;
            status.stale = telemetry.stale;
            status.last_audio_ms_ago = telemetry.last_audio_ms_ago;
            status.runtime = Some("WebSocket + Web Audio".to_string());
            if running.task.is_finished() {
                status.exit = Some("browser source route stopped".to_string());
            } else if telemetry.stale {
                status.warning = Some(
                    "Browser source is running but has not received recent RedLine audio"
                        .to_string(),
                );
            } else {
                status.warning = Some(
                    "Browser source page is streaming RedLine audio to connected vMix browser inputs"
                        .to_string(),
                );
            }
        } else if let Some(running) = children.get_mut(&route.id) {
            match running.child.try_wait() {
                Ok(Some(exit)) => {
                    status.exit = Some(exit.to_string());
                }
                Ok(None) => {
                    status.running = true;
                    status.pid = running.child.id();
                    status.started_at_ms = Some(running.started_at_ms);
                }
                Err(err) => {
                    status.exit = Some(format!("status error: {err}"));
                }
            }
        }
        apply_ndi_route_status(
            route,
            &mut status,
            ndi_available,
            &ndi_sources,
            ndi_error.as_deref(),
        );
        if !status.running {
            children.remove(&route.id);
        }
        statuses.push(status);
    }
    BridgeAppStateResponse {
        config,
        bridge_bin,
        base_url: state.base_url.clone(),
        routes: statuses,
        input_devices,
        output_devices,
        ndi_available,
        ndi_sources,
        ndi_error,
        channels,
        discovery_warnings,
    }
}

fn apply_ndi_route_status(
    route: &BridgeRouteConfig,
    status: &mut RouteRuntimeStatus,
    ndi_available: bool,
    ndi_sources: &[String],
    ndi_error: Option<&str>,
) {
    let uses_ndi_source = route.input_kind == RouteInputKind::NdiSource;
    let uses_ndi_output = route.output_kind == RouteOutputKind::NdiOutput;
    if !uses_ndi_source && !uses_ndi_output {
        return;
    }
    status.runtime = Some(if ndi_available {
        "system NDI runtime".to_string()
    } else {
        "NDI runtime unavailable".to_string()
    });
    if !ndi_available {
        status.warning = ndi_error
            .map(str::to_string)
            .or_else(|| Some("NDI runtime unavailable".to_string()));
        status.stale = true;
        return;
    }
    if uses_ndi_source {
        let selected = route.ndi_source.as_deref().unwrap_or_default();
        if ndi_sources.is_empty() {
            status.warning = Some("NDI runtime ready, but no sources were discovered".to_string());
        } else if !selected.is_empty() && !ndi_sources.iter().any(|source| source == selected) {
            status.warning = Some(format!(
                "selected NDI source `{selected}` is not discovered"
            ));
        }
    }
}

async fn start_route(state: &AppState, route_id: &str) -> Result<(), BridgeAppError> {
    let config = state.config.read().await.clone();
    let route = config
        .routes
        .iter()
        .find(|route| route.id == route_id)
        .cloned()
        .ok_or_else(|| BridgeAppError::bad_request(format!("unknown bridge route `{route_id}`")))?;
    route.validate().map_err(BridgeAppError::bad_request)?;
    if route.is_vmix_browser_source() {
        let mut browser_sources = state.browser_sources.lock().await;
        if browser_sources.contains_key(route_id) {
            return Ok(());
        }
        let (audio_tx, _) = broadcast::channel(64);
        let telemetry = Arc::new(BrowserSourceTelemetry::default());
        let task = tokio::spawn(run_browser_source_route(
            config,
            route.clone(),
            audio_tx.clone(),
            Arc::clone(&telemetry),
        ));
        browser_sources.insert(
            route_id.to_string(),
            BrowserSourceRuntime {
                started_at_ms: unix_time_ms(),
                audio_tx,
                telemetry,
                task,
            },
        );
        return Ok(());
    }
    let bridge_bin = bridge_bin_path(state, &config);
    let args = bridge_args(&config, &route);

    let mut children = state.children.lock().await;
    if let Some(running) = children.get_mut(route_id) {
        match running.child.try_wait() {
            Ok(None) => return Ok(()),
            Ok(Some(_)) | Err(_) => {
                children.remove(route_id);
            }
        }
    }
    let mut command = Command::new(&bridge_bin);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let child = command.spawn().map_err(|err| {
        BridgeAppError::internal(format!(
            "start route `{route_id}` using {}: {err}",
            bridge_bin.display()
        ))
    })?;
    children.insert(
        route_id.to_string(),
        RunningRoute {
            child,
            started_at_ms: unix_time_ms(),
        },
    );
    Ok(())
}

async fn stop_route(state: &AppState, route_id: &str) -> Result<(), BridgeAppError> {
    if let Some(runtime) = state.browser_sources.lock().await.remove(route_id) {
        runtime.task.abort();
    }
    state.browser_connections.lock().await.remove(route_id);
    let mut children = state.children.lock().await;
    if let Some(mut running) = children.remove(route_id) {
        if running
            .child
            .try_wait()
            .map_err(BridgeAppError::internal)?
            .is_none()
        {
            running
                .child
                .kill()
                .await
                .map_err(BridgeAppError::internal)?;
        }
    }
    Ok(())
}

async fn run_browser_source_route(
    config: BridgeAppConfig,
    route: BridgeRouteConfig,
    audio_tx: broadcast::Sender<Vec<u8>>,
    telemetry: Arc<BrowserSourceTelemetry>,
) -> anyhow::Result<()> {
    let endpoints = config.endpoint_overrides();
    let server = endpoints.effective_server().unwrap_or(config.server);
    let control = endpoints
        .effective_control()
        .unwrap_or_else(|_| config.control.clone());
    let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    socket.connect(server).await?;
    let runtime_config = Arc::new(std::sync::Mutex::new(browser_source_client_config(&route)));
    let capabilities = ClientCapabilities::bridge();
    let (control_tx, control_rx) = mpsc::channel::<ControlRequest>(16);
    let control_task = tokio::spawn(run_control_connection(
        control,
        control_rx,
        Arc::clone(&runtime_config),
        None,
    ));
    let initial = runtime_config.lock().unwrap().clone();
    match send_control_request(
        &control_tx,
        ControlMessage::Hello {
            user_id: initial.user_id,
            requested_user_id: Some(initial.user_id),
            client_uid: initial.client_uid.clone(),
            codecs: supported_codecs(),
            buttons: Vec::new(),
            capabilities,
            role: ClientRole::Bridge,
        },
    )
    .await?
    {
        ControlResponse::Hello {
            user_id,
            client_uid,
            enrollment,
            ..
        } => {
            if enrollment != common::EnrollmentStatus::Enrolled {
                bail!("browser source enrollment is {enrollment:?}; waiting for admin approval");
            }
            let mut config = runtime_config.lock().unwrap();
            config.user_id = user_id;
            if !client_uid.is_empty() {
                config.client_uid = client_uid;
            }
        }
        ControlResponse::Ack => {}
        ControlResponse::Error { message } => bail!("{message}"),
        other => bail!("unexpected browser source hello response: {other:?}"),
    }
    let startup_config = runtime_config.lock().unwrap().clone();
    match send_control_request(&control_tx, startup_config.control_message()).await? {
        ControlResponse::Ack => {}
        ControlResponse::Error { message } => bail!("{message}"),
        other => bail!("unexpected browser source config response: {other:?}"),
    }

    let registration =
        run_browser_source_registration(Arc::clone(&socket), Arc::clone(&runtime_config));
    let receiver = run_browser_source_receiver(socket, runtime_config, audio_tx, telemetry);
    tokio::select! {
        result = control_task => result.context("browser source control task panicked")??,
        result = registration => result?,
        result = receiver => result?,
    }
    Ok(())
}

fn browser_source_client_config(route: &BridgeRouteConfig) -> ClientConfig {
    ClientConfig {
        user_id: route.user_id,
        client_uid: route
            .client_uid
            .clone()
            .unwrap_or_else(|| format!("bridge-app-vmix-{}", route.id)),
        role: ClientRole::Bridge,
        name: route.name.clone(),
        listen: route.listen_channels.clone(),
        tx: Vec::new(),
        codec: route.codec,
        opus_profile: route.opus_profile,
        talk_mode: TalkMode::Muted,
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
        capabilities: ClientCapabilities::bridge(),
        ifb: IfbConfig::default(),
        lockout: ClientLockoutPolicy::default(),
        stereo: StereoConfig {
            enabled: route.stereo,
            channel_pan: HashMap::new(),
        },
        esp32_audio: Esp32AudioConfig::default(),
        tally: TallyStatus::default(),
    }
}

async fn run_browser_source_registration(
    socket: Arc<UdpSocket>,
    config: Arc<std::sync::Mutex<ClientConfig>>,
) -> anyhow::Result<()> {
    let mut seq = 0_u16;
    let mut encoded = Vec::new();
    let mut tick = interval(Duration::from_secs(2));
    loop {
        tick.tick().await;
        let (user_id, codec) = {
            let config = config.lock().unwrap();
            (config.user_id, config.codec)
        };
        let packet = AudioPacket::registration(user_id, codec, seq);
        seq = seq.wrapping_add(1);
        packet.encode(&mut encoded)?;
        socket.send(&encoded).await?;
    }
}

async fn run_browser_source_receiver(
    socket: Arc<UdpSocket>,
    config: Arc<std::sync::Mutex<ClientConfig>>,
    audio_tx: broadcast::Sender<Vec<u8>>,
    telemetry: Arc<BrowserSourceTelemetry>,
) -> anyhow::Result<()> {
    let mut decoder = AudioDecoder::default();
    let mut buf = vec![0_u8; common::MAX_PACKET_BYTES];
    loop {
        let len = socket.recv(&mut buf).await?;
        let packet = match AudioPacket::decode(&buf[..len]) {
            Ok(packet) => packet,
            Err(err) => {
                telemetry.record_drop();
                tracing::warn!(%err, "dropped malformed browser source packet");
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
                telemetry.record_drop();
                tracing::warn!(%err, codec = ?packet.codec, "dropped invalid browser source packet");
                continue;
            }
        };
        let level = i16_peak_level(&samples);
        let bytes = samples_to_float32_bytes(&samples, receive_channels);
        telemetry.record_audio(level);
        if audio_tx.send(bytes).is_err() {
            telemetry.record_drop();
        }
    }
}

fn samples_to_float32_bytes(samples: &[i16], channels: usize) -> Vec<u8> {
    let channels = channels.max(1);
    let mut bytes = Vec::with_capacity((samples.len() / channels) * std::mem::size_of::<f32>());
    for frame in samples.chunks(channels) {
        let sum = frame
            .iter()
            .map(|sample| *sample as f32 / i16::MAX as f32)
            .sum::<f32>();
        let sample = (sum / frame.len().max(1) as f32).clamp(-1.0, 1.0);
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn i16_peak_level(samples: &[i16]) -> f32 {
    samples
        .iter()
        .map(|sample| (*sample as f32).abs() / i16::MAX as f32)
        .fold(0.0, f32::max)
        .clamp(0.0, 1.0)
}

#[derive(Debug, Clone, Copy)]
enum AudioDeviceKind {
    Input,
    Output,
}

fn audio_device_names(kind: AudioDeviceKind) -> anyhow::Result<Vec<String>> {
    let host = cpal::default_host();
    let devices = match kind {
        AudioDeviceKind::Input => host.input_devices().context("list input devices")?,
        AudioDeviceKind::Output => host.output_devices().context("list output devices")?,
    };
    let mut names = devices
        .filter_map(|device| device.name().ok())
        .filter(|name| !name.trim().is_empty())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    Ok(names)
}

fn discover_ndi_sources() -> (bool, Vec<String>, Option<String>) {
    let runtime = match NdiRuntime::load() {
        Ok(runtime) => runtime,
        Err(err) => {
            return (
                false,
                Vec::new(),
                Some(format!("NDI runtime unavailable: {err}")),
            );
        }
    };
    match runtime.find_sources(Duration::from_millis(250), None) {
        Ok(sources) => {
            let mut names = sources
                .into_iter()
                .map(|source| source.name)
                .filter(|name| !name.trim().is_empty())
                .collect::<Vec<_>>();
            names.sort();
            names.dedup();
            (true, names, None)
        }
        Err(err) => (
            true,
            Vec::new(),
            Some(format!("NDI source discovery failed: {err}")),
        ),
    }
}

async fn channel_options_for_config(
    config: &BridgeAppConfig,
) -> anyhow::Result<Vec<ChannelOption>> {
    let admin_base = config.endpoint_overrides().effective_admin().or_else(|_| {
        config
            .admin
            .clone()
            .or_else(|| derive_admin_url_from_control(&config.control))
            .context("no admin URL configured or derivable from control URL")
    })?;
    let state_url = admin_state_url(&admin_base);
    let channels = tokio::time::timeout(
        std::time::Duration::from_millis(750),
        fetch_admin_channels(&state_url),
    )
    .await
    .context("admin channel request timed out")??;
    Ok(merge_channel_options(channels, config))
}

async fn fetch_admin_channels(url: &str) -> anyhow::Result<Vec<ChannelOption>> {
    let target = parse_http_url(url).with_context(|| format!("parse admin URL `{url}`"))?;
    let mut stream = TcpStream::connect((target.host.as_str(), target.port))
        .await
        .with_context(|| format!("connect admin server at {}:{}", target.host, target.port))?;
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
        target.path, target.host_header
    );
    stream
        .write_all(request.as_bytes())
        .await
        .context("send admin state request")?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .context("read admin state response")?;
    let response = String::from_utf8(response).context("admin state response is not utf-8")?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .context("admin state response has no header/body split")?;
    if !headers.starts_with("HTTP/1.1 200") && !headers.starts_with("HTTP/1.0 200") {
        bail!(
            "admin state returned {}",
            headers.lines().next().unwrap_or("unknown status")
        );
    }
    let value: serde_json::Value = serde_json::from_str(body).context("parse admin state JSON")?;
    let channels = value
        .get("channels")
        .and_then(serde_json::Value::as_array)
        .context("admin state JSON has no channels array")?;
    let mut options = Vec::new();
    for channel in channels {
        let Some(id) = channel
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .and_then(|id| u16::try_from(id).ok())
        else {
            continue;
        };
        let name = channel
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        options.push(ChannelOption { id, name });
    }
    if options.is_empty() {
        bail!("admin state returned no channel options");
    }
    Ok(options)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedHttpUrl {
    host: String,
    host_header: String,
    port: u16,
    path: String,
}

fn parse_http_url(url: &str) -> Option<ParsedHttpUrl> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = rest
        .split_once('/')
        .map(|(authority, path)| (authority, format!("/{path}")))
        .unwrap_or((rest, "/".to_string()));
    let (host, port) = split_host_port(authority, 80)?;
    Some(ParsedHttpUrl {
        host,
        host_header: authority.to_string(),
        port,
        path,
    })
}

fn split_host_port(authority: &str, default_port: u16) -> Option<(String, u16)> {
    if authority.is_empty() {
        return None;
    }
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, after_host) = rest.split_once(']')?;
        let port = after_host
            .strip_prefix(':')
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or(default_port);
        return Some((host.to_string(), port));
    }
    if let Some((host, port)) = authority.rsplit_once(':') {
        if let Ok(port) = port.parse::<u16>() {
            return Some((host.to_string(), port));
        }
    }
    Some((authority.to_string(), default_port))
}

fn admin_state_url(admin_base: &str) -> String {
    let base = admin_base.trim().trim_end_matches('/');
    if base.ends_with("/admin/api/state") {
        base.to_string()
    } else if base.ends_with("/admin") {
        format!("{base}/api/state")
    } else {
        format!("{base}/admin/api/state")
    }
}

#[cfg(test)]
fn derive_admin_base_from_control(control: &str) -> Option<String> {
    let rest = control.strip_prefix("ws://")?;
    let authority = rest.split('/').next()?;
    let (host, _) = split_host_port(authority, 40001)?;
    if host.is_empty() {
        return None;
    }
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host
    };
    Some(format!("http://{host}:40002"))
}

fn default_channel_options() -> Vec<ChannelOption> {
    vec![
        ChannelOption {
            id: 0,
            name: "open".to_string(),
        },
        ChannelOption {
            id: 1,
            name: "Program".to_string(),
        },
        ChannelOption {
            id: 2,
            name: "Production PL".to_string(),
        },
        ChannelOption {
            id: 3,
            name: "Referee PL".to_string(),
        },
        ChannelOption {
            id: 4,
            name: "Director IFB".to_string(),
        },
        ChannelOption {
            id: 5,
            name: "Producer Cue".to_string(),
        },
        ChannelOption {
            id: 6,
            name: "PA".to_string(),
        },
        ChannelOption {
            id: 7,
            name: "Utility".to_string(),
        },
    ]
}

fn fallback_channel_options(config: &BridgeAppConfig) -> Vec<ChannelOption> {
    merge_channel_options(default_channel_options(), config)
}

fn merge_channel_options(
    mut channels: Vec<ChannelOption>,
    config: &BridgeAppConfig,
) -> Vec<ChannelOption> {
    for channel_id in config
        .routes
        .iter()
        .flat_map(|route| route.tx_channels.iter().chain(route.listen_channels.iter()))
        .copied()
    {
        if !channels.iter().any(|channel| channel.id == channel_id) {
            channels.push(ChannelOption {
                id: channel_id,
                name: format!("Channel {channel_id}"),
            });
        }
    }
    channels.sort_by_key(|channel| channel.id);
    channels.dedup_by_key(|channel| channel.id);
    channels
}

fn bridge_bin_path(state: &AppState, config: &BridgeAppConfig) -> PathBuf {
    state
        .bridge_bin_override
        .clone()
        .or_else(|| config.bridge_bin.clone())
        .unwrap_or_else(default_bridge_bin_path)
}

fn default_bridge_bin_path() -> PathBuf {
    let exe_name = if cfg!(target_os = "windows") {
        "bridge.exe"
    } else {
        "bridge"
    };
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join(exe_name)))
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from(exe_name))
}

fn bridge_args(config: &BridgeAppConfig, route: &BridgeRouteConfig) -> Vec<String> {
    let endpoints = config.endpoint_overrides();
    let server_host = endpoints.server_host.clone();
    let server = endpoints
        .effective_server()
        .unwrap_or_else(|_| config.server)
        .to_string();
    let control = endpoints
        .effective_control()
        .unwrap_or_else(|_| config.control.clone());
    let mut args = vec![
        "--server-host".to_string(),
        server_host,
        "--server".to_string(),
        server,
        "--control".to_string(),
        control,
        "--user-id".to_string(),
        route.user_id.to_string(),
        "--name".to_string(),
        route.name.clone(),
        "--mode".to_string(),
        bridge_mode_arg(route.mode).to_string(),
        "--tx-channels".to_string(),
        csv_channels_or_default(&route.tx_channels),
        "--listen-channels".to_string(),
        csv_channels_or_default(&route.listen_channels),
        "--codec".to_string(),
        codec_arg(route.codec).to_string(),
        "--opus-profile".to_string(),
        opus_profile_arg(route.opus_profile).to_string(),
        "--input-gain".to_string(),
        route.input_gain.to_string(),
        "--output-gain".to_string(),
        route.output_gain.to_string(),
    ];
    args.extend([
        "--input-kind".to_string(),
        route_input_kind_arg(route.input_kind).to_string(),
        "--output-kind".to_string(),
        route_output_kind_arg(route.output_kind).to_string(),
    ]);
    if let Some(client_uid) = route
        .client_uid
        .as_deref()
        .filter(|uid| !uid.trim().is_empty())
    {
        args.extend(["--client-uid".to_string(), client_uid.to_string()]);
    }
    if let Some(identity_file) = &route.identity_file {
        args.extend([
            "--identity-file".to_string(),
            identity_file.display().to_string(),
        ]);
    }
    if route.stereo {
        args.push("--stereo".to_string());
    }
    if let Some(input_device) = route
        .input_device
        .as_deref()
        .filter(|device| !device.trim().is_empty())
    {
        args.extend(["--input-device".to_string(), input_device.to_string()]);
    }
    if let Some(output_device) = route
        .output_device
        .as_deref()
        .filter(|device| !device.trim().is_empty())
    {
        args.extend(["--output-device".to_string(), output_device.to_string()]);
    }
    if let Some(ndi_source) = route
        .ndi_source
        .as_deref()
        .filter(|source| !source.trim().is_empty())
    {
        args.extend(["--ndi-source".to_string(), ndi_source.to_string()]);
    }
    if let Some(ndi_output_name) = route
        .ndi_output_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
    {
        args.extend(["--ndi-output-name".to_string(), ndi_output_name.to_string()]);
    }
    if let Some(ndi_groups) = route
        .ndi_groups
        .as_deref()
        .filter(|groups| !groups.trim().is_empty())
    {
        args.extend(["--ndi-groups".to_string(), ndi_groups.to_string()]);
    }
    if route.is_vmix_browser_source() {
        args.extend([
            "--vmix-source-url".to_string(),
            format!("/vmix/source/{}", encode_path_segment(&route.id)),
        ]);
    }
    if !route.note.trim().is_empty() {
        args.extend(["--note".to_string(), route.note.clone()]);
    }
    args
}

fn csv_channels(channels: &[u16]) -> String {
    channels
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn csv_channels_or_default(channels: &[u16]) -> String {
    if channels.is_empty() {
        "1".to_string()
    } else {
        csv_channels(channels)
    }
}

fn bridge_mode_arg(mode: BridgeMode) -> &'static str {
    match mode {
        BridgeMode::Input => "input",
        BridgeMode::Output => "output",
        BridgeMode::Duplex => "duplex",
    }
}

fn route_input_kind_arg(kind: RouteInputKind) -> &'static str {
    match kind {
        RouteInputKind::AudioDevice => "audio-device",
        RouteInputKind::NdiSource => "ndi-source",
    }
}

fn route_output_kind_arg(kind: RouteOutputKind) -> &'static str {
    match kind {
        RouteOutputKind::AudioDevice => "audio-device",
        RouteOutputKind::VmixBrowserSource => "vmix-browser-source",
        RouteOutputKind::NdiOutput => "ndi-output",
    }
}

fn codec_arg(codec: Codec) -> &'static str {
    match codec {
        Codec::Pcm16 => "pcm16",
        Codec::Pcm24 => "pcm24",
        Codec::Pcm48 => "pcm48",
        Codec::Opus => "opus",
        Codec::Adpcm => "adpcm",
    }
}

fn opus_profile_arg(profile: OpusProfile) -> &'static str {
    match profile {
        OpusProfile::Speech16Low => "speech-16-low",
        OpusProfile::Speech24Standard => "speech-24-standard",
        OpusProfile::Speech48High => "speech-48-high",
        OpusProfile::Music48 => "music-48",
    }
}

fn vmix_source_url(state: &AppState, route: &BridgeRouteConfig) -> String {
    format!(
        "{}/vmix/source/{}",
        state.base_url.trim_end_matches('/'),
        encode_path_segment(&route.id)
    )
}

fn encode_path_segment(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn vmix_source_page(route: &BridgeRouteConfig) -> String {
    let title = html_escape(&route.name);
    let route_id = html_escape(&route.id);
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{title}</title>
<style>html,body{{margin:0;width:100%;height:100%;background:transparent;color:#e5e7eb;font:12px system-ui,sans-serif;overflow:hidden}}#status{{position:fixed;left:8px;bottom:8px;padding:4px 7px;border-radius:6px;background:rgba(15,23,42,.72)}}</style></head>
<body><div id="status">RedLine {title}</div><script>
const statusEl=document.getElementById('status');
async function start(){{
  const audio=new AudioContext({{sampleRate:48000}});
  const code=`class RedLineSource extends AudioWorkletProcessor{{constructor(){{super();this.q=[];this.read=0;this.port.onmessage=e=>{{this.q.push(new Float32Array(e.data));}}}}process(i,o){{const out=o[0][0];for(let n=0;n<out.length;n++){{if(!this.q.length){{out[n]=0;continue;}}const f=this.q[0];out[n]=f[this.read++]||0;if(this.read>=f.length){{this.q.shift();this.read=0;}}}}return true;}}}}registerProcessor('redline-source',RedLineSource);`;
  await audio.audioWorklet.addModule(URL.createObjectURL(new Blob([code],{{type:'text/javascript'}})));
  const node=new AudioWorkletNode(audio,'redline-source');
  node.connect(audio.destination);
  const wsUrl=(location.protocol==='https:'?'wss://':'ws://')+location.host+'/vmix/source/{route_id}/ws';
  const ws=new WebSocket(wsUrl);ws.binaryType='arraybuffer';
  ws.onopen=()=>{{statusEl.textContent='RedLine {title} connected';audio.resume();}};
  ws.onmessage=e=>node.port.postMessage(e.data,[e.data]);
  ws.onclose=()=>statusEl.textContent='RedLine {title} disconnected';
  ws.onerror=()=>statusEl.textContent='RedLine {title} websocket error';
}}
start().catch(err=>{{statusEl.textContent='RedLine {title} failed: '+err.message;}});
</script></body></html>"#
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn validate_gain(name: &str, gain: f32) -> anyhow::Result<()> {
    if !gain.is_finite() || !(0.0..=2.0).contains(&gain) {
        bail!("{name} must be a finite value between 0 and 2");
    }
    Ok(())
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn spawn_window_opener(url: String) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        if let Err(err) = open_url(&url) {
            tracing::warn!(%url, %err, "failed to open bridge app URL");
        }
    });
}

fn open_url(url: &str) -> anyhow::Result<()> {
    let command = open_command_for_platform(url, HostPlatform::current());
    std::process::Command::new(&command.program)
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

#[derive(Debug)]
struct BridgeAppError {
    status: axum::http::StatusCode,
    message: String,
}

impl BridgeAppError {
    fn bad_request(message: impl ToString) -> Self {
        Self {
            status: axum::http::StatusCode::BAD_REQUEST,
            message: message.to_string(),
        }
    }

    fn internal(message: impl ToString) -> Self {
        Self {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            message: message.to_string(),
        }
    }
}

impl IntoResponse for BridgeAppError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.message });
        (self.status, Json(body)).into_response()
    }
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>RedLine Bridge</title>
  <link rel="stylesheet" href="/style.css">
</head>
<body>
  <header class="topbar">
    <div><strong>Bridge App</strong><span id="summary"></span></div>
    <div class="actions"><button id="refresh" type="button">Refresh</button><button id="start-all" type="button">Start Enabled</button><button id="stop-all" type="button">Stop All</button></div>
  </header>
  <main>
    <section class="panel connection-panel">
      <h1>Connection</h1>
      <p class="muted">Use one server host for standard RedLine deployments. Custom ports and URLs stay in Advanced.</p>
      <div class="grid">
        <label>Server Host<input id="server-host" autocomplete="off" placeholder="192.168.1.10"></label>
        <label class="check"><input id="advanced-endpoints" type="checkbox"> Advanced endpoints</label>
        <label>Audio Address<input id="server" autocomplete="off" placeholder="192.168.1.10:40000"></label>
        <label>Control WebSocket<input id="control" autocomplete="off" placeholder="ws://192.168.1.10:40001"></label>
        <label>Admin URL<input id="admin" autocomplete="off" placeholder="http://192.168.1.10:40002"></label>
      </div>
      <p id="connection-summary" class="hint"></p>
    </section>
    <section class="panel">
      <h1>Routes</h1>
      <p id="message" class="muted">Create one route per vMix bus, PA output, program input, or recorder feed.</p>
      <div id="routes"></div>
      <div class="actions"><button id="add-route" type="button">Add Route</button><button id="save" class="primary" type="button">Save Configuration</button></div>
    </section>
  </main>
  <script src="/app.js"></script>
</body>
</html>"#;

const STYLE_CSS: &str = r#":root{font-family:Inter,ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;color:#17202b;background:#f4f6f8;--line:#d7dee7;--muted:#637083;--blue:#2166d1;--green:#137a45;--red:#b42318}
body{margin:0}.topbar{position:sticky;top:0;z-index:5;display:flex;align-items:center;justify-content:space-between;gap:16px;background:#182331;color:#fff;padding:13px 18px}.topbar div:first-child{display:grid;gap:2px}.topbar span{color:#ced6e0;font-size:12px}
main{max-width:1480px;margin:0 auto;padding:16px}.panel,.route{background:#fff;border:1px solid var(--line);border-radius:8px;padding:14px}.panel h1{margin:0 0 4px}.muted{color:var(--muted)}.ok{color:var(--green)}.error{color:var(--red)}
.actions{display:flex;gap:8px;align-items:center;flex-wrap:wrap}button{border:1px solid #b8c3d0;background:#fff;border-radius:6px;padding:7px 10px;cursor:pointer}button.primary{background:var(--blue);border-color:var(--blue);color:#fff}button.danger{border-color:#f2b8b5;color:var(--red)}
#routes{display:grid;gap:12px;margin:14px 0}.route-head{display:flex;align-items:center;justify-content:space-between;gap:10px;margin-bottom:10px}.route-head h2{margin:0;font-size:17px}.badge{display:inline-flex;border-radius:999px;padding:2px 8px;background:#e9f7ef;color:#146c43;font-size:12px;font-weight:700}.badge.off{background:#edf1f5;color:#566576}.badge.err{background:#fee4e2;color:#912018}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:10px}label,.field{display:grid;gap:4px;font-weight:650;font-size:13px}input,select{border:1px solid #b8c3d0;border-radius:6px;padding:7px;min-width:0;background:#fff}.check{display:flex;gap:8px;align-items:center}.wide{grid-column:1/-1}.hint{font-size:12px;color:var(--muted)}
.multi{position:relative}.multi-button{width:100%;min-height:34px;text-align:left;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.multi-menu{display:none;position:absolute;z-index:20;top:100%;left:0;right:0;max-height:260px;overflow:auto;background:#fff;border:1px solid #b8c3d0;border-radius:6px;box-shadow:0 10px 24px rgba(20,30,45,.14);padding:6px;margin-top:4px}.multi.open .multi-menu{display:grid;gap:2px}.multi-menu label{display:flex;align-items:center;gap:8px;padding:5px;border-radius:4px;font-weight:500}.multi-menu label:hover{background:#f2f5f8}
@media(max-width:800px){.topbar{display:grid}.route-head{display:grid}.grid{grid-template-columns:1fr}}"#;

const APP_JS: &str = r#"const $ = id => document.getElementById(id);
let state = null;
function esc(value){return String(value ?? '').replace(/[&<>"']/g,ch=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[ch]));}
const DEFAULT_HOST='127.0.0.1',AUDIO_PORT=40000,CONTROL_PORT=40001,ADMIN_PORT=40002;
function normalizeHost(host){return String(host||'').trim().replace(/^\[(.*)\]$/,'$1');}
function hostForUrl(host){const normalized=normalizeHost(host);return normalized.includes(':')?`[${normalized}]`:normalized;}
function audioForHost(host){const normalized=normalizeHost(host)||DEFAULT_HOST;return `${hostForUrl(normalized)}:${AUDIO_PORT}`;}
function controlForHost(host){const normalized=normalizeHost(host)||DEFAULT_HOST;return `ws://${hostForUrl(normalized)}:${CONTROL_PORT}`;}
function adminForHost(host){const normalized=normalizeHost(host)||DEFAULT_HOST;return `http://${hostForUrl(normalized)}:${ADMIN_PORT}`;}
function syncConnectionFields(){const advanced=$('advanced-endpoints').checked;for(const id of ['server','control','admin']){$(id).disabled=!advanced;}if(!advanced){const host=$('server-host').value.trim()||DEFAULT_HOST;$('server').value=audioForHost(host);$('control').value=controlForHost(host);$('admin').value=adminForHost(host);}$('connection-summary').textContent=`audio ${$('server').value} | control ${$('control').value} | admin ${$('admin').value || adminForHost($('server-host').value)}`;}
function bindConnection(){const config=state.config||{};$('server-host').value=config.server_host||DEFAULT_HOST;$('advanced-endpoints').checked=!!config.advanced_endpoints;$('server').value=config.server||audioForHost($('server-host').value);$('control').value=config.control||controlForHost($('server-host').value);$('admin').value=config.admin||adminForHost($('server-host').value);$('server-host').oninput=syncConnectionFields;$('advanced-endpoints').onchange=syncConnectionFields;syncConnectionFields();}
function routeStatus(id){return (state.routes || []).find(route=>route.id===id) || {};}
function setMessage(text, kind='muted'){$('message').className=kind;$('message').textContent=text;}
async function api(path, opts={}){const res=await fetch('/api'+path,{headers:{'content-type':'application/json'},...opts});if(!res.ok){let msg=res.statusText;try{msg=(await res.json()).error||msg;}catch{}throw new Error(msg);}return res.json();}
function option(value,label,selected=false){return `<option value="${esc(value)}" ${selected?'selected':''}>${esc(label)}</option>`;}
function deviceSelect(field,label,devices,current,placeholder){let values=[...(devices||[])];if(current&&!values.includes(current)){values.unshift(current);}const opts=[option('',`System default ${placeholder}`,!current),...values.map(name=>option(name,name,name===current))].join('');return `<label>${esc(label)}<select data-field="${field}" data-device-select="true">${opts}</select></label>`;}
function ndiSourceDatalist(){return `<datalist id="ndi-source-options">${(state.ndi_sources||[]).map(name=>`<option value="${esc(name)}"></option>`).join('')}</datalist>`;}
function channelLabel(channel){return channel.name ? `${channel.id} - ${channel.name}` : `Channel ${channel.id}`;}
function channelsFor(selected){const byId=new Map((state.channels||[]).map(ch=>[Number(ch.id),ch]));for(const id of selected||[]){if(!byId.has(Number(id))){byId.set(Number(id),{id:Number(id),name:`Channel ${id}`});}}return [...byId.values()].sort((a,b)=>Number(a.id)-Number(b.id));}
function channelDropdown(field,label,selected){const picked=new Set((selected||[]).map(Number));const checks=channelsFor(selected).map(ch=>`<label><input type="checkbox" value="${ch.id}" ${picked.has(Number(ch.id))?'checked':''}> ${esc(channelLabel(ch))}</label>`).join('');return `<div class="field"><span>${esc(label)}</span><div class="multi" data-field="${field}" data-channel-dropdown="true"><button class="multi-button" type="button"></button><div class="multi-menu">${checks}</div></div><span class="hint">Select one or more configured server channels.</span></div>`;}
function selectedChannels(card,field){return [...card.querySelectorAll(`[data-field="${field}"] input:checked`)].map(input=>Number(input.value)).filter(value=>Number.isInteger(value)&&value>=0);}
function channelSummary(values){if(!values.length){return 'No channels selected';}const byId=new Map((state.channels||[]).map(ch=>[Number(ch.id),ch]));return values.map(id=>channelLabel(byId.get(Number(id))||{id,name:''})).join(', ');}
function refreshChannelButtons(scope=document){scope.querySelectorAll('[data-channel-dropdown]').forEach(dropdown=>{const values=[...dropdown.querySelectorAll('input:checked')].map(input=>Number(input.value));dropdown.querySelector('.multi-button').textContent=channelSummary(values);});}
function setFieldVisible(card,field,visible){const input=card.querySelector(`[data-field="${field}"]`);const wrap=input?.closest('label,.field');if(wrap)wrap.style.display=visible?'':'none';}
function updateRouteVisibility(card){const field=name=>card.querySelector(`[data-field="${name}"]`);const mode=field('mode').value;const inputKind=field('input_kind').value;const outputKind=field('output_kind').value;const captures=mode==='input'||mode==='duplex';const plays=mode==='output'||mode==='duplex';setFieldVisible(card,'input_kind',captures);setFieldVisible(card,'output_kind',plays);setFieldVisible(card,'tx_channels',captures);setFieldVisible(card,'listen_channels',plays);setFieldVisible(card,'input_device',captures&&inputKind==='audio_device');setFieldVisible(card,'ndi_source',captures&&inputKind==='ndi_source');setFieldVisible(card,'output_device',plays&&outputKind==='audio_device');setFieldVisible(card,'ndi_output_name',plays&&outputKind==='ndi_output');setFieldVisible(card,'ndi_groups',(captures&&inputKind==='ndi_source')||(plays&&outputKind==='ndi_output'));setFieldVisible(card,'output_gain',plays);setFieldVisible(card,'input_gain',captures);setFieldVisible(card,'stereo',plays);}
function sourceUrlHtml(status){if(!status.source_url){return '';}return `<label class="wide">vMix Source URL<input readonly value="${esc(status.source_url)}" onclick="this.select()"></label>`;}
function routeTelemetryHtml(status){const parts=[];if(status.runtime)parts.push(status.runtime);if(status.audio_level!=null)parts.push(`level ${Math.round(Number(status.audio_level)*100)}%`);if(status.audio_frames)parts.push(`${status.audio_frames} frames`);if(status.underflows)parts.push(`${status.underflows} underflows`);if(status.drops)parts.push(`${status.drops} drops`);if(status.last_audio_ms_ago!=null)parts.push(`last audio ${status.last_audio_ms_ago} ms ago`);if(status.stale)parts.push('stale');return parts.length?`<p class="hint wide">${esc(parts.join(' | '))}</p>`:'';}
function routeHtml(route,index){const status=routeStatus(route.id);const running=!!status.running;return `<div class="route" data-index="${index}">
  <div class="route-head"><h2>${esc(route.name || route.id || 'Bridge Route')} ${running?'<span class="badge">running</span>':status.exit?`<span class="badge err">${esc(status.exit)}</span>`:'<span class="badge off">stopped</span>'}</h2><div class="actions"><button data-start="${index}" type="button">Start</button><button data-stop="${index}" type="button">Stop</button><button data-remove="${index}" class="danger" type="button">Remove</button></div></div>
  <div class="grid">
    <label>ID<input data-field="id" value="${esc(route.id)}"></label>
    <label>Name<input data-field="name" value="${esc(route.name)}"></label>
    <label>User ID<input data-field="user_id" type="number" min="1" max="65535" value="${route.user_id || 90}"></label>
    <label>Mode<select data-field="mode"><option value="input">Input</option><option value="output">Output</option><option value="duplex">Duplex</option></select></label>
    <label>Input Type<select data-field="input_kind"><option value="audio_device">Audio Device</option><option value="ndi_source">NDI Source</option></select></label>
    <label>Output Type<select data-field="output_kind"><option value="audio_device">Audio Device</option><option value="vmix_browser_source">vMix Browser Source</option><option value="ndi_output">NDI Output</option></select></label>
    ${channelDropdown('tx_channels','TX Channels',route.tx_channels)}
    ${channelDropdown('listen_channels','Listen Channels',route.listen_channels)}
    ${deviceSelect('input_device','Input Device',state.input_devices,route.input_device,'input')}
    ${deviceSelect('output_device','Output Device',state.output_devices,route.output_device,'output')}
    <label>NDI Source<input data-field="ndi_source" list="ndi-source-options" value="${esc(route.ndi_source || '')}" placeholder="Discovered NDI source name"></label>
    <label>NDI Output Name<input data-field="ndi_output_name" value="${esc(route.ndi_output_name || '')}" placeholder="RedLine Program"></label>
    <label>NDI Groups<input data-field="ndi_groups" value="${esc(route.ndi_groups || '')}" placeholder="Optional"></label>
    ${sourceUrlHtml(status)}
    ${routeTelemetryHtml(status)}
    <label>Codec<select data-field="codec"><option value="pcm16">PCM16</option><option value="pcm24">PCM24</option><option value="pcm48">PCM48</option><option value="opus">Opus</option></select></label>
    <label>Opus Profile<select data-field="opus_profile"><option value="speech_16_low">Speech 16 Low</option><option value="speech_24_standard">Speech 24 Standard</option><option value="speech_48_high">Speech 48 High</option><option value="music_48">Music 48</option></select></label>
    <label>Input Gain<input data-field="input_gain" type="number" min="0" max="2" step="0.05" value="${route.input_gain ?? 1}"></label>
    <label>Output Gain<input data-field="output_gain" type="number" min="0" max="2" step="0.05" value="${route.output_gain ?? 1}"></label>
    <label class="check"><input data-field="stereo" type="checkbox" ${route.stereo ? 'checked' : ''}> Stereo receive</label>
    <label class="check"><input data-field="enabled" type="checkbox" ${route.enabled !== false ? 'checked' : ''}> Start with Start Enabled</label>
    <label class="wide">Note<input data-field="note" value="${esc(route.note || '')}" placeholder="vMix bus A into Program"></label>
    ${status.warning ? `<p class="hint wide">${esc(status.warning)}</p>` : ''}
  </div>
</div>`;}
function bindRoutes(){document.querySelectorAll('.route').forEach(card=>{const index=Number(card.dataset.index);const route=state.config.routes[index];card.querySelector('[data-field="mode"]').value=route.mode||'duplex';card.querySelector('[data-field="input_kind"]').value=route.input_kind||'audio_device';card.querySelector('[data-field="output_kind"]').value=route.output_kind||'audio_device';card.querySelector('[data-field="codec"]').value=route.codec||'pcm48';card.querySelector('[data-field="opus_profile"]').value=route.opus_profile||'speech_48_high';['mode','input_kind','output_kind'].forEach(name=>card.querySelector(`[data-field="${name}"]`).onchange=()=>updateRouteVisibility(card));updateRouteVisibility(card);});document.querySelectorAll('[data-start]').forEach(button=>button.onclick=()=>startRoute(Number(button.dataset.start)));document.querySelectorAll('[data-stop]').forEach(button=>button.onclick=()=>stopRoute(Number(button.dataset.stop)));document.querySelectorAll('[data-remove]').forEach(button=>button.onclick=()=>removeRoute(Number(button.dataset.remove)));document.querySelectorAll('.multi-button').forEach(button=>button.onclick=event=>{event.stopPropagation();const dropdown=button.closest('.multi');document.querySelectorAll('.multi.open').forEach(open=>{if(open!==dropdown)open.classList.remove('open');});dropdown.classList.toggle('open');});document.querySelectorAll('[data-channel-dropdown] input').forEach(input=>input.onchange=()=>refreshChannelButtons(input.closest('.route')));refreshChannelButtons();}
function render(){const running=(state.routes||[]).filter(route=>route.running).length;const warnings=(state.discovery_warnings||[]).length?` | ${(state.discovery_warnings||[]).length} discovery warning(s)`:'';const ndi=state.ndi_available?` | NDI ready (${(state.ndi_sources||[]).length} source(s))`:' | NDI unavailable';$('summary').textContent=`${running}/${(state.config.routes||[]).length} routes running | bridge ${state.bridge_bin}${ndi}${warnings}`;$('routes').innerHTML=ndiSourceDatalist()+((state.config.routes||[]).map(routeHtml).join('')||'<p class="muted">No routes configured.</p>');if((state.discovery_warnings||[]).length){setMessage(state.discovery_warnings.join(' | '),'muted');}bindConnection();bindRoutes();}
function collect(){syncConnectionFields();const routes=[...document.querySelectorAll('.route')].map(card=>{const field=name=>card.querySelector(`[data-field="${name}"]`);return {id:field('id').value.trim(),name:field('name').value.trim(),user_id:Number(field('user_id').value),mode:field('mode').value,input_kind:field('input_kind').value,output_kind:field('output_kind').value,tx_channels:selectedChannels(card,'tx_channels'),listen_channels:selectedChannels(card,'listen_channels'),input_device:field('input_device').value.trim()||null,output_device:field('output_device').value.trim()||null,ndi_source:field('ndi_source').value.trim()||null,ndi_output_name:field('ndi_output_name').value.trim()||null,ndi_groups:field('ndi_groups').value.trim()||null,codec:field('codec').value,opus_profile:field('opus_profile').value,input_gain:Number(field('input_gain').value)||1,output_gain:Number(field('output_gain').value)||1,stereo:field('stereo').checked,enabled:field('enabled').checked,note:field('note').value.trim()};});return {...state.config,server_host:normalizeHost($('server-host').value)||DEFAULT_HOST,server:$('server').value.trim(),control:$('control').value.trim(),admin:$('admin').value.trim()||null,advanced_endpoints:$('advanced-endpoints').checked,routes};}
async function load(){state=await api('/state');render();}
async function save(){try{state.config=collect();await api('/config',{method:'PUT',body:JSON.stringify(state.config)});setMessage('Configuration saved.','ok');await load();}catch(err){setMessage(err.message,'error');}}
async function startRoute(index){try{await save();await api(`/routes/${encodeURIComponent(state.config.routes[index].id)}/start`,{method:'POST'});await load();}catch(err){setMessage(err.message,'error');}}
async function stopRoute(index){try{const id=state.config.routes[index].id;await api(`/routes/${encodeURIComponent(id)}/stop`,{method:'POST'});await load();}catch(err){setMessage(err.message,'error');}}
function removeRoute(index){state.config=collect();state.config.routes.splice(index,1);render();setMessage('Route removed. Save configuration to persist.','muted');}
$('add-route').onclick=()=>{state.config=collect();const n=(state.config.routes||[]).length+1;state.config.routes.push({id:`route-${n}`,name:`Bridge Route ${n}`,user_id:90+n,mode:'input',input_kind:'audio_device',output_kind:'audio_device',tx_channels:[1],listen_channels:[],codec:'pcm48',opus_profile:'speech_48_high',input_gain:1,output_gain:1,enabled:true,note:''});render();};
$('save').onclick=save;$('refresh').onclick=load;$('start-all').onclick=async()=>{try{await save();await api('/routes/start-all',{method:'POST'});await load();}catch(err){setMessage(err.message,'error');}};$('stop-all').onclick=async()=>{try{await api('/routes/stop-all',{method:'POST'});await load();}catch(err){setMessage(err.message,'error');}};
document.addEventListener('click',()=>document.querySelectorAll('.multi.open').forEach(dropdown=>dropdown.classList.remove('open')));
load().catch(err=>setMessage(err.message,'error'));"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "intercom-bridge-app-{name}-{}-{now}.json",
            std::process::id()
        ))
    }

    #[test]
    fn default_config_contains_program_input_route() {
        let config = BridgeAppConfig::default();

        config.validate().unwrap();
        assert_eq!(config.routes.len(), 1);
        assert_eq!(config.routes[0].mode, BridgeMode::Input);
        assert_eq!(config.routes[0].tx_channels, vec![1]);
        assert!(config.routes[0].listen_channels.is_empty());
    }

    #[test]
    fn bridge_args_include_route_devices_levels_and_note() {
        let config = BridgeAppConfig::default();
        let route = BridgeRouteConfig {
            id: "pa-out".to_string(),
            name: "PA Out".to_string(),
            user_id: 91,
            mode: BridgeMode::Output,
            listen_channels: vec![6],
            tx_channels: Vec::new(),
            codec: Codec::Opus,
            opus_profile: OpusProfile::Music48,
            stereo: true,
            output_device: Some("USB Audio".to_string()),
            output_gain: 0.75,
            note: "arena PA".to_string(),
            ..BridgeRouteConfig::default()
        };

        let args = bridge_args(&config, &route);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--server-host", "127.0.0.1"]));
        assert!(args.windows(2).any(|pair| pair == ["--mode", "output"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--listen-channels", "6"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--output-device", "USB Audio"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--output-gain", "0.75"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--input-kind", "audio-device"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--output-kind", "audio-device"]));
        assert!(args.windows(2).any(|pair| pair == ["--note", "arena PA"]));
        assert!(args.contains(&"--stereo".to_string()));
    }

    #[test]
    fn vmix_browser_source_routes_validate_as_output_only() {
        let route = BridgeRouteConfig {
            id: "program mix".to_string(),
            name: "Program Mix".to_string(),
            mode: BridgeMode::Output,
            tx_channels: Vec::new(),
            listen_channels: vec![1],
            output_kind: RouteOutputKind::VmixBrowserSource,
            ..BridgeRouteConfig::default()
        };

        route.validate().unwrap();
        assert_eq!(encode_path_segment(&route.id), "program%20mix");

        let bad = BridgeRouteConfig {
            mode: BridgeMode::Duplex,
            tx_channels: vec![2],
            ..route
        };
        assert!(bad
            .validate()
            .unwrap_err()
            .to_string()
            .contains("must be an output route"));
    }

    #[test]
    fn ndi_routes_validate_and_emit_cli_args() {
        let config = BridgeAppConfig::default();
        let input = BridgeRouteConfig {
            id: "ndi-in".to_string(),
            name: "NDI In".to_string(),
            mode: BridgeMode::Input,
            input_kind: RouteInputKind::NdiSource,
            ndi_source: Some("vMix - Program".to_string()),
            tx_channels: vec![2],
            listen_channels: Vec::new(),
            ..BridgeRouteConfig::default()
        };
        input.validate().unwrap();
        let input_args = bridge_args(&config, &input);
        assert!(input_args
            .windows(2)
            .any(|pair| pair == ["--input-kind", "ndi-source"]));
        assert!(input_args
            .windows(2)
            .any(|pair| pair == ["--ndi-source", "vMix - Program"]));

        let output = BridgeRouteConfig {
            id: "ndi-out".to_string(),
            name: "NDI Out".to_string(),
            user_id: 91,
            mode: BridgeMode::Output,
            output_kind: RouteOutputKind::NdiOutput,
            ndi_output_name: Some("RedLine Program".to_string()),
            ndi_groups: Some("arena".to_string()),
            tx_channels: Vec::new(),
            listen_channels: vec![2],
            ..BridgeRouteConfig::default()
        };
        output.validate().unwrap();
        let output_args = bridge_args(&config, &output);
        assert!(output_args
            .windows(2)
            .any(|pair| pair == ["--output-kind", "ndi-output"]));
        assert!(output_args
            .windows(2)
            .any(|pair| pair == ["--ndi-output-name", "RedLine Program"]));
        assert!(output_args
            .windows(2)
            .any(|pair| pair == ["--ndi-groups", "arena"]));

        let missing = BridgeRouteConfig {
            ndi_output_name: None,
            ..output
        };
        assert!(missing
            .validate()
            .unwrap_err()
            .to_string()
            .contains("no NDI output name"));
    }

    #[test]
    fn browser_source_audio_downmixes_to_float32_pcm() {
        let bytes = samples_to_float32_bytes(&[i16::MAX, i16::MAX, 0, 0], 2);
        let first = f32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let second = f32::from_le_bytes(bytes[4..8].try_into().unwrap());

        assert!((first - 1.0).abs() < 0.0001);
        assert_eq!(second, 0.0);
        assert!((i16_peak_level(&[0, i16::MAX]) - 1.0).abs() < 0.0001);
    }

    #[test]
    fn rejects_feedback_overlap() {
        let route = BridgeRouteConfig {
            id: "bad".to_string(),
            tx_channels: vec![6],
            listen_channels: vec![6],
            ..BridgeRouteConfig::default()
        };

        assert!(route
            .validate()
            .unwrap_err()
            .to_string()
            .contains("feedback"));
    }

    #[test]
    fn saves_and_loads_config() {
        let path = temp_path("roundtrip");
        let config = BridgeAppConfig {
            routes: vec![BridgeRouteConfig {
                id: "program".to_string(),
                name: "Program".to_string(),
                user_id: 90,
                mode: BridgeMode::Input,
                tx_channels: vec![1],
                listen_channels: Vec::new(),
                ..BridgeRouteConfig::default()
            }],
            ..BridgeAppConfig::default()
        };

        save_config(&path, &config).unwrap();
        let loaded = load_config(&path).unwrap();
        assert_eq!(loaded, config);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn derives_admin_url_and_merges_selected_channels() {
        let config = BridgeAppConfig {
            control: "ws://192.0.2.10:40001/control".to_string(),
            routes: vec![BridgeRouteConfig {
                id: "custom".to_string(),
                tx_channels: vec![42],
                listen_channels: vec![6],
                ..BridgeRouteConfig::default()
            }],
            ..BridgeAppConfig::default()
        };

        assert_eq!(
            derive_admin_base_from_control(&config.control).as_deref(),
            Some("http://192.0.2.10:40002")
        );
        assert_eq!(
            admin_state_url("http://192.0.2.10:40002/admin"),
            "http://192.0.2.10:40002/admin/api/state"
        );
        let channels = merge_channel_options(default_channel_options(), &config);
        assert!(channels
            .iter()
            .any(|channel| channel.id == 0 && channel.name == "open"));
        assert!(channels
            .iter()
            .any(|channel| channel.id == 42 && channel.name == "Channel 42"));
        assert!(channels
            .iter()
            .any(|channel| channel.id == 6 && channel.name == "PA"));
    }

    #[test]
    fn bridge_app_ui_uses_dropdown_controls_for_devices_and_channels() {
        assert!(INDEX_HTML.contains("server-host"));
        assert!(INDEX_HTML.contains("advanced-endpoints"));
        assert!(APP_JS.contains("server_host"));
        assert!(APP_JS.contains("advanced_endpoints"));
        assert!(APP_JS.contains("deviceSelect('input_device'"));
        assert!(APP_JS.contains("deviceSelect('output_device'"));
        assert!(APP_JS.contains("channelDropdown('tx_channels'"));
        assert!(APP_JS.contains("channelDropdown('listen_channels'"));
        assert!(APP_JS.contains("ndi-source-options"));
        assert!(APP_JS.contains("NDI ready"));
        assert!(APP_JS.contains("updateRouteVisibility"));
        assert!(APP_JS.contains("routeTelemetryHtml"));
        assert!(!APP_JS.contains("TX Channels<input"));
        assert!(!APP_JS.contains("Input Device<input"));
    }

    #[test]
    fn tauri_native_assets_are_present() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));

        assert!(manifest.join("tauri.conf.json").exists());
        assert!(manifest.join("tauri-assets/index.html").exists());
        assert!(manifest.join("scripts/package-native.sh").exists());
    }
}
