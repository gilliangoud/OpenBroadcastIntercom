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
    use std::path::PathBuf;
    use std::thread;
    use std::thread::JoinHandle;
    use std::time::Duration;

    use anyhow::{bail, Context, Result};
    use clap::Parser;
    use common::{AlertId, ButtonId, Codec, TalkMode};
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
            || runtime_settings.disable_local_ui
            || runtime_settings.window_mode != app::AppWindowMode::Native
        {
            return run_desktop(runtime_settings, args.list_devices);
        }

        runtime_settings.disable_local_ui = true;
        let title = runtime_settings.app_title.clone();
        let delay_ms = runtime_settings.ui_open_delay_ms;
        let config_file = args.config_file.clone();

        let runtime = spawn_desktop_runtime(runtime_settings, args.list_devices)?;
        if delay_ms > 0 {
            thread::sleep(Duration::from_millis(delay_ms));
        }
        let result = run_tauri_window(title, runtime.api.clone(), config_file);
        runtime.shutdown()?;
        result
    }

    fn run_desktop(settings: app::AppSettings, list_devices: bool) -> Result<()> {
        let desktop_args = settings.desktop_args(list_devices)?;
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(desktop::run(desktop_args))
    }

    struct DesktopRuntimeHandle {
        api: desktop::LocalClientApi,
        shutdown_tx: Option<oneshot::Sender<()>>,
        join: Option<JoinHandle<Result<()>>>,
    }

    impl DesktopRuntimeHandle {
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
            api,
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
        })
    }

    fn run_tauri_window(
        title: String,
        api: desktop::LocalClientApi,
        config_file: PathBuf,
    ) -> Result<()> {
        let app_state = NativeAppState { api, config_file };

        tauri::Builder::default()
            .manage(app_state)
            .invoke_handler(tauri::generate_handler![
                load_native_settings,
                save_native_settings,
                default_native_settings,
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
                    tauri::WebviewUrl::App("client-controls.html".into()),
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

    #[derive(Clone)]
    struct NativeAppState {
        api: desktop::LocalClientApi,
        config_file: PathBuf,
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
                    match run_tray_action(&state.api, action) {
                        Ok(tooltip) => {
                            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                                let _ =
                                    tray.set_tooltip(Some(format!("Intercom Suite - {tooltip}")));
                            }
                        }
                        Err(err) => {
                            eprintln!("native tray command failed: {err:?}");
                            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                                let _ = tray.set_tooltip(Some(format!("Intercom Suite - {err}")));
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
        .title("Intercom Suite Settings")
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
        settings: app::AppSettings,
    ) -> Result<(), String> {
        app::save_settings(&state.config_file, &settings).map_err(|err| err.to_string())
    }

    #[tauri::command]
    fn default_native_settings() -> app::AppSettings {
        app::AppSettings::default()
    }

    #[tauri::command]
    fn client_state(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::StateResponse, String> {
        Ok(state.api.state())
    }

    #[tauri::command]
    async fn client_config(
        state: tauri::State<'_, NativeAppState>,
        request: client_core::FullConfigRequest,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api
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
            .api
            .set_talk_mode(mode)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_mute(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state.api.mute().await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_unmute(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state.api.unmute().await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_talk_down(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state.api.talk_down().await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_talk_up(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state.api.talk_up().await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_talk_toggle(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state.api.talk_toggle().await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_codec(
        state: tauri::State<'_, NativeAppState>,
        codec: Codec,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api
            .set_codec(codec)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    fn client_gain(
        state: tauri::State<'_, NativeAppState>,
        request: client_core::GainRequest,
    ) -> Result<client_core::OkResponse, String> {
        state.api.set_gain(request).map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_button_down(
        state: tauri::State<'_, NativeAppState>,
        id: ButtonId,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api
            .button_down(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_button_up(
        state: tauri::State<'_, NativeAppState>,
        id: ButtonId,
    ) -> Result<client_core::OkResponse, String> {
        state.api.button_up(id).await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_button_toggle(
        state: tauri::State<'_, NativeAppState>,
        id: ButtonId,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api
            .button_toggle(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_call_down(
        state: tauri::State<'_, NativeAppState>,
        id: u16,
    ) -> Result<client_core::OkResponse, String> {
        state.api.call_down(id).await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_call_up(
        state: tauri::State<'_, NativeAppState>,
        id: u16,
    ) -> Result<client_core::OkResponse, String> {
        state.api.call_up(id).await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_call_toggle(
        state: tauri::State<'_, NativeAppState>,
        id: u16,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api
            .call_toggle(id)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_reply_down(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state.api.reply_down().await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_reply_up(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state.api.reply_up().await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_reply_toggle(
        state: tauri::State<'_, NativeAppState>,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api
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
            .api
            .send_alert(request)
            .await
            .map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_ack_alert(
        state: tauri::State<'_, NativeAppState>,
        id: AlertId,
    ) -> Result<client_core::OkResponse, String> {
        state.api.ack_alert(id).await.map_err(|err| err.to_string())
    }

    #[tauri::command]
    async fn client_cancel_alert(
        state: tauri::State<'_, NativeAppState>,
        id: AlertId,
    ) -> Result<client_core::OkResponse, String> {
        state
            .api
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
