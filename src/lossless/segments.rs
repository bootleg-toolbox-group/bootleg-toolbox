//! Parts of a file (plan §1.5 and §2.2, E9): a lossy stretch spliced into a lossless file may be
//! too short to show in the measures of the whole. The kept frames are cut into slices of 10, 15 or
//! 30 s (each a half-size after the last, so that a slice may be taken every other one without
//! overlap); every slice is measured (cutoff, holes, floor, treble) and its loudest windows are
//! tried against the grid the whole file shows, if it shows one. Runs of slices that look alike
//! are the parts: each is measured again as a whole, as if it were a file of its own.
//! Nothing is read again: the kept frames and the loudest window of each stretch of 2.5 s
//! (`Atom`) were collected while the file was read.

use super::{facts_of, Atom, Facts, Measured, ATOM_SECONDS};
use crate::audio::AudioInfo;
use crate::mp3trace::Collector;

/// Slice sizes, seconds. The step is half the size.
pub(crate) const SIZES: [f64; 3] = [10.0, 15.0, 30.0];
/// A slice with fewer kept frames than this is not measured (digital silence, too short).
const MIN_FRAMES: usize = 50;
/// Cutoffs this close (Hz) are the same cutoff: the first empty stretch of the gaps between neighbouring slices of the lossless shows of the tuning group, above their 99th percentile (notes/REFONTE-E9.md).
const SAME_CUTOFF: f64 = 900.0;
/// Witness positions against which the rate at the file's grid is read.
const WITNESSES: usize = 15;

/// The rate of bands with a too small rounding error at the grid position found in the whole
/// file, in the windows of a slice, against the rates at `WITNESSES` other positions.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct GridRate {
    pub(crate) rate: f64,
    pub(crate) witness_max: f64,
    pub(crate) witness_median: f64,
    pub(crate) windows: u8,
}

impl GridRate {
    /// The grid of the file is in this slice: its rate is over the detector's threshold and over
    /// that of every witness position.
    fn shows(&self, threshold: f64) -> bool {
        self.rate > threshold && self.rate > self.witness_max
    }
}

/// The measures of one slice (kept frames starting in [t0, t1)).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Slice {
    pub(crate) t0: f64,
    pub(crate) t1: f64,
    pub(crate) frames: u32,
    #[serde(with = "crate::wire")]
    pub(crate) cutoff: Option<f64>,
    #[serde(with = "crate::wire")]
    pub(crate) holes: Option<f64>,
    #[serde(with = "crate::wire")]
    pub(crate) above_floor: Option<f64>,
    #[serde(with = "crate::wire")]
    pub(crate) hf: Option<f64>,
    pub(crate) mp3: Option<GridRate>,
    pub(crate) aac: Option<GridRate>,
    /// BT_GRID_SLICES: the most likely grid position of the slice's own windows, (offset, rate,
    /// contrast), for MP3 and AAC: to look at a doubtful stretch, not used by any decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) best: Option<[Option<(usize, f64, f64)>; 2]>,
}

/// The slices of one size, `size / 2` apart.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Series {
    pub(crate) size: f64,
    pub(crate) slices: Vec<Slice>,
}

/// A run of at least 2 slices of one variant (size, with or without overlap) that look alike,
/// measured as a whole.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Part {
    pub(crate) size: f64,
    pub(crate) overlap: bool,
    pub(crate) t0: f64,
    pub(crate) t1: f64,
    /// Slices of the variant in the run.
    pub(crate) slices: u32,
    pub(crate) facts: Box<Facts>,
}

/// The loudest windows of the atoms in the stretch [first, last) of atoms, at most 16.
fn loudest(atoms: &[Atom], first: u32, last: u32) -> Vec<(f64, Vec<[f32; 2]>)> {
    let mut picked: Vec<&Atom> = atoms.iter().filter(|a| a.index >= first && a.index < last).collect();
    picked.sort_by(|a, b| b.energy.total_cmp(&a.energy));
    picked.truncate(16);
    picked.sort_by_key(|a| a.index);
    picked.into_iter().map(|a| (a.energy, a.window.clone())).collect()
}

/// The windows of a codec's atoms tried at the grid position of the whole file and at witnesses.
struct Probe {
    /// Per atom: index, then the hits per position and channel mode (position 0 is the file's).
    atoms: Vec<(u32, AtomHits)>,
}

/// Hits and bands tried, per grid position and channel mode.
type AtomHits = Vec<[(usize, usize); 2]>;

impl Probe {
    fn mp3(m: &Measured, trace: &crate::mp3trace::Trace) -> Option<Probe> {
        let offsets: Vec<usize> = std::iter::once(trace.offset).chain((1..=WITNESSES).map(|k| (trace.offset + 37 * k) % 576)).collect();
        let channels = m.atoms_channels;
        let atoms = m
            .atoms_mp3
            .iter()
            .map(|a| crate::mp3trace::probe(&a.window, channels, m.rate as u32, trace.mid_side, &offsets).map(|p| (a.index, p)))
            .collect::<Option<Vec<_>>>()?;
        Some(Probe { atoms })
    }

    fn aac(m: &Measured, trace: &crate::mp3trace::Trace) -> Option<Probe> {
        let offsets: Vec<usize> = std::iter::once(trace.offset).chain((1..=WITNESSES).map(|k| (trace.offset + 67 * k) % 1024)).collect();
        let windows: Vec<&[[f32; 2]]> = m.atoms_aac.iter().map(|a| a.window.as_slice()).collect();
        let hits = crate::aactrace::probe(&windows, m.atoms_channels, m.rate as u32, trace.mid_side, &offsets)?;
        let atoms = m.atoms_aac.iter().zip(hits).map(|(a, h)| (a.index, h.into_iter().map(|x| [x, (0, 0)]).collect())).collect();
        Some(Probe { atoms })
    }

    /// The pooled rate at position `pos` over the atoms [first, last): best of the two modes.
    fn rate(&self, first: u32, last: u32, pos: usize) -> (f64, usize) {
        let mut sums = [(0usize, 0usize); 2];
        let mut windows = 0;
        for (index, hits) in &self.atoms {
            if *index >= first && *index < last {
                windows += 1;
                for (s, h) in sums.iter_mut().zip(&hits[pos]) {
                    s.0 += h.0;
                    s.1 += h.1;
                }
            }
        }
        let best = sums.iter().filter(|s| s.1 > 0).map(|s| s.0 as f64 / s.1 as f64).fold(0.0, f64::max);
        (best, windows)
    }

    fn grid_rate(&self, first: u32, last: u32) -> Option<GridRate> {
        let (rate, windows) = self.rate(first, last, 0);
        if windows == 0 {
            return None;
        }
        let mut others: Vec<f64> = (1..=WITNESSES).map(|p| self.rate(first, last, p).0).collect();
        others.sort_by(f64::total_cmp);
        Some(GridRate { rate, witness_max: others[others.len() - 1], witness_median: others[others.len() / 2], windows: windows.min(255) as u8 })
    }
}

/// The part of the measures `m` that the kept frames [lo, hi) make, as the measures of a file of
/// their own: a spectrum made of their bands (what `facts_of` computes from), their stereo
/// measures, the given grid traces.
fn subset(m: &Measured, lo: usize, hi: usize, t0: f64, t1: f64, mp3_trace: Option<crate::mp3trace::Trace>, aac: Option<Collector>) -> Measured {
    let kb = m.kept_hi - m.kept_lo;
    let sums: Vec<f64> = (0..kb).map(|j| (lo..hi).map(|f| m.kept[f * kb + j] as f64).sum()).collect();
    let level: f64 = m.level[lo..hi].iter().map(|&v| v as f64).sum();
    // Below 7 kHz the level of 1-4 kHz, above 22 kHz the top band: only the bands between count.
    let power = (0..m.bins)
        .map(|k| {
            let b = k / m.band;
            if b < m.kept_lo {
                level
            } else {
                sums[(b - m.kept_lo).min(kb - 1)]
            }
        })
        .collect();
    let between = |at: &[u32]| at.partition_point(|&i| (i as usize) < lo)..at.partition_point(|&i| (i as usize) < hi);
    let (ir, wr) = (between(&m.intensity_at), between(&m.width_at));
    Measured {
        samples: ((t1 - t0) * m.rate) as u64,
        rate: m.rate,
        bins: m.bins,
        band: m.band,
        bin_hz: m.bin_hz,
        kept_lo: m.kept_lo,
        kept_hi: m.kept_hi,
        window: m.window.clone(),
        frames: hi - lo,
        power,
        kept: m.kept[lo * kb..hi * kb].to_vec(),
        intensity: m.intensity[ir.clone()].to_vec(),
        side_vs_mid: m.side_vs_mid[wr.clone()].to_vec(),
        level: m.level[lo..hi].to_vec(),
        kept_block: Vec::new(),
        intensity_at: Vec::new(),
        width_at: Vec::new(),
        block: m.block,
        atoms_channels: m.atoms_channels,
        atoms_mp3: Vec::new(),
        atoms_aac: Vec::new(),
        check_low: false,
        low_bits: false,
        wasted: [0; 33],
        comb_check: false,
        histogram: Vec::new(),
        mp3_trace,
        aac,
        mp2: None,
        opus: None,
    }
}

/// What a slice shows of the spectrum, for grouping.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Look {
    /// A cutoff was measured.
    Cut(f64),
    /// No cutoff, and treble enough for the file-level test (`TREBLE_UNCLEAR`): full band.
    Full,
    /// Too few frames or too little treble to tell: neutral, it neither cuts a segment nor makes one.
    Unmeasurable,
}

fn look(s: &Slice) -> Look {
    if (s.frames as usize) < MIN_FRAMES {
        Look::Unmeasurable
    } else if let Some(c) = s.cutoff {
        Look::Cut(c)
    } else if s.hf.is_some_and(|h| h >= super::TREBLE_UNCLEAR) {
        Look::Full
    } else {
        Look::Unmeasurable
    }
}

fn together(a: Look, b: Look) -> bool {
    match (a, b) {
        (Look::Cut(x), Look::Cut(y)) => (x - y).abs() <= SAME_CUTOFF,
        (Look::Full, Look::Full) => true,
        _ => false,
    }
}

/// Groups of slices that look alike, as (first, one past last, measurable slices): the neutral
/// slices between two slices of a group are in it, those at its ends or between two groups are in
/// none.
fn groups(v: &[&Slice]) -> Vec<(usize, usize, usize)> {
    let looks: Vec<Look> = v.iter().map(|s| look(s)).collect();
    let mut out: Vec<(usize, usize, usize)> = Vec::new();
    let mut last: Option<(usize, Look)> = None;
    for (i, &l) in looks.iter().enumerate() {
        if l == Look::Unmeasurable {
            continue;
        }
        match (last, out.last_mut()) {
            (Some((_, prev)), Some(g)) if together(prev, l) => {
                g.1 = i + 1;
                g.2 += 1;
            }
            _ => out.push((i, i + 1, 1)),
        }
        last = Some((i, l));
    }
    out
}

/// The groups of at least 2 measurable slices that are not the whole file.
fn runs(v: &[&Slice]) -> Vec<(usize, usize)> {
    groups(v).into_iter().filter(|g| g.2 >= 2 && g.1 - g.0 < v.len()).map(|g| (g.0, g.1)).collect()
}

/// A slice of the largest group of one look (the rest of the file, whatever a splice brings).
fn dominant<'a>(v: &[&'a Slice]) -> Option<&'a Slice> {
    groups(v).into_iter().max_by_key(|g| g.1 - g.0).map(|g| v[g.0])
}

/// Runs of at least 2 consecutive slices where `member`: not the whole file.
fn member_runs(v: &[&Slice], member: &dyn Fn(&Slice) -> bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        if !member(v[i]) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < v.len() && member(v[j]) {
            j += 1;
        }
        if j - i >= 2 && j - i < v.len() {
            out.push((i, j));
        }
        i = j;
    }
    out
}

/// The slices of the file in every size and the parts of every variant, measured as a whole.
/// `whole`: the measures of the file.
/// `only`: a single variant (size, overlap) instead of the six (production).
pub(super) fn measure(info: &AudioInfo, m: &Measured, whole: &Facts, aac_check: bool, only: Option<(f64, bool)>) -> (Vec<Series>, Vec<Part>) {
    let kb = m.kept_hi.saturating_sub(m.kept_lo);
    if m.rate >= 88200.0 || m.frames == 0 || kb == 0 || m.kept_block.is_empty() || m.kept.len() != m.kept_block.len() * kb || m.level.len() != m.kept_block.len() {
        return (Vec::new(), Vec::new());
    }
    let block_s = m.block as f64 / m.rate;
    let times: Vec<f64> = m.kept_block.iter().map(|&b| b as f64 * block_s).collect();
    let total = m.samples as f64 / m.rate;
    let frames_of = |t0: f64, t1: f64| times.partition_point(|&t| t < t0)..times.partition_point(|&t| t < t1);
    let atoms_of = |t0: f64, t1: f64| (libm::ceil(t0 / ATOM_SECONDS - 1e-9) as u32, libm::floor(t1 / ATOM_SECONDS + 1e-9) as u32);
    let mp3 = whole.mp3_trace.and_then(|t| Probe::mp3(m, &t));
    let aac = whole.aac_trace.and_then(|t| Probe::aac(m, &t));
    let mut series = Vec::new();
    for &size in SIZES.iter().filter(|&&s| only.is_none_or(|o| o.0 == s)) {
        let step = size / 2.0;
        let mut slices = Vec::new();
        let mut i = 0usize;
        while i as f64 * step + size <= total + 1e-9 {
            let (t0, t1) = (i as f64 * step, i as f64 * step + size);
            let r = frames_of(t0, t1);
            let mut slice = Slice { t0, t1, frames: r.len() as u32, ..Default::default() };
            if r.len() >= MIN_FRAMES {
                let _t = crate::prof::span_hot("slice_facts");
                let f = facts_of(info, &subset(m, r.start, r.end, t0, t1, None, None), false, false, false, Some(whole));
                (slice.cutoff, slice.holes, slice.above_floor, slice.hf) = (f.cutoff, f.holes, f.above_floor, f.hf);
            }
            let (a0, a1) = atoms_of(t0, t1);
            slice.mp3 = mp3.as_ref().and_then(|p| p.grid_rate(a0, a1));
            slice.aac = aac.as_ref().and_then(|p| p.grid_rate(a0, a1));
            if std::env::var_os("BT_GRID_SLICES").is_some() {
                let best = |atoms: &[Atom], frame: usize, frames: usize, aac: bool| {
                    let w = loudest(atoms, a0, a1);
                    let c = Collector::from_windows(m.atoms_channels, frame, frames, w);
                    let t = if aac { crate::aactrace::detect(c, m.rate as u32) } else { c.detect(m.rate as u32) };
                    t.map(|t| (t.offset, t.likelihood, t.contrast))
                };
                slice.best = Some([best(&m.atoms_mp3, 576, 4, false), best(&m.atoms_aac, 1024, 3, true)]);
            }
            slices.push(slice);
            i += 1;
        }
        series.push(Series { size, slices });
    }
    // BT_NO_PARTS: the slices only (to look at their measures before the rule that groups them is fixed).
    if std::env::var_os("BT_NO_PARTS").is_some() {
        return (series, Vec::new());
    }
    let mut parts: Vec<Part> = Vec::new();
    let mut seen: std::collections::HashMap<(u64, u64, bool), usize> = std::collections::HashMap::new();
    // The grid of the file in a slice: in either codec.
    let grid = |s: &Slice| s.mp3.is_some_and(|g| g.shows(crate::mp3trace::THRESHOLD)) || s.aac.is_some_and(|g| g.shows(crate::aactrace::THRESHOLD));
    for s in &series {
        for overlap in [false, true].into_iter().filter(|&o| only.is_none_or(|x| x.1 == o)) {
            let v: Vec<&Slice> = s.slices.iter().step_by(if overlap { 1 } else { 2 }).collect();
            // Runs of one cutoff, and runs where the file's grid shows.
            let mut found = runs(&v);
            // The rest of the file: the cutoff of the longest run of slices with one cutoff.
            let rest = dominant(&v);
            let mut searched: Vec<bool> = found.iter().map(|&(i, _)| rest.is_some_and(|r| !together(look(v[i]), look(r)))).collect();
            let by_grid = member_runs(&v, &grid);
            searched.extend(by_grid.iter().map(|_| true));
            found.extend(by_grid);
            for (&(i, j), search) in found.iter().zip(searched) {
                let (t0, t1) = (v[i].t0, v[j - 1].t1);
                // The grid is searched in a part unlike the rest of the file only: elsewhere it would
                // find what the whole file shows (plan §2.2).
                let key = ((t0 * 1000.0) as u64, (t1 * 1000.0) as u64, search);
                let mut facts = match seen.get(&key) {
                    Some(&k) => parts[k].facts.clone(),
                    None => {
                        let r = frames_of(t0, t1);
                        let (a0, a1) = atoms_of(t0, t1);
                        let ch = m.atoms_channels;
                        // No grid is looked for under hiss above the cutoff: an analog stage came after the
                        // coding, and a grid does not survive one (amendment 16, as amendment 6).
                        let plain = {
                            let _t = crate::prof::span_hot("part_plain");
                            facts_of(info, &subset(m, r.start, r.end, t0, t1, None, None), false, false, false, Some(whole))
                        };
                        let (trace, aac_windows) = if search && !crate::decision::hiss_above_cutoff(&plain) {
                            let _t = crate::prof::span_hot("part_mp3_grid");
                            (
                                Collector::from_windows(ch, 576, 4, loudest(&m.atoms_mp3, a0, a1)).detect(m.rate as u32),
                                Some(Collector::from_windows(ch, 1024, 3, loudest(&m.atoms_aac, a0, a1))),
                            )
                        } else {
                            (None, None)
                        };
                        let mut f = if trace.is_none() && aac_windows.is_none() {
                            // Nothing was searched: the measures are the ones just made.
                            Box::new(Facts { searched: aac_check, ..plain })
                        } else {
                            let _t = crate::prof::span_hot("part_searched");
                            Box::new(facts_of(info, &subset(m, r.start, r.end, t0, t1, trace, aac_windows), aac_check, true, false, Some(whole)))
                        };
                        f.wasted = whole.wasted.clone();
                        f.zero_low = whole.zero_low;
                        f.comb_stats = whole.comb_stats;
                        f
                    }
                };
                facts.samples = ((t1 - t0) * m.rate) as u64;
                seen.entry(key).or_insert(parts.len());
                parts.push(Part { size: s.size, overlap, t0, t1, slices: (j - i) as u32, facts });
            }
        }
    }
    (series, parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice(t0: f64, cutoff: Option<f64>, hf: f64, frames: u32) -> Slice {
        Slice { t0, t1: t0 + 10.0, frames, cutoff, hf: Some(hf), ..Default::default() }
    }

    fn series(spec: &[(Option<f64>, f64, u32)]) -> Vec<Slice> {
        spec.iter().enumerate().map(|(i, &(c, hf, n))| slice(i as f64 * 5.0, c, hf, n)).collect()
    }

    fn found(v: &[Slice]) -> Vec<(usize, usize)> {
        runs(&v.iter().collect::<Vec<_>>())
    }

    #[test]
    fn a_lossy_stretch_between_full_band_slices_is_a_part() {
        let full = (None, -30.0, 100);
        let cut = (Some(16800.0), -30.0, 100);
        let v = series(&[full, full, cut, cut, cut, full, full, full]);
        assert_eq!(found(&v), vec![(0, 2), (2, 5), (5, 8)]);
    }

    #[test]
    fn neighbouring_cutoffs_go_together_within_the_threshold() {
        let a = (Some(21000.0), -30.0, 100);
        let b = (Some(21000.0 + SAME_CUTOFF - 1.0), -30.0, 100);
        let c = (Some(21000.0 + 2.0 * SAME_CUTOFF + 200.0), -30.0, 100);
        assert_eq!(found(&series(&[a, b, a, b, c, c])), vec![(0, 4), (4, 6)]);
    }

    #[test]
    fn a_quiet_slice_is_neutral() {
        let cut = (Some(16800.0), -30.0, 100);
        let quiet = (None, -80.0, 100);
        let few = (Some(16800.0), -30.0, 10);
        let full = (None, -30.0, 100);
        // Inside a run it does not cut it; at the ends it is left out; alone it makes nothing.
        assert_eq!(found(&series(&[full, cut, quiet, few, cut, full])), vec![(1, 5)]);
        assert_eq!(found(&series(&[full, full, quiet, cut, cut])), vec![(0, 2), (3, 5)]);
        assert!(found(&series(&[quiet, few, quiet])).is_empty());
    }

    #[test]
    fn the_whole_file_is_not_a_part() {
        let cut = (Some(16800.0), -30.0, 100);
        assert!(found(&series(&[cut, cut, cut])).is_empty());
    }

    #[test]
    fn the_rest_of_the_file_is_its_longest_group() {
        let full = (None, -30.0, 100);
        let cut = (Some(16800.0), -30.0, 100);
        let v = series(&[cut, cut, full, full, full, full, cut]);
        let refs: Vec<&Slice> = v.iter().collect();
        assert_eq!(dominant(&refs).map(look), Some(Look::Full));
    }
}
