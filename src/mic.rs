//! Microphone capture via CoreAudio (through cpal).

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Records from the default input device until dropped.
pub struct Recorder {
    _stream: cpal::Stream,
    samples: Arc<Mutex<Vec<f32>>>,
    pub sample_rate: u32,
    pub device_name: String,
}

impl Recorder {
    pub fn start() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow!("no microphone / input device found"))?;
        let device_name = device.name().unwrap_or_else(|_| "unknown device".into());
        let supported = device.default_input_config()?;
        let sample_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let config: cpal::StreamConfig = supported.config();

        let samples = Arc::new(Mutex::new(Vec::new()));
        let err_fn = |e| eprintln!("audio stream error: {e}");
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => {
                let s = samples.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[f32], _: &_| push_mono(&s, data, channels, |x| x),
                    err_fn,
                    None,
                )?
            }
            cpal::SampleFormat::I16 => {
                let s = samples.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[i16], _: &_| push_mono(&s, data, channels, |x| x as f32 / 32768.0),
                    err_fn,
                    None,
                )?
            }
            other => bail!("unsupported microphone sample format: {other:?}"),
        };
        stream.play()?;
        Ok(Self {
            _stream: stream,
            samples,
            sample_rate,
            device_name,
        })
    }

    /// A copy of everything recorded so far (mono, at `sample_rate`).
    pub fn snapshot(&self) -> Vec<f32> {
        self.samples.lock().unwrap().clone()
    }
}

fn push_mono<T: Copy>(buf: &Mutex<Vec<f32>>, data: &[T], channels: usize, to_f32: impl Fn(T) -> f32) {
    let mut buf = buf.lock().unwrap();
    buf.extend(
        data.chunks_exact(channels)
            .map(|frame| frame.iter().map(|&x| to_f32(x)).sum::<f32>() / channels as f32),
    );
}
