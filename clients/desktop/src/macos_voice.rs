use std::sync::Arc;

use anyhow::Context;
use client_core::AudioSettings;
use common::MIX_SAMPLE_RATE;
use coreaudio::audio_unit::audio_format::LinearPcmFlags;
use coreaudio::audio_unit::render_callback::{self, data};
use coreaudio::audio_unit::{AudioUnit, Element, IOType, SampleFormat, Scope, StreamFormat};
use coreaudio::sys::{kAudioOutputUnitProperty_EnableIO, kAudioUnitProperty_StreamFormat};
use tokio::sync::mpsc;

use super::{CaptureAdapter, CapturePipelineOptions, CaptureSourceInfo};

pub struct VoiceProcessingInputStream {
    audio_unit: AudioUnit,
    started: bool,
}

impl VoiceProcessingInputStream {
    pub fn new(
        tx: mpsc::Sender<Vec<i16>>,
        audio_settings: Arc<AudioSettings>,
        capture_options: CapturePipelineOptions,
    ) -> anyhow::Result<Self> {
        let mut audio_unit = AudioUnit::new(IOType::VoiceProcessingIO)
            .context("create macOS VoiceProcessingIO audio unit")?;
        audio_unit
            .uninitialize()
            .context("prepare VoiceProcessingIO configuration")?;

        set_enable_io(&mut audio_unit, Scope::Input, Element::Input, true)
            .context("enable VoiceProcessingIO input")?;
        let output_bus_enabled = match set_enable_io(
            &mut audio_unit,
            Scope::Output,
            Element::Output,
            true,
        ) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(
                    %err,
                    "VoiceProcessingIO output bus could not be enabled; continuing with input-only voice processing"
                );
                false
            }
        };

        let stream_format = StreamFormat {
            sample_rate: MIX_SAMPLE_RATE as f64,
            sample_format: SampleFormat::F32,
            flags: LinearPcmFlags::IS_FLOAT
                | LinearPcmFlags::IS_PACKED
                | LinearPcmFlags::IS_NON_INTERLEAVED,
            channels: 1,
        };
        let asbd = stream_format.to_asbd();
        audio_unit
            .set_property(
                kAudioUnitProperty_StreamFormat,
                Scope::Output,
                Element::Input,
                Some(&asbd),
            )
            .context("set VoiceProcessingIO input stream format")?;

        if output_bus_enabled {
            if let Err(err) = configure_silent_output(&mut audio_unit, &asbd) {
                tracing::warn!(
                    %err,
                    "VoiceProcessingIO silent output callback could not be installed; continuing with capture only"
                );
                if let Err(disable_err) =
                    set_enable_io(&mut audio_unit, Scope::Output, Element::Output, false)
                {
                    tracing::warn!(
                        %disable_err,
                        "VoiceProcessingIO output bus could not be disabled after silent callback setup failed"
                    );
                }
            }
        }

        if let Some(diagnostics) = &capture_options.diagnostics {
            diagnostics.set_source(CaptureSourceInfo {
                backend: super::AudioInputBackend::VoiceProcessing,
                device: "macOS default VoiceProcessingIO input".to_string(),
                sample_format: "F32".to_string(),
                sample_rate_hz: MIX_SAMPLE_RATE,
                channels: 1,
                channel_mode: capture_options.channel_mode,
            });
        }
        let mut capture =
            CaptureAdapter::new(tx, MIX_SAMPLE_RATE, 1, audio_settings, capture_options);
        type Args = render_callback::Args<data::NonInterleaved<f32>>;
        audio_unit
            .set_input_callback(move |args: Args| {
                let Args { data, .. } = args;
                let mut channels = data.channels();
                let Some(channel) = channels.next() else {
                    return Ok(());
                };
                for sample in channel {
                    capture.push_mono(*sample);
                }
                Ok(())
            })
            .context("set VoiceProcessingIO input callback")?;
        audio_unit
            .initialize()
            .context("initialize VoiceProcessingIO input")?;

        Ok(Self {
            audio_unit,
            started: false,
        })
    }

    pub fn play(&mut self) -> anyhow::Result<()> {
        if !self.started {
            self.audio_unit
                .start()
                .context("start macOS VoiceProcessingIO input")?;
            self.started = true;
        }
        Ok(())
    }
}

fn set_enable_io(
    audio_unit: &mut AudioUnit,
    scope: Scope,
    element: Element,
    enabled: bool,
) -> anyhow::Result<()> {
    let value = u32::from(enabled);
    audio_unit
        .set_property(
            kAudioOutputUnitProperty_EnableIO,
            scope,
            element,
            Some(&value),
        )
        .context("set VoiceProcessingIO EnableIO property")
}

fn configure_silent_output(
    audio_unit: &mut AudioUnit,
    asbd: &coreaudio::sys::AudioStreamBasicDescription,
) -> anyhow::Result<()> {
    audio_unit
        .set_property(
            kAudioUnitProperty_StreamFormat,
            Scope::Input,
            Element::Output,
            Some(asbd),
        )
        .context("set VoiceProcessingIO output stream format")?;

    type Args = render_callback::Args<data::NonInterleaved<f32>>;
    audio_unit
        .set_render_callback(move |args: Args| {
            let Args {
                num_frames,
                mut data,
                ..
            } = args;
            for channel in data.channels_mut() {
                for sample in channel.iter_mut().take(num_frames) {
                    *sample = 0.0;
                }
            }
            Ok(())
        })
        .context("set VoiceProcessingIO silent output callback")?;
    Ok(())
}
