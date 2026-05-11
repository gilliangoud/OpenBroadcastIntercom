use std::{collections::BTreeMap, path::PathBuf, time::Instant};

use anyhow::{bail, Context};
use clap::{Parser, ValueEnum};
use hound::{SampleFormat, WavReader};
use serde::{Deserialize, Serialize};

#[cfg(feature = "transcription-whisper")]
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

#[derive(Debug, Parser)]
#[command(about = "Run RedLine transcription benchmark corpora through whisper-rs")]
struct Args {
    #[arg(long)]
    corpus: PathBuf,
    #[arg(long)]
    model: PathBuf,
    #[arg(long)]
    model_id: String,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long, default_value = "reliable")]
    mode: BenchmarkMode,
    #[arg(long, default_value = "en")]
    language: String,
    #[arg(long)]
    prompt: Option<String>,
    #[arg(long)]
    threads: Option<i32>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BenchmarkMode {
    Fast,
    Balanced,
    Reliable,
}

impl BenchmarkMode {
    fn reliable(self) -> bool {
        matches!(self, Self::Reliable)
    }

    fn default_threads(self) -> i32 {
        if self.reliable() {
            4
        } else {
            2
        }
    }
}

#[derive(Debug, Deserialize)]
struct Corpus {
    segments: Vec<CorpusSegment>,
}

#[derive(Debug, Deserialize)]
struct CorpusSegment {
    id: String,
    audio: String,
}

#[derive(Debug, Serialize)]
struct Predictions {
    model_id: String,
    runtime: &'static str,
    backend: BackendInfo,
    model_load_ms: f64,
    mode: String,
    language: String,
    threads: i32,
    segments: BTreeMap<String, PredictionSegment>,
}

#[derive(Debug, Serialize)]
struct BackendInfo {
    metal: bool,
    coreml: bool,
    target_os: &'static str,
    target_arch: &'static str,
}

#[derive(Debug, Serialize)]
struct PredictionSegment {
    text: String,
    latency_ms: f64,
    audio_duration_ms: f64,
    realtime_factor: f64,
    sample_rate_hz: u32,
}

#[cfg(not(feature = "transcription-whisper"))]
fn main() {
    eprintln!(
        "transcription_benchmark_whisper requires --features transcription-whisper or macos-metal"
    );
    std::process::exit(2);
}

#[cfg(feature = "transcription-whisper")]
fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let corpus_text = std::fs::read_to_string(&args.corpus)
        .with_context(|| format!("read corpus {}", args.corpus.display()))?;
    let corpus: Corpus = serde_json::from_str(&corpus_text)
        .with_context(|| format!("parse corpus {}", args.corpus.display()))?;
    let corpus_dir = args
        .corpus
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let threads = args.threads.unwrap_or_else(|| args.mode.default_threads());

    let load_started = Instant::now();
    let context = WhisperContext::new_with_params(
        args.model.to_string_lossy().as_ref(),
        WhisperContextParameters::default(),
    )
    .with_context(|| format!("load Whisper model {}", args.model.display()))?;
    let model_load_ms = load_started.elapsed().as_secs_f64() * 1000.0;

    let mut segments = BTreeMap::new();
    for segment in &corpus.segments {
        let audio_path = corpus_dir.join(&segment.audio);
        let audio = read_wav_as_16khz_f32(&audio_path)
            .with_context(|| format!("read audio for {}", segment.id))?;
        let started = Instant::now();
        let text = transcribe_segment(
            &context,
            &audio.samples,
            args.mode,
            &args.language,
            args.prompt.as_deref(),
            threads,
        )
        .with_context(|| format!("transcribe {}", segment.id))?;
        let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
        segments.insert(
            segment.id.clone(),
            PredictionSegment {
                text,
                latency_ms,
                audio_duration_ms: audio.duration_ms,
                realtime_factor: latency_ms / audio.duration_ms.max(1.0),
                sample_rate_hz: 16_000,
            },
        );
    }

    let predictions = Predictions {
        model_id: args.model_id,
        runtime: "whisper-rs",
        backend: BackendInfo {
            metal: cfg!(feature = "macos-metal"),
            coreml: cfg!(feature = "macos-coreml"),
            target_os: std::env::consts::OS,
            target_arch: std::env::consts::ARCH,
        },
        model_load_ms,
        mode: format!("{:?}", args.mode).to_lowercase(),
        language: args.language,
        threads,
        segments,
    };
    let output = serde_json::to_string_pretty(&predictions)? + "\n";
    if let Some(path) = args.out {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create output directory {}", parent.display()))?;
        }
        std::fs::write(&path, output)
            .with_context(|| format!("write predictions {}", path.display()))?;
    } else {
        print!("{output}");
    }
    Ok(())
}

#[cfg(feature = "transcription-whisper")]
fn transcribe_segment(
    context: &WhisperContext,
    samples_16khz: &[f32],
    mode: BenchmarkMode,
    language: &str,
    prompt: Option<&str>,
    threads: i32,
) -> anyhow::Result<String> {
    let mut state = context.create_state().context("create Whisper state")?;
    let reliable = mode.reliable();
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some(language));
    params.set_translate(false);
    params.set_no_context(!reliable);
    params.set_single_segment(!reliable);
    if reliable {
        if let Some(prompt) = prompt.map(str::trim).filter(|prompt| !prompt.is_empty()) {
            params.set_initial_prompt(prompt);
        }
    }
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_n_threads(threads);
    state.full(params, samples_16khz).context("run Whisper")?;
    Ok(state
        .as_iter()
        .map(|segment| segment.to_string())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string())
}

#[cfg(feature = "transcription-whisper")]
struct AudioData {
    samples: Vec<f32>,
    duration_ms: f64,
}

#[cfg(feature = "transcription-whisper")]
fn read_wav_as_16khz_f32(path: &PathBuf) -> anyhow::Result<AudioData> {
    let mut reader =
        WavReader::open(path).with_context(|| format!("open WAV {}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 1 {
        bail!("expected mono WAV, got {} channels", spec.channels);
    }
    if spec.sample_rate == 0 {
        bail!("invalid WAV sample rate 0");
    }

    let samples = match spec.sample_format {
        SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .context("read float WAV samples")?,
        SampleFormat::Int if spec.bits_per_sample <= 16 => reader
            .samples::<i16>()
            .map(|sample| sample.map(|sample| sample as f32 / i16::MAX as f32))
            .collect::<Result<Vec<_>, _>>()
            .context("read 16-bit WAV samples")?,
        SampleFormat::Int => reader
            .samples::<i32>()
            .map(|sample| sample.map(|sample| sample as f32 / i32::MAX as f32))
            .collect::<Result<Vec<_>, _>>()
            .context("read 32-bit WAV samples")?,
    };
    let duration_ms = samples.len() as f64 / spec.sample_rate as f64 * 1000.0;
    Ok(AudioData {
        samples: resample_linear(&samples, spec.sample_rate, 16_000),
        duration_ms,
    })
}

#[cfg(feature = "transcription-whisper")]
fn resample_linear(input: &[f32], from_hz: u32, to_hz: u32) -> Vec<f32> {
    if from_hz == to_hz || input.is_empty() {
        return input.to_vec();
    }
    let output_len = ((input.len() as f64) * (to_hz as f64) / (from_hz as f64)).round() as usize;
    let mut output = Vec::with_capacity(output_len);
    let ratio = from_hz as f64 / to_hz as f64;
    for index in 0..output_len {
        let position = index as f64 * ratio;
        let left = position.floor() as usize;
        let right = (left + 1).min(input.len() - 1);
        let fraction = (position - left as f64) as f32;
        output.push(input[left] * (1.0 - fraction) + input[right] * fraction);
    }
    output
}
