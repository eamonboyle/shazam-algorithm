//! Audio decoding, resampling and WAV output.

use std::f64::consts::PI;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const SUPPORTED_EXTENSIONS: &[&str] = &["mp3", "wav", "flac", "ogg", "m4a", "aac"];

/// Decodes an audio file to mono f32 samples. Returns `(samples, sample_rate)`.
pub fn decode_file(path: &Path) -> Result<(Vec<f32>, u32)> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .with_context(|| format!("unrecognised audio format: {}", path.display()))?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("no audio track in {}", path.display()))?;
    let track_id = track.id;
    let mut sample_rate = track.codec_params.sample_rate;
    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let mut mono = Vec::new();
    let mut buf: Option<SampleBuffer<f32>> = None;
    let mut buf_frames = 0u64;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // A corrupt frame is recoverable; skip it like any media player would.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };

        let spec = *decoded.spec();
        sample_rate = Some(spec.rate);
        let channels = spec.channels.count();
        let frames = decoded.capacity() as u64;
        if frames > buf_frames {
            buf = Some(SampleBuffer::new(frames, spec));
            buf_frames = frames;
        }
        let b = buf.as_mut().unwrap();
        b.copy_interleaved_ref(decoded);
        mono.extend(
            b.samples()
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
    }

    let rate = sample_rate.ok_or_else(|| anyhow!("unknown sample rate: {}", path.display()))?;
    Ok((mono, rate))
}

/// Number of sinc zero-crossings on each side of the resampling kernel.
const ZERO_CROSSINGS: f64 = 10.0;
/// Kernel lookup-table entries per input sample.
const TABLE_RES: usize = 512;

/// Band-limited (windowed-sinc) resampler. When downsampling it also acts as
/// the anti-aliasing low-pass filter, cutting at 90% of the output Nyquist.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let cutoff = 0.9 * ratio.min(1.0); // as a fraction of the input Nyquist
    let half_width = ZERO_CROSSINGS / cutoff; // in input samples

    // Hann-windowed sinc, tabulated once since evaluating sin() per tap is slow.
    let table_len = (half_width * TABLE_RES as f64).ceil() as usize + 2;
    let table: Vec<f32> = (0..table_len)
        .map(|i| {
            let u = i as f64 / TABLE_RES as f64;
            if u >= half_width {
                return 0.0;
            }
            let x = PI * cutoff * u;
            let sinc = if x == 0.0 { 1.0 } else { x.sin() / x };
            let window = 0.5 * (1.0 + (PI * u / half_width).cos());
            (cutoff * sinc * window) as f32
        })
        .collect();
    let kernel = |u: f64| -> f32 {
        let pos = u.abs() * TABLE_RES as f64;
        let i = pos as usize;
        if i + 1 >= table_len {
            return 0.0;
        }
        let frac = (pos - i as f64) as f32;
        table[i] + (table[i + 1] - table[i]) * frac
    };

    let out_len = (input.len() as f64 * ratio).floor() as usize;
    let last = input.len() - 1;
    (0..out_len)
        .map(|n| {
            let t = n as f64 / ratio;
            let lo = (t - half_width).ceil().max(0.0) as usize;
            let hi = ((t + half_width).floor() as usize).min(last);
            (lo..=hi).map(|k| input[k] * kernel(t - k as f64)).sum()
        })
        .collect()
}

/// Writes mono samples as a 16-bit PCM WAV file.
pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) -> Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    let data_len = (samples.len() * 2) as u32;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?; // fmt chunk size
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&1u16.to_le_bytes())?; // mono
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&(sample_rate * 2).to_le_bytes())?; // byte rate
    w.write_all(&2u16.to_le_bytes())?; // block align
    w.write_all(&16u16.to_le_bytes())?; // bits per sample
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        w.write_all(&v.to_le_bytes())?;
    }
    w.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, secs: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin() * 0.5)
            .collect()
    }

    /// Estimates a pure tone's frequency by counting upward zero crossings.
    fn estimate_freq(samples: &[f32], rate: u32) -> f32 {
        let crossings = samples.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        crossings as f32 * rate as f32 / samples.len() as f32
    }

    #[test]
    fn resample_same_rate_is_identity() {
        let x = sine(440.0, 11_025, 0.5);
        assert_eq!(resample(&x, 11_025, 11_025), x);
    }

    #[test]
    fn resample_preserves_length_and_pitch() {
        for from in [44_100, 48_000, 8_000] {
            let x = sine(1_000.0, from, 2.0);
            let y = resample(&x, from, 11_025);
            assert!(
                (y.len() as i64 - 22_050).abs() <= 1,
                "{from} Hz -> {} samples",
                y.len()
            );
            let f = estimate_freq(&y, 11_025);
            assert!((f - 1_000.0).abs() < 5.0, "{from} Hz: tone came out at {f} Hz");
        }
    }

    #[test]
    fn resample_filters_out_tones_above_new_nyquist() {
        // 8 kHz can't be represented at 11,025 Hz (Nyquist 5.5 kHz); it must
        // be removed rather than aliased down to 3 kHz.
        let y = resample(&sine(8_000.0, 44_100, 1.0), 44_100, 11_025);
        let rms =
            (y[1000..y.len() - 1000].iter().map(|v| v * v).sum::<f32>() / (y.len() - 2000) as f32).sqrt();
        assert!(rms < 0.01, "aliased energy remained: rms {rms}");
    }

    #[test]
    fn wav_round_trip() {
        let x = sine(440.0, 11_025, 0.5);
        let path = std::env::temp_dir().join(format!("shazam-test-{}.wav", std::process::id()));
        write_wav(&path, &x, 11_025).unwrap();
        let (y, rate) = decode_file(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(rate, 11_025);
        assert_eq!(y.len(), x.len());
        assert!(x.iter().zip(&y).all(|(a, b)| (a - b).abs() < 1e-3));
    }
}
