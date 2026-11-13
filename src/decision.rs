//! The decision of "Lossless?": the verdict of a file from its measures (`lossless::Facts`),
//! and the verdict of a show from those of its files. Pure functions: the same measures always
//! give the same verdict, without reading the audio again.

use std::path::Path;

use crate::encoders::Codec;
use crate::lossless::{
    build_report, line_whistle_of, no_hires, pilot_found, shifted_pilot_of, Facts, IntensityStats, Measure, Report, Verdict, ANALOG_COHERENCE, ANALOG_STEP,
    LIKELY_LOSSLESS, LOCK_EDGES, LOSSLESS_NO_HIRES, LOSSLESS_PADDED, TREBLE_LIKELY, TREBLE_PLAIN, TREBLE_UNCLEAR,
};
use crate::ops::Level;
use crate::report::{Confidence, Evidence, Family, Side, Strength};

/// A lowpass seen through the noise: where the music stops above its own steady floor.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Masked {
    pub(crate) cutoff: f64,
    /// The steady floor above the cutoff, in dB over 16-bit digital silence.
    pub(crate) floor_above: f64,
    /// How much that floor rises toward 18.5-19.5 kHz (None when the cutoff is too high).
    pub(crate) rise: Option<f64>,
}

impl Masked {
    /// Noise-shaped dither: a moderate floor (louder than plain dither, quieter than most tape
    /// or FM hiss), rising toward 19 kHz where it can be measured (FM hiss stays flat).
    pub(crate) fn dithered(&self) -> bool {
        (24.0..=42.0).contains(&self.floor_above) && self.rise.is_none_or(|r| r >= 8.0)
    }
}

/// Where the music stops in the table of bands (`Facts::masked_bands`: Hz, how far the mean
/// stands over the steady floor, that floor over digital silence). The cutoff is the lowest
/// frequency from 13 kHz up whose band, and 90 % of the bands above, are steady noise (at most
/// 4 dB over the floor: steady noise alone gives about 3 dB, music far more), while the music
/// stands 8 dB or more above the floor in the 1.5 kHz below.
pub(crate) fn masked_cutoff(bands: &[(f64, f64, f64)]) -> Option<Masked> {
    let median = |lo: f64, hi: f64, pick: &dyn Fn(&(f64, f64, f64)) -> f64| {
        let v: Vec<f64> = bands.iter().filter(|b| b.0 >= lo && b.0 < hi).map(pick).collect();
        crate::dsp::median(&v)
    };
    let i = (0..bands.len()).find(|&i| {
        let c = bands[i].0;
        let above = &bands[i..];
        c >= 13000.0
            && above.len() >= 3
            && above[0].1 <= 4.0
            && above.iter().filter(|b| b.1 <= 4.0).count() * 10 >= above.len() * 9
            && bands.iter().filter(|b| b.0 >= c - 1500.0 && b.0 < c - 300.0).count() >= 5
            && median(c - 1500.0, c - 300.0, &|b| b.1).is_some_and(|m| m >= 8.0)
    })?;
    let cutoff = bands[i].0;
    let floor_above = crate::dsp::median(&bands[i..].iter().map(|b| b.2).collect::<Vec<_>>())?;
    let rise = if cutoff < 18000.0 {
        median(18500.0, 19500.0, &|b| b.2).zip(median(cutoff + 300.0, cutoff + 1300.0, &|b| b.2)).map(|(high, low)| high - low)
    } else {
        None
    };
    Some(Masked { cutoff, floor_above, rise })
}

/// Blocks whose lowest bits are all zero beyond the file's own resolution (lossyWAV).
pub(crate) struct Waste {
    pub(crate) blocks: u32,
    /// Share of the blocks with extra zero bits, and their mean number in those blocks.
    pub(crate) share: f64,
    pub(crate) mean_bits: f64,
    /// Share of the blocks with another number of zero bits than the most common one:
    /// lossyWAV changes it with the loudness; a constant number is the file's resolution
    /// (every sample even after a level change, say), not lossyWAV.
    pub(crate) varying: f64,
}

/// `counts[b]`: the blocks of 128 sample frames whose lowest `b` bits are zero in all of them.
pub(crate) fn wasted_bits(counts: &[u32]) -> Option<Waste> {
    let blocks: u32 = counts.iter().sum();
    // The smallest number of zero bits: the file's own resolution (8 for 16-bit in 24).
    let base = counts.iter().position(|&c| c > 0)?;
    let extra: u32 = counts[base + 1..].iter().sum();
    let bits: f64 = counts.iter().enumerate().skip(base + 1).map(|(b, &c)| (b - base) as f64 * c as f64).sum();
    let mode = counts.iter().copied().max().unwrap_or(0);
    Some(Waste {
        blocks,
        share: extra as f64 / blocks as f64,
        mean_bits: if extra > 0 { bits / extra as f64 } else { 0.0 },
        varying: (blocks - mode) as f64 / blocks as f64,
    })
}

/// lossyWAV: enough blocks, a third of them with extra zero bits, and a number that varies.
pub(crate) fn is_lossywav(w: &Waste) -> bool {
    w.blocks >= 200 && w.share >= 0.3 && w.varying >= 0.2
}

/// The bits a 24-bit file really uses: the share of the sample values around zero that occur,
/// and the resolution that gives. Audio made from fewer bits then scaled uses only a comb of
/// values: 1 in 256 for 16-bit audio, whatever the level change.
pub(crate) fn comb_bits(stats: &crate::lossless::CombStats) -> Option<(f64, f64)> {
    // Too few values to tell (a pure test tone visits only a handful).
    if stats.values < 64 {
        return None;
    }
    let used = stats.values as f64 / stats.range as f64;
    // A comb has regular teeth (a level change spaces them evenly); otherwise the values are
    // just sparse.
    if used < 0.9 && stats.regular.is_none_or(|r| r < 0.8) {
        return None;
    }
    Some((used, 24.0 - libm::log2(1.0 / used)))
}

/// The share of the louder half of the frames whose treble is locked between the channels,
/// when it is high enough (10 %) and the mids are real stereo (a mono file is locked
/// everywhere): Opus's intensity stereo. Genuine recordings tested (249 CD tracks
/// included): none.
pub(crate) fn intensity_share(s: &IntensityStats) -> Option<f64> {
    if s.frames < 50 || s.loud < 25 {
        return None;
    }
    // Locked: the lowest coherence of the treble bands above 0.995.
    let edge = LOCK_EDGES.iter().position(|e| *e == 0.995)?;
    let locked: u32 = s.locked.get(edge + 1..)?.iter().sum();
    let share = locked as f64 / s.loud as f64;
    (share >= 0.10 && s.mid_mean < 0.9).then_some(share)
}

/// The sample rate whose band ends at `end` Hz (a resampler cuts a little under half the rate).
pub(crate) fn source_rate(end: f64) -> Option<f64> {
    match end {
        19500.0..=22150.0 => Some(44100.0),
        22150.0..=24300.0 => Some(48000.0),
        40000.0..=44200.0 => Some(88200.0),
        44200.0..=48300.0 => Some(96000.0),
        _ => None,
    }
}

/// The measures of a file with the decision's thresholds applied, and the AAC and Opus
/// searches gated as the decision wants them (AAC only when no MP3 grid was found, Opus when
/// neither was, and neither unless the searches were asked for). What the report is drawn
/// from; the measures themselves are `Facts`, reached through `Deref`.
pub(crate) struct Judged<'a> {
    facts: &'a Facts,
    /// No digital silence's worth of noise above the cutoff nor at 18-19.5 kHz.
    pub(crate) silent_above: bool,
    /// No MP3 grid, yet more small rounding errors at every position than genuine audio shows:
    /// official CDs measured 1.8 to 2.0 %, the detector's threshold is 2.5 %.
    pub(crate) uniform: bool,
    pub(crate) intensity: Option<f64>,
    pub(crate) upsampled: bool,
    /// The sample rate an upsampled file comes from, when its content's edge tells it.
    pub(crate) origin: Option<f64>,
    /// A 48 kHz file whose content stops at the top of a 44.1 kHz band.
    pub(crate) from_44k: bool,
    pub(crate) pilot_on_19k: bool,
    pub(crate) shifted_pilot: Option<f64>,
    pub(crate) line_whistle: Option<f64>,
    pub(crate) speed: Option<String>,
    pub(crate) lossywav: bool,
    /// Share of the blocks with extra zero low bits, and their mean number.
    pub(crate) waste: Option<(f64, f64)>,
    /// Why the file holds 16-bit audio, when it does.
    pub(crate) padded: Option<String>,
    /// 24-bit files: share of the values used around zero, and the bits that gives.
    pub(crate) comb: Option<(f64, f64)>,
    pub(crate) masked: Option<Masked>,
    /// Hi-res files: level 27-30 kHz against 18.5-20.5 kHz.
    pub(crate) hires_gap: Option<f64>,
    /// Gated: what the decision looks at.
    pub(crate) aac_trace: Option<crate::mp3trace::Trace>,
    pub(crate) opus_trace: Option<crate::opustrace::Trace>,
}

impl std::ops::Deref for Judged<'_> {
    type Target = Facts;
    fn deref(&self) -> &Facts {
        self.facts
    }
}

impl<'a> Judged<'a> {
    /// `aac_check`: the AAC and Opus searches count (they are the option, or a doubt's second look).
    pub(crate) fn new(f: &'a Facts, aac_check: bool) -> Judged<'a> {
        let silent_above = f.above_floor.is_some_and(|a| a < 3.0) || f.high_floor.is_some_and(|h| h < 3.0);
        let pilot_on_19k = pilot_found(f.pilot_prominence);
        let shifted_pilot = shifted_pilot_of(f.shifted_candidate, pilot_on_19k);
        let speed = shifted_pilot.map(|hz| {
            let percent = (hz / 19000.0 - 1.0) * 100.0;
            let semitones = 12.0 * libm::log2(hz / 19000.0);
            format!(
                "playback {:.1} % too {} (pitch {:.2} semitone {})",
                percent.abs(),
                if percent < 0.0 { "slow" } else { "fast" },
                semitones.abs(),
                if semitones < 0.0 { "low" } else { "high" }
            )
        });
        // A wall where a lower sample rate's band ends (a resampler's filter), even when clipped
        // inter-sample peaks put some noise above it: the rate the file was upsampled from.
        let origin = f.cutoff.filter(|_| f.rate >= 88200.0).and_then(source_rate).filter(|&r| r * 1.5 < f.rate);
        // Real content stops around 20-24 kHz in a hi-res file. A wall below 30 kHz followed by an
        // empty floor counts as well, whatever the resampler's images do just above it (ffmpeg's
        // leave a tail 15 dB under the music at 25.7 kHz).
        let walled = f.rate >= 88200.0 && f.cutoff.is_some_and(|c| c < 30000.0) && f.hires.is_some_and(|h| h.low - h.high >= 40.0);
        let upsampled = f.hires.is_some_and(|h| h.low - h.high >= 40.0 && h.tail.unwrap_or(f64::INFINITY) <= h.high + 15.0) || origin.is_some() || walled;
        // A 48 kHz file whose content stops dead at the top of a 44.1 kHz band, with digital
        // silence above: possibly upsampled from 44.1 kHz (a 48 kHz converter's own filter may cut
        // there too, so only said).
        let from_44k = f.rate == 48000.0
            && f.cutoff.is_some_and(|c| (20900.0..=22150.0).contains(&c))
            && silent_above
            && f.holes.unwrap_or(0.0) < 5.0;
        let waste = wasted_bits(&f.wasted);
        let lossywav = waste.as_ref().is_some_and(is_lossywav);
        let comb = if f.zero_low { None } else { f.comb_stats.as_ref().and_then(comb_bits) };
        // 16-bit audio in a 24-bit file: the lowest 8 bits always zero, or a comb of values
        // (16-bit audio whose level was changed in a 24-bit editor).
        let padded: Option<String> = if f.zero_low {
            Some("24-bit file holding 16-bit audio: the lowest 8 bits are always zero".into())
        } else {
            // Teeth 32 values apart or more: 16-bit audio, even turned down by 18 dB.
            comb.filter(|c| c.0 <= 1.0 / 32.0).map(|c| {
                format!("24-bit file made from 16-bit audio with a level change: only about 1 sample value in {:.0} is ever used", 1.0 / c.0)
            })
        };
        // AAC only when no MP3 was found, Opus when neither was.
        let mp3_found = f.mp3_trace.is_some_and(|t| t.found());
        let aac_trace = if !aac_check || mp3_found { None } else { f.aac_trace };
        let opus_trace = if !aac_check || mp3_found || aac_trace.is_some_and(|t| crate::aactrace::found(&t)) { None } else { f.opus_trace };
        Judged {
            facts: f,
            silent_above,
            uniform: f.mp3_trace.is_some_and(|t| !t.found() && t.likelihood > crate::mp3trace::THRESHOLD),
            intensity: intensity_share(&f.intensity),
            upsampled,
            origin,
            from_44k,
            pilot_on_19k,
            shifted_pilot,
            line_whistle: line_whistle_of(f.line_candidate),
            speed,
            lossywav,
            waste: waste.map(|w| (w.share, w.mean_bits)),
            padded,
            comb,
            masked: masked_cutoff(&f.masked_bands),
            hires_gap: f.hires.map(|h| h.high - h.low),
            aac_trace,
            opus_trace,
        }
    }
}

/// The lines of the tooltip: the measures that matter, in the order they are looked at.
fn details(f: &Judged) -> Vec<String> {
    let mut details = Vec::new();
    if let Some(t) = f.mp3_trace.filter(|t| t.found()) {
        details.push(format!("MP3 coding traces found ({:.1} % of the bands, {:.0} standard deviations above chance)", t.likelihood * 100.0, t.contrast));
    }
    if f.frames == 0 {
        return details;
    }
    let nyquist = f.rate / 2.0;
    if let Some(e) = f.edge {
        details.push(match f.cutoff {
            Some(_) => format!("Edge of the lowpass: {e:.0} dB per octave"),
            None => format!("Treble roll-off (10 to 16 kHz): {e:.0} dB per octave"),
        });
    }
    match f.cutoff {
        Some(c) => details.push(format!("Steep lowpass at {:.1} kHz ({:.0} dB drop)", c / 1000.0, f.drop)),
        None => details.push(format!("No brick-wall lowpass between 10 kHz and {:.1} kHz", (nyquist - 700.0) / 1000.0)),
    }
    if let (Some(c), Some(holes)) = (f.cutoff, f.holes) {
        details.push(format!("Encoder holes just below the cutoff: {holes:.0} dB"));
        if let Some(a) = f.above_floor {
            details.push(format!("Floor above the cutoff: {a:+.0} dB over digital silence"));
        }
        if let Some(h) = f.high_floor.filter(|_| c < 18000.0) {
            details.push(format!("Floor at 18-19.5 kHz: {h:+.0} dB over digital silence"));
        }
    }
    if let Some(m) = f.masked.filter(|m| m.dithered()) {
        details.push(format!(
            "The music stops at {:.1} kHz; above it only steady noise ({:+.0} dB over digital silence{}), as the noise-shaped dither of a conversion to 16-bit leaves",
            m.cutoff / 1000.0,
            m.floor_above,
            m.rise.map(|r| format!(", rising {r:.0} dB toward 19 kHz")).unwrap_or_default()
        ));
    }
    if let Some(r) = f.roll {
        details.push(format!(
            "Edge of the treble: drops by {:.0} dB at most between 300 Hz bands; the band above follows the music at {:.2}",
            r.0, r.1
        ));
    }
    if let Some(h) = f.hf {
        details.push(format!("15-19.5 kHz level: {h:.0} dB relative to 1-4 kHz"));
    }
    if f.pilot_on_19k {
        details.push("19 kHz FM stereo pilot tone found".into());
    } else if let (Some(hz), Some(s)) = (f.shifted_pilot, &f.speed) {
        details.push(format!("19 kHz FM stereo pilot tone found at {:.2} kHz: {s}", hz / 1000.0));
    }
    if let Some(hz) = f.line_whistle {
        details.push(format!("Tone at {:.2} kHz: TV line frequency (TV simulcast, or a TV set near the recorder)", hz / 1000.0));
    }
    if let Some(gap) = f.hires_gap {
        details.push(format!("Level 27-30 kHz vs 18.5-20.5 kHz: {gap:.0} dB"));
    }
    if let Some(c) = f.cutoff.filter(|_| f.from_44k) {
        details.push(format!("Content stops at {:.1} kHz with digital silence above: possibly upsampled from a 44.1 kHz source", c / 1000.0));
    }
    if let Some(p) = &f.padded {
        details.push(p.clone());
    } else if let Some(c) = f.comb.filter(|c| c.1 < 23.0) {
        details.push(format!("About {:.0} bits really used (1 sample value in {:.0} occurs)", c.1, 1.0 / c.0));
    }
    if let Some(w) = f.waste.filter(|_| f.lossywav) {
        details.push(format!("lossyWAV: lowest bits zeroed in {:.0} % of the blocks ({:.1} bits on average)", w.0 * 100.0, w.1));
    }
    details
}

/// A candidate MP3 grid (C3): the rate over the detector's threshold and a position that stands
/// out from the others by more than this many robust standard deviations, without reaching the
/// detector's own (`mp3trace::MIN_CONTRAST`). Set from the contrast of the tracks of known
/// truth: lossless 7.9 at most (1 054 tracks of the tuning group, Officiels, rolloffbank,
/// Iron Maiden V10), notes/REFONTE-E7.md.
pub(crate) const C3_CONTRAST: f64 = 8.5;

pub(crate) fn is_candidate(t: &crate::mp3trace::Trace) -> bool {
    t.likelihood > crate::mp3trace::THRESHOLD && t.contrast > C3_CONTRAST && !t.found()
}

/// A clue, by its name in the plan of the decision (notes/PLAN-refonte-decision.md, §1.2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Id {
    /// Coding: a grid found (C1), lossyWAV (C2), a candidate grid (C3, §2.3), a
    /// rate over the threshold at every position (C4), a search that found nothing (C5).
    C1(Codec),
    C2,
    C3(Codec),
    C4,
    C5(Codec),
    /// Structure: encoder holes of 15 dB and more (S1a), 10-15 dB (S1b), 8-10 dB (S1c: no
    /// clue, a reason to doubt), under 5 dB (S1n).
    S1a,
    S1b,
    S1c,
    S1n,
    /// Digital silence above a wall under 19.5 kHz (S2a); a low floor or noise-shaped dither
    /// above it (S2b).
    S2a,
    S2b,
    /// Intensity stereo (S3); hiss above the cutoff up to 19 kHz (S4; `low`: a cutoff under
    /// 19.5 kHz).
    S3,
    S4 { low: bool },
    /// Too little treble above 15 kHz to see an encoder's lowpass (H1), little (H2).
    H1,
    H2,
    /// The shape of the lowpass: shown, never decides.
    F,
    /// Origin: FM pilot, no hi-res content, 16-bit in 24, 48 kHz from 44.1 kHz, a 32 kHz
    /// source, a TV line whistle.
    O1,
    O2,
    O3,
    O4,
    O5,
    O6,
    /// Digital processing last: a gradual roll-off then digital silence (T1), a wall at
    /// 19.5-21.5 kHz without hiss above (T3), noise-shaped dither after a sharp stop (T4).
    T1,
    T3,
    T4,
}

impl Id {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Id::C1(_) => "C1",
            Id::C2 => "C2",
            Id::C3(_) => "C3",
            Id::C4 => "C4",
            Id::C5(_) => "C5",
            Id::S1a => "S1a",
            Id::S1b => "S1b",
            Id::S1c => "S1c",
            Id::S1n => "S1n",
            Id::S2a => "S2a",
            Id::S2b => "S2b",
            Id::S3 => "S3",
            Id::S4 { .. } => "S4",
            Id::H1 => "H1",
            Id::H2 => "H2",
            Id::F => "F",
            Id::O1 => "O1",
            Id::O2 => "O2",
            Id::O3 => "O3",
            Id::O4 => "O4",
            Id::O5 => "O5",
            Id::O6 => "O6",
            Id::T1 => "T1",
            Id::T3 => "T3",
            Id::T4 => "T4",
        }
    }

    pub(crate) fn family(self) -> Family {
        match self.code().as_bytes()[0] {
            b'C' => Family::Coding,
            b'S' | b'H' => Family::Structure,
            b'F' => Family::Shape,
            b'O' => Family::Origin,
            _ => Family::Processing,
        }
    }

    /// What it measures, for the reasoning of a pair.
    fn nature(self) -> &'static str {
        match self {
            Id::S1a | Id::S1b => "how the top bands behave from frame to frame",
            Id::S2a | Id::S2b => "what came after the lowpass",
            Id::S3 => "how the two channels relate",
            _ => "the rounding grid of an encoder",
        }
    }
}

/// One clue found in a file.
#[derive(Clone, Debug)]
pub(crate) struct Finding {
    pub(crate) id: Id,
    pub(crate) side: Side,
    pub(crate) strength: Strength,
    pub(crate) text: String,
}

/// Why a file is "Unclear". The first three are the transfer chain's (a filter, an analog stage
/// or a noise reducer that hides what came before); the others the music's or the measure's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    Wall,
    CutHiss,
    LowFloor,
    ShallowHoles,
    Treble,
    /// Nothing in the track itself, but most of the show carries lossy evidence.
    Show,
}

impl Reason {
    pub(crate) fn chain(self) -> bool {
        matches!(self, Reason::Wall | Reason::CutHiss | Reason::LowFloor)
    }
}

/// The five states of a file's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// Lossy, red: a coding proof, or two independent clues.
    R,
    /// Lossy?, orange: one family of clues.
    O,
    /// Unclear, blue.
    U(Reason),
    /// Likely lossless, green.
    L2,
    /// Lossless (or FM broadcast), green.
    L1,
}

impl State {
    pub(crate) fn confidence(self) -> Confidence {
        match self {
            State::R | State::L1 => Confidence::High,
            State::O | State::L2 => Confidence::Medium,
            State::U(_) => Confidence::Low,
        }
    }
}

/// A file's state, and the admitted pair that made it lossy (if one did).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Class {
    pub(crate) state: State,
    pub(crate) pair: Option<(Id, Id)>,
}

/// The pairs of independent clues that prove a lossy source together (closed list, §1.3).
const PAIRS: [(&[&str], &[&str]); 5] = [
    (&["S1a", "S1b"], &["S2a"]),
    (&["S1a"], &["S2b"]),
    (&["S1a", "S1b"], &["S3"]),
    (&["S1a", "S1b"], &["C3"]),
    (&["S3"], &["C3"]),
];

/// The state from the clues alone (§1.3, steps 3 to 7).
pub(crate) fn classify(found: &[Finding]) -> Class {
    let find = |codes: &[&str]| found.iter().find(|f| codes.contains(&f.id.code())).map(|f| f.id);
    let has = |code: &str| find(&[code]).is_some();
    let class = |state| Class { state, pair: None };
    if has("C1") || has("C2") {
        return class(State::R);
    }
    for (a, b) in PAIRS {
        if let (Some(x), Some(y)) = (find(a), find(b)) {
            return Class { state: State::R, pair: Some((x, y)) };
        }
    }
    if found.iter().any(|f| f.side == Side::Lossy && f.strength == Strength::Moderate) {
        return class(State::O);
    }
    let cut_hiss = found.iter().any(|f| f.id == Id::S4 { low: true }) && !has("O1");
    let unclear = if has("T3") {
        Some(Reason::Wall)
    } else if cut_hiss {
        Some(Reason::CutHiss)
    } else if has("S2b") {
        Some(Reason::LowFloor)
    } else if has("S1c") {
        Some(Reason::ShallowHoles)
    } else if has("H1") {
        Some(Reason::Treble)
    } else {
        None
    };
    match unclear {
        Some(reason) => class(State::U(reason)),
        None if has("H2") || has("T1") || has("T4") => class(State::L2),
        None => class(State::L1),
    }
}

/// The clues of a file, family by family.
pub(crate) fn findings(f: &Judged) -> Vec<Finding> {
    let khz = |c: f64| format!("{:.1} kHz", c / 1000.0);
    let mut out = Vec::new();
    let mut add = |id: Id, side: Side, strength: Strength, text: String| out.push(Finding { id, side, strength, text });

    // C: coding.
    let mp3 = f.mp3_trace.filter(|t| t.found());
    let aac = f.aac_trace.filter(crate::aactrace::found);
    let opus = f.opus_trace.filter(|t| t.found());
    let share = |t: &crate::mp3trace::Trace| format!("{:.1} % of the bands, {:.0} standard deviations above chance", t.likelihood * 100.0, t.contrast);
    if let Some(t) = mp3 {
        add(Id::C1(Codec::Mp3), Side::Lossy, Strength::Strong, format!("MP3 coding traces: the sound falls back onto an MP3 encoder's rounding grid ({})", share(&t)));
    }
    // A candidate: the rate is over the threshold and the best position stands out, though not
    // enough to be found. Weak, and never alone: only in an admitted pair, or promoted by the
    // show (a track whose grid continues another's).
    if let Some(t) = f.mp3_trace.filter(is_candidate) {
        add(
            Id::C3(Codec::Mp3),
            Side::Lossy,
            Strength::Weak,
            format!(
                "A weak MP3 grid at position {}: {:.1} % of the bands round onto it, {:.1} standard deviations above chance (a grid counts as found from {:.0})",
                t.offset,
                t.likelihood * 100.0,
                t.contrast,
                crate::mp3trace::MIN_CONTRAST
            ),
        );
    }
    if let Some(t) = aac {
        add(Id::C1(Codec::Aac), Side::Lossy, Strength::Strong, format!("AAC coding traces: the sound falls back onto an AAC encoder's rounding grid ({})", share(&t)));
    }
    if let Some(t) = opus {
        add(Id::C1(Codec::Opus), Side::Lossy, Strength::Strong, format!("Opus coding traces: {} of the {} frames analysed are exactly Opus codewords", t.clean, t.frames));
    }
    if f.lossywav {
        let (share, bits) = f.waste.unwrap_or_default();
        add(
            Id::C2,
            Side::Lossy,
            Strength::Strong,
            format!("Lowest bits zeroed block by block, as lossyWAV does ({:.0} % of the blocks, {bits:.1} bits on average)", share * 100.0),
        );
    }
    if let Some(t) = f.mp3_trace.filter(|_| f.uniform) {
        add(
            Id::C4,
            Side::Neutral,
            Strength::Weak,
            format!(
                "No MP3 grid at any position, yet {:.1} % of the bands round onto it everywhere, more than genuine audio shows: noise reduction, or a lossy stage whose grid was erased since",
                t.likelihood * 100.0
            ),
        );
    }
    let none = |name: &str| format!("No {name} coding traces (they do not survive effects or resampling after the decoder)");
    if f.mp3_trace.is_some_and(|t| !t.found() && !f.uniform) {
        add(Id::C5(Codec::Mp3), Side::Lossless, Strength::Weak, none("MP3"));
    }
    if f.aac_trace.is_some_and(|t| !crate::aactrace::found(&t) && t.likelihood <= crate::aactrace::THRESHOLD) {
        add(Id::C5(Codec::Aac), Side::Lossless, Strength::Weak, none("AAC"));
    }
    if f.opus_trace.is_some_and(|t| !t.found()) {
        add(Id::C5(Codec::Opus), Side::Lossless, Strength::Weak, none("Opus"));
    }

    // S: structure. What lies above a cutoff: digital silence, hiss up to 19 kHz, or neither
    // (a low floor, or nothing measurable so close to the top of the band).
    let hiss = f.above_floor.is_some_and(|a| a >= 10.0) && f.high_floor.is_none_or(|h| h >= 10.0);
    if let Some(c) = f.cutoff {
        let h = f.holes.unwrap_or(0.0);
        match h {
            h if h >= 15.0 => add(Id::S1a, Side::Lossy, Strength::Moderate, format!("Encoder holes of {h:.0} dB below the {} lowpass", khz(c))),
            h if h >= 10.0 => add(
                Id::S1b,
                Side::Lossy,
                Strength::Moderate,
                format!("Encoder holes of {h:.0} dB below the {} lowpass (shallower than 15 dB: weaker on their own)", khz(c)),
            ),
            h if h >= 8.0 => add(
                Id::S1c,
                Side::Lossy,
                Strength::Weak,
                format!("Holes of {h:.0} dB below the {} lowpass: deeper than analog sources leave (about 5 dB), too shallow for an encoder's mark", khz(c)),
            ),
            h if h < 5.0 => add(Id::S1n, Side::Lossless, Strength::Weak, format!("No encoder holes below the {} lowpass ({h:.0} dB)", khz(c))),
            _ => {}
        }
        // Digital silence is a clue on its own under 19.5 kHz; higher up a resampler leaves it
        // too, but it still pairs with encoder holes, which no resampler digs (amendment 1).
        // A low floor is a clue only with deep holes: a noise reducer lowering a tape's hiss
        // leaves the same floor (amendment 2).
        if f.silent_above && c < 19500.0 {
            add(Id::S2a, Side::Lossy, Strength::Moderate, format!("Digital silence above the {} lowpass: nothing analog came after it", khz(c)));
        } else if f.silent_above {
            add(
                Id::S2a,
                Side::Lossy,
                Strength::Weak,
                format!("Digital silence above the {} lowpass: a resampler leaves it too, so it counts only with encoder holes", khz(c)),
            );
        } else if hiss {
            add(
                Id::S4 { low: c < 19500.0 },
                Side::Neutral,
                Strength::Moderate,
                format!(
                    "Hiss goes on above the {} lowpass ({:+.0} dB over digital silence) up to 19 kHz: an analog stage came after it, whatever came before",
                    khz(c),
                    f.above_floor.unwrap_or(0.0)
                ),
            );
        } else if let Some(a) = f.above_floor.filter(|_| c < 19500.0 || f.holes.is_some_and(|h| h >= 15.0)) {
            // From 19.5 kHz up, only said next to holes of 15 dB and more, its only partner
            // (amendment 5): alone it is the resampler's wall (T3).
            add(
                Id::S2b,
                Side::Lossy,
                Strength::Weak,
                format!(
                    "Only a low floor above the {} lowpass ({a:+.0} dB over digital silence), neither digital silence nor hiss: a noise reducer leaves it too, so it counts only with deep encoder holes",
                    khz(c)
                ),
            );
        }
        if (19500.0..21500.0).contains(&c) && !hiss {
            let above = if f.silent_above { "digital silence" } else { "no hiss" };
            add(
                Id::T3,
                Side::Cap,
                Strength::Moderate,
                format!("A wall at {} with {above} above it: a resampler's or converter's filter came last, and a high-bitrate encoder looks the same", khz(c)),
            );
        }
        if f.roll.is_some_and(|r| r.0 <= ANALOG_STEP) && f.silent_above {
            add(
                Id::T1,
                Side::Cap,
                Strength::Moderate,
                format!("Gradual roll-off at {} followed by digital silence: a digital process came after the last analog stage", khz(c)),
            );
        }
    }
    let floor_s2b = f.cutoff.is_some_and(|c| c < 19500.0) && !f.silent_above && !hiss && f.above_floor.is_some();
    if let Some(m) = f.masked.filter(|m| m.dithered()) {
        if m.cutoff < 19500.0 {
            if !floor_s2b {
                add(
                    Id::S2b,
                    Side::Lossy,
                    Strength::Moderate,
                    format!(
                        "The music stops at {}; above it only steady noise{}, the noise-shaped dither of a conversion to 16-bit, which hides a lowpass",
                        khz(m.cutoff),
                        m.rise.map(|r| format!(" rising {r:.0} dB toward 19 kHz")).unwrap_or_default()
                    ),
                );
            }
        } else {
            add(
                Id::T4,
                Side::Cap,
                Strength::Moderate,
                format!("Noise-shaped dither above a sharp stop at {}: a conversion to 16-bit came last, which would hide a high-bitrate encoder's lowpass", khz(m.cutoff)),
            );
        }
    }
    if let Some(s) = f.intensity {
        add(
            Id::S3,
            Side::Lossy,
            Strength::Moderate,
            format!("Treble locked between the channels (joint-stereo coding) in {:.0} % of the loud frames, while the mids are real stereo", s * 100.0),
        );
    }
    let treble = |h: f64| format!("{h:.0} dB relative to 1-4 kHz");
    match f.hf {
        Some(h) if h < if f.mp3_trace.is_some() { TREBLE_UNCLEAR } else { TREBLE_PLAIN } => {
            add(Id::H1, Side::Neutral, Strength::Moderate, format!("Almost no treble above 15 kHz ({}): an encoder's lowpass cannot be seen", treble(h)))
        }
        Some(h) if f.mp3_trace.is_some() && h < TREBLE_LIKELY => {
            add(Id::H2, Side::Neutral, Strength::Weak, format!("Little treble above 15 kHz ({}), where an encoder's lowpass shows best", treble(h)))
        }
        _ => {}
    }

    // F: the shape of the lowpass, shown only.
    match (f.cutoff, f.roll) {
        (Some(c), Some((step, coherence))) if step <= ANALOG_STEP && coherence >= ANALOG_COHERENCE => add(
            Id::F,
            Side::Lossless,
            Strength::Weak,
            format!("Gradual roll-off at {} with the music still above it, as analog sources leave (lossy files can too)", khz(c)),
        ),
        (Some(c), Some((step, coherence))) if step > ANALOG_STEP && coherence < 0.3 => add(
            Id::F,
            Side::Lossy,
            Strength::Weak,
            format!("A wall at {}: {step:.0} dB between neighbouring bands and nothing of the music above, where encoders cut (digital filters too)", khz(c)),
        ),
        (Some(c), _) => add(Id::F, Side::Lossy, Strength::Weak, format!("Brick-wall lowpass at {}, where encoders cut (digital filters too)", khz(c))),
        (None, _) if f.hf.is_some_and(|h| h >= TREBLE_PLAIN) && !f.upsampled => {
            add(Id::F, Side::Lossless, Strength::Weak, "No brick-wall lowpass, with treble present above 15 kHz".into())
        }
        _ => {}
    }
    if let Some(s) = crate::lossless::encoder_settings(f) {
        add(Id::F, Side::Lossy, Strength::Weak, format!("The cutoff matches the lowpass of {s}"));
    }

    // O: origin.
    if f.pilot_on_19k || f.shifted_pilot.is_some() {
        let speed = f.speed.as_ref().map(|s| format!(" (shifted: {s})")).unwrap_or_default();
        add(Id::O1, Side::Origin, Strength::Moderate, format!("19 kHz FM stereo pilot tone{speed}: recorded off air"));
    }
    if f.upsampled {
        add(Id::O2, Side::Origin, Strength::Moderate, capital(&no_hires(f.cutoff, f.origin)));
    }
    if let Some(p) = &f.padded {
        add(Id::O3, Side::Origin, Strength::Moderate, p.clone());
    }
    if let Some(c) = f.cutoff.filter(|_| f.from_44k) {
        add(Id::O4, Side::Origin, Strength::Weak, format!("Content stops at {} with digital silence above: possibly a 44.1 kHz source upsampled to 48 kHz", khz(c)));
    }
    if let Some(c) = f.cutoff.filter(|c| (14000.0..=16000.0).contains(c) && f.silent_above) {
        add(Id::O5, Side::Origin, Strength::Weak, format!("A wall at {} with digital silence above: also where a 32 kHz source ends", khz(c)));
    }
    if let Some(hz) = f.line_whistle {
        add(Id::O6, Side::Origin, Strength::Weak, format!("Tone at {:.2} kHz: TV line frequency (TV simulcast, or a TV set near the recorder)", hz / 1000.0));
    }
    out
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|first| first.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// The label, colour and sentence of a file's verdict.
fn wording(class: &Class, found: &[Finding], f: &Judged) -> (String, Level, String) {
    let has = |code: &str| found.iter().any(|x| x.id.code() == code);
    let khz = |c: f64| format!("{:.1}", c / 1000.0);
    let holes = f.holes.unwrap_or(0.0);
    // The lowpass a doubt is about: the wall, or where the music stops under dither.
    let cut = f.cutoff.or(f.masked.filter(|m| m.dithered()).map(|m| m.cutoff)).unwrap_or(0.0);
    let pilot = has("O1");
    let hiss = f.above_floor.is_some_and(|a| a >= 10.0) && f.high_floor.is_none_or(|h| h >= 10.0);
    let upstream = (has("S1a") || has("S1b")) && found.iter().any(|x| matches!(x.id, Id::S4 { .. }));
    // The clues, said as part of a sentence.
    let clue = |id: Id| -> String {
        match id {
            Id::S1a | Id::S1b => format!("encoder holes of {holes:.0} dB below the {} kHz lowpass", khz(cut)),
            Id::S2a => "digital silence above it".into(),
            Id::S2b if f.cutoff.is_some_and(|c| c < 19500.0) && !f.silent_above && !hiss => {
                format!("only a low floor above it ({:+.0} dB over digital silence)", f.above_floor.unwrap_or(0.0))
            }
            Id::S2b => "only steady noise above it rising toward 19 kHz, the noise-shaped dither of a conversion to 16-bit, which hides the lowpass".into(),
            Id::S3 => format!("the treble locked between the channels in {:.0} % of the loud frames (joint-stereo coding)", f.intensity.unwrap_or(0.0) * 100.0),
            Id::C3(_) => "a weak coding grid".into(),
            _ => String::new(),
        }
    };
    let (mut label, mut level, mut sentence) = match class.state {
        State::R => {
            let grid = |codec| found.iter().find(|x| x.id == Id::C1(codec));
            if has("C2") {
                let (share, bits) = f.waste.unwrap_or_default();
                (
                    "Lossy (lossyWAV)".to_string(),
                    format!(
                        "Processed by lossyWAV: in {:.0} % of the blocks the lowest bits ({bits:.1} on average) are zero, block by block, the mark lossyWAV leaves when it discards detail to make smaller FLAC files",
                        share * 100.0
                    ),
                )
            } else if let Some(name) = [(Codec::Mp3, "MP3"), (Codec::Aac, "AAC"), (Codec::Opus, "Opus")].iter().find(|(c, _)| grid(*c).is_some()).map(|x| x.1) {
                let detail = match (name, f.mp3_trace.filter(|t| t.found()), f.aac_trace.filter(crate::aactrace::found), f.opus_trace) {
                    ("MP3", Some(t), _, _) | ("AAC", _, Some(t), _) => format!(
                        "the file still holds the rounding pattern of an {name} encoder ({:.1} % of the bands match it, {:.0} standard deviations above chance), which only a lossy file converted back leaves",
                        t.likelihood * 100.0,
                        t.contrast
                    ),
                    (_, _, _, Some(t)) => format!("{} of the {} frames analysed are exactly Opus codewords, which only a lossy file converted back leaves", t.clean, t.frames),
                    _ => String::new(),
                };
                let source = if name == "Opus" { "an Opus stream (YouTube, streaming)".to_string() } else { format!("an {name}") };
                (format!("Lossy ({name} traces)"), format!("Decoded from {source}: {detail}"))
            } else {
                let (a, b) = class.pair.unwrap_or((Id::S1a, Id::S2a));
                let label = match (cut > 0.0, upstream) {
                    (true, true) => format!("Lossy upstream {} kHz", khz(cut)),
                    (true, false) => format!("Lossy {} kHz", khz(cut)),
                    _ => "Lossy (joint-stereo treble)".into(),
                };
                let head = if pilot { "FM capture of a lossy-coded program" } else { "Lossy source" };
                (label, format!("{head}: {} and {}, two independent clues that a lossy encoder leaves together", clue(a), clue(b)))
            }
            .into_level(Level::Error)
        }
        State::O => {
            let label = if upstream {
                format!("Lossy? upstream {} kHz", khz(cut))
            } else if has("S1a") || has("S1b") || has("S2a") || has("S2b") {
                format!("Lossy? {} kHz", khz(cut))
            } else {
                "Lossy? (joint-stereo treble)".into()
            };
            let moderate: Vec<Id> = found.iter().filter(|x| x.side == Side::Lossy && x.strength == Strength::Moderate).map(|x| x.id).collect();
            let sentence = if upstream {
                let head = if pilot { "FM capture of a lossy-coded program (19 kHz pilot found)" } else { "Probably a lossy source upstream" };
                format!(
                    "{head}: {}, with steady noise above it that came later (an analog stage, or dither added when converting); one family of clues only, so not proven",
                    clue(moderate[0])
                )
            } else {
                let said = moderate.iter().map(|&id| clue(id)).collect::<Vec<_>>().join(", and ");
                let single = moderate.len() == 1;
                if has("S1a") || has("S1b") {
                    format!("Possibly lossy: {said}; {}", if single { "one family of clues only, so not proven" } else { "not a pair of clues that proves it" })
                } else if has("S2a") || has("S2b") {
                    let lead = if f.cutoff.is_some() { "brick-wall lowpass at" } else { "the music stops at" };
                    format!(
                        "Possibly lossy: {lead} {} kHz with {said}, as a lossy file converted back leaves; but no encoder holes{}",
                        khz(cut),
                        if single { ", and a digital lowpass filter looks the same" } else { ": not a pair of clues that proves it" }
                    )
                } else {
                    format!("Possibly lossy: {said}, as lossy codecs code it at moderate bitrates; no coding grid found, so not proven")
                }
            };
            (label, sentence).into_level(Level::Warn)
        }
        State::U(reason) => {
            let c = khz(f.cutoff.unwrap_or(0.0));
            let sentence = match reason {
                Reason::Wall => format!(
                    "Cannot tell: a wall at {c} kHz with {} above it: a resampler's or converter's filter and a high-bitrate encoder look the same",
                    if f.silent_above { "digital silence" } else { "no hiss" }
                ),
                Reason::CutHiss => format!(
                    "Cannot tell: treble cut at {c} kHz with hiss above and no FM pilot: an FM capture or a lossy source followed by an analog stage look the same"
                ),
                Reason::LowFloor => format!(
                    "Cannot tell: treble cut at {c} kHz with only a low floor above it ({:+.0} dB over digital silence), neither digital silence nor hiss: a lossy source and a noise-reduced analog source look the same",
                    f.above_floor.unwrap_or(0.0)
                ),
                Reason::ShallowHoles => format!(
                    "Cannot tell: holes of {holes:.0} dB below the {c} kHz lowpass, deeper than analog sources leave but too shallow to prove an encoder"
                ),
                Reason::Show => String::new(),
                Reason::Treble => {
                    "Cannot tell: almost no content above 15 kHz (common with audience and analog-tape recordings), so encoder traces cannot be seen".into()
                }
            };
            ("Unclear".to_string(), sentence).into_level(Level::Info)
        }
        State::L2 | State::L1 => {
            let mut buts: Vec<String> = Vec::new();
            if has("H2") {
                buts.push("little treble above 15 kHz, where an encoder's lowpass shows best".into());
            }
            for x in found.iter().filter(|x| matches!(x.id, Id::T1 | Id::T4)) {
                buts.push(lower(&x.text));
            }
            let sentence = if !buts.is_empty() {
                format!("Probably lossless: no lossy clue and no MP3 coding traces, but {}", buts.join("; and "))
            } else if pilot {
                match &f.speed {
                    Some(s) => format!("FM broadcast recording (19 kHz pilot tone found, shifted: {s}): lossless capture of an analog source"),
                    None => "FM broadcast recording (19 kHz pilot tone found): lossless capture of an analog source".into(),
                }
            } else if let Some(c) = f.cutoff {
                format!(
                    "No sign of lossy encoding: the lowpass at {} kHz is the converter's anti-alias filter (hiss goes on above it, no encoder holes)",
                    khz(c)
                )
            } else {
                "No sign of lossy encoding: no encoder lowpass, high frequencies present".into()
            };
            let label = if class.state == State::L2 {
                crate::lossless::LIKELY_LOSSLESS
            } else if pilot {
                "FM broadcast"
            } else {
                "Lossless"
            };
            (label.to_string(), sentence).into_level(Level::Ok)
        }
    };
    let lossless = matches!(class.state, State::L1 | State::L2);
    // Origin: in the label of a lossless file (orange), in the sentence otherwise.
    if lossless && has("O2") {
        label = LOSSLESS_NO_HIRES.into();
        level = Level::Warn;
        sentence = format!("Lossless, but no hi-res content: no sign of lossy encoding; {}", no_hires(f.cutoff, f.origin));
    } else if has("O2") {
        sentence.push_str(&format!("; also {}", no_hires(f.cutoff, f.origin)));
    }
    if let Some(p) = f.padded.as_ref().filter(|_| has("O3")) {
        if lossless && !has("O2") {
            label = LOSSLESS_PADDED.into();
            level = Level::Warn;
            sentence = format!("Lossless, but not true 24-bit: no sign of lossy encoding; {p}");
        } else {
            sentence.push_str("; also 16-bit audio in a 24-bit file");
            if level == Level::Ok {
                level = Level::Warn;
            }
        }
    }
    if let Some(c) = f.cutoff.filter(|_| has("O4")) {
        sentence.push_str(&format!("; the content stops at {} kHz with digital silence above, as a 44.1 kHz source upsampled to 48 kHz leaves", khz(c)));
    }
    if has("O5") {
        sentence.push_str("; a wall there with digital silence above is also where a 32 kHz source ends");
    }
    if pilot && !matches!(class.state, State::L1) && !(upstream || class.pair.is_some()) {
        sentence.push_str("; 19 kHz FM pilot found: recorded off air");
    }
    if matches!(class.state, State::O | State::U(_)) && f.mp3_trace.is_some_and(|t| !t.found()) && f.aac_trace.is_some_and(|t| !crate::aactrace::found(&t)) {
        if f.opus_trace.is_some_and(|t| !t.found()) {
            sentence.push_str("; no MP3, AAC or Opus coding traces, so not an MP3, AAC or Opus converted straight back");
        } else {
            sentence.push_str("; no MP3 or AAC coding traces, so not an MP3 or AAC converted straight back");
        }
    }
    (label, level, sentence)
}

trait IntoLevel {
    fn into_level(self, level: Level) -> (String, Level, String);
}

impl IntoLevel for (String, String) {
    fn into_level(self, level: Level) -> (String, Level, String) {
        (self.0, level, self.1)
    }
}

fn lower(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|first| first.to_lowercase().chain(c).collect()).unwrap_or_default()
}

/// The steps from the clues to the verdict.
fn reasoning(class: &Class, found: &[Finding], f: &Judged) -> Vec<String> {
    let has = |code: &str| found.iter().any(|x| x.id.code() == code);
    let text = |code: &str| found.iter().find(|x| x.id.code() == code).map(|x| x.text.clone()).unwrap_or_default();
    let khz = |c: f64| format!("{:.1} kHz", c / 1000.0);
    let cut = f.cutoff.map(khz).unwrap_or_default();
    let holes = f.holes.unwrap_or(0.0);
    let above = f.above_floor.unwrap_or(0.0);
    let mut r: Vec<String> = Vec::new();
    if has("C2") {
        r.push("In most blocks of samples the lowest bits are all zero, and how many changes from block to block with the loudness: that is lossyWAV's mark. It discards the detail it judges inaudible so that FLAC can store the file in fewer bits; the result is lossy even though it is a FLAC file.".into());
    }
    for (t, name, rest) in [
        (f.mp3_trace.filter(|t| t.found()), "an MP3 encoder's filter bank", "Genuine audio shows about 2 % at no particular position: this file was decoded from an MP3 and converted back to a lossless format, whatever its spectrum looks like."),
        (f.aac_trace.filter(crate::aactrace::found), "an AAC encoder's transform", "Genuine audio shows no such position: this file was decoded from an AAC and converted back to a lossless format."),
    ] {
        if let Some(t) = t {
            r.push(format!(
                "Analysed again with {name}, the sound falls back onto the encoder's rounding grid in {:.1} % of the bands at one position (sample {}), {:.0} standard deviations above the other positions. {rest}",
                t.likelihood * 100.0,
                t.offset,
                t.contrast
            ));
        }
    }
    if let Some(t) = f.opus_trace.filter(|t| t.found()) {
        r.push(format!(
            "Analysed again with Opus's transform at 48 kHz, {} of the {} frames are exactly Opus codewords: in every band, a vector of small integers. Genuine audio never is: this file was decoded from an Opus stream (YouTube and most streaming services use Opus) and converted back to a lossless format.",
            t.clean, t.frames
        ));
    }
    if has("C1") || has("C2") {
        r.push("A coding proof decides on its own: no other clue can overrule it.".into());
    }
    match class.state {
        State::R if class.pair.is_some() => {
            let (a, b) = class.pair.unwrap_or((Id::S1a, Id::S2a));
            let say = |id: Id| found.iter().find(|x| x.id == id).map(|x| x.text.clone()).unwrap_or_default();
            r.push(format!("{}.", say(a)));
            r.push(format!("{}.", say(b)));
            r.push(format!(
                "These two clues measure different things ({}; {}), and a lossy encoder leaves both: together they prove a lossy source.",
                a.nature(),
                b.nature()
            ));
        }
        State::O => {
            for x in found.iter().filter(|x| x.side == Side::Lossy && x.strength == Strength::Moderate) {
                r.push(format!("{}.", x.text));
            }
            if has("S1a") || has("S1b") {
                r.push("Encoder holes are the fingerprint of a perceptual encoder (MP3, MP2, AAC switch the top bands off and on); FM and analog recordings stay under about 5 dB.".into());
            }
            if found.iter().any(|x| matches!(x.id, Id::S4 { .. })) && (has("S1a") || has("S1b")) {
                r.push(format!("Yet steady hiss ({above:+.0} dB over digital silence) fills the band above the cutoff. An encoder removes everything above its lowpass, so this hiss was added afterwards, by an analog stage (tuner, sound card) or as dither: the content itself may have been lossy-coded upstream."));
                if let Some(s) = f.stereo.filter(|s| *s < -18.0) {
                    r.push(format!("The program is nearly mono: the difference channel is {:.0} dB below the sum (concert mixes usually sit 4 to 10 dB below), as radio streams often are.", -s));
                }
            }
            if has("S2a") || has("S2b") {
                r.push("A digital lowpass filter, as some remasters use, gives the same picture as an encoder's lowpass.".into());
            }
            r.push("This is one family of clues only: a doubt, not a proof. A second, independent clue (encoder holes with digital silence above, intensity stereo, coding traces) would make it certain. Compare with other versions of the show if you can.".into());
        }
        State::U(reason) => match reason {
            Reason::Wall => {
                r.push(format!("{}.", text("T3")));
                r.push(format!("No encoder holes below it ({holes:.0} dB) and no other lossy clue: a lossless source resampled (or filtered digitally) at the end of the chain and a lossy file encoded at a high bitrate look the same here. Neither lossy nor lossless can be shown."));
            }
            Reason::CutHiss => {
                r.push(format!("A lowpass at {cut} with hiss above it ({above:+.0} dB over digital silence) up to 19 kHz, and no 19 kHz FM pilot."));
                r.push(format!("No encoder holes deep enough to tell ({holes:.0} dB) and no digital silence above the cutoff: an FM capture whose pilot was filtered out, or a lossy source followed by an analog stage (a tape, a sound card), look the same. Neither lossy nor lossless can be shown."));
            }
            Reason::LowFloor => {
                r.push(format!("{}.", text("S2b")));
                r.push(format!("No encoder holes deep enough to pair with it ({holes:.0} dB) and no other lossy clue: neither lossy nor lossless can be shown."));
            }
            Reason::ShallowHoles => {
                r.push(format!("{}.", text("S1c")));
                r.push("No second clue: neither lossy nor lossless can be shown.".into());
            }
            Reason::Show => {}
            Reason::Treble => {
                r.push(format!("There is almost nothing above 15 kHz ({:.0} dB relative to 1-4 kHz).", f.hf.unwrap_or(0.0)));
                r.push("Encoder traces live up there, so neither lossy nor lossless can be shown. Audience and cassette recordings often look like this; the spectrogram and the lineage are the best clues.".into());
            }
        },
        State::L2 | State::L1 => {
            if f.cutoff.is_some() && found.iter().any(|x| matches!(x.id, Id::S4 { .. })) {
                r.push(format!("A lowpass at {cut}, but no encoder holes ({holes:.0} dB) and hiss goes on above it ({above:+.0} dB over digital silence)."));
                r.push("That is the converter's anti-alias filter, not an encoder's lowpass.".into());
            } else if f.cutoff.is_none() {
                r.push("No brick-wall lowpass: lossy encoders always cut the top of the spectrum.".into());
            }
            if has("O1") {
                r.push("The 19 kHz stereo pilot tone only exists on air: this was recorded from an FM broadcast, and no lossy clue was found.".into());
                if let Some(s) = &f.speed {
                    r.push(format!("The pilot is not at exactly 19 000 Hz: the tape was played back at the wrong speed ({s}). A speed correction would put the music back in tune."));
                }
            }
            if has("H2") {
                r.push(format!(
                    "There is little treble above 15 kHz ({:.0} dB relative to 1-4 kHz), where an encoder's lowpass shows best: probably lossless rather than certain. Audience, cassette and FM recordings often look like this.",
                    f.hf.unwrap_or(0.0)
                ));
            } else if f.hf.is_some() && f.cutoff.is_none() {
                r.push(format!("High frequencies are present ({:.0} dB relative to 1-4 kHz above 15 kHz), so encoder traces would show.", f.hf.unwrap_or(0.0)));
            }
            for code in ["T1", "T4"] {
                if has(code) {
                    r.push(format!("{}. This caps the verdict at \"Likely lossless\".", text(code)));
                }
            }
        }
        State::R => {}
    }
    for code in ["O2", "O3", "O4", "O5", "O6"] {
        if has(code) {
            r.push(format!("Also: {}.", lower(&text(code))));
        }
    }
    if matches!(class.state, State::O | State::U(_)) {
        if f.mp3_trace.is_some_and(|t| !t.found()) && f.aac_trace.is_some_and(|t| !crate::aactrace::found(&t)) {
            r.push("No MP3 or AAC coding traces were found. An MP3 or AAC converted straight back to a lossless format shows them, even at high bitrates; audio processed after decoding (resampled, equalised) or another codec would not, so the doubt is smaller but remains.".into());
        }
        if let Some(e) = f.edge.filter(|_| f.cutoff.is_some()) {
            r.push(if e < 150.0 {
                format!("The edge of the lowpass is gentle ({e:.0} dB per octave): more like an analog filter or a converter than an encoder's brick wall (a hint only).")
            } else {
                format!("The edge of the lowpass is sharp ({e:.0} dB per octave), as encoders and digital filters cut (a hint only).")
            });
        }
    }
    r
}

/// The verdict for a file from its measures. `aac_check`: the AAC and Opus searches count.
pub(crate) fn decide(path: &Path, f: &Facts, aac_check: bool) -> Verdict {
    let j = Judged::new(f, aac_check);
    let details = details(&j);
    if f.frames == 0 {
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
        if std::env::var_os("BT_FACTS").is_some() {
            verdict.facts = Some(f.clone());
        }
        return verdict;
    }
    let found = findings(&j);
    let class = classify(&found);
    let (label, level, sentence) = wording(&class, &found, &j);
    let mut verdict = Verdict::new(label, sentence, level, details);
    let mut report = build_report(crate::files::file_name(path), &verdict, &j);
    report.evidence = found.iter().map(|x| Evidence { family: x.id.family(), side: x.side, strength: x.strength, text: x.text.clone() }).collect();
    report.reasoning = reasoning(&class, &found, &j);
    report.confidence = Some(class.state.confidence());
    verdict.report = report;
    verdict.track = Some(TrackDecision::of(&class, &found, f));
    if std::env::var_os("BT_FACTS").is_some() {
        verdict.facts = Some(f.clone());
    }
    // Worth the slower search for AAC and Opus traces: a lowpass that leaves the origin open, or
    // lossy without a coding proof (a grid names the codec). Generous: a search costs time,
    // never a false alarm (amendment 4). An FM pilot tone survives an Opus or AAC stream at a
    // high bitrate too: with the music stopping dead around 20 kHz (FM itself stops at 15 kHz),
    // the origin is open.
    let lowpass = f.cutoff.is_some() || j.masked.is_some_and(|m| m.dithered());
    let stream_lowpass = f.cutoff.is_some_and(|c| (19500.0..20800.0).contains(&c));
    let proof = found.iter().any(|x| matches!(x.id, Id::C1(_) | Id::C2));
    // Hiss above the cutoff and nothing else lossy: an analog stage came after the filter, and
    // no coding grid survives one (none found on any tuning set): not searched (amendment 6).
    let analog_after = class.state == State::U(Reason::CutHiss)
        && !found.iter().any(|x| x.side == Side::Lossy && matches!(x.id.family(), Family::Coding | Family::Structure));
    verdict.doubt = (matches!(class.state, State::O | State::U(_)) && lowpass && !analog_after)
        || (class.state == State::R && !proof)
        || (found.iter().any(|x| x.id == Id::O1) && stream_lowpass)
        || found.iter().any(|x| matches!(x.id, Id::C3(_)));
    verdict
}

/// What the show's verdict needs of a file's decision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrackDecision {
    pub(crate) state: State,
    pub(crate) cutoff: Option<f64>,
    /// Encoder holes of 15 dB and more (S1a): repeated at one cutoff, they prove an encoder.
    pub(crate) deep_holes: bool,
    /// Hiss above the cutoff (S4): "upstream" when the holes make it lossy.
    pub(crate) hiss: bool,
    /// A cap of digital processing (T1, T3, T4).
    pub(crate) capped: bool,
    /// A rate over the threshold at every grid position (C4).
    pub(crate) uniform: bool,
    /// The MP3 grid of the track when it is found or a candidate, to follow it from one track of
    /// the show to the next.
    pub(crate) grid: Option<Grid>,
}

/// An MP3 grid of one track: where its granules start, how long the track is, and whether the
/// grid is found (C1) or a candidate (C3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Grid {
    pub(crate) offset: usize,
    pub(crate) samples: u64,
    pub(crate) found: bool,
}

impl TrackDecision {
    /// A file lossy by its format (an MP3 in the show).
    pub(crate) fn lossy_format() -> TrackDecision {
        TrackDecision { state: State::R, cutoff: None, deep_holes: false, hiss: false, capped: false, uniform: false, grid: None }
    }

    fn of(class: &Class, found: &[Finding], f: &Facts) -> TrackDecision {
        let has = |code: &str| found.iter().any(|x| x.id.code() == code);
        TrackDecision {
            state: class.state,
            cutoff: f.cutoff,
            deep_holes: has("S1a"),
            hiss: has("S4"),
            capped: has("T1") || has("T3") || has("T4"),
            uniform: has("C4"),
            grid: f.mp3_trace.filter(|t| t.found() || is_candidate(t)).map(|t| Grid { offset: t.offset, samples: f.samples, found: t.found() }),
        }
    }

    fn green(&self) -> bool {
        matches!(self.state, State::L1 | State::L2)
    }
}

/// One analysed file of a show: what is shown of it, which the show's verdict may change.
pub(crate) struct TrackResult {
    /// Its path in the show, with "/".
    pub name: String,
    pub label: String,
    pub level: Level,
    pub sentence: String,
    pub report: Report,
    /// None for a silent file (not counted).
    pub decision: Option<TrackDecision>,
    /// Changed by the show's verdict: to be shown again.
    pub changed: bool,
}

impl TrackResult {
    fn set(&mut self, state: State, label: String, level: Level, sentence: String) {
        if let Some(d) = self.decision.as_mut() {
            d.state = state;
        }
        self.report.label = label.clone();
        self.report.level = level;
        self.report.sentence = sentence.clone();
        self.report.confidence = Some(state.confidence());
        (self.label, self.level, self.sentence, self.changed) = (label, level, sentence, true);
    }
}

/// The verdict of a show.
pub(crate) struct ShowDecision {
    /// What the Source column shows.
    pub label: String,
    /// The counts of the labels, "12 Lossless, 2 Unclear".
    pub text: String,
    pub level: Level,
    pub report: Report,
}

/// The tracks whose holes of 15 dB and more sit at one cutoff (within 500 Hz of their median),
/// when there are at least 3 of them and a fifth of the show: the same encoder on every one.
fn repeated_holes(tracks: &[TrackDecision]) -> Option<Vec<usize>> {
    let deep: Vec<(usize, f64)> = tracks.iter().enumerate().filter(|(_, t)| t.deep_holes).filter_map(|(i, t)| t.cutoff.map(|c| (i, c))).collect();
    let median = crate::dsp::median(&deep.iter().map(|d| d.1).collect::<Vec<_>>())?;
    let same: Vec<usize> = deep.iter().filter(|d| (d.1 - median).abs() <= 500.0).map(|d| d.0).collect();
    (same.len() >= 3 && same.len() * 5 >= tracks.len()).then_some(same)
}

/// Why a candidate grid was promoted.
#[derive(Clone, Copy)]
enum Promotion {
    Continuity,
    Repeated,
}

/// The distance on the circle of the 576 positions of an MP3 granule.
fn grid_distance(a: usize, b: i64) -> i64 {
    let d = (a as i64 - b).rem_euclid(576);
    d.min(576 - d)
}

/// The candidate grids (C3) of a show that are promoted to found (§2.3), as indexes into
/// `decisions`:
/// (b) continuity: a track cut from a stream encoded in one piece has its granules where the
/// previous track's continue: position(next) = position(previous) - length(previous), modulo
/// 576, within 2. Consecutive tracks linked this way make a run; the candidates of a run are
/// promoted when it holds a grid found or three candidates (a chance link is 5 in 576);
/// (c) repetition: half the tracks of the show or more have a grid found.
/// The persistence of a grid across the windows of a track, which the plan listed as (a),
/// was measured and abandoned (notes/REFONTE-E7.md).
fn promoted_grids(tracks: &[TrackResult], idx: &[usize], decisions: &[TrackDecision]) -> Vec<(usize, Promotion)> {
    let m = decisions.len();
    let mut out: Vec<(usize, Promotion)> = Vec::new();
    // The tracks in the order of the show.
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|&a, &b| crate::files::natural_cmp(&tracks[idx[a]].name, &tracks[idx[b]].name));
    let mut run: Vec<usize> = Vec::new();
    let close = |run: &mut Vec<usize>, out: &mut Vec<(usize, Promotion)>| {
        let found = run.iter().filter(|&&k| decisions[k].grid.is_some_and(|g| g.found)).count();
        let candidates: Vec<usize> = run.iter().copied().filter(|&k| decisions[k].grid.is_some_and(|g| !g.found)).collect();
        if !candidates.is_empty() && (found >= 1 || candidates.len() >= 3) {
            out.extend(candidates.into_iter().map(|k| (k, Promotion::Continuity)));
        }
        run.clear();
    };
    for &k in &order {
        let Some(g) = decisions[k].grid.filter(|g| g.samples > 0) else {
            close(&mut run, &mut out);
            continue;
        };
        if let Some(&prev) = run.last() {
            let p = decisions[prev].grid.unwrap();
            if grid_distance(g.offset, p.offset as i64 - p.samples as i64) > 2 {
                close(&mut run, &mut out);
            }
        }
        run.push(k);
    }
    close(&mut run, &mut out);
    // (c) half the show has a grid found: its candidates are promoted too.
    let found = decisions.iter().filter(|d| d.grid.is_some_and(|g| g.found)).count();
    if found > 0 && found * 2 >= m {
        for (k, d) in decisions.iter().enumerate() {
            if d.grid.is_some_and(|g| !g.found) && !out.iter().any(|o| o.0 == k) {
                out.push((k, Promotion::Repeated));
            }
        }
    }
    out.sort_by_key(|o| o.0);
    out
}

/// The coherence pass over a show's tracks (§1.6): it never makes a track red from the context
/// alone, except encoder holes repeated at one cutoff. (The promotion of candidate grids comes
/// with them.) Returns the lines it adds to the show's reasoning.
fn coherence(tracks: &mut [TrackResult]) -> Vec<String> {
    let mut said = Vec::new();
    let idx: Vec<usize> = (0..tracks.len()).filter(|&i| tracks[i].decision.is_some()).collect();
    let decisions: Vec<TrackDecision> = idx.iter().filter_map(|&i| tracks[i].decision).collect();
    let m = decisions.len();
    if m == 0 {
        return said;
    }
    // Candidate grids that the show confirms (§2.3): by the continuity of the grid from one track
    // to the next (b), or because half the show has a grid found (c).
    let promoted = promoted_grids(tracks, &idx, &decisions);
    if !promoted.is_empty() {
        let found = decisions.iter().filter(|d| d.grid.is_some_and(|g| g.found)).count();
        for &(k, why) in &promoted {
            let t = &mut tracks[idx[k]];
            let reason = match why {
                Promotion::Continuity => "its MP3 grid continues, from the previous track to the next, the grid of the tracks around it: the show was encoded in one piece".to_string(),
                Promotion::Repeated => format!("{found} of the {m} tracks of the show have an MP3 grid found, and this one has a weak grid"),
            };
            let sentence = format!("Decoded from an MP3: a weak grid, confirmed by the show ({reason})");
            t.report.reasoning.push(format!("Weak MP3 grid confirmed by the show: {reason}."));
            t.set(State::R, "Lossy (MP3 traces)".into(), Level::Error, sentence);
        }
        said.push(format!("{} track(s) with a weak MP3 grid are confirmed as lossy by the show.", promoted.len()));
    }
    let decisions: Vec<TrackDecision> = idx.iter().filter_map(|&i| tracks[i].decision).collect();
    // Encoder holes repeated on several tracks at the same cutoff: one encoder for the show.
    if let Some(same) = repeated_holes(&decisions) {
        let n = same.len();
        for &k in &same {
            let t = &mut tracks[idx[k]];
            let d = decisions[k];
            if d.state == State::R {
                continue;
            }
            let c = d.cutoff.unwrap_or(0.0) / 1000.0;
            let label = if d.hiss { format!("Lossy upstream {c:.1} kHz") } else { format!("Lossy {c:.1} kHz") };
            let sentence = format!("Lossy source: encoder holes repeated on {n} tracks of the show at the same cutoff, which only an encoder leaves; this track: {}", lower(&t.sentence));
            t.report.reasoning.push(format!("Encoder holes of 15 dB and more are repeated on {n} of the {m} tracks of the show at the same cutoff (within 500 Hz): one encoder for the show, so lossy."));
            t.set(State::R, label, Level::Error, sentence);
        }
        said.push(format!("Encoder holes repeated on {n} of the {m} tracks at the same cutoff: those tracks are lossy."));
    }
    let now: Vec<TrackDecision> = idx.iter().filter_map(|&i| tracks[i].decision).collect();
    // Most of the show carries lossy evidence: the tracks without any cannot be called lossless.
    let lossy = now.iter().filter(|d| matches!(d.state, State::R | State::O)).count();
    if lossy * 2 >= m {
        let mut n = 0;
        for &i in &idx {
            let t = &mut tracks[i];
            if t.decision.is_some_and(|d| d.green()) {
                n += 1;
                let sentence = format!(
                    "Cannot tell: nothing in this track itself, but {lossy} of {m} tracks of the show carry lossy evidence; on its own it reads: {}",
                    lower(&t.sentence)
                );
                t.report.reasoning.push(format!("{lossy} of the {m} tracks of the show carry lossy evidence: a track without any cannot be called lossless on its own."));
                t.set(State::U(Reason::Show), "Unclear".into(), Level::Info, sentence);
            }
        }
        if n > 0 {
            said.push(format!("{lossy} of the {m} tracks carry lossy evidence: the {n} others are Unclear, not lossless."));
        }
    }
    // Most of the show went through digital processing last: the same transfer chain for all.
    let capped = now.iter().filter(|d| d.capped).count();
    if capped * 2 >= m {
        let mut n = 0;
        for &i in &idx {
            let t = &mut tracks[i];
            if t.decision.is_some_and(|d| d.state == State::L1) {
                let label = if matches!(t.label.as_str(), "Lossless" | "FM broadcast") { LIKELY_LOSSLESS.to_string() } else { t.label.clone() };
                let sentence = format!(
                    "Probably lossless: no lossy clue, but {capped} of {m} tracks of the show show digital processing after the last analog stage (the same transfer chain); on its own it reads: {}",
                    lower(&t.sentence)
                );
                let level = t.level;
                t.set(State::L2, label, level, sentence);
                n += 1;
            }
        }
        if n > 0 {
            said.push(format!("{capped} of the {m} tracks show digital processing after the last analog stage: the other tracks are at best \"Likely lossless\"."));
        }
    }
    // A rate over the threshold at every grid position on most tracks: said, nothing more.
    let uniform = now.iter().filter(|d| d.uniform).count();
    if uniform >= 5 && uniform * 2 >= m {
        said.push(format!(
            "On {uniform} of the {m} tracks more bands round onto the MP3 grid at every position than genuine audio shows: noise reduction, or a lossy stage whose grid was erased since (information only)."
        ));
    }
    said
}

/// The verdict of a show from those of its files (None without any): the coherence pass, then
/// the colour (§1.6): red with one lossy track, orange with one doubt, Unclear when half the
/// tracks are or one is for a reason of the transfer chain, green otherwise.
pub(crate) fn aggregate(title: String, done: &mut [TrackResult]) -> Option<ShowDecision> {
    if done.is_empty() {
        return None;
    }
    let mut lines = coherence(done);
    let states: Vec<State> = done.iter().filter_map(|t| t.decision.map(|d| d.state)).collect();
    let m = states.len();
    let count = |f: &dyn Fn(&State) -> bool| states.iter().filter(|s| f(s)).count();
    let red = count(&|s| *s == State::R);
    let doubt = count(&|s| *s == State::O);
    let unclear = count(&|s| matches!(s, State::U(_)));
    let chain = count(&|s| matches!(s, State::U(r) if r.chain()));
    let green = count(&|s| matches!(s, State::L1 | State::L2));
    // Counts in order of first appearance, then sorted by count (as the reference).
    let mut counts: Vec<(String, usize)> = Vec::new();
    for t in done.iter() {
        // "Lossy? upstream 16.1 kHz" counts as "Lossy? upstream": the frequency is dropped.
        let key = match t.label.strip_suffix(" kHz").and_then(|l| l.rsplit_once(' ')) {
            Some((head, _)) => head.to_string(),
            None => t.label.clone(),
        };
        match counts.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 += 1,
            None => counts.push((key, 1)),
        }
    }
    counts.sort_by_key(|a| std::cmp::Reverse(a.1));
    let text = counts.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(", ");
    let (level, mut shown, confidence) = if red > 0 {
        // A few lossy tracks among lossless ones: a splice, or tracks from another source.
        if red * 4 < m && green * 2 > m - red {
            for t in done.iter_mut().filter(|t| t.decision.is_some_and(|d| d.state == State::R)) {
                let sentence = format!("Isolated: {red} of {m} tracks of the show are lossy (a splice, or another source?); {}", lower(&t.sentence));
                let (label, level) = (t.label.clone(), t.level);
                t.set(State::R, label, level, sentence);
            }
            lines.push(format!("{red} lossy track(s) among {m}, the rest mostly lossless: a splice or mixed sources?"));
            (Level::Error, format!("Mixed: {red} lossy of {m} (splice or mixed sources?)"), Confidence::High)
        } else {
            (Level::Error, text.clone(), Confidence::High)
        }
    } else if doubt > 0 {
        (Level::Warn, text.clone(), Confidence::Medium)
    } else if m > 0 && (unclear * 2 >= m || chain > 0) {
        if chain > 0 && unclear * 2 < m {
            lines.push(format!("{chain} track(s) Unclear for a reason of the transfer chain (a filter, an analog stage or a noise reducer that hides what came before): it is the show's chain, so the show is Unclear."));
        }
        (Level::Info, text.clone(), Confidence::Low)
    } else {
        let all = states.iter().all(|s| *s == State::L1);
        (Level::Ok, if counts.len() == 1 && counts[0].0 == "Lossless" { "All lossless".into() } else { text.clone() }, if all { Confidence::High } else { Confidence::Medium })
    };
    // Origin labels of lossless tracks (16-bit in 24, no hi-res content) stay orange.
    let level = if level == Level::Ok && done.iter().any(|t| t.level == Level::Warn) { Level::Warn } else { level };
    if shown.is_empty() {
        shown = text.clone();
    }
    // Show report: every track, then the reasoning of the most common verdict.
    let main = &counts[0].0;
    let example = done.iter().find(|t| t.label.starts_with(main.as_str())).map(|t| &t.report);
    let mut reasoning = vec![format!("{} track(s) analysed: {text}.", done.len())];
    reasoning.extend(lines);
    if let Some(r) = example {
        reasoning.push(format!("Most tracks read \"{main}\". Why, for {}:", r.title));
        reasoning.extend(r.reasoning.iter().map(|line| format!("    {line}")));
    }
    let report = Report {
        heading: "Lossless analysis".into(),
        title,
        label: shown.clone(),
        level,
        sentence: example.map(|r| r.sentence.clone()).unwrap_or_default(),
        measures: Vec::new(),
        tracks: done.iter().map(|t| Measure { name: t.name.clone(), value: t.label.clone(), meaning: t.report.key_facts.clone(), level: t.level }).collect(),
        reasoning,
        key_facts: String::new(),
        confidence: Some(confidence),
        ..Default::default()
    };
    Some(ShowDecision { label: shown, text, level, report })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lossless::{comb_stats, intensity_stats, masked_bands, CombStats, Hires};
    use crate::mp3trace::Trace;

    /// A lowpass at 16.5 kHz without holes and with hiss above: doubtful, so "analog" when the
    /// roll-off is gradual (`step` small, the band above follows the music).
    fn lowpass(step: f64) -> Facts {
        Facts {
            rate: 44100.0,
            frames: 100,
            cutoff: Some(16500.0),
            drop: 30.0,
            holes: Some(3.0),
            above_floor: Some(5.0),
            high_floor: Some(5.0),
            hf: Some(-30.0),
            roll: Some((step, 0.8)),
            edge: Some(40.0),
            ..Default::default()
        }
    }

    fn grid(likelihood: f64, contrast: f64) -> Trace {
        Trace { likelihood, contrast, offset: 100, mid_side: false, median: 0.01, sigma: 0.002, second: 0.02, ..Default::default() }
    }

    /// The shape of the lowpass (family F) never decides: a gradual roll-off and a wall over the
    /// same floor give the same verdict, and the report still shows the shape.
    #[test]
    fn shape_never_decides() {
        let path = Path::new("x.flac");
        let gradual = decide(path, &lowpass(10.0), false);
        let wall = decide(path, &lowpass(30.0), false);
        assert_eq!((gradual.label.as_str(), gradual.level), (wall.label.as_str(), wall.level));
        assert!(gradual.report.to_text().contains("Gradual roll-off at 16.5 kHz"));
    }

    #[test]
    fn facts_survive_json_with_awkward_values() {
        let mut f = lowpass(10.0);
        f.rate = 5e-324;
        f.drop = f64::NAN;
        f.holes = Some(f64::INFINITY);
        f.above_floor = Some(f64::NEG_INFINITY);
        f.hf = Some(0.1 + 0.2);
        f.hiss = Some((-0.0, f64::MAX, f64::MIN_POSITIVE));
        f.shifted_candidate = Some((f64::NAN, 18950.123456789));
        f.hires = Some(Hires { low: -90.0, high: f64::NEG_INFINITY, tail: None });
        f.comb_stats = Some(CombStats { range: 512, values: 70, regular: Some(1.0 / 3.0) });
        f.wasted = vec![0, 50, u32::MAX];
        f.intensity = IntensityStats { frames: 300, loud: 150, mid_mean: f64::NAN, locked: vec![1, 2, 3] };
        f.masked_bands = vec![(13050.0, 2.5, 29.999999999999996), (13150.0, f64::INFINITY, -0.0)];
        f.mp3_trace = Some(Trace { likelihood: 0.025000000000000005, offset: 358, mid_side: true, contrast: 9.999999999999998, median: 0.1, sigma: 1e-4, second: 0.3, ..Default::default() });
        f.searched = true;
        let text = serde_json::to_string(&f).unwrap();
        assert!(text.contains(r#""drop":"NaN""#) && text.contains(r#""holes":"inf""#) && !text.contains(r#""drop":null"#), "{text}");
        let back: Facts = serde_json::from_str(&text).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), text);
        assert!(back.drop.is_nan() && back.holes == Some(f64::INFINITY) && back.above_floor == Some(f64::NEG_INFINITY));
        assert_eq!(back.hf.unwrap().to_bits(), (0.1f64 + 0.2).to_bits());
        assert_eq!(back.hiss.unwrap().0.to_bits(), (-0.0f64).to_bits());
        assert_eq!(back.masked_bands[0].2.to_bits(), 29.999999999999996f64.to_bits());
        assert_eq!(back.mp3_trace.unwrap().contrast.to_bits(), 9.999999999999998f64.to_bits());
        assert!(back.intensity.mid_mean.is_nan() && back.wasted[2] == u32::MAX && back.searched);
    }

    /// Opus's intensity stereo: treble locked in a fifth of the loud frames, mids in stereo.
    /// A mono file (locked everywhere) and genuine stereo are left alone.
    #[test]
    fn intensity_stereo() {
        let share = |locked_every: usize, mids: f64| {
            let frames: Vec<(f64, f64, f64)> = (0..200).map(|i| (1.0, if i % locked_every == 0 { 0.9999 } else { 0.9 }, mids)).collect();
            intensity_share(&intensity_stats(&frames))
        };
        assert!(share(5, 0.6).is_some_and(|s| (s - 0.2).abs() < 0.01));
        assert!(share(5, 0.97).is_none());
        assert!(share(50, 0.6).is_none());
        // The threshold is on the lowest coherence: 0.995 itself is not locked, just above it is.
        let at = |v: f64| intensity_share(&intensity_stats(&vec![(1.0, v, 0.5); 100]));
        assert!(at(0.995).is_none() && at(0.9951).is_some());
        // Too few frames, too few loud ones.
        assert!(intensity_share(&intensity_stats(&vec![(1.0, 0.9999, 0.5); 49])).is_none());
    }

    #[test]
    fn upsampling_origin() {
        assert_eq!(source_rate(21600.0), Some(44100.0));
        assert_eq!(source_rate(23300.0), Some(48000.0));
        assert_eq!(source_rate(47500.0), Some(96000.0));
        assert_eq!(source_rate(30000.0), None);
        assert!(no_hires(Some(21600.0), Some(44100.0)).contains("(the band of a 44.1 kHz source): upsampled, or an analog source"));
        // A hi-res file: nothing above 27 kHz is upsampled; a tail within 15 dB is not.
        let judge = |h: Hires| {
            let f = Facts { rate: 96000.0, hires: Some(h), ..Default::default() };
            Judged::new(&f, false).upsampled
        };
        assert!(judge(Hires { low: -60.0, high: -110.0, tail: Some(-100.0) }));
        assert!(!judge(Hires { low: -60.0, high: -110.0, tail: Some(-90.0) }));
        assert!(!judge(Hires { low: -60.0, high: -90.0, tail: Some(-100.0) }));
        // ffmpeg's 88.2 kHz files (fakehires s06-s08): a wall at 25.7 kHz, an empty floor above,
        // and a tail of the resampler's images only 16 dB over it: upsampled all the same, said
        // with the measured edge and both causes.
        let walled = |rate: f64, cutoff: Option<f64>, h: Hires| Judged::new(&Facts { rate, cutoff, hires: Some(h), ..Default::default() }, false).upsampled;
        let images = Hires { low: -8.6, high: -97.3, tail: Some(-81.2) };
        assert!(walled(88200.0, Some(25721.0), images));
        assert!(walled(96000.0, Some(25781.0), images));
        // No wall found (an authentic hi-res file), a wall at 30 kHz or more, a floor that is
        // not empty, or a lower rate: not decided by this rule.
        assert!(!walled(88200.0, None, images));
        assert!(!walled(88200.0, Some(31000.0), images));
        assert!(!walled(88200.0, Some(25721.0), Hires { low: -8.6, high: -40.0, tail: Some(-35.0) }));
        assert!(!walled(48000.0, Some(25721.0), images));
        let message = no_hires(Some(25721.0), None);
        assert_eq!(message, "no content above 25.7 kHz: upsampled, or an analog source transferred at a high rate");
        assert!(!message.contains("22-24"));
    }

    /// Music up to 16 kHz over a steady floor; above it the floor alone, rising toward
    /// 19 kHz (noise-shaped dither) or flat (FM hiss).
    #[test]
    fn lowpass_under_dither() {
        let freq = |b: usize| 10050.0 + b as f64 * 100.0;
        let bands = 115;
        let frames = 400;
        let make = |rising: bool| -> Vec<f32> {
            let mut kept = Vec::new();
            for i in 0..frames {
                for b in 0..bands {
                    let f = freq(b);
                    let floor_db = if rising && f > 16000.0 { 25.0 + (f - 16000.0).min(3000.0) / 3000.0 * 12.0 } else { 30.0 };
                    // A little frame to frame variation; music comes and goes 20 dB above.
                    let wobble = 1.0 + 0.2 * ((i * 7 + b * 3) % 5) as f64 / 4.0;
                    let music = if f < 16000.0 && i % 2 == 0 { 100.0 } else { 0.0 };
                    kept.push((libm::pow(10.0, floor_db / 10.0) * wobble + libm::pow(10.0, (floor_db + 20.0) / 10.0) * music / 100.0) as f32);
                }
            }
            kept
        };
        let none = |_: f64| false;
        let m = masked_cutoff(&masked_bands(&make(true), bands, &freq, &none, 0.0)).unwrap();
        assert!((m.cutoff - 16050.0).abs() <= 100.0, "{m:?}");
        assert!(m.dithered(), "{m:?}");
        let flat = masked_cutoff(&masked_bands(&make(false), bands, &freq, &none, 0.0)).unwrap();
        assert!(!flat.dithered(), "{flat:?}");
        // Too few frames: no table, nothing found.
        assert!(masked_cutoff(&masked_bands(&make(true)[..bands * 99], bands, &freq, &none, 0.0)).is_none());
    }

    #[test]
    fn lossywav_varies_resolution_does_not() {
        let mut lossy = [0u32; 33];
        lossy[0] = 50;
        lossy[2] = 300;
        lossy[3] = 400;
        lossy[5] = 250;
        let w = wasted_bits(&lossy).unwrap();
        assert!(w.share > 0.9 && w.varying > 0.5 && is_lossywav(&w));
        // Every sample even (a level change): one zero bit everywhere, not lossyWAV.
        let mut even = [0u32; 33];
        even[1] = 990;
        even[2] = 10;
        let w = wasted_bits(&even).unwrap();
        assert!(w.varying < 0.2 && !is_lossywav(&w));
        // The histogram of a silent file is empty.
        assert!(wasted_bits(&[]).is_none());
    }

    #[test]
    fn comb_of_values() {
        let span = 1usize << 16;
        // 16-bit audio turned down by 3 dB in a 24-bit file: a value every 181.
        let mut h = vec![0u32; 2 * span];
        let mut v = 0.0f64;
        while v < span as f64 {
            for x in [span as f64 + v, span as f64 - v] {
                h[libm::round(x) as usize] += 2000;
            }
            v += 181.02;
        }
        let c = comb_bits(&comb_stats(&h).unwrap()).unwrap();
        assert!(c.0 <= 1.0 / 32.0, "{}", c.0);
        // Every value used: true 24-bit.
        let full = vec![7u32; 2 * span];
        assert!(comb_bits(&comb_stats(&full).unwrap()).unwrap().1 > 23.9);
    }

    /// The thresholds are the decision's: the same measures, a different value either side.
    #[test]
    fn thresholds_are_applied_here() {
        struct Flags {
            pilot_on_19k: bool,
            silent_above: bool,
            from_44k: bool,
            uniform: bool,
        }
        let judge = |f: Facts| {
            let j = Judged::new(&f, false);
            Flags { pilot_on_19k: j.pilot_on_19k, silent_above: j.silent_above, from_44k: j.from_44k, uniform: j.uniform }
        };
        // The FM pilot: more than 15 dB over its surroundings.
        assert!(!judge(Facts { pilot_prominence: Some(15.0), ..Default::default() }).pilot_on_19k);
        assert!(judge(Facts { pilot_prominence: Some(15.0001), ..Default::default() }).pilot_on_19k);
        // Digital silence above the cutoff, or at 18-19.5 kHz: under 3 dB.
        assert!(judge(Facts { above_floor: Some(2.9), ..Default::default() }).silent_above);
        assert!(judge(Facts { above_floor: Some(9.0), high_floor: Some(1.0), ..Default::default() }).silent_above);
        assert!(!judge(Facts { above_floor: Some(3.0), high_floor: Some(3.0), ..Default::default() }).silent_above);
        // A 48 kHz file stopping at the top of a 44.1 kHz band, silent above, with few holes.
        let from44 = |holes: f64| Facts { rate: 48000.0, cutoff: Some(21500.0), holes: Some(holes), above_floor: Some(0.0), ..Default::default() };
        assert!(judge(from44(4.9)).from_44k && !judge(from44(5.0)).from_44k);
        // No MP3 grid but a rate over 2.5 % at every position: uniform; a found grid is not.
        assert!(judge(Facts { mp3_trace: Some(grid(0.03, 5.0)), ..Default::default() }).uniform);
        assert!(!judge(Facts { mp3_trace: Some(grid(0.02, 5.0)), ..Default::default() }).uniform);
        assert!(!judge(Facts { mp3_trace: Some(grid(0.2, 30.0)), ..Default::default() }).uniform);
    }

    /// The AAC and Opus searches count only when asked, AAC unless an MP3 grid was found, Opus
    /// unless either was: the same measures give either mode.
    #[test]
    fn searches_are_gated_by_the_decision() {
        let path = Path::new("x.flac");
        let opus = crate::opustrace::Trace { clean: 500, frames: 1000, offset: 0, phase: 0, phases: 1 };
        let aac = Facts { rate: 44100.0, frames: 100, aac_trace: Some(grid(0.5, 30.0)), searched: true, ..Default::default() };
        assert_eq!(decide(path, &aac, false).label, "Lossless");
        assert_eq!(decide(path, &aac, true).label, "Lossy (AAC traces)");
        // Found MP3 grid: AAC is not looked at.
        let mp3 = Facts { mp3_trace: Some(grid(0.5, 30.0)), ..aac.clone() };
        assert_eq!(decide(path, &mp3, true).label, "Lossy (MP3 traces)");
        assert!(decide(path, &mp3, true).sentence.contains("MP3"));
        // Opus follows only when neither was found.
        let both = Facts { opus_trace: Some(opus), ..aac.clone() };
        assert_eq!(decide(path, &both, true).label, "Lossy (AAC traces)");
        let only = Facts { aac_trace: None, opus_trace: Some(opus), ..aac };
        assert_eq!(decide(path, &only, true).label, "Lossy (Opus traces)");
        assert_eq!(decide(path, &only, false).label, "Lossless");
    }

    // The rules of the decision, one test per line of the plan's §3 and per pair of §1.3
    // (notes/PLAN-refonte-decision.md, with its amendments).

    /// A file with a cutoff: holes below it, floors above it (over digital silence).
    fn cut(cutoff: f64, holes: f64, above: f64, high: f64) -> Facts {
        Facts { rate: 44100.0, frames: 100, cutoff: Some(cutoff), drop: 40.0, holes: Some(holes), above_floor: Some(above), high_floor: Some(high), ..Default::default() }
    }

    fn verdict(f: &Facts) -> Verdict {
        decide(Path::new("x.flac"), f, false)
    }

    fn state(f: &Facts) -> State {
        classify(&findings(&Judged::new(f, false))).state
    }

    fn clue(id: Id) -> Finding {
        let strength = match id {
            Id::C3(_) => Strength::Weak,
            _ => Strength::Moderate,
        };
        Finding { id, side: Side::Lossy, strength, text: String::new() }
    }

    /// Opus's intensity stereo in a fifth of the loud frames.
    fn intensity() -> IntensityStats {
        let frames: Vec<(f64, f64, f64)> = (0..200).map(|i| (1.0, if i % 5 == 0 { 0.9999 } else { 0.9 }, 0.6)).collect();
        intensity_stats(&frames)
    }

    /// Music up to `cutoff`, then only noise-shaped dither (a floor rising toward 19 kHz).
    fn dithered(cutoff: f64) -> Vec<(f64, f64, f64)> {
        (0..80)
            .map(|i| {
                let hz = 13050.0 + 100.0 * i as f64;
                if hz < cutoff {
                    (hz, 20.0, 30.0)
                } else {
                    (hz, 2.0, 25.0 + ((hz - cutoff) / 3000.0).min(1.0) * 12.0)
                }
            })
            .collect()
    }

    #[test]
    fn pairs_of_independent_clues_are_lossy() {
        let pair = |a: Id, b: Id| classify(&[clue(a), clue(b)]);
        for (a, b) in [
            (Id::S1a, Id::S2a),
            (Id::S1b, Id::S2a),
            (Id::S1a, Id::S2b),
            (Id::S1a, Id::S3),
            (Id::S1b, Id::S3),
            (Id::S1a, Id::C3(Codec::Mp3)),
            (Id::S1b, Id::C3(Codec::Mp3)),
            (Id::S3, Id::C3(Codec::Aac)),
        ] {
            assert_eq!(pair(a, b), Class { state: State::R, pair: Some((a, b)) }, "{a:?} + {b:?}");
            assert_eq!(pair(b, a).state, State::R, "{b:?} + {a:?}");
        }
        // Not admitted: two clues, still a doubt.
        for (a, b) in [(Id::S1b, Id::S2b), (Id::S2a, Id::S3), (Id::S2b, Id::S3)] {
            assert_eq!(pair(a, b).state, State::O, "{a:?} + {b:?}");
        }
        // A candidate grid alone changes nothing.
        assert_eq!(classify(&[clue(Id::C3(Codec::Mp3))]).state, State::L1);
    }

    #[test]
    fn rule_silent_file() {
        let v = verdict(&Facts { rate: 44100.0, ..Default::default() });
        assert_eq!((v.label.as_str(), v.level), ("Silent", Level::Info));
    }

    #[test]
    fn rule_coding_grid_is_red_and_names_the_label() {
        let mp3 = Facts { mp3_trace: Some(grid(0.2, 30.0)), ..cut(16000.0, 20.0, 0.0, 0.0) };
        let v = verdict(&mp3);
        // The label comes from the strongest clue: the grid, not the lowpass.
        assert_eq!((v.label.as_str(), v.level), ("Lossy (MP3 traces)", Level::Error));
        assert_eq!(v.report.confidence, Some(Confidence::High));
        // No other family overrules a grid: hiss above, an FM pilot.
        let fm = Facts { mp3_trace: Some(grid(0.2, 30.0)), pilot_prominence: Some(30.0), ..cut(15000.0, 2.0, 30.0, 30.0) };
        assert_eq!(verdict(&fm).label, "Lossy (MP3 traces)");
    }

    /// Histograms measured on real lossyWAV 1.4.2 files (8 lossless excerpts at the extreme,
    /// standard, portable and extraportable presets), on the excerpts themselves and on
    /// reduced-resolution files (13 bits, with a gain, 12 bits): see tests/data and
    /// notes/REFONTE-E5.md.
    #[test]
    fn lossywav_measured_on_real_files() {
        // lossyWAV removed almost nothing from these noisy sources (0.01 to 0.31 of the
        // blocks changed): practically the original, "Lossless" is not misleading. Kept so
        // that a change of the thresholds shows.
        const BARELY_TOUCHED: [&str; 5] = ["aud-cassette-E", "fm-glasgow-E", "fm-glasgow-S", "fm-glasgow-P", "fm-honeymoon-E"];
        let (mut found, mut total) = (0, 0);
        for line in include_str!("../tests/data/lossywav-histograms.tsv").lines().filter(|l| !l.starts_with('#')) {
            let [class, id, hist] = line.split('\t').collect::<Vec<_>>()[..] else { panic!("{line}") };
            let mut wasted = vec![0u32; 33];
            for pair in hist.split(',') {
                let (bits, blocks) = pair.split_once(':').unwrap();
                wasted[bits.parse::<usize>().unwrap()] = blocks.parse().unwrap();
            }
            let detected = wasted_bits(&wasted).as_ref().is_some_and(is_lossywav);
            let label = verdict(&Facts { rate: 44100.0, frames: 100, wasted, ..Default::default() }).label;
            assert_eq!(label == "Lossy (lossyWAV)", detected, "{id}");
            match class {
                // A source, or a file cut to fewer bits: never lossyWAV.
                "lossless" | "reduced" => assert!(!detected, "{id}"),
                _ => {
                    total += 1;
                    found += detected as usize;
                    assert_eq!(detected, !BARELY_TOUCHED.contains(&id), "{id}");
                }
            }
        }
        assert_eq!((found, total), (27, 32));
    }

    #[test]
    fn rule_lossywav_is_red() {
        let mut wasted = vec![0u32; 33];
        (wasted[0], wasted[2], wasted[3], wasted[5]) = (50, 300, 400, 250);
        let v = verdict(&Facts { rate: 44100.0, frames: 100, wasted, ..Default::default() });
        assert_eq!((v.label.as_str(), v.level), ("Lossy (lossyWAV)", Level::Error));
    }

    #[test]
    fn rule_deep_holes_alone_are_orange() {
        // Variant B (15 dB alone = red) is gone: one family, a doubt.
        let v = verdict(&cut(20300.0, 20.0, 15.0, 15.0));
        assert_eq!((v.label.as_str(), v.level), ("Lossy? upstream 20.3 kHz", Level::Warn));
        assert_eq!(state(&Facts { above_floor: None, ..cut(21000.0, 18.0, 0.0, 30.0) }), State::O);
    }

    #[test]
    fn rule_holes_and_silence_are_red_at_any_cutoff() {
        assert_eq!(verdict(&cut(16000.0, 12.0, 0.0, 0.0)).label, "Lossy 16.0 kHz");
        // Above 19.5 kHz too (amendment 1): a resampler digs no holes.
        let v = verdict(&cut(20100.0, 11.0, -3.0, 20.0));
        assert_eq!((v.label.as_str(), v.level), ("Lossy 20.1 kHz", Level::Error));
        // A low floor above 19.5 kHz pairs only with holes of 15 dB and more (amendment 5).
        assert_eq!(state(&cut(20200.0, 17.0, 7.0, 8.0)), State::R);
        assert_eq!(state(&cut(20200.0, 12.0, 7.0, 8.0)), State::O);
        assert_eq!(state(&cut(20200.0, 5.0, 7.0, 8.0)), State::U(Reason::Wall));
        // Silence alone above 19.5 kHz: the resampler's wall, Unclear.
        assert_eq!(state(&cut(20400.0, 5.0, -4.0, 10.0)), State::U(Reason::Wall));
    }

    #[test]
    fn rule_holes_and_hiss_are_upstream_orange_or_unclear() {
        let deep = verdict(&cut(16900.0, 17.0, 20.0, 20.0));
        assert_eq!((deep.label.as_str(), deep.level), ("Lossy? upstream 16.9 kHz", Level::Warn));
        assert_eq!(state(&cut(16900.0, 12.0, 20.0, 20.0)), State::O);
        // 8-10 dB with hiss: no clue left, the cutoff with hiss makes it Unclear.
        assert_eq!(state(&cut(16000.0, 9.0, 20.0, 20.0)), State::U(Reason::CutHiss));
        // 8-10 dB alone (no hiss below 19.5 kHz to blame): shallow holes.
        assert_eq!(state(&cut(20000.0, 9.0, 15.0, 15.0)), State::U(Reason::ShallowHoles));
    }

    #[test]
    fn rule_fm_pilot_is_origin_only() {
        let pilot = |f: Facts| Facts { pilot_prominence: Some(30.0), ..f };
        let plain = verdict(&pilot(Facts { rate: 44100.0, frames: 100, hf: Some(-40.0), ..Default::default() }));
        assert_eq!((plain.label.as_str(), plain.level), ("FM broadcast", Level::Ok));
        // Encoder holes with hiss: an FM capture of a lossy-coded program, orange.
        let coded = verdict(&pilot(cut(15000.0, 17.0, 30.0, 30.0)));
        assert_eq!(coded.label, "Lossy? upstream 15.0 kHz");
        assert!(coded.sentence.starts_with("FM capture of a lossy-coded program"), "{}", coded.sentence);
        // The FM band with hiss and a pilot: lossless.
        assert_eq!(verdict(&pilot(cut(15000.0, 3.0, 30.0, 30.0))).label, "FM broadcast");
        // Too little treble to see anything: Unclear even with a pilot (amendment, reading 3).
        assert_eq!(state(&pilot(Facts { rate: 44100.0, frames: 100, hf: Some(-65.0), ..Default::default() })), State::U(Reason::Treble));
    }

    #[test]
    fn rule_fm_15k_is_unclear_with_its_reason() {
        let v = verdict(&cut(15000.0, 3.0, 30.0, 30.0));
        assert_eq!((v.label.as_str(), v.level), ("Unclear", Level::Info));
        assert!(v.sentence.contains("no FM pilot: an FM capture or a lossy source followed by an analog stage look the same"), "{}", v.sentence);
        assert_eq!(v.report.confidence, Some(Confidence::Low));
        // An analog stage came after the filter: no grid to look for (amendment 6).
        assert!(!v.doubt);
        // With shallow holes as well, still searched.
        assert!(verdict(&cut(15000.0, 9.0, 30.0, 30.0)).doubt);
        // A wall with no hiss above: searched (a grid can survive).
        assert!(verdict(&cut(20400.0, 5.0, -4.0, 10.0)).doubt);
    }

    #[test]
    fn rule_15k_lowpass_variants_are_spread() {
        // Silence above: S2a (orange), and possibly a 32 kHz source.
        let silent = verdict(&cut(15000.0, 3.0, 0.0, 0.0));
        assert_eq!(silent.label, "Lossy? 15.0 kHz");
        assert!(silent.sentence.contains("32 kHz source"), "{}", silent.sentence);
        // Holes of 10-15 dB under hiss: upstream, orange.
        assert_eq!(verdict(&cut(15000.0, 12.0, 30.0, 30.0)).label, "Lossy? upstream 15.0 kHz");
        // A low floor alone: Unclear (amendment 2); with deep holes: red.
        assert_eq!(state(&cut(15000.0, 3.0, 6.0, 6.0)), State::U(Reason::LowFloor));
        assert_eq!(state(&cut(15000.0, 17.0, 6.0, 6.0)), State::R);
        // Holes of 10-15 dB with a low floor: one admitted clue only, orange.
        assert_eq!(state(&cut(15000.0, 12.0, 6.0, 6.0)), State::O);
    }

    #[test]
    fn rule_wall_with_silence_below_19k5_is_orange() {
        let v = verdict(&cut(17000.0, 3.0, 0.0, 0.0));
        assert_eq!((v.label.as_str(), v.level), ("Lossy? 17.0 kHz", Level::Warn));
        assert!(v.doubt);
    }

    #[test]
    fn rule_wall_19k5_21k5_without_hiss_is_unclear() {
        let v = verdict(&cut(20400.0, 5.0, -4.0, 10.0));
        assert_eq!((v.label.as_str(), v.level), ("Unclear", Level::Info));
        assert!(v.sentence.contains("a resampler's or converter's filter and a high-bitrate encoder look the same"), "{}", v.sentence);
        // Nothing measurable above (too close to the top of the band): no hiss either (reading 1).
        let top = Facts { above_floor: None, ..cut(21264.0, 7.0, 0.0, 24.0) };
        assert_eq!(state(&top), State::U(Reason::Wall));
    }

    #[test]
    fn rule_converter_filter_with_hiss_is_lossless() {
        let v = verdict(&cut(20500.0, 3.0, 25.0, 25.0));
        assert_eq!((v.label.as_str(), v.level), ("Lossless", Level::Ok));
        assert!(v.sentence.contains("anti-alias filter"));
    }

    #[test]
    fn rule_gradual_roll_off_is_no_longer_a_rule() {
        // Below 19.5 kHz with hiss: Unclear by the hiss, whatever the shape.
        let hiss = |step: f64| Facts { roll: Some((step, 0.8)), ..cut(16500.0, 3.0, 20.0, 20.0) };
        assert_eq!(state(&hiss(10.0)), State::U(Reason::CutHiss));
        assert_eq!(state(&hiss(30.0)), State::U(Reason::CutHiss));
        // A gradual roll-off followed by digital silence caps at Likely lossless (T1).
        let t1 = Facts { rate: 48000.0, roll: Some((10.0, 0.2)), ..cut(22500.0, 2.0, 0.0, 30.0) };
        let v = verdict(&t1);
        assert_eq!((v.label.as_str(), v.level), (LIKELY_LOSSLESS, Level::Ok));
        assert!(v.sentence.contains("a digital process came after the last analog stage"), "{}", v.sentence);
    }

    #[test]
    fn rule_hidden_lowpass_under_dither() {
        let at = |c: f64| Facts { rate: 44100.0, frames: 100, hf: Some(-40.0), masked_bands: dithered(c), ..Default::default() };
        let low = verdict(&at(16000.0));
        assert!(low.label.starts_with("Lossy? 16.") && low.level == Level::Warn, "{}", low.label);
        assert!(low.doubt);
        // Above 19.5 kHz: dither after a sharp stop caps at Likely lossless (T4).
        let high = verdict(&at(20000.0));
        assert_eq!(high.label, LIKELY_LOSSLESS);
    }

    #[test]
    fn rule_intensity_stereo_is_joint_stereo_treble() {
        let f = Facts { rate: 44100.0, frames: 100, hf: Some(-40.0), intensity: intensity(), ..Default::default() };
        let v = verdict(&f);
        assert_eq!((v.label.as_str(), v.level), ("Lossy? (joint-stereo treble)", Level::Warn));
        assert!(!v.sentence.contains("Opus"), "{}", v.sentence);
        // With encoder holes: a pair, red.
        assert_eq!(state(&Facts { intensity: intensity(), ..cut(19000.0, 12.0, 20.0, 20.0) }), State::R);
    }

    #[test]
    fn rule_treble_thresholds() {
        let hf = |h: f64, mp3: bool| Facts { rate: 44100.0, frames: 100, hf: Some(h), mp3_trace: mp3.then(|| grid(0.02, 3.0)), ..Default::default() };
        assert_eq!(state(&hf(-70.0, true)), State::U(Reason::Treble));
        assert_eq!(state(&hf(-60.0, true)), State::L2);
        assert_eq!(state(&hf(-50.0, true)), State::L1);
        // Without the MP3 search: one limit.
        assert_eq!(state(&hf(-61.0, false)), State::U(Reason::Treble));
        assert_eq!(state(&hf(-59.0, false)), State::L1);
    }

    #[test]
    fn rule_uniform_rate_is_info_and_cancels_no_grid() {
        let f = Facts { rate: 44100.0, frames: 100, hf: Some(-40.0), mp3_trace: Some(grid(0.03, 5.0)), ..Default::default() };
        let ids: Vec<&str> = findings(&Judged::new(&f, false)).iter().map(|x| x.id.code()).collect();
        assert!(ids.contains(&"C4") && !ids.contains(&"C5"), "{ids:?}");
        // d.0 stays cancelled: a weak grid is not a lossy verdict.
        assert_eq!(verdict(&f).label, "Lossless");
    }

    #[test]
    fn rule_no_hires_content_is_a_qualifier() {
        let up = Facts { rate: 96000.0, frames: 100, hf: Some(-30.0), hires: Some(Hires { low: -60.0, high: -110.0, tail: Some(-100.0) }), ..Default::default() };
        let v = verdict(&up);
        assert_eq!((v.label.as_str(), v.level), (LOSSLESS_NO_HIRES, Level::Warn));
        assert!(v.sentence.contains("upsampled, or an analog source transferred at a high rate"), "{}", v.sentence);
        // On an Unclear file it goes into the sentence, never hidden.
        let wall = Facts { rate: 96000.0, hires: up.hires, ..cut(20400.0, 3.0, -4.0, 10.0) };
        let v = verdict(&wall);
        assert_eq!(v.label, "Unclear");
        assert!(v.sentence.contains("no content above"), "{}", v.sentence);
    }

    #[test]
    fn rule_16_bit_in_24_is_always_said() {
        let pad = |f: Facts| Facts { zero_low: true, ..f };
        let v = verdict(&pad(Facts { rate: 44100.0, frames: 100, hf: Some(-30.0), ..Default::default() }));
        assert_eq!((v.label.as_str(), v.level), (LOSSLESS_PADDED, Level::Warn));
        // Behind a wall that makes the file Unclear, it is still said.
        let v = verdict(&pad(cut(21167.0, 3.0, f64::NAN, 30.0)));
        assert_eq!(v.label, "Unclear");
        assert!(v.sentence.contains("16-bit audio in a 24-bit file"), "{}", v.sentence);
        let v = verdict(&pad(cut(16000.0, 20.0, 0.0, 0.0)));
        assert_eq!(v.level, Level::Error);
        assert!(v.sentence.contains("16-bit audio in a 24-bit file"), "{}", v.sentence);
    }

    #[test]
    fn rule_from_44k_is_said() {
        let f = Facts { rate: 48000.0, ..cut(21600.0, 2.0, 0.0, 30.0) };
        let v = verdict(&f);
        assert!(v.sentence.contains("44.1 kHz source upsampled to 48 kHz"), "{}", v.sentence);
    }

    #[test]
    fn rule_encoder_table_only_with_a_grid() {
        let plain = verdict(&cut(16000.0, 20.0, 0.0, 0.0));
        assert!(!plain.report.to_text().contains("matches the lowpass of"));
        let traced = verdict(&Facts { mp3_trace: Some(grid(0.2, 30.0)), ..cut(16000.0, 20.0, 0.0, 0.0) });
        assert!(traced.report.to_text().contains("matches the lowpass of MP3"), "{}", traced.report.to_text());
    }

    #[test]
    fn rule_report_by_family_with_confidence() {
        let v = verdict(&cut(16000.0, 12.0, 0.0, 0.0));
        let text = v.report.to_text();
        assert!(text.contains("Verdict: Lossy 16.0 kHz (confidence: high)"), "{text}");
        assert!(text.contains("Structure of the spectrum\n- [lossy, moderate] Encoder holes of 12 dB"), "{text}");
        assert!(text.contains("Shape of the lowpass\n- [lossy, weak]"), "{text}");
    }

    /// A track of a fabricated show: its state, cutoff, holes of 15 dB and more, hiss above, cap.
    fn track(state: State, cutoff: Option<f64>, deep_holes: bool, hiss: bool, capped: bool) -> TrackResult {
        let label = match state {
            State::R => "Lossy 16.0 kHz",
            State::O if hiss => "Lossy? upstream 16.0 kHz",
            State::O => "Lossy? 16.0 kHz",
            State::U(_) => "Unclear",
            State::L2 => LIKELY_LOSSLESS,
            State::L1 => "Lossless",
        };
        let level = match state {
            State::R => Level::Error,
            State::O => Level::Warn,
            State::U(_) => Level::Info,
            _ => Level::Ok,
        };
        TrackResult {
            name: "t.flac".into(),
            label: label.into(),
            level,
            sentence: "As measured".into(),
            report: Report::default(),
            decision: Some(TrackDecision { state, cutoff, deep_holes, hiss, capped, uniform: false, grid: None }),
            changed: false,
        }
    }

    /// A track of a show cut from a stream: named by its number, with the MP3 grid it shows
    /// (position of the granules, length of the track, found or only a candidate).
    fn grid_track(number: usize, state: State, grid: Option<(usize, u64, bool)>) -> TrackResult {
        let mut t = track(state, None, false, false, false);
        t.name = format!("{number:02}.flac");
        if let Some((offset, samples, found)) = grid {
            t.decision.as_mut().unwrap().grid = Some(Grid { offset, samples, found });
        }
        t
    }

    /// The tracks of a show that was encoded in one piece and cut again: each track's granules
    /// start where the previous track's continue, so position(next) = position(previous) -
    /// length(previous) modulo 576. `found[i]`: whether the grid of track i is found, or only a
    /// candidate; None: no grid. Lengths are not multiples of 576.
    fn stream(found: &[Option<bool>]) -> Vec<TrackResult> {
        let lengths = [3_000_001u64, 2_900_117, 3_100_333, 2_800_059, 3_200_747, 2_700_911];
        let mut offset = 528i64;
        found
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let t = match f {
                    Some(found) => grid_track(i + 1, if *found { State::R } else { State::L1 }, Some((offset as usize, lengths[i % 6], *found))),
                    None => grid_track(i + 1, State::L1, None),
                };
                offset = (offset - lengths[i % 6] as i64).rem_euclid(576);
                t
            })
            .collect()
    }

    fn promoted(tracks: &[TrackResult]) -> Vec<usize> {
        tracks.iter().enumerate().filter(|(_, t)| t.changed && t.label == "Lossy (MP3 traces)").map(|(i, _)| i).collect()
    }

    /// (b) A candidate grid that continues a grid found on the next or previous track is the
    /// same encoder's: promoted. A candidate whose position does not follow is not.
    #[test]
    fn grid_continuity_promotes_a_candidate_next_to_a_found_grid() {
        let mut t = stream(&[Some(true), Some(false), None, None]);
        show(&mut t);
        assert_eq!(promoted(&t), [1]);
        assert!(t[1].sentence.contains("continues"), "{}", t[1].sentence);
        // The same candidate, 40 positions off: not linked, nothing promoted (a quarter of the
        // show has a grid found, under half).
        let mut t = stream(&[Some(true), Some(false), None, None]);
        let g = t[1].decision.as_mut().unwrap().grid.as_mut().unwrap();
        g.offset = (g.offset + 40) % 576;
        show(&mut t);
        assert!(promoted(&t).is_empty());
        // Within 2 positions: still the same grid.
        let mut t = stream(&[Some(true), Some(false), None, None]);
        let g = t[1].decision.as_mut().unwrap().grid.as_mut().unwrap();
        g.offset = (g.offset + 2) % 576;
        show(&mut t);
        assert_eq!(promoted(&t), [1]);
        // Tracks are taken in the order of the show, not in the order they finished.
        let mut t = stream(&[Some(true), Some(false), None, None]);
        t.swap(0, 3);
        show(&mut t);
        assert_eq!(promoted(&t).len(), 1);
    }

    /// A real case, generated/splitmp3/pantera-320-ffmpeg (9 tracks of a lossless concert encoded
    /// in one piece at 320 kbps with LAME, decoded with ffmpeg, cut at the original boundaries):
    /// 4 grids found, 4 candidates, 1 track without; the position law holds on every pair, and
    /// the 4 candidates are promoted (three after the first track, one after the eighth).
    #[test]
    fn grid_continuity_on_a_real_stream() {
        let measured = [
            (528, 3391448, Some(true)),
            (568, 3399367, Some(false)),
            (177, 3407286, Some(false)),
            (507, 3415205, Some(false)),
            (406, 3423124, Some(true)),
            (450, 3431043, Some(true)),
            (63, 3438962, None),
            (397, 3446881, Some(true)),
            (300, 3454800, Some(false)),
        ];
        let mut t: Vec<TrackResult> = measured
            .iter()
            .enumerate()
            .map(|(i, &(offset, samples, found))| match found {
                Some(found) => grid_track(i + 1, if found { State::R } else { State::L1 }, Some((offset, samples, found))),
                None => grid_track(i + 1, State::L1, None),
            })
            .collect();
        show(&mut t);
        assert_eq!(promoted(&t), [1, 2, 3, 8]);
        // The track without a grid is not lossless either, the rest of the show being lossy.
        assert_eq!(t[6].label, "Unclear");
    }

    /// (b) Three candidates that follow one another are promoted without any grid found; two are
    /// not (a chance link between two positions of 576 is 5 in 576).
    #[test]
    fn grid_continuity_of_three_candidates() {
        let mut t = stream(&[Some(false), Some(false), Some(false), None, None, None]);
        show(&mut t);
        assert_eq!(promoted(&t), [0, 1, 2]);
        let mut t = stream(&[Some(false), Some(false), None, None, None, None]);
        show(&mut t);
        assert!(promoted(&t).is_empty());
        // A track without a grid between breaks the chain.
        let mut t = stream(&[Some(false), Some(false), None, Some(false), None, None]);
        show(&mut t);
        assert!(promoted(&t).is_empty());
    }

    /// (c) Half the show has a grid found: the candidates of the show are promoted, linked or
    /// not; under half, not.
    #[test]
    fn grid_repetition_promotes_the_candidates_of_the_show() {
        let mut t = stream(&[Some(true), Some(true), Some(false), None]);
        t[2].decision.as_mut().unwrap().grid.as_mut().unwrap().offset = 7;
        show(&mut t);
        assert_eq!(promoted(&t), [2]);
        assert!(t[2].sentence.contains("2 of the 4 tracks"), "{}", t[2].sentence);
        let mut t = stream(&[Some(true), None, Some(false), None, None]);
        t[2].decision.as_mut().unwrap().grid.as_mut().unwrap().offset = 7;
        show(&mut t);
        assert!(promoted(&t).is_empty());
    }

    /// A candidate grid is a weak clue: alone it changes nothing; with encoder holes it is the
    /// admitted pair.
    #[test]
    fn candidate_grid_is_weak_and_alone_changes_nothing() {
        let candidate = Trace { likelihood: 0.026, contrast: 9.3, offset: 528, median: 0.014, sigma: 0.0012, second: 0.018, ..Default::default() };
        assert!(is_candidate(&candidate));
        // Found (10 and more), or not enough contrast (8.5 and less), or a rate under the threshold: not.
        assert!(!is_candidate(&Trace { contrast: 10.0, ..candidate }));
        assert!(!is_candidate(&Trace { contrast: 8.5, ..candidate }));
        assert!(!is_candidate(&Trace { likelihood: 0.025, ..candidate }));
        let base = Facts { rate: 44100.0, frames: 100, ..Default::default() };
        let with = Facts { mp3_trace: Some(candidate), ..base.clone() };
        let (plain, candidate_only) = (verdict(&base), verdict(&with));
        assert_eq!((candidate_only.label.as_str(), candidate_only.level), (plain.label.as_str(), plain.level));
        assert!(candidate_only.report.to_text().contains("A weak MP3 grid at position 528"), "{}", candidate_only.report.to_text());
        // With encoder holes under a lowpass: a pair, lossy.
        let holes = Facts { cutoff: Some(16000.0), holes: Some(16.0), above_floor: Some(30.0), high_floor: Some(30.0), ..with.clone() };
        let v = verdict(&holes);
        assert_eq!((v.label.as_str(), v.level), ("Lossy upstream 16.0 kHz", Level::Error), "{}", v.sentence);
    }

    fn show(tracks: &mut [TrackResult]) -> ShowDecision {
        aggregate("show".into(), tracks).unwrap()
    }

    fn lossless(n: usize) -> Vec<TrackResult> {
        (0..n).map(|_| track(State::L1, None, false, false, false)).collect()
    }

    /// Anathema 2004: 21 Lossless and 2 walls at 20.9-21.1 kHz with digital silence (T3): one
    /// Unclear track of the transfer chain is enough, the show is never green (rule 3).
    #[test]
    fn show_anathema_is_unclear() {
        let mut t = lossless(21);
        t.push(track(State::U(Reason::Wall), Some(20900.0), false, false, true));
        t.push(track(State::U(Reason::Wall), Some(21100.0), false, false, true));
        let s = show(&mut t);
        assert_eq!(s.level, Level::Info);
        assert_eq!(s.report.confidence, Some(Confidence::Low));
        assert!(t.iter().all(|x| !x.changed));
        // A track Unclear for its treble only is the music's: it counts only in the majority.
        let mut t = lossless(21);
        t.push(track(State::U(Reason::Treble), None, false, false, false));
        assert_eq!(show(&mut t).level, Level::Ok);
    }

    /// Ace Frehley 1990: 13 Unclear (too little treble) of 17: Unclear by the majority.
    #[test]
    fn show_ace_frehley_majority_unclear() {
        let mut t: Vec<TrackResult> = (0..13).map(|_| track(State::U(Reason::Treble), None, false, false, false)).collect();
        t.extend((0..4).map(|_| track(State::L2, None, false, false, false)));
        assert_eq!(show(&mut t).level, Level::Info);
    }

    /// One lossy track among lossless ones: red, said "Mixed", the track called isolated.
    #[test]
    fn show_isolated_red_track_is_mixed() {
        let mut t = lossless(9);
        t.push(track(State::R, Some(16000.0), true, false, false));
        let s = show(&mut t);
        assert_eq!((s.label.as_str(), s.level), ("Mixed: 1 lossy of 10 (splice or mixed sources?)", Level::Error));
        assert!(t[9].changed && t[9].sentence.starts_with("Isolated: 1 of 10 tracks"), "{}", t[9].sentence);
        assert!(t[..9].iter().all(|x| !x.changed && x.label == "Lossless"));
        // A quarter or more lossy: red, without "Mixed".
        let mut t = lossless(6);
        t.extend((0..2).map(|_| track(State::R, Some(16000.0), true, false, false)));
        assert!(!show(&mut t).label.starts_with("Mixed"));
    }

    /// Paul Di Anno 2001 (MiniDisc): holes of 15 dB and more with hiss above on 6 of 10 tracks at
    /// 16.6-16.9 kHz: each alone is orange, repeated they prove an encoder, and the tracks
    /// without any clue are no longer lossless.
    #[test]
    fn show_paul_di_anno_repeated_holes() {
        let mut t: Vec<TrackResult> = [16900.0, 16900.0, 16800.0, 16600.0, 16600.0, 16900.0].iter().map(|&c| track(State::O, Some(c), true, true, false)).collect();
        t.extend(lossless(3));
        t.push(track(State::L2, None, false, false, false));
        let s = show(&mut t);
        assert_eq!(s.level, Level::Error);
        assert!(t[..6].iter().all(|x| x.label.starts_with("Lossy upstream 16.") && x.level == Level::Error), "{:?}", t[0].label);
        assert!(t[6..].iter().all(|x| x.label == "Unclear" && x.decision.is_some_and(|d| d.state == State::U(Reason::Show))));
        assert!(t[6].sentence.contains("6 of 10 tracks of the show carry lossy evidence"), "{}", t[6].sentence);
    }

    /// Oasis 1997: one track with deep holes (14.7 kHz), one with 10-15 dB holes: no repetition,
    /// the show stays orange.
    #[test]
    fn show_oasis_stays_orange() {
        let mut t = vec![track(State::O, Some(14700.0), true, true, false), track(State::O, Some(14700.0), false, true, false)];
        t.extend((0..9).map(|_| track(State::U(Reason::CutHiss), Some(15000.0), false, true, false)));
        t.extend(lossless(4));
        let s = show(&mut t);
        assert_eq!(s.level, Level::Warn);
        assert!(t.iter().all(|x| x.level != Level::Error));
    }

    /// Meat Loaf 1996 (DCC): holes of 15 dB and more on 3 of 18 tracks (16.7 %, under a fifth):
    /// no repetition, orange. A fourth track (22 %) would make them red.
    #[test]
    fn show_meat_loaf_threshold() {
        let make = |deep: usize| {
            let mut t: Vec<TrackResult> = (0..deep).map(|_| track(State::O, Some(15900.0), true, true, false)).collect();
            t.extend((0..7 - deep).map(|_| track(State::O, Some(15400.0), false, true, false)));
            t.push(track(State::U(Reason::CutHiss), Some(15400.0), false, true, false));
            t.extend(lossless(10));
            t
        };
        let mut three = make(3);
        assert_eq!(show(&mut three).level, Level::Warn);
        assert!(three.iter().all(|x| !x.changed));
        let mut four = make(4);
        assert_eq!(show(&mut four).level, Level::Error);
        assert_eq!(four.iter().filter(|x| x.level == Level::Error).count(), 4);
        // Holes at cutoffs more than 500 Hz from their median do not repeat.
        let mut apart: Vec<TrackResult> = [14000.0, 16000.0, 18000.0].iter().map(|&c| track(State::O, Some(c), true, false, false)).collect();
        apart.extend(lossless(2));
        assert_eq!(show(&mut apart).level, Level::Warn);
    }

    /// Half the show with a cap of digital processing: the other tracks are "Likely lossless".
    #[test]
    fn show_caps_by_transfer_chain() {
        let mut t: Vec<TrackResult> = (0..13).map(|_| track(State::U(Reason::Wall), Some(20400.0), false, false, true)).collect();
        t.push(track(State::L1, None, false, false, false));
        let s = show(&mut t);
        assert_eq!(s.level, Level::Info);
        assert_eq!((t[13].label.as_str(), t[13].level), (LIKELY_LOSSLESS, Level::Ok));
        // A rate over the threshold everywhere on most tracks: said, colour unchanged.
        let mut t = lossless(6);
        for x in &mut t {
            x.decision.as_mut().unwrap().uniform = true;
        }
        let s = show(&mut t);
        assert_eq!((s.label.as_str(), s.level), ("All lossless", Level::Ok));
        assert!(s.report.reasoning.iter().any(|l| l.contains("round onto the MP3 grid at every position")));
    }

    /// The order of the show's colour: red, orange, Unclear, green; silent files not counted;
    /// origin labels of lossless tracks keep the show orange.
    #[test]
    fn show_colour_order() {
        let mut t = vec![track(State::O, Some(16000.0), false, false, false), track(State::R, Some(16000.0), false, false, false), track(State::L1, None, false, false, false)];
        assert_eq!(show(&mut t).level, Level::Error);
        let mut t = vec![track(State::O, Some(16000.0), false, false, false), track(State::U(Reason::Wall), Some(20400.0), false, false, true)];
        assert_eq!(show(&mut t).level, Level::Warn);
        let mut t = lossless(3);
        t.push(TrackResult { decision: None, label: "Silent".into(), level: Level::Info, ..track(State::L1, None, false, false, false) });
        assert_eq!(show(&mut t).level, Level::Ok);
        let mut t = lossless(3);
        t[0].label = LOSSLESS_PADDED.into();
        t[0].level = Level::Warn;
        assert_eq!(show(&mut t).level, Level::Warn);
    }
}
