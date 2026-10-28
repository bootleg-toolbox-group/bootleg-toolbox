//! Traces of a previous MP3 encoding, found in the encoder's own filter bank: a port of the
//! detector of O. Derrien, "Detection of genuine lossless audio files. Application to the
//! MPEG-AAC codec", JAES 67(3), 2019, MP3 version (MATLAB, MIT licence, CNRS/PRISM, 2018).
//!
//! An MP3 encoder splits the sound into 576 frequency lines per granule (polyphase filter
//! bank, then MDCT) and rounds them on a scaled grid. Decoded, then analysed again with the
//! same transforms at the same position, the lines fall back near that grid: in some bands
//! the rounding error is far smaller than chance allows. Every position (576), the 8
//! loudest windows, 8 scale factors, the long, start, stop and short blocks and the L, R,
//! M, S channels are tried; the share of bands with a too small error is the likelihood.

/// Above this share of bands, the file may hold traces of MP3 coding (detector's setting)...
pub const THRESHOLD: f64 = 0.025;
/// ...when the best position also stands out from the others. Measured with 16 windows:
/// genuine files 3.5 to 4.7, MP3 at 320 kbps 14 and more, lower bitrates 17 to 170.
pub const MIN_CONTRAST: f64 = 10.0;

const GRANULE: usize = 576;
const WINDOWS: usize = 16;
const SCALE_FACTORS: usize = 8;
const SF_MIN: f64 = 0.5;
const SF_MAX: f64 = 0.7;

/// The result for a file.
#[derive(Clone, Copy, Debug)]
pub struct Trace {
    /// Share of the bands whose rounding error is improbably small (genuine audio: about 1 %).
    pub likelihood: f64,
    /// Position of the MP3 granules in the file, in samples (0-575).
    pub offset: usize,
    /// The channels as the encoder coded them: left/right, or mid/side.
    pub mid_side: bool,
    /// How far the best position stands out from the others, in robust standard deviations
    /// (a genuine file only has the highest of 576 random values).
    pub contrast: f64,
}

impl Trace {
    pub fn found(&self) -> bool {
        self.likelihood > THRESHOLD && self.contrast >= MIN_CONTRAST
    }
}

/// Collects, while the file is read, the loudest windows of 2 codec frames (with the frames
/// after them that the analysis needs), on the mid channel. MP3: frames of 576 samples
/// (granules), 4 kept per window; AAC: frames of 1024, 3 kept.
pub struct Collector {
    pub(crate) channels: usize,
    frame: usize,
    /// Frames kept per window.
    frames: usize,
    /// The last 2 frames read (L, R), circular from `head`.
    ring: Vec<[f32; 2]>,
    head: usize,
    /// Running energy of the current frame and the one before.
    energy: [f64; 2],
    frame_len: usize,
    count: usize,
    /// (energy, samples collected so far)
    kept: Vec<(f64, Vec<[f32; 2]>)>,
}

impl Collector {
    /// For the MP3 detector.
    pub fn new(channels: usize) -> Self {
        Self::with(channels, GRANULE, 4)
    }

    pub(crate) fn with(channels: usize, frame: usize, frames: usize) -> Self {
        Collector { channels: channels.min(2), frame, frames, ring: vec![[0.0; 2]; 2 * frame], head: 0, energy: [0.0; 2], frame_len: 0, count: 0, kept: Vec::new() }
    }

    /// One sample frame (one value per channel, -1.0..1.0).
    pub fn push(&mut self, values: &[f64]) {
        let (l, r) = if self.channels == 2 { (values[0], values[1]) } else { (values[0], values[0]) };
        let frame = [l as f32, r as f32];
        let mid = if self.channels == 2 { (l + r) / 2.0 } else { l };
        let full = self.frames * self.frame;
        for (_, seg) in self.kept.iter_mut().filter(|(_, s)| s.len() < full) {
            seg.push(frame);
        }
        self.ring[self.head] = frame;
        self.head = (self.head + 1) % (2 * self.frame);
        self.energy[1] += mid * mid;
        self.frame_len += 1;
        if self.frame_len == self.frame {
            self.count += 1;
            // A window of the last 2 frames is complete: keep it if it is among the loudest.
            if self.count >= 2 {
                let e = self.energy[0] + self.energy[1];
                let snapshot = || {
                    let mut seg = Vec::with_capacity(full);
                    seg.extend_from_slice(&self.ring[self.head..]);
                    seg.extend_from_slice(&self.ring[..self.head]);
                    seg
                };
                let weakest = self.kept.iter().enumerate().min_by(|a, b| a.1 .0.total_cmp(&b.1 .0)).map(|(i, k)| (i, k.0));
                if self.kept.len() < WINDOWS {
                    self.kept.push((e, snapshot()));
                } else if let Some((i, _)) = weakest.filter(|(_, low)| e > *low) {
                    self.kept[i] = (e, snapshot());
                }
            }
            self.energy = [self.energy[1], 0.0];
            self.frame_len = 0;
        }
    }

    /// The windows that could be completed.
    pub(crate) fn segments(self) -> Vec<Vec<[f32; 2]>> {
        let full = self.frames * self.frame;
        self.kept.into_iter().filter(|(e, s)| *e > 0.0 && s.len() == full).map(|(_, s)| s).collect()
    }

    /// Runs the detector (44.1, 48 or 32 kHz only: the rates of MP3).
    pub fn detect(self, rate: u32) -> Option<Trace> {
        let bands = Bands::for_rate(rate)?;
        let channels = self.channels;
        let segments = self.segments();
        if segments.len() < 2 {
            return None;
        }
        let tables = Tables::new(&bands);
        let mut best = Trace { likelihood: 0.0, offset: 0, mid_side: false, contrast: 0.0 };
        let mut per_offset = vec![0f64; GRANULE];
        let mut buf = vec![0f64; 4 * GRANULE];
        for offset in 0..GRANULE {
            let modes: &[Mode] = if channels == 2 {
                &[(false, |f| f[0] as f64), (false, |f| f[1] as f64), (true, |f| (f[0] as f64 + f[1] as f64) / 2.0), (true, |f| (f[0] as f64 - f[1] as f64) / 2.0)]
            } else {
                &[(false, |f| f[0] as f64)]
            };
            for &(mid_side, take) in modes {
                let (mut hits, mut total) = (0usize, 0usize);
                for seg in &segments {
                    // granule1 starts at `offset` in the segment (granules 1 to 3 then follow).
                    for (i, f) in seg[offset..offset + 3 * GRANULE].iter().enumerate() {
                        buf[i] = take(*f);
                    }
                    let (h, t) = tables.window(&buf[..3 * GRANULE]);
                    hits += h;
                    total += t;
                }
                let likelihood = if total > 0 { hits as f64 / total as f64 } else { 0.0 };
                per_offset[offset] = per_offset[offset].max(likelihood);
                if likelihood > best.likelihood {
                    best = Trace { likelihood, offset, mid_side, contrast: 0.0 };
                }
            }
        }
        // The other positions (away from the best one) give the level of chance.
        let mut others: Vec<f64> = (0..GRANULE).filter(|o| o.abs_diff(best.offset) > 2).map(|o| per_offset[o]).collect();
        others.sort_by(f64::total_cmp);
        let median = others[others.len() / 2];
        let mut dev: Vec<f64> = others.iter().map(|v| (v - median).abs()).collect();
        dev.sort_by(f64::total_cmp);
        let sigma = (1.4826 * dev[dev.len() / 2]).max(1e-4);
        best.contrast = (best.likelihood - median) / sigma;
        Some(best)
    }
}

/// A channel to analyse: whether it is a mid/side one, and how to take it from (L, R).
type Mode = (bool, fn([f32; 2]) -> f64);

/// The scale factor bands of MP3 (first line of each band, end of the last one).
struct Bands {
    long: Vec<usize>,
    long_end: usize,
    short: Vec<usize>,
    short_end: usize,
}

impl Bands {
    fn for_rate(rate: u32) -> Option<Bands> {
        let (long, long_end, short, short_end): (&[usize], usize, &[usize], usize) = match rate {
            44100 => (
                &[1, 5, 9, 13, 17, 21, 25, 31, 37, 45, 53, 63, 75, 91, 111, 135, 163, 197, 239, 289, 343],
                418,
                &[1, 5, 9, 13, 17, 23, 31, 41, 53, 67, 85, 107],
                136,
            ),
            48000 => (
                &[1, 5, 9, 13, 17, 21, 25, 31, 37, 43, 51, 61, 73, 89, 107, 129, 157, 191, 231, 277, 331],
                384,
                &[1, 5, 9, 13, 17, 23, 29, 39, 51, 65, 81, 101],
                126,
            ),
            32000 => (
                &[1, 5, 9, 13, 17, 21, 25, 31, 37, 45, 55, 67, 83, 103, 127, 157, 195, 241, 297, 365, 449],
                550,
                &[1, 5, 9, 13, 17, 23, 31, 43, 59, 79, 105, 139],
                180,
            ),
            _ => return None,
        };
        // The tables count from 1.
        Some(Bands { long: long.iter().map(|b| b - 1).collect(), long_end, short: short.iter().map(|b| b - 1).collect(), short_end })
    }

    /// (first, last + 1) of each band.
    fn ranges(starts: &[usize], end: usize) -> Vec<(usize, usize)> {
        (0..starts.len()).map(|i| (starts[i], starts.get(i + 1).copied().unwrap_or(end))).collect()
    }
}

/// Everything computed once: windows, modulation, thresholds.
struct Tables {
    long_bands: Vec<(usize, usize)>,
    short_bands: Vec<(usize, usize)>,
    /// Error threshold of each band: genuine audio falls under it in 1 % of the cases.
    tau_long: Vec<f64>,
    tau_short: Vec<f64>,
    /// Polyphase modulation, 32 x 64.
    modulation: Vec<f64>,
    /// MDCT windows: long, start, stop (36) and short (12).
    win_long: [f64; 36],
    win_start: [f64; 36],
    win_stop: [f64; 36],
    win_short: [f64; 12],
    /// MDCT kernels: 18 x 36 and 6 x 12.
    mdct_long: Vec<f64>,
    mdct_short: Vec<f64>,
    scale: [f64; SCALE_FACTORS],
}

/// Aliasing-reduction butterflies of MP3.
const CI: [f64; 8] = [-0.6, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037];

impl Tables {
    fn new(bands: &Bands) -> Tables {
        let long_bands = Bands::ranges(&bands.long, bands.long_end);
        let short_bands = Bands::ranges(&bands.short, bands.short_end);
        let tau = |width: usize| {
            const P: f64 = 0.01;
            let mu = 1.0 / 12.0;
            let sigma = (1.0 / width as f64 / 180.0).sqrt();
            mu - 2f64.sqrt() * sigma * erfinv(erf(mu / sigma / 2f64.sqrt()) - 2.0 * P)
        };
        let tau_long = long_bands.iter().map(|(a, b)| tau(b - a)).collect();
        let tau_short = short_bands.iter().map(|(a, b)| tau((b - a) * 3)).collect();
        let mut modulation = vec![0f64; 32 * 64];
        for i in 0..32 {
            for k in 0..64 {
                modulation[i * 64 + k] = ((2 * i + 1) as f64 * (k as f64 - 16.0) * std::f64::consts::PI / 64.0).cos();
            }
        }
        let sine = |n: usize, len: usize| ((n as f64 + 0.5) * std::f64::consts::PI / len as f64).sin();
        let mut win_long = [0f64; 36];
        let mut win_start = [0f64; 36];
        let mut win_stop = [0f64; 36];
        let mut win_short = [0f64; 12];
        for (n, w) in win_long.iter_mut().enumerate() {
            *w = sine(n, 36);
        }
        for (n, w) in win_short.iter_mut().enumerate() {
            *w = sine(n, 12);
        }
        // Start: long rise, flat, short fall, zeros. Stop: the mirror.
        for n in 0..18 {
            win_start[n] = sine(n, 36);
            win_stop[n + 18] = sine(n + 18, 36);
        }
        for n in 0..6 {
            win_start[18 + n] = 1.0;
            win_start[24 + n] = sine(n + 6, 12);
            win_stop[6 + n] = sine(n, 12);
            win_stop[12 + n] = 1.0;
        }
        let kernel = |n_out: usize| {
            // MDCT of 2N samples into N lines, as the detector's FFT version computes it.
            let n2 = 2 * n_out;
            let mut k = vec![0f64; n_out * n2];
            for f in 0..n_out {
                for t in 0..n2 {
                    let phase = std::f64::consts::PI / n_out as f64 * (t as f64 + 0.5 + n_out as f64 / 2.0) * (f as f64 + 0.5);
                    k[f * n2 + t] = phase.cos();
                }
            }
            k
        };
        let mut scale = [0f64; SCALE_FACTORS];
        for (i, s) in scale.iter_mut().enumerate() {
            *s = SF_MIN + (SF_MAX - SF_MIN) * i as f64 / (SCALE_FACTORS - 1) as f64;
        }
        Tables { long_bands, short_bands, tau_long, tau_short, modulation, win_long, win_start, win_stop, win_short, mdct_long: kernel(18), mdct_short: kernel(6), scale }
    }

    /// Polyphase analysis of one granule: `input` holds the 480 samples before it and its
    /// 576 samples; returns 32 subbands x 18 samples (subband-major).
    fn polyphase(&self, input: &[f64], out: &mut [f64; 32 * 18]) {
        let mut z = [0f64; 64];
        for k in 0..18 {
            // The last 512 samples, newest first, weighted.
            let start = k * 32;
            for v in z.iter_mut() {
                *v = 0.0;
            }
            for j in 0..512 {
                let x = input[start + 511 - j] * crate::mp3trace::PQMF_WINDOW[j];
                z[j % 64] += x;
            }
            for i in 0..32 {
                let row = &self.modulation[i * 64..(i + 1) * 64];
                out[i * 18 + k] = row.iter().zip(&z).map(|(m, v)| m * v).sum();
            }
        }
    }

    fn mdct(kernel: &[f64], x: &[f64], out: &mut [f64]) {
        let n2 = x.len();
        for (f, o) in out.iter_mut().enumerate() {
            *o = kernel[f * n2..(f + 1) * n2].iter().zip(x).map(|(k, v)| k * v).sum();
        }
    }

    /// One analysis window (3 granules: the first one only feeds the filter bank):
    /// (bands with a too small rounding error, bands tried) for the block type that fits best.
    fn window(&self, signal: &[f64]) -> (usize, usize) {
        let mut sub2 = [0f64; 32 * 18];
        let mut sub3 = [0f64; 32 * 18];
        self.polyphase(&signal[GRANULE - 480..2 * GRANULE], &mut sub2);
        self.polyphase(&signal[2 * GRANULE - 480..3 * GRANULE], &mut sub3);
        // 32 subbands x 36 samples, every other sample of the odd subbands inverted.
        let mut rows = [[0f64; 36]; 32];
        for i in 0..32 {
            for k in 0..18 {
                rows[i][k] = sub2[i * 18 + k];
                rows[i][18 + k] = sub3[i * 18 + k];
            }
            if i % 2 == 1 {
                for j in (1..36).step_by(2) {
                    rows[i][j] = -rows[i][j];
                }
            }
        }
        // Long, start and stop blocks: 18 lines per subband; short: 3 x 6 lines.
        let mut spectra = [[0f64; GRANULE]; 4];
        let mut tmp = [0f64; 36];
        let mut line = [0f64; 18];
        for (b, win) in [&self.win_long, &self.win_start, &self.win_stop].into_iter().enumerate() {
            for i in 0..32 {
                for n in 0..36 {
                    tmp[n] = rows[i][n] * win[n];
                }
                Self::mdct(&self.mdct_long, &tmp, &mut line);
                spectra[b][i * 18..(i + 1) * 18].copy_from_slice(&line);
            }
        }
        // Short blocks, laid out window after window for each line (as the detector does).
        let mut short = [[0f64; 192]; 3];
        let mut s12 = [0f64; 12];
        let mut l6 = [0f64; 6];
        for i in 0..32 {
            for (w, block) in short.iter_mut().enumerate() {
                let n1 = w * 6 + 12;
                for n in 0..12 {
                    s12[n] = rows[i][n1 + n] * self.win_short[n];
                }
                Self::mdct(&self.mdct_short, &s12, &mut l6);
                block[i * 6..(i + 1) * 6].copy_from_slice(&l6);
            }
        }
        for c in 0..192 {
            for w in 0..3 {
                spectra[3][c * 3 + w] = short[w][c];
            }
        }
        // Aliasing reduction between neighbouring subbands.
        for spectrum in spectra.iter_mut() {
            let orig = *spectrum;
            for sb in 1..32 {
                for (i, ci) in CI.iter().enumerate() {
                    let norm = (1.0 + ci * ci).sqrt();
                    let (cs, ca) = (1.0 / norm, ci / norm);
                    let (a, b) = (sb * 18 - 1 - i, sb * 18 + i);
                    spectrum[a] = cs * orig[a] - ca * orig[b];
                    spectrum[b] = cs * orig[b] + ca * orig[a];
                }
            }
        }
        // Count, per block type, the bands x scale factors whose rounding error is too small.
        let mut counts = [(0usize, 0usize); 4];
        let mut band = Vec::with_capacity(GRANULE);
        for b in 0..3 {
            for (s, &(lo, hi)) in self.long_bands.iter().enumerate() {
                band.clear();
                band.extend_from_slice(&spectra[b][lo..hi]);
                counts[b].0 += self.band_hits(&band, self.tau_long[s]);
                counts[b].1 += SCALE_FACTORS;
            }
        }
        // Short: the 3 windows of each band together (the last band left out).
        let mut short_lines = [[0f64; 192]; 3];
        for c in 0..192 {
            for w in 0..3 {
                short_lines[w][c] = spectra[3][c * 3 + w];
            }
        }
        for (s, &(lo, hi)) in self.short_bands.iter().enumerate().take(self.short_bands.len() - 1) {
            band.clear();
            for c in lo..hi {
                for w in short_lines.iter() {
                    band.push(w[c]);
                }
            }
            counts[3].0 += self.band_hits(&band, self.tau_short[s]);
            counts[3].1 += SCALE_FACTORS;
        }
        *counts.iter().max_by(|a, b| (a.0 as f64 / a.1 as f64).total_cmp(&(b.0 as f64 / b.1 as f64))).unwrap_or(&(0, 0))
    }

    /// For each scale factor: is the mean squared rounding error of the band under `tau`?
    fn band_hits(&self, band: &[f64], tau: f64) -> usize {
        let max = band.iter().fold(0f64, |m, x| m.max(x.abs()));
        if max <= 0.0 || band.is_empty() {
            return 0;
        }
        let sf_max = 4.0 * max.log2() + 16.0 / 3.0 + 80.0;
        let powered: Vec<f64> = band.iter().map(|x| { let r = x.abs().sqrt(); r * r.sqrt() }).collect();
        self.scale
            .iter()
            .filter(|&&sf| {
                let gain = 2f64.powf(-3.0 / 16.0 * (sf * sf_max - 80.0));
                grid_error(&powered, gain) < tau
            })
            .count()
    }
}

/// Mean squared distance of `values × gain` (all ≥ 0) to the nearest integers: how well
/// they fit an encoder's quantisation grid.
pub(crate) fn grid_error(values: &[f64], gain: f64) -> f64 {
    // Adding then removing 2^52 rounds to the nearest integer in plain arithmetic: f64::round
    // is a call into the C library on x86-64 without SSE4.1, the bulk of the detectors' time.
    // Halves go to the even integer instead of away from zero, which leaves e² unchanged.
    const SHIFT: f64 = 4_503_599_627_370_496.0;
    let sum: f64 = values
        .iter()
        .map(|p| {
            let v = p * gain;
            let e = if v < SHIFT { (v + SHIFT) - SHIFT - v } else { 0.0 };
            e * e
        })
        .sum();
    sum / values.len() as f64
}

/// erf, to 1.2e-7 (Numerical Recipes, Chebyshev fit of erfc).
pub(crate) fn erf(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let poly = -z * z - 1.26551223
        + t * (1.00002368 + t * (0.37409196 + t * (0.09678418 + t * (-0.18628806 + t * (0.27886807 + t * (-1.13520398 + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277))))))));
    let erfc = t * poly.exp();
    if x >= 0.0 { 1.0 - erfc } else { erfc - 1.0 }
}

/// Inverse of erf: an approximation refined by Newton's method.
pub(crate) fn erfinv(y: f64) -> f64 {
    let y = y.clamp(-0.999_999_999, 0.999_999_999);
    let a = 0.147;
    let ln = (1.0 - y * y).ln();
    let first = 2.0 / (std::f64::consts::PI * a) + ln / 2.0;
    let mut x = y.signum() * ((first * first - ln / a).sqrt() - first).sqrt();
    for _ in 0..4 {
        let err = erf(x) - y;
        x -= err / (2.0 / std::f64::consts::PI.sqrt() * (-x * x).exp());
    }
    x
}

pub const PQMF_WINDOW: [f64; 512] = [
    0.0, -4.77e-07, -4.77e-07, -4.77e-07, -4.77e-07, -4.77e-07, -4.77e-07, -9.54e-07,
    -9.54e-07, -9.54e-07, -9.54e-07, -1.431e-06, -1.431e-06, -1.907e-06, -1.907e-06, -2.384e-06,
    -2.384e-06, -2.861e-06, -3.338e-06, -3.338e-06, -3.815e-06, -4.292e-06, -4.768e-06, -5.245e-06,
    -6.199e-06, -6.676e-06, -7.629e-06, -8.106e-06, -9.06e-06, -1.0014e-05, -1.1444e-05, -1.2398e-05,
    -1.3828e-05, -1.4782e-05, -1.6689e-05, -1.812e-05, -1.955e-05, -2.1458e-05, -2.3365e-05, -2.5272e-05,
    -2.7657e-05, -3.0041e-05, -3.2425e-05, -3.4809e-05, -3.767e-05, -4.0531e-05, -4.3392e-05, -4.6253e-05,
    -4.9591e-05, -5.2929e-05, -5.579e-05, -5.9605e-05, -6.2943e-05, -6.628e-05, -7.0095e-05, -7.3433e-05,
    -7.6771e-05, -8.0585e-05, -8.3923e-05, -8.7261e-05, -9.0599e-05, -9.346e-05, -9.6321e-05, -9.9182e-05,
    0.000101566, 0.000103951, 0.000105858, 0.000107288, 0.000108242, 0.000108719, 0.000108719, 0.000108242,
    0.000106812, 0.000105381, 0.00010252, 9.9182e-05, 9.5367e-05, 9.0122e-05, 8.44e-05, 7.7724e-05,
    6.9618e-05, 6.0558e-05, 5.0545e-05, 3.9577e-05, 2.718e-05, 1.3828e-05, -9.54e-07, -1.7166e-05,
    -3.4332e-05, -5.2929e-05, -7.2956e-05, -9.3937e-05, -0.000116348, -0.00014019, -0.000165462, -0.000191212,
    -0.000218868, -0.000247478, -0.000277042, -0.00030756, -0.000339031, -0.000371456, -0.000404358, -0.000438213,
    -0.000472546, -0.000507355, -0.000542164, -0.000576973, -0.000611782, -0.000646591, -0.000680923, -0.000714302,
    -0.000747204, -0.000779152, -0.000809669, -0.000838757, -0.000866413, -0.000891685, -0.000915051, -0.000935555,
    -0.000954151, -0.000968933, -0.000980854, -0.000989437, -0.000994205, -0.000995159, -0.000991821, -0.000983715,
    0.000971317, 0.000953674, 0.000930786, 0.000902653, 0.000868797, 0.00082922, 0.00078392, 0.000731945,
    0.000674248, 0.000610352, 0.000539303, 0.000462532, 0.000378609, 0.000288486, 0.000191689, 8.8215e-05,
    -2.1458e-05, -0.000137329, -0.000259876, -0.000388145, -0.000522137, -0.00066185, -0.000806808, -0.000956535,
    -0.001111031, -0.001269817, -0.001432419, -0.001597881, -0.001766682, -0.001937389, -0.002110004, -0.002283096,
    -0.002457142, -0.002630711, -0.002803326, -0.002974033, -0.00314188, -0.003306866, -0.003467083, -0.003622532,
    -0.003771782, -0.003914356, -0.004048824, -0.004174709, -0.004290581, -0.004395962, -0.004489899, -0.004570484,
    -0.004638195, -0.004691124, -0.004728317, -0.004748821, -0.004752159, -0.004737377, -0.004703045, -0.004649162,
    -0.004573822, -0.004477024, -0.004357815, -0.00421524, -0.004049301, -0.003858566, -0.003643036, -0.003401756,
    0.003134727, 0.002841473, 0.002521515, 0.002174854, 0.001800537, 0.001399517, 0.000971317, 0.000515938,
    3.3379e-05, -0.000475883, -0.001011848, -0.001573563, -0.002161503, -0.002774239, -0.003411293, -0.004072189,
    -0.004756451, -0.00546217, -0.006189346, -0.006937027, -0.007703304, -0.008487225, -0.009287834, -0.010103703,
    -0.010933399, -0.011775017, -0.012627602, -0.013489246, -0.014358521, -0.015233517, -0.016112804, -0.016994476,
    -0.017876148, -0.018756866, -0.019634247, -0.020506859, -0.021372318, -0.022228718, -0.02307415, -0.023907185,
    -0.024725437, -0.025527, -0.026310921, -0.02707386, -0.027815342, -0.028532982, -0.029224873, -0.02989006,
    -0.030526638, -0.031132698, -0.03170681, -0.03224802, -0.032754898, -0.033225536, -0.033659935, -0.03405571,
    -0.034412861, -0.034730434, -0.035007, -0.035242081, -0.0354352, -0.035586357, -0.035694122, -0.035758972,
    0.035780907, 0.035758972, 0.035694122, 0.035586357, 0.0354352, 0.035242081, 0.035007, 0.034730434,
    0.034412861, 0.03405571, 0.033659935, 0.033225536, 0.032754898, 0.03224802, 0.03170681, 0.031132698,
    0.030526638, 0.02989006, 0.029224873, 0.028532982, 0.027815342, 0.02707386, 0.026310921, 0.025527,
    0.024725437, 0.023907185, 0.02307415, 0.022228718, 0.021372318, 0.020506859, 0.019634247, 0.018756866,
    0.017876148, 0.016994476, 0.016112804, 0.015233517, 0.014358521, 0.013489246, 0.012627602, 0.011775017,
    0.010933399, 0.010103703, 0.009287834, 0.008487225, 0.007703304, 0.006937027, 0.006189346, 0.00546217,
    0.004756451, 0.004072189, 0.003411293, 0.002774239, 0.002161503, 0.001573563, 0.001011848, 0.000475883,
    -3.3379e-05, -0.000515938, -0.000971317, -0.001399517, -0.001800537, -0.002174854, -0.002521515, -0.002841473,
    0.003134727, 0.003401756, 0.003643036, 0.003858566, 0.004049301, 0.00421524, 0.004357815, 0.004477024,
    0.004573822, 0.004649162, 0.004703045, 0.004737377, 0.004752159, 0.004748821, 0.004728317, 0.004691124,
    0.004638195, 0.004570484, 0.004489899, 0.004395962, 0.004290581, 0.004174709, 0.004048824, 0.003914356,
    0.003771782, 0.003622532, 0.003467083, 0.003306866, 0.00314188, 0.002974033, 0.002803326, 0.002630711,
    0.002457142, 0.002283096, 0.002110004, 0.001937389, 0.001766682, 0.001597881, 0.001432419, 0.001269817,
    0.001111031, 0.000956535, 0.000806808, 0.00066185, 0.000522137, 0.000388145, 0.000259876, 0.000137329,
    2.1458e-05, -8.8215e-05, -0.000191689, -0.000288486, -0.000378609, -0.000462532, -0.000539303, -0.000610352,
    -0.000674248, -0.000731945, -0.00078392, -0.00082922, -0.000868797, -0.000902653, -0.000930786, -0.000953674,
    0.000971317, 0.000983715, 0.000991821, 0.000995159, 0.000994205, 0.000989437, 0.000980854, 0.000968933,
    0.000954151, 0.000935555, 0.000915051, 0.000891685, 0.000866413, 0.000838757, 0.000809669, 0.000779152,
    0.000747204, 0.000714302, 0.000680923, 0.000646591, 0.000611782, 0.000576973, 0.000542164, 0.000507355,
    0.000472546, 0.000438213, 0.000404358, 0.000371456, 0.000339031, 0.00030756, 0.000277042, 0.000247478,
    0.000218868, 0.000191212, 0.000165462, 0.00014019, 0.000116348, 9.3937e-05, 7.2956e-05, 5.2929e-05,
    3.4332e-05, 1.7166e-05, 9.54e-07, -1.3828e-05, -2.718e-05, -3.9577e-05, -5.0545e-05, -6.0558e-05,
    -6.9618e-05, -7.7724e-05, -8.44e-05, -9.0122e-05, -9.5367e-05, -9.9182e-05, -0.00010252, -0.000105381,
    -0.000106812, -0.000108242, -0.000108719, -0.000108719, -0.000108242, -0.000107288, -0.000105858, -0.000103951,
    0.000101566, 9.9182e-05, 9.6321e-05, 9.346e-05, 9.0599e-05, 8.7261e-05, 8.3923e-05, 8.0585e-05,
    7.6771e-05, 7.3433e-05, 7.0095e-05, 6.628e-05, 6.2943e-05, 5.9605e-05, 5.579e-05, 5.2929e-05,
    4.9591e-05, 4.6253e-05, 4.3392e-05, 4.0531e-05, 3.767e-05, 3.4809e-05, 3.2425e-05, 3.0041e-05,
    2.7657e-05, 2.5272e-05, 2.3365e-05, 2.1458e-05, 1.955e-05, 1.812e-05, 1.6689e-05, 1.4782e-05,
    1.3828e-05, 1.2398e-05, 1.1444e-05, 1.0014e-05, 9.06e-06, 8.106e-06, 7.629e-06, 6.676e-06,
    6.199e-06, 5.245e-06, 4.768e-06, 4.292e-06, 3.815e-06, 3.338e-06, 3.338e-06, 2.861e-06,
    2.384e-06, 2.384e-06, 1.907e-06, 1.907e-06, 1.431e-06, 1.431e-06, 9.54e-07, 9.54e-07,
    9.54e-07, 9.54e-07, 4.77e-07, 4.77e-07, 4.77e-07, 4.77e-07, 4.77e-07, 4.77e-07,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_function() {
        assert!((erf(0.5) - 0.520_499_877_8).abs() < 1e-6);
        assert!((erf(-1.2) + 0.910_313_978_2).abs() < 1e-6);
        for y in [-0.95, -0.3, 0.0, 0.42, 0.975] {
            assert!((erf(erfinv(y)) - y).abs() < 1e-9, "{y}");
        }
    }

    #[test]
    fn genuine_noise_has_no_trace() {
        // White noise: no encoder's grid anywhere.
        let mut c = Collector::new(2);
        let mut x: u64 = 12345;
        let mut next = || {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((x >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 0.5
        };
        for _ in 0..44100 * 3 {
            c.push(&[next(), next()]);
        }
        let t = c.detect(44100).unwrap();
        assert!(!t.found(), "{t:?}");
    }
}
