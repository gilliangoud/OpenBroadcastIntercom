use std::process::{Child, Command as StdCommand, Stdio};

use anyhow::Context;

pub const DISCOVERY_SERVICE_TYPE: &str = "_intercom-suite._tcp";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryAdvertisement {
    pub name: String,
    pub control_port: u16,
    pub audio_port: u16,
    pub admin_port: Option<u16>,
    pub auth_required: bool,
    pub version: String,
}

impl DiscoveryAdvertisement {
    pub fn txt_records(&self) -> Vec<String> {
        let mut records = vec![
            format!("audio_port={}", self.audio_port),
            format!("name={}", sanitize_discovery_txt_value(&self.name)),
            format!("version={}", sanitize_discovery_txt_value(&self.version)),
            format!(
                "auth={}",
                if self.auth_required {
                    "required"
                } else {
                    "none"
                }
            ),
        ];
        if let Some(admin_port) = self.admin_port {
            records.push(format!("admin_port={admin_port}"));
        }
        records
    }
}

pub struct DiscoveryAdvertisementHandle {
    child: Child,
}

impl Drop for DiscoveryAdvertisementHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn default_discovery_name() -> String {
    "Intercom Suite".to_string()
}

pub fn start_discovery_advertisement(
    advertisement: &DiscoveryAdvertisement,
) -> anyhow::Result<DiscoveryAdvertisementHandle> {
    let mut command = discovery_command(advertisement);
    let child = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("start dns-sd Bonjour advertisement")?;
    Ok(DiscoveryAdvertisementHandle { child })
}

pub(crate) fn discovery_command(advertisement: &DiscoveryAdvertisement) -> StdCommand {
    let mut command = StdCommand::new("dns-sd");
    command
        .arg("-R")
        .arg(&advertisement.name)
        .arg(DISCOVERY_SERVICE_TYPE)
        .arg("local.")
        .arg(advertisement.control_port.to_string());
    for record in advertisement.txt_records() {
        command.arg(record);
    }
    command
}

fn sanitize_discovery_txt_value(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !matches!(ch, '\0' | '\n' | '\r'))
        .collect::<String>()
}
