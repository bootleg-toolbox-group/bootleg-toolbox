//! Traces of a previous Opus encoding (YouTube, streaming), found with Opus's own transform.
//!
//! For music, Opus codes with CELT: each 20 ms frame is 960 MDCT lines (48 kHz, a window that
//! overlaps the next frame by 120 samples only) in 21 bands, and the shape of each band is a
//! PVQ codeword, a vector of small integers scaled to the band's energy (big bands are split
//! in halves, each with its own codeword). Decoded, its de-emphasis undone and analysed again
//! with the same MDCT at the same position, the bands of the frames where CELT's pitch
//! post-filter was off fall back onto small integers, which genuine audio never does (a few
//! per cent of the frames in music; none in any genuine file tested). Other rates are first
//! resampled to 48 kHz: a good resampler keeps the audio band, and the codewords with it.

use crate::dsp::Fft;

const N: usize = 960;
const OVERLAP: usize = 120;
/// CELT's de-emphasis coefficient (the decoder's last filter).
const PREEMPHASIS: f64 = 0.850_006_103_5;
/// Band edges in MDCT lines (CELT's 2.5 ms bands, times 8 for 20 ms frames).
const BANDS: [usize; 22] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 272, 320, 384, 480, 624, 800];
/// Largest codeword tried, and how close a band piece must come to it.
const K_MAX: usize = 128;
const TOLERANCE: f64 = 0.003;
/// A frame is clean when this many bands (of 21) are codewords.
const CLEAN_BANDS: usize = 12;
/// At most this much audio is analysed (from the start).
const MAX_SECONDS: usize = 60;
/// Found: this many clean frames, and this share of the frames.
pub const MIN_CLEAN: usize = 10;
pub const MIN_SHARE: f64 = 0.005;

/// The result for a file.
#[derive(Clone, Copy, Debug)]
pub struct Trace {
    /// Frames whose bands are Opus codewords (in the better of mid and side).
    pub clean: usize,
    pub frames: usize,
    /// Position of the Opus frames at 48 kHz (0-959).
    pub offset: usize,
}

impl Trace {
    pub fn share(&self) -> f64 {
        if self.frames == 0 { 0.0 } else { self.clean as f64 / self.frames as f64 }
    }

    pub fn found(&self) -> bool {
        self.clean >= MIN_CLEAN && self.share() >= MIN_SHARE
    }
}

/// Collects the first minute of the mid and side channels, at the file's rate.
pub struct Collector {
    channels: usize,
    rate: u32,
    mid: Vec<f32>,
    side: Vec<f32>,
    max: usize,
}

impl Collector {
    /// None for the rates it cannot bring to 48 kHz (44.1, 48, 88.2 and 96 kHz only).
    pub fn new(channels: usize, rate: u32) -> Option<Collector> {
        matches!(rate, 44100 | 48000 | 88200 | 96000).then(|| Collector { channels: channels.min(2), rate, mid: Vec::new(), side: Vec::new(), max: rate as usize * MAX_SECONDS })
    }

    /// One sample frame (one value per channel, -1.0..1.0).
    pub fn push(&mut self, values: &[f64]) {
        if self.mid.len() >= self.max {
            return;
        }
        let (l, r) = if self.channels == 2 { (values[0], values[1]) } else { (values[0], values[0]) };
        self.mid.push((l + r) as f32);
        self.side.push((l - r) as f32);
    }

    /// Runs the detector: None when there is too little audio.
    pub fn detect(self) -> Option<Trace> {
        let to48 = |x: &[f32]| -> Vec<f64> {
            let x: Vec<f64> = x.iter().map(|&v| v as f64).collect();
            let mut y = if self.rate == 48000 { x } else { resample_to_48k(&x, self.rate) };
            // Undo the de-emphasis: y[n] - c y[n-1], from the end.
            for i in (1..y.len()).rev() {
                y[i] -= PREEMPHASIS * y[i - 1];
            }
            y
        };
        let mid = to48(&self.mid);
        let side = if self.channels == 2 { to48(&self.side) } else { Vec::new() };
        if mid.len() < 40 * N {
            return None;
        }
        let mut mdct = Mdct::new();
        let offset = align(&mut mdct, &mid, &side);
        let count = |x: &[f64], mdct: &mut Mdct| -> (usize, usize) {
            if x.is_empty() {
                return (0, 0);
            }
            let (mut clean, mut frames) = (0, 0);
            let mut out = vec![0f64; N];
            let mut start = offset + N / 2;
            while start + 2 * N <= x.len() {
                mdct.forward(&x[start..start + 2 * N], &mut out);
                start += N;
                let loud = BANDS.windows(2).filter(|b| energy(&out[b[0]..b[1]]) > 1e-14).count();
                if loud < CLEAN_BANDS {
                    continue;
                }
                frames += 1;
                if clean_bands(&out) >= CLEAN_BANDS {
                    clean += 1;
                }
            }
            (clean, frames)
        };
        let (mc, mf) = count(&mid, &mut mdct);
        let (sc, sf) = count(&side, &mut mdct);
        let (clean, frames) = if sc > mc { (sc, sf) } else { (mc, mf) };
        Some(Trace { clean, frames, offset })
    }
}

fn energy(x: &[f64]) -> f64 {
    x.iter().map(|v| v * v).sum()
}

/// The bands that are codewords, whole or piece by piece; stops as soon as `CLEAN_BANDS`
/// can no longer be reached.
fn clean_bands(x: &[f64]) -> usize {
    let bands = BANDS.len() - 1;
    let mut clean = 0;
    for (i, b) in BANDS.windows(2).enumerate() {
        if clean + (bands - i) < CLEAN_BANDS {
            break;
        }
        if band_ok(&x[b[0]..b[1]]) {
            clean += 1;
        }
    }
    clean
}

/// A band is a codeword as a whole, or its halves are, or its quarters... (pieces of 4 or more).
fn band_ok(band: &[f64]) -> bool {
    let mut size = band.len();
    while size >= 4 {
        if band.chunks(size).all(|piece| codeword_error(piece) < TOLERANCE) {
            return true;
        }
        if size % 2 == 1 {
            return false;
        }
        size /= 2;
    }
    false
}

/// How far a piece is from the nearest direction of a small-integer vector (|sum| <= K_MAX).
fn codeword_error(piece: &[f64]) -> f64 {
    let norm = energy(piece).sqrt();
    if norm < 1e-12 {
        return 0.0;
    }
    let l1: f64 = piece.iter().map(|v| v.abs()).sum::<f64>() / norm;
    let mut best = f64::INFINITY;
    for k in 1..=K_MAX {
        let scale = k as f64 / l1;
        let (mut dot, mut yy) = (0.0, 0.0);
        for &v in piece {
            let y = (v / norm * scale).round();
            dot += y * v / norm;
            yy += y * y;
        }
        if yy == 0.0 {
            continue;
        }
        // |v - y/|y||^2 = 2 - 2 v.y/|y| for a unit v.
        let e = (2.0 - 2.0 * dot / yy.sqrt()).max(0.0).sqrt();
        if e < best {
            best = e;
            if best < TOLERANCE {
                break;
            }
        }
    }
    best
}

/// The frame position: where the band above 20 kHz, which CELT leaves empty, is emptiest;
/// the best candidates are then compared by their clean frames (added noise can fill that
/// band).
fn align(mdct: &mut Mdct, mid: &[f64], side: &[f64]) -> usize {
    const PROBE: usize = 8;
    let mut out = vec![0f64; N];
    let mut leaks: Vec<(f64, usize)> = (0..N)
        .map(|offset| {
            let mut leak = 0.0;
            for x in [mid, side].into_iter().filter(|x| !x.is_empty()) {
                let mut ratios = Vec::with_capacity(PROBE);
                for f in 1..=PROBE {
                    let start = offset + N / 2 + f * N;
                    if start + 2 * N > x.len() {
                        break;
                    }
                    mdct.forward(&x[start..start + 2 * N], &mut out);
                    let total = energy(&out);
                    if total > 0.0 {
                        ratios.push(energy(&out[800..]) / total);
                    }
                }
                leak += crate::dsp::median(&ratios).unwrap_or(1.0);
            }
            (leak, offset)
        })
        .collect();
    leaks.sort_by(|a, b| a.0.total_cmp(&b.0));
    // The best few by leak, by their clean frames over the first frames.
    let mut best = (0, leaks[0].1);
    for &(_, offset) in leaks.iter().take(6) {
        let mut clean = 0;
        for x in [mid, side].into_iter().filter(|x| !x.is_empty()) {
            for f in 1..=300 {
                let start = offset + N / 2 + f * N;
                if start + 2 * N > x.len() {
                    break;
                }
                mdct.forward(&x[start..start + 2 * N], &mut out);
                if clean_bands(&out) >= CLEAN_BANDS {
                    clean += 1;
                }
            }
        }
        if clean > best.0 {
            best = (clean, offset);
        }
    }
    best.1
}

/// CELT's MDCT for 20 ms frames: 2N windowed samples to N lines,
/// X[k] = sum w[n] x[n] cos(pi/N (n + 1/2 + N/2)(k + 1/2)), by folding to a DCT-IV of N points,
/// itself computed with an N/2-point complex DFT (Bluestein, on power-of-two FFTs).
struct Mdct {
    window: Vec<f64>,
    dft: Bluestein,
    re: Vec<f64>,
    im: Vec<f64>,
    folded: Vec<f64>,
}

impl Mdct {
    fn new() -> Mdct {
        let rise: Vec<f64> = (0..OVERLAP)
            .map(|i| {
                let s = (0.5 * std::f64::consts::PI * (i as f64 + 0.5) / OVERLAP as f64).sin();
                (0.5 * std::f64::consts::PI * s * s).sin()
            })
            .collect();
        let zeros = (N - OVERLAP) / 2;
        let mut window = vec![0.0; zeros];
        window.extend(&rise);
        window.extend(std::iter::repeat_n(1.0, N - OVERLAP));
        window.extend(rise.iter().rev());
        window.extend(std::iter::repeat_n(0.0, zeros));
        Mdct { window, dft: Bluestein::new(N / 2), re: vec![0.0; N / 2], im: vec![0.0; N / 2], folded: vec![0.0; N] }
    }

    fn forward(&mut self, input: &[f64], out: &mut [f64]) {
        let w = |i: usize| input[i] * self.window[i];
        // TDAC folding of the quarters (a, b, c, d) to (-c_r - d, a - b_r).
        let h = N / 2;
        for n in 0..h {
            self.folded[n] = -w(3 * h - 1 - n) - w(3 * h + n);
            self.folded[h + n] = w(n) - w(N - 1 - n);
        }
        dct4(&self.folded, out, &mut self.dft, &mut self.re, &mut self.im);
    }
}

/// DCT-IV of N points, X[k] = sum u[n] cos(pi/N (n + 1/2)(k + 1/2)), with an N/2-point complex DFT.
fn dct4(u: &[f64], out: &mut [f64], dft: &mut Bluestein, re: &mut [f64], im: &mut [f64]) {
    let n = u.len();
    let h = n / 2;
    let pi = std::f64::consts::PI;
    // z[m] = (u[2m] + i u[n-1-2m]) * exp(-i pi (4m + 1) / (4n))
    for m in 0..h {
        let (a, b) = (u[2 * m], u[n - 1 - 2 * m]);
        let t = -pi * (4 * m + 1) as f64 / (4 * n) as f64;
        let (c, s) = (t.cos(), t.sin());
        re[m] = a * c - b * s;
        im[m] = a * s + b * c;
    }
    dft.run(re, im);
    // Z[k] * exp(-i pi k / n): real part -> X[2k], minus imaginary -> X[n-1-2k].
    for k in 0..h {
        let t = -pi * k as f64 / n as f64;
        let (c, s) = (t.cos(), t.sin());
        let (zr, zi) = (re[k], im[k]);
        let yr = zr * c - zi * s;
        let yi = zr * s + zi * c;
        out[2 * k] = yr;
        out[n - 1 - 2 * k] = -yi;
    }
}

/// A complex DFT of any length, through power-of-two FFTs (chirp z-transform).
struct Bluestein {
    n: usize,
    fft: Fft,
    chirp: Vec<(f64, f64)>,
    kernel_re: Vec<f64>,
    kernel_im: Vec<f64>,
    a_re: Vec<f64>,
    a_im: Vec<f64>,
}

impl Bluestein {
    fn new(n: usize) -> Bluestein {
        let m = (2 * n - 1).next_power_of_two();
        let fft = Fft::new(m);
        let pi = std::f64::consts::PI;
        // w[k] = exp(-i pi k^2 / n), k^2 taken modulo 2n for precision.
        let chirp: Vec<(f64, f64)> = (0..n)
            .map(|k| {
                let a = -pi * ((k * k) % (2 * n)) as f64 / n as f64;
                (a.cos(), a.sin())
            })
            .collect();
        let (mut kernel_re, mut kernel_im) = (vec![0.0; m], vec![0.0; m]);
        for k in 0..n {
            let (c, s) = chirp[k];
            kernel_re[k] = c;
            kernel_im[k] = -s;
            if k > 0 {
                kernel_re[m - k] = c;
                kernel_im[m - k] = -s;
            }
        }
        fft.run(&mut kernel_re, &mut kernel_im);
        Bluestein { n, fft, chirp, kernel_re, kernel_im, a_re: vec![0.0; m], a_im: vec![0.0; m] }
    }

    fn run(&mut self, re: &mut [f64], im: &mut [f64]) {
        let m = self.a_re.len();
        self.a_re.iter_mut().for_each(|v| *v = 0.0);
        self.a_im.iter_mut().for_each(|v| *v = 0.0);
        for k in 0..self.n {
            let (c, s) = self.chirp[k];
            self.a_re[k] = re[k] * c - im[k] * s;
            self.a_im[k] = re[k] * s + im[k] * c;
        }
        self.fft.run(&mut self.a_re, &mut self.a_im);
        for k in 0..m {
            let (ar, ai) = (self.a_re[k], self.a_im[k]);
            let (br, bi) = (self.kernel_re[k], self.kernel_im[k]);
            // Product, conjugated for the inverse transform done with the forward one.
            self.a_re[k] = ar * br - ai * bi;
            self.a_im[k] = -(ar * bi + ai * br);
        }
        self.fft.run(&mut self.a_re, &mut self.a_im);
        let scale = 1.0 / m as f64;
        for k in 0..self.n {
            let (cr, ci) = (self.a_re[k] * scale, -self.a_im[k] * scale);
            let (c, s) = self.chirp[k];
            re[k] = cr * c - ci * s;
            im[k] = cr * s + ci * c;
        }
    }
}

/// Resamples to 48 kHz with a windowed-sinc polyphase filter (Kaiser, flat to 20 kHz).
fn resample_to_48k(x: &[f64], rate: u32) -> Vec<f64> {
    let gcd = |mut a: usize, mut b: usize| {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    };
    let g = gcd(48000, rate as usize);
    let (up, down) = (48000 / g, rate as usize / g);
    // Cut-off at the lower of the two Nyquist rates, a little below it, in input samples.
    let cutoff = 0.5 * (48000.0f64 / rate as f64).min(1.0) * 0.955;
    const HALF: usize = 96;
    let beta = 10.0;
    let bessel = |v: f64| {
        let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
        while term > 1e-16 * sum {
            term *= (v / (2.0 * k)).powi(2);
            sum += term;
            k += 1.0;
        }
        sum
    };
    let norm = bessel(beta);
    // Taps of each phase p (output time = input time + p/up), from -HALF+1 to HALF.
    let taps: Vec<Vec<f64>> = (0..up)
        .map(|p| {
            let frac = p as f64 / up as f64;
            (0..2 * HALF)
                .map(|j| {
                    let t = j as f64 - (HALF as f64 - 1.0) - frac;
                    let sinc = if t.abs() < 1e-12 { 2.0 * cutoff } else { (2.0 * std::f64::consts::PI * cutoff * t).sin() / (std::f64::consts::PI * t) };
                    let r = t / HALF as f64;
                    let window = if r.abs() >= 1.0 { 0.0 } else { bessel(beta * (1.0 - r * r).sqrt()) / norm };
                    sinc * window
                })
                .collect()
        })
        .collect();
    let out_len = x.len() * up / down;
    let mut y = Vec::with_capacity(out_len);
    for j in 0..out_len {
        let pos = j * down;
        let (i, p) = (pos / up, pos % up);
        let h = &taps[p];
        let mut acc = 0.0;
        for (tap, coef) in h.iter().enumerate() {
            let idx = i as isize + tap as isize - (HALF as isize - 1);
            if idx >= 0 && (idx as usize) < x.len() {
                acc += x[idx as usize] * coef;
            }
        }
        y.push(acc);
    }
    y
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct_mdct(input: &[f64], window: &[f64]) -> Vec<f64> {
        let pi = std::f64::consts::PI;
        (0..N)
            .map(|k| {
                (0..2 * N).map(|n| input[n] * window[n] * (pi / N as f64 * (n as f64 + 0.5 + N as f64 / 2.0) * (k as f64 + 0.5)).cos()).sum()
            })
            .collect()
    }

    #[test]
    fn mdct_matches_the_definition() {
        let mut m = Mdct::new();
        let input: Vec<f64> = (0..2 * N).map(|i| ((i * 7919) % 1000) as f64 / 500.0 - 1.0).collect();
        let mut fast = vec![0.0; N];
        m.forward(&input, &mut fast);
        let slow = direct_mdct(&input, &m.window.clone());
        let worst = fast.iter().zip(&slow).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
        assert!(worst < 1e-9, "{worst}");
    }

    #[test]
    fn codewords_and_noise() {
        // Two halves with their own scale, as CELT splits a band.
        let band: Vec<f64> = [1.0, -2.0, 0.0, 3.0, 1.0, 0.0, -1.0, 1.0].iter().map(|v| v * 0.137).chain([2.0, 1.0, -1.0, 0.0, 0.0, 4.0, -1.0, 1.0].iter().map(|v| v * 0.051)).collect();
        assert!(band_ok(&band));
        let noise: Vec<f64> = (0..16).map(|i| ((i * 7919 + 13) % 997) as f64 / 997.0 - 0.5).collect();
        assert!(!band_ok(&noise));
    }

    #[test]
    fn resampling_keeps_a_tone() {
        // A 1 kHz tone at 44.1 kHz comes out as the same tone at 48 kHz.
        let x: Vec<f64> = (0..44100).map(|i| (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44100.0).sin()).collect();
        let y = resample_to_48k(&x, 44100);
        let worst = (2000..46000).map(|i| (y[i] - (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 48000.0).sin()).abs()).fold(0.0, f64::max);
        assert!(worst < 1e-4, "{worst}");
    }
}
