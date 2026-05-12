use std::env;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use libloading::Library;

const NDI_FRAME_TYPE_NONE: c_int = 0;
const NDI_FRAME_TYPE_AUDIO: c_int = 2;
const NDI_FRAME_TYPE_ERROR: c_int = 4;
const NDI_RECV_BANDWIDTH_AUDIO_ONLY: c_int = 10;
const NDI_FOURCC_FLTP: u32 = u32::from_le_bytes(*b"FLTp");
const NDI_TIME_CODE_SYNTHESIZE: i64 = i64::MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NdiSource {
    pub name: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NdiAudioFrame {
    pub sample_rate_hz: u32,
    pub channels: usize,
    pub samples: Vec<f32>,
}

#[derive(Clone)]
pub struct NdiRuntime {
    inner: Arc<NdiRuntimeInner>,
}

struct NdiRuntimeInner {
    _library: Library,
    path: Option<PathBuf>,
    fns: NdiFns,
}

struct NdiFns {
    initialize: unsafe extern "C" fn() -> bool,
    destroy: unsafe extern "C" fn(),
    find_create_v2: unsafe extern "C" fn(*const NdiFindCreateV2) -> *mut c_void,
    find_destroy: unsafe extern "C" fn(*mut c_void),
    find_wait_for_sources: unsafe extern "C" fn(*mut c_void, u32) -> bool,
    find_get_current_sources: unsafe extern "C" fn(*mut c_void, *mut u32) -> *const NdiSourceFfi,
    recv_create_v3: unsafe extern "C" fn(*const NdiRecvCreateV3) -> *mut c_void,
    recv_destroy: unsafe extern "C" fn(*mut c_void),
    recv_capture_v3: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        *mut NdiAudioFrameV3,
        *mut c_void,
        u32,
    ) -> c_int,
    recv_free_audio_v3: unsafe extern "C" fn(*mut c_void, *mut NdiAudioFrameV3),
    send_create: unsafe extern "C" fn(*const NdiSendCreate) -> *mut c_void,
    send_destroy: unsafe extern "C" fn(*mut c_void),
    send_send_audio_v3: unsafe extern "C" fn(*mut c_void, *const NdiAudioFrameV3),
}

#[repr(C)]
struct NdiSourceFfi {
    p_ndi_name: *const c_char,
    p_url_address: *const c_char,
}

#[repr(C)]
struct NdiFindCreateV2 {
    show_local_sources: bool,
    p_groups: *const c_char,
    p_extra_ips: *const c_char,
}

#[repr(C)]
struct NdiRecvCreateV3 {
    source_to_connect_to: NdiSourceFfi,
    color_format: c_int,
    bandwidth: c_int,
    allow_video_fields: bool,
    p_ndi_recv_name: *const c_char,
}

#[repr(C)]
struct NdiSendCreate {
    p_ndi_name: *const c_char,
    p_groups: *const c_char,
    clock_video: bool,
    clock_audio: bool,
}

#[repr(C)]
struct NdiAudioFrameV3 {
    sample_rate: c_int,
    no_channels: c_int,
    no_samples: c_int,
    timecode: i64,
    four_cc: u32,
    p_data: *mut u8,
    channel_stride_in_bytes: c_int,
    p_metadata: *const c_char,
    timestamp: i64,
}

pub struct NdiReceiver {
    runtime: NdiRuntime,
    handle: *mut c_void,
    _source_name: CString,
    _receiver_name: CString,
}

pub struct NdiSender {
    runtime: NdiRuntime,
    handle: *mut c_void,
    _name: CString,
    _groups: Option<CString>,
    planar: Vec<f32>,
}

unsafe impl Send for NdiReceiver {}
unsafe impl Send for NdiSender {}

impl NdiRuntime {
    pub fn load() -> anyhow::Result<Self> {
        let candidates = library_candidates();
        let mut load_errors = Vec::new();
        for candidate in &candidates {
            let load_result = unsafe { Library::new(candidate) };
            match load_result {
                Ok(library) => {
                    let path = candidate_path(&candidate);
                    match unsafe { Self::from_library(library, path) } {
                        Ok(runtime) => return Ok(runtime),
                        Err(err) => {
                            tracing::debug!(candidate, %err, "NDI runtime candidate opened but was unusable");
                            load_errors.push(format!("{candidate}: {err}"));
                        }
                    }
                }
                Err(err) => {
                    tracing::debug!(candidate, %err, "NDI runtime candidate failed to load")
                }
            }
        }
        let symbol_errors = if load_errors.is_empty() {
            String::new()
        } else {
            format!(" Usable library errors: {}", load_errors.join("; "))
        };
        bail!(
            "NDI runtime library was not found. Install the NDI runtime/SDK and make libndi available on the system library path. Tried: {}.{}",
            candidates.join(", "),
            symbol_errors
        );
    }

    pub fn library_path(&self) -> Option<&Path> {
        self.inner.path.as_deref()
    }

    pub fn find_sources(
        &self,
        timeout: Duration,
        groups: Option<&str>,
    ) -> anyhow::Result<Vec<NdiSource>> {
        let groups = optional_cstring(groups)?;
        let create = NdiFindCreateV2 {
            show_local_sources: true,
            p_groups: groups
                .as_ref()
                .map(|value| value.as_ptr())
                .unwrap_or(std::ptr::null()),
            p_extra_ips: std::ptr::null(),
        };
        let handle = unsafe { (self.inner.fns.find_create_v2)(&create) };
        if handle.is_null() {
            bail!("NDI source discovery could not be started");
        }
        let guard = NdiFindGuard {
            runtime: self.clone(),
            handle,
        };
        let timeout_ms = millis_u32(timeout);
        unsafe {
            (self.inner.fns.find_wait_for_sources)(guard.handle, timeout_ms);
        }
        let mut count = 0_u32;
        let ptr = unsafe { (self.inner.fns.find_get_current_sources)(guard.handle, &mut count) };
        if ptr.is_null() || count == 0 {
            return Ok(Vec::new());
        }
        let mut sources = Vec::with_capacity(count as usize);
        for source in unsafe { std::slice::from_raw_parts(ptr, count as usize) } {
            let Some(name) = cstr_to_string(source.p_ndi_name) else {
                continue;
            };
            sources.push(NdiSource {
                name,
                url: cstr_to_string(source.p_url_address),
            });
        }
        Ok(sources)
    }

    pub fn receiver(&self, source_name: &str) -> anyhow::Result<NdiReceiver> {
        let source_name = CString::new(source_name.trim())
            .context("NDI source name contains an embedded null byte")?;
        let receiver_name = CString::new("RedLine Bridge").expect("static string is valid C");
        let create = NdiRecvCreateV3 {
            source_to_connect_to: NdiSourceFfi {
                p_ndi_name: source_name.as_ptr(),
                p_url_address: std::ptr::null(),
            },
            color_format: 0,
            bandwidth: NDI_RECV_BANDWIDTH_AUDIO_ONLY,
            allow_video_fields: false,
            p_ndi_recv_name: receiver_name.as_ptr(),
        };
        let handle = unsafe { (self.inner.fns.recv_create_v3)(&create) };
        if handle.is_null() {
            bail!(
                "NDI receiver could not connect to `{}`",
                source_name.to_string_lossy()
            );
        }
        Ok(NdiReceiver {
            runtime: self.clone(),
            handle,
            _source_name: source_name,
            _receiver_name: receiver_name,
        })
    }

    pub fn sender(&self, name: &str, groups: Option<&str>) -> anyhow::Result<NdiSender> {
        let name =
            CString::new(name.trim()).context("NDI output name contains an embedded null byte")?;
        let groups = optional_cstring(groups)?;
        let create = NdiSendCreate {
            p_ndi_name: name.as_ptr(),
            p_groups: groups
                .as_ref()
                .map(|value| value.as_ptr())
                .unwrap_or(std::ptr::null()),
            clock_video: false,
            clock_audio: true,
        };
        let handle = unsafe { (self.inner.fns.send_create)(&create) };
        if handle.is_null() {
            bail!(
                "NDI output source `{}` could not be created",
                name.to_string_lossy()
            );
        }
        Ok(NdiSender {
            runtime: self.clone(),
            handle,
            _name: name,
            _groups: groups,
            planar: Vec::new(),
        })
    }

    unsafe fn from_library(library: Library, path: Option<PathBuf>) -> anyhow::Result<Self> {
        let fns = NdiFns {
            initialize: *library.get(b"NDIlib_initialize\0")?,
            destroy: *library.get(b"NDIlib_destroy\0")?,
            find_create_v2: *library.get(b"NDIlib_find_create_v2\0")?,
            find_destroy: *library.get(b"NDIlib_find_destroy\0")?,
            find_wait_for_sources: *library.get(b"NDIlib_find_wait_for_sources\0")?,
            find_get_current_sources: *library.get(b"NDIlib_find_get_current_sources\0")?,
            recv_create_v3: *library.get(b"NDIlib_recv_create_v3\0")?,
            recv_destroy: *library.get(b"NDIlib_recv_destroy\0")?,
            recv_capture_v3: *library.get(b"NDIlib_recv_capture_v3\0")?,
            recv_free_audio_v3: *library.get(b"NDIlib_recv_free_audio_v3\0")?,
            send_create: *library.get(b"NDIlib_send_create\0")?,
            send_destroy: *library.get(b"NDIlib_send_destroy\0")?,
            send_send_audio_v3: *library.get(b"NDIlib_send_send_audio_v3\0")?,
        };
        if !(fns.initialize)() {
            bail!("NDI runtime initialization failed");
        }
        Ok(Self {
            inner: Arc::new(NdiRuntimeInner {
                _library: library,
                path,
                fns,
            }),
        })
    }
}

impl NdiReceiver {
    pub fn capture_audio(&mut self, timeout: Duration) -> anyhow::Result<Option<NdiAudioFrame>> {
        let mut frame = NdiAudioFrameV3 {
            sample_rate: 0,
            no_channels: 0,
            no_samples: 0,
            timecode: 0,
            four_cc: 0,
            p_data: std::ptr::null_mut(),
            channel_stride_in_bytes: 0,
            p_metadata: std::ptr::null(),
            timestamp: 0,
        };
        let frame_type = unsafe {
            (self.runtime.inner.fns.recv_capture_v3)(
                self.handle,
                std::ptr::null_mut(),
                &mut frame,
                std::ptr::null_mut(),
                millis_u32(timeout),
            )
        };
        match frame_type {
            NDI_FRAME_TYPE_NONE => Ok(None),
            NDI_FRAME_TYPE_AUDIO => {
                let copied = copy_audio_frame(&frame);
                unsafe {
                    (self.runtime.inner.fns.recv_free_audio_v3)(self.handle, &mut frame);
                }
                copied.map(Some)
            }
            NDI_FRAME_TYPE_ERROR => bail!("NDI receiver reported an error"),
            _ => Ok(None),
        }
    }
}

impl NdiSender {
    pub fn send_interleaved_f32(
        &mut self,
        sample_rate_hz: u32,
        channels: usize,
        samples: &[f32],
    ) -> anyhow::Result<()> {
        if channels == 0 {
            bail!("cannot send NDI audio with zero channels");
        }
        let samples_per_channel = samples.len() / channels;
        if samples_per_channel == 0 {
            return Ok(());
        }
        let used = samples_per_channel * channels;
        self.planar.resize(used, 0.0);
        for sample_index in 0..samples_per_channel {
            for channel in 0..channels {
                self.planar[channel * samples_per_channel + sample_index] =
                    samples[sample_index * channels + channel].clamp(-1.0, 1.0);
            }
        }
        let frame = NdiAudioFrameV3 {
            sample_rate: sample_rate_hz.try_into().unwrap_or(48_000),
            no_channels: channels.try_into().unwrap_or(i32::MAX),
            no_samples: samples_per_channel.try_into().unwrap_or(i32::MAX),
            timecode: NDI_TIME_CODE_SYNTHESIZE,
            four_cc: NDI_FOURCC_FLTP,
            p_data: self.planar.as_mut_ptr().cast(),
            channel_stride_in_bytes: (samples_per_channel * std::mem::size_of::<f32>())
                .try_into()
                .unwrap_or(i32::MAX),
            p_metadata: std::ptr::null(),
            timestamp: 0,
        };
        unsafe {
            (self.runtime.inner.fns.send_send_audio_v3)(self.handle, &frame);
        }
        Ok(())
    }
}

impl Drop for NdiReceiver {
    fn drop(&mut self) {
        unsafe {
            (self.runtime.inner.fns.recv_destroy)(self.handle);
        }
    }
}

impl Drop for NdiSender {
    fn drop(&mut self) {
        unsafe {
            (self.runtime.inner.fns.send_destroy)(self.handle);
        }
    }
}

impl Drop for NdiRuntimeInner {
    fn drop(&mut self) {
        unsafe {
            (self.fns.destroy)();
        }
    }
}

struct NdiFindGuard {
    runtime: NdiRuntime,
    handle: *mut c_void,
}

impl Drop for NdiFindGuard {
    fn drop(&mut self) {
        unsafe {
            (self.runtime.inner.fns.find_destroy)(self.handle);
        }
    }
}

fn copy_audio_frame(frame: &NdiAudioFrameV3) -> anyhow::Result<NdiAudioFrame> {
    if frame.p_data.is_null() {
        bail!("NDI audio frame had no data");
    }
    if frame.no_channels <= 0 || frame.no_samples <= 0 || frame.sample_rate <= 0 {
        bail!("NDI audio frame had invalid dimensions");
    }
    if frame.four_cc != NDI_FOURCC_FLTP {
        bail!("unsupported NDI audio format: 0x{:08x}", frame.four_cc);
    }
    let channels = frame.no_channels as usize;
    let samples_per_channel = frame.no_samples as usize;
    let stride = if frame.channel_stride_in_bytes > 0 {
        frame.channel_stride_in_bytes as usize
    } else {
        samples_per_channel * std::mem::size_of::<f32>()
    };
    let mut samples = vec![0.0; channels * samples_per_channel];
    for sample_index in 0..samples_per_channel {
        for channel in 0..channels {
            let offset = channel * stride + sample_index * std::mem::size_of::<f32>();
            let value = unsafe { std::ptr::read_unaligned(frame.p_data.add(offset).cast::<f32>()) };
            samples[sample_index * channels + channel] = value;
        }
    }
    Ok(NdiAudioFrame {
        sample_rate_hz: frame.sample_rate as u32,
        channels,
        samples,
    })
}

fn cstr_to_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let value = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .trim()
        .to_string();
    (!value.is_empty()).then_some(value)
}

fn optional_cstring(value: Option<&str>) -> anyhow::Result<Option<CString>> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| CString::new(value).context("NDI string contains an embedded null byte"))
        .transpose()
}

fn millis_u32(duration: Duration) -> u32 {
    duration.as_millis().try_into().unwrap_or(u32::MAX)
}

fn candidate_path(candidate: &str) -> Option<PathBuf> {
    let path = PathBuf::from(candidate);
    path.is_absolute().then_some(path)
}

fn library_candidates() -> Vec<String> {
    let mut candidates = Vec::new();
    for key in ["NDI_RUNTIME_DIR", "NDI_SDK_DIR"] {
        if let Some(dir) = env::var_os(key) {
            push_dir_candidates(&mut candidates, Path::new(&dir));
        }
    }
    #[cfg(target_os = "macos")]
    {
        candidates.extend([
            "/usr/local/lib/libndi.dylib".to_string(),
            "/usr/local/lib/libndi.6.dylib".to_string(),
            "/usr/local/lib/libndi.5.dylib".to_string(),
            "/usr/local/lib/libndi.4.dylib".to_string(),
            "/opt/homebrew/lib/libndi.dylib".to_string(),
            "/opt/homebrew/lib/libndi.6.dylib".to_string(),
            "/opt/homebrew/lib/libndi.5.dylib".to_string(),
            "/opt/homebrew/lib/libndi.4.dylib".to_string(),
            "/Library/NDI SDK for Apple/lib/macOS/libndi.dylib".to_string(),
            "/Library/Application Support/NDI/lib/libndi.dylib".to_string(),
            "/Applications/NDI Monitor.app/Contents/Frameworks/libndi.dylib".to_string(),
            "/Applications/NDI Virtual Input.app/Contents/Frameworks/libndi.dylib".to_string(),
            "/Applications/NDI Scan Converter.app/Contents/Frameworks/libndi.dylib".to_string(),
            "/Applications/NDI Router.app/Contents/Frameworks/NTFramework.framework/Versions/A/Frameworks/libndi.dylib"
                .to_string(),
            "/Applications/NDI Discovery.app/Contents/Frameworks/libndi_advanced.dylib".to_string(),
            "/Applications/NDI Video Monitor.app/Contents/Frameworks/libndi_advanced.dylib"
                .to_string(),
            "/Applications/NDI Test Patterns.app/Contents/Frameworks/libndi_advanced.dylib"
                .to_string(),
            "/Library/Audio/Plug-Ins/HAL/NDIAudio.driver/Contents/Frameworks/libndi_advanced.dylib"
                .to_string(),
            "/Library/CoreMediaIO/Plug-Ins/DAL/NDIVideoOut.plugin/Contents/Frameworks/libndi.dylib"
                .to_string(),
            "/Library/CoreMediaIO/Plug-Ins/DAL/NDIVirtualCamera.plugin/Contents/Frameworks/libndi_advanced.dylib"
                .to_string(),
            "/Library/SystemExtensions/050BB35D-1379-45AB-B4B6-7A07CDD1FBE0/com.newtek.Application-Mac-NDI-VirtualInput.Extension.systemextension/Contents/Frameworks/libndi_advanced.dylib"
                .to_string(),
            "/Library/Application Support/Adobe/Common/Plug-ins/7.0/MediaCore/NDI_Transmit_AdobeCC.bundle/Contents/Frameworks/libndi.dylib"
                .to_string(),
            "libndi.dylib".to_string(),
            "libndi.6.dylib".to_string(),
            "libndi.5.dylib".to_string(),
            "libndi.4.dylib".to_string(),
        ]);
    }
    #[cfg(target_os = "linux")]
    {
        candidates.extend([
            "libndi.so".to_string(),
            "libndi.so.6".to_string(),
            "/usr/local/lib/libndi.so".to_string(),
            "/usr/lib/libndi.so".to_string(),
        ]);
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(path) = env::var_os("PATH") {
            for dir in env::split_paths(&path) {
                candidates.push(dir.join("Processing.NDI.Lib.x64.dll").display().to_string());
            }
        }
        candidates.push("Processing.NDI.Lib.x64.dll".to_string());
    }
    dedupe(candidates)
}

fn push_dir_candidates(candidates: &mut Vec<String>, dir: &Path) {
    #[cfg(target_os = "macos")]
    {
        candidates.push(dir.join("libndi.dylib").display().to_string());
        candidates.push(dir.join("libndi.6.dylib").display().to_string());
        candidates.push(dir.join("libndi.5.dylib").display().to_string());
        candidates.push(dir.join("libndi.4.dylib").display().to_string());
        candidates.push(dir.join("lib/macOS/libndi.dylib").display().to_string());
        candidates.push(dir.join("lib/macOS/libndi.6.dylib").display().to_string());
        candidates.push(dir.join("lib/macOS/libndi.5.dylib").display().to_string());
        candidates.push(dir.join("lib/macOS/libndi.4.dylib").display().to_string());
    }
    #[cfg(target_os = "linux")]
    {
        candidates.push(dir.join("libndi.so").display().to_string());
        candidates.push(dir.join("lib/libndi.so").display().to_string());
    }
    #[cfg(target_os = "windows")]
    {
        candidates.push(dir.join("Processing.NDI.Lib.x64.dll").display().to_string());
    }
}

fn dedupe(values: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_planarizes_interleaved_audio() {
        let input = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
        let mut planar = vec![0.0; input.len()];
        let channels = 2;
        let samples_per_channel = input.len() / channels;
        for sample_index in 0..samples_per_channel {
            for channel in 0..channels {
                planar[channel * samples_per_channel + sample_index] =
                    input[sample_index * channels + channel];
            }
        }
        assert_eq!(planar, vec![0.1, 0.3, 0.5, 0.2, 0.4, 0.6]);
    }

    #[test]
    fn library_candidates_include_platform_name() {
        let candidates = library_candidates();
        #[cfg(target_os = "macos")]
        {
            assert!(candidates.iter().any(|path| path.ends_with("libndi.dylib")));
            assert!(candidates
                .iter()
                .any(|path| path.ends_with("libndi.4.dylib")));
            assert!(candidates
                .iter()
                .any(|path| path.contains("NDI Monitor.app")));
        }
        #[cfg(target_os = "linux")]
        assert!(candidates.iter().any(|path| path.contains("libndi.so")));
        #[cfg(target_os = "windows")]
        assert!(candidates
            .iter()
            .any(|path| path.ends_with("Processing.NDI.Lib.x64.dll")));
    }

    #[test]
    fn fltp_fourcc_matches_ndi_value() {
        assert_eq!(NDI_FOURCC_FLTP, 1_884_572_742);
    }
}
