//! The fingerprinting pipeline from Wang (2003), "An Industrial-Strength Audio
//! Search Algorithm": spectrogram -> constellation of peaks -> paired hashes.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// Everything is fingerprinted at this rate. Most identifying content lives
/// below ~5 kHz, and a lower rate means far less work.
pub const SAMPLE_RATE: u32 = 11_025;
pub const FFT_SIZE: usize = 1024;
pub const HOP: usize = 256;

/// Frequency bins considered for peaks (~170 Hz to ~5.5 kHz). Laptop mics
/// barely pick up bass, so peaks below that aren't reliable.
const MIN_BIN: usize = 16;
const MAX_BIN: usize = FFT_SIZE / 2;

/// A peak must be the loudest point within this many bins / frames.
const PEAK_FREQ_RADIUS: usize = 5;
const PEAK_TIME_RADIUS: usize = 3;
/// Peaks quieter than the loudest point minus this are ignored.
const PEAK_DYNAMIC_RANGE_DB: f32 = 60.0;
/// Absolute floor so digital silence produces no peaks.
const PEAK_ABS_FLOOR_DB: f32 = -30.0;
/// Constellation density cap: keep the strongest N peaks per second.
const PEAKS_PER_SEC: usize = 30;

/// Each anchor peak is paired with this many later peaks...
const FAN_OUT: usize = 20;
/// ...that fall within this many frames after it.
const MIN_DT: u32 = 1;
const MAX_DT: u32 = 90; // ~2 s

/// Version tag for the database: bump whenever any parameter above changes.
pub const FINGERPRINT_VERSION: u32 = 2;

pub fn frames_to_secs(frames: f32) -> f32 {
    frames * HOP as f32 / SAMPLE_RATE as f32
}

#[derive(Clone, Copy, Debug)]
pub struct Peak {
    pub frame: u32,
    pub bin: u16,
    pub db: f32,
}

/// A hash plus the frame of its anchor peak.
#[derive(Clone, Copy, Debug)]
pub struct Hash {
    pub hash: u32,
    pub frame: u32,
}

pub struct Fingerprinter {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
}

impl Default for Fingerprinter {
    fn default() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);
        let window = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32).cos())
            .collect();
        Self { fft, window }
    }
}

impl Fingerprinter {
    /// Full pipeline: mono samples at `SAMPLE_RATE` -> hashes.
    pub fn fingerprint(&self, samples: &[f32]) -> Vec<Hash> {
        hashes(&find_peaks(&self.spectrogram(samples)))
    }

    /// Log-magnitude spectrogram, `[frame][bin]` in dB, bins `0..FFT_SIZE/2`.
    pub fn spectrogram(&self, samples: &[f32]) -> Vec<Vec<f32>> {
        if samples.len() < FFT_SIZE {
            return Vec::new();
        }
        let n_frames = (samples.len() - FFT_SIZE) / HOP + 1;
        let mut buf = vec![Complex::new(0.0f32, 0.0); FFT_SIZE];
        let mut scratch = vec![Complex::new(0.0f32, 0.0); self.fft.get_inplace_scratch_len()];
        (0..n_frames)
            .map(|f| {
                let chunk = &samples[f * HOP..f * HOP + FFT_SIZE];
                for ((b, &s), &w) in buf.iter_mut().zip(chunk).zip(&self.window) {
                    *b = Complex::new(s * w, 0.0);
                }
                self.fft.process_with_scratch(&mut buf, &mut scratch);
                buf[..FFT_SIZE / 2]
                    .iter()
                    .map(|c| 10.0 * (c.norm_sqr() + 1e-20).log10())
                    .collect()
            })
            .collect()
    }
}

/// Builds the "constellation map": points that are local maxima in a
/// time-frequency neighbourhood, thinned to the strongest per second.
pub fn find_peaks(spec: &[Vec<f32>]) -> Vec<Peak> {
    let n_frames = spec.len();
    if n_frames == 0 {
        return Vec::new();
    }

    // Separable 2D max filter: first across frequency, then across time.
    let freq_max: Vec<Vec<f32>> = spec
        .iter()
        .map(|frame| {
            (0..MAX_BIN)
                .map(|b| {
                    let lo = b.saturating_sub(PEAK_FREQ_RADIUS);
                    let hi = (b + PEAK_FREQ_RADIUS + 1).min(MAX_BIN);
                    frame[lo..hi].iter().copied().fold(f32::MIN, f32::max)
                })
                .collect()
        })
        .collect();

    let loudest = spec
        .iter()
        .flat_map(|f| f[MIN_BIN..MAX_BIN].iter().copied())
        .fold(f32::MIN, f32::max);
    let floor = (loudest - PEAK_DYNAMIC_RANGE_DB).max(PEAK_ABS_FLOOR_DB);

    let mut candidates = Vec::new();
    for (t, frame) in spec.iter().enumerate() {
        let lo = t.saturating_sub(PEAK_TIME_RADIUS);
        let hi = (t + PEAK_TIME_RADIUS + 1).min(n_frames);
        for b in MIN_BIN..MAX_BIN {
            let v = frame[b];
            if v <= floor {
                continue;
            }
            if (lo..hi).all(|tt| freq_max[tt][b] <= v) {
                candidates.push(Peak {
                    frame: t as u32,
                    bin: b as u16,
                    db: v,
                });
            }
        }
    }

    // Density cap per one-second chunk, keeping the loudest.
    let chunk = (SAMPLE_RATE as usize / HOP).max(1) as u32;
    let mut peaks = Vec::with_capacity(candidates.len());
    for group in candidates.chunk_by_mut(|a, b| a.frame / chunk == b.frame / chunk) {
        group.sort_unstable_by(|a, b| b.db.total_cmp(&a.db));
        peaks.extend_from_slice(&group[..group.len().min(PEAKS_PER_SEC)]);
    }
    peaks.sort_unstable_by_key(|p| (p.frame, p.bin));
    peaks
}

/// Combinatorial hashing: pair each anchor peak with the next `FAN_OUT` peaks
/// in its target zone. A hash packs `(anchor bin, target bin, frame delta)`,
/// which is invariant to where in the song the clip starts.
pub fn hashes(peaks: &[Peak]) -> Vec<Hash> {
    let mut out = Vec::with_capacity(peaks.len() * FAN_OUT);
    for (i, anchor) in peaks.iter().enumerate() {
        let targets = peaks[i + 1..]
            .iter()
            .filter(|p| p.frame - anchor.frame >= MIN_DT)
            .take_while(|p| p.frame - anchor.frame <= MAX_DT)
            .take(FAN_OUT);
        for target in targets {
            out.push(Hash {
                hash: pack(anchor.bin, target.bin, target.frame - anchor.frame),
                frame: anchor.frame,
            });
        }
    }
    out
}

/// 9 bits anchor bin | 9 bits target bin | 14 bits delta frames.
fn pack(anchor_bin: u16, target_bin: u16, dt: u32) -> u32 {
    (anchor_bin as u32 & 0x1FF) << 23 | (target_bin as u32 & 0x1FF) << 14 | (dt & 0x3FFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_keeps_fields_separate() {
        assert_eq!(pack(1, 0, 0), 1 << 23);
        assert_eq!(pack(0, 1, 0), 1 << 14);
        assert_eq!(pack(0, 0, 1), 1);
        assert_eq!(pack(511, 511, MAX_DT), (511 << 23) | (511 << 14) | MAX_DT);
        assert_ne!(pack(100, 200, 5), pack(200, 100, 5));
    }

    #[test]
    fn silence_has_no_peaks() {
        let fp = Fingerprinter::default();
        assert!(fp.fingerprint(&vec![0.0; SAMPLE_RATE as usize * 3]).is_empty());
    }

    #[test]
    fn tone_peaks_land_on_its_frequency_bin() {
        let freq = 1_000.0;
        let samples: Vec<f32> = (0..SAMPLE_RATE as usize * 2)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / SAMPLE_RATE as f32).sin())
            .collect();
        let peaks = find_peaks(&Fingerprinter::default().spectrogram(&samples));
        let expected = (freq * FFT_SIZE as f32 / SAMPLE_RATE as f32).round() as i32;
        assert!(!peaks.is_empty());
        assert!(
            peaks.iter().all(|p| (p.bin as i32 - expected).abs() <= 1),
            "{peaks:?}"
        );
    }

    #[test]
    fn peak_density_is_capped() {
        // White noise is the worst case: local maxima everywhere.
        let mut x = 1u32;
        let noise: Vec<f32> = (0..SAMPLE_RATE as usize * 4)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect();
        let peaks = find_peaks(&Fingerprinter::default().spectrogram(&noise));
        assert!(
            peaks.len() <= PEAKS_PER_SEC * 4 + PEAKS_PER_SEC,
            "{} peaks",
            peaks.len()
        );
        assert!(peaks
            .windows(2)
            .all(|w| (w[0].frame, w[0].bin) <= (w[1].frame, w[1].bin)));
    }

    #[test]
    fn hashes_respect_fan_out_and_target_zone() {
        let peaks: Vec<Peak> = (0..200)
            .map(|i| Peak {
                frame: i * 2,
                bin: 50 + (i % 40) as u16,
                db: 0.0,
            })
            .collect();
        let hashes = hashes(&peaks);
        assert!(hashes.len() <= peaks.len() * FAN_OUT);
        for h in &hashes {
            let dt = h.hash & 0x3FFF;
            assert!((MIN_DT..=MAX_DT).contains(&dt));
        }
    }
}
