//! "Lossless?" check: spectral analysis looking for the traces that lossy
//! encoders, FM broadcasts and upsampling leave behind (same method and
//! thresholds as `analyse_source` in the Python reference).

use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::audio::AudioInfo;
use crate::decision::Judged;
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
    /// The origin is left open: worth the slower search for AAC and Opus traces.
    pub(crate) doubt: bool,
    /// The measures the verdict was drawn from, as JSON (only with BT_FACTS).
    pub(crate) facts: Option<Facts>,
    /// What the show's verdict needs (None for a silent file).
    pub(crate) track: Option<crate::decision::TrackDecision>,
}

impl Verdict {
    pub(crate) fn new(label: impl Into<String>, sentence: impl Into<String>, level: Level, details: Vec<String>) -> Self {
        Verdict { label: label.into(), sentence: sentence.into(), level, details, report: Report::default(), doubt: false, facts: None, track: None }
    }
}

pub use crate::report::{Measure, Report};

pub const LIKELY_LOSSLESS: &str = "Likely lossless";
/// Treble above 15 kHz (relative to 1-4 kHz) below which the lack of an encoder lowpass
/// proves less: "Likely lossless" when the MP3 coding traces were looked for in vain, and
/// "Unclear" below `TREBLE_UNCLEAR` (set in gaps of the measures of real shows, so that
/// tracks of one show do not fall on both sides).
pub(crate) const TREBLE_LIKELY: f64 = -55.0;
pub(crate) const TREBLE_UNCLEAR: f64 = -67.0;
/// Without the MP3 traces (other sample rates): one limit, as before.
pub(crate) const TREBLE_PLAIN: f64 = -60.0;
pub const LOSSLESS_NO_HIRES: &str = "Lossless (no hi-res content)";
/// A gradual roll-off, not a wall: the treble falls by at most this much between neighbouring
/// 300 Hz cells. Set on synthetic filters (9-15 dB) and encodes (18 dB and more); on real
/// shows a fifth to a third of the lossy files with a cutoff also stay under it, so it never
/// decides alone...
pub(crate) const ANALOG_STEP: f64 = 16.0;
/// ...and the band just above the cutoff still follows the music (few lossy files with a
/// cutoff reach it on real shows, but neither do most lossless ones).
pub(crate) const ANALOG_COHERENCE: f64 = 0.6;
pub const LOSSLESS_PADDED: &str = "Lossless (16-bit in 24)";

/// Python's round(): halves go to the even neighbour.
fn round_py(x: f64) -> usize {
    x.round_ties_even().max(0.0) as usize
}

/// The verdict for a file. An MP3 or MP2 file is lossy by its format, whatever its spectrum
/// shows: the measures then only tell how much was cut. `aac`: also look for AAC coding
/// traces (several times slower, an option in Settings).
pub fn analyse(path: &Path, info: &AudioInfo, aac: bool, cancel: &AtomicBool) -> Result<Verdict, String> {
    if info.codec.is_some_and(|c| !c.decoded()) {
        return Ok(by_format(path, info));
    }
    let record = std::env::var_os("BT_FACTS").is_some();
    // `--cli replay`: the measures recorded earlier (BT_FACTS), the audio not read again.
    let mut verdict = if let Some(recorded) = REPLAY.get() {
        match recorded.get(path.to_string_lossy().as_ref()).ok_or("no measures recorded for this file")? {
            Ok(facts) => {
                let first = crate::decision::decide(path, facts, aac);
                let again = !aac && first.doubt;
                if (aac || again) && !facts.searched {
                    return Err("recorded without the AAC and Opus searches: record with BT_AAC=1".into());
                }
                if again { crate::decision::decide(path, facts, true) } else { first }
            }
            Err(message) => {
                write_line(path, serde_json::json!({ "error": message }));
                return Err(message.clone());
            }
        }
    } else {
        let measured = measure(path, info, cancel).inspect_err(|e| write_line(path, serde_json::json!({ "error": e })))?;
        let mut verdict = crate::decision::decide(path, &facts(info, &measured, aac, record), aac);
        // The AAC search is slow, so an option; a doubt is worth it anyway: the verdict is drawn
        // again with it, and an AAC converted straight back turns the doubt into a certainty.
        if !aac && verdict.doubt && !cancel.load(std::sync::atomic::Ordering::Relaxed) {
            verdict = crate::decision::decide(path, &facts(info, &measured, true, record), true);
        }
        verdict
    };
    if info.lossy() {
        let name = info.name();
        verdict.track = Some(crate::decision::TrackDecision::lossy_format());
        verdict.label = format!("Lossy ({name})");
        verdict.level = Level::Error;
        // The measured figures only ("cut 12.6 kHz, holes 4 dB"): the analysis's own doubts
        // about the origin no longer apply.
        let facts = &verdict.report.key_facts;
        verdict.sentence = if facts.is_empty() {
            format!("{name} file: lossy by its format")
        } else {
            format!("{name} file: lossy by its format (measured: {facts})")
        };
        verdict.report.label = verdict.label.clone();
        verdict.report.level = Level::Error;
        verdict.report.sentence = verdict.sentence.clone();
        verdict.report.evidence.insert(
            0,
            crate::report::Evidence {
                family: crate::report::Family::Coding,
                side: crate::report::Side::Lossy,
                strength: crate::report::Strength::Strong,
                text: format!("The file is an {name}: a lossy format"),
            },
        );
        verdict.report.reasoning.insert(
            0,
            format!(
                "The file is an {name}: this format always discards part of the sound when it is made, so it is lossy whatever \
                 its spectrum shows. The measures below only tell how much was cut (an MP3 at 320 kbps usually keeps up to \
                 about 20 kHz, at 128 kbps about 16 kHz)."
            ),
        );
    }
    write_facts(path, &verdict);
    Ok(verdict)
}

/// BT_FACTS=<file>: one JSON line per analysed file (its path, final verdict and measures)
/// is appended to that file (or its error, when the analysis failed), to score the verdicts and
/// replay the decision without the audio.
fn write_facts(path: &Path, verdict: &Verdict) {
    let Some(facts) = &verdict.facts else {
        return;
    };
    let level = match verdict.level {
        Level::Ok => "ok",
        Level::Warn => "warn",
        Level::Error => "error",
        Level::Info => "info",
    };
    write_line(path, serde_json::json!({ "label": verdict.label, "level": level, "doubt": verdict.doubt, "facts": facts }));
}

/// Appends the line of `path` to the BT_FACTS file (nothing without the variable).
fn write_line(path: &Path, mut line: serde_json::Value) {
    use std::io::Write;
    static OUT: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> = std::sync::OnceLock::new();
    let Some(out) = OUT.get_or_init(facts_file) else {
        return;
    };
    line["path"] = path.to_string_lossy().into();
    if let Ok(mut file) = out.lock() {
        let _ = writeln!(file, "{line}");
    }
}

fn facts_file() -> Option<std::sync::Mutex<std::fs::File>> {
    let name = std::env::var_os("BT_FACTS")?;
    std::fs::OpenOptions::new().create(true).append(true).open(name).ok().map(std::sync::Mutex::new)
}

/// The measures read back by `--cli replay`, by path as written in the BT_FACTS lines.
static REPLAY: std::sync::OnceLock<std::collections::HashMap<String, Result<Facts, String>>> = std::sync::OnceLock::new();

/// Loads the BT_FACTS lines of `file`: from then on `analyse` draws its verdicts from them
/// instead of reading the audio. Returns the number of files.
pub fn load_replay(file: &Path) -> Result<usize, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut recorded = std::collections::HashMap::new();
    for (n, line) in text.lines().enumerate() {
        let at = |e: String| format!("{} line {}: {e}", file.display(), n + 1);
        let v: serde_json::Value = serde_json::from_str(line).map_err(|e| at(e.to_string()))?;
        let path = v["path"].as_str().ok_or_else(|| at("no path".into()))?.to_string();
        // A file whose analysis failed is recorded with its error message.
        let facts = match v["error"].as_str() {
            Some(message) => Err(message.to_string()),
            None => Ok(serde_json::from_value(v["facts"].clone()).map_err(|e| at(e.to_string()))?),
        };
        recorded.insert(path, facts);
    }
    let count = recorded.len();
    REPLAY.set(recorded).map_err(|_| "measures already loaded".to_string())?;
    Ok(count)
}

/// The verdict for a file lossy by its format, without measuring its sound: "Lossy (AAC)".
/// For the Source column as soon as a file is read, and for codecs this version does not
/// decode (AC-3, DTS).
pub fn by_format(path: &Path, info: &AudioInfo) -> Verdict {
    let name = info.name();
    let mut verdict = Verdict::new(format!("Lossy ({name})"), format!("{name} file: lossy by its format"), Level::Error, Vec::new());
    verdict.track = Some(crate::decision::TrackDecision::lossy_format());
    let measured = if info.codec.is_some_and(|c| !c.decoded()) {
        "This version does not decode it, so nothing was measured."
    } else {
        "Lossless? (Ctrl+L) also measures how much was cut."
    };
    verdict.report = Report {
        heading: "Lossless analysis".into(),
        title: crate::files::file_name(path),
        label: verdict.label.clone(),
        level: verdict.level,
        sentence: verdict.sentence.clone(),
        reasoning: vec![format!(
            "The file is an {name}: this format always discards part of the sound when it is made, so it is lossy whatever \
             its spectrum shows. {measured}"
        )],
        ..Default::default()
    };
    verdict
}

/// What the spectra of the blocks are measured with.
struct Shape {
    fft: Fft,
    window: Vec<f64>,
    bins: usize,
    /// ~100 Hz bands, in bins; the kept ones (7 to 22 kHz).
    band: usize,
    kept_lo: usize,
    kept_hi: usize,
    bin_hz: f64,
    intensity_check: bool,
}

/// One block's power spectrum, and when kept: its 7-22 kHz bands, intensity stereo and width.
type BlockMeasures = (Vec<f64>, Vec<f32>, Option<(f64, f64, f64)>, Option<f64>);

/// The spectra added up over the track, and the measures of the kept blocks.
struct Spectra {
    power: Vec<f64>,
    /// Per kept block, the power of the bands between 7 and 22 kHz.
    kept: Vec<f32>,
    /// Intensity stereo (energy, treble coherence, mid coherence), per kept block.
    intensity: Vec<(f64, f64, f64)>,
    /// Stereo width: the difference channel against the sum, 5-10 kHz, in dB.
    side_vs_mid: Vec<f64>,
}

impl Spectra {
    /// Blocks (mono, side when kept, kept), in the order read: each one's spectrum is
    /// computed in parallel, then added in that order, as one after the other.
    fn add(&mut self, blocks: Vec<(Vec<f64>, Option<Vec<f64>>, bool)>, s: &Shape) {
        use rayon::prelude::*;
        let width_band = |f: f64| (5000.0..10000.0).contains(&f);
        let measured: Vec<BlockMeasures> = blocks
            .into_par_iter()
            .map_init(
                || (Vec::new(), Vec::new()),
                |(re, im), (mut block, side, keep)| {
                    for (x, w) in block.iter_mut().zip(&s.window) {
                        *x *= w;
                    }
                    let mut frame_power = vec![0f64; s.bins];
                    s.fft.power(&block, re, im, &mut frame_power);
                    let mut kept = Vec::new();
                    let (mut intensity, mut width) = (None, None);
                    if keep {
                        for b in s.kept_lo..s.kept_hi {
                            kept.push((frame_power[b * s.band..(b + 1) * s.band].iter().sum::<f64>() / s.band as f64) as f32);
                        }
                        if let Some(mut side_block) = side {
                            for (x, w) in side_block.iter_mut().zip(&s.window) {
                                *x *= w;
                            }
                            // The mid spectrum is still in re/im: keep it for the coherence.
                            let (mid_re, mid_im) = if s.intensity_check { (re.clone(), im.clone()) } else { (Vec::new(), Vec::new()) };
                            let mut side_power = vec![0f64; s.bins];
                            s.fft.power(&side_block, re, im, &mut side_power);
                            if s.intensity_check {
                                intensity = Some(coherence(&mid_re, &mid_im, re, im, s.bin_hz, frame_power.iter().sum()));
                            }
                            let sum = |p: &[f64]| (0..s.bins).filter(|&k| width_band(k as f64 * s.bin_hz)).map(|k| p[k]).sum::<f64>();
                            let (sd, m) = (sum(&side_power), sum(&frame_power));
                            if m > 0.0 {
                                width = Some(10.0 * libm::log10((sd + 1e-30) / m));
                            }
                        }
                    }
                    (frame_power, kept, intensity, width)
                },
            )
            .collect();
        for (frame_power, kept, intensity, width) in measured {
            for (p, f) in self.power.iter_mut().zip(&frame_power) {
                *p += f;
            }
            self.kept.extend(kept);
            self.intensity.extend(intensity);
            self.side_vs_mid.extend(width);
        }
    }
}

/// What one reading of the file measured (the AAC and Opus windows are collected in any
/// case: a doubtful verdict is then drawn again with them, without reading the file again).
struct Measured {
    rate: f64,
    bins: usize,
    band: usize,
    bin_hz: f64,
    kept_lo: usize,
    kept_hi: usize,
    window: Vec<f64>,
    frames: usize,
    power: Vec<f64>,
    kept: Vec<f32>,
    intensity: Vec<(f64, f64, f64)>,
    side_vs_mid: Vec<f64>,
    check_low: bool,
    low_bits: bool,
    wasted: [u32; 33],
    comb_check: bool,
    histogram: Vec<u32>,
    mp3_trace: Option<crate::mp3trace::Trace>,
    aac: Option<crate::mp3trace::Collector>,
    opus: Option<crate::opustrace::Collector>,
}

fn measure(path: &Path, info: &AudioInfo, cancel: &AtomicBool) -> Result<Measured, String> {
    let rate = info.rate as f64;
    if rate <= 0.0 {
        return Err("invalid sample rate".into());
    }
    let channels = info.channels.max(1) as usize;
    // About 11 Hz per bin.
    let nfft = 1usize << (libm::log2(rate / 11.0).round_ties_even() as u32).max(12);
    let fft = Fft::new(nfft);
    let window = dsp::hann_symmetric(nfft);
    let bins = nfft / 2 + 1;
    let mut frames = 0usize;
    let mut block = Vec::with_capacity(nfft);
    let mut silent = true;
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
    // Stereo width: the difference channel (L-R) against the sum, 5-10 kHz, on the kept frames.
    let mut side_block = Vec::with_capacity(nfft);
    // Intensity stereo, on the kept frames: how alike left and right are per 1 kHz band
    // (energy, treble 14-19.5 kHz all locked, mean coherence 3-8 kHz).
    let intensity_check = channels == 2 && rate >= 44100.0;
    // The spectra of blocks of frames are computed in parallel, then added in order.
    const BATCH: usize = 64;
    let shape = Shape { fft, window, bins, band, kept_lo, kept_hi, bin_hz, intensity_check };
    let mut spectra = Spectra { power: vec![0f64; bins], kept: Vec::new(), intensity: Vec::new(), side_vs_mid: Vec::new() };
    let mut pending: Vec<(Vec<f64>, Option<Vec<f64>>, bool)> = Vec::with_capacity(BATCH);
    // lossyWAV zeroes the lowest bits of whole blocks: the common trailing zero bits of
    // every block of 128 sample frames (silent blocks left out).
    let integer = !info.float;
    const WASTE_BLOCK: usize = 128;
    let (mut waste_or, mut waste_len) = (0i32, 0usize);
    let mut wasted = [0u32; 33];
    // 24-bit made from fewer bits with a level change: only some sample values are used
    // (a comb); the values around zero are counted.
    let comb_check = info.bits == 24 && integer;
    // MP3 traces in the encoder's own filter bank (MP3 sample rates only).
    let codec_rates = matches!(info.rate, 32000 | 44100 | 48000) && channels <= 2 && !info.lossy();
    let mut mp3 = codec_rates.then(|| crate::mp3trace::Collector::new(channels));
    let mut aac = codec_rates.then(|| crate::aactrace::collector(channels));
    // Opus traces, with the AAC search (Opus is 48 kHz; 44.1 to 96 kHz are resampled to it).
    let mut opus = (channels <= 2 && !info.lossy()).then(|| crate::opustrace::Collector::new(channels, info.rate)).flatten();
    const SPAN: i32 = 1 << 16;
    let mut histogram = if comb_check { vec![0u32; 2 * SPAN as usize] } else { Vec::new() };

    dsp::for_each_frame(path, info, cancel, |values, bytes| {
        if check_low && !low_bits {
            low_bits = bytes.chunks_exact(width).any(|s| dsp::int_sample(s) & 0xFF != 0);
        }
        if integer {
            for s in bytes.chunks_exact(width) {
                let v = dsp::int_sample(s);
                waste_or |= v;
                if comb_check && (-SPAN..SPAN).contains(&v) {
                    histogram[(v + SPAN) as usize] += 1;
                }
            }
            waste_len += 1;
            if waste_len == WASTE_BLOCK {
                if waste_or != 0 {
                    wasted[waste_or.trailing_zeros() as usize] += 1;
                }
                (waste_or, waste_len) = (0, 0);
            }
        }
        if let Some(m) = mp3.as_mut() {
            m.push(values);
        }
        if let Some(a) = aac.as_mut() {
            a.push(values);
        }
        if let Some(o) = opus.as_mut() {
            o.push(values);
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
                let keep = frames.is_multiple_of(stride);
                let side = (keep && channels == 2).then(|| std::mem::replace(&mut side_block, Vec::with_capacity(nfft)));
                pending.push((std::mem::replace(&mut block, Vec::with_capacity(nfft)), side, keep));
                frames += 1;
                if pending.len() == BATCH {
                    spectra.add(std::mem::take(&mut pending), &shape);
                }
            }
            side_block.clear();
            block.clear();
            silent = true;
        }
    })?;
    spectra.add(pending, &shape);
    let Spectra { power, kept, intensity, side_vs_mid } = spectra;
    let mp3_trace = mp3.and_then(|m| m.detect(info.rate));
    Ok(Measured {
        rate,
        bins,
        band,
        bin_hz,
        kept_lo,
        kept_hi,
        window: shape.window,
        frames,
        power,
        kept,
        intensity,
        side_vs_mid,
        check_low,
        low_bits,
        wasted,
        comb_check,
        histogram,
        mp3_trace,
        aac,
        opus,
    })
}

/// The measures of a file. `aac_check`: the AAC and Opus searches are made (they are slow);
/// `all`: whichever of them is made covers every detector, even where the decision would not
/// look at the result (BT_FACTS records everything, so that a replay can gate as it chooses).
fn facts(info: &AudioInfo, m: &Measured, aac_check: bool, all: bool) -> Facts {
    let Measured { rate, bins, band, bin_hz, kept_lo, kept_hi, frames, check_low, low_bits, comb_check, mp3_trace, .. } = *m;
    let (window, kept, intensity, side_vs_mid, wasted, histogram) = (&m.window, &m.kept, &m.intensity, &m.side_vs_mid, &m.wasted, &m.histogram);
    let band_freq = |b: usize| (b * band) as f64 * bin_hz + (band - 1) as f64 * bin_hz / 2.0;
    let mut power = m.power.clone();
    // AAC only when no MP3 was found, Opus when neither was (the decision gates the same way).
    let mp3_found = mp3_trace.is_some_and(|t| t.found());
    let aac_trace = if !aac_check || (mp3_found && !all) { None } else { m.aac.clone().and_then(|a| crate::aactrace::detect(a, info.rate)) };
    let aac_found = aac_trace.is_some_and(|t| crate::aactrace::found(&t));
    let opus_trace = if !aac_check || ((mp3_found || aac_found) && !all) { None } else { m.opus.clone().and_then(|o| o.detect()) };
    if frames == 0 {
        return Facts { rate, frames: 0, mp3_trace, searched: aac_check, ..Default::default() };
    }

    for p in power.iter_mut() {
        *p /= frames as f64;
    }
    let freq = |k: usize| k as f64 * bin_hz;
    let raw: Vec<f64> = power.iter().map(|p| 10.0 * libm::log10(p + 1e-30)).collect();
    let median = |s: &[f64]| dsp::median(s).unwrap_or(f64::NEG_INFINITY);

    // Narrow tones: how far bin k stands above the median of +-1 kHz around it.
    let prominence = |k: usize| {
        let (span, skip) = ((1000.0 / bin_hz) as usize, libm::ceil(150.0 / bin_hz) as usize);
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
    let pilot_prominence = (!near.is_empty() && peak.is_finite()).then(|| peak - median(&near));
    let pilot_on_19k = pilot_found(pilot_prominence);
    // ...or shifted by a tape running too fast or too slow (it is 19 000 Hz exactly on air),
    // which then measures the speed error; a stricter threshold away from 19 kHz.
    let shifted_candidate = if rate / 2.0 < 20500.0 {
        None
    } else {
        let span = 19000.0 * crate::speed::PILOT_SPAN;
        strongest(19000.0 - span, 19000.0 + span)
    };
    let shifted_pilot = shifted_pilot_of(shifted_candidate, pilot_on_19k);
    // TV line frequency (15 625 Hz in Europe, 15 734 Hz in America): TV simulcast or a TV set nearby.
    let line_candidate = strongest(15560.0, 15800.0);
    let line_whistle = line_whistle_of(line_candidate);
    let mut tones: Vec<f64> = line_whistle.into_iter().collect();
    if pilot_on_19k {
        tones.push(19000.0);
    }
    tones.extend(shifted_pilot);

    let nb = bins / band;
    let mut level: Vec<f64> =
        (0..nb).map(|b| 10.0 * libm::log10(power[b * band..(b + 1) * band].iter().sum::<f64>() / band as f64 + 1e-30)).collect();
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
    // Steepest drop above 10 kHz: lossy encoders cut with a brick wall a few hundred Hz wide.
    let gap = round_py(150.0 / bw).max(1);
    let width_bands = round_py(600.0 / bw).max(2);
    let find_cutoff = |level: &[f64]| {
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
        (best_drop, best_i, cutoff)
    };
    let (mut best_drop, mut best_i, mut cutoff) = find_cutoff(&level);
    // A few loud clicks (all frequencies at once) fill the average above a real cutoff:
    // look again without the 1 % of kept frames with the most treble.
    let kept_bands = kept_hi - kept_lo;
    if cutoff.is_none() && kept_bands > 0 && kept.len() / kept_bands >= 100 {
        let treble: Vec<f64> = kept.chunks_exact(kept_bands).map(|frame| frame.iter().map(|&p| p as f64).sum()).collect();
        let limit = dsp::percentile(&treble, 99.0).unwrap_or(f64::INFINITY);
        let calm: Vec<&[f32]> = kept.chunks_exact(kept_bands).zip(&treble).filter(|(_, &t)| t <= limit).map(|(f, _)| f).collect();
        let mut steady = level.clone();
        for b in kept_lo..kept_hi.min(nb) {
            let mean = calm.iter().map(|frame| frame[b - kept_lo] as f64).sum::<f64>() / calm.len().max(1) as f64;
            steady[b] = 10.0 * libm::log10(mean + 1e-30);
        }
        for &b in &toned {
            let around: Vec<f64> = (b.saturating_sub(12)..b).filter(|&j| !in_tone(fb[j])).map(|j| steady[j]).collect();
            if let Some(m) = dsp::median(&around) {
                steady[b] = m;
            }
        }
        let found = find_cutoff(&steady);
        if found.2.is_some() {
            (best_drop, best_i, cutoff) = found;
            level = steady;
        }
    }
    let median_between = |lo: f64, hi: f64| {
        let sel: Vec<f64> = (0..nb).filter(|&i| fb[i] >= lo && fb[i] < hi).map(|i| level[i]).collect();
        dsp::median(&sel)
    };

    let reference = median_between(1000.0, 4000.0);

    // How fast the level falls: across the cutoff (3 dB to 20 dB below the level under it),
    // or, without one, over the treble (10 to 16 kHz). Encoders and digital filters drop
    // by hundreds of dB per octave; tape, microphones and analog filters by tens.
    let edge = match (cutoff, best_i) {
        (Some(_), Some(bi)) => {
            let below = median(&level[bi - gap - width_bands..bi - gap]);
            let start = bi - gap - width_bands;
            let a = (start..nb).find(|&j| level[j] < below - 3.0);
            let b = a.and_then(|a| (a..nb).find(|&j| level[j] < below - 20.0));
            a.zip(b).map(|(a, b)| 17.0 / libm::log2(fb[b] / fb[a].max(1.0)).max(1.0 / 192.0))
        }
        _ => match (median_between(9500.0, 10500.0), median_between(15000.0, 16500.0)) {
            (Some(lo), Some(hi)) if nyquist > 17000.0 => Some((lo - hi) / libm::log2(15750.0f64 / 10000.0)),
            _ => None,
        },
    };

    // Around a cutoff, what tells an encoder from an FM broadcast (tuned on real FM shows):
    // - encoder holes: perceptual encoders switch the top bands off and on from frame to
    //   frame, so just below the cutoff the level often collapses; FM and analog do not;
    // - the floor above the cutoff: an FM or analog capture keeps its hiss there up to the
    //   converter's filter, a decoded MP3/AAC falls to digital silence.
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
            .map(|frame| 10.0 * libm::log10(sel.iter().map(|&i| frame[i] as f64).sum::<f64>() / sel.len() as f64 + 1e-30))
            .collect()
    };
    // "Digital silence": the rounding noise of 16-bit audio, per bin. Hiss from an analog
    // source stays well above it; a decoded MP3/AAC falls to it (or below, in 24-bit).
    let quant_bits = 16;
    let lsb = 10.0 * libm::log10((2f64.powi(1 - quant_bits)).powi(2) / 12.0 * window.iter().map(|w| w * w).sum::<f64>());
    let traces = cutoff.map(|c| {
        let reference_band = series(c - 3000.0, c - 1500.0);
        let below: Vec<f64> = series(c - 800.0, c - 200.0).iter().zip(&reference_band).map(|(b, r)| b - r).collect();
        let holes = match (dsp::median(&below), dsp::percentile(&below, 5.0)) {
            (Some(m), Some(p)) => m - p,
            _ => 0.0,
        };
        let above_floor = dsp::percentile(&series(c + 600.0, (c + 2600.0).min(nyquist - 300.0)), 5.0).map(|p| p - lsb);
        let high_floor = dsp::percentile(&series(18000.0, 19500.0), 5.0).map(|p| p - lsb);
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
    let masked_bands = masked_bands(kept, kept_bands, &|b| band_freq(kept_lo + b), &|f| (18900.0..19100.0).contains(&f) || in_tone(f), lsb);
    let roll = cutoff.and_then(|c| rolloff(kept, kept_bands, &|b| band_freq(kept_lo + b), &|f| (18900.0..19100.0).contains(&f) || in_tone(f), c));
    let hf = median_between(15000.0, 19500.0);
    let hires = (rate >= 88200.0)
        .then(|| {
            let (low, high) = (median_between(18500.0, 20500.0)?, median_between(27000.0, 30000.0)?);
            let above27: Vec<f64> = (0..nb).filter(|&i| fb[i] >= 27000.0).map(|i| level[i]).collect();
            Some(Hires { low, high, tail: dsp::percentile(&above27, 90.0) })
        })
        .flatten();
    Facts {
        rate,
        frames,
        cutoff,
        drop: best_drop,
        holes: traces.map(|t| t.0),
        above_floor: traces.and_then(|t| t.1),
        high_floor: traces.and_then(|t| t.2),
        hiss,
        hf: reference.zip(hf).map(|(r, h)| h - r),
        stereo: dsp::median(side_vs_mid),
        edge,
        roll: roll.map(|r| (r.step, r.coherence)),
        pilot_prominence,
        shifted_candidate,
        line_candidate,
        hires,
        zero_low: check_low && !low_bits,
        comb_stats: if comb_check { comb_stats(histogram) } else { None },
        wasted: wasted.to_vec(),
        intensity: intensity_stats(intensity),
        masked_bands,
        mp3_trace,
        aac_trace,
        opus_trace,
        searched: aac_check,
    }
}

/// The 19 kHz peak counts as the FM pilot when it stands this far (dB) above its surroundings.
pub(crate) const PILOT_PROMINENCE: f64 = 15.0;

/// An FM stereo pilot tone left over at 19 kHz.
pub(crate) fn pilot_found(prominence: Option<f64>) -> bool {
    prominence.is_some_and(|p| p > PILOT_PROMINENCE)
}

/// The pilot shifted by a tape running too fast or too slow (it is 19 000 Hz exactly on air),
/// which then measures the speed error; a stricter threshold away from 19 kHz. Only where the
/// pilot itself was not found, and the candidate was measured (rates that reach it).
pub(crate) fn shifted_pilot_of(candidate: Option<(f64, f64)>, pilot_on_19k: bool) -> Option<f64> {
    if pilot_on_19k {
        return None;
    }
    candidate.filter(|(p, f)| *p >= crate::speed::SHIFTED_PILOT_DB && (f - 19000.0).abs() > 60.0).map(|(_, f)| f)
}

/// TV line frequency (15 625 Hz in Europe, 15 734 Hz in America): TV simulcast or a TV set nearby.
pub(crate) fn line_whistle_of(candidate: Option<(f64, f64)>) -> Option<f64> {
    candidate.filter(|(p, f)| *p >= 15.0 && ((f - 15625.0).abs() <= 40.0 || (f - 15734.0).abs() <= 40.0)).map(|(_, f)| f)
}

/// Per kept frame, from its mid and side spectra: (energy, the lowest coherence of the 1 kHz
/// bands from 14 to 19.5 kHz between left and right, the mean coherence of the 3-8 kHz bands).
fn coherence(mid_re: &[f64], mid_im: &[f64], side_re: &[f64], side_im: &[f64], bin_hz: f64, energy: f64) -> (f64, f64, f64) {
    let band = |lo: f64, hi: f64| -> f64 {
        let (mut cross_re, mut cross_im, mut ll, mut rr) = (0.0, 0.0, 0.0, 0.0);
        let first = libm::ceil(lo / bin_hz) as usize;
        let last = (libm::ceil(hi / bin_hz) as usize).min(mid_re.len());
        for k in first..last {
            let (lr, li) = (mid_re[k] + side_re[k], mid_im[k] + side_im[k]);
            let (rr_, ri) = (mid_re[k] - side_re[k], mid_im[k] - side_im[k]);
            cross_re += lr * rr_ + li * ri;
            cross_im += li * rr_ - lr * ri;
            ll += lr * lr + li * li;
            rr += rr_ * rr_ + ri * ri;
        }
        (cross_re * cross_re + cross_im * cross_im).sqrt() / ((ll * rr).sqrt() + 1e-30)
    };
    let high = [(14000.0, 15000.0), (15000.0, 16000.0), (16000.0, 17000.0), (17000.0, 18000.0), (18000.0, 19000.0), (19000.0, 19500.0)];
    // Opus's intensity stereo is one signal only panned: coherence all but 1 in every band (a
    // near-mono mix stays lower). The lowest of the bands tells; a value that is not a number
    // makes it one (and puts the frame with the least coherent).
    let locked = high.iter().map(|&(lo, hi)| band(lo, hi)).fold(f64::INFINITY, |m, v| if m.is_nan() || v.is_nan() { f64::NAN } else { m.min(v) });
    let low = [(3000.0, 4000.0), (4000.0, 5000.0), (5000.0, 6000.0), (6000.0, 7000.0), (7000.0, 8000.0)];
    let mean = low.iter().map(|&(lo, hi)| band(lo, hi)).sum::<f64>() / low.len() as f64;
    (energy, locked, mean)
}

/// What an empty top of a hi-res file says, without choosing between its two causes: "no
/// content above 21.6 kHz (the band of a 44.1 kHz source): upsampled, or an analog source
/// transferred at a high rate".
pub(crate) fn no_hires(cutoff: Option<f64>, origin: Option<f64>) -> String {
    match (cutoff, origin) {
        (Some(c), Some(r)) => format!(
            "no content above {:.1} kHz (the band of a {} kHz source): upsampled, or an analog source transferred at a high rate",
            c / 1000.0,
            crate::audio::trim_float(r / 1000.0)
        ),
        (Some(c), None) => format!("no content above {:.1} kHz: upsampled, or an analog source transferred at a high rate", c / 1000.0),
        _ => "no content above 22-24 kHz: upsampled, or an analog source transferred at a high rate".into(),
    }
}

/// Per ~100 Hz band of the kept frames: (Hz, how far the mean power stands above the 10th
/// percentile, which is the steady floor, in dB, and that floor over 16-bit digital silence in dB).
/// Steady noise alone stands about 3 dB above its floor, music far more. `skip`: bands left out
/// (tones). Empty without enough frames. Where the music stops is looked for in this table by
/// the decision.
pub(crate) fn masked_bands(kept: &[f32], kept_bands: usize, freq: &dyn Fn(usize) -> f64, skip: &dyn Fn(f64) -> bool, lsb: f64) -> Vec<(f64, f64, f64)> {
    if kept_bands == 0 || kept.len() / kept_bands < 100 {
        return Vec::new();
    }
    let frames = kept.len() / kept_bands;
    let db = |p: f64| 10.0 * libm::log10(p + 1e-30);
    (0..kept_bands)
        .filter(|&b| !skip(freq(b)))
        .map(|b| {
            let values: Vec<f64> = (0..frames).map(|i| kept[i * kept_bands + b] as f64).collect();
            let mean = values.iter().sum::<f64>() / frames as f64;
            let floor = dsp::percentile(&values, 10.0).unwrap_or(0.0);
            (freq(b), db(mean) - db(floor), db(floor) - lsb)
        })
        .collect()
}

/// How the treble ends at a lowpass, frame by frame: what tells an analog roll-off (tape,
/// microphones, a tuner) from an encoder's or a digital filter's wall.
#[derive(Clone, Copy, Debug)]
struct Rolloff {
    /// Median over the loud frames of the steepest drop between two 300 Hz bands within
    /// 1 kHz of the cutoff, in dB: a wall drops by tens of dB at once, a roll-off gradually.
    step: f64,
    /// Correlation over the frames between the level just below the cutoff and just above:
    /// under an analog roll-off the band above still follows the music.
    coherence: f64,
}

fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n < 2 {
        return 0.0;
    }
    let (ma, mb) = (a[..n].iter().sum::<f64>() / n as f64, b[..n].iter().sum::<f64>() / n as f64);
    let cov: f64 = a[..n].iter().zip(&b[..n]).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let var = |v: &[f64], m: f64| v.iter().map(|x| (x - m) * (x - m)).sum::<f64>();
    cov / (var(&a[..n], ma) * var(&b[..n], mb)).sqrt().max(1e-12)
}

/// The roll-off measures around `cutoff`, on the kept frames (bands of about 100 Hz; `skip`:
/// bands left out, tones). None without enough frames or bands around the cutoff.
fn rolloff(kept: &[f32], kept_bands: usize, freq: &dyn Fn(usize) -> f64, skip: &dyn Fn(f64) -> bool, cutoff: f64) -> Option<Rolloff> {
    if kept_bands == 0 || kept.len() / kept_bands < 100 {
        return None;
    }
    let db = |p: f64| 10.0 * libm::log10(p + 1e-30);
    let frames: Vec<&[f32]> = kept.chunks_exact(kept_bands).collect();
    let pick = |lo: f64, hi: f64| -> Vec<usize> { (0..kept_bands).filter(|&b| freq(b) >= lo && freq(b) < hi && !skip(freq(b))).collect() };
    let mean_db = |frame: &[f32], bands: &[usize]| db(bands.iter().map(|&b| frame[b] as f64).sum::<f64>() / bands.len().max(1) as f64);
    let (reference, below, above) = (pick(cutoff - 3000.0, cutoff - 1500.0), pick(cutoff - 1000.0, cutoff - 300.0), pick(cutoff + 300.0, cutoff + 1000.0));
    if reference.len() < 5 || below.len() < 3 || above.len() < 3 {
        return None;
    }
    let loudness: Vec<f64> = frames.iter().map(|f| mean_db(f, &reference)).collect();
    let median_loudness = dsp::median(&loudness)?;
    let loud: Vec<usize> = (0..frames.len()).filter(|&i| loudness[i] >= median_loudness).collect();
    if loud.len() < 50 {
        return None;
    }
    // Steepest drop between neighbouring 300 Hz bands within 1 kHz of the cutoff.
    let cells: Vec<Vec<usize>> =
        (0..7).map(|k| pick(cutoff - 1000.0 + 300.0 * k as f64, cutoff - 700.0 + 300.0 * k as f64)).filter(|c| !c.is_empty()).collect();
    let steps: Vec<f64> = loud
        .iter()
        .map(|&i| {
            let levels: Vec<f64> = cells.iter().map(|c| mean_db(frames[i], c)).collect();
            levels.windows(2).map(|w| w[0] - w[1]).fold(f64::NEG_INFINITY, f64::max)
        })
        .collect();
    let lo: Vec<f64> = frames.iter().map(|f| mean_db(f, &below)).collect();
    let hi: Vec<f64> = frames.iter().map(|f| mean_db(f, &above)).collect();
    Some(Rolloff { step: dsp::median(&steps)?, coherence: correlation(&lo, &hi) })
}

/// What was measured in a file: numbers and observations, with none of the decision's
/// thresholds (decision.rs applies those). It is what BT_FACTS records and `--cli replay`
/// reads back. The classifications still made while measuring are those the later
/// measures are taken from: the lowpass (`cutoff`) that holes, floors, hiss and roll-off are
/// measured around, and the tones that are masked before it is looked for.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Facts {
    #[serde(with = "crate::wire")]
    pub(crate) rate: f64,
    /// Blocks analysed (0: digital silence only).
    pub(crate) frames: usize,
    /// The steepest drop above 10 kHz, when it is a lowpass (a wall of 20 dB and more with the
    /// treble above it 15 dB under).
    #[serde(with = "crate::wire")]
    pub(crate) cutoff: Option<f64>,
    #[serde(with = "crate::wire")]
    pub(crate) drop: f64,
    /// Around the cutoff: depth of the holes just below it, floors above it (over 16-bit digital
    /// silence), then the floor in quiet and loud passages and its correlation with the music.
    #[serde(with = "crate::wire")]
    pub(crate) holes: Option<f64>,
    #[serde(with = "crate::wire")]
    pub(crate) above_floor: Option<f64>,
    #[serde(with = "crate::wire")]
    pub(crate) high_floor: Option<f64>,
    #[serde(with = "crate::wire")]
    pub(crate) hiss: Option<(f64, f64, f64)>,
    /// 15-19.5 kHz against 1-4 kHz.
    #[serde(with = "crate::wire")]
    pub(crate) hf: Option<f64>,
    /// Difference channel against the sum, 5-10 kHz.
    #[serde(with = "crate::wire")]
    pub(crate) stereo: Option<f64>,
    /// dB per octave across the cutoff, or over 10-16 kHz without one.
    #[serde(with = "crate::wire")]
    pub(crate) edge: Option<f64>,
    /// At the cutoff: the steepest drop between neighbouring bands (dB) and how much the band
    /// above follows the music (correlation).
    #[serde(with = "crate::wire")]
    pub(crate) roll: Option<(f64, f64)>,
    /// The 19 kHz peak above the median of 18-20 kHz, in dB (None without a peak to measure).
    #[serde(with = "crate::wire")]
    pub(crate) pilot_prominence: Option<f64>,
    /// The strongest tone around 19 kHz, where a tape running fast or slow would shift the
    /// pilot: (dB above its surroundings, Hz). Only measured at rates that reach it.
    #[serde(with = "crate::wire")]
    pub(crate) shifted_candidate: Option<(f64, f64)>,
    /// The strongest tone around 15.6-15.8 kHz (TV line frequency): (dB, Hz).
    #[serde(with = "crate::wire")]
    pub(crate) line_candidate: Option<(f64, f64)>,
    /// Hi-res files: levels at 18.5-20.5 kHz and 27-30 kHz, and above 27 kHz.
    pub(crate) hires: Option<Hires>,
    /// A 24-bit file whose lowest 8 bits are zero in every sample.
    pub(crate) zero_low: bool,
    /// A 24-bit file: how the sample values around zero are used (a comb of values).
    pub(crate) comb_stats: Option<CombStats>,
    /// Blocks of 128 sample frames by the number of low bits that are zero in all of them.
    pub(crate) wasted: Vec<u32>,
    /// Intensity stereo: the louder half of the frames (see `IntensityStats`).
    pub(crate) intensity: IntensityStats,
    /// Per ~100 Hz band of the kept frames: (Hz, how far the mean power stands over the steady
    /// floor in dB, that floor over digital silence in dB). Empty without enough frames.
    #[serde(with = "crate::wire")]
    pub(crate) masked_bands: Vec<(f64, f64, f64)>,
    /// The MP3 encoder's rounding pattern, looked for in its own filter bank.
    pub(crate) mp3_trace: Option<crate::mp3trace::Trace>,
    /// The same with AAC's and Opus's transforms. With BT_FACTS every detector is run whether or
    /// not the decision would look at it (it gates them); otherwise AAC is skipped when an MP3
    /// grid was found and Opus when either was.
    pub(crate) aac_trace: Option<crate::mp3trace::Trace>,
    pub(crate) opus_trace: Option<crate::opustrace::Trace>,
    /// The AAC and Opus searches were made (the option, or a doubt that asked for them).
    pub(crate) searched: bool,
}

/// The levels that tell a file upsampled to a high rate: median at 18.5-20.5 kHz, median at
/// 27-30 kHz, and the 90th percentile of everything above 27 kHz.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Hires {
    #[serde(with = "crate::wire")]
    pub(crate) low: f64,
    #[serde(with = "crate::wire")]
    pub(crate) high: f64,
    #[serde(with = "crate::wire")]
    pub(crate) tail: Option<f64>,
}

/// The sample values around zero of a 24-bit file, over the widest range where each value is
/// expected about 5 times: the range, how many distinct values occur, and how regular the gaps
/// between them are (the share within half to one and a half times the median gap).
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CombStats {
    pub(crate) range: usize,
    pub(crate) values: usize,
    #[serde(with = "crate::wire")]
    pub(crate) regular: Option<f64>,
}

/// Intensity stereo, over the kept frames that carry both channels: how many there are, how
/// many are in the louder half, the mean coherence of the 3-8 kHz bands over those, and how they
/// spread over the lowest coherence of the 14-19.5 kHz bands (`LOCK_EDGES`: bin i holds the
/// frames above edge i-1 and up to edge i).
#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct IntensityStats {
    pub(crate) frames: usize,
    pub(crate) loud: usize,
    #[serde(with = "crate::wire")]
    pub(crate) mid_mean: f64,
    pub(crate) locked: Vec<u32>,
}

/// Edges of the treble-coherence bins; the decision's threshold (0.995) is one of them.
pub(crate) const LOCK_EDGES: [f64; 18] =
    [0.5, 0.8, 0.9, 0.95, 0.97, 0.98, 0.985, 0.99, 0.992, 0.994, 0.995, 0.996, 0.997, 0.998, 0.999, 0.9995, 0.9999, 0.99999];

/// Louder half of the frames (energy at or above the median, and not zero), their mean mid
/// coherence, and their bins of treble coherence.
pub(crate) fn intensity_stats(frames: &[(f64, f64, f64)]) -> IntensityStats {
    let energies: Vec<f64> = frames.iter().map(|f| f.0).collect();
    let Some(median) = dsp::median(&energies) else {
        return IntensityStats::default();
    };
    let loud: Vec<&(f64, f64, f64)> = frames.iter().filter(|f| f.0 >= median && f.0 > 0.0).collect();
    let mut locked = vec![0u32; LOCK_EDGES.len() + 1];
    for f in &loud {
        locked[LOCK_EDGES.partition_point(|e| *e < f.1)] += 1;
    }
    let mid_mean = if loud.is_empty() { 0.0 } else { loud.iter().map(|f| f.2).sum::<f64>() / loud.len() as f64 };
    IntensityStats { frames: frames.len(), loud: loud.len(), mid_mean, locked }
}

/// The sample values around zero: see `CombStats`.
pub(crate) fn comb_stats(histogram: &[u32]) -> Option<CombStats> {
    let span = histogram.len() / 2;
    let mut r = span;
    while r >= 256 {
        let slice = &histogram[span - r..span + r];
        let n: u64 = slice.iter().map(|&c| c as u64).sum();
        if n >= 5 * slice.len() as u64 {
            let values: Vec<usize> = (0..slice.len()).filter(|&i| slice[i] > 0).collect();
            let regular = (values.len() >= 2).then(|| {
                let mut gaps: Vec<f64> = values.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
                gaps.sort_by(f64::total_cmp);
                let mid = gaps[gaps.len() / 2];
                gaps.iter().filter(|&&g| g >= 0.5 * mid && g <= 1.5 * mid).count() as f64 / gaps.len() as f64
            });
            return Some(CombStats { range: slice.len(), values: values.len(), regular });
        }
        r /= 2;
    }
    None
}

/// The measures of a file's report; the decision adds the clues, the reasoning and the confidence.
pub(crate) fn build_report(title: String, verdict: &Verdict, f: &Judged) -> Report {
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
            if f.mp3_trace.is_some() {
                "Below about -55 dB an encoder's lowpass shows less, and below about -67 dB there is too little treble to see it at all (common with audience, cassette and FM recordings)."
            } else {
                "Below about -60 dB there is too little treble to see an encoder's lowpass (common with audience, cassette and FM recordings)."
            },
            if h < if f.mp3_trace.is_some() { TREBLE_UNCLEAR } else { TREBLE_PLAIN } { Level::Warn } else { Level::Info },
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
            match (f.upsampled, f.origin, f.cutoff) {
                (true, Some(r), Some(c)) => format!("nothing above {} (the band of a {} kHz source)", khz(c), crate::audio::trim_float(r / 1000.0)),
                (true, _, _) => "nothing above 22-24 kHz".into(),
                _ => "real content above 22-24 kHz".into(),
            },
            "A true hi-res recording has content (or at least hiss) up there; an upsampled 44.1/48 kHz file is empty above 22-24 kHz, and so is an analog source with nothing up there transferred at a high rate.",
            if f.upsampled { Level::Warn } else { Level::Ok },
        );
    }
    if let Some(t) = f.mp3_trace {
        add(
            "MP3 coding traces",
            if t.found() {
                format!(
                    "found: {:.1} % of the bands, {:.0} standard deviations above chance (MP3 granules at sample {}, {} coding)",
                    t.likelihood * 100.0,
                    t.contrast,
                    t.offset,
                    if t.mid_side { "mid/side" } else { "left/right" }
                )
            } else {
                trace_none(t.likelihood, crate::mp3trace::THRESHOLD)
            },
            "The sound is analysed again with an MP3 encoder's own filter bank, at every possible position. A file decoded from an MP3 falls back onto the encoder's rounding grid in many bands (3 % and more, far above the other positions), even at 320 kbps; genuine audio stays around 2 %, at no particular position. Resampling or effects after the decoder erase this trace, so its absence does not prove a file lossless.",
            if t.found() { Level::Error } else if f.uniform { Level::Info } else { Level::Ok },
        );
    }
    if let Some(t) = f.aac_trace {
        let found = crate::aactrace::found(&t);
        add(
            "AAC coding traces",
            if found {
                format!(
                    "found: {:.1} % of the bands, {:.0} standard deviations above chance (AAC frames at sample {}, {})",
                    t.likelihood * 100.0,
                    t.contrast,
                    t.offset,
                    if t.mid_side { "mid/side" } else { "left/right" }
                )
            } else {
                trace_none(t.likelihood, crate::aactrace::THRESHOLD)
            },
            "The same search with the transform of AAC (iTunes, YouTube, streaming services, many webcasts). A file decoded from an AAC falls back onto the encoder's rounding grid at one position; genuine audio does not. Resampling or effects after the decoder erase this trace.",
            if found { Level::Error } else { Level::Ok },
        );
    }
    if let Some(t) = f.opus_trace {
        add(
            "Opus coding traces",
            if t.found() {
                let between = if t.phase > 0 { format!(" + {}/{}", t.phase, t.phases) } else { String::new() };
                format!("found: {} of {} frames are Opus codewords (frames at sample {}{between} of 48 kHz)", t.clean, t.frames, t.offset)
            } else {
                format!("none ({} of {} frames)", t.clean, t.frames)
            },
            "The sound is brought to 48 kHz, its de-emphasis undone, and analysed with Opus's own transform (20 ms frames). In the frames where Opus's pitch filter was off, every band of a decoded Opus is a vector of small integers, scaled; genuine audio never is. A resampling back and forth keeps this trace; added noise, equalising or a speed change erase it.",
            if t.found() { Level::Error } else { Level::Ok },
        );
    }
    if let Some(e) = f.edge {
        add(
            if f.cutoff.is_some() { "Edge of the lowpass" } else { "Treble roll-off" },
            format!("{e:.0} dB per octave"),
            "How fast the level falls (across the cutoff, or from 10 to 16 kHz without one). Encoders and digital filters drop by hundreds of dB per octave; tape, microphones and analog filters by tens.",
            Level::Info,
        );
    }
    if let Some(p) = &f.padded {
        add("Bit depth", p.clone(), "A 24-bit file holding 16-bit audio.", Level::Warn);
    } else if let Some((used, bits)) = f.comb {
        add(
            "Bits really used",
            if bits >= 23.0 { "all 24 bits (sample values used evenly)".into() } else { format!("about {bits:.0} (1 sample value in {:.0} occurs)", 1.0 / used) },
            "A true 24-bit recording uses every sample value near zero. Audio made from fewer bits and then turned up or down uses only a comb of values.",
            if bits >= 23.0 { Level::Ok } else { Level::Info },
        );
    }
    if let Some((share, bits)) = f.waste.filter(|(share, _)| *share > 0.01) {
        add(
            "Zeroed low bits (lossyWAV)",
            format!("{:.0} % of the blocks ({bits:.1} bits on average)", share * 100.0),
            "lossyWAV makes FLAC files smaller by zeroing the lowest bits of each block of samples, more in loud blocks: a lossy process. Genuine audio has almost no such blocks.",
            if f.lossywav { Level::Error } else { Level::Info },
        );
    }

    if let Some((step, coherence)) = f.roll {
        add(
            "Edge of the treble at the cutoff",
            format!("drops by {step:.0} dB at most between 300 Hz bands; the band above follows the music at {coherence:.2}"),
            "An encoder's or a digital filter's lowpass is usually a wall: the level falls steeply from one band to the next and nothing of the music is left above it. Tape, microphones and analog filters roll off gradually (16 dB or less) and the band above still follows the music (0.6 and more). Lossy files can roll off gradually too: a hint, never a proof.",
            if step > ANALOG_STEP && coherence < 0.3 { Level::Warn } else { Level::Info },
        );
    }
    if let Some(settings) = encoder_settings(f) {
        add(
            "Lowpass of encoder settings",
            settings,
            "Lossy encoders cut at a set frequency for each bitrate or quality (LAME, FDK-AAC, Vorbis; Opus always at 20 kHz). A cutoff that matches one is a hint only: a filter may cut there too, and several settings share a cutoff.",
            Level::Info,
        );
    }
    if f.from_44k {
        add(
            "Hi-res content",
            "content stops at the top of a 44.1 kHz band, digital silence above".into(),
            "A 48 kHz file upsampled from a 44.1 kHz source is empty above about 21-22 kHz; some 48 kHz converters' filters also cut there, so this is a hint only.",
            Level::Info,
        );
    }
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
        reasoning: Vec::new(),
        key_facts: key.join(", "),
        tracks: Vec::new(),
        cutoff: f.cutoff,
        speed: None,
        evidence: Vec::new(),
        confidence: None,
    }
}

/// The codec the coding traces name, when they were found.
fn traced_codec(f: &Judged) -> Option<crate::encoders::Codec> {
    use crate::encoders::Codec;
    if f.mp3_trace.is_some_and(|t| t.found()) {
        Some(Codec::Mp3)
    } else if f.aac_trace.is_some_and(|t| crate::aactrace::found(&t)) {
        Some(Codec::Aac)
    } else if f.opus_trace.is_some_and(|t| t.found()) {
        Some(Codec::Opus)
    } else {
        None
    }
}

/// The encoder settings whose lowpass matches the cutoff, only when coding traces name the
/// codec ("MP3 128 kbps"): without them a cutoff does not name a codec, let alone a bitrate.
pub(crate) fn encoder_settings(f: &Judged) -> Option<String> {
    crate::encoders::describe(f.cutoff?, Some(traced_codec(f)?))
}

/// No coding trace: the share of bands, said to be normal, without the contrast (for a
/// genuine file, "4 standard deviations above chance" read like a signal).
fn trace_none(likelihood: f64, threshold: f64) -> String {
    let share = likelihood * 100.0;
    if likelihood <= threshold {
        format!("none ({share:.1} % of the bands, within the normal range)")
    } else {
        format!("none at any position, but {share:.1} % of the bands, more than genuine audio shows: noise reduction, or a lossy stage whose grid was erased since, can leave this")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Music up to 16 kHz over a steady floor; above it the floor alone, rising toward
    /// 19 kHz (noise-shaped dither) or flat (FM hiss).
    #[test]
    fn analog_rolloff_and_wall() {
        let freq = |b: usize| 7050.0 + b as f64 * 100.0;
        let bands = 150;
        let frames = 400;
        // The music's loudness changes from frame to frame (0 to 30 dB); above 15 kHz either a
        // wall (steady noise only) or a roll-off of 3 dB per 100 Hz down to a steady hiss.
        let make = |wall: bool| -> Vec<f32> {
            let mut kept = Vec::new();
            for i in 0..frames {
                let loudness = ((i * 37) % 31) as f64;
                for b in 0..bands {
                    let f = freq(b);
                    let fall = if f <= 15000.0 { 0.0 } else if wall { 200.0 } else { (f - 15000.0) / 100.0 * 3.0 };
                    let music = libm::pow(10.0, (60.0 + loudness - fall) / 10.0);
                    kept.push((music + 1000.0 * (1.0 + 0.1 * ((i + b) % 3) as f64)) as f32);
                }
            }
            kept
        };
        let none = |_: f64| false;
        let analog = rolloff(&make(false), bands, &freq, &none, 15500.0).unwrap();
        assert!(analog.step <= ANALOG_STEP && analog.coherence >= ANALOG_COHERENCE, "{analog:?}");
        let wall = rolloff(&make(true), bands, &freq, &none, 15050.0).unwrap();
        assert!(wall.step > ANALOG_STEP && wall.coherence < ANALOG_COHERENCE, "{wall:?}");
    }
}
