//! "Lossless?" check: spectral analysis looking for the traces that lossy
//! encoders, FM broadcasts and upsampling leave behind (same method and
//! thresholds as `analyse_source` in the Python reference).

use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::audio::AudioInfo;
use crate::dsp::{self, Fft};
use crate::ops::Level;

pub struct Verdict {
    /// Short text for the Source column.
    pub label: String,
    /// Explanation for the Result column.
    pub sentence: String,
    pub level: Level,
    /// Measurements, for the tooltip.
    pub details: Vec<String>,
    /// The full analysis ("See more").
    pub report: Report,
}

impl Verdict {
    fn new(label: impl Into<String>, sentence: impl Into<String>, level: Level, details: Vec<String>) -> Self {
        Verdict { label: label.into(), sentence: sentence.into(), level, details, report: Report::default() }
    }
}

pub use crate::report::{Measure, Report};

pub const FM_15K: &str = "FM (15 kHz)";
pub const LOSSY_UPSTREAM: &str = "Lossy upstream";
pub const LOSSLESS_UPSAMPLED: &str = "Lossless (upsampled)";
pub const LOSSLESS_PADDED: &str = "Lossless (16-bit in 24)";

fn lossy_bitrate_hint(cutoff: f64) -> &'static str {
    for (limit, hint) in [
        (15500.0, "96 kbps or less"),
        (17500.0, "about 128 kbps"),
        (18400.0, "about 160 kbps"),
        (19300.0, "about 192 kbps"),
        (19900.0, "about 256 kbps"),
    ] {
        if cutoff < limit {
            return hint;
        }
    }
    "about 320 kbps"
}

/// Python's round(): halves go to the even neighbour.
fn round_py(x: f64) -> usize {
    x.round_ties_even().max(0.0) as usize
}

pub fn analyse(path: &Path, info: &AudioInfo, cancel: &AtomicBool) -> Result<Verdict, String> {
    let rate = info.rate as f64;
    if rate <= 0.0 {
        return Err("invalid sample rate".into());
    }
    let channels = info.channels.max(1) as usize;
    // About 11 Hz per bin.
    let nfft = 1usize << ((rate / 11.0).log2().round_ties_even() as u32).max(12);
    let fft = Fft::new(nfft);
    let window = dsp::hann_symmetric(nfft);
    let bins = nfft / 2 + 1;
    let mut power = vec![0f64; bins];
    let mut frames = 0usize;
    let mut block = Vec::with_capacity(nfft);
    let mut silent = true;
    let (mut re, mut im) = (Vec::new(), Vec::new());
    let mut frame_power = vec![0f64; bins];
    // A 24-bit file whose lowest 8 bits are always zero holds 16-bit audio.
    let check_low = info.bits == 24 && !info.float;
    let width = info.bits.div_ceil(8) as usize;
    let mut low_bits = false;
    // ~100 Hz bands
    let bin_hz = rate / nfft as f64;
    let band = round_py(100.0 / bin_hz).max(1);
    let band_freq = |b: usize| (b * band) as f64 * bin_hz + (band - 1) as f64 * bin_hz / 2.0;
    // Per-frame power of the bands between 7 and 22 kHz, to look for encoder holes and
    // for the noise floor above a cutoff; at most MAX_KEPT frames spread over the track.
    const MAX_KEPT: u64 = 20000;
    let kept_lo = (0..bins / band).find(|&b| band_freq(b) >= 7000.0).unwrap_or(bins / band);
    let kept_hi = (0..bins / band).find(|&b| band_freq(b) >= 22000.0).unwrap_or(bins / band).max(kept_lo);
    let stride = (info.samples / nfft as u64).div_ceil(MAX_KEPT).max(1) as usize;
    let mut kept: Vec<f32> = Vec::new();
    // Stereo width: the difference channel (L-R) against the sum, 5-10 kHz, on the kept frames.
    let mut side_block = Vec::with_capacity(nfft);
    let mut side_power = vec![0f64; bins];
    let mut side_vs_mid: Vec<f64> = Vec::new();
    let width_band = |f: f64| (5000.0..10000.0).contains(&f);

    dsp::for_each_frame(path, info, cancel, |values, bytes| {
        if check_low && !low_bits {
            low_bits = bytes.chunks_exact(width).any(|s| dsp::int_sample(s) & 0xFF != 0);
        }
        let mono = values.iter().sum::<f64>() / channels as f64;
        if mono != 0.0 {
            silent = false;
        }
        block.push(mono);
        if channels == 2 {
            side_block.push((values[0] - values[1]) / 2.0);
        }
        if block.len() == nfft {
            // Blocks of digital silence are skipped.
            if !silent {
                for (x, w) in block.iter_mut().zip(&window) {
                    *x *= w;
                }
                fft.power(&block, &mut re, &mut im, &mut frame_power);
                for (p, f) in power.iter_mut().zip(&frame_power) {
                    *p += f;
                }
                if frames % stride == 0 {
                    for b in kept_lo..kept_hi {
                        kept.push((frame_power[b * band..(b + 1) * band].iter().sum::<f64>() / band as f64) as f32);
                    }
                    if channels == 2 {
                        for (x, w) in side_block.iter_mut().zip(&window) {
                            *x *= w;
                        }
                        fft.power(&side_block, &mut re, &mut im, &mut side_power);
                        let sum = |p: &[f64]| (0..bins).filter(|&k| width_band(k as f64 * bin_hz)).map(|k| p[k]).sum::<f64>();
                        let (s, m) = (sum(&side_power), sum(&frame_power));
                        if m > 0.0 {
                            side_vs_mid.push(10.0 * ((s + 1e-30) / m).log10());
                        }
                    }
                }
                frames += 1;
            }
            side_block.clear();
            block.clear();
            silent = true;
        }
    })?;

    let mut details = Vec::new();
    let padded = (check_low && !low_bits).then_some("24-bit file holding 16-bit audio: the lowest 8 bits are always zero");
    if frames == 0 {
        let mut verdict = Verdict::new("Silent", "Digital silence only, nothing to analyse", Level::Info, details);
        verdict.report = Report {
            heading: "Lossless analysis".into(),
            title: crate::files::file_name(path),
            label: verdict.label.clone(),
            level: verdict.level,
            sentence: verdict.sentence.clone(),
            reasoning: vec!["Every sample is zero: there is no audio to analyse.".into()],
            ..Default::default()
        };
        return Ok(verdict);
    }

    for p in power.iter_mut() {
        *p /= frames as f64;
    }
    let freq = |k: usize| k as f64 * bin_hz;
    let raw: Vec<f64> = power.iter().map(|p| 10.0 * (p + 1e-30).log10()).collect();
    let median = |s: &[f64]| dsp::median(s).unwrap_or(f64::NEG_INFINITY);

    // Narrow tones: how far bin k stands above the median of +-1 kHz around it.
    let prominence = |k: usize| {
        let (span, skip) = ((1000.0 / bin_hz) as usize, (150.0 / bin_hz).ceil() as usize);
        let around: Vec<f64> =
            (k.saturating_sub(span)..(k + span + 1).min(bins)).filter(|&j| j + skip < k || j > k + skip).map(|j| raw[j]).collect();
        raw[k] - median(&around)
    };
    let strongest = |lo: f64, hi: f64| {
        (0..bins).filter(|&k| freq(k) >= lo && freq(k) <= hi).map(|k| (prominence(k), freq(k))).max_by(|a, b| a.0.total_cmp(&b.0))
    };
    // 19 kHz stereo pilot tone left over from an FM broadcast (test of the reference)...
    let near: Vec<f64> = (0..bins).filter(|&k| freq(k) > 18000.0 && freq(k) < 20000.0).map(|k| raw[k]).collect();
    let peak = (0..bins).filter(|&k| freq(k) > 18950.0 && freq(k) < 19050.0).map(|k| raw[k]).fold(f64::NEG_INFINITY, f64::max);
    let pilot_on_19k = !near.is_empty() && peak.is_finite() && peak - median(&near) > 15.0;
    // ...or shifted by a tape running too fast or too slow (it is 19 000 Hz exactly on air),
    // which then measures the speed error; a stricter threshold away from 19 kHz.
    let shifted_pilot = if pilot_on_19k || rate / 2.0 < 20500.0 {
        None
    } else {
        let span = 19000.0 * crate::speed::PILOT_SPAN;
        strongest(19000.0 - span, 19000.0 + span)
            .filter(|(p, f)| *p >= crate::speed::SHIFTED_PILOT_DB && (f - 19000.0).abs() > 60.0)
            .map(|(_, f)| f)
    };
    let fm_pilot = pilot_on_19k || shifted_pilot.is_some();
    // TV line frequency (15 625 Hz in Europe, 15 734 Hz in America): TV simulcast or a TV set nearby.
    let line_whistle = strongest(15560.0, 15800.0)
        .filter(|(p, f)| *p >= 15.0 && ((f - 15625.0).abs() <= 40.0 || (f - 15734.0).abs() <= 40.0))
        .map(|(_, f)| f);
    let mut tones: Vec<f64> = line_whistle.into_iter().collect();
    if pilot_on_19k {
        tones.push(19000.0);
    }
    tones.extend(shifted_pilot);

    let nb = bins / band;
    let mut level: Vec<f64> =
        (0..nb).map(|b| 10.0 * (power[b * band..(b + 1) * band].iter().sum::<f64>() / band as f64 + 1e-30).log10()).collect();
    let fb: Vec<f64> = (0..nb).map(|b| (b * band..(b + 1) * band).map(freq).sum::<f64>() / band as f64).collect();
    // Tones would fake a cutoff right above them: their bands take the level of the bands
    // just below them (not above: a tone may sit right under a real cutoff).
    let in_tone = |f: f64| tones.iter().any(|t| (f - t).abs() <= 200.0);
    let toned: Vec<usize> = (0..nb).filter(|&b| in_tone(fb[b])).collect();
    for &b in &toned {
        let around: Vec<f64> = (b.saturating_sub(12)..b).filter(|&j| !in_tone(fb[j])).map(|j| level[j]).collect();
        if let Some(m) = dsp::median(&around) {
            level[b] = m;
        }
    }
    let bw = bin_hz * band as f64;
    let nyquist = rate / 2.0;
    let median_between = |lo: f64, hi: f64| {
        let sel: Vec<f64> = (0..nb).filter(|&i| fb[i] >= lo && fb[i] < hi).map(|i| level[i]).collect();
        dsp::median(&sel)
    };

    let reference = median_between(1000.0, 4000.0);

    // Steepest drop above 10 kHz: lossy encoders cut with a brick wall a few hundred Hz wide.
    let gap = round_py(150.0 / bw).max(1);
    let width_bands = round_py(600.0 / bw).max(2);
    let (mut best_drop, mut best_i) = (0.0f64, None);
    for i in (width_bands + gap)..nb.saturating_sub(width_bands + gap) {
        if fb[i] < 10000.0 || fb[i] > nyquist - 700.0 {
            continue;
        }
        let drop = median(&level[i - gap - width_bands..i - gap]) - median(&level[i + gap..i + gap + width_bands]);
        if drop > best_drop {
            best_drop = drop;
            best_i = Some(i);
        }
    }
    let mut cutoff = None;
    if let Some(bi) = best_i {
        if best_drop >= 20.0 {
            let below = median(&level[bi - gap - width_bands..bi - gap]);
            let above = &level[bi + gap..];
            if dsp::percentile(above, 90.0).unwrap_or(f64::INFINITY) <= below - 15.0 {
                let middle = (below + median(above)) / 2.0;
                let lo = (bi + 1).saturating_sub(gap + 2);
                let hi = (bi + gap + 2).min(nb);
                cutoff = Some((lo..hi).find(|&j| level[j] < middle).map(|j| fb[j]).unwrap_or(fb[bi]));
            }
        }
    }
    match cutoff {
        Some(c) => details.push(format!("Steep lowpass at {:.1} kHz ({:.0} dB drop)", c / 1000.0, best_drop)),
        None => details.push(format!("No brick-wall lowpass between 10 kHz and {:.1} kHz", (nyquist - 700.0) / 1000.0)),
    }

    // Around a cutoff, what tells an encoder from an FM broadcast (tuned on real FM shows):
    // - encoder holes: perceptual encoders switch the top bands off and on from frame to
    //   frame, so just below the cutoff the level often collapses; FM and analog do not;
    // - the floor above the cutoff: an FM or analog capture keeps its hiss there up to the
    //   converter's filter, a decoded MP3/AAC falls to digital silence.
    let kept_bands = kept_hi - kept_lo;
    let series = |lo: f64, hi: f64| -> Vec<f64> {
        // Tones (and 18.9-19.1 kHz, where a weak pilot may hide) are not part of the floor.
        let sel: Vec<usize> = (kept_lo..kept_hi)
            .filter(|&b| {
                let f = band_freq(b);
                f >= lo && f < hi && !(18900.0..19100.0).contains(&f) && !in_tone(f)
            })
            .map(|b| b - kept_lo)
            .collect();
        if sel.is_empty() || kept_bands == 0 {
            return Vec::new();
        }
        kept.chunks_exact(kept_bands)
            .map(|frame| 10.0 * (sel.iter().map(|&i| frame[i] as f64).sum::<f64>() / sel.len() as f64 + 1e-30).log10())
            .collect()
    };
    // "Digital silence": the rounding noise of 16-bit audio, per bin. Hiss from an analog
    // source stays well above it; a decoded MP3/AAC falls to it (or below, in 24-bit).
    let quant_bits = 16;
    let lsb = 10.0 * ((2f64.powi(1 - quant_bits as i32)).powi(2) / 12.0 * window.iter().map(|w| w * w).sum::<f64>()).log10();
    let traces = cutoff.map(|c| {
        let reference_band = series(c - 3000.0, c - 1500.0);
        let below: Vec<f64> = series(c - 800.0, c - 200.0).iter().zip(&reference_band).map(|(b, r)| b - r).collect();
        let holes = match (dsp::median(&below), dsp::percentile(&below, 5.0)) {
            (Some(m), Some(p)) => m - p,
            _ => 0.0,
        };
        let above_floor = dsp::percentile(&series(c + 600.0, (c + 2600.0).min(nyquist - 300.0)), 5.0).map(|p| p - lsb);
        let high_floor = dsp::percentile(&series(18000.0, 19500.0), 5.0).map(|p| p - lsb);
        details.push(format!("Encoder holes just below the cutoff: {holes:.0} dB"));
        if let Some(a) = above_floor {
            details.push(format!("Floor above the cutoff: {a:+.0} dB over digital silence"));
        }
        if let Some(h) = high_floor.filter(|_| c < 18000.0) {
            details.push(format!("Floor at 18-19.5 kHz: {h:+.0} dB over digital silence"));
        }
        (holes, above_floor, high_floor)
    });
    // Is the floor above the cutoff steady hiss, or does it follow the music?
    let hiss = cutoff.and_then(|c| {
        let above = series(c + 800.0, (c + 4000.0).min(nyquist - 300.0));
        let music = series((c - 5000.0).max(7000.0), c - 2000.0);
        if above.len() < 20 || above.len() != music.len() {
            return None;
        }
        let (q, l) = (dsp::percentile(&music, 10.0)?, dsp::percentile(&music, 90.0)?);
        let pick = |keep: &dyn Fn(f64) -> bool| -> Vec<f64> { above.iter().zip(&music).filter(|(_, m)| keep(**m)).map(|(a, _)| *a).collect() };
        let quiet = dsp::median(&pick(&|m| m <= q))? - lsb;
        let loud = dsp::median(&pick(&|m| m >= l))? - lsb;
        let (ma, mm) = (above.iter().sum::<f64>() / above.len() as f64, music.iter().sum::<f64>() / music.len() as f64);
        let cov: f64 = above.iter().zip(&music).map(|(a, m)| (a - ma) * (m - mm)).sum();
        let var = |v: &[f64], mean: f64| v.iter().map(|x| (x - mean).powi(2)).sum::<f64>();
        let corr = cov / (var(&above, ma) * var(&music, mm)).sqrt().max(1e-12);
        Some((quiet, loud, corr))
    });
    let holes = traces.map(|t| t.0).unwrap_or(0.0);
    let encoder_holes = holes >= 15.0;
    // Encoder holes, yet steady hiss above the cutoff: the hiss was added after the encoder,
    // so this is a capture (FM, analog line) of a program already coded upstream (a radio's
    // digital link or internet stream), not a lossy file converted back to lossless.
    let hiss_above = traces.is_some_and(|(_, a, _)| a.is_some_and(|a| a >= 10.0));
    let upstream = |c: f64, details: Vec<String>| {
        Verdict::new(
            format!("{LOSSY_UPSTREAM} {:.1} kHz", c / 1000.0),
            format!(
                "Lossless capture of a lossy-coded program: encoder holes below the {:.1} kHz lowpass, but steady hiss above it, so the coding happened before an analog stage (a radio's digital link or internet stream), not by converting a lossy file back",
                c / 1000.0
            ),
            Level::Warn,
            details,
        )
    };
    // Hiss above the cutoff and no holes: not an encoder's lowpass.
    let analog_above = traces.is_some_and(|(holes, a, _)| holes < 8.0 && a.is_some_and(|a| a >= 10.0));
    let silent_above = traces.is_some_and(|(_, a, h)| a.is_some_and(|a| a < 3.0) || h.is_some_and(|h| h < 3.0));
    let fm_like = traces.is_some_and(|(holes, a, h)| holes <= 8.0 && a.is_some_and(|a| a >= 10.0) && h.is_some_and(|h| h >= 10.0));

    let hf = median_between(15000.0, 19500.0);
    if let (Some(r), Some(h)) = (reference, hf) {
        details.push(format!("15-19.5 kHz level: {:.0} dB relative to 1-4 kHz", h - r));
    }

    let speed = shifted_pilot.map(|f| {
        let percent = (f / 19000.0 - 1.0) * 100.0;
        let semitones = 12.0 * (f / 19000.0).log2();
        format!(
            "playback {:.1} % too {} (pitch {:.2} semitone {})",
            percent.abs(),
            if percent < 0.0 { "slow" } else { "fast" },
            semitones.abs(),
            if semitones < 0.0 { "low" } else { "high" }
        )
    });
    if pilot_on_19k {
        details.push("19 kHz FM stereo pilot tone found".into());
    } else if let (Some(f), Some(s)) = (shifted_pilot, &speed) {
        details.push(format!("19 kHz FM stereo pilot tone found at {:.2} kHz: {s}", f / 1000.0));
    }
    if let Some(f) = line_whistle {
        details.push(format!("Tone at {:.2} kHz: TV line frequency (TV simulcast, or a TV set near the recorder)", f / 1000.0));
    }

    // Upsampling: real content stops around 20-24 kHz in a hi-res file
    let mut upsampled = false;
    if rate >= 88200.0 {
        if let (Some(low), Some(high)) = (median_between(18500.0, 20500.0), median_between(27000.0, 30000.0)) {
            let above27: Vec<f64> = (0..nb).filter(|&i| fb[i] >= 27000.0).map(|i| level[i]).collect();
            details.push(format!("Level 27-30 kHz vs 18.5-20.5 kHz: {:.0} dB", high - low));
            if low - high >= 40.0 && dsp::percentile(&above27, 90.0).unwrap_or(f64::INFINITY) <= high + 15.0 {
                upsampled = true;
            }
        }
    }
    if let Some(p) = padded {
        details.push(p.into());
    }

    let khz = |c: f64| format!("{:.1}", c / 1000.0);
    let mut verdict = if fm_pilot {
        let sentence = match &speed {
            Some(s) => format!("FM broadcast recording (19 kHz pilot tone found, shifted: {s}): lossless capture of an analog source"),
            None => "FM broadcast recording (19 kHz pilot tone found): lossless capture of an analog source".into(),
        };
        Verdict::new("FM broadcast", sentence, Level::Ok, details)
    } else if let Some(c) = cutoff.filter(|c| (14500.0..=15800.0).contains(c)) {
        // The FM band stops at 15 kHz: the reference could only say "FM or MP3" here.
        if fm_like {
            Verdict::new(
                FM_15K,
                format!(
                    "FM broadcast: the lowpass at {} kHz looks lossy at first sight, but it is the FM limit; hiss goes on above it and no encoder holes: lossless capture",
                    khz(c)
                ),
                Level::Ok,
                details,
            )
        } else if encoder_holes && hiss_above {
            upstream(c, details)
        } else if encoder_holes {
            Verdict::new(
                format!("Lossy {} kHz", khz(c)),
                format!(
                    "Lossy source likely: lowpass at {} kHz with encoder holes below it (bands switched off and on): an MP3/AAC, even if the show was broadcast on FM",
                    khz(c)
                ),
                Level::Error,
                details,
            )
        } else if silent_above {
            Verdict::new(
                "15 kHz lowpass",
                format!(
                    "Steep lowpass at {} kHz with digital silence above it: a lossy encoder (MP3/AAC) or a digital filter, not a plain FM capture",
                    khz(c)
                ),
                Level::Warn,
                details,
            )
        } else {
            Verdict::new(
                "15 kHz lowpass",
                format!("Steep lowpass at {} kHz: typical of an FM broadcast, but also of a low-bitrate MP3", khz(c)),
                Level::Warn,
                details,
            )
        }
    } else if let Some(c) = cutoff.filter(|c| *c < 20500.0) {
        // Encoder holes are the encoder's fingerprint. A clean brick wall alone may also be
        // a digital lowpass filter (some remasters): then only a doubt, in orange.
        if holes >= 15.0 && hiss_above {
            upstream(c, details)
        } else if holes >= 10.0 {
            Verdict::new(
                format!("Lossy {} kHz", khz(c)),
                format!(
                    "Lossy source (high confidence): brick-wall lowpass at {} kHz with encoder holes below it, typical of an MP3/AAC at {}",
                    khz(c),
                    lossy_bitrate_hint(c)
                ),
                Level::Error,
                details,
            )
        } else {
            let above = if silent_above { "nothing above it" } else { "hiss still above it" };
            Verdict::new(
                format!("Lossy? {} kHz", khz(c)),
                format!(
                    "Possibly lossy: brick-wall lowpass at {} kHz with {above}, as an MP3/AAC at {} leaves; but no encoder holes, and a digital lowpass filter (some remasters) looks the same",
                    khz(c),
                    lossy_bitrate_hint(c)
                ),
                Level::Warn,
                details,
            )
        }
    } else if let Some(c) = cutoff.filter(|c| *c < 21500.0 && rate <= 48000.0 && analog_above) {
        Verdict::new(
            "Lossless",
            format!("No sign of lossy encoding: the lowpass at {} kHz is the converter's anti-alias filter (hiss goes on above it, no encoder holes)", khz(c)),
            Level::Ok,
            details,
        )
    } else if let Some(c) = cutoff.filter(|c| *c < 21500.0 && rate <= 48000.0) {
        Verdict::new(
            format!("Unsure {} kHz", khz(c)),
            format!(
                "Possible lossy source: lowpass at {} kHz matches an MP3 at 320 kbps, but some recorders' anti-alias filters also cut there",
                khz(c)
            ),
            Level::Warn,
            details,
        )
    } else if upsampled {
        // Only reached without a lossy lowpass. Unlike the reference, say so, and keep
        // the 20.5-21.5 kHz doubt (MP3 at 320 kbps) that hi-res rates otherwise skip.
        let origin = format!(
            "no real content above about 22-24 kHz, so this {} kHz file comes from a 44.1 or 48 kHz source",
            crate::audio::trim_float(rate / 1000.0)
        );
        match cutoff.filter(|c| *c < 21500.0) {
            Some(c) => Verdict::new(
                "Unsure (upsampled)",
                format!(
                    "Upsampled, and possibly lossy: lowpass at {} kHz matches an MP3 at 320 kbps, but some recorders' anti-alias filters also cut there; {origin}",
                    khz(c)
                ),
                Level::Warn,
                details,
            ),
            None => Verdict::new(
                LOSSLESS_UPSAMPLED,
                format!("Lossless, but upsampled: no sign of lossy encoding; {origin}"),
                Level::Warn,
                details,
            ),
        }
    } else if let Some(p) = padded {
        Verdict::new(
            LOSSLESS_PADDED,
            format!("Lossless, but not true 24-bit: no sign of lossy encoding; {p}"),
            Level::Warn,
            details,
        )
    } else if matches!((reference, hf), (Some(r), Some(h)) if h - r < -60.0) {
        Verdict::new(
            "Unclear",
            "Inconclusive: almost no content above 15 kHz (common with audience and analog-tape recordings), so encoder traces cannot be seen",
            Level::Info,
            details,
        )
    } else {
        Verdict::new(
            "Lossless",
            "No sign of lossy encoding or upsampling: no encoder lowpass, high frequencies present",
            Level::Ok,
            details,
        )
    };
    if padded.is_some() && verdict.label != LOSSLESS_PADDED {
        verdict.sentence.push_str("; also 16-bit audio padded to 24-bit");
        if verdict.level == Level::Ok {
            verdict.level = Level::Warn;
        }
    }
    let facts = Facts {
        rate,
        cutoff,
        drop: best_drop,
        holes: traces.map(|t| t.0),
        above_floor: traces.and_then(|t| t.1),
        high_floor: traces.and_then(|t| t.2),
        hiss,
        hf: reference.zip(hf).map(|(r, h)| h - r),
        pilot_on_19k,
        shifted_pilot,
        speed,
        line_whistle,
        silent_above,
        stereo: dsp::median(&side_vs_mid),
        upsampled,
        padded: padded.is_some(),
    };
    verdict.report = build_report(crate::files::file_name(path), &verdict, &facts);
    Ok(verdict)
}

/// Everything measured, for the report.
struct Facts {
    rate: f64,
    cutoff: Option<f64>,
    drop: f64,
    holes: Option<f64>,
    above_floor: Option<f64>,
    high_floor: Option<f64>,
    /// Floor above the cutoff in quiet and loud passages, and its correlation with the music.
    hiss: Option<(f64, f64, f64)>,
    /// 15-19.5 kHz against 1-4 kHz.
    hf: Option<f64>,
    pilot_on_19k: bool,
    shifted_pilot: Option<f64>,
    speed: Option<String>,
    line_whistle: Option<f64>,
    silent_above: bool,
    /// Difference channel against the sum, 5-10 kHz.
    stereo: Option<f64>,
    upsampled: bool,
    padded: bool,
}

fn build_report(title: String, verdict: &Verdict, f: &Facts) -> Report {
    let khz = |c: f64| format!("{:.1} kHz", c / 1000.0);
    let mut measures = Vec::new();
    let mut add = |name: &str, value: String, meaning: &str, level: Level| {
        measures.push(Measure { name: name.into(), value, meaning: meaning.into(), level });
    };
    let nyquist = f.rate / 2.0;
    match f.cutoff {
        Some(c) => add(
            "Highest frequencies",
            format!("steep lowpass at {} ({:.0} dB drop)", khz(c), f.drop),
            "FM stereo stops at 15 kHz; MP3/AAC encoders cut between about 15 and 20 kHz; a converter's anti-alias filter sits around 20-22 kHz; analog tape rolls off gently instead of cutting.",
            Level::Warn,
        ),
        None => add(
            "Highest frequencies",
            format!("no brick-wall lowpass up to {}", khz(nyquist - 700.0)),
            "Lossy encoders cut the top of the spectrum with a brick wall; there is none here.",
            Level::Ok,
        ),
    }
    if let Some(h) = f.hf {
        add(
            "Content above 15 kHz",
            format!("{h:.0} dB relative to 1-4 kHz"),
            "Below about -60 dB there is too little treble to see encoder traces (common with audience and cassette recordings).",
            if h < -60.0 { Level::Warn } else { Level::Info },
        );
    }
    if let Some(h) = f.holes {
        add(
            "Encoder holes below the cutoff",
            format!("{h:.0} dB"),
            "How deep the band just below the cutoff collapses in some frames. Perceptual encoders (MP3, MP2, AAC) switch these bands off and on: 10 dB and more. FM and analog sources stay under about 5 dB.",
            if h >= 10.0 { Level::Error } else if h >= 8.0 { Level::Warn } else { Level::Ok },
        );
    }
    if let Some(a) = f.above_floor {
        let steady = match f.hiss {
            Some((q, l, corr)) if corr.abs() < 0.3 && (l - q).abs() < 4.0 => {
                format!(", steady (quiet passages {q:+.0} dB, loud {l:+.0} dB)")
            }
            Some((q, l, _)) => format!(", follows the music (quiet passages {q:+.0} dB, loud {l:+.0} dB)"),
            None => String::new(),
        };
        add(
            "Floor above the cutoff",
            format!("{a:+.0} dB over digital silence{steady}"),
            "A lossy file converted back has digital silence there (about 0 dB). Steady hiss well above 0 dB was added by an analog stage (tuner, sound card) after any encoder.",
            if a < 3.0 { Level::Error } else { Level::Ok },
        );
    }
    if let (Some(h), Some(c)) = (f.high_floor, f.cutoff.filter(|c| *c < 18000.0)) {
        let _ = c;
        add(
            "Floor at 18-19.5 kHz",
            format!("{h:+.0} dB over digital silence"),
            "An FM or analog capture keeps its hiss up to the converter's filter (+20 dB and more); an encoder after that stage leaves silence there.",
            if h < 3.0 { Level::Error } else { Level::Ok },
        );
    }
    if nyquist >= 20500.0 {
        let (value, level) = if f.pilot_on_19k {
            ("found at 19.00 kHz".to_string(), Level::Ok)
        } else if let (Some(p), Some(s)) = (f.shifted_pilot, &f.speed) {
            (format!("found at {:.2} kHz: {s}", p / 1000.0), Level::Ok)
        } else {
            ("not found".to_string(), Level::Info)
        };
        add(
            "19 kHz FM stereo pilot",
            value,
            "FM stereo stations transmit a 19 kHz tone; a capture that keeps it was recorded off air, and its exact frequency measures the tape speed. Tuners often filter it out, so its absence proves nothing.",
            level,
        );
    }
    if let Some(t) = f.line_whistle {
        add(
            "Tone near 15.6 kHz",
            format!("{:.2} kHz", t / 1000.0),
            "The line frequency of a TV set (15 625 Hz in Europe, 15 734 Hz in America): TV simulcast, or a TV set near the recorder.",
            Level::Info,
        );
    }
    if let Some(s) = f.stereo {
        add(
            "Stereo width",
            if s <= 0.0 {
                format!("difference channel (L-R) {:.0} dB below the sum", -s)
            } else {
                format!("difference channel (L-R) {s:.0} dB above the sum")
            },
            "Concert mixes usually sit 4 to 10 dB below; 20 dB and more is nearly mono.",
            if s < -18.0 { Level::Warn } else { Level::Info },
        );
    }
    if f.rate >= 88200.0 {
        add(
            "Hi-res content",
            if f.upsampled { "nothing above 22-24 kHz".into() } else { "real content above 22-24 kHz".into() },
            "A true hi-res recording has content (or at least hiss) up there; an upsampled 44.1/48 kHz file is empty above 22-24 kHz.",
            if f.upsampled { Level::Warn } else { Level::Ok },
        );
    }
    if f.padded {
        add("Bit depth", "the lowest 8 bits are always zero".into(), "A 24-bit file holding 16-bit audio.", Level::Warn);
    }

    let reasoning = reasoning(verdict, f);
    let mut key = Vec::new();
    if let Some(c) = f.cutoff {
        key.push(format!("cut {}", khz(c)));
    }
    if let Some(h) = f.holes {
        key.push(format!("holes {h:.0} dB"));
    }
    if let Some(a) = f.above_floor {
        key.push(format!("floor {a:+.0} dB"));
    }
    if f.pilot_on_19k || f.shifted_pilot.is_some() {
        key.push("19 kHz pilot".into());
    }
    if f.cutoff.is_none() {
        if let Some(h) = f.hf {
            key.push(format!("treble {h:.0} dB"));
        }
    }
    Report {
        heading: "Lossless analysis".into(),
        title,
        label: verdict.label.clone(),
        level: verdict.level,
        sentence: verdict.sentence.clone(),
        measures,
        reasoning,
        key_facts: key.join(", "),
        tracks: Vec::new(),
    }
}

/// The steps from the measurements to the verdict.
fn reasoning(verdict: &Verdict, f: &Facts) -> Vec<String> {
    let khz = |c: f64| format!("{:.1} kHz", c / 1000.0);
    let cut = f.cutoff.map(khz).unwrap_or_default();
    let holes = f.holes.unwrap_or(0.0);
    let above = f.above_floor.unwrap_or(0.0);
    let label = verdict.label.as_str();
    let mut r: Vec<String> = Vec::new();
    if label == "FM broadcast" {
        r.push("The 19 kHz stereo pilot tone only exists on air: this was recorded from an FM broadcast.".into());
        if let Some(s) = &f.speed {
            r.push(format!("The pilot is not at exactly 19 000 Hz: the tape was played back at the wrong speed ({s}). A speed correction would put the music back in tune."));
        }
        if f.cutoff.is_none() {
            r.push("No brick-wall lowpass below the pilot: nothing points to a lossy encoder.".into());
        }
    } else if label == FM_15K {
        r.push(format!("The steep lowpass at {cut} sits at the FM limit: FM stereo carries audio up to 15 kHz only."));
        r.push(format!("There are no encoder holes below it ({holes:.0} dB; FM and analog sources stay under about 5 dB, MP3 shows 10 dB and more)."));
        r.push(format!("Hiss goes on above the cutoff ({above:+.0} dB over digital silence) and up to 19 kHz: no encoder came after the FM stage, it would have left silence."));
        r.push("So the lowpass is the FM transmitter's, not an encoder's: a lossless capture of an FM broadcast.".into());
    } else if label.starts_with(LOSSY_UPSTREAM) {
        r.push(format!("Encoder holes of {holes:.0} dB below the {cut} lowpass are the fingerprint of a perceptual encoder (MP3, MP2, AAC). FM and analog recordings stay under about 5 dB."));
        r.push(format!("Yet steady hiss ({above:+.0} dB over digital silence, the same in quiet and loud passages) fills the band above the cutoff. An encoder removes everything above its lowpass, so this hiss was added afterwards, by an analog stage (tuner, sound card)."));
        r.push("So the file was not made by converting a lossy file back: it is a lossless capture of a program that had already been lossy-coded.".into());
        let mut hints = 0;
        if f.cutoff.is_some_and(|c| c > 15800.0) && !f.pilot_on_19k && f.shifted_pilot.is_none() {
            r.push(format!("The audio goes up to {cut}, beyond the 15 kHz an FM stereo transmitter passes, and there is no 19 kHz pilot: probably not captured off air."));
            hints += 1;
        }
        if let Some(s) = f.stereo.filter(|s| *s < -18.0) {
            r.push(format!("The program is nearly mono: the difference channel is {:.0} dB below the sum (concert mixes usually sit 4 to 10 dB below).", -s));
            hints += 1;
        }
        r.push(if hints > 0 {
            "These hints point to the radio's internet stream recorded through a sound card, without proving it; a digital link inside the station (studio to transmitter) is also possible.".into()
        } else {
            "The encoder may be the station's digital link (studio to transmitter) or an internet stream recorded through a sound card.".into()
        });
    } else if label.starts_with("Lossy?") {
        r.push(format!("A brick-wall lowpass at {cut}, as MP3/AAC encoders leave."));
        r.push(format!("But no encoder holes below it ({holes:.0} dB; MP3 usually shows 10 dB and more)."));
        r.push(if f.silent_above {
            "Above the cutoff there is digital silence: nothing analog came after this filter.".into()
        } else {
            format!("Above the cutoff there is still some hiss ({above:+.0} dB over digital silence).")
        });
        r.push("A digital lowpass filter, as some remasters use, gives the same picture: lossy is possible, not proven. Compare with other versions of the show if you can.".into());
    } else if label.starts_with("Lossy") {
        r.push(format!("Encoder holes of {holes:.0} dB below the {cut} lowpass: the fingerprint of a perceptual encoder (MP3, MP2, AAC). FM and analog recordings stay under about 5 dB."));
        if f.silent_above {
            r.push("Above the cutoff there is digital silence: nothing was added after the encoder, so a lossy file was converted back to a lossless format.".into());
        } else {
            r.push(format!("Above the cutoff the floor is {above:+.0} dB over digital silence."));
        }
        if let Some(c) = f.cutoff {
            r.push(format!("A cutoff at {cut} is typical of an MP3/AAC at {}.", lossy_bitrate_hint(c)));
        }
    } else if label == "15 kHz lowpass" {
        r.push(format!("A steep lowpass at {cut}: both FM broadcasts and low-bitrate MP3s cut there."));
        r.push(format!("Encoder holes: {holes:.0} dB, not enough to prove an encoder."));
        r.push(if f.silent_above {
            "But above the cutoff (or by 18-19.5 kHz) there is digital silence, which a plain FM capture would not show: an encoder or a digital filter came after the FM stage.".into()
        } else {
            "The floor above the cutoff does not settle the question either way.".into()
        });
    } else if label.starts_with("Unsure") {
        r.push(format!("A lowpass at {cut}: an MP3 at 320 kbps cuts there, but so do the anti-alias filters of some recorders and converters."));
        r.push(format!("Encoder holes: {holes:.0} dB; floor above the cutoff: {above:+.0} dB over digital silence."));
        if label.contains("upsampled") {
            r.push("The file is also upsampled: it comes from a 44.1 or 48 kHz source.".into());
        }
    } else if label == LOSSLESS_UPSAMPLED {
        r.push("No lowpass from a lossy encoder and no encoder holes: the source is lossless.".into());
        r.push(format!("But this {} kHz file has no real content above about 22-24 kHz: it was upsampled from a 44.1 or 48 kHz source.", crate::audio::trim_float(f.rate / 1000.0)));
    } else if label == LOSSLESS_PADDED {
        r.push("No lowpass from a lossy encoder and no encoder holes: the source is lossless.".into());
        r.push("But the lowest 8 bits of every sample are zero: 16-bit audio stored in a 24-bit file.".into());
    } else if label == "Unclear" {
        r.push(format!("There is almost nothing above 15 kHz ({:.0} dB relative to 1-4 kHz).", f.hf.unwrap_or(0.0)));
        r.push("Encoder traces live up there, so neither lossy nor lossless can be shown. Audience and cassette recordings often look like this; the spectrogram and the lineage are the best clues.".into());
    } else if label == "Lossless" && f.cutoff.is_some() {
        r.push(format!("A lowpass at {cut}, but no encoder holes ({holes:.0} dB) and hiss goes on above it ({above:+.0} dB over digital silence)."));
        r.push("That is the converter's anti-alias filter, not an encoder's lowpass.".into());
    } else if label == "Lossless" {
        r.push("No brick-wall lowpass: lossy encoders always cut the top of the spectrum.".into());
        r.push(format!("High frequencies are present ({:.0} dB relative to 1-4 kHz above 15 kHz), so encoder traces would show.", f.hf.unwrap_or(0.0)));
    }
    if f.padded && label != LOSSLESS_PADDED {
        r.push("Also: the lowest 8 bits of every sample are zero (16-bit audio in a 24-bit file).".into());
    }
    if let Some(t) = f.line_whistle {
        r.push(format!("A tone at {:.2} kHz is a TV set's line frequency: the show was probably simulcast on TV, or a TV set was near the recorder.", t / 1000.0));
    }
    r
}
