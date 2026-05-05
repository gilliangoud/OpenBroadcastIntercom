#![cfg_attr(test, allow(dead_code))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};
use common::{resample_linear, MIX_SAMPLES_PER_FRAME, MIX_SAMPLE_RATE};
use ndarray17::{Array, Array3};
use ort::session::Session;
use ort::value::Tensor;
use rand_distr::{Distribution, Normal};
use regex::Regex;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

pub const ENGINE_NAME: &str = "supertonic";

const DEFAULT_LANG: &str = "en";
const DEFAULT_TOTAL_STEPS: usize = 5;
const DEFAULT_SPEED: f32 = 1.05;
const DEFAULT_SILENCE_SECONDS: f32 = 0.3;
const MAX_CHUNK_LENGTH: usize = 300;
const AVAILABLE_LANGS: &[&str] = &["en", "ko", "es", "pt", "fr"];

const DURATION_PREDICTOR_ONNX: &[u8] =
    include_bytes!("../assets/supertonic/onnx/duration_predictor.onnx");
const TEXT_ENCODER_ONNX: &[u8] = include_bytes!("../assets/supertonic/onnx/text_encoder.onnx");
const VECTOR_ESTIMATOR_ONNX: &[u8] =
    include_bytes!("../assets/supertonic/onnx/vector_estimator.onnx");
const VOCODER_ONNX: &[u8] = include_bytes!("../assets/supertonic/onnx/vocoder.onnx");
const TTS_JSON: &[u8] = include_bytes!("../assets/supertonic/onnx/tts.json");
const UNICODE_INDEXER_JSON: &[u8] =
    include_bytes!("../assets/supertonic/onnx/unicode_indexer.json");
const DEFAULT_VOICE_STYLE_JSON: &[u8] = include_bytes!("../assets/supertonic/voice_styles/M1.json");

static TTS_ENGINE: OnceLock<Mutex<Option<TextToSpeech>>> = OnceLock::new();
static VOICE_STYLE: OnceLock<Mutex<Option<Style>>> = OnceLock::new();

pub async fn synthesize(message: &str, gain: f32) -> Result<Vec<Vec<i16>>> {
    let message = message.to_string();
    tokio::task::spawn_blocking(move || synthesize_blocking(&message, gain))
        .await
        .context("join supertonic synthesis task")?
}

#[cfg(test)]
fn embedded_asset_summary() -> Vec<(&'static str, usize)> {
    vec![
        (
            "onnx/duration_predictor.onnx",
            DURATION_PREDICTOR_ONNX.len(),
        ),
        ("onnx/text_encoder.onnx", TEXT_ENCODER_ONNX.len()),
        ("onnx/vector_estimator.onnx", VECTOR_ESTIMATOR_ONNX.len()),
        ("onnx/vocoder.onnx", VOCODER_ONNX.len()),
        ("onnx/tts.json", TTS_JSON.len()),
        ("onnx/unicode_indexer.json", UNICODE_INDEXER_JSON.len()),
        ("voice_styles/M1.json", DEFAULT_VOICE_STYLE_JSON.len()),
    ]
}

fn synthesize_blocking(message: &str, gain: f32) -> Result<Vec<Vec<i16>>> {
    let style = cached_voice_style()?;
    with_engine(|engine| {
        let (samples, _duration_seconds) = engine.call(
            message,
            DEFAULT_LANG,
            &style,
            DEFAULT_TOTAL_STEPS,
            DEFAULT_SPEED,
            DEFAULT_SILENCE_SECONDS,
        )?;
        let samples = normalize_and_convert_samples(&samples, engine.sample_rate as u32, gain);
        Ok(samples_to_frames(samples))
    })
}

fn cached_voice_style() -> Result<Style> {
    let cache = VOICE_STYLE.get_or_init(|| Mutex::new(None));
    let mut cache = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("Supertonic voice style mutex was poisoned"))?;
    if let Some(style) = cache.as_ref() {
        return Ok(style.clone());
    }
    let style = load_voice_style_from_bytes(DEFAULT_VOICE_STYLE_JSON)
        .context("load embedded Supertonic voice style")?;
    *cache = Some(style.clone());
    Ok(style)
}

fn with_engine<T>(run: impl FnOnce(&mut TextToSpeech) -> Result<T>) -> Result<T> {
    let cache = TTS_ENGINE.get_or_init(|| Mutex::new(None));
    let mut cache = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("Supertonic engine mutex was poisoned"))?;
    if cache.is_none() {
        let asset_root = ensure_embedded_assets()?;
        let onnx_dir = asset_root.join("onnx");
        *cache = Some(load_text_to_speech(&onnx_dir)?);
    }
    let engine = cache
        .as_mut()
        .context("Supertonic engine cache was unexpectedly empty")?;
    run(engine)
}

fn ensure_embedded_assets() -> Result<PathBuf> {
    let root = std::env::temp_dir().join(format!(
        "intercom-supertonic-{}-{}-{}-{}",
        DURATION_PREDICTOR_ONNX.len(),
        TEXT_ENCODER_ONNX.len(),
        VECTOR_ESTIMATOR_ONNX.len(),
        VOCODER_ONNX.len()
    ));
    let onnx_dir = root.join("onnx");
    let voice_dir = root.join("voice_styles");
    fs::create_dir_all(&onnx_dir).context("create embedded Supertonic ONNX asset directory")?;
    fs::create_dir_all(&voice_dir).context("create embedded Supertonic voice asset directory")?;
    write_asset_if_needed(
        &onnx_dir.join("duration_predictor.onnx"),
        DURATION_PREDICTOR_ONNX,
    )?;
    write_asset_if_needed(&onnx_dir.join("text_encoder.onnx"), TEXT_ENCODER_ONNX)?;
    write_asset_if_needed(
        &onnx_dir.join("vector_estimator.onnx"),
        VECTOR_ESTIMATOR_ONNX,
    )?;
    write_asset_if_needed(&onnx_dir.join("vocoder.onnx"), VOCODER_ONNX)?;
    write_asset_if_needed(&onnx_dir.join("tts.json"), TTS_JSON)?;
    write_asset_if_needed(&onnx_dir.join("unicode_indexer.json"), UNICODE_INDEXER_JSON)?;
    write_asset_if_needed(&voice_dir.join("M1.json"), DEFAULT_VOICE_STYLE_JSON)?;
    Ok(root)
}

fn write_asset_if_needed(path: &Path, bytes: &[u8]) -> Result<()> {
    if path
        .metadata()
        .is_ok_and(|metadata| metadata.len() == bytes.len() as u64)
    {
        return Ok(());
    }
    fs::write(path, bytes).with_context(|| format!("write embedded asset {}", path.display()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    ae: AEConfig,
    ttl: TtlConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AEConfig {
    sample_rate: i32,
    base_chunk_size: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TtlConfig {
    chunk_compress_factor: i32,
    latent_dim: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VoiceStyleData {
    style_ttl: StyleComponent,
    style_dp: StyleComponent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StyleComponent {
    data: Vec<Vec<Vec<f32>>>,
    dims: Vec<usize>,
    #[serde(rename = "type")]
    _dtype: String,
}

#[derive(Clone)]
struct Style {
    ttl: Array3<f32>,
    dp: Array3<f32>,
}

struct UnicodeProcessor {
    indexer: Vec<i64>,
}

impl UnicodeProcessor {
    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let indexer = serde_json::from_slice(bytes).context("parse Supertonic unicode indexer")?;
        Ok(Self { indexer })
    }

    fn call(
        &self,
        text_list: &[String],
        lang_list: &[String],
    ) -> Result<(Vec<Vec<i64>>, Array3<f32>)> {
        let mut processed_texts = Vec::with_capacity(text_list.len());
        for (text, lang) in text_list.iter().zip(lang_list.iter()) {
            processed_texts.push(preprocess_text(text, lang)?);
        }

        let text_ids_lengths = processed_texts
            .iter()
            .map(|text| text.chars().count())
            .collect::<Vec<_>>();
        let max_len = *text_ids_lengths.iter().max().unwrap_or(&0);

        let mut text_ids = Vec::with_capacity(processed_texts.len());
        for text in &processed_texts {
            let mut row = vec![0i64; max_len];
            for (index, unicode_value) in text_to_unicode_values(text).into_iter().enumerate() {
                row[index] = self.indexer.get(unicode_value).copied().unwrap_or(-1);
            }
            text_ids.push(row);
        }

        let text_mask = get_text_mask(&text_ids_lengths);
        Ok((text_ids, text_mask))
    }
}

struct TextToSpeech {
    cfgs: Config,
    text_processor: UnicodeProcessor,
    dp_ort: Session,
    text_enc_ort: Session,
    vector_est_ort: Session,
    vocoder_ort: Session,
    sample_rate: i32,
}

impl TextToSpeech {
    fn new(
        cfgs: Config,
        text_processor: UnicodeProcessor,
        dp_ort: Session,
        text_enc_ort: Session,
        vector_est_ort: Session,
        vocoder_ort: Session,
    ) -> Self {
        let sample_rate = cfgs.ae.sample_rate;
        Self {
            cfgs,
            text_processor,
            dp_ort,
            text_enc_ort,
            vector_est_ort,
            vocoder_ort,
            sample_rate,
        }
    }

    fn infer(
        &mut self,
        text_list: &[String],
        lang_list: &[String],
        style: &Style,
        total_step: usize,
        speed: f32,
    ) -> Result<(Vec<f32>, Vec<f32>)> {
        let batch_size = text_list.len();
        let (text_ids, text_mask) = self.text_processor.call(text_list, lang_list)?;
        let text_ids_width = text_ids
            .first()
            .map(|row| row.len())
            .context("Supertonic received no text rows")?;

        let text_ids_array = {
            let mut flat = Vec::with_capacity(batch_size * text_ids_width);
            for row in &text_ids {
                flat.extend_from_slice(row);
            }
            Array::from_shape_vec((batch_size, text_ids_width), flat)?
        };

        let text_ids_value = Tensor::from_array(text_ids_array)?;
        let text_mask_value = Tensor::from_array(text_mask.clone())?;
        let style_dp_value = Tensor::from_array(style.dp.clone())?;

        let dp_outputs = self.dp_ort.run(ort::inputs! {
            "text_ids" => &text_ids_value,
            "style_dp" => &style_dp_value,
            "text_mask" => &text_mask_value,
        })?;
        let (_, duration_data) = dp_outputs["duration"].try_extract_tensor::<f32>()?;
        let mut duration = duration_data.to_vec();
        for item in &mut duration {
            *item /= speed;
        }

        let style_ttl_value = Tensor::from_array(style.ttl.clone())?;
        let text_enc_outputs = self.text_enc_ort.run(ort::inputs! {
            "text_ids" => &text_ids_value,
            "style_ttl" => &style_ttl_value,
            "text_mask" => &text_mask_value,
        })?;
        let (text_emb_shape, text_emb_data) =
            text_enc_outputs["text_emb"].try_extract_tensor::<f32>()?;
        let text_emb = shape3_from_tensor("text_emb", text_emb_shape, text_emb_data)?;

        let (mut xt, latent_mask) = sample_noisy_latent(
            &duration,
            self.sample_rate,
            self.cfgs.ae.base_chunk_size,
            self.cfgs.ttl.chunk_compress_factor,
            self.cfgs.ttl.latent_dim,
        );

        let total_step_array = Array::from_elem(batch_size, total_step as f32);
        for step in 0..total_step {
            let current_step_array = Array::from_elem(batch_size, step as f32);

            let xt_value = Tensor::from_array(xt.clone())?;
            let text_emb_value = Tensor::from_array(text_emb.clone())?;
            let latent_mask_value = Tensor::from_array(latent_mask.clone())?;
            let text_mask_value = Tensor::from_array(text_mask.clone())?;
            let current_step_value = Tensor::from_array(current_step_array)?;
            let total_step_value = Tensor::from_array(total_step_array.clone())?;

            let vector_est_outputs = self.vector_est_ort.run(ort::inputs! {
                "noisy_latent" => &xt_value,
                "text_emb" => &text_emb_value,
                "style_ttl" => &style_ttl_value,
                "latent_mask" => &latent_mask_value,
                "text_mask" => &text_mask_value,
                "current_step" => &current_step_value,
                "total_step" => &total_step_value,
            })?;
            let (denoised_shape, denoised_data) =
                vector_est_outputs["denoised_latent"].try_extract_tensor::<f32>()?;
            xt = shape3_from_tensor("denoised_latent", denoised_shape, denoised_data)?;
        }

        let final_latent_value = Tensor::from_array(xt)?;
        let vocoder_outputs = self.vocoder_ort.run(ort::inputs! {
            "latent" => &final_latent_value,
        })?;
        let (_, wav_data) = vocoder_outputs["wav_tts"].try_extract_tensor::<f32>()?;
        Ok((wav_data.to_vec(), duration))
    }

    fn call(
        &mut self,
        text: &str,
        lang: &str,
        style: &Style,
        total_step: usize,
        speed: f32,
        silence_duration: f32,
    ) -> Result<(Vec<f32>, f32)> {
        let max_len = if lang == "ko" { 120 } else { MAX_CHUNK_LENGTH };
        let chunks = chunk_text(text, Some(max_len));
        let mut wav_cat = Vec::new();
        let mut duration_cat = 0.0;

        for (index, chunk) in chunks.iter().enumerate() {
            let lang = lang.to_string();
            let (wav, duration) = self.infer(
                std::slice::from_ref(chunk),
                std::slice::from_ref(&lang),
                style,
                total_step,
                speed,
            )?;
            let duration = duration[0];
            let wav_chunk = duration_limited_wav_chunk(&wav, self.sample_rate, duration);

            if index > 0 {
                let silence_len = (silence_duration * self.sample_rate as f32) as usize;
                wav_cat.extend(std::iter::repeat_n(0.0, silence_len));
                duration_cat += silence_duration;
            }
            wav_cat.extend_from_slice(wav_chunk);
            duration_cat += duration;
            tracing::debug!(
                chunk,
                predicted_duration_seconds = duration,
                output_duration_seconds = wav_chunk.len() as f32 / self.sample_rate as f32,
                "generated Supertonic chunk"
            );
        }

        Ok((wav_cat, duration_cat))
    }
}

fn load_text_to_speech(onnx_dir: &Path) -> Result<TextToSpeech> {
    let cfgs: Config =
        serde_json::from_slice(TTS_JSON).context("parse embedded Supertonic config")?;
    let dp_ort = Session::builder()?.commit_from_file(onnx_dir.join("duration_predictor.onnx"))?;
    let text_enc_ort = Session::builder()?.commit_from_file(onnx_dir.join("text_encoder.onnx"))?;
    let vector_est_ort =
        Session::builder()?.commit_from_file(onnx_dir.join("vector_estimator.onnx"))?;
    let vocoder_ort = Session::builder()?.commit_from_file(onnx_dir.join("vocoder.onnx"))?;
    let text_processor = UnicodeProcessor::from_bytes(UNICODE_INDEXER_JSON)?;

    Ok(TextToSpeech::new(
        cfgs,
        text_processor,
        dp_ort,
        text_enc_ort,
        vector_est_ort,
        vocoder_ort,
    ))
}

fn load_voice_style_from_bytes(bytes: &[u8]) -> Result<Style> {
    let data: VoiceStyleData = serde_json::from_slice(bytes).context("parse voice style JSON")?;
    let ttl = style_component_to_array3("style_ttl", &data.style_ttl)?;
    let dp = style_component_to_array3("style_dp", &data.style_dp)?;
    Ok(Style { ttl, dp })
}

fn style_component_to_array3(name: &str, component: &StyleComponent) -> Result<Array3<f32>> {
    if component.dims.len() != 3 {
        bail!("{name} must have three dimensions");
    }
    let shape = (component.dims[0], component.dims[1], component.dims[2]);
    let mut flat = Vec::with_capacity(shape.0 * shape.1 * shape.2);
    for batch in &component.data {
        for row in batch {
            flat.extend_from_slice(row);
        }
    }
    if flat.len() != shape.0 * shape.1 * shape.2 {
        bail!(
            "{name} data length {} does not match declared shape {:?}",
            flat.len(),
            shape
        );
    }
    Ok(Array3::from_shape_vec(shape, flat)?)
}

fn shape3_from_tensor(name: &str, shape: &[i64], data: &[f32]) -> Result<Array3<f32>> {
    if shape.len() != 3 {
        bail!("{name} expected rank 3 output, got shape {shape:?}");
    }
    let shape = (shape[0] as usize, shape[1] as usize, shape[2] as usize);
    Ok(Array3::from_shape_vec(shape, data.to_vec())?)
}

fn duration_limited_wav_chunk(wav: &[f32], sample_rate: i32, duration: f32) -> &[f32] {
    let wav_len = (sample_rate as f32 * duration) as usize;
    &wav[..wav_len.min(wav.len())]
}

fn preprocess_text(text: &str, lang: &str) -> Result<String> {
    let mut text: String = text.nfkd().collect();
    let emoji_pattern = Regex::new(
        r"[\x{1F600}-\x{1F64F}\x{1F300}-\x{1F5FF}\x{1F680}-\x{1F6FF}\x{1F700}-\x{1F77F}\x{1F780}-\x{1F7FF}\x{1F800}-\x{1F8FF}\x{1F900}-\x{1F9FF}\x{1FA00}-\x{1FA6F}\x{1FA70}-\x{1FAFF}\x{2600}-\x{26FF}\x{2700}-\x{27BF}\x{1F1E6}-\x{1F1FF}]+",
    )?;
    text = emoji_pattern.replace_all(&text, "").to_string();

    for (from, to) in [
        ("–", "-"),
        ("‑", "-"),
        ("—", "-"),
        ("_", " "),
        ("\u{201C}", "\""),
        ("\u{201D}", "\""),
        ("\u{2018}", "'"),
        ("\u{2019}", "'"),
        ("´", "'"),
        ("`", "'"),
        ("[", " "),
        ("]", " "),
        ("|", " "),
        ("/", " "),
        ("#", " "),
        ("→", " "),
        ("←", " "),
    ] {
        text = text.replace(from, to);
    }

    for symbol in ["\u{2665}", "\u{2606}", "\u{2661}", "\u{00A9}", "\\"] {
        text = text.replace(symbol, "");
    }

    for (from, to) in [
        ("@", " at "),
        ("e.g.,", "for example, "),
        ("i.e.,", "that is, "),
    ] {
        text = text.replace(from, to);
    }

    text = normalize_spoken_numbers(&text, lang);

    text = Regex::new(r" ,")?.replace_all(&text, ",").to_string();
    text = Regex::new(r" \.")?.replace_all(&text, ".").to_string();
    text = Regex::new(r" !")?.replace_all(&text, "!").to_string();
    text = Regex::new(r" \?")?.replace_all(&text, "?").to_string();
    text = Regex::new(r" ;")?.replace_all(&text, ";").to_string();
    text = Regex::new(r" :")?.replace_all(&text, ":").to_string();
    text = Regex::new(r" '")?.replace_all(&text, "'").to_string();

    while text.contains("\"\"") {
        text = text.replace("\"\"", "\"");
    }
    while text.contains("''") {
        text = text.replace("''", "'");
    }
    while text.contains("``") {
        text = text.replace("``", "`");
    }

    text = Regex::new(r"\s+")?.replace_all(&text, " ").to_string();
    text = text.trim().to_string();

    if !text.is_empty() {
        let ends_with_punct =
            Regex::new(r#"[.!?;:,'"\u{201C}\u{201D}\u{2018}\u{2019})\]}…。」』】〉》›»]$"#)?;
        if !ends_with_punct.is_match(&text) {
            text.push('.');
        }
    }

    if !AVAILABLE_LANGS.contains(&lang) {
        bail!(
            "Invalid language: {}. Available: {:?}",
            lang,
            AVAILABLE_LANGS
        );
    }

    Ok(format!("<{lang}>{text}</{lang}>"))
}

fn normalize_spoken_numbers(text: &str, lang: &str) -> String {
    if lang != "en" {
        return text.to_string();
    }

    let mut normalized = String::with_capacity(text.len());
    let mut cursor = 0usize;

    while let Some(start) = find_next_ascii_digit(text, cursor) {
        normalized.push_str(&text[cursor..start]);

        if let Some((spoken_numbers, end)) = collect_countoff_number_run(text, start) {
            normalized.push_str(&spoken_numbers.join(", "));
            cursor = end;
            continue;
        }

        let end = ascii_digit_run_end(text, start);
        let digits = &text[start..end];
        let previous = text[..start].chars().next_back();
        let next = text[end..].chars().next();
        if number_context_blocks_expansion(previous) || number_context_blocks_expansion(next) {
            normalized.push_str(digits);
            cursor = end;
            continue;
        }

        if digits.len() > 6 {
            normalized.push_str(digits);
            cursor = end;
            continue;
        }

        let spoken = if digits.len() > 1 && digits.starts_with('0') {
            spell_digits(digits)
        } else if let Ok(value) = digits.parse::<u32>() {
            integer_to_words(value)
        } else {
            digits.to_string()
        };
        normalized.push_str(&spoken);
        cursor = end;
    }

    normalized.push_str(&text[cursor..]);
    normalized
}

fn find_next_ascii_digit(text: &str, cursor: usize) -> Option<usize> {
    text[cursor..]
        .char_indices()
        .find(|(_, ch)| ch.is_ascii_digit())
        .map(|(index, _)| cursor + index)
}

fn collect_countoff_number_run(text: &str, start: usize) -> Option<(Vec<String>, usize)> {
    let first = parse_expandable_number_token(text, start)?;
    let mut spoken_numbers = vec![first.spoken];
    let mut end = first.end;

    loop {
        let separator_end = countoff_separator_end(text, end);
        if separator_end == end {
            break;
        }
        if !text[separator_end..]
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit())
        {
            break;
        }
        let Some(next) = parse_expandable_number_token(text, separator_end) else {
            break;
        };
        spoken_numbers.push(next.spoken);
        end = next.end;
    }

    if spoken_numbers.len() >= 2 {
        Some((spoken_numbers, end))
    } else {
        None
    }
}

fn countoff_separator_end(text: &str, start: usize) -> usize {
    let mut end = start;
    for (offset, ch) in text[start..].char_indices() {
        if !(ch.is_whitespace() || ch == ',') {
            break;
        }
        end = start + offset + ch.len_utf8();
    }
    end
}

struct SpokenNumberToken {
    spoken: String,
    end: usize,
}

fn parse_expandable_number_token(text: &str, start: usize) -> Option<SpokenNumberToken> {
    let end = ascii_digit_run_end(text, start);
    let digits = &text[start..end];
    let previous = text[..start].chars().next_back();
    let next = text[end..].chars().next();
    if digits.is_empty()
        || digits.len() > 6
        || number_context_blocks_expansion(previous)
        || number_context_blocks_expansion(next)
    {
        return None;
    }

    let spoken = if digits.len() > 1 && digits.starts_with('0') {
        spell_digits(digits)
    } else {
        integer_to_words(digits.parse::<u32>().ok()?)
    };
    Some(SpokenNumberToken { spoken, end })
}

fn ascii_digit_run_end(text: &str, start: usize) -> usize {
    let mut end = start;
    for (offset, ch) in text[start..].char_indices() {
        if !ch.is_ascii_digit() {
            break;
        }
        end = start + offset + ch.len_utf8();
    }
    end
}

fn number_context_blocks_expansion(ch: Option<char>) -> bool {
    ch.is_some_and(|ch| {
        ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                '.' | ':' | '/' | '-' | '+' | '%' | '$' | '€' | '£' | '¥'
            )
    })
}

fn spell_digits(digits: &str) -> String {
    digits
        .chars()
        .filter_map(|digit| digit.to_digit(10).map(|digit| DIGIT_WORDS[digit as usize]))
        .collect::<Vec<_>>()
        .join(" ")
}

const DIGIT_WORDS: &[&str] = &[
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];

const SMALL_NUMBER_WORDS: &[&str] = &[
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];

const TENS_WORDS: &[&str] = &[
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

fn integer_to_words(value: u32) -> String {
    match value {
        0..=19 => SMALL_NUMBER_WORDS[value as usize].to_string(),
        20..=99 => {
            let tens = value / 10;
            let ones = value % 10;
            if ones == 0 {
                TENS_WORDS[tens as usize].to_string()
            } else {
                format!(
                    "{} {}",
                    TENS_WORDS[tens as usize], SMALL_NUMBER_WORDS[ones as usize]
                )
            }
        }
        100..=999 => {
            let hundreds = value / 100;
            let remainder = value % 100;
            if remainder == 0 {
                format!("{} hundred", SMALL_NUMBER_WORDS[hundreds as usize])
            } else {
                format!(
                    "{} hundred {}",
                    SMALL_NUMBER_WORDS[hundreds as usize],
                    integer_to_words(remainder)
                )
            }
        }
        1_000..=999_999 => {
            let thousands = value / 1_000;
            let remainder = value % 1_000;
            if remainder == 0 {
                format!("{} thousand", integer_to_words(thousands))
            } else {
                format!(
                    "{} thousand {}",
                    integer_to_words(thousands),
                    integer_to_words(remainder)
                )
            }
        }
        _ => value.to_string(),
    }
}

fn text_to_unicode_values(text: &str) -> Vec<usize> {
    text.chars().map(|ch| ch as usize).collect()
}

fn get_text_mask(text_ids_lengths: &[usize]) -> Array3<f32> {
    let max_len = *text_ids_lengths.iter().max().unwrap_or(&0);
    length_to_mask(text_ids_lengths, Some(max_len))
}

fn length_to_mask(lengths: &[usize], max_len: Option<usize>) -> Array3<f32> {
    let batch_size = lengths.len();
    let max_len = max_len.unwrap_or_else(|| *lengths.iter().max().unwrap_or(&0));
    let mut mask = Array3::<f32>::zeros((batch_size, 1, max_len));
    for (batch, length) in lengths.iter().enumerate() {
        for index in 0..(*length).min(max_len) {
            mask[[batch, 0, index]] = 1.0;
        }
    }
    mask
}

fn sample_noisy_latent(
    duration: &[f32],
    sample_rate: i32,
    base_chunk_size: i32,
    chunk_compress: i32,
    latent_dim: i32,
) -> (Array3<f32>, Array3<f32>) {
    let batch_size = duration.len();
    let max_duration = duration.iter().fold(0.0f32, |max, item| max.max(*item));
    let wav_len_max = (max_duration * sample_rate as f32) as usize;
    let wav_lengths = duration
        .iter()
        .map(|duration| (*duration * sample_rate as f32) as usize)
        .collect::<Vec<_>>();
    let chunk_size = (base_chunk_size * chunk_compress) as usize;
    let latent_len = wav_len_max.div_ceil(chunk_size).max(1);
    let latent_dim = (latent_dim * chunk_compress) as usize;

    let mut noisy_latent = Array3::<f32>::zeros((batch_size, latent_dim, latent_len));
    let normal = Normal::new(0.0, 1.0).expect("normal distribution parameters are valid");
    let mut rng = rand::thread_rng();
    for batch in 0..batch_size {
        for dim in 0..latent_dim {
            for index in 0..latent_len {
                noisy_latent[[batch, dim, index]] = normal.sample(&mut rng);
            }
        }
    }

    let latent_lengths = wav_lengths
        .iter()
        .map(|length| length.div_ceil(chunk_size).max(1))
        .collect::<Vec<_>>();
    let latent_mask = length_to_mask(&latent_lengths, Some(latent_len));
    for batch in 0..batch_size {
        for dim in 0..latent_dim {
            for index in 0..latent_len {
                noisy_latent[[batch, dim, index]] *= latent_mask[[batch, 0, index]];
            }
        }
    }

    (noisy_latent, latent_mask)
}

const ABBREVIATIONS: &[&str] = &[
    "Dr.", "Mr.", "Mrs.", "Ms.", "Prof.", "Sr.", "Jr.", "St.", "Ave.", "Rd.", "Blvd.", "Dept.",
    "Inc.", "Ltd.", "Co.", "Corp.", "etc.", "vs.", "i.e.", "e.g.", "Ph.D.",
];

fn chunk_text(text: &str, max_len: Option<usize>) -> Vec<String> {
    let max_len = max_len.unwrap_or(MAX_CHUNK_LENGTH);
    let text = text.trim();

    if text.is_empty() {
        return vec![String::new()];
    }

    let para_re = Regex::new(r"\n\s*\n").expect("paragraph regex is valid");
    let paragraphs: Vec<&str> = para_re.split(text).collect();
    let mut chunks = Vec::new();

    for para in paragraphs {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }

        if para.len() <= max_len {
            chunks.push(para.to_string());
            continue;
        }

        let sentences = split_sentences(para);
        let mut current = String::new();
        let mut current_len = 0usize;

        for sentence in sentences {
            let sentence = sentence.trim();
            if sentence.is_empty() {
                continue;
            }

            let sentence_len = sentence.len();
            if sentence_len > max_len {
                if !current.is_empty() {
                    chunks.push(current.trim().to_string());
                    current.clear();
                    current_len = 0;
                }

                let parts: Vec<&str> = sentence.split(',').collect();
                for part in parts {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }

                    let part_len = part.len();
                    if part_len > max_len {
                        let words: Vec<&str> = part.split_whitespace().collect();
                        let mut word_chunk = String::new();
                        let mut word_chunk_len = 0usize;

                        for word in words {
                            let word_len = word.len();
                            if word_chunk_len + word_len + 1 > max_len && !word_chunk.is_empty() {
                                chunks.push(word_chunk.trim().to_string());
                                word_chunk.clear();
                                word_chunk_len = 0;
                            }

                            if !word_chunk.is_empty() {
                                word_chunk.push(' ');
                                word_chunk_len += 1;
                            }
                            word_chunk.push_str(word);
                            word_chunk_len += word_len;
                        }

                        if !word_chunk.is_empty() {
                            chunks.push(word_chunk.trim().to_string());
                        }
                    } else {
                        if current_len + part_len + 1 > max_len && !current.is_empty() {
                            chunks.push(current.trim().to_string());
                            current.clear();
                            current_len = 0;
                        }

                        if !current.is_empty() {
                            current.push_str(", ");
                            current_len += 2;
                        }
                        current.push_str(part);
                        current_len += part_len;
                    }
                }
                continue;
            }

            if current_len + sentence_len + 1 > max_len && !current.is_empty() {
                chunks.push(current.trim().to_string());
                current.clear();
                current_len = 0;
            }
            if !current.is_empty() {
                current.push(' ');
                current_len += 1;
            }
            current.push_str(sentence);
            current_len += sentence_len;
        }

        if !current.is_empty() {
            chunks.push(current.trim().to_string());
        }
    }

    if chunks.is_empty() {
        vec![String::new()]
    } else {
        chunks
    }
}

fn split_sentences(text: &str) -> Vec<String> {
    let regex = Regex::new(r"([.!?])\s+").expect("sentence regex is valid");
    let matches = regex.find_iter(text).collect::<Vec<_>>();
    if matches.is_empty() {
        return vec![text.to_string()];
    }

    let mut sentences = Vec::new();
    let mut last_end = 0usize;
    for item in matches {
        let before = &text[last_end..item.start()];
        let punctuation = &text[item.start()..item.start() + 1];
        let combined = format!("{}{}", before.trim(), punctuation);
        if ABBREVIATIONS
            .iter()
            .any(|abbreviation| combined.ends_with(abbreviation))
        {
            continue;
        }
        sentences.push(text[last_end..item.end()].to_string());
        last_end = item.end();
    }
    if last_end < text.len() {
        sentences.push(text[last_end..].to_string());
    }
    if sentences.is_empty() {
        vec![text.to_string()]
    } else {
        sentences
    }
}

fn normalize_and_convert_samples(samples: &[f32], source_rate: u32, gain: f32) -> Vec<i16> {
    if samples.is_empty() {
        return vec![0; MIX_SAMPLES_PER_FRAME];
    }
    let gain = gain.clamp(0.02, 1.0);
    let peak = samples
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    let normalizer = if peak > 0.001 {
        (0.85 / peak).min(4.0)
    } else {
        1.0
    };
    let source_samples = samples
        .iter()
        .map(|sample| float_to_i16(sample * normalizer * gain))
        .collect::<Vec<_>>();
    if source_rate == MIX_SAMPLE_RATE {
        source_samples
    } else {
        resample_linear(&source_samples, source_rate, MIX_SAMPLE_RATE)
    }
}

fn float_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

fn samples_to_frames(mut samples: Vec<i16>) -> Vec<Vec<i16>> {
    if samples.is_empty() {
        samples.resize(MIX_SAMPLES_PER_FRAME, 0);
    }
    let padding =
        (MIX_SAMPLES_PER_FRAME - samples.len() % MIX_SAMPLES_PER_FRAME) % MIX_SAMPLES_PER_FRAME;
    samples.extend(std::iter::repeat_n(0, padding));
    samples
        .chunks(MIX_SAMPLES_PER_FRAME)
        .map(|chunk| chunk.to_vec())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_supertonic_assets_are_present() {
        for (path, size) in embedded_asset_summary() {
            assert!(size > 1024, "{path} should be embedded");
        }
    }

    #[test]
    fn text_preprocessing_wraps_language_and_punctuation() {
        let text = preprocess_text("Stand by", "en").unwrap();
        assert_eq!(text, "<en>Stand by.</en>");
    }

    #[test]
    fn text_preprocessing_matches_upstream_symbol_handling() {
        let text = preprocess_text("“Use e.g., A—B @ rink → now”", "en").unwrap();
        assert_eq!(text, "<en>\"Use for example, A-B at rink now\"</en>");

        let text = preprocess_text("i.e., test  'quoted'", "en").unwrap();
        assert_eq!(text, "<en>that is, test 'quoted'</en>");
    }

    #[test]
    fn text_preprocessing_expands_short_spoken_numbers() {
        let text = preprocess_text("Test 1, 2, 3", "en").unwrap();
        assert_eq!(text, "<en>Test one, two, three.</en>");

        let text = preprocess_text("test 1 2 3", "en").unwrap();
        assert_eq!(text, "<en>test one, two, three.</en>");

        let text = preprocess_text("Client 50 to channel 7", "en").unwrap();
        assert_eq!(text, "<en>Client fifty to channel seven.</en>");
    }

    #[test]
    fn text_preprocessing_keeps_structured_numbers_intact() {
        let text = preprocess_text("Use 4:45 PM, $5.20, GOU-65, and 1.2.3", "en").unwrap();
        assert_eq!(text, "<en>Use 4:45 PM, $5.20, GOU-65, and 1.2.3.</en>");

        let text = preprocess_text("Code 0142", "en").unwrap();
        assert_eq!(text, "<en>Code zero one four two.</en>");
    }

    #[test]
    fn chunking_matches_upstream_comma_and_abbreviation_behavior() {
        let chunks = chunk_text("alpha, beta, gamma, delta", Some(12));
        assert_eq!(chunks, vec!["alpha, beta", "gamma, delta"]);

        let chunks = chunk_text("Dr. Smith went home. It was late.", Some(24));
        assert_eq!(chunks, vec!["Dr. Smith went home.", "It was late."]);
    }

    #[test]
    fn duration_limited_chunk_uses_predicted_duration() {
        let wav = vec![0.0; 100];
        assert_eq!(duration_limited_wav_chunk(&wav, 10, 3.2).len(), 32);
        assert_eq!(duration_limited_wav_chunk(&wav, 10, 20.0).len(), 100);
    }

    #[test]
    fn samples_are_split_into_mixer_frames() {
        let frames = samples_to_frames(vec![1; MIX_SAMPLES_PER_FRAME + 4]);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].len(), MIX_SAMPLES_PER_FRAME);
        assert_eq!(frames[1].len(), MIX_SAMPLES_PER_FRAME);
    }

    #[test]
    #[ignore = "runs the embedded ONNX model and is too slow for the default test suite"]
    fn real_supertonic_synthesis_smoke() {
        let frames = synthesize_blocking("Stand by", 0.5).unwrap();
        assert!(frames.len() > 10);
        assert!(frames
            .iter()
            .all(|frame| frame.len() == MIX_SAMPLES_PER_FRAME));
        assert!(frames.iter().flatten().any(|sample| *sample != 0));
    }

    #[test]
    #[ignore = "runs the embedded ONNX model and is intended for count-off TTS spot checks"]
    fn real_supertonic_countoff_synthesis_smoke() {
        let frames = synthesize_blocking("test 1 2 3", 0.5).unwrap();
        assert!(frames.len() > 10);
        assert!(frames
            .iter()
            .all(|frame| frame.len() == MIX_SAMPLES_PER_FRAME));
        assert!(frames.iter().flatten().any(|sample| *sample != 0));
    }
}
