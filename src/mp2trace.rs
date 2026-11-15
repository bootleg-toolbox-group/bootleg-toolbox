//! Traces of a previous MPEG-1 Layer II (MP2) encoding, found in the encoder's own filter bank.
//! Written from the description of the format (ISO/IEC 11172-3, Layer II), with the idea of the MP3
//! and AAC detectors of `mp3trace` and `aactrace`: decoded, then analysed again with the same filter
//! bank at the right position, a file falls back onto the encoder's grid.
//!
//! Layer II cuts the sound into 32 subbands with the polyphase filter bank that Layer III uses,
//! and codes 12 successive samples of each subband (a part of 384 sound samples, three parts to a
//! frame of 1 152) with one scale factor and one quantizer. The scale factors are 2^(1 - i/3),
//! i = 0 to 62: the largest sample of the part is no larger than its scale factor. A quantizer of N
//! levels (3, 5, 7, 9, 15, 31, 63 ... 65 535, always odd) rebuilds the sample as scale factor x
//! 2k/N, k an integer from -(N-1)/2 to (N-1)/2: the samples of a part sit on a regular lattice
//! of step 2 x scale factor / N, zero included. Analysed again at the position of the parts, the
//! 12 samples of a decoded part therefore fall near that lattice, in the subbands the encoder
//! coded (all of them up to 27 or 30, depending on the bit rate).
//!
//! For every position (the parts start every 384 samples, the slots of the filter bank every 32:
//! 384 positions), part, subband and candidate (a scale factor near the one the largest sample
//! asks for, a number of levels), the mean squared distance to the lattice is compared with what
//! random samples would leave (1 % of the candidates fall under the threshold by chance): the share
//! of candidates under it is the likelihood, as in `mp3trace`. Intensity stereo (joint stereo of
//! the encoders that use it) is not followed: left and right are tried on their own.

use rayon::prelude::*;

use crate::mp3trace::{erf, erfinv, Bands, Collector, Tables, Trace};

/// As for MP3: above this share of the candidates, and standing out from the other positions by
/// `MIN_CONTRAST` robust standard deviations, the file holds traces of MP2 coding.
pub const THRESHOLD: f64 = 0.025;
pub const MIN_CONTRAST: f64 = 10.0;

const FRAME: usize = 1152;
/// A part: 12 slots of the filter bank, 12 x 32 sound samples.
const PART: usize = 384;
const PART_SLOTS: usize = 12;
/// Parts tried in a window.
const PARTS: usize = 4;
/// Subbands tried: the encoders code up to the 27th or 30th.
const SUBBANDS: usize = 30;
/// The numbers of levels tried: a larger one leaves a lattice finer than the rounding of 16-bit sound.
const LEVELS: [f64; 7] = [3.0, 5.0, 7.0, 9.0, 15.0, 31.0, 63.0];
/// The scale factors tried, around the one the largest sample asks for (a scale factor shared
/// by several parts asks for a larger one): from this many steps under to one step over.
const SF_UNDER: i32 = 2;
const SF_OVER: i32 = 1;
/// A part whose largest sample is under this is silence: it says nothing.
const QUIET: f64 = 1e-5;
/// Slots computed for a window: one at every sound sample.
const WINDOW: usize = 3 * FRAME;
const SLOT_STARTS: usize = WINDOW - 512 + 1;

/// The collector of the loudest windows for this detector (3 frames of 1 152 samples).
pub fn collector(channels: usize) -> Collector {
    Collector::with(channels, FRAME, 3)
}

/// Is this a trace of MP2 coding?
pub fn found(t: &Trace) -> bool {
    t.refused.is_none() && t.likelihood > THRESHOLD && t.contrast >= MIN_CONTRAST
}

/// Candidates tried per (part, subband).
#[cfg(test)]
fn candidates() -> usize {
    (SF_UNDER + SF_OVER + 1) as usize * LEVELS.len()
}

/// The distance under which 12 samples sit on a lattice: 1 % of random ones are under it (the
/// mean of 12 squared rounding errors, mean 1/12, variance 1/180 per sample).
fn threshold() -> f64 {
    const P: f64 = 0.01;
    let mu = 1.0 / 12.0;
    let sigma = (1.0 / PART_SLOTS as f64 / 180.0).sqrt();
    mu - 2f64.sqrt() * sigma * erfinv(erf(mu / sigma / 2f64.sqrt()) - 2.0 * P)
}

/// The scale factors of the format, 2^(1 - i/3).
fn scale_factors() -> [f64; 63] {
    let mut out = [0f64; 63];
    for (i, s) in out.iter_mut().enumerate() {
        *s = libm::exp2(1.0 - i as f64 / 3.0);
    }
    out
}

/// (candidates under the threshold, candidates tried) for the 12 samples of a subband in a part.
fn part_hits(x: &[f64; PART_SLOTS], sf: &[f64; 63], tau: f64) -> (usize, usize) {
    let largest = x.iter().fold(0f64, |m, v| m.max(v.abs()));
    if largest < QUIET {
        return (0, 0);
    }
    // The smallest scale factor that is no smaller than the largest sample.
    let ask = libm::ceil(3.0 * (1.0 - libm::log2(largest)) - 1e-9) as i32;
    let (mut hits, mut total) = (0usize, 0usize);
    for i in (ask - SF_UNDER)..=(ask + SF_OVER) {
        let scale = sf[i.clamp(0, 62) as usize];
        for &n in &LEVELS {
            let inv = n / (2.0 * scale);
            let error = x.iter().map(|v| (v * inv - (v * inv).round_ties_even()).powi(2)).sum::<f64>() / PART_SLOTS as f64;
            hits += usize::from(error < tau);
            total += 1;
        }
    }
    (hits, total)
}

/// Runs the detector (32, 44.1 and 48 kHz: the rates of MPEG-1).
pub fn detect(collector: Collector, rate: u32) -> Option<Trace> {
    let bands = Bands::for_rate(rate)?;
    let channels = collector.channels;
    let segments = collector.segments();
    if segments.len() < 2 {
        return None;
    }
    let tables = Tables::new(&bands);
    let modes: &[fn([f32; 2]) -> f64] = if channels == 2 { &[|f| f[0] as f64, |f| f[1] as f64] } else { &[|f| f[0] as f64] };
    // The filter bank's output at every sound sample of every window and channel.
    let slots: Vec<Vec<[f64; 32]>> = modes
        .iter()
        .flat_map(|&take| segments.iter().map(move |seg| (take, seg)))
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|(take, seg)| {
            let x: Vec<f64> = seg.iter().map(|f| take(*f)).collect();
            (0..SLOT_STARTS).map(|s| tables.slot(&x[s..s + 512])).collect()
        })
        .collect();
    let (sf, tau) = (scale_factors(), threshold());
    // Per position: the rate of each channel over all windows, and the best rate in each window.
    let rates: Vec<(Vec<f64>, Vec<f64>)> = (0..PART)
        .into_par_iter()
        .map(|offset| {
            let mut per_window = vec![0f64; segments.len()];
            let pooled = slots
                .chunks(segments.len())
                .map(|per_segment| {
                    let (mut hits, mut total) = (0usize, 0usize);
                    for (w, window) in per_segment.iter().enumerate() {
                        let (mut h, mut t) = (0usize, 0usize);
                        for part in 0..PARTS {
                            let first = offset + part * PART;
                            // The 12 samples of each subband, slot after slot.
                            let mut columns = [[0f64; PART_SLOTS]; SUBBANDS];
                            for j in 0..PART_SLOTS {
                                for (column, v) in columns.iter_mut().zip(&window[first + 32 * j]) {
                                    column[j] = *v;
                                }
                            }
                            for column in &columns {
                                let (ph, pt) = part_hits(column, &sf, tau);
                                h += ph;
                                t += pt;
                            }
                        }
                        hits += h;
                        total += t;
                        if t > 0 {
                            per_window[w] = per_window[w].max(h as f64 / t as f64);
                        }
                    }
                    if total > 0 { hits as f64 / total as f64 } else { 0.0 }
                })
                .collect();
            (pooled, per_window)
        })
        .collect();
    let mut best = Trace::default();
    let mut per_offset = vec![0f64; PART];
    for (offset, (row, _)) in rates.iter().enumerate() {
        for &likelihood in row.iter() {
            per_offset[offset] = per_offset[offset].max(likelihood);
            if likelihood > best.likelihood {
                best = Trace { likelihood, offset, mid_side: false, ..Default::default() };
            }
        }
    }
    // The other positions (away from the best one) give the level of chance.
    let mut others: Vec<f64> = (0..PART).filter(|o| o.abs_diff(best.offset) > 2).map(|o| per_offset[o]).collect();
    others.sort_by(f64::total_cmp);
    let median = others[others.len() / 2];
    let mut dev: Vec<f64> = others.iter().map(|v| (v - median).abs()).collect();
    dev.sort_by(f64::total_cmp);
    let sigma = (1.4826 * dev[dev.len() / 2]).max(1e-4);
    best.contrast = (best.likelihood - median) / sigma;
    (best.median, best.sigma, best.second) = (median, sigma, others[others.len() - 1]);
    best.windows = segments.len().min(16) as u8;
    for w in 0..best.windows as usize {
        let mut top = (0usize, f64::NEG_INFINITY);
        for (offset, r) in rates.iter().enumerate() {
            if r.1[w] > top.1 {
                top = (offset, r.1[w]);
            }
        }
        best.window_best[w] = top.0 as u16;
    }
    Some(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A part rebuilt from a quantizer: scale factor 2^(1 - i/3), N levels.
    fn lattice(i: usize, n: f64, seed: u64) -> [f64; PART_SLOTS] {
        let scale = scale_factors()[i];
        let mut state = seed;
        let mut x = [0f64; PART_SLOTS];
        let top = ((n - 1.0) / 2.0) as i64;
        for v in x.iter_mut() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let k = ((state >> 33) as i64 % (2 * top + 1)) - top;
            *v = scale * 2.0 * k as f64 / n;
        }
        // The sample that makes the scale factor the one asked for.
        x[0] = scale * (n - 1.0) / n * if top > 0 { 1.0 } else { 0.0 };
        x
    }

    #[test]
    fn scale_factors_are_the_formats() {
        let sf = scale_factors();
        assert_eq!(sf[0], 2.0);
        assert!((sf[3] - 1.0).abs() < 1e-12);
        assert!((sf[1] - 1.5874010519681994).abs() < 1e-12);
        assert!((sf[62] - 2.0 * libm::exp2(-62.0 / 3.0)).abs() < 1e-18);
    }

    #[test]
    fn the_threshold_leaves_one_percent_of_noise() {
        // The threshold is under the mean error of random samples, 1/12.
        let tau = threshold();
        assert!(tau > 0.02 && tau < 1.0 / 12.0, "{tau}");
        // Random samples (a different seed each) fall under it about 1 % of the time per candidate.
        let sf = scale_factors();
        let (mut hits, mut total) = (0usize, 0usize);
        let mut state = 12345u64;
        for _ in 0..4000 {
            let mut x = [0f64; PART_SLOTS];
            for v in x.iter_mut() {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                *v = ((state >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 0.3;
            }
            let (h, t) = part_hits(&x, &sf, tau);
            hits += h;
            total += t;
        }
        let rate = hits as f64 / total as f64;
        assert!(rate > 0.003 && rate < 0.03, "{rate}");
    }

    #[test]
    fn a_part_on_the_lattice_is_found() {
        let (sf, tau) = (scale_factors(), threshold());
        for (i, n) in [(10usize, 3.0), (17, 5.0), (25, 15.0), (30, 31.0)] {
            let (h, t) = part_hits(&lattice(i, n, i as u64), &sf, tau);
            assert!(h >= 1 && t == candidates(), "{i} {n}: {h}/{t}");
        }
    }

    #[test]
    fn silence_says_nothing() {
        assert_eq!(part_hits(&[0.0; PART_SLOTS], &scale_factors(), threshold()), (0, 0));
    }
}
