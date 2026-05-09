#[cfg(not(feature = "native"))]
fn main() -> anyhow::Result<()> {
    use clap::Parser;

    desktop::init_tracing()?;
    let args = app::AppArgs::parse();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(app::run_app(args))
}

#[cfg(all(feature = "native", any(target_os = "ios", target_os = "android")))]
fn main() {}

#[cfg(all(feature = "native", not(any(target_os = "ios", target_os = "android"))))]
fn main() -> anyhow::Result<()> {
    native::main()
}

#[cfg(all(feature = "native", not(any(target_os = "ios", target_os = "android"))))]
mod native {
    use std::net::ToSocketAddrs;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::thread;
    use std::thread::JoinHandle;
    use std::time::Duration;

    use anyhow::{bail, Context, Result};
    use clap::Parser;
    use client_core::ClientRuntimePhase;
    use common::{AlertId, ButtonId, Codec, TalkMode};
    use serde::Serialize;
    use tauri::Manager;
    use tokio::sync::oneshot;

    const TRAY_ID: &str = "intercom-tray";
    const MENU_OPEN: &str = "open";
    const MENU_SETTINGS: &str = "settings";
    const MENU_STATUS: &str = "status";
    const MENU_MUTE: &str = "mute";
    const MENU_UNMUTE: &str = "unmute";
    const MENU_TALK_DOWN: &str = "talk_down";
    const MENU_TALK_UP: &str = "talk_up";
    const MENU_QUIT: &str = "quit";

    pub fn main() -> Result<()> {
        desktop::init_tracing()?;
        let mut args = app::AppArgs::parse();
        apply_native_defaults(&mut args);
        if matches!(
            args.window_mode,
            Some(app::AppWindowMode::SystemBrowser | app::AppWindowMode::Disabled)
        ) {
            return tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(app::run_app(args));
        }

        let mut settings = app::load_settings(&args.config_file)?;
        settings.merge_cli(&args);
        settings.validate()?;

        if args.print_config {
            println!("{}", serde_json::to_string_pretty(&settings)?);
            return Ok(());
        }

        if args.init_config || args.write_config {
            app::save_settings(&args.config_file, &settings)?;
            eprintln!("wrote app settings to {}", args.config_file.display());
        }
        if args.init_config {
            eprintln!(
            "settings initialized; start the native app with `cargo run -p app --features native --bin app-native`"
        );
            return Ok(());
        }

        let mut runtime_settings = settings.clone();
        let launch_plan = app::build_launch_plan(&mut runtime_settings, args.list_devices)?;
        if args.print_launch_plan {
            println!("{}", serde_json::to_string_pretty(&launch_plan)?);
            return Ok(());
        }

        if args.list_devices
            || args.disable_local_ui
            || runtime_settings.window_mode != app::AppWindowMode::Native
        {
            return run_desktop(runtime_settings, args.list_devices);
        }

        let title = runtime_settings.app_title.clone();
        let config_file = args.config_file.clone();
        run_tauri_window(title, config_file)
    }

    fn run_desktop(settings: app::AppSettings, list_devices: bool) -> Result<()> {
        let desktop_args = settings.desktop_args(list_devices)?;
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(desktop::run(desktop_args))
    }

    struct DesktopRuntimeHandle {
        controls_url: String,
        api: desktop::LocalClientApi,
        shutdown_tx: Option<oneshot::Sender<()>>,
        join: Option<JoinHandle<Result<()>>>,
    }

    impl DesktopRuntimeHandle {
        fn finish_if_done(&mut self) -> Option<Result<()>> {
            if !self.join.as_ref().is_some_and(JoinHandle::is_finished) {
                return None;
            }
            let join = self.join.take().expect("join handle checked above");
            Some(
                join.join()
                    .map_err(|_| anyhow::anyhow!("desktop runtime thread panicked"))
                    .and_then(|result| result),
            )
        }

        fn shutdown(mut self) -> Result<()> {
            if let Some(shutdown_tx) = self.shutdown_tx.take() {
                let _ = shutdown_tx.send(());
            }
            if let Some(join) = self.join.take() {
                join.join()
                    .map_err(|_| anyhow::anyhow!("desktop runtime thread panicked"))??;
            }
            Ok(())
        }
    }

    fn spawn_desktop_runtime(
        settings: app::AppSettings,
        list_devices: bool,
    ) -> Result<DesktopRuntimeHandle> {
        let desktop_args = settings.desktop_args(list_devices)?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (api_tx, api_rx) = std::sync::mpsc::channel::<desktop::LocalClientApi>();
        let join = thread::Builder::new()
            .name("intercom-desktop-runtime".to_string())
            .spawn(move || {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .context("build desktop runtime")
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
            .context("spawn desktop runtime")?;
        let api = match api_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(api) => api,
            Err(err) => {
                let _ = shutdown_tx.send(());
                bail!("desktop runtime did not become ready: {err}");
            }
        };
        Ok(DesktopRuntimeHandle {
            controls_url: "client-controls.html".to_string(),
            api,
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
        })
    }

    fn run_tauri_window(title: String, config_file: PathBuf) -> Result<()> {
        let app_state = NativeAppState {
            config_file,
            runtime: Mutex::default(),
        };

        tauri::Builder::default()
            .manage(app_state)
            .invoke_handler(tauri::generate_handler![
                load_native_settings,
                save_native_settings,
                default_native_settings,
                native_discover_servers,
                native_forget_server,
                native_open_controls,
                native_select_server,
                native_start_client,
                native_stop_client,
                native_status,
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
            .setup(move |app| {
                tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    tauri::WebviewUrl::App("settings.html".into()),
                )
                .title(title.clone())
                .inner_size(430.0, 860.0)
                .min_inner_size(360.0, 640.0)
                .center()
                .build()?;
                create_tray(app.handle(), &title)?;
                Ok(())
            })
            .run(tauri::generate_context!("tauri.conf.json"))
            .context("run Tauri native app")?;
        Ok(())
    }

    fn apply_native_defaults(args: &mut app::AppArgs) {
        if args.window_mode.is_none() {
            args.window_mode = Some(app::AppWindowMode::Native);
        }
    }

    struct NativeAppState {
        config_file: PathBuf,
        runtime: Mutex<NativeRuntimeState>,
    }

    struct NativeRuntimeState {
        runtime: Option<DesktopRuntimeHandle>,
        phase: ClientRuntimePhase,
        last_error: Option<String>,
    }

    impl Default for NativeRuntimeState {
        fn default() -> Self {
            Self {
                runtime: None,
                phase: ClientRuntimePhase::Stopped,
                last_error: None,
            }
        }
    }

    impl NativeAppState {
        fn api(&self) -> std::result::Result<desktop::LocalClientApi, String> {
            let mut runtime_state = self
                .runtime
                .lock()
                .map_err(|_| "native runtime state is poisoned".to_string())?;
            refresh_native_runtime_state(&mut runtime_state);
            runtime_state
                .runtime
                .as_ref()
                .map(|runtime| runtime.api.clone())
                .ok_or_else(|| {
                    runtime_state
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "native client is not running".to_string())
                })
        }
    }

    impl Drop for NativeAppState {
        fn drop(&mut self) {
            let runtime = self
                .runtime
                .get_mut()
                .ok()
                .and_then(|runtime_state| runtime_state.runtime.take());
            if let Some(runtime) = runtime {
                let _ = runtime.shutdown();
            }
        }
    }

    #[derive(Debug, Clone, Serialize)]
    struct NativeStartResponse {
        build: common::BuildInfo,
        running: bool,
        phase: ClientRuntimePhase,
        local_ui_url: String,
        last_error: Option<String>,
    }

    #[derive(Debug, Clone, Serialize)]
    struct NativeStatusResponse {
        build: common::BuildInfo,
        running: bool,
        phase: ClientRuntimePhase,
        local_ui_url: Option<String>,
        last_error: Option<String>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum NativeTrayAction {
        Open,
        Settings,
        Status,
        Mute,
        Unmute,
        TalkDown,
        TalkUp,
        Quit,
        Unknown,
    }

    impl NativeTrayAction {
        fn from_menu_id(id: &str) -> Self {
            match id {
                MENU_OPEN => Self::Open,
                MENU_SETTINGS => Self::Settings,
                MENU_STATUS => Self::Status,
                MENU_MUTE => Self::Mute,
                MENU_UNMUTE => Self::Unmute,
                MENU_TALK_DOWN => Self::TalkDown,
                MENU_TALK_UP => Self::TalkUp,
                MENU_QUIT => Self::Quit,
                _ => Self::Unknown,
            }
        }

        fn uses_client_api(self) -> bool {
            matches!(
                self,
                Self::Status | Self::Mute | Self::Unmute | Self::TalkDown | Self::TalkUp
            )
        }
    }

    fn create_tray(app: &tauri::AppHandle, title: &str) -> tauri::Result<()> {
        let menu = tauri::menu::MenuBuilder::new(app)
            .text(MENU_OPEN, "Open Operator Window")
            .text(MENU_SETTINGS, "App Settings")
            .text(MENU_STATUS, "Refresh Status")
            .separator()
            .text(MENU_MUTE, "Mute")
            .text(MENU_UNMUTE, "Unmute")
            .separator()
            .text(MENU_TALK_DOWN, "Start Talk")
            .text(MENU_TALK_UP, "Stop Talk")
            .separator()
            .text(MENU_QUIT, "Quit")
            .build()?;

        let mut tray = tauri::tray::TrayIconBuilder::with_id(TRAY_ID)
            .menu(&menu)
            .tooltip(format!("{title} - starting"))
            .show_menu_on_left_click(true);
        if let Some(icon) = app.default_window_icon().cloned() {
            tray = tray.icon(icon);
        }
        tray.on_menu_event(|app, event| {
            handle_tray_menu(app, NativeTrayAction::from_menu_id(event.id().as_ref()));
        })
        .build(app)?;
        Ok(())
    }

    fn handle_tray_menu(app: &tauri::AppHandle, action: NativeTrayAction) {
        match action {
            NativeTrayAction::Open => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            }
            NativeTrayAction::Settings => {
                if let Err(err) = open_settings_window(app) {
                    eprintln!("open settings window failed: {err:?}");
                }
            }
            NativeTrayAction::Quit => app.exit(0),
            NativeTrayAction::Unknown => {}
            _ => {
                if !action.uses_client_api() {
                    return;
                }
                let app = app.clone();
                thread::spawn(move || {
                    let Some(state) = app.try_state::<NativeAppState>() else {
                        eprintln!("native tray command failed: missing tray state");
                        return;
                    };
                    let result = state
                        .api()
                        .map_err(|err| anyhow::anyhow!(err))
                        .and_then(|api| run_tray_action(&api, action));
                    match result {
                        Ok(tooltip) => {
                            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                                let _ = tray.set_tooltip(Some(format!("RedLine - {tooltip}")));
                            }
                        }
                        Err(err) => {
                            eprintln!("native tray command failed: {err:?}");
                            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                                let _ = tray.set_tooltip(Some(format!("RedLine - {err}")));
                            }
                        }
                    }
                });
            }
        }
    }

    fn open_settings_window(app: &tauri::AppHandle) -> tauri::Result<()> {
        if let Some(window) = app.get_webview_window("settings") {
            window.show()?;
            window.unminimize()?;
            window.set_focus()?;
            return Ok(());
        }
        tauri::WebviewWindowBuilder::new(
            app,
            "settings",
            tauri::WebviewUrl::App("settings.html".into()),
        )
        .title("RedLine Settings")
        .inner_size(560.0, 760.0)
        .min_inner_size(420.0, 560.0)
        .center()
        .build()?;
        Ok(())
    }

    #[tauri::command]
    fn load_native_settings(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<app::AppSettings, String> {
        app::load_settings(&state.config_file).map_err(|err| err.to_string())
    }

    #[tauri::command]
    fn save_native_settings(
        state: tauri::State<'_, NativeAppState>,
        mut settings: app::AppSettings,
    ) -> Result<(), String> {
        prepare_native_saved_settings(&mut settings);
        let profile = native_profile_for_settings(&settings, None);
        app::remember_mobile_profile(&mut settings, profile);
        app::save_settings(&state.config_file, &settings).map_err(|err| err.to_string())
    }

    #[tauri::command]
    fn default_native_settings() -> app::AppSettings {
        let mut settings = app::AppSettings::default();
        prepare_native_saved_settings(&mut settings);
        settings
    }

    #[tauri::command]
    fn native_discover_servers(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<Vec<app::MobileServerProfile>, String> {
        let settings = app::load_settings(&state.config_file).map_err(|err| err.to_string())?;
        let discovered = native_discover_servers_for_platform().map_err(|err| err.to_string())?;
        Ok(native_merge_profiles(settings.server_profiles, discovered))
    }

    #[tauri::command]
    fn native_select_server(
        state: tauri::State<'_, NativeAppState>,
        profile: app::MobileServerProfile,
    ) -> Result<app::AppSettings, String> {
        let mut profile = app::normalize_mobile_profile(profile);
        let server = native_parse_server_addr(&profile.server)?;
        if profile.control.is_empty() {
            return Err("control WebSocket URL cannot be empty".to_string());
        }
        if profile.name.is_empty() {
            profile.name = profile.control.clone();
        }
        if profile.id.is_empty() {
            profile.id = app::mobile_profile_id(&profile.name, &profile.control);
        }

        let mut settings = app::load_settings(&state.config_file).map_err(|err| err.to_string())?;
        settings.server = server;
        settings.control = profile.control.clone();
        app::remember_mobile_profile(&mut settings, profile);
        prepare_native_saved_settings(&mut settings);
        app::save_settings(&state.config_file, &settings).map_err(|err| err.to_string())?;
        Ok(settings)
    }

    #[tauri::command]
    fn native_forget_server(
        state: tauri::State<'_, NativeAppState>,
        id: String,
    ) -> Result<app::AppSettings, String> {
        let mut settings = app::load_settings(&state.config_file).map_err(|err| err.to_string())?;
        settings.server_profiles.retain(|profile| profile.id != id);
        app::save_settings(&state.config_file, &settings).map_err(|err| err.to_string())?;
        Ok(settings)
    }

    #[tauri::command]
    fn native_start_client(
        state: tauri::State<'_, NativeAppState>,
        mut settings: app::AppSettings,
    ) -> Result<NativeStartResponse, String> {
        prepare_native_saved_settings(&mut settings);
        let mut saved_settings = settings.clone();
        let connected_profile = native_profile_for_settings(
            &saved_settings,
            Some(app::mobile_connected_timestamp_ms()),
        );
        app::remember_mobile_profile(&mut saved_settings, connected_profile);
        app::save_settings(&state.config_file, &saved_settings).map_err(|err| err.to_string())?;

        {
            let mut runtime_state = state
                .runtime
                .lock()
                .map_err(|_| "native runtime state is poisoned".to_string())?;
            refresh_native_runtime_state(&mut runtime_state);
            if let Some(runtime) = runtime_state.runtime.as_ref() {
                return Ok(NativeStartResponse {
                    build: common::current_build_info(),
                    running: true,
                    phase: runtime_state.phase,
                    local_ui_url: runtime.controls_url.clone(),
                    last_error: runtime_state.last_error.clone(),
                });
            }
            runtime_state.phase = ClientRuntimePhase::Starting;
            runtime_state.last_error = None;
        }

        let start_result = (|| {
            let mut runtime_settings = saved_settings.clone();
            prepare_native_runtime_settings(&mut runtime_settings);
            let runtime =
                spawn_desktop_runtime(runtime_settings, false).map_err(|err| err.to_string())?;
            let controls_url = runtime.controls_url.clone();
            Ok::<_, String>((runtime, controls_url))
        })();
        let (runtime, controls_url) = match start_result {
            Ok(result) => result,
            Err(err) => {
                set_native_runtime_failure(&state, &err);
                return Err(err);
            }
        };

        let mut runtime_state = state
            .runtime
            .lock()
            .map_err(|_| "native runtime state is poisoned".to_string())?;
        runtime_state.runtime = Some(runtime);
        runtime_state.phase = ClientRuntimePhase::Running;
        runtime_state.last_error = None;
        Ok(NativeStartResponse {
            build: common::current_build_info(),
            running: true,
            phase: ClientRuntimePhase::Running,
            local_ui_url: controls_url,
            last_error: None,
        })
    }

    #[tauri::command]
    fn native_stop_client(state: tauri::State<'_, NativeAppState>) -> Result<(), String> {
        let runtime = {
            let mut runtime_state = state
                .runtime
                .lock()
                .map_err(|_| "native runtime state is poisoned".to_string())?;
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
    fn native_open_controls(state: tauri::State<'_, NativeAppState>) -> Result<(), String> {
        state.api().map(|_| ())
    }

    #[tauri::command]
    fn native_status(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<NativeStatusResponse, String> {
        let mut runtime_state = state
            .runtime
            .lock()
            .map_err(|_| "native runtime state is poisoned".to_string())?;
        refresh_native_runtime_state(&mut runtime_state);
        Ok(NativeStatusResponse {
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

    fn prepare_native_saved_settings(settings: &mut app::AppSettings) {
        settings.app_title = if settings.app_title.trim().is_empty() {
            "RedLine".to_string()
        } else {
            settings.app_title.trim().to_string()
        };
        settings.window_mode = app::AppWindowMode::Native;
        settings.disable_local_ui = false;
    }

    fn prepare_native_runtime_settings(settings: &mut app::AppSettings) {
        prepare_native_saved_settings(settings);
        settings.disable_local_ui = true;
    }

    fn native_profile_for_settings(
        settings: &app::AppSettings,
        last_connected_ms: Option<u64>,
    ) -> app::MobileServerProfile {
        let control = settings.control.clone();
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
            .unwrap_or_else(|| app::mobile_profile_id(&name, &control));
        app::MobileServerProfile {
            id,
            name,
            server: settings.server.to_string(),
            control,
            admin: existing.and_then(|profile| profile.admin.clone()),
            auth: existing.and_then(|profile| profile.auth.clone()),
            version: existing.and_then(|profile| profile.version.clone()),
            last_connected_ms,
            discovered: false,
        }
    }

    fn native_sorted_profiles(
        profiles: Vec<app::MobileServerProfile>,
    ) -> Vec<app::MobileServerProfile> {
        let mut profiles = profiles
            .into_iter()
            .map(app::normalize_mobile_profile)
            .collect::<Vec<_>>();
        profiles.sort_by(|a, b| {
            b.last_connected_ms
                .cmp(&a.last_connected_ms)
                .then_with(|| b.discovered.cmp(&a.discovered))
                .then_with(|| a.name.cmp(&b.name))
        });
        profiles
    }

    fn native_merge_profiles(
        mut saved: Vec<app::MobileServerProfile>,
        discovered: Vec<app::MobileServerProfile>,
    ) -> Vec<app::MobileServerProfile> {
        for mut profile in discovered {
            profile = app::normalize_mobile_profile(profile);
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
        native_sorted_profiles(saved)
    }

    fn native_parse_server_addr(value: &str) -> Result<std::net::SocketAddr, String> {
        if let Ok(addr) = value.parse() {
            return Ok(addr);
        }
        value
            .to_socket_addrs()
            .map_err(|err| format!("resolve server audio address {value}: {err}"))?
            .next()
            .ok_or_else(|| format!("resolve server audio address {value}: no addresses found"))
    }

    #[cfg(target_os = "macos")]
    fn native_discover_servers_for_platform() -> Result<Vec<app::MobileServerProfile>> {
        use std::collections::{BTreeMap, BTreeSet};
        use std::io::Read;
        use std::process::{Command, Stdio};

        const SERVICE_TYPE: &str = "_intercom-suite._tcp";

        fn dns_sd_output(args: &[&str], timeout: Duration) -> Result<String> {
            let mut child = Command::new("dns-sd")
                .args(args)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .context("start dns-sd")?;
            thread::sleep(timeout);
            let _ = child.kill();
            let _ = child.wait();
            let mut output = String::new();
            if let Some(mut stdout) = child.stdout.take() {
                stdout
                    .read_to_string(&mut output)
                    .context("read dns-sd output")?;
            }
            Ok(output)
        }

        fn browse_instance_names(output: &str) -> Vec<String> {
            let mut names = BTreeSet::new();
            for line in output.lines() {
                if !line.contains(" Add ") {
                    continue;
                }
                let Some((_, name)) = line.split_once("_intercom-suite._tcp.") else {
                    continue;
                };
                let name = name.trim();
                if !name.is_empty() {
                    names.insert(name.to_string());
                }
            }
            names.into_iter().collect()
        }

        fn decode_dns_sd_txt(value: &str) -> String {
            let mut decoded = String::new();
            let mut chars = value.chars().peekable();
            while let Some(ch) = chars.next() {
                if ch == '\\' {
                    let digits = chars.by_ref().take(3).collect::<String>().parse::<u8>();
                    if let Ok(byte) = digits {
                        decoded.push(byte as char);
                    }
                    continue;
                }
                decoded.push(ch);
            }
            decoded
        }

        fn parse_txt_records(output: &str) -> BTreeMap<String, String> {
            let mut records = BTreeMap::new();
            for token in output.split_whitespace() {
                let Some((key, value)) = token.split_once('=') else {
                    continue;
                };
                records.insert(key.to_string(), decode_dns_sd_txt(value));
            }
            records
        }

        fn resolve_host_ip(host: &str) -> Option<String> {
            let host = host.trim_end_matches('.');
            (host, 0)
                .to_socket_addrs()
                .ok()?
                .find(|addr| addr.is_ipv4())
                .or_else(|| (host, 0).to_socket_addrs().ok()?.next())
                .map(|addr| addr.ip().to_string())
        }

        fn host_for_url(host: &str) -> String {
            if host.contains(':') {
                format!("[{host}]")
            } else {
                host.to_string()
            }
        }

        fn parse_resolved_profile(
            instance: &str,
            output: &str,
        ) -> Option<app::MobileServerProfile> {
            let mut host = None;
            let mut port = None;
            for line in output.lines() {
                let Some((_, rest)) = line.split_once(" can be reached at ") else {
                    continue;
                };
                let target = rest
                    .split(" (")
                    .next()
                    .unwrap_or(rest)
                    .trim()
                    .trim_end_matches('.');
                let Some((raw_host, raw_port)) = target.rsplit_once(':') else {
                    continue;
                };
                host = Some(raw_host.trim_end_matches('.').to_string());
                port = raw_port.parse::<u16>().ok();
                break;
            }
            let host = host?;
            let control_port = port?;
            let ip = resolve_host_ip(&host).unwrap_or(host);
            let url_host = host_for_url(&ip);
            let txt = parse_txt_records(output);
            let audio_port = txt
                .get("audio_port")
                .and_then(|value| value.parse::<u16>().ok())
                .unwrap_or(control_port);
            let name = txt
                .get("name")
                .filter(|name| !name.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| instance.to_string());
            let control = format!("ws://{url_host}:{control_port}");

            Some(app::MobileServerProfile {
                id: app::mobile_profile_id(&name, &control),
                name,
                server: format!("{ip}:{audio_port}"),
                control,
                admin: txt
                    .get("admin_port")
                    .and_then(|value| value.parse::<u16>().ok())
                    .map(|admin_port| format!("http://{url_host}:{admin_port}")),
                auth: txt.get("auth").cloned(),
                version: txt.get("version").cloned(),
                last_connected_ms: None,
                discovered: true,
            })
        }

        let browse = dns_sd_output(&["-B", SERVICE_TYPE, "local"], Duration::from_millis(1_500))?;
        let mut profiles = Vec::new();
        for instance in browse_instance_names(&browse) {
            let resolved = dns_sd_output(
                &["-L", &instance, SERVICE_TYPE, "local"],
                Duration::from_millis(1_500),
            )?;
            if let Some(profile) = parse_resolved_profile(&instance, &resolved) {
                profiles.push(profile);
            }
        }
        Ok(profiles)
    }

    #[cfg(not(target_os = "macos"))]
    fn native_discover_servers_for_platform() -> Result<Vec<app::MobileServerProfile>> {
        Ok(Vec::new())
    }

    fn refresh_native_runtime_state(runtime_state: &mut NativeRuntimeState) {
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
                runtime_state.last_error = Some("native client runtime exited".to_string());
            }
            Err(err) => {
                runtime_state.phase = ClientRuntimePhase::Failed;
                runtime_state.last_error = Some(err.to_string());
            }
        }
    }

    fn set_native_runtime_failure(state: &tauri::State<'_, NativeAppState>, err: &str) {
        if let Ok(mut runtime_state) = state.runtime.lock() {
            runtime_state.runtime = None;
            runtime_state.phase = ClientRuntimePhase::Failed;
            runtime_state.last_error = Some(err.to_string());
        }
    }

    #[tauri::command]
    fn client_state(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::StateResponse, String> {
        Ok(state.api()?.state())
    }

    #[tauri::command]
    async fn client_config(
        state: tauri::State<'_, NativeAppState>,
        request: client_core::FullConfigRequest,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .apply_config(request)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_talk_mode(
        state: tauri::State<'_, NativeAppState>,
        mode: TalkMode,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .set_talk_mode(mode)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_mute(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state.api()?.mute().await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_unmute(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state.api()?.unmute().await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_talk_down(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .talk_down()
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_talk_up(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state.api()?.talk_up().await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_talk_toggle(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .talk_toggle()
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_codec(
        state: tauri::State<'_, NativeAppState>,
        codec: Codec,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .set_codec(codec)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    fn client_gain(
        state: tauri::State<'_, NativeAppState>,
        request: client_core::GainRequest,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .set_gain(request)
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_button_down(
        state: tauri::State<'_, NativeAppState>,
        id: ButtonId,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .button_down(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_button_up(
        state: tauri::State<'_, NativeAppState>,
        id: ButtonId,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .button_up(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_button_toggle(
        state: tauri::State<'_, NativeAppState>,
        id: ButtonId,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .button_toggle(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_call_down(
        state: tauri::State<'_, NativeAppState>,
        id: u16,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .call_down(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_call_up(
        state: tauri::State<'_, NativeAppState>,
        id: u16,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .call_up(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_call_toggle(
        state: tauri::State<'_, NativeAppState>,
        id: u16,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .call_toggle(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_reply_down(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .reply_down()
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_reply_up(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state.api()?.reply_up().await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_reply_toggle(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .reply_toggle()
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_send_alert(
        state: tauri::State<'_, NativeAppState>,
        request: client_core::AlertRequest,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .send_alert(request)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_ack_alert(
        state: tauri::State<'_, NativeAppState>,
        id: AlertId,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .ack_alert(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_cancel_alert(
        state: tauri::State<'_, NativeAppState>,
        id: AlertId,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api()?
            .cancel_alert(id)
            .await
            .map_err(|err| err.to_string())
    }

    fn run_tray_action(api: &desktop::LocalClientApi, action: NativeTrayAction) -> Result<String> {
        if action == NativeTrayAction::Status {
            return Ok(summarize_state_response(&api.state()));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tray command runtime")?;
        runtime.block_on(async {
            match action {
                NativeTrayAction::Mute => api.mute().await?,
                NativeTrayAction::Unmute => api.unmute().await?,
                NativeTrayAction::TalkDown => api.talk_down().await?,
                NativeTrayAction::TalkUp => api.talk_up().await?,
                _ => return Ok("status refreshed".to_string()),
            };
            Ok(format!("{action:?} sent"))
        })
    }

    fn summarize_state_response(state: &client_core::StateResponse) -> String {
        summarize_state_fields(
            state.user_id,
            &format!("{:?}", state.talk_mode),
            &format!("{:?}", state.codec),
        )
    }

    fn summarize_state_fields(user_id: u16, talk_mode: &str, codec: &str) -> String {
        format!("client {user_id} {talk_mode} {codec}")
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use clap::Parser;

        #[test]
        fn tray_action_maps_menu_ids() {
            assert_eq!(
                NativeTrayAction::from_menu_id(MENU_OPEN),
                NativeTrayAction::Open
            );
            assert_eq!(
                NativeTrayAction::from_menu_id(MENU_SETTINGS),
                NativeTrayAction::Settings
            );
            assert_eq!(
                NativeTrayAction::from_menu_id(MENU_STATUS),
                NativeTrayAction::Status
            );
            assert_eq!(
                NativeTrayAction::from_menu_id(MENU_MUTE),
                NativeTrayAction::Mute
            );
            assert_eq!(
                NativeTrayAction::from_menu_id(MENU_UNMUTE),
                NativeTrayAction::Unmute
            );
            assert_eq!(
                NativeTrayAction::from_menu_id(MENU_TALK_DOWN),
                NativeTrayAction::TalkDown
            );
            assert_eq!(
                NativeTrayAction::from_menu_id(MENU_TALK_UP),
                NativeTrayAction::TalkUp
            );
            assert_eq!(
                NativeTrayAction::from_menu_id(MENU_QUIT),
                NativeTrayAction::Quit
            );
            assert_eq!(
                NativeTrayAction::from_menu_id("other"),
                NativeTrayAction::Unknown
            );
        }

        #[test]
        fn tray_actions_map_to_client_api() {
            assert!(NativeTrayAction::Status.uses_client_api());
            assert!(NativeTrayAction::Mute.uses_client_api());
            assert!(NativeTrayAction::Unmute.uses_client_api());
            assert!(NativeTrayAction::TalkDown.uses_client_api());
            assert!(NativeTrayAction::TalkUp.uses_client_api());
            assert!(!NativeTrayAction::Open.uses_client_api());
            assert!(!NativeTrayAction::Settings.uses_client_api());
        }

        #[test]
        fn state_summary_uses_client_identity_mode_and_codec() {
            assert_eq!(
                summarize_state_fields(7, "Ptt", "Pcm48"),
                "client 7 Ptt Pcm48"
            );
        }

        #[test]
        fn native_defaults_to_native_window_mode() {
            let mut args = app::AppArgs::parse_from(["app-native"]);

            apply_native_defaults(&mut args);

            assert_eq!(args.window_mode, Some(app::AppWindowMode::Native));

            let mut explicit_disabled =
                app::AppArgs::parse_from(["app-native", "--window-mode", "disabled"]);
            apply_native_defaults(&mut explicit_disabled);

            assert_eq!(
                explicit_disabled.window_mode,
                Some(app::AppWindowMode::Disabled)
            );
        }
    }
}
