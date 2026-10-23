//! Offline audio bands for exported clips, shaped like cava's live output: 50 log-spaced bands
//! from an FFT of the song's audio, with cava-style noise reduction, auto-sensitivity and wave
//! smoothing, in cava's raw range of 0 to 1000.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

pub const SAMPLE_RATE: u32 = 44_100;
pub const BANDS: usize = 50;
/// Analysis steps per second, matching cava's frame rate.
pub const RATE: f64 = 60.0;
const WINDOW: usize = 2048;
const LOW_HZ: f32 = 50.0;
const HIGH_HZ: f32 = 10_000.0;
/// cava's raw ASCII range.
const RANGE: f32 = 1000.0;
/// cava's `noise_reduction = 20`.
const NOISE_REDUCTION: f32 = 0.2;
/// Mean band level below which the input counts as silence for auto-sensitivity.
const SILENCE: f32 = 1e-4;

/// `BANDS + 1` log-spaced band edges in Hz.
pub fn band_edges() -> Vec<f32> {
    let ratio = (HIGH_HZ / LOW_HZ).powf(1.0 / BANDS as f32);
    (0..=BANDS).map(|i| LOW_HZ * ratio.powi(i as i32)).collect()
}

/// Mean FFT magnitude of each band. Bands narrower than one bin take the magnitude
/// interpolated at their centre frequency.
pub fn band_magnitudes(magnitudes: &[f32], bin_hz: f32, edges: &[f32]) -> Vec<f32> {
    edges
        .windows(2)
        .map(|e| {
            let (lo, hi) = (e[0] / bin_hz, e[1] / bin_hz);
            let first = lo.ceil() as usize;
            let last = (hi.ceil() as usize).min(magnitudes.len());
            if last > first {
                magnitudes[first..last].iter().sum::<f32>() / (last - first) as f32
            } else {
                let at = ((lo + hi) / 2.0).min(magnitudes.len().saturating_sub(1) as f32);
                let i = at.floor() as usize;
                let next = magnitudes.get(i + 1).copied().unwrap_or(0.0);
                let here = magnitudes.get(i).copied().unwrap_or(0.0);
                here + (next - here) * (at - i as f32)
            }
        })
        .collect()
}

/// cava's per-frame processing of raw band levels: gravity fall-off and integral noise
/// reduction, auto-sensitivity, scaling to the raw range and the wave filter.
pub struct Smoother {
    sens: f32,
    sens_init: bool,
    eq: Vec<f32>,
    peak: Vec<f32>,
    fall: Vec<f32>,
    prev: Vec<f32>,
    mem: Vec<f32>,
}

impl Default for Smoother {
    fn default() -> Self {
        Self::new()
    }
}

impl Smoother {
    pub fn new() -> Self {
        Self {
            sens: 1.0,
            sens_init: true,
            // Higher bands carry less energy; lift them so all of the wave moves.
            eq: (0..BANDS).map(|n| (2.0 + n as f32).log2()).collect(),
            peak: vec![0.0; BANDS],
            fall: vec![0.0; BANDS],
            prev: vec![0.0; BANDS],
            mem: vec![0.0; BANDS],
        }
    }

    /// One step at [`RATE`]: raw band magnitudes in, bars from 0 to 1000 out.
    pub fn step(&mut self, raw: &[f32]) -> Vec<f32> {
        let gravity = 1.54 / NOISE_REDUCTION;
        let silent = raw.iter().sum::<f32>() / raw.len().max(1) as f32 <= SILENCE;
        let mut overshoot = false;
        let mut out: Vec<f32> = raw
            .iter()
            .zip(&self.eq)
            .map(|(v, eq)| v * eq * self.sens)
            .collect();
        for (n, v) in out.iter_mut().enumerate() {
            if *v < self.prev[n] {
                *v = (self.peak[n] * (1.0 - self.fall[n] * self.fall[n] * gravity)).max(0.0);
                self.fall[n] += 0.028;
            } else {
                self.peak[n] = *v;
                self.fall[n] = 0.0;
            }
            self.prev[n] = *v;
            *v += self.mem[n] * NOISE_REDUCTION;
            self.mem[n] = *v;
            overshoot |= *v > 1.0;
        }
        if overshoot {
            self.sens *= 0.98;
            self.sens_init = false;
        } else if !silent {
            self.sens *= 1.001;
            if self.sens_init {
                self.sens *= 1.1;
            }
        }
        for v in &mut out {
            *v *= RANGE;
        }
        waves(&mut out);
        for v in &mut out {
            *v = v.clamp(0.0, RANGE);
        }
        out
    }
}

/// cava's wave filter: each bar lowers by a fifth and spreads to its neighbours, falling off
/// with the square of the distance.
pub fn waves(bars: &mut [f32]) {
    let n = bars.len();
    for z in 0..n {
        bars[z] /= 1.25;
        for m in 0..n {
            if m != z {
                let de = m.abs_diff(z) as f32;
                bars[m] = bars[m].max(bars[z] - de * de);
            }
        }
    }
}

/// Bands of a stretch of audio at [`RATE`] steps.
pub struct Spectrum {
    /// Song position of the first step.
    start: Duration,
    frames: Vec<Vec<f32>>,
}

impl Spectrum {
    /// Decodes `len` of `path` from song position `start` with ffmpeg and analyses it.
    pub fn decode(path: &Path, start: Duration, len: Duration) -> Result<Self, String> {
        let mut child = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-ss"])
            .arg(format!("{:.3}", start.as_secs_f64()))
            .arg("-t")
            .arg(format!("{:.3}", len.as_secs_f64()))
            .arg("-i")
            .arg(path)
            .args(["-vn", "-ac", "1", "-ar"])
            .arg(SAMPLE_RATE.to_string())
            .args(["-f", "f32le", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to start ffmpeg: {e}"))?;
        let mut bytes = Vec::new();
        if let Some(mut out) = child.stdout.take() {
            out.read_to_end(&mut bytes)
                .map_err(|e| format!("failed to read decoded audio: {e}"))?;
        }
        let status = child
            .wait()
            .map_err(|e| format!("failed to wait for ffmpeg: {e}"))?;
        if !status.success() {
            return Err(format!("ffmpeg could not decode the audio ({status})"));
        }
        let samples: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        Ok(Self::analyse(&samples, start))
    }

    /// Bands for mono `samples` at [`SAMPLE_RATE`] that begin at song position `start`.
    pub fn analyse(samples: &[f32], start: Duration) -> Self {
        let fft: Arc<dyn Fft<f32>> = FftPlanner::new().plan_fft_forward(WINDOW);
        let hann: Vec<f32> = (0..WINDOW)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (WINDOW - 1) as f32).cos())
            .collect();
        let edges = band_edges();
        let bin_hz = SAMPLE_RATE as f32 / WINDOW as f32;
        let hop = f64::from(SAMPLE_RATE) / RATE;
        let steps = (samples.len() as f64 / hop).floor() as usize;
        let mut smoother = Smoother::new();
        let mut buffer = vec![Complex::new(0.0, 0.0); WINDOW];
        let mut magnitudes = vec![0.0; WINDOW / 2];
        let frames = (0..steps)
            .map(|step| {
                // The window ends at the step's time, like cava reading the latest audio.
                let end = ((step + 1) as f64 * hop) as isize;
                for (i, slot) in buffer.iter_mut().enumerate() {
                    let at = end - WINDOW as isize + i as isize;
                    let s = if at >= 0 {
                        samples.get(at as usize).copied().unwrap_or(0.0)
                    } else {
                        0.0
                    };
                    *slot = Complex::new(s * hann[i], 0.0);
                }
                fft.process(&mut buffer);
                for (m, c) in magnitudes.iter_mut().zip(&buffer) {
                    *m = c.norm() / WINDOW as f32;
                }
                smoother.step(&band_magnitudes(&magnitudes, bin_hz, &edges))
            })
            .collect();
        Self { start, frames }
    }

    /// Bands at song position `at`: the latest step at or before it, or none outside the
    /// analysed stretch.
    pub fn at(&self, at: Duration) -> Option<&[f32]> {
        let offset = at.checked_sub(self.start)?.as_secs_f64();
        let step = (offset * RATE).floor() as usize;
        self.frames.get(step).map(Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, secs: f32) -> Vec<f32> {
        (0..(SAMPLE_RATE as f32 * secs) as usize)
            .map(|i| 0.5 * (std::f32::consts::TAU * hz * i as f32 / SAMPLE_RATE as f32).sin())
            .collect()
    }

    fn band_of(hz: f32) -> usize {
        band_edges()
            .windows(2)
            .position(|e| (e[0]..e[1]).contains(&hz))
            .expect("in range")
    }

    #[test]
    fn edges_are_log_spaced() {
        let e = band_edges();
        assert_eq!(e.len(), BANDS + 1);
        assert!((e[0] - LOW_HZ).abs() < 1e-3);
        assert!((e[BANDS] - HIGH_HZ).abs() < 1.0);
        let r0 = e[1] / e[0];
        assert!(e.windows(2).all(|w| (w[1] / w[0] - r0).abs() < 1e-3));
    }

    #[test]
    fn sine_peaks_in_its_band() {
        for hz in [120.0, 1000.0, 4000.0] {
            let samples = sine(hz, 1.0);
            let fft = FftPlanner::new().plan_fft_forward(WINDOW);
            let mut buf: Vec<Complex<f32>> = samples[..WINDOW]
                .iter()
                .map(|&s| Complex::new(s, 0.0))
                .collect();
            fft.process(&mut buf);
            let mags: Vec<f32> = buf[..WINDOW / 2].iter().map(|c| c.norm()).collect();
            let bands = band_magnitudes(&mags, SAMPLE_RATE as f32 / WINDOW as f32, &band_edges());
            let peak = bands
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(i, _)| i)
                .expect("bands");
            assert!(peak.abs_diff(band_of(hz)) <= 1, "{hz} Hz: band {peak}");
        }
    }

    #[test]
    fn analysis_settles_in_range_and_decays_in_silence() {
        let mut samples = sine(1000.0, 4.0);
        samples.extend(std::iter::repeat_n(0.0, SAMPLE_RATE as usize * 2));
        let spectrum = Spectrum::analyse(&samples, Duration::from_secs(10));
        assert_eq!(spectrum.frames.len(), 360);
        assert!(spectrum.at(Duration::from_secs(9)).is_none());
        let loud = spectrum.at(Duration::from_millis(13_500)).expect("frame");
        assert!(loud.iter().all(|v| (0.0..=RANGE).contains(v)));
        let peak = loud.iter().copied().fold(0.0, f32::max);
        // Auto-sensitivity brings a steady tone near the top of the range.
        assert!(peak > 500.0, "peak {peak}");
        let at = band_of(1000.0);
        assert!(loud[at] >= loud[0] && loud[at] >= loud[BANDS - 1]);
        let quiet = spectrum.at(Duration::from_millis(15_900)).expect("frame");
        assert!(quiet.iter().all(|&v| v < 1.0), "{quiet:?}");
    }

    #[test]
    fn waves_spread_to_neighbours() {
        let mut bars = vec![0.0; 7];
        bars[3] = 100.0;
        waves(&mut bars);
        assert_eq!(bars[3], 80.0);
        assert_eq!(bars[2], 79.0);
        assert_eq!(bars[0], 71.0);
        assert!(bars[1] < bars[2]);
    }
}
