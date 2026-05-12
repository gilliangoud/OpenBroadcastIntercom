use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use clap::Parser;
use serde::{Deserialize, Serialize};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, default_value = "pi-buttons.json")]
    config_file: PathBuf,
    #[arg(long, default_value = "http://127.0.0.1:41001")]
    local_api: String,
    #[arg(long, env = "INTERCOM_LOCAL_API_TOKEN")]
    local_api_token: Option<String>,
    #[arg(long, default_value = "/sys/class/gpio")]
    gpio_root: PathBuf,
    #[arg(long)]
    init_config: bool,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct GpioConfig {
    debounce_ms: u64,
    poll_ms: u64,
    #[serde(default = "default_tally_poll_ms")]
    tally_poll_ms: u64,
    outputs: GpioOutputs,
    buttons: Vec<GpioButton>,
}

impl Default for GpioConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 30,
            poll_ms: 20,
            tally_poll_ms: default_tally_poll_ms(),
            outputs: GpioOutputs::default(),
            buttons: vec![
                GpioButton {
                    name: "regular-talk".to_string(),
                    gpio: 17,
                    active_low: true,
                    mode: ButtonMode::Momentary,
                    action: ButtonAction::RegularTalk,
                },
                GpioButton {
                    name: "director".to_string(),
                    gpio: 27,
                    active_low: true,
                    mode: ButtonMode::Momentary,
                    action: ButtonAction::TalkButton {
                        button_id: "director".to_string(),
                    },
                },
            ],
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct GpioOutputs {
    preview: Option<GpioOutput>,
    live: Option<GpioOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GpioOutput {
    name: String,
    gpio: u32,
    #[serde(default)]
    active_low: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GpioButton {
    name: String,
    gpio: u32,
    #[serde(default = "default_true")]
    active_low: bool,
    #[serde(default)]
    mode: ButtonMode,
    action: ButtonAction,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ButtonMode {
    #[default]
    Momentary,
    Latching,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ButtonAction {
    RegularTalk,
    TalkButton { button_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpTarget {
    host: String,
    port: u16,
}

#[derive(Debug)]
struct ButtonRuntime {
    config: GpioButton,
    stable_pressed: bool,
    candidate_pressed: bool,
    candidate_since: Instant,
}

#[derive(Debug, Default)]
struct TallyOutputState {
    preview: Option<bool>,
    live: Option<bool>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("pi_gpio=info".parse()?))
        .init();

    let args = Args::parse();
    if args.init_config {
        save_config(&args.config_file, &GpioConfig::default())?;
        eprintln!("wrote GPIO button config to {}", args.config_file.display());
        return Ok(());
    }

    let config = load_config(&args.config_file)?;
    validate_config(&config)?;
    let target = parse_http_target(&args.local_api)?;
    run_loop(
        config,
        &args.gpio_root,
        &target,
        args.local_api_token.as_deref(),
        args.dry_run,
    )
}

fn run_loop(
    config: GpioConfig,
    gpio_root: &Path,
    target: &HttpTarget,
    token: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<()> {
    let debounce = Duration::from_millis(config.debounce_ms);
    let poll = Duration::from_millis(config.poll_ms.max(1));
    let tally_poll = Duration::from_millis(config.tally_poll_ms.max(1));
    let outputs = config.outputs.clone();
    let mut last_tally_poll = Instant::now()
        .checked_sub(tally_poll)
        .unwrap_or_else(Instant::now);
    let mut tally_state = TallyOutputState::default();
    let mut buttons = config
        .buttons
        .into_iter()
        .map(|button| {
            let pressed = read_gpio_pressed(gpio_root, &button).unwrap_or(false);
            ButtonRuntime {
                config: button,
                stable_pressed: pressed,
                candidate_pressed: pressed,
                candidate_since: Instant::now(),
            }
        })
        .collect::<Vec<_>>();

    loop {
        for button in &mut buttons {
            let pressed = read_gpio_pressed(gpio_root, &button.config)
                .with_context(|| format!("read GPIO for {}", button.config.name))?;
            if pressed != button.candidate_pressed {
                button.candidate_pressed = pressed;
                button.candidate_since = Instant::now();
                continue;
            }
            if pressed != button.stable_pressed && button.candidate_since.elapsed() >= debounce {
                button.stable_pressed = pressed;
                for endpoint in endpoints_for_event(&button.config, pressed)? {
                    if dry_run {
                        tracing::info!(button = %button.config.name, endpoint, "GPIO event");
                    } else {
                        post_empty(target, &endpoint, token)?;
                    }
                }
            }
        }
        if last_tally_poll.elapsed() >= tally_poll {
            if let Err(err) = update_tally_outputs(
                gpio_root,
                &outputs,
                &mut tally_state,
                target,
                token,
                dry_run,
            ) {
                tracing::warn!(%err, "failed to update tally GPIO outputs");
            }
            last_tally_poll = Instant::now();
        }
        thread::sleep(poll);
    }
}

fn load_config(path: &Path) -> anyhow::Result<GpioConfig> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("read GPIO button config from {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("parse GPIO button config from {}", path.display()))
}

fn save_config(path: &Path, config: &GpioConfig) -> anyhow::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("create GPIO config directory {}", parent.display()))?;
    }
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(config)?))
        .with_context(|| format!("write GPIO button config to {}", path.display()))
}

fn validate_config(config: &GpioConfig) -> anyhow::Result<()> {
    if config.buttons.is_empty()
        && config.outputs.preview.is_none()
        && config.outputs.live.is_none()
    {
        bail!("GPIO config must define at least one button or tally output");
    }
    for output in [&config.outputs.preview, &config.outputs.live]
        .into_iter()
        .flatten()
    {
        if output.name.trim().is_empty() {
            bail!("GPIO output name cannot be empty");
        }
    }
    for button in &config.buttons {
        if button.name.trim().is_empty() {
            bail!("GPIO button name cannot be empty");
        }
        if let ButtonAction::TalkButton { button_id } = &button.action {
            if button_id.trim().is_empty() {
                bail!("GPIO talk button action requires a button_id");
            }
        }
    }
    Ok(())
}

fn read_gpio_pressed(root: &Path, button: &GpioButton) -> anyhow::Result<bool> {
    let path = root.join(format!("gpio{}/value", button.gpio));
    let value = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let high = value.trim() == "1";
    Ok(if button.active_low { !high } else { high })
}

fn endpoints_for_event(button: &GpioButton, pressed: bool) -> anyhow::Result<Vec<String>> {
    let endpoints = match (&button.action, button.mode, pressed) {
        (ButtonAction::RegularTalk, ButtonMode::Momentary, true) => vec!["/talk/down".to_string()],
        (ButtonAction::RegularTalk, ButtonMode::Momentary, false) => vec!["/talk/up".to_string()],
        (ButtonAction::RegularTalk, ButtonMode::Latching, true) => {
            vec!["/talk/toggle".to_string()]
        }
        (ButtonAction::RegularTalk, ButtonMode::Latching, false) => Vec::new(),
        (ButtonAction::TalkButton { button_id }, ButtonMode::Momentary, true) => {
            vec![format!("/buttons/{}/down", encode_path_segment(button_id))]
        }
        (ButtonAction::TalkButton { button_id }, ButtonMode::Momentary, false) => {
            vec![format!("/buttons/{}/up", encode_path_segment(button_id))]
        }
        (ButtonAction::TalkButton { button_id }, ButtonMode::Latching, true) => {
            vec![format!(
                "/buttons/{}/toggle",
                encode_path_segment(button_id)
            )]
        }
        (ButtonAction::TalkButton { .. }, ButtonMode::Latching, false) => Vec::new(),
    };
    Ok(endpoints)
}

fn update_tally_outputs(
    root: &Path,
    outputs: &GpioOutputs,
    cached: &mut TallyOutputState,
    target: &HttpTarget,
    token: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<()> {
    if outputs.preview.is_none() && outputs.live.is_none() {
        return Ok(());
    }
    let state = get_local_state(target, token)?;
    let tally = state
        .get("tally")
        .and_then(|tally| tally.get("state"))
        .and_then(|state| state.as_str())
        .unwrap_or("off");
    let preview = tally == "preview";
    let live = tally == "live";
    if let Some(output) = &outputs.preview {
        if cached.preview != Some(preview) {
            write_gpio_output(root, output, preview, dry_run)?;
            cached.preview = Some(preview);
        }
    }
    if let Some(output) = &outputs.live {
        if cached.live != Some(live) {
            write_gpio_output(root, output, live, dry_run)?;
            cached.live = Some(live);
        }
    }
    Ok(())
}

fn write_gpio_output(
    root: &Path,
    output: &GpioOutput,
    active: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let high = if output.active_low { !active } else { active };
    let value = if high { "1\n" } else { "0\n" };
    if dry_run {
        tracing::info!(output = %output.name, gpio = output.gpio, active, high, "GPIO tally output");
        return Ok(());
    }
    let path = root.join(format!("gpio{}/value", output.gpio));
    fs::write(&path, value).with_context(|| format!("write {}", path.display()))
}

fn parse_http_target(value: &str) -> anyhow::Result<HttpTarget> {
    let rest = value
        .strip_prefix("http://")
        .context("local API URL must start with http://")?;
    let host_port = rest.trim_end_matches('/');
    let (host, port) = host_port
        .rsplit_once(':')
        .context("local API URL must include host:port")?;
    Ok(HttpTarget {
        host: host.to_string(),
        port: port.parse().context("parse local API port")?,
    })
}

fn get_local_state(target: &HttpTarget, token: Option<&str>) -> anyhow::Result<serde_json::Value> {
    let response = request(target, "GET", "/api/state", token)?;
    if !(response.starts_with("HTTP/1.1 2") || response.starts_with("HTTP/1.0 2")) {
        bail!(
            "local API rejected /api/state: {}",
            response.lines().next().unwrap_or("")
        );
    }
    let (_headers, body) = response
        .split_once("\r\n\r\n")
        .context("local API state response has no body")?;
    serde_json::from_str(body).context("parse local API state JSON")
}

fn post_empty(target: &HttpTarget, endpoint: &str, token: Option<&str>) -> anyhow::Result<()> {
    let response = request(target, "POST", endpoint, token)?;
    if response.starts_with("HTTP/1.1 2") || response.starts_with("HTTP/1.0 2") {
        Ok(())
    } else {
        bail!(
            "local API rejected {endpoint}: {}",
            response.lines().next().unwrap_or("")
        )
    }
}

fn request(
    target: &HttpTarget,
    method: &str,
    endpoint: &str,
    token: Option<&str>,
) -> anyhow::Result<String> {
    let mut stream = TcpStream::connect((&*target.host, target.port))
        .with_context(|| format!("connect to local API {}:{}", target.host, target.port))?;
    let authorization = token
        .filter(|token| !token.is_empty())
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {endpoint} HTTP/1.1\r\nHost: {}:{}\r\n{authorization}Content-Length: 0\r\nConnection: close\r\n\r\n",
        target.host, target.port
    );
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

fn default_true() -> bool {
    true
}

fn default_tally_poll_ms() -> u64 {
    500
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn maps_momentary_regular_talk_to_talk_down_up() {
        let button = GpioButton {
            name: "regular-talk".to_string(),
            gpio: 1,
            active_low: true,
            mode: ButtonMode::Momentary,
            action: ButtonAction::RegularTalk,
        };

        assert_eq!(
            endpoints_for_event(&button, true).unwrap(),
            vec!["/talk/down"]
        );
        assert_eq!(
            endpoints_for_event(&button, false).unwrap(),
            vec!["/talk/up"]
        );
    }

    #[test]
    fn maps_talk_buttons_to_button_endpoints() {
        let mut button = GpioButton {
            name: "director".to_string(),
            gpio: 2,
            active_low: true,
            mode: ButtonMode::Momentary,
            action: ButtonAction::TalkButton {
                button_id: "director one".to_string(),
            },
        };

        assert_eq!(
            endpoints_for_event(&button, true).unwrap(),
            vec!["/buttons/director%20one/down"]
        );
        assert_eq!(
            endpoints_for_event(&button, false).unwrap(),
            vec!["/buttons/director%20one/up"]
        );

        button.mode = ButtonMode::Latching;
        assert_eq!(
            endpoints_for_event(&button, true).unwrap(),
            vec!["/buttons/director%20one/toggle"]
        );
        assert!(endpoints_for_event(&button, false).unwrap().is_empty());
    }

    #[test]
    fn parses_local_api_url() {
        assert_eq!(
            parse_http_target("http://127.0.0.1:41001").unwrap(),
            HttpTarget {
                host: "127.0.0.1".to_string(),
                port: 41001
            }
        );
    }

    #[test]
    fn reads_active_low_gpio_value() {
        let root = unique_test_dir();
        let gpio_dir = root.join("gpio17");
        fs::create_dir_all(&gpio_dir).unwrap();
        fs::write(gpio_dir.join("value"), "0\n").unwrap();

        let button = GpioButton {
            name: "regular-talk".to_string(),
            gpio: 17,
            active_low: true,
            mode: ButtonMode::Momentary,
            action: ButtonAction::RegularTalk,
        };

        assert!(read_gpio_pressed(&root, &button).unwrap());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn writes_active_high_and_active_low_outputs() {
        let root = unique_test_dir();
        fs::create_dir_all(root.join("gpio22")).unwrap();
        fs::create_dir_all(root.join("gpio23")).unwrap();

        write_gpio_output(
            &root,
            &GpioOutput {
                name: "preview".to_string(),
                gpio: 22,
                active_low: false,
            },
            true,
            false,
        )
        .unwrap();
        write_gpio_output(
            &root,
            &GpioOutput {
                name: "live".to_string(),
                gpio: 23,
                active_low: true,
            },
            true,
            false,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(root.join("gpio22/value")).unwrap(),
            "1\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("gpio23/value")).unwrap(),
            "0\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn default_config_is_valid() {
        let config = GpioConfig::default();
        assert_eq!(config.tally_poll_ms, 500);
        validate_config(&config).unwrap();
    }

    #[test]
    fn output_only_config_is_valid() {
        validate_config(&GpioConfig {
            buttons: Vec::new(),
            outputs: GpioOutputs {
                preview: Some(GpioOutput {
                    name: "preview".to_string(),
                    gpio: 22,
                    active_low: false,
                }),
                live: None,
            },
            ..GpioConfig::default()
        })
        .unwrap();
    }

    fn unique_test_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("intercom-pi-gpio-test-{nanos}"))
    }
}
