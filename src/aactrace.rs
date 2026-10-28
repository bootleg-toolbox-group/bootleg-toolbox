//! Traces of a previous AAC encoding, found with the encoder's own transform: a port of the
//! AAC detector of O. Derrien (MATLAB, MIT licence, CNRS/PRISM, 2018), described in
//! "Detection of genuine lossless audio files. Application to the MPEG-AAC codec", JAES
//! 67(3), 2019. Same idea as `mp3trace`: the sound is analysed again with AAC's MDCT
//! (1024 lines, or 8 x 128 for short blocks, sine windows) at every position, and the
//! bands whose rounding error is improbably small are counted.

use crate::dsp::Fft;
use crate::mp3trace::{erf, erfinv, Collector, Trace};

const FRAME: usize = 1024;
const SHORT: usize = 128;
const SHORT_OFFSET: usize = (FRAME - SHORT) / 2;
const SCALE_FACTORS: usize = 8;
const SF_MIN: f64 = 0.3;
const SF_MAX: f64 = 0.7;

/// Above this share of bands, and when the best position stands out from the others by
/// `MIN_CONTRAST` robust standard deviations, the file holds traces of AAC coding.
pub const THRESHOLD: f64 = 0.03;
pub const MIN_CONTRAST: f64 = 10.0;

/// The collector of the loudest windows for this detector (frames of 1024 samples).
pub fn collector(channels: usize) -> Collector {
    Collector::with(channels, FRAME, 3)
}

/// Runs the detector (32, 44.1 and 48 kHz: the rates its band tables cover).
pub fn detect(collector: Collector, rate: u32) -> Option<Trace> {
    let long_starts: &[usize] = match rate {
        32000 => &[
            1, 5, 9, 13, 17, 21, 25, 29, 33, 37, 41, 49, 57, 65, 73, 81, 89, 97, 109, 121, 133, 145, 161, 177, 197, 217, 241, 265, 293, 321,
            353, 385, 417, 449, 481, 513, 545, 577, 609, 641, 673, 705, 737, 769, 801, 833, 865, 897, 929, 961, 993,
        ],
        44100 | 48000 => &[
            1, 5, 9, 13, 17, 21, 25, 29, 33, 37, 41, 49, 57, 65, 73, 81, 89, 97, 109, 121, 133, 145, 161, 177, 197, 217, 241, 265, 293, 321,
            353, 385, 417, 449, 481, 513, 545, 577, 609, 641, 673, 705, 737, 769, 801, 833, 865, 897, 929,
        ],
        _ => return None,
    };
    let short_starts: &[usize] = &[1, 5, 9, 13, 17, 21, 29, 37, 45, 57, 69, 81, 97, 113];
    let channels = collector.channels;
    let segments = collector.segments();
    if segments.len() < 2 {
        return None;
    }
    let tables = Tables::new(long_starts, short_starts);
    // Of left and right, and of mid and side, only the louder one is analysed.
    let energy = |f: fn([f32; 2]) -> f64| segments.iter().map(|s| s[..2 * FRAME].iter().map(|x| f(*x).powi(2)).sum::<f64>()).sum::<f64>();
    let left: fn([f32; 2]) -> f64 = |f| f[0] as f64;
    let right: fn([f32; 2]) -> f64 = |f| f[1] as f64;
    let mid: fn([f32; 2]) -> f64 = |f| f[0] as f64 + f[1] as f64;
    let side: fn([f32; 2]) -> f64 = |f| f[0] as f64 - f[1] as f64;
    let modes: Vec<Mode> = if channels == 2 {
        vec![(false, if energy(left) >= energy(right) { left } else { right }), (true, if energy(mid) >= energy(side) { mid } else { side })]
    } else {
        vec![(false, left)]
    };
    let mut work = Work::new();
    let mut per_offset = vec![0f64; FRAME];
    let mut best = Trace { likelihood: 0.0, offset: 0, mid_side: false, contrast: 0.0 };
    let mut buf = vec![0f64; 2 * FRAME];
    for offset in 0..FRAME {
        for &(mid_side, take) in &modes {
            let (mut hits, mut total) = (0usize, 0usize);
            for seg in &segments {
                for (i, f) in seg[offset..offset + 2 * FRAME].iter().enumerate() {
                    buf[i] = take(*f);
                }
                let (h, t) = tables.window(&buf, &mut work);
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
    let mut others: Vec<f64> = (0..FRAME).filter(|o| o.abs_diff(best.offset) > 2).map(|o| per_offset[o]).collect();
    others.sort_by(f64::total_cmp);
    let median = others[others.len() / 2];
    let mut dev: Vec<f64> = others.iter().map(|v| (v - median).abs()).collect();
    dev.sort_by(f64::total_cmp);
    let sigma = (1.4826 * dev[dev.len() / 2]).max(1e-4);
    best.contrast = (best.likelihood - median) / sigma;
    Some(best)
}

/// A channel to analyse: whether it is a mid/side one, and how to take it from (L, R).
type Mode = (bool, fn([f32; 2]) -> f64);

/// Is this a trace of AAC coding?
pub fn found(t: &Trace) -> bool {
    t.likelihood > THRESHOLD && t.contrast >= MIN_CONTRAST
}

/// An MDCT of 2N samples into N lines (the detector's definition), computed as a DCT-IV of
/// the folded input through an N/2-point complex FFT.
struct Mdct {
    n: usize,
    fft: Fft,
    /// exp(-j pi (n + 1/4) / N) and exp(-j pi n / N), n < N/2.
    pre: Vec<(f64, f64)>,
    post: Vec<(f64, f64)>,
}

impl Mdct {
    fn new(n: usize) -> Mdct {
        let pi = std::f64::consts::PI;
        let cis = |a: f64| (a.cos(), a.sin());
        let pre = (0..n / 2).map(|i| cis(-pi * (i as f64 + 0.25) / n as f64)).collect();
        let post = (0..n / 2).map(|i| cis(-pi * i as f64 / n as f64)).collect();
        Mdct { n, fft: Fft::new(n / 2), pre, post }
    }

    /// `x` (2N samples, already windowed) into `out` (N lines); `fold` holds N values, `re`
    /// and `im` N/2.
    fn run(&self, x: &[f64], fold: &mut [f64], re: &mut [f64], im: &mut [f64], out: &mut [f64]) {
        let n = self.n;
        let h = n / 2;
        // (a, b, c, d) -> (-c reversed - d, a - b reversed).
        for i in 0..h {
            fold[i] = -x[3 * h - 1 - i] - x[3 * h + i];
            fold[h + i] = x[i] - x[n - 1 - i];
        }
        for i in 0..h {
            let (a, b) = (fold[2 * i], fold[n - 1 - 2 * i]);
            let (c, s) = self.pre[i];
            re[i] = a * c - b * s;
            im[i] = a * s + b * c;
        }
        self.fft.run(&mut re[..h], &mut im[..h]);
        for i in 0..h {
            let (c, s) = self.post[i];
            let (yr, yi) = (re[i] * c - im[i] * s, re[i] * s + im[i] * c);
            out[2 * i] = yr;
            out[n - 1 - 2 * i] = -yi;
        }
    }
}

/// Kaiser-Bessel-derived window of `len` samples (AAC: alpha 4 for long blocks, 6 for short).
fn kbd(len: usize, alpha: f64) -> Vec<f64> {
    let i0 = |x: f64| {
        // Modified Bessel function of the first kind, order 0 (series).
        let (mut sum, mut term) = (1.0, 1.0);
        for k in 1..50 {
            term *= (x / 2.0) / k as f64;
            sum += term * term;
        }
        sum
    };
    let half = len / 2;
    let beta = std::f64::consts::PI * alpha;
    let kaiser: Vec<f64> = (0..=half).map(|n| i0(beta * (1.0 - (2.0 * n as f64 / half as f64 - 1.0).powi(2)).max(0.0).sqrt())).collect();
    let total: f64 = kaiser.iter().sum();
    let mut left = Vec::with_capacity(half);
    let mut acc = 0.0;
    for w in kaiser.iter().take(half) {
        acc += w;
        left.push((acc / total).sqrt());
    }
    let mut window = left.clone();
    window.extend(left.iter().rev());
    window
}

/// Buffers reused from window to window.
struct Work {
    windowed: Vec<f64>,
    fold: Vec<f64>,
    re: Vec<f64>,
    im: Vec<f64>,
    spectrum: Vec<f64>,
    short: Vec<f64>,
    band: Vec<f64>,
    powered: Vec<f64>,
}

impl Work {
    fn new() -> Work {
        Work {
            windowed: vec![0.0; 2 * FRAME],
            fold: vec![0.0; FRAME],
            re: vec![0.0; FRAME / 2],
            im: vec![0.0; FRAME / 2],
            spectrum: vec![0.0; FRAME],
            short: vec![0.0; 8 * SHORT],
            band: Vec::with_capacity(FRAME),
            powered: Vec::with_capacity(FRAME),
        }
    }
}

struct Tables {
    long_bands: Vec<(usize, usize)>,
    short_bands: Vec<(usize, usize)>,
    tau_long: Vec<f64>,
    tau_short: Vec<f64>,
    /// Long, start and stop windows (2048), sine then KBD shaped.
    windows: Vec<Vec<f64>>,
    /// Short windows (256), sine and KBD.
    shorts: Vec<Vec<f64>>,
    mdct_long: Mdct,
    mdct_short: Mdct,
    scale: [f64; SCALE_FACTORS],
}

impl Tables {
    fn new(long_starts: &[usize], short_starts: &[usize]) -> Tables {
        let ranges = |starts: &[usize], end: usize| -> Vec<(usize, usize)> {
            (0..starts.len()).map(|i| (starts[i] - 1, starts.get(i + 1).map(|s| s - 1).unwrap_or(end))).collect()
        };
        let long_bands = ranges(long_starts, FRAME);
        let short_bands = ranges(short_starts, SHORT);
        let tau = |width: usize| {
            const P: f64 = 0.01;
            let mu = 1.0 / 12.0;
            let sigma = (1.0 / width as f64 / 180.0).sqrt();
            mu - 2f64.sqrt() * sigma * erfinv(erf(mu / sigma / 2f64.sqrt()) - 2.0 * P)
        };
        let tau_long = long_bands.iter().map(|(a, b)| tau(b - a)).collect();
        let tau_short = short_bands.iter().map(|(a, b)| tau((b - a) * 8)).collect();
        let sine = |i: usize, n2: usize| (std::f64::consts::PI * (2 * i + 1) as f64 / n2 as f64 / 2.0).sin();
        // The detector only knew sine windows (iTunes); other encoders (FDK, ffmpeg) use the
        // Kaiser-Bessel-derived ones the standard also allows.
        let shapes = [
            ((0..2 * FRAME).map(|i| sine(i, 2 * FRAME)).collect::<Vec<f64>>(), (0..2 * SHORT).map(|i| sine(i, 2 * SHORT)).collect::<Vec<f64>>()),
            (kbd(2 * FRAME, 4.0), kbd(2 * SHORT, 6.0)),
        ];
        let mut windows = Vec::new();
        let mut shorts = Vec::new();
        for (long, short) in shapes {
            // Start: long rise, flat, short fall, zeros. Stop: zeros, short rise, flat, long fall.
            let mut start = vec![0f64; 2 * FRAME];
            let mut stop = vec![0f64; 2 * FRAME];
            start[..FRAME].copy_from_slice(&long[..FRAME]);
            stop[FRAME..].copy_from_slice(&long[FRAME..]);
            for i in 0..SHORT_OFFSET {
                start[FRAME + i] = 1.0;
                stop[SHORT_OFFSET + SHORT + i] = 1.0;
            }
            for i in 0..SHORT {
                start[FRAME + SHORT_OFFSET + i] = short[SHORT + i];
                stop[SHORT_OFFSET + i] = short[i];
            }
            windows.extend([long, start, stop]);
            shorts.push(short);
        }
        let mut scale = [0f64; SCALE_FACTORS];
        for (i, s) in scale.iter_mut().enumerate() {
            *s = SF_MIN + (SF_MAX - SF_MIN) * i as f64 / (SCALE_FACTORS - 1) as f64;
        }
        Tables { long_bands, short_bands, tau_long, tau_short, windows, shorts, mdct_long: Mdct::new(FRAME), mdct_short: Mdct::new(SHORT), scale }
    }

    /// One analysis window of 2048 samples: (bands with a too small rounding error, bands
    /// tried) for the block type and window shape that fit best.
    fn window(&self, signal: &[f64], w: &mut Work) -> (usize, usize) {
        let ratio = |c: (usize, usize)| if c.1 > 0 { c.0 as f64 / c.1 as f64 } else { 0.0 };
        let mut best = (0usize, 0usize);
        for window in &self.windows {
            for ((o, x), win) in w.windowed.iter_mut().zip(signal).zip(window) {
                *o = x * win;
            }
            let Work { windowed, fold, re, im, spectrum, .. } = w;
            self.mdct_long.run(windowed, fold, re, im, spectrum);
            let mut count = (0usize, 0usize);
            // The last band is left out, as the detector does.
            for (s, &(lo, hi)) in self.long_bands.iter().enumerate().take(self.long_bands.len() - 1) {
                count.0 += self.band_hits(&w.spectrum[lo..hi], &mut w.powered, self.tau_long[s]);
                count.1 += SCALE_FACTORS;
            }
            if ratio(count) > ratio(best) {
                best = count;
            }
        }
        for window in &self.shorts {
            for k in 0..8 {
                let n1 = k * SHORT + SHORT_OFFSET;
                for i in 0..2 * SHORT {
                    w.windowed[i] = signal[n1 + i] * window[i];
                }
                let Work { windowed, fold, re, im, short, .. } = w;
                self.mdct_short.run(&windowed[..2 * SHORT], &mut fold[..SHORT], &mut re[..SHORT / 2], &mut im[..SHORT / 2], &mut short[k * SHORT..(k + 1) * SHORT]);
            }
            let mut count = (0usize, 0usize);
            for (s, &(lo, hi)) in self.short_bands.iter().enumerate().take(self.short_bands.len() - 1) {
                w.band.clear();
                for k in 0..8 {
                    w.band.extend_from_slice(&w.short[k * SHORT + lo..k * SHORT + hi]);
                }
                let band = std::mem::take(&mut w.band);
                count.0 += self.band_hits(&band, &mut w.powered, self.tau_short[s]);
                count.1 += SCALE_FACTORS;
                w.band = band;
            }
            if ratio(count) > ratio(best) {
                best = count;
            }
        }
        best
    }

    fn band_hits(&self, band: &[f64], powered: &mut Vec<f64>, tau: f64) -> usize {
        let max = band.iter().fold(0f64, |m, x| m.max(x.abs()));
        if max <= 0.0 || band.is_empty() {
            return 0;
        }
        let sf_max = 4.0 * max.log2() + 16.0 / 3.0 + 60.0;
        powered.clear();
        powered.extend(band.iter().map(|x| { let r = x.abs().sqrt(); r * r.sqrt() }));
        self.scale
            .iter()
            .filter(|&&sf| {
                let gain = 2f64.powf(-3.0 / 16.0 * (sf * sf_max - 60.0));
                crate::mp3trace::grid_error(powered, gain) < tau
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kbd_window() {
        // Symmetric, and power complementary (w[n]^2 + w[n + N/2]^2 = 1), as MDCT windows must be.
        for (len, alpha) in [(2048, 4.0), (256, 6.0)] {
            let w = kbd(len, alpha);
            assert_eq!(w.len(), len);
            for n in 0..len / 2 {
                assert!((w[n] - w[len - 1 - n]).abs() < 1e-12);
                assert!((w[n].powi(2) + w[n + len / 2].powi(2) - 1.0).abs() < 1e-9, "{n}");
            }
        }
    }

    #[test]
    fn mdct_matches_the_definition() {
        let n = 16;
        let m = Mdct::new(n);
        let x: Vec<f64> = (0..2 * n).map(|i| ((i * 7 % 11) as f64 - 5.0) / 5.0).collect();
        let (mut fold, mut re, mut im, mut out) = (vec![0.0; n], vec![0.0; n / 2], vec![0.0; n / 2], vec![0.0; n]);
        m.run(&x, &mut fold, &mut re, &mut im, &mut out);
        for (f, o) in out.iter().enumerate() {
            let direct: f64 = (0..2 * n)
                .map(|t| x[t] * (std::f64::consts::PI / n as f64 * (t as f64 + 0.5 + n as f64 / 2.0) * (f as f64 + 0.5)).cos())
                .sum();
            assert!((o - direct).abs() < 1e-9, "{f}: {o} {direct}");
        }
    }
}
