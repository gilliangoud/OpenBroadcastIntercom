use std::ffi::{c_char, c_void, CStr};
use std::ptr::NonNull;
use std::sync::Arc;

use anyhow::bail;
use client_core::AudioSettings;
use common::MIX_SAMPLE_RATE;
use tokio::sync::mpsc;

use super::{CaptureAdapter, CapturePipelineOptions, CaptureSourceInfo};

type IosVoiceInputCallback = unsafe extern "C" fn(*mut c_void, *const f32, usize);

unsafe extern "C" {
    fn intercom_ios_voice_input_create(
        callback: Option<IosVoiceInputCallback>,
        userdata: *mut c_void,
        error_buffer: *mut c_char,
        error_buffer_len: usize,
    ) -> *mut c_void;
    fn intercom_ios_voice_input_start(
        handle: *mut c_void,
        error_buffer: *mut c_char,
        error_buffer_len: usize,
    ) -> i32;
    fn intercom_ios_voice_input_stop(handle: *mut c_void);
    fn intercom_ios_voice_input_destroy(handle: *mut c_void);
}

pub struct VoiceProcessingInputStream {
    handle: NonNull<c_void>,
    capture: NonNull<CaptureAdapter>,
    started: bool,
}

unsafe impl Send for VoiceProcessingInputStream {}

impl VoiceProcessingInputStream {
    pub fn new(
        tx: mpsc::Sender<Vec<i16>>,
        audio_settings: Arc<AudioSettings>,
        capture_options: CapturePipelineOptions,
    ) -> anyhow::Result<Self> {
        if let Some(diagnostics) = &capture_options.diagnostics {
            diagnostics.set_source(CaptureSourceInfo {
                backend: super::AudioInputBackend::VoiceProcessing,
                device: "iOS default VoiceProcessingIO input".to_string(),
                sample_format: "F32".to_string(),
                sample_rate_hz: MIX_SAMPLE_RATE,
                channels: 1,
                channel_mode: capture_options.channel_mode,
            });
        }

        let capture = Box::new(CaptureAdapter::new(
            tx,
            MIX_SAMPLE_RATE,
            1,
            audio_settings,
            capture_options,
        ));
        let capture = NonNull::new(Box::into_raw(capture)).expect("boxed capture is non-null");

        let mut error_buffer = [0 as c_char; 512];
        let handle = unsafe {
            intercom_ios_voice_input_create(
                Some(capture_callback),
                capture.as_ptr().cast(),
                error_buffer.as_mut_ptr(),
                error_buffer.len(),
            )
        };
        let Some(handle) = NonNull::new(handle) else {
            unsafe {
                drop(Box::from_raw(capture.as_ptr()));
            }
            bail!(
                "{}",
                error_message(&error_buffer, "create iOS VoiceProcessingIO input")
            );
        };

        Ok(Self {
            handle,
            capture,
            started: false,
        })
    }

    pub fn play(&mut self) -> anyhow::Result<()> {
        if self.started {
            return Ok(());
        }

        let mut error_buffer = [0 as c_char; 512];
        let result = unsafe {
            intercom_ios_voice_input_start(
                self.handle.as_ptr(),
                error_buffer.as_mut_ptr(),
                error_buffer.len(),
            )
        };
        if result != 0 {
            bail!(
                "{}",
                error_message(&error_buffer, "start iOS VoiceProcessingIO input")
            );
        }
        self.started = true;
        Ok(())
    }
}

impl Drop for VoiceProcessingInputStream {
    fn drop(&mut self) {
        unsafe {
            intercom_ios_voice_input_stop(self.handle.as_ptr());
            intercom_ios_voice_input_destroy(self.handle.as_ptr());
            drop(Box::from_raw(self.capture.as_ptr()));
        }
    }
}

unsafe extern "C" fn capture_callback(
    userdata: *mut c_void,
    samples: *const f32,
    sample_count: usize,
) {
    if userdata.is_null() || samples.is_null() || sample_count == 0 {
        return;
    }

    let capture = unsafe { &mut *(userdata.cast::<CaptureAdapter>()) };
    let samples = unsafe { std::slice::from_raw_parts(samples, sample_count) };
    for sample in samples {
        capture.push_mono(*sample);
    }
}

fn error_message(buffer: &[c_char], fallback: &str) -> String {
    if buffer.first().copied().unwrap_or_default() == 0 {
        return fallback.to_string();
    }
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}
