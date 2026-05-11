use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use common::{AppleComputeUnits, MIX_SAMPLES_PER_FRAME, MIX_SAMPLE_RATE};
use coreml_native::{BorrowedTensor, ComputeUnits, Model};
use df::{post_filter, Complex32, DFState};

#[derive(Debug, Clone)]
pub(crate) struct CoreMlRuntimeParams {
    pub post_filter: bool,
    pub post_filter_beta: f32,
    pub atten_lim_db: f32,
    pub min_db_thresh: f32,
    pub max_db_erb_thresh: f32,
    pub max_db_df_thresh: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct CoreMlDeepFilterNetResult {
    pub samples: [i16; MIX_SAMPLES_PER_FRAME],
    pub lsnr_db: f32,
    pub lookahead_frames: usize,
    pub model_name: String,
}

pub(crate) struct CoreMlDeepFilterNet {
    enc: Model,
    erb_dec: Model,
    df_dec: Model,
    model_name: String,
    state: DFState,
    spec_buf: Vec<Complex32>,
    spec_enh: Vec<Complex32>,
    rolling_spec_buf_y: VecDeque<Vec<Complex32>>,
    rolling_spec_buf_x: VecDeque<Vec<Complex32>>,
    feat_erb: Vec<f32>,
    feat_spec: Vec<f32>,
    cplx_feature: Vec<Complex32>,
    enc_e0: Vec<f32>,
    enc_e1: Vec<f32>,
    enc_e2: Vec<f32>,
    enc_e3: Vec<f32>,
    enc_emb: Vec<f32>,
    enc_c0: Vec<f32>,
    enc_lsnr: Vec<f32>,
    erb_mask: Vec<f32>,
    df_coefs_raw: Vec<f32>,
    df_coefs: Vec<Complex32>,
    output: [f32; MIX_SAMPLES_PER_FRAME],
    nb_erb: usize,
    nb_df: usize,
    n_freqs: usize,
    df_order: usize,
    lookahead: usize,
    conv_lookahead: usize,
    alpha: f32,
    post_filter: bool,
    post_filter_beta: f32,
    min_db_thresh: f32,
    max_db_erb_thresh: f32,
    max_db_df_thresh: f32,
    atten_lim: Option<f32>,
    skip_counter: usize,
}

impl CoreMlDeepFilterNet {
    pub(crate) fn new(
        package_dir: impl AsRef<Path>,
        compute_units: AppleComputeUnits,
        runtime_params: &CoreMlRuntimeParams,
    ) -> Result<Self> {
        let package_dir = package_dir.as_ref();
        let package = CoreMlPackageConfig::read(package_dir)?;
        if package.sr != MIX_SAMPLE_RATE as usize || package.hop_size != MIX_SAMPLES_PER_FRAME {
            bail!(
                "DeepFilterNet Core ML package must run at {} Hz with {}-sample frames; package is {} Hz / {} samples",
                MIX_SAMPLE_RATE,
                MIX_SAMPLES_PER_FRAME,
                package.sr,
                package.hop_size
            );
        }

        let compute_units = coreml_compute_units(compute_units);
        let enc = load_model(package_dir.join("enc.mlmodelc"), compute_units)?;
        let erb_dec = load_model(package_dir.join("erb_dec.mlmodelc"), compute_units)?;
        let df_dec = load_model(package_dir.join("df_dec.mlmodelc"), compute_units)?;

        let n_freqs = package.fft_size / 2 + 1;
        let atten_lim = attenuation_limit(runtime_params.atten_lim_db);
        let lookahead = package.conv_lookahead.max(package.df_lookahead);
        let mut state = DFState::new(
            package.sr,
            package.fft_size,
            package.hop_size,
            package.nb_erb,
            package.min_nb_erb_freqs,
        );
        state.init_norm_states(package.nb_df);

        let mut model = Self {
            enc,
            erb_dec,
            df_dec,
            model_name: package_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("coreml-package")
                .to_string(),
            state,
            spec_buf: vec![Complex32::default(); n_freqs],
            spec_enh: vec![Complex32::default(); n_freqs],
            rolling_spec_buf_y: VecDeque::with_capacity(package.df_order + lookahead),
            rolling_spec_buf_x: VecDeque::with_capacity(lookahead.max(package.df_order)),
            feat_erb: vec![0.0; package.nb_erb],
            feat_spec: vec![0.0; 2 * package.nb_df],
            cplx_feature: vec![Complex32::default(); package.nb_df],
            enc_e0: vec![0.0; 64 * package.nb_erb],
            enc_e1: vec![0.0; 64 * (package.nb_erb / 2)],
            enc_e2: vec![0.0; 64 * (package.nb_erb / 4)],
            enc_e3: vec![0.0; 64 * (package.nb_erb / 4)],
            enc_emb: vec![0.0; 512],
            enc_c0: vec![0.0; 64 * package.nb_df],
            enc_lsnr: vec![0.0; 1],
            erb_mask: vec![0.0; package.nb_erb],
            df_coefs_raw: vec![0.0; package.nb_df * package.df_order * 2],
            df_coefs: vec![Complex32::default(); package.nb_df * package.df_order],
            output: [0.0; MIX_SAMPLES_PER_FRAME],
            nb_erb: package.nb_erb,
            nb_df: package.nb_df,
            n_freqs,
            df_order: package.df_order,
            lookahead,
            conv_lookahead: package.conv_lookahead,
            alpha: package.alpha,
            post_filter: runtime_params.post_filter,
            post_filter_beta: runtime_params.post_filter_beta,
            min_db_thresh: runtime_params.min_db_thresh,
            max_db_erb_thresh: runtime_params.max_db_erb_thresh,
            max_db_df_thresh: runtime_params.max_db_df_thresh,
            atten_lim,
            skip_counter: 0,
        };
        model.init_buffers();
        Ok(model)
    }

    pub(crate) fn process(
        &mut self,
        samples: [i16; MIX_SAMPLES_PER_FRAME],
    ) -> Result<CoreMlDeepFilterNetResult> {
        let mut input = [0.0_f32; MIX_SAMPLES_PER_FRAME];
        for (dst, sample) in input.iter_mut().zip(samples.iter()) {
            *dst = *sample as f32 / i16::MAX as f32;
        }

        let (mut max_a, mut energy) = (0.0_f32, 0.0_f32);
        for sample in input.iter() {
            max_a = max_a.max(sample.abs());
            energy += sample * sample;
        }
        let rms = energy / input.len() as f32;
        if rms < 1e-7 {
            self.skip_counter += 1;
        } else {
            self.skip_counter = 0;
        }
        if self.skip_counter > 5 {
            return Ok(CoreMlDeepFilterNetResult {
                samples: [0; MIX_SAMPLES_PER_FRAME],
                lsnr_db: -15.0,
                lookahead_frames: self.lookahead,
                model_name: self.model_name.clone(),
            });
        }
        if max_a > 0.9999 {
            tracing::warn!(
                max_amplitude = max_a,
                "possible clipping detected before DeepFilterNet Core ML"
            );
        }

        self.state.analysis(&input, &mut self.spec_buf);
        let mut enhanced_frame = self
            .rolling_spec_buf_y
            .pop_front()
            .context("DeepFilterNet enhanced rolling buffer missing")?;
        enhanced_frame.clone_from(&self.spec_buf);
        self.rolling_spec_buf_y.push_back(enhanced_frame);
        let mut noisy_frame = self
            .rolling_spec_buf_x
            .pop_front()
            .context("DeepFilterNet noisy rolling buffer missing")?;
        noisy_frame.clone_from(&self.spec_buf);
        self.rolling_spec_buf_x.push_back(noisy_frame);

        if self.atten_lim == Some(1.0) {
            let mut output = [0_i16; MIX_SAMPLES_PER_FRAME];
            for (dst, sample) in output.iter_mut().zip(input.iter()) {
                *dst = f32_to_i16(*sample);
            }
            return Ok(CoreMlDeepFilterNetResult {
                samples: output,
                lsnr_db: 35.0,
                lookahead_frames: self.lookahead,
                model_name: self.model_name.clone(),
            });
        }

        let (lsnr_db, has_gains, has_coefs) = self.process_raw()?;
        let (apply_erb, _, _) = self.apply_stages(lsnr_db);

        let stage_index = self.df_order - 1;
        if has_gains {
            let spec = self
                .rolling_spec_buf_y
                .get_mut(stage_index)
                .context("DeepFilterNet ERB stage buffer missing")?;
            self.state.apply_mask(spec, &self.erb_mask);
            self.skip_counter = 0;
        } else {
            self.skip_counter += 1;
        }

        self.spec_buf.clone_from(
            self.rolling_spec_buf_y
                .get(stage_index)
                .context("DeepFilterNet enhanced spectrum buffer missing")?,
        );
        if has_coefs {
            apply_deep_filter(
                &self.rolling_spec_buf_x,
                &self.df_coefs,
                self.nb_df,
                self.df_order,
                &mut self.spec_buf,
            )?;
        }

        let noisy_index = self.lookahead.max(self.df_order) - self.lookahead - 1;
        let spec_noisy = self
            .rolling_spec_buf_x
            .get(noisy_index)
            .context("DeepFilterNet noisy spectrum buffer missing")?;

        if apply_erb && self.post_filter {
            post_filter(spec_noisy, &mut self.spec_buf, self.post_filter_beta);
        }

        if let Some(lim) = self.atten_lim {
            for (enh, noisy) in self.spec_buf.iter_mut().zip(spec_noisy.iter()) {
                *enh = *enh * (1.0 - lim) + *noisy * lim;
            }
        }

        self.spec_enh.clone_from(&self.spec_buf);
        self.state.synthesis(&mut self.spec_enh, &mut self.output);
        let mut output = [0_i16; MIX_SAMPLES_PER_FRAME];
        for (dst, sample) in output.iter_mut().zip(self.output.iter()) {
            *dst = f32_to_i16(*sample);
        }

        Ok(CoreMlDeepFilterNetResult {
            samples: output,
            lsnr_db,
            lookahead_frames: self.lookahead,
            model_name: self.model_name.clone(),
        })
    }

    fn init_buffers(&mut self) {
        self.rolling_spec_buf_y.clear();
        for _ in 0..(self.df_order + self.conv_lookahead) {
            self.rolling_spec_buf_y
                .push_back(vec![Complex32::default(); self.n_freqs]);
        }
        self.rolling_spec_buf_x.clear();
        for _ in 0..self.df_order.max(self.lookahead) {
            self.rolling_spec_buf_x
                .push_back(vec![Complex32::default(); self.n_freqs]);
        }
    }

    fn process_raw(&mut self) -> Result<(f32, bool, bool)> {
        self.state
            .feat_erb(&self.spec_buf, self.alpha, &mut self.feat_erb);
        self.state.feat_cplx(
            &self.spec_buf[..self.nb_df],
            self.alpha,
            &mut self.cplx_feature,
        );
        for (index, sample) in self.cplx_feature.iter().enumerate() {
            self.feat_spec[index] = sample.re;
            self.feat_spec[self.nb_df + index] = sample.im;
        }

        {
            let feat_erb = BorrowedTensor::from_f32(&self.feat_erb, &[1, 1, 1, self.nb_erb])
                .context("create Core ML DeepFilterNet ERB tensor")?;
            let feat_spec = BorrowedTensor::from_f32(&self.feat_spec, &[1, 2, 1, self.nb_df])
                .context("create Core ML DeepFilterNet complex feature tensor")?;
            let enc = self
                .enc
                .predict(&[("feat_erb", &feat_erb), ("feat_spec", &feat_spec)])
                .context("run DeepFilterNet Core ML encoder")?;

            enc.get_f32_into("lsnr", &mut self.enc_lsnr)
                .context("read Core ML LSNR output")?;
            enc.get_f32_into("e0", &mut self.enc_e0)
                .context("read Core ML e0 output")?;
            enc.get_f32_into("e1", &mut self.enc_e1)
                .context("read Core ML e1 output")?;
            enc.get_f32_into("e2", &mut self.enc_e2)
                .context("read Core ML e2 output")?;
            enc.get_f32_into("e3", &mut self.enc_e3)
                .context("read Core ML e3 output")?;
            enc.get_f32_into("emb", &mut self.enc_emb)
                .context("read Core ML emb output")?;
            enc.get_f32_into("c0", &mut self.enc_c0)
                .context("read Core ML c0 output")?;
        }

        let lsnr = self.enc_lsnr.first().copied().unwrap_or(-15.0);
        let (apply_gains, apply_gain_zeros, apply_df) = self.apply_stages(lsnr);

        let has_gains = if apply_gains {
            let pred = {
                let emb_t = BorrowedTensor::from_f32(&self.enc_emb, &[1, 1, 512])
                    .context("create Core ML ERB decoder emb tensor")?;
                let e3_t = BorrowedTensor::from_f32(&self.enc_e3, &[1, 64, 1, self.nb_erb / 4])
                    .context("create Core ML ERB decoder e3 tensor")?;
                let e2_t = BorrowedTensor::from_f32(&self.enc_e2, &[1, 64, 1, self.nb_erb / 4])
                    .context("create Core ML ERB decoder e2 tensor")?;
                let e1_t = BorrowedTensor::from_f32(&self.enc_e1, &[1, 64, 1, self.nb_erb / 2])
                    .context("create Core ML ERB decoder e1 tensor")?;
                let e0_t = BorrowedTensor::from_f32(&self.enc_e0, &[1, 64, 1, self.nb_erb])
                    .context("create Core ML ERB decoder e0 tensor")?;
                self.erb_dec
                    .predict(&[
                        ("emb", &emb_t),
                        ("e3", &e3_t),
                        ("e2", &e2_t),
                        ("e1", &e1_t),
                        ("e0", &e0_t),
                    ])
                    .context("run DeepFilterNet Core ML ERB decoder")?
            };
            pred.get_f32_into("m", &mut self.erb_mask)
                .context("read Core ML ERB mask")?;
            true
        } else if apply_gain_zeros {
            self.erb_mask.fill(0.0);
            true
        } else {
            false
        };

        let has_coefs = if apply_df {
            let pred = {
                let emb_t = BorrowedTensor::from_f32(&self.enc_emb, &[1, 1, 512])
                    .context("create Core ML DF decoder emb tensor")?;
                let c0_t = BorrowedTensor::from_f32(&self.enc_c0, &[1, 64, 1, self.nb_df])
                    .context("create Core ML DF decoder c0 tensor")?;
                self.df_dec
                    .predict(&[("emb", &emb_t), ("c0", &c0_t)])
                    .context("run DeepFilterNet Core ML DF decoder")?
            };
            pred.get_f32_into("coefs", &mut self.df_coefs_raw)
                .context("read Core ML DF coefficients")?;
            coreml_coefs_to_complex_into(
                &self.df_coefs_raw,
                self.nb_df,
                self.df_order,
                &mut self.df_coefs,
            )?;
            true
        } else {
            false
        };

        Ok((lsnr, has_gains, has_coefs))
    }

    fn apply_stages(&self, lsnr: f32) -> (bool, bool, bool) {
        if lsnr < self.min_db_thresh {
            (false, true, false)
        } else if lsnr > self.max_db_erb_thresh {
            (false, false, false)
        } else if lsnr > self.max_db_df_thresh {
            (true, false, false)
        } else {
            (true, false, true)
        }
    }
}

#[derive(Debug)]
struct CoreMlPackageConfig {
    sr: usize,
    hop_size: usize,
    fft_size: usize,
    min_nb_erb_freqs: usize,
    nb_erb: usize,
    nb_df: usize,
    df_order: usize,
    df_lookahead: usize,
    conv_lookahead: usize,
    alpha: f32,
}

impl CoreMlPackageConfig {
    fn read(package_dir: &Path) -> Result<Self> {
        if !package_dir.is_dir() {
            bail!(
                "DeepFilterNet Core ML package is not a directory: {}",
                package_dir.display()
            );
        }
        for entry in ["enc.mlmodelc", "erb_dec.mlmodelc", "df_dec.mlmodelc"] {
            let path = package_dir.join(entry);
            if !path.is_dir() {
                bail!(
                    "DeepFilterNet Core ML package is missing {}",
                    path.display()
                );
            }
        }
        let config = read_simple_ini(&package_dir.join("config.ini"))?;
        let sr = config_usize(&config, "df", "sr")?;
        let hop_size = config_usize(&config, "df", "hop_size")?;
        let fft_size = config_usize(&config, "df", "fft_size")?;
        let min_nb_erb_freqs = config_usize(&config, "df", "min_nb_erb_freqs")?;
        let nb_erb = config_usize(&config, "df", "nb_erb")?;
        let nb_df = config_usize(&config, "df", "nb_df")?;
        let df_order = config_usize_fallback(&config, "df", "df_order", "deepfilternet")?;
        let df_lookahead = config_usize_fallback(&config, "df", "df_lookahead", "deepfilternet")?;
        let conv_lookahead = config_usize(&config, "deepfilternet", "conv_lookahead")?;
        let alpha = config
            .get("df")
            .and_then(|section| section.get("norm_alpha"))
            .map(|value| {
                value
                    .parse::<f32>()
                    .with_context(|| format!("parse df.norm_alpha value `{value}`"))
            })
            .transpose()?
            .unwrap_or_else(|| {
                calc_norm_alpha(
                    sr,
                    hop_size,
                    config_f32(&config, "df", "norm_tau").unwrap_or(1.0),
                )
            });

        Ok(Self {
            sr,
            hop_size,
            fft_size,
            min_nb_erb_freqs,
            nb_erb,
            nb_df,
            df_order,
            df_lookahead,
            conv_lookahead,
            alpha,
        })
    }
}

fn load_model(path: PathBuf, compute_units: ComputeUnits) -> Result<Model> {
    Model::load(&path, compute_units)
        .with_context(|| format!("load Core ML model {}", path.display()))
}

fn coreml_compute_units(units: AppleComputeUnits) -> ComputeUnits {
    match units {
        AppleComputeUnits::CpuOnly => ComputeUnits::CpuOnly,
        AppleComputeUnits::CpuAndGpu => ComputeUnits::CpuAndGpu,
        AppleComputeUnits::CpuAndNeuralEngine => ComputeUnits::CpuAndNeuralEngine,
        AppleComputeUnits::All => ComputeUnits::All,
    }
}

fn attenuation_limit(db: f32) -> Option<f32> {
    let lim = db.abs();
    if lim >= 100.0 {
        None
    } else if lim < 0.01 {
        tracing::warn!(
            "DeepFilterNet attenuation limit is too strong; no noise reduction will be performed"
        );
        Some(1.0)
    } else {
        Some(10f32.powf(-lim / 20.0))
    }
}

fn apply_deep_filter(
    spec: &VecDeque<Vec<Complex32>>,
    coefs: &[Complex32],
    nb_df: usize,
    df_order: usize,
    spec_out: &mut [Complex32],
) -> Result<()> {
    if spec.len() < df_order {
        bail!(
            "DeepFilterNet Core ML DF buffer too short: {} < {}",
            spec.len(),
            df_order
        );
    }
    if coefs.len() != nb_df * df_order {
        bail!(
            "DeepFilterNet Core ML coefficient shape mismatch: {} != {}",
            coefs.len(),
            nb_df * df_order
        );
    }
    spec_out[..nb_df].fill(Complex32::default());
    for (order, spec_frame) in spec.iter().take(df_order).enumerate() {
        for freq in 0..nb_df {
            spec_out[freq] += spec_frame[freq] * coefs[freq * df_order + order];
        }
    }
    Ok(())
}

fn coreml_coefs_to_complex_into(
    coefs: &[f32],
    nb_df: usize,
    df_order: usize,
    output: &mut [Complex32],
) -> Result<()> {
    let expected = nb_df * df_order * 2;
    if coefs.len() != expected {
        bail!(
            "DeepFilterNet Core ML coefficient output shape mismatch: {} != {}",
            coefs.len(),
            expected
        );
    }
    if output.len() != nb_df * df_order {
        bail!(
            "DeepFilterNet Core ML coefficient buffer shape mismatch: {} != {}",
            output.len(),
            nb_df * df_order
        );
    }
    for freq in 0..nb_df {
        for order in 0..df_order {
            let src = (freq * df_order + order) * 2;
            output[freq * df_order + order] = Complex32::new(coefs[src], coefs[src + 1]);
        }
    }
    Ok(())
}

fn f32_to_i16(sample: f32) -> i16 {
    (sample * i16::MAX as f32)
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

fn read_simple_ini(path: &Path) -> Result<HashMap<String, HashMap<String, String>>> {
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut section = String::new();
    let mut values: HashMap<String, HashMap<String, String>> = HashMap::new();
    for raw_line in contents.lines() {
        let line = raw_line.split(['#', ';']).next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        values
            .entry(section.clone())
            .or_default()
            .insert(key.trim().to_string(), value.trim().to_string());
    }
    Ok(values)
}

fn config_value<'a>(
    config: &'a HashMap<String, HashMap<String, String>>,
    section: &str,
    key: &str,
) -> Result<&'a str> {
    config
        .get(section)
        .and_then(|section| section.get(key))
        .map(String::as_str)
        .with_context(|| format!("missing DeepFilterNet Core ML config value {section}.{key}"))
}

fn config_usize(
    config: &HashMap<String, HashMap<String, String>>,
    section: &str,
    key: &str,
) -> Result<usize> {
    let value = config_value(config, section, key)?;
    value
        .parse::<usize>()
        .with_context(|| format!("parse {section}.{key} value `{value}`"))
}

fn config_usize_fallback(
    config: &HashMap<String, HashMap<String, String>>,
    section: &str,
    key: &str,
    fallback_section: &str,
) -> Result<usize> {
    config_usize(config, section, key).or_else(|_| config_usize(config, fallback_section, key))
}

fn config_f32(
    config: &HashMap<String, HashMap<String, String>>,
    section: &str,
    key: &str,
) -> Result<f32> {
    let value = config_value(config, section, key)?;
    value
        .parse::<f32>()
        .with_context(|| format!("parse {section}.{key} value `{value}`"))
}

fn calc_norm_alpha(sr: usize, hop_size: usize, tau: f32) -> f32 {
    let dt = hop_size as f32 / sr as f32;
    let alpha = f32::exp(-dt / tau);
    let mut rounded = 1.0;
    let mut precision = 3;
    while rounded >= 1.0 {
        rounded = (alpha * 10i32.pow(precision) as f32).round() / 10i32.pow(precision) as f32;
        precision += 1;
    }
    rounded
}
