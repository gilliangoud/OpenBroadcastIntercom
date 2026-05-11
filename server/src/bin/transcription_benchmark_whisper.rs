use std::{collections::BTreeMap, path::PathBuf, time::Instant};

use anyhow::{bail, Context};
use clap::{ArgAction, Parser, ValueEnum};
use hound::{SampleFormat, WavReader};
use serde::{Deserialize, Serialize};

#[cfg(feature = "transcription-whisper")]
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

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
    #[arg(long, default_value = "offline")]
    replay_mode: ReplayMode,
    #[arg(long, default_value = "en")]
    language: String,
    #[arg(long)]
    prompt: Option<String>,
    #[arg(long)]
    threads: Option<i32>,
    #[arg(long, default_value = "fast")]
    partial_mode: BenchmarkMode,
    #[arg(long)]
    partial_threads: Option<i32>,
    #[arg(long, default_value_t = 32)]
    partial_max_tokens: i32,
    #[arg(long, default_value_t = 256)]
    partial_audio_ctx: i32,
    #[arg(long, default_value_t = false, action = ArgAction::Set)]
    partial_prompt: bool,
    #[arg(long, default_value_t = 8000)]
    window_ms: u64,
    #[arg(long, default_value_t = 1000)]
    step_ms: u64,
    #[arg(long, default_value_t = 1500)]
    commit_lag_ms: u64,
    #[arg(long, default_value_t = 2)]
    min_stable_passes: usize,
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    final_pass_on_release: bool,
    #[arg(long, default_value = "utterance")]
    final_pass_scope: FinalPassScope,
    #[arg(long, default_value_t = 24)]
    max_overlap_words: usize,
    #[arg(long, default_value_t = 20)]
    frame_ms: u64,
    #[arg(long, default_value_t = 0.01)]
    vad_rms_threshold: f32,
    #[arg(long, default_value_t = 600)]
    vad_hangover_ms: u64,
    #[arg(long, default_value_t = 120)]
    vad_min_speech_ms: u64,
    #[arg(long, default_value_t = 30_000)]
    stale_job_ms: u64,
    #[arg(long, default_value_t = 8)]
    queue_limit: usize,
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    drop_busy_partials: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BenchmarkMode {
    Fast,
    Balanced,
    Reliable,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ReplayMode {
    Offline,
    RollingBuffer,
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum FinalPassScope {
    Utterance,
    Window,
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
    replay_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rolling_buffer: Option<RollingBufferConfig>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    partials: Option<Vec<PartialTranscript>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    live_metrics: Option<LiveMetrics>,
}

#[derive(Debug, Clone, Serialize)]
struct RollingBufferConfig {
    window_ms: u64,
    step_ms: u64,
    commit_lag_ms: u64,
    min_stable_passes: usize,
    final_pass_on_release: bool,
    final_pass_scope: FinalPassScope,
    max_overlap_words: usize,
    partial_mode: String,
    partial_threads: i32,
    partial_max_tokens: i32,
    partial_audio_ctx: i32,
    partial_prompt: bool,
    state_reuse: bool,
    vad_rms_threshold: f32,
    frame_ms: u64,
    vad_hangover_ms: u64,
    vad_min_speech_ms: u64,
    stale_job_ms: u64,
    queue_limit: usize,
    drop_busy_partials: bool,
}

#[derive(Debug, Clone, Serialize)]
struct PartialTranscript {
    kind: &'static str,
    text: String,
    audio_end_ms: f64,
    emitted_at_ms: f64,
    latency_ms: f64,
    stable_passes: usize,
}

#[derive(Debug, Clone, Serialize)]
struct LiveMetrics {
    mode: &'static str,
    window_ms: u64,
    step_ms: u64,
    commit_lag_ms: u64,
    min_stable_passes: usize,
    final_pass_on_release: bool,
    final_pass_scope: FinalPassScope,
    vad_rms_threshold: f32,
    frame_ms: u64,
    vad_hangover_ms: u64,
    vad_min_speech_ms: u64,
    vad_ranges: Vec<VadRange>,
    vad_frame_count: usize,
    first_token_latency_ms: Option<f64>,
    finalization_latency_ms: Option<f64>,
    final_emitted_at_ms: Option<f64>,
    endpoint_ms: Option<f64>,
    partial_updates: usize,
    hypothesis_updates: usize,
    stale_jobs: u64,
    dropped_jobs: u64,
    stale_job_ms: u64,
    queue_limit: usize,
    drop_busy_partials: bool,
    total_compute_ms: f64,
    average_emission_lag_ms: Option<f64>,
    max_job_queue_delay_ms: f64,
    flicker_ratio: f64,
    audio_duration_ms: f64,
    audio_frames: usize,
}

#[derive(Debug, Clone, Serialize)]
struct VadRange {
    start_ms: f64,
    end_ms: f64,
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
    let partial_threads = args
        .partial_threads
        .unwrap_or_else(|| args.partial_mode.default_threads());
    let rolling_config = RollingBufferConfig {
        window_ms: args.window_ms,
        step_ms: args.step_ms,
        commit_lag_ms: args.commit_lag_ms,
        min_stable_passes: args.min_stable_passes,
        final_pass_on_release: args.final_pass_on_release,
        final_pass_scope: args.final_pass_scope,
        max_overlap_words: args.max_overlap_words,
        partial_mode: format!("{:?}", args.partial_mode).to_lowercase(),
        partial_threads,
        partial_max_tokens: args.partial_max_tokens,
        partial_audio_ctx: args.partial_audio_ctx,
        partial_prompt: args.partial_prompt,
        state_reuse: true,
        vad_rms_threshold: args.vad_rms_threshold,
        frame_ms: args.frame_ms,
        vad_hangover_ms: args.vad_hangover_ms,
        vad_min_speech_ms: args.vad_min_speech_ms,
        stale_job_ms: args.stale_job_ms,
        queue_limit: args.queue_limit,
        drop_busy_partials: args.drop_busy_partials,
    };

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
        let (text, latency_ms, partials, live_metrics) = match args.replay_mode {
            ReplayMode::Offline => {
                let started = Instant::now();
                let mut runner = WhisperRunner::new(&context)?;
                let text = runner
                    .transcribe(
                        &audio.samples,
                        DecodeOptions {
                            mode: args.mode,
                            language: &args.language,
                            prompt: args.prompt.as_deref(),
                            threads,
                            streaming_partial: false,
                            max_tokens: 0,
                            audio_ctx: 0,
                        },
                    )
                    .with_context(|| format!("transcribe {}", segment.id))?;
                (text, started.elapsed().as_secs_f64() * 1000.0, None, None)
            }
            ReplayMode::RollingBuffer => {
                let mut partial_runner = WhisperRunner::new(&context)?;
                let mut final_runner = WhisperRunner::new(&context)?;
                let result = transcribe_rolling_buffer(
                    &mut partial_runner,
                    &mut final_runner,
                    &audio.samples,
                    audio.duration_ms,
                    args.mode,
                    args.partial_mode,
                    &args.language,
                    args.prompt.as_deref(),
                    threads,
                    partial_threads,
                    &rolling_config,
                )
                .with_context(|| format!("rolling replay {}", segment.id))?;
                (
                    result.text,
                    result.latency_ms,
                    Some(result.partials),
                    Some(result.live_metrics),
                )
            }
        };
        segments.insert(
            segment.id.clone(),
            PredictionSegment {
                text,
                latency_ms,
                audio_duration_ms: audio.duration_ms,
                realtime_factor: latency_ms / audio.duration_ms.max(1.0),
                sample_rate_hz: 16_000,
                partials,
                live_metrics,
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
        replay_mode: match args.replay_mode {
            ReplayMode::Offline => "offline".to_string(),
            ReplayMode::RollingBuffer => "rolling-buffer".to_string(),
        },
        rolling_buffer: matches!(args.replay_mode, ReplayMode::RollingBuffer)
            .then_some(rolling_config),
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
struct WhisperRunner {
    state: WhisperState,
}

#[cfg(feature = "transcription-whisper")]
struct DecodeOptions<'a> {
    mode: BenchmarkMode,
    language: &'a str,
    prompt: Option<&'a str>,
    threads: i32,
    streaming_partial: bool,
    max_tokens: i32,
    audio_ctx: i32,
}

#[cfg(feature = "transcription-whisper")]
impl WhisperRunner {
    fn new(context: &WhisperContext) -> anyhow::Result<Self> {
        Ok(Self {
            state: context.create_state().context("create Whisper state")?,
        })
    }

    fn transcribe(
        &mut self,
        samples_16khz: &[f32],
        options: DecodeOptions<'_>,
    ) -> anyhow::Result<String> {
        let reliable = options.mode.reliable();
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(options.language));
        params.set_translate(false);
        params.set_no_context(options.streaming_partial || !reliable);
        params.set_single_segment(options.streaming_partial || !reliable);
        params.set_no_timestamps(options.streaming_partial);
        if options.streaming_partial {
            if options.max_tokens > 0 {
                params.set_max_tokens(options.max_tokens);
            }
            if options.audio_ctx > 0 {
                params.set_audio_ctx(options.audio_ctx);
            }
        } else if reliable {
            if let Some(prompt) = options
                .prompt
                .map(str::trim)
                .filter(|prompt| !prompt.is_empty())
            {
                params.set_initial_prompt(prompt);
            }
        }
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_n_threads(options.threads);
        self.state
            .full(params, samples_16khz)
            .context("run Whisper")?;
        Ok(self
            .state
            .as_iter()
            .map(|segment| segment.to_string())
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string())
    }
}

#[cfg(feature = "transcription-whisper")]
struct RollingResult {
    text: String,
    latency_ms: f64,
    partials: Vec<PartialTranscript>,
    live_metrics: LiveMetrics,
}

#[cfg(feature = "transcription-whisper")]
#[derive(Debug, Clone, Copy)]
struct SampleRange {
    start: usize,
    end: usize,
}

#[cfg(feature = "transcription-whisper")]
#[derive(Default)]
struct ReplayRuntime {
    worker_available_ms: f64,
    pending_finishes: Vec<f64>,
    total_latency_ms: f64,
    emission_lags: Vec<f64>,
    max_job_queue_delay_ms: f64,
    stale_jobs: u64,
    dropped_jobs: u64,
}

#[cfg(feature = "transcription-whisper")]
struct WindowOutput {
    text: String,
    latency_ms: f64,
    emitted_at_ms: f64,
}

#[cfg(feature = "transcription-whisper")]
fn transcribe_rolling_buffer(
    partial_runner: &mut WhisperRunner,
    final_runner: &mut WhisperRunner,
    samples_16khz: &[f32],
    duration_ms: f64,
    final_mode: BenchmarkMode,
    partial_mode: BenchmarkMode,
    language: &str,
    prompt: Option<&str>,
    final_threads: i32,
    partial_threads: i32,
    config: &RollingBufferConfig,
) -> anyhow::Result<RollingResult> {
    validate_rolling_config(config)?;
    let (voiced_ranges, vad_frame_count) = detect_voiced_ranges(samples_16khz, config);
    if voiced_ranges.is_empty() {
        return Ok(RollingResult {
            text: String::new(),
            latency_ms: 0.0,
            partials: Vec::new(),
            live_metrics: LiveMetrics {
                mode: "rolling-buffer",
                window_ms: config.window_ms,
                step_ms: config.step_ms,
                commit_lag_ms: config.commit_lag_ms,
                min_stable_passes: config.min_stable_passes,
                final_pass_on_release: config.final_pass_on_release,
                final_pass_scope: config.final_pass_scope,
                vad_rms_threshold: config.vad_rms_threshold,
                frame_ms: config.frame_ms,
                vad_hangover_ms: config.vad_hangover_ms,
                vad_min_speech_ms: config.vad_min_speech_ms,
                vad_ranges: Vec::new(),
                vad_frame_count,
                first_token_latency_ms: None,
                finalization_latency_ms: None,
                final_emitted_at_ms: None,
                endpoint_ms: None,
                partial_updates: 0,
                hypothesis_updates: 0,
                stale_jobs: 0,
                dropped_jobs: 0,
                stale_job_ms: config.stale_job_ms,
                queue_limit: config.queue_limit,
                drop_busy_partials: config.drop_busy_partials,
                total_compute_ms: 0.0,
                average_emission_lag_ms: None,
                max_job_queue_delay_ms: 0.0,
                flicker_ratio: 0.0,
                audio_duration_ms: duration_ms,
                audio_frames: samples_16khz.len(),
            },
        });
    }

    let mut runtime = ReplayRuntime::default();
    let mut partials = Vec::new();
    let mut final_pieces: Vec<String> = Vec::new();
    let mut hypothesis_history: Vec<String> = Vec::new();
    let mut provisional_text = String::new();
    let mut committed_text = String::new();
    let mut first_token_latency_ms = None;
    let mut finalization_latency_ms = None;
    let mut flicker_revisions = 0usize;
    let mut emitted_words = 0usize;

    for (range_index, range) in voiced_ranges.iter().copied().enumerate() {
        let range_start_ms = samples_to_ms(range.start);
        let range_end_ms = samples_to_ms(range.end);
        let mut next_end_ms = range_start_ms + config.step_ms as f64;
        let mut local_history: Vec<String> = Vec::new();

        while next_end_ms < range_end_ms {
            let window_start_ms = range_start_ms.max(next_end_ms - config.window_ms as f64);
            let window_start = ms_to_sample(window_start_ms).min(samples_16khz.len());
            let window_end = ms_to_sample(next_end_ms).min(samples_16khz.len());
            if let Some(output) = process_rolling_window(
                partial_runner,
                samples_16khz,
                partial_mode,
                language,
                config.partial_prompt.then_some(prompt).flatten(),
                partial_threads,
                true,
                config.partial_max_tokens,
                config.partial_audio_ctx,
                config,
                &mut runtime,
                "partial",
                range_index,
                next_end_ms,
                window_start,
                window_end,
            )? {
                let previous_provisional = provisional_text.clone();
                if window_start <= range.start {
                    provisional_text = output.text.trim().to_string();
                } else {
                    provisional_text = stitch_by_word_overlap(
                        if committed_text.is_empty() {
                            &provisional_text
                        } else {
                            &committed_text
                        },
                        &output.text,
                        config.max_overlap_words,
                    );
                }
                if !previous_provisional.is_empty() {
                    let stable_prefix_words =
                        common_prefix_word_count(&previous_provisional, &provisional_text);
                    flicker_revisions += normalized_words(&previous_provisional)
                        .len()
                        .saturating_sub(stable_prefix_words);
                }
                emitted_words += normalized_words(&provisional_text).len();
                local_history.push(provisional_text.clone());
                hypothesis_history.push(provisional_text.clone());
                if local_history.len() >= config.min_stable_passes {
                    let start = local_history.len() - config.min_stable_passes;
                    let stable_text = common_prefix_text(&local_history[start..]);
                    let stable_text = trim_commit_lag(&stable_text, config.commit_lag_ms);
                    if normalized_words(&stable_text).len()
                        > normalized_words(&committed_text).len()
                    {
                        committed_text = stable_text;
                        partials.push(PartialTranscript {
                            kind: "partial",
                            text: committed_text.clone(),
                            audio_end_ms: next_end_ms,
                            emitted_at_ms: output.emitted_at_ms,
                            latency_ms: output.latency_ms,
                            stable_passes: config.min_stable_passes,
                        });
                        if first_token_latency_ms.is_none()
                            && !normalized_words(&committed_text).is_empty()
                        {
                            first_token_latency_ms = Some(output.emitted_at_ms - range_start_ms);
                        }
                    }
                }
            }
            next_end_ms += config.step_ms as f64;
        }

        if config.final_pass_on_release {
            let final_start = match config.final_pass_scope {
                FinalPassScope::Utterance => range.start,
                FinalPassScope::Window => {
                    let final_start_ms = range_start_ms.max(range_end_ms - config.window_ms as f64);
                    ms_to_sample(final_start_ms).min(samples_16khz.len())
                }
            };
            if let Some(output) = process_rolling_window(
                final_runner,
                samples_16khz,
                final_mode,
                language,
                prompt,
                final_threads,
                false,
                0,
                0,
                config,
                &mut runtime,
                "final",
                range_index,
                range_end_ms,
                final_start,
                range.end,
            )? {
                let previous = final_pieces.join(" ");
                let final_text_so_far =
                    stitch_by_word_overlap(&previous, &output.text, config.max_overlap_words);
                final_pieces.push(output.text.clone());
                partials.push(PartialTranscript {
                    kind: "final",
                    text: final_text_so_far,
                    audio_end_ms: range_end_ms,
                    emitted_at_ms: output.emitted_at_ms,
                    latency_ms: output.latency_ms,
                    stable_passes: config.min_stable_passes,
                });
                if first_token_latency_ms.is_none() && !normalized_words(&output.text).is_empty() {
                    first_token_latency_ms = Some(output.emitted_at_ms - range_start_ms);
                }
                finalization_latency_ms = Some(output.emitted_at_ms - range_end_ms);
            }
        } else {
            final_pieces.push(if committed_text.is_empty() {
                provisional_text.clone()
            } else {
                committed_text.clone()
            });
        }
    }

    let mut final_text = String::new();
    if config.final_pass_on_release {
        for piece in &final_pieces {
            final_text = stitch_by_word_overlap(&final_text, piece, config.max_overlap_words);
        }
    } else {
        final_text = if committed_text.is_empty() {
            provisional_text
        } else {
            committed_text
        };
    }

    let vad_ranges = voiced_ranges
        .iter()
        .map(|range| VadRange {
            start_ms: samples_to_ms(range.start),
            end_ms: samples_to_ms(range.end),
        })
        .collect::<Vec<_>>();
    let final_emitted_at_ms = partials
        .last()
        .filter(|partial| partial.kind == "final")
        .map(|partial| partial.emitted_at_ms);
    let partial_updates = partials
        .iter()
        .filter(|partial| partial.kind == "partial")
        .count();
    let average_emission_lag_ms = average(&runtime.emission_lags);
    let flicker_ratio = if emitted_words == 0 {
        0.0
    } else {
        flicker_revisions as f64 / emitted_words as f64
    };

    Ok(RollingResult {
        text: final_text.trim().to_string(),
        latency_ms: runtime.total_latency_ms,
        partials,
        live_metrics: LiveMetrics {
            mode: "rolling-buffer",
            window_ms: config.window_ms,
            step_ms: config.step_ms,
            commit_lag_ms: config.commit_lag_ms,
            min_stable_passes: config.min_stable_passes,
            final_pass_on_release: config.final_pass_on_release,
            final_pass_scope: config.final_pass_scope,
            vad_rms_threshold: config.vad_rms_threshold,
            frame_ms: config.frame_ms,
            vad_hangover_ms: config.vad_hangover_ms,
            vad_min_speech_ms: config.vad_min_speech_ms,
            vad_ranges,
            vad_frame_count,
            first_token_latency_ms,
            finalization_latency_ms,
            final_emitted_at_ms,
            endpoint_ms: voiced_ranges.last().map(|range| samples_to_ms(range.end)),
            partial_updates,
            hypothesis_updates: hypothesis_history.len(),
            stale_jobs: runtime.stale_jobs,
            dropped_jobs: runtime.dropped_jobs,
            stale_job_ms: config.stale_job_ms,
            queue_limit: config.queue_limit,
            drop_busy_partials: config.drop_busy_partials,
            total_compute_ms: runtime.total_latency_ms,
            average_emission_lag_ms,
            max_job_queue_delay_ms: runtime.max_job_queue_delay_ms,
            flicker_ratio,
            audio_duration_ms: duration_ms,
            audio_frames: samples_16khz.len(),
        },
    })
}

#[cfg(feature = "transcription-whisper")]
#[allow(clippy::too_many_arguments)]
fn process_rolling_window(
    runner: &mut WhisperRunner,
    samples_16khz: &[f32],
    mode: BenchmarkMode,
    language: &str,
    prompt: Option<&str>,
    threads: i32,
    streaming_partial: bool,
    max_tokens: i32,
    audio_ctx: i32,
    config: &RollingBufferConfig,
    runtime: &mut ReplayRuntime,
    kind: &'static str,
    _range_index: usize,
    scheduled_at_ms: f64,
    start: usize,
    end: usize,
) -> anyhow::Result<Option<WindowOutput>> {
    runtime
        .pending_finishes
        .retain(|finish| *finish > scheduled_at_ms);
    if kind == "partial" && runtime.pending_finishes.len() >= config.queue_limit {
        runtime.dropped_jobs += 1;
        return Ok(None);
    }

    let process_started_ms = scheduled_at_ms.max(runtime.worker_available_ms);
    let queue_delay_ms = process_started_ms - scheduled_at_ms;
    runtime.max_job_queue_delay_ms = runtime.max_job_queue_delay_ms.max(queue_delay_ms);
    if kind == "partial"
        && config.drop_busy_partials
        && process_started_ms > scheduled_at_ms + config.step_ms as f64
    {
        runtime.dropped_jobs += 1;
        return Ok(None);
    }
    if kind == "partial" && queue_delay_ms > config.stale_job_ms as f64 {
        runtime.stale_jobs += 1;
        return Ok(None);
    }

    if start >= end || start >= samples_16khz.len() {
        return Ok(None);
    }
    let end = end.min(samples_16khz.len());
    let started = Instant::now();
    let text = runner.transcribe(
        &samples_16khz[start..end],
        DecodeOptions {
            mode,
            language,
            prompt,
            threads,
            streaming_partial,
            max_tokens,
            audio_ctx,
        },
    )?;
    let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
    runtime.total_latency_ms += latency_ms;
    let emitted_at_ms = process_started_ms + latency_ms;
    runtime.worker_available_ms = emitted_at_ms;
    runtime.pending_finishes.push(emitted_at_ms);
    runtime.emission_lags.push(emitted_at_ms - scheduled_at_ms);
    Ok(Some(WindowOutput {
        text,
        latency_ms,
        emitted_at_ms,
    }))
}

#[cfg(feature = "transcription-whisper")]
fn validate_rolling_config(config: &RollingBufferConfig) -> anyhow::Result<()> {
    if config.window_ms == 0 {
        bail!("window-ms must be positive");
    }
    if config.step_ms == 0 {
        bail!("step-ms must be positive");
    }
    if config.min_stable_passes == 0 {
        bail!("min-stable-passes must be positive");
    }
    if config.frame_ms == 0 {
        bail!("frame-ms must be positive");
    }
    if config.queue_limit == 0 {
        bail!("queue-limit must be positive");
    }
    if config.partial_max_tokens < 0 {
        bail!("partial-max-tokens must be zero or positive");
    }
    if config.partial_audio_ctx < 0 {
        bail!("partial-audio-ctx must be zero or positive");
    }
    Ok(())
}

#[cfg(feature = "transcription-whisper")]
fn detect_voiced_ranges(
    samples_16khz: &[f32],
    config: &RollingBufferConfig,
) -> (Vec<SampleRange>, usize) {
    let frame_samples = ((16_000.0 * config.frame_ms as f64 / 1000.0).round() as usize).max(1);
    let hangover_frames =
        ((config.vad_hangover_ms as f64 / config.frame_ms as f64).ceil() as usize).max(1);
    let min_speech_frames =
        ((config.vad_min_speech_ms as f64 / config.frame_ms as f64).ceil() as usize).max(1);
    let mut ranges = Vec::new();
    let mut active_start = None;
    let mut voiced_frames = 0usize;
    let mut silence_frames = 0usize;
    let mut frame_index = 0usize;

    while frame_index * frame_samples < samples_16khz.len() {
        let start = frame_index * frame_samples;
        let end = (start + frame_samples).min(samples_16khz.len());
        let rms = frame_rms(&samples_16khz[start..end]);
        let voiced = rms >= config.vad_rms_threshold;
        if active_start.is_none() && voiced {
            active_start = Some(start);
            voiced_frames = 0;
            silence_frames = 0;
        }
        if active_start.is_some() {
            if voiced {
                voiced_frames += 1;
                silence_frames = 0;
            } else {
                silence_frames += 1;
            }
            if silence_frames >= hangover_frames && voiced_frames >= min_speech_frames {
                ranges.push(SampleRange {
                    start: active_start.unwrap_or(start),
                    end,
                });
                active_start = None;
                voiced_frames = 0;
                silence_frames = 0;
            } else if silence_frames >= hangover_frames {
                active_start = None;
                voiced_frames = 0;
                silence_frames = 0;
            }
        }
        frame_index += 1;
    }
    if let Some(start) = active_start {
        if voiced_frames >= min_speech_frames {
            ranges.push(SampleRange {
                start,
                end: samples_16khz.len(),
            });
        }
    }
    (ranges, frame_index)
}

#[cfg(feature = "transcription-whisper")]
fn frame_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let square_sum = samples.iter().map(|sample| sample * sample).sum::<f32>();
    (square_sum / samples.len() as f32).sqrt()
}

#[cfg(feature = "transcription-whisper")]
fn samples_to_ms(samples: usize) -> f64 {
    samples as f64 / 16_000.0 * 1000.0
}

#[cfg(feature = "transcription-whisper")]
fn ms_to_sample(ms: f64) -> usize {
    ((ms / 1000.0) * 16_000.0).round() as usize
}

#[cfg(feature = "transcription-whisper")]
fn normalized_words(text: &str) -> Vec<String> {
    let mut normalized = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
        } else {
            normalized.push(' ');
        }
    }
    normalized.split_whitespace().map(str::to_string).collect()
}

#[cfg(feature = "transcription-whisper")]
fn common_prefix_word_count(left: &str, right: &str) -> usize {
    let left_words = normalized_words(left);
    let right_words = normalized_words(right);
    left_words
        .iter()
        .zip(right_words.iter())
        .take_while(|(left, right)| left == right)
        .count()
}

#[cfg(feature = "transcription-whisper")]
fn common_prefix_text(texts: &[String]) -> String {
    if texts.is_empty() {
        return String::new();
    }
    let raw_words = texts.last().unwrap().split_whitespace().collect::<Vec<_>>();
    let normalized = texts
        .iter()
        .map(|text| normalized_words(text))
        .collect::<Vec<_>>();
    let mut prefix_len = normalized.iter().map(Vec::len).min().unwrap_or(0);
    for index in 0..prefix_len {
        let word = &normalized[0][index];
        if normalized.iter().any(|words| &words[index] != word) {
            prefix_len = index;
            break;
        }
    }
    raw_words
        .into_iter()
        .take(prefix_len)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(feature = "transcription-whisper")]
fn trim_commit_lag(text: &str, commit_lag_ms: u64) -> String {
    let words = text.split_whitespace().collect::<Vec<_>>();
    if commit_lag_ms == 0 || words.is_empty() {
        return text.trim().to_string();
    }
    let lag_words = ((commit_lag_ms as f64 / 1000.0) * 2.5).ceil().max(1.0) as usize;
    if lag_words >= words.len() {
        return String::new();
    }
    words[..words.len() - lag_words].join(" ")
}

#[cfg(feature = "transcription-whisper")]
fn stitch_by_word_overlap(existing: &str, update: &str, max_overlap_words: usize) -> String {
    let existing = existing.trim();
    let update = update.trim();
    if existing.is_empty() {
        return update.to_string();
    }
    if update.is_empty() {
        return existing.to_string();
    }
    let existing_raw = existing.split_whitespace().collect::<Vec<_>>();
    let update_raw = update.split_whitespace().collect::<Vec<_>>();
    let existing_norm = normalized_words(existing);
    let update_norm = normalized_words(update);
    let max_overlap = max_overlap_words
        .min(existing_norm.len())
        .min(update_norm.len());
    for overlap in (1..=max_overlap).rev() {
        if existing_norm[existing_norm.len() - overlap..] == update_norm[..overlap] {
            return existing_raw
                .iter()
                .copied()
                .chain(update_raw.iter().skip(overlap).copied())
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    format!("{existing} {update}").trim().to_string()
}

#[cfg(feature = "transcription-whisper")]
fn average(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
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
