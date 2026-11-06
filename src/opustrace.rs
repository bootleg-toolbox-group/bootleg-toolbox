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

use rayon::prelude::*;

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
    /// Position of the Opus frames at 48 kHz (0-959), and between two samples: `phase` of
    /// `phases` (a 44.1 kHz file cut out of a longer conversion).
    pub offset: usize,
    pub phase: usize,
    pub phases: usize,
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
#[derive(Clone)]
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
        let resampler = (self.rate != 48000).then(|| Resampler::new(self.rate));
        let to64 = |x: &[f32]| -> Vec<f64> { x.iter().map(|&v| v as f64).collect() };
        let inputs = [to64(&self.mid), if self.channels == 2 { to64(&self.side) } else { Vec::new() }];
        let to48 = |x: &[f64], phase: usize| -> Vec<f64> {
            if x.is_empty() {
                return Vec::new();
            }
            let mut y = match &resampler {
                Some(r) => r.run(x, phase),
                None => x.to_vec(),
            };
            deemphasis(&mut y);
            y
        };
        let mut mid = to48(&inputs[0], 0);
        let mut side = to48(&inputs[1], 0);
        if mid.len() < 40 * N {
            return None;
        }
        let phases = resampler.as_ref().map_or(1, |r| r.down);
        let (mut offset, clean, near) = align(&mid, &side, phases > 1);
        let mut phase = 0;
        // A cut-out file: the Opus frames may lie between two samples of the 48 kHz grid.
        if let Some(r) = resampler.as_ref().filter(|r| r.down > 1 && !near.is_empty()) {
            if let Some((p, o)) = align_phases(r, &inputs, &mid, clean, &near) {
                (phase, offset) = (p, o);
                mid = to48(&inputs[0], phase);
                side = to48(&inputs[1], phase);
            }
        }
        // Every frame on its own (in parallel): (loud enough, clean).
        let count = |x: &[f64]| -> (usize, usize) {
            if x.is_empty() {
                return (0, 0);
            }
            let starts: Vec<usize> = (0..).map(|f| offset + N / 2 + f * N).take_while(|s| s + 2 * N <= x.len()).collect();
            starts
                .par_iter()
                .map_init(
                    || (Mdct::new(), vec![0f64; N]),
                    |(mdct, out), &start| {
                        mdct.forward(&x[start..start + 2 * N], out);
                        let loud = BANDS.windows(2).filter(|b| energy(&out[b[0]..b[1]]) > 1e-14).count();
                        if loud < CLEAN_BANDS {
                            (0, 0)
                        } else {
                            (usize::from(clean_bands(out) >= CLEAN_BANDS), 1)
                        }
                    },
                )
                .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
        };
        let (mc, mf) = count(&mid);
        let (sc, sf) = count(&side);
        let (clean, frames) = if sc > mc { (sc, sf) } else { (mc, mf) };
        Some(Trace { clean, frames, offset, phase, phases })
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
    #[cfg(all(target_arch = "x86_64", not(bt_no_avx2)))]
    if std::is_x86_feature_detected!("avx2") {
        #[target_feature(enable = "avx2")]
        fn avx2(piece: &[f64]) -> f64 {
            codeword_error_plain(piece)
        }
        // SAFETY: the processor has AVX2 (checked just above).
        return unsafe { avx2(piece) };
    }
    codeword_error_plain(piece)
}

/// K tried 4 at a time (side by side: each K's sums still add their terms in order), then
/// taken in order, as one after the other.
#[inline(always)]
fn codeword_error_plain(piece: &[f64]) -> f64 {
    const LANES: usize = 4;
    const _: () = assert!(K_MAX.is_multiple_of(LANES));
    let norm = energy(piece).sqrt();
    if norm < 1e-12 {
        return 0.0;
    }
    let l1: f64 = piece.iter().map(|v| v.abs()).sum::<f64>() / norm;
    let mut best = f64::INFINITY;
    for k0 in (1..=K_MAX).step_by(LANES) {
        let scale: [f64; LANES] = std::array::from_fn(|l| (k0 + l) as f64 / l1);
        let (mut dot, mut yy) = ([0f64; LANES], [0f64; LANES]);
        for &v in piece {
            let unit = v / norm;
            for l in 0..LANES {
                let y = libm::round(unit * scale[l]);
                dot[l] += y * v / norm;
                yy[l] += y * y;
            }
        }
        for l in 0..LANES {
            if yy[l] == 0.0 {
                continue;
            }
            // |v - y/|y||^2 = 2 - 2 v.y/|y| for a unit v.
            let e = (2.0 - 2.0 * dot[l] / yy[l].sqrt()).max(0.0).sqrt();
            if e < best {
                best = e;
                if best < TOLERANCE {
                    return best;
                }
            }
        }
    }
    best
}

/// The frame position: where the band above 20 kHz, which CELT leaves empty, is emptiest, or
/// where the middle bands of the loudest frames are most often codewords (a 16-bit file's
/// rounding noise can fill the empty band as much as a misaligned clean stream leaks); the best
/// candidates of both are then compared by their clean frames. Returns the offset, its clean
/// frames and, when `near` is set, the offsets whose low bands come closest to codewords.
fn align(mid: &[f64], side: &[f64], near: bool) -> (usize, usize, Vec<usize>) {
    let probe = loudest(mid, PROBE);
    let near_probe = if near { loudest(mid, NEAR_PROBE) } else { Vec::new() };
    // Every position on its own (in parallel), in order.
    let measured: Vec<(f64, usize, usize)> = (0..N)
        .into_par_iter()
        .map_init(
            || (Mdct::new(), vec![0f64; N]),
            |(mdct, out), offset| {
                let (mut leak, mut words, mut close) = (0.0, 0usize, 0usize);
                for x in [mid, side].into_iter().filter(|x| !x.is_empty()) {
                    let mut ratios = Vec::with_capacity(probe.len());
                    for &f in &probe {
                        let start = offset + N / 2 + f * N;
                        if start + 2 * N > x.len() {
                            continue;
                        }
                        mdct.forward(&x[start..start + 2 * N], out);
                        let total = energy(out);
                        if total > 0.0 {
                            ratios.push(energy(&out[800..]) / total);
                        }
                        words += middle_words(out);
                    }
                    leak += crate::dsp::median(&ratios).unwrap_or(1.0);
                    for &f in &near_probe {
                        let start = offset + N / 2 + f * N;
                        mdct.forward(&x[start..start + 2 * N], out);
                        close += BANDS[..9].windows(2).filter(|b| near_word(&out[b[0]..b[1]])).count();
                    }
                }
                (leak, words, close)
            },
        )
        .collect();
    let mut leaks: Vec<(f64, usize)> = measured.iter().enumerate().map(|(offset, m)| (m.0, offset)).collect();
    let mut codewords: Vec<(usize, usize)> = measured.iter().enumerate().map(|(offset, m)| (m.1, offset)).collect();
    leaks.sort_by(|a, b| a.0.total_cmp(&b.0));
    codewords.sort_by_key(|c| std::cmp::Reverse(c.0));
    // The best few of each, by their clean frames over the first frames.
    let mut best = (0, leaks[0].1);
    let candidates: Vec<usize> = leaks.iter().take(6).map(|l| l.1).chain(codewords.iter().take(6).map(|c| c.1)).collect();
    let cleans: Vec<usize> = candidates
        .par_iter()
        .map_init(|| (Mdct::new(), vec![0f64; N]), |(mdct, out), &offset| [mid, side].iter().map(|x| first_clean(mdct, out, x, offset)).sum())
        .collect();
    for (&offset, &clean) in candidates.iter().zip(&cleans) {
        if clean > best.0 {
            best = (clean, offset);
        }
    }
    let mut close: Vec<(usize, usize)> = measured.iter().enumerate().map(|(offset, m)| (m.2, offset)).collect();
    close.sort_by_key(|c| std::cmp::Reverse(c.0));
    // Only an offset that stands out: genuine audio stays within a few bands of the tenth best.
    let stands_out = near && close[0].0 >= close[9].0 + NEAR_MARGIN;
    let near = if stands_out { close.iter().take(NEAR_OFFSETS).map(|c| c.1).collect() } else { Vec::new() };
    (best.1, best.0, near)
}

/// Frames probed: by the alignment, and for the low bands that point to the phase search.
const PROBE: usize = 16;
const NEAR_PROBE: usize = 48;
/// Offsets tried at every phase (and their neighbours): the best by their low bands, when the
/// first has this many more bands than the tenth.
const NEAR_OFFSETS: usize = 2;
const NEAR_MARGIN: usize = 8;

/// Close to a codeword (whole piece, K up to 32, a looser tolerance): a low band stays so when
/// the frames lie a fraction of a sample away from the 48 kHz grid, where `band_ok` fails.
fn near_word(piece: &[f64]) -> bool {
    let norm = energy(piece).sqrt();
    if norm < 1e-12 {
        return false;
    }
    let l1: f64 = piece.iter().map(|v| v.abs()).sum::<f64>() / norm;
    (1..=32).any(|k| {
        let scale = k as f64 / l1;
        let (mut dot, mut yy) = (0.0, 0.0);
        for &v in piece {
            let y = libm::round(v / norm * scale);
            dot += y * v / norm;
            yy += y * y;
        }
        yy > 0.0 && (2.0 - 2.0 * dot / yy.sqrt()).max(0.0).sqrt() < 0.02
    })
}

/// Frames probed by the alignment: the loudest (a show often opens on silence, crowd or a
/// fade, where the leak above 20 kHz is only noise).
fn loudest(mid: &[f64], count: usize) -> Vec<usize> {
    let blocks = (mid.len() / N).saturating_sub(3);
    let mut loudest: Vec<(f64, usize)> = (1..blocks).map(|f| (energy(&mid[f * N..(f + 1) * N]), f)).collect();
    loudest.sort_by(|a, b| b.0.total_cmp(&a.0));
    loudest.iter().take(count).map(|&(_, f)| f).collect()
}

/// Bands 8 to 14 (16 to 32 lines) that are codewords: seldom by chance.
fn middle_words(out: &[f64]) -> usize {
    BANDS[8..16].windows(2).filter(|b| band_ok(&out[b[0]..b[1]])).count()
}

/// Clean frames among the first 300 at `offset` (`x` from its first 48 kHz sample).
fn first_clean(mdct: &mut Mdct, out: &mut [f64], x: &[f64], offset: usize) -> usize {
    let mut clean = 0;
    for f in 1..=300 {
        let start = offset + N / 2 + f * N;
        if start + 2 * N > x.len() {
            break;
        }
        mdct.forward(&x[start..start + 2 * N], out);
        if clean_bands(out) >= CLEAN_BANDS {
            clean += 1;
        }
    }
    clean
}

/// The other phases of the 48 kHz grid (a file cut out of a longer 44.1 kHz conversion starts
/// between two of the original samples): the `near` offsets and their neighbours, at every
/// phase, scored by bands 0 to 14 of the loudest frames, the best few then compared by their
/// clean frames. Some((phase, offset)) when one has more clean frames than `clean`, the best of
/// phase 0.
fn align_phases(r: &Resampler, inputs: &[Vec<f64>; 2], mid0: &[f64], clean: usize, near: &[usize]) -> Option<(usize, usize)> {
    const CANDIDATES: usize = 6;
    let probe = loudest(mid0, PROBE);
    let inputs: Vec<&[f64]> = inputs.iter().map(|x| x.as_slice()).filter(|x| !x.is_empty()).collect();
    let mut offsets: Vec<usize> = near.iter().flat_map(|&o| [(o + N - 1) % N, o, (o + 1) % N]).collect();
    offsets.sort();
    offsets.dedup();
    // Each phase on its own (in parallel), its offsets in order: (words, phase, offset).
    let mut scored: Vec<(usize, usize, usize)> = (1..r.down)
        .into_par_iter()
        .map_init(
            || (Mdct::new(), vec![0f64; N]),
            |(mdct, out), phase| {
                let mut words = vec![0usize; offsets.len()];
                for x in &inputs {
                    for &f in &probe {
                        let seg = r.range(x, phase, f * N + N / 2, f * N + N / 2 + 3 * N);
                        if seg.len() < 3 * N {
                            continue;
                        }
                        for (&offset, w) in offsets.iter().zip(words.iter_mut()) {
                            mdct.forward(&seg[offset..offset + 2 * N], out);
                            *w += BANDS[..16].windows(2).filter(|b| band_ok(&out[b[0]..b[1]])).count();
                        }
                    }
                }
                offsets.iter().zip(words).map(|(&offset, w)| (w, phase, offset)).collect::<Vec<_>>()
            },
        )
        .flatten()
        .collect();
    scored.sort_by_key(|&(w, phase, offset)| (std::cmp::Reverse(w), phase, offset));
    scored.truncate(CANDIDATES);
    let cleans: Vec<usize> = scored
        .par_iter()
        .map_init(
            || (Mdct::new(), vec![0f64; N]),
            |(mdct, out), &(_, phase, offset)| {
                // The first 300 frames, from 48 kHz sample 0 (`first_clean` counts from there).
                let end = offset + N / 2 + 302 * N;
                inputs.iter().map(|x| first_clean(mdct, out, &r.range(x, phase, 0, end), offset)).sum()
            },
        )
        .collect();
    let mut best = (clean, None);
    for (&(_, phase, offset), &c) in scored.iter().zip(&cleans) {
        if c > best.0 {
            best = (c, Some((phase, offset)));
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
    /// The DCT-IV's twiddles, before and after the DFT.
    pre: Vec<(f64, f64)>,
    post: Vec<(f64, f64)>,
}

impl Mdct {
    fn new() -> Mdct {
        let rise: Vec<f64> = (0..OVERLAP)
            .map(|i| {
                let s = libm::sin(0.5 * std::f64::consts::PI * (i as f64 + 0.5) / OVERLAP as f64);
                libm::sin(0.5 * std::f64::consts::PI * s * s)
            })
            .collect();
        let zeros = (N - OVERLAP) / 2;
        let mut window = vec![0.0; zeros];
        window.extend(&rise);
        window.extend(std::iter::repeat_n(1.0, N - OVERLAP));
        window.extend(rise.iter().rev());
        window.extend(std::iter::repeat_n(0.0, zeros));
        let pi = std::f64::consts::PI;
        let cis = |t: f64| (libm::cos(t), libm::sin(t));
        let pre = (0..N / 2).map(|m| cis(-pi * (4 * m + 1) as f64 / (4 * N) as f64)).collect();
        let post = (0..N / 2).map(|k| cis(-pi * k as f64 / N as f64)).collect();
        Mdct { window, dft: Bluestein::new(N / 2), re: vec![0.0; N / 2], im: vec![0.0; N / 2], folded: vec![0.0; N], pre, post }
    }

    fn forward(&mut self, input: &[f64], out: &mut [f64]) {
        #[cfg(all(target_arch = "x86_64", not(bt_no_avx2)))]
        if std::is_x86_feature_detected!("avx2") {
            #[target_feature(enable = "avx2")]
            fn avx2(mdct: &mut Mdct, input: &[f64], out: &mut [f64]) {
                mdct.forward_plain(input, out)
            }
            // SAFETY: the processor has AVX2 (checked just above).
            return unsafe { avx2(self, input, out) };
        }
        self.forward_plain(input, out)
    }

    #[inline(always)]
    fn forward_plain(&mut self, input: &[f64], out: &mut [f64]) {
        let w = |i: usize| input[i] * self.window[i];
        // TDAC folding of the quarters (a, b, c, d) to (-c_r - d, a - b_r).
        let h = N / 2;
        for n in 0..h {
            self.folded[n] = -w(3 * h - 1 - n) - w(3 * h + n);
            self.folded[h + n] = w(n) - w(N - 1 - n);
        }
        dct4(&self.folded, out, &mut self.dft, &mut self.re, &mut self.im, &self.pre, &self.post);
    }
}

/// DCT-IV of N points, X[k] = sum u[n] cos(pi/N (n + 1/2)(k + 1/2)), with an N/2-point complex DFT.
#[inline(always)]
fn dct4(u: &[f64], out: &mut [f64], dft: &mut Bluestein, re: &mut [f64], im: &mut [f64], pre: &[(f64, f64)], post: &[(f64, f64)]) {
    let n = u.len();
    let h = n / 2;
    // z[m] = (u[2m] + i u[n-1-2m]) * exp(-i pi (4m + 1) / (4n))
    for m in 0..h {
        let (a, b) = (u[2 * m], u[n - 1 - 2 * m]);
        let (c, s) = pre[m];
        re[m] = a * c - b * s;
        im[m] = a * s + b * c;
    }
    dft.run(re, im);
    // Z[k] * exp(-i pi k / n): real part -> X[2k], minus imaginary -> X[n-1-2k].
    for k in 0..h {
        let (c, s) = post[k];
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
                (libm::cos(a), libm::sin(a))
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

    #[inline(always)]
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

/// Undoes CELT's de-emphasis: y[n] - c y[n-1], from the end.
fn deemphasis(y: &mut [f64]) {
    for i in (1..y.len()).rev() {
        y[i] -= PREEMPHASIS * y[i - 1];
    }
}

/// Resampling to 48 kHz with a windowed-sinc polyphase filter (Kaiser, flat to 20 kHz). Output
/// sample j is taken at input time (j * down + phase) / up: a phase (0 to down - 1) moves the
/// 48 kHz grid by phase/down of a sample.
struct Resampler {
    up: usize,
    down: usize,
    /// Taps of each sub-sample position p (output time = input time + p/up), from -HALF+1 to HALF.
    taps: Vec<Vec<f64>>,
}

const HALF: usize = 96;

impl Resampler {
    fn new(rate: u32) -> Resampler {
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
        let taps = (0..up)
            .map(|p| {
                let frac = p as f64 / up as f64;
                (0..2 * HALF)
                    .map(|j| {
                        let t = j as f64 - (HALF as f64 - 1.0) - frac;
                        let sinc = if t.abs() < 1e-12 { 2.0 * cutoff } else { libm::sin(2.0 * std::f64::consts::PI * cutoff * t) / (std::f64::consts::PI * t) };
                        let r = t / HALF as f64;
                        let window = if r.abs() >= 1.0 { 0.0 } else { bessel(beta * (1.0 - r * r).sqrt()) / norm };
                        sinc * window
                    })
                    .collect()
            })
            .collect();
        Resampler { up, down, taps }
    }

    /// Output length for `n` input samples.
    fn len(&self, n: usize, phase: usize) -> usize {
        (n * self.up).saturating_sub(phase) / self.down
    }

    fn sample(&self, x: &[f64], j: usize, phase: usize) -> f64 {
        let pos = j * self.down + phase;
        let (i, p) = (pos / self.up, pos % self.up);
        let mut acc = 0.0;
        for (tap, coef) in self.taps[p].iter().enumerate() {
            let idx = i as isize + tap as isize - (HALF as isize - 1);
            if idx >= 0 && (idx as usize) < x.len() {
                acc += x[idx as usize] * coef;
            }
        }
        acc
    }

    /// The whole signal (every output sample on its own, in parallel).
    fn run(&self, x: &[f64], phase: usize) -> Vec<f64> {
        (0..self.len(x.len(), phase)).into_par_iter().map(|j| self.sample(x, j, phase)).collect()
    }

    /// Output samples `from..to` (fewer at the end of the signal), de-emphasis undone as in the
    /// whole signal.
    fn range(&self, x: &[f64], phase: usize, from: usize, to: usize) -> Vec<f64> {
        let first = from.saturating_sub(1);
        let mut y: Vec<f64> = (first..to.min(self.len(x.len(), phase))).map(|j| self.sample(x, j, phase)).collect();
        deemphasis(&mut y);
        y.split_off((from - first).min(y.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::dsp::tests::{noise, same_bits};

    #[test]
    fn kernels_plain_and_avx2_agree() {
        let (mut a, mut b) = (Mdct::new(), Mdct::new());
        let x = noise(2 * N, 3);
        let (mut out1, mut out2) = (vec![0f64; N], vec![0f64; N]);
        a.forward(&x, &mut out1);
        b.forward_plain(&x, &mut out2);
        same_bits(&out1, &out2);
        for len in [4, 8, 16, 32] {
            let piece = noise(len, len as u64);
            assert_eq!(codeword_error(&piece).to_bits(), codeword_error_plain(&piece).to_bits());
            // A codeword (small integers), found at its own K.
            let word: Vec<f64> = (0..len).map(|i| ((i * 7) % 5) as f64 - 2.0).collect();
            assert_eq!(codeword_error(&word).to_bits(), codeword_error_plain(&word).to_bits());
        }
    }

    fn direct_mdct(input: &[f64], window: &[f64]) -> Vec<f64> {
        let pi = std::f64::consts::PI;
        (0..N)
            .map(|k| {
                (0..2 * N).map(|n| input[n] * window[n] * libm::cos(pi / N as f64 * (n as f64 + 0.5 + N as f64 / 2.0) * (k as f64 + 0.5))).sum()
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
    fn a_range_is_the_whole_signal_cut() {
        // The same samples, de-emphasis included, at every phase.
        let x = noise(4000, 5);
        let r = Resampler::new(44100);
        for phase in [0, 1, 85, 146] {
            let mut whole = r.run(&x, phase);
            deemphasis(&mut whole);
            same_bits(&r.range(&x, phase, 0, 1000), &whole[..1000]);
            same_bits(&r.range(&x, phase, 1234, 2345), &whole[1234..2345]);
        }
    }

    #[test]
    fn resampling_keeps_a_tone() {
        // A 1 kHz tone at 44.1 kHz comes out as the same tone at 48 kHz.
        let x: Vec<f64> = (0..44100).map(|i| libm::sin(2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44100.0)).collect();
        let y = Resampler::new(44100).run(&x, 0);
        let worst = (2000..46000).map(|i| (y[i] - libm::sin(2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 48000.0)).abs()).fold(0.0, f64::max);
        assert!(worst < 1e-4, "{worst}");
    }
}
