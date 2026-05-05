use anyhow::bail;
use client_core::MacosMicrophoneModeStatus;

unsafe extern "C" {
    fn intercom_macos_microphone_mode_supported() -> i32;
    fn intercom_macos_preferred_microphone_mode() -> i32;
    fn intercom_macos_active_microphone_mode() -> i32;
    fn intercom_macos_show_microphone_modes_ui();
}

pub fn status() -> Option<MacosMicrophoneModeStatus> {
    if !supported() {
        return None;
    }

    let preferred = mode_name(unsafe { intercom_macos_preferred_microphone_mode() });
    let active = mode_name(unsafe { intercom_macos_active_microphone_mode() });
    let voice_isolation_active = active == "voice_isolation";
    let note = if voice_isolation_active {
        None
    } else {
        Some(
            "Voice Isolation is user-selected in macOS Control Center; open microphone modes and choose Voice Isolation."
                .to_string(),
        )
    };

    Some(MacosMicrophoneModeStatus {
        preferred,
        active,
        voice_isolation_active,
        system_ui_available: true,
        note,
    })
}

pub fn show_system_microphone_modes_ui() -> anyhow::Result<()> {
    if !supported() {
        bail!("macOS microphone modes require macOS 12 or newer");
    }
    unsafe { intercom_macos_show_microphone_modes_ui() };
    Ok(())
}

fn supported() -> bool {
    unsafe { intercom_macos_microphone_mode_supported() != 0 }
}

fn mode_name(value: i32) -> String {
    match value {
        0 => "standard".to_string(),
        1 => "wide_spectrum".to_string(),
        2 => "voice_isolation".to_string(),
        -1 => "unsupported".to_string(),
        other => format!("unknown_{other}"),
    }
}

pub fn context_note() -> anyhow::Result<Option<String>> {
    let Some(status) = status() else {
        return Ok(None);
    };
    Ok(if status.voice_isolation_active {
        None
    } else {
        status.note
    })
}
