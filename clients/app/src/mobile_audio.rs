use client_core::{ClientAudioBackend, ClientAudioBackendKind};

pub struct DefaultMobileAudioPlatform;

impl ClientAudioBackend for DefaultMobileAudioPlatform {
    fn kind(&self) -> ClientAudioBackendKind {
        #[cfg(target_os = "ios")]
        {
            ClientAudioBackendKind::IosAvAudioSession
        }
        #[cfg(not(target_os = "ios"))]
        {
            ClientAudioBackendKind::Raw
        }
    }

    fn prepare(&self) -> Result<(), String> {
        prepare_platform_audio_session()
    }
}

#[cfg(target_os = "ios")]
fn prepare_platform_audio_session() -> Result<(), String> {
    use std::ffi::CStr;
    use std::os::raw::c_char;

    unsafe extern "C" {
        fn intercom_ios_prepare_audio_session(error: *mut c_char, error_len: usize) -> i32;
    }

    let mut error = vec![0_i8; 512];
    let status = unsafe { intercom_ios_prepare_audio_session(error.as_mut_ptr(), error.len()) };
    if status == 0 {
        return Ok(());
    }
    let message = unsafe {
        CStr::from_ptr(error.as_ptr())
            .to_string_lossy()
            .into_owned()
    };
    Err(if message.is_empty() {
        "iOS audio session setup failed".to_string()
    } else {
        message
    })
}

#[cfg(not(target_os = "ios"))]
fn prepare_platform_audio_session() -> Result<(), String> {
    Ok(())
}
