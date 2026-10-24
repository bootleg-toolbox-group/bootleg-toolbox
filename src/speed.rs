//! "Speed?" check: is the tape running too fast or too slow, and by how much?
//! Three references, tuned on real shows (Dire Straits 1979-03-06, Black Sabbath
//! 1971/1974 in wrong-speed and speed-corrected versions):
//! - the 19 kHz FM stereo pilot: exact when present;
//! - the tuning of the instruments against A = 440 Hz: an estimate, blind to errors of
//!   a whole semitone (5.9 %) and fooled by bands tuning by ear;
//! - the 50/60 Hz mains hum: a hint, trusted only when several tracks agree.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::audio::AudioInfo;
use crate::dsp::{self, Fft};
use crate::ops::Level;
use crate::report::{Measure, Report};

/// Under this, "in tune with A = 440 Hz" (correctly running recordings measure -4 to +8 cents).
const TUNING_TOLERANCE_CENTS: f64 = 12.0;
/// Under this, the tuning is not coherent enough to say anything.
const MIN_COHERENCE: f64 = 0.08;
/// Under this, coherent enough to say "in tune", not to put a figure on a speed error
/// (Dire Straits 0.16-0.26, modern recordings 0.18-0.20, early-70s Black Sabbath 0.05-0.15).
const CLAIM_COHERENCE: f64 = 0.14;
/// A shifted FM pilot is looked for within +-4 % of 19 kHz (cassette speed errors stay
/// well within; wider, other tones such as a 20 kHz line get mistaken for it).
pub const PILOT_SPAN: f64 = 0.04;
/// Prominence a tone away from 19 000 Hz needs to be taken for a shifted pilot (Dire Straits
/// 1979: 28-35 dB; a 21 dB tone heard on a single track of another show was not one).
pub const SHIFTED_PILOT_DB: f64 = 25.0;

/// What one track tells about its speed.
#[derive(Clone, Debug, Default)]
pub struct SpeedFacts {
    /// Speed error from the FM pilot, in percent.
    pub pilot: Option<f64>,
    /// Offset of the tuning from A = 440 Hz (cents, -50..50) and its coherence (0..1).
    pub tuning: Option<(f64, f64)>,
    /// Mains hum: nominal frequency (50/60), measured frequency, prominence in dB.
    pub hum: Option<(f64, f64, f64)>,
}

pub fn cents_to_percent(cents: f64) -> f64 {
    (2f64.powf(cents / 1200.0) - 1.0) * 100.0
}

fn percent_to_cents(percent: f64) -> f64 {
    1200.0 * (1.0 + percent / 100.0).log2()
}

/// "1.1 % slow", "2.0 % fast".
fn describe(percent: f64) -> String {
    format!("{:.1} % {}", percent.abs(), if percent < 0.0 { "slow" } else { "fast" })
}

/// The tuning only knows the error modulo a semitone: the other candidates.
fn semitone_alternatives(cents: f64) -> String {
    format!(
        "If the error passed a whole semitone, it could also be {} or {}: the tuning alone cannot tell, compare by ear with other recordings of the tour.",
        describe(cents_to_percent(cents - 100.0)),
        describe(cents_to_percent(cents + 100.0))
    )
}

/// What to do about it: "speed it up by 1.1 % (play at 101.1 %, +19 cents)".
fn correction(percent: f64) -> String {
    let factor = 100.0 / (1.0 + percent / 100.0);
    format!(
        "{} it {} by {:.1} % (play at {:.1} %, {:+.0} cents)",
        if percent < 0.0 { "speed" } else { "slow" },
        if percent < 0.0 { "up" } else { "down" },
        (factor - 100.0).abs(),
        factor,
        -percent_to_cents(percent)
    )
}

pub fn analyse(path: &Path, info: &AudioInfo, cancel: &AtomicBool) -> Result<SpeedFacts, String> {
    let rate = info.rate as f64;
    if rate <= 0.0 {
        return Err("invalid sample rate".into());
    }
    let channels = info.channels.max(1) as f64;
    let pow2 = |seconds: f64| (rate * seconds).log2().round().clamp(10.0, 22.0) as u32;
    // Pilot: an average spectrum of ~5 Hz bins.
    let n_pilot = 1usize << pow2(0.186);
    // Tuning: ~2.7 Hz bins, one window every ~0.75 s.
    let n_tune = 1usize << pow2(0.37);
    let tune_every = n_tune * 2;
    // Hum: ~0.08 Hz bins, averaged over the track.
    let n_hum = 1usize << pow2(11.9);

    let (fft_pilot, fft_tune, fft_hum) = (Fft::new(n_pilot), Fft::new(n_tune), Fft::new(n_hum));
    let (w_pilot, w_tune, w_hum) = (dsp::hann_symmetric(n_pilot), dsp::hann_symmetric(n_tune), dsp::hann_symmetric(n_hum));
    let mut pilot_power = vec![0f64; n_pilot / 2 + 1];
    let mut hum_power = vec![0f64; n_hum / 2 + 1];
    let (mut pilot_blocks, mut hum_blocks) = (0usize, 0usize);
    let mut scratch = vec![0f64; n_hum.max(n_tune)];
    let mut bins = vec![0f64; n_hum / 2 + 1];
    let (mut re, mut im) = (Vec::new(), Vec::new());
    let (mut b_pilot, mut b_tune, mut b_hum) = (Vec::with_capacity(n_pilot), Vec::with_capacity(n_tune), Vec::with_capacity(n_hum));
    let mut since_tune = 0usize;
    // Tuning: sum of the peak weights on the unit circle (one turn = one semitone).
    let (mut zr, mut zi, mut zw) = (0f64, 0f64, 0f64);
    let tune_lo = (80.0 * n_tune as f64 / rate) as usize;
    let tune_hi = ((2000.0 * n_tune as f64 / rate) as usize).min(n_tune / 2 - 1);


    dsp::for_each_frame(path, info, cancel, |values, _| {
        let mono = values.iter().sum::<f64>() / channels;
        b_pilot.push(mono);
        b_hum.push(mono);
        if since_tune < n_tune {
            b_tune.push(mono);
        }
        since_tune += 1;
        if b_pilot.len() == n_pilot {
            if b_pilot.iter().any(|x| *x != 0.0) {
                accumulate(&b_pilot, &fft_pilot, &w_pilot, &mut pilot_power, &mut scratch, &mut bins, &mut re, &mut im);
                pilot_blocks += 1;
            }
            b_pilot.clear();
        }
        if b_hum.len() == n_hum {
            if b_hum.iter().any(|x| *x != 0.0) {
                accumulate(&b_hum, &fft_hum, &w_hum, &mut hum_power, &mut scratch, &mut bins, &mut re, &mut im);
                hum_blocks += 1;
            }
            b_hum.clear();
        }
        if b_tune.len() == n_tune && since_tune >= n_tune {
            for i in 0..n_tune {
                scratch[i] = b_tune[i] * w_tune[i];
            }
            fft_tune.power(&scratch[..n_tune], &mut re, &mut im, &mut bins[..n_tune / 2 + 1]);
            // Spectral peaks within 30 dB of the loudest, refined by parabolic interpolation.
            let db: Vec<f64> = bins[..n_tune / 2 + 1].iter().map(|p| 10.0 * (p + 1e-24).log10()).collect();
            let top = db[tune_lo..=tune_hi].iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            for k in tune_lo.max(1)..=tune_hi {
                let (a, b, c) = (db[k - 1], db[k], db[k + 1]);
                if b > a && b > c && b > top - 30.0 {
                    let d = 0.5 * (a - c) / (a - 2.0 * b + c);
                    let freq = (k as f64 + d) * rate / n_tune as f64;
                    let cents = 1200.0 * (freq / 440.0).log2();
                    let angle = cents / 100.0 * std::f64::consts::TAU;
                    let weight = 10f64.powf(b / 20.0);
                    zr += weight * angle.cos();
                    zi += weight * angle.sin();
                    zw += weight;
                }
            }
            b_tune.clear();
        }
        if since_tune >= tune_every {
            since_tune = 0;
        }
    })?;

    let mut facts = SpeedFacts::default();
    if zw > 0.0 {
        let cents = zi.atan2(zr) / std::f64::consts::TAU * 100.0;
        facts.tuning = Some((cents, (zr * zr + zi * zi).sqrt() / zw));
    }

    // FM pilot, as in the Lossless? check: at 19 000 Hz, or shifted by the tape speed.
    if pilot_blocks > 0 && rate >= 41000.0 {
        let hz = rate / n_pilot as f64;
        let raw: Vec<f64> = pilot_power.iter().map(|p| 10.0 * (p / pilot_blocks as f64 + 1e-30).log10()).collect();
        let around = |k: usize| {
            let (span, skip) = ((1000.0 / hz) as usize, (150.0 / hz).ceil() as usize);
            let v: Vec<f64> = (k.saturating_sub(span)..(k + span + 1).min(raw.len())).filter(|&j| j + skip < k || j > k + skip).map(|j| raw[j]).collect();
            dsp::median(&v).unwrap_or(f64::INFINITY)
        };
        let (lo, hi) = ((19000.0 * (1.0 - PILOT_SPAN) / hz) as usize, ((19000.0 * (1.0 + PILOT_SPAN) / hz) as usize).min(raw.len() - 2));
        if let Some(k) = (lo..=hi).max_by(|&a, &b| (raw[a] - around(a)).total_cmp(&(raw[b] - around(b)))) {
            let prominence = raw[k] - around(k);
            let (a, b, c) = (raw[k - 1], raw[k], raw[k + 1]);
            let freq = (k as f64 + 0.5 * (a - c) / (a - 2.0 * b + c)) * hz;
            let near_19k = (freq - 19000.0).abs() <= 60.0;
            if prominence >= SHIFTED_PILOT_DB || (near_19k && prominence >= 15.0) {
                facts.pilot = Some((freq / 19000.0 - 1.0) * 100.0);
            }
        }
    }

    // Mains hum: the most prominent local peak within +-8 % of 50 or 60 Hz, measured against
    // its own +-2 Hz surroundings (the slope of the bass would otherwise favour the range edges).
    if hum_blocks > 0 {
        let hz = rate / n_hum as f64;
        let raw: Vec<f64> = hum_power.iter().map(|p| 10.0 * (p / hum_blocks as f64 + 1e-30).log10()).collect();
        let (span, skip) = ((2.0 / hz) as usize, (0.3 / hz).ceil() as usize);
        let prominence_at = |k: usize| {
            let around: Vec<f64> = (k.saturating_sub(span)..=(k + span)).filter(|&j| j + skip < k || j > k + skip).map(|j| raw[j]).collect();
            raw[k] - dsp::median(&around).unwrap_or(raw[k])
        };
        for nominal in [50.0, 60.0] {
            let (lo, hi) = ((nominal * 0.92 / hz) as usize, (nominal * 1.08 / hz) as usize);
            let Some(k) = (lo..=hi)
                .filter(|&k| raw[k] >= raw[k - 1] && raw[k] >= raw[k + 1])
                .max_by(|&a, &b| prominence_at(a).total_cmp(&prominence_at(b)))
            else {
                continue;
            };
            let prominence = prominence_at(k);
            let (a, b, c) = (raw[k - 1], raw[k], raw[k + 1]);
            let freq = (k as f64 + 0.5 * (a - c) / (a - 2.0 * b + c)) * hz;
            if facts.hum.is_none_or(|(_, _, p)| prominence > p) {
                facts.hum = Some((nominal, freq, prominence));
            }
        }
        if facts.hum.is_some_and(|(_, _, p)| p < 10.0) {
            facts.hum = None;
        }
    }
    Ok(facts)
}

/// Adds the power spectrum of a windowed block to `out`.
#[allow(clippy::too_many_arguments)]
fn accumulate(block: &[f64], fft: &Fft, window: &[f64], out: &mut [f64], scratch: &mut [f64], bins: &mut [f64], re: &mut Vec<f64>, im: &mut Vec<f64>) {
    let n = block.len();
    for i in 0..n {
        scratch[i] = block[i] * window[i];
    }
    fft.power(&scratch[..n], re, im, &mut bins[..n / 2 + 1]);
    for (o, b) in out.iter_mut().zip(bins.iter()) {
        *o += b;
    }
}

/// The verdict of a track or a show, with its report.
pub struct SpeedVerdict {
    pub label: String,
    pub level: Level,
    pub sentence: String,
    pub report: Report,
}

/// The measurements common to track and show reports.
fn measures(pilot: Option<f64>, tuning: Option<(f64, f64)>, hum: Option<(f64, f64)>, hum_note: &str) -> Vec<Measure> {
    let mut out = Vec::new();
    out.push(Measure {
        name: "19 kHz FM pilot".into(),
        value: match pilot {
            Some(p) => format!("found at {:.2} kHz: {}", 19.0 * (1.0 + p / 100.0), if p.abs() < 0.2 { "right speed".into() } else { describe(p) }),
            None => "not found".into(),
        },
        meaning: "FM stereo stations transmit a tone at exactly 19 000 Hz: when it is on the recording, it measures the speed to the hundredth of a percent.".into(),
        level: if pilot.is_some() { Level::Ok } else { Level::Info },
    });
    out.push(Measure {
        name: "Tuning against A = 440 Hz".into(),
        value: match tuning {
            Some((c, coh)) if coh >= MIN_COHERENCE => {
                format!("{c:+.0} cents (would be {} if the band tuned to A = 440 Hz), coherence {coh:.2}", describe(cents_to_percent(c)))
            }
            Some((c, coh)) => format!("{c:+.0} cents, but coherence {coh:.2}: too weak to use"),
            None => "no tonal content".into(),
        },
        meaning: "Bands usually tune to A = 440 Hz: a whole recording sharp or flat by the same amount points to a speed error. Blind to errors of a whole semitone (5.9 %): a recording 5.5 % fast looks only 7 cents flat. Bands tuning down or by ear (common in the early 70s) and old pianos or organs fool it.".into(),
        level: match tuning {
            Some((c, coh)) if coh >= MIN_COHERENCE && c.abs() >= TUNING_TOLERANCE_CENTS => Level::Warn,
            Some((_, coh)) if coh >= MIN_COHERENCE => Level::Ok,
            _ => Level::Info,
        },
    });
    out.push(Measure {
        name: "Mains hum".into(),
        value: match hum {
            Some((nominal, f)) => format!("{f:.2} Hz, {} from {nominal:.0} Hz{hum_note}", describe((f / nominal - 1.0) * 100.0)),
            None => format!("none clear{hum_note}"),
        },
        meaning: "Hum from the mains (50 Hz in Europe, 60 Hz in America) recorded with the music moves with the tape speed. But it may have come in at a later copy, so it only measures the stages after it: a hint, not a proof.".into(),
        level: Level::Info,
    });
    out
}

/// The verdict for one track, from its own measurements.
pub fn track_verdict(title: String, f: &SpeedFacts) -> SpeedVerdict {
    let hum = f.hum.filter(|(_, _, p)| *p >= 12.0).map(|(n, freq, _)| (n, freq));
    let mut reasoning = Vec::new();
    let (label, level, sentence) = if let Some(p) = f.pilot {
        reasoning.push(format!("The 19 kHz FM pilot is at {:.2} kHz: the recording plays {}.", 19.0 * (1.0 + p / 100.0), if p.abs() < 0.2 { "at the right speed".into() } else { describe(p) }));
        if p.abs() < 0.2 {
            ("OK".to_string(), Level::Ok, "Right speed: the 19 kHz FM pilot is where it should be".to_string())
        } else {
            reasoning.push(format!("To correct: {}.", correction(p)));
            (describe(p), Level::Warn, format!("Plays {} (measured on the 19 kHz FM pilot): {}", describe(p), correction(p)))
        }
    } else {
        match f.tuning {
            Some((c, coh)) if coh >= CLAIM_COHERENCE && c.abs() >= TUNING_TOLERANCE_CENTS => {
                let p = cents_to_percent(c);
                reasoning.push(format!("The instruments are {c:+.0} cents from A = 440 Hz: if the band tuned to 440 Hz, the recording plays {}.", describe(p)));
                reasoning.push(format!("To correct: {}. Check it by ear before applying it.", correction(p)));
                reasoning.push(semitone_alternatives(c));
                (format!("{}?", describe(p)), Level::Warn, format!("Possibly plays {} (estimated from the tuning): {}", describe(p), correction(p)))
            }
            Some((c, coh)) if coh >= MIN_COHERENCE && c.abs() < TUNING_TOLERANCE_CENTS => {
                reasoning.push(format!("The instruments are {c:+.0} cents from A = 440 Hz: in tune, so no small speed error."));
                reasoning.push("A whole-semitone error (5.9 %) would also look in tune.".into());
                ("OK?".to_string(), Level::Ok, "In tune with A = 440 Hz: no sign of a small speed error".to_string())
            }
            Some((c, coh)) if coh >= MIN_COHERENCE => {
                reasoning.push(format!("The instruments are {c:+.0} cents from A = 440 Hz, but the tuning is not coherent enough (coherence {coh:.2}) to put a figure on a speed error: the band may not have tuned to A = 440 Hz."));
                ("Unclear".to_string(), Level::Info, "Speed not measurable reliably on this track".to_string())
            }
            _ => {
                reasoning.push("Not enough steady pitched notes to measure the tuning, and no FM pilot.".into());
                ("Unclear".to_string(), Level::Info, "Speed not measurable on this track".to_string())
            }
        }
    };
    if let Some((nominal, freq)) = hum {
        reasoning.push(format!(
            "Possible mains hum at {freq:.2} Hz ({} from {nominal:.0} Hz); on a single track it may be noise: the show report checks whether other tracks agree.",
            describe((freq / nominal - 1.0) * 100.0)
        ));
    }
    let key = [
        f.pilot.map(|p| format!("pilot {}", describe(p))),
        f.tuning.filter(|(_, coh)| *coh >= MIN_COHERENCE).map(|(c, _)| format!("tuning {c:+.0} cents")),
        hum.map(|(_, freq)| format!("hum {freq:.2} Hz")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ");
    let report = Report {
        heading: "Speed analysis".into(),
        title,
        label: label.clone(),
        level,
        sentence: sentence.clone(),
        measures: measures(f.pilot, f.tuning, hum, ""),
        reasoning,
        key_facts: key,
        tracks: Vec::new(),
    };
    SpeedVerdict { label, level, sentence, report }
}

/// The verdict for a whole show: tracks agreeing with each other are much more convincing.
pub fn show_verdict(title: String, tracks: &[(String, SpeedFacts, String, Level, String)]) -> SpeedVerdict {
    let pilots: Vec<f64> = tracks.iter().filter_map(|t| t.1.pilot).collect();
    // Tuning: the coherence-weighted median of the usable tracks, and how much they disagree.
    let mut tunings: Vec<(f64, f64)> = tracks.iter().filter_map(|t| t.1.tuning).filter(|(_, coh)| *coh >= MIN_COHERENCE).collect();
    tunings.sort_by(|a, b| a.0.total_cmp(&b.0));
    let tuning = if tunings.is_empty() {
        None
    } else {
        let half = tunings.iter().map(|t| t.1).sum::<f64>() / 2.0;
        let mut acc = 0.0;
        let median = tunings.iter().find(|t| {
            acc += t.1;
            acc >= half
        });
        let median = median.map(|t| t.0).unwrap_or(tunings[0].0);
        let spread = dsp::median(&tunings.iter().map(|t| (t.0 - median).abs()).collect::<Vec<_>>()).unwrap_or(0.0);
        let coherence = dsp::median(&tunings.iter().map(|t| t.1).collect::<Vec<_>>()).unwrap_or(0.0);
        Some((median, spread, tunings.len(), coherence))
    };
    // Hum: kept only when several tracks show the same line (+-0.15 %).
    let hums: Vec<(f64, f64)> = tracks.iter().filter_map(|t| t.1.hum).map(|(n, f, _)| (n, f)).collect();
    let needed = 3.max((tracks.len() as f64 * 0.3).ceil() as usize).min(tracks.len().max(1));
    let hum = hums
        .iter()
        .map(|(n, f)| {
            let agreeing: Vec<f64> = hums.iter().filter(|(m, g)| m == n && (g / f - 1.0).abs() <= 0.0015).map(|(_, g)| *g).collect();
            (*n, agreeing)
        })
        .max_by_key(|(_, a)| a.len())
        .filter(|(_, a)| a.len() >= needed)
        .map(|(n, a)| (n, dsp::median(&a).unwrap_or(0.0), a.len()));

    let mut reasoning = Vec::new();
    let hum_note = hum.map(|(_, _, k)| format!(" (on {k} of {} tracks)", tracks.len())).unwrap_or_default();
    // A real FM pilot is on most tracks, at the same frequency.
    let pilot = dsp::median(&pilots).filter(|p| {
        let agreeing = pilots.iter().filter(|q| (*q - p).abs() <= 0.15).count();
        agreeing * 2 >= tracks.len()
    });
    if pilot.is_none() && !pilots.is_empty() {
        reasoning.push(format!("A tone that could be the FM pilot shows on only {} of {} tracks: not used.", pilots.len(), tracks.len()));
    }
    let (label, level, sentence) = if let Some(p) = pilot {
        reasoning.push(format!("The 19 kHz FM pilot is on {} of {} tracks: the recording plays {}.", pilots.len(), tracks.len(), if p.abs() < 0.2 { "at the right speed".into() } else { describe(p) }));
        if p.abs() < 0.2 {
            ("OK".to_string(), Level::Ok, "Right speed: the 19 kHz FM pilot is where it should be".to_string())
        } else {
            reasoning.push(format!("To correct: {}.", correction(p)));
            (describe(p), Level::Warn, format!("Plays {} (measured on the 19 kHz FM pilot): {}", describe(p), correction(p)))
        }
    } else {
        let tuning_pct = tuning.map(|(c, _, _, _)| cents_to_percent(c));
        let hum_pct = hum.map(|(n, f, _)| (f / n - 1.0) * 100.0);
        if let Some((c, spread, k, coh)) = tuning {
            reasoning.push(format!(
                "Tuning: {c:+.0} cents from A = 440 Hz (median of {k} tracks, spread {spread:.0} cents, coherence {coh:.2}){}.",
                if spread > 10.0 {
                    ": the tracks disagree"
                } else if coh < CLAIM_COHERENCE {
                    ": not coherent enough to put a figure on a speed error (the band may not have tuned to A = 440 Hz)"
                } else {
                    ""
                }
            ));
        }
        if let (Some((n, f, k)), Some(h)) = (hum, hum_pct) {
            reasoning.push(format!("Mains hum: {k} tracks show the same line at {f:.2} Hz, {} from {n:.0} Hz.", describe(h)));
        }
        match (tuning, tuning_pct, hum_pct) {
            (Some((_, spread, _, coh)), Some(t), Some(h)) if spread <= 10.0 && coh >= MIN_COHERENCE && (t - h).abs() > 1.0 => {
                reasoning.push(format!(
                    "The two disagree ({} from the tuning, {} from the hum). The band may not have tuned to A = 440 Hz (early 70s bands often tuned down by ear), the error may be close to a whole semitone (5.9 %), which the tuning cannot see, or the hum came in at a later copy.",
                    describe(t),
                    describe(h)
                ));
                reasoning.push(format!(
                    "If the hum was recorded at the show, the right correction is: {}. Compare by ear with other recordings of the tour before deciding.",
                    correction(h)
                ));
                ("Unclear (conflict)".to_string(), Level::Warn, format!("Tuning and mains hum disagree ({} vs {}): see the analysis", describe(t), describe(h)))
            }
            (Some((c, spread, _, coh)), Some(t), hum_pct) if spread <= 10.0 && coh >= CLAIM_COHERENCE && c.abs() >= TUNING_TOLERANCE_CENTS => {
                let agree = hum_pct.is_some_and(|h| (t - h).abs() <= 0.5);
                if agree {
                    reasoning.push("The mains hum agrees with the tuning.".into());
                }
                reasoning.push(format!("To correct: {}. Check it by ear before applying it.", correction(t)));
                if !agree {
                    reasoning.push(semitone_alternatives(percent_to_cents(t)));
                }
                (
                    format!("{}?", describe(t)),
                    Level::Warn,
                    format!("Possibly plays {} (estimated from the tuning{}): {}", describe(t), if agree { " and the mains hum" } else { "" }, correction(t)),
                )
            }
            (Some((c, spread, _, coh)), Some(_), hum_pct) if spread <= 10.0 && coh >= MIN_COHERENCE && c.abs() < TUNING_TOLERANCE_CENTS => {
                if let Some(h) = hum_pct.filter(|h| h.abs() > 0.7) {
                    reasoning.push(format!("In tune with A = 440 Hz, but the hum says {}: the hum may have come in at a later copy.", describe(h)));
                } else {
                    reasoning.push("In tune with A = 440 Hz: no sign of a small speed error (a whole-semitone error would also look in tune).".into());
                }
                ("OK?".to_string(), Level::Ok, "In tune with A = 440 Hz: no sign of a small speed error".to_string())
            }
            (_, _, Some(h)) => {
                reasoning.push(format!("The tuning gives no usable answer; the hum alone suggests {}: {}.", describe(h), correction(h)));
                (format!("{}?", describe(h)), Level::Warn, format!("Possibly plays {} (from the mains hum only): {}", describe(h), correction(h)))
            }
            _ => {
                reasoning.push("No FM pilot, the tracks disagree on the tuning and no steady mains hum: the speed cannot be measured.".into());
                ("Unclear".to_string(), Level::Info, "Speed not measurable: see the analysis".to_string())
            }
        }
    };
    let mut measures = measures(
        pilot,
        tuning.map(|(c, spread, _, coh)| (c, if spread <= 10.0 { coh } else { 0.0 })),
        hum.map(|(n, f, _)| (n, f)),
        &hum_note,
    );
    if let Some((_, spread, k, _)) = tuning {
        measures[1].value = format!("{} (median of {k} tracks, spread {spread:.0} cents)", measures[1].value.split(", coherence").next().unwrap_or_default());
    }
    let report = Report {
        heading: "Speed analysis".into(),
        title,
        label: label.clone(),
        level,
        sentence: sentence.clone(),
        measures,
        reasoning,
        key_facts: String::new(),
        tracks: tracks.iter().map(|(name, _, label, level, key)| Measure { name: name.clone(), value: label.clone(), meaning: key.clone(), level: *level }).collect(),
    };
    SpeedVerdict { label, level, sentence, report }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_and_cents() {
        assert!((cents_to_percent(-19.0) + 1.09).abs() < 0.01);
        assert!((percent_to_cents(5.95) - 100.0).abs() < 0.5);
        assert_eq!(describe(-1.1), "1.1 % slow");
        assert!(correction(-1.1).starts_with("speed it up by 1.1 %"));
    }
}
