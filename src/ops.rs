//! The tasks run in the background: Details, Verify, Create checksums, Test, Lossless?,
//! Torrent, Spectrogram.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha1::Sha1;

use crate::audio::{self, hex, AudioInfo, Format, ZERO_MD5};
use crate::files::{self, ext_lower, file_name, rel_posix, Show};

pub const APP_NAME: &str = "Bootleg Toolbox";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Level {
    #[default]
    Info,
    Ok,
    Warn,
    Error,
}

impl Level {
    fn rank(self) -> u8 {
        match self {
            Level::Info => 0,
            Level::Ok => 1,
            Level::Warn => 2,
            Level::Error => 3,
        }
    }
}

pub enum Msg {
    Log(String, Level),
    Status { path: PathBuf, text: String, level: Level, tooltip: String },
    Info { path: PathBuf, info: AudioInfo },
    Progress { done: usize, total: usize },
    Image { path: PathBuf, image: crate::spectrogram::Image },
    /// Lossless check verdict, for the Source column, with the full analysis.
    Source { path: PathBuf, text: String, level: Level, tooltip: String, report: Arc<crate::lossless::Report> },
    /// Speed check verdict, for the Speed column, with the full analysis.
    Speed { path: PathBuf, text: String, level: Level, tooltip: String, report: Arc<crate::report::Report> },
    /// After a conversion: the sources converted successfully (except the float WAVs that
    /// were rounded, whose originals are kept), how many were not, how many were rounded.
    Converted { target: Target, sources: Vec<PathBuf>, failed: usize, rounded: usize },
    /// Float WAVs read before a conversion to FLAC (see `encode::scan_float`).
    FloatScan(Vec<(PathBuf, crate::encode::FloatScan)>),
    /// A media info summary: its name (show or DVD) and text.
    MediaInfo { title: String, text: String },
    Finished { summary: String, rescan: bool },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tracker {
    Dime,
    Ttd,
    /// One of `Options::custom_trackers`.
    Custom(usize),
}

/// A tracker added by the user in Settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CustomTracker {
    pub name: String,
    pub announce: String,
}

impl CustomTracker {
    /// For the file name ("<show> [Name].torrent") and the "source" field: the name without
    /// characters that file systems refuse.
    pub fn tag(&self) -> String {
        let tag: String = self.name.trim().chars().filter(|c| !"\\/:*?\"<>|[]".contains(*c) && !c.is_control()).collect();
        tag.trim().to_string()
    }
}

impl Tracker {
    pub fn label(self, opt: &Options) -> String {
        match self {
            Tracker::Dime => "DIME".into(),
            Tracker::Ttd => "The Traders' Den".into(),
            Tracker::Custom(i) => opt.custom_trackers.get(i).map(|t| t.name.trim().to_string()).unwrap_or_default(),
        }
    }
    pub fn tag(self, opt: &Options) -> String {
        match self {
            Tracker::Dime => "DIME".into(),
            Tracker::Ttd => "TTD".into(),
            Tracker::Custom(i) => opt.custom_trackers.get(i).map(|t| t.tag()).unwrap_or_default(),
        }
    }
    pub fn announce(self, opt: &Options) -> String {
        match self {
            Tracker::Dime => opt.announce_dime.trim().to_string(),
            Tracker::Ttd => opt.announce_ttd.trim().to_string(),
            Tracker::Custom(i) => opt.custom_trackers.get(i).map(|t| t.announce.trim().to_string()).unwrap_or_default(),
        }
    }
    /// Every tracker: the two defaults, then the user's.
    pub fn all(opt: &Options) -> Vec<Tracker> {
        let mut all = vec![Tracker::Dime, Tracker::Ttd];
        all.extend((0..opt.custom_trackers.len()).map(Tracker::Custom));
        all
    }
    pub fn default_announce(self) -> &'static str {
        match self {
            Tracker::Dime => "http://bt.dimeadozen.org/announce.php",
            Tracker::Ttd => "http://www.thetradersden.org/forums/tracker/announce.php",
            Tracker::Custom(_) => "",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub create_ffp: bool,
    pub create_md5: bool,
    pub create_st5: bool,
    /// SFV: CRC32 of whole files, found on older sets.
    pub create_sfv: bool,
    pub overwrite: bool,
    pub crlf: bool,
    pub jobs: usize,
    pub auto_details: bool,
    /// Lossless? also looks for AAC coding traces (several times slower; off by default).
    pub aac_check: bool,
    pub announce_dime: String,
    pub announce_ttd: String,
    pub torrent_dir: String,
    pub torrent_private: bool,
    pub torrent_source: bool,
    /// Other trackers, added by the user.
    pub custom_trackers: Vec<CustomTracker>,
    pub theme: ThemeChoice,
    /// FLAC compression level used by "To FLAC" (0-8).
    pub flac_level: u8,
    /// Folder for MP3 copies (empty: next to the show folder).
    pub mp3_dir: String,
    /// Default choices of the window shown after a conversion to FLAC.
    pub after_flac_trash: bool,
    pub after_flac_old: bool,
    pub after_flac_ffp: bool,
    /// The log panel is unfolded (it unfolds by itself on an error).
    pub log_open: bool,
    /// The main window stays above the other windows.
    pub always_on_top: bool,
    /// "Open in Bootleg Toolbox" in the file manager's right-click menu.
    pub shell_integration: bool,
    /// Look for a newer release on GitHub at startup (the only network access; off by default).
    pub check_updates: bool,
    /// File names chosen in "Tag and rename from info file".
    pub tag_scheme: crate::tagging::NameScheme,
    /// The entries of the former name (Bootleg Helper) have been looked for and removed.
    pub legacy_cleaned: bool,
    /// Compact view: fewer columns (Format then gives bits and rate), tighter rows.
    pub compact: bool,
    /// Table columns the user chose to hide (by title).
    pub hidden_columns: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

impl Default for Options {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        Options {
            create_ffp: true,
            create_md5: false,
            create_st5: true,
            create_sfv: false,
            overwrite: false,
            crlf: cfg!(windows),
            jobs: cpus.clamp(1, 8),
            auto_details: true,
            aac_check: false,
            announce_dime: Tracker::Dime.default_announce().into(),
            announce_ttd: Tracker::Ttd.default_announce().into(),
            torrent_dir: String::new(),
            torrent_private: true,
            torrent_source: true,
            custom_trackers: Vec::new(),
            theme: ThemeChoice::System,
            flac_level: 8,
            mp3_dir: String::new(),
            after_flac_trash: true,
            after_flac_old: true,
            after_flac_ffp: true,
            hidden_columns: Vec::new(),
            tag_scheme: Default::default(),
            log_open: false,
            compact: false,
            always_on_top: false,
            shell_integration: false,
            legacy_cleaned: false,
            check_updates: false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Op {
    Details { verbose: bool },
    Verify,
    Create,
    Test,
    Torrent(Vec<Tracker>),
    Spectrogram,
    Lossless,
    Speed,
    /// `round_float`: float WAVs that do not hold exact 16/24-bit values are rounded to
    /// 24 bits (otherwise they are refused).
    Convert { target: Target, level: u8, round_float: bool },
    /// Reads the float WAVs among the files before a conversion to FLAC.
    ScanFloat,
    Mp3(crate::encode::Mp3Mode),
    FixSbe,
    CleanWav,
    /// Checks the files against these .torrent files.
    TorrentCheck(Vec<PathBuf>),
    /// Technical summary of the selected files and of these DVDs (VIDEO_TS folders, VOB files).
    MediaInfo(Vec<PathBuf>),
    /// Tags and new names from the info file (the choices of its window).
    Tag(Vec<crate::tagging::TagJob>),
    /// Follow-up of a conversion to FLAC (the choices of its window).
    AfterFlac { roots: Vec<PathBuf>, sources: Vec<PathBuf>, trash_sources: bool, trash_checksums: Vec<PathBuf>, create_ffp: bool },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Flac,
    Wav,
}

impl Target {
    pub fn name(self) -> &'static str {
        match self {
            Target::Flac => "FLAC",
            Target::Wav => "WAV",
        }
    }
    fn format(self) -> Format {
        match self {
            Target::Flac => Format::Flac,
            Target::Wav => Format::Wav,
        }
    }
}

impl Op {
    pub fn label(&self) -> &'static str {
        match self {
            Op::Details { .. } => "Reading file details",
            Op::Verify => "Verifying checksums",
            Op::Create => "Creating checksum files",
            Op::Test => "Decode test",
            Op::Torrent(_) => "Creating torrents",
            Op::Spectrogram => "Rendering spectrogram",
            Op::Lossless => "Checking lossless origin",
            Op::Speed => "Checking tape speed",
            Op::Convert { target: Target::Flac, .. } => "Converting to FLAC",
            Op::Convert { target: Target::Wav, .. } => "Converting to WAV",
            Op::ScanFloat => "Reading the 32-bit float files",
            Op::Mp3(_) => "Encoding MP3 copies",
            Op::FixSbe => "Fixing sector boundary errors",
            Op::CleanWav => "Cleaning WAV headers",
            Op::TorrentCheck(_) => "Checking against the torrent",
            Op::MediaInfo(_) => "Reading media info",
            Op::Tag(_) => "Tagging from the info file",
            Op::AfterFlac { .. } => "Tidying up after the conversion",
        }
    }
}

pub struct Ctx {
    tx: Sender<Msg>,
    pub cancel: Arc<AtomicBool>,
    pub opt: Options,
    done: AtomicUsize,
    total: AtomicUsize,
    rescan: AtomicBool,
}

impl Ctx {
    pub fn log(&self, text: impl Into<String>, level: Level) {
        let _ = self.tx.send(Msg::Log(text.into(), level));
    }
    pub fn status(&self, path: &Path, text: impl Into<String>, level: Level, tooltip: impl Into<String>) {
        let _ = self.tx.send(Msg::Status {
            path: path.to_path_buf(),
            text: text.into(),
            level,
            tooltip: tooltip.into(),
        });
    }
    fn info(&self, path: &Path, info: &AudioInfo) {
        let _ = self.tx.send(Msg::Info { path: path.to_path_buf(), info: info.clone() });
    }
    fn add_total(&self, n: usize) {
        let total = self.total.fetch_add(n, Ordering::SeqCst) + n;
        let _ = self.tx.send(Msg::Progress { done: self.done.load(Ordering::SeqCst), total });
    }
    fn advance(&self, n: usize) {
        let done = self.done.fetch_add(n, Ordering::SeqCst) + n;
        let _ = self.tx.send(Msg::Progress { done, total: self.total.load(Ordering::SeqCst) });
    }
    fn tick(&self) {
        let done = self.done.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.tx.send(Msg::Progress { done, total: self.total.load(Ordering::SeqCst) });
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Starts an operation on a background thread; progress arrives through `tx`.
pub fn spawn(op: Op, shows: Vec<Show>, files: Vec<PathBuf>, opt: Options, tx: Sender<Msg>, cancel: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let ctx = Ctx {
            tx: tx.clone(),
            cancel,
            opt,
            done: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            rescan: AtomicBool::new(false),
        };
        let pool = rayon::ThreadPoolBuilder::new().num_threads(ctx.opt.jobs.max(1)).build();
        let summary = match pool {
            Ok(pool) => pool.install(|| match &op {
                Op::Details { verbose } => op_details(&ctx, &shows, &files, *verbose),
                Op::Verify => op_verify(&ctx, &shows),
                Op::Create => op_create(&ctx, &shows),
                Op::Test => op_test(&ctx, &files),
                Op::Torrent(trackers) => op_torrent(&ctx, &shows, trackers),
                Op::Spectrogram => op_spectrogram(&ctx, &files),
                Op::Lossless => op_lossless(&ctx, &shows, &files),
                Op::Speed => op_speed(&ctx, &shows, &files),
                Op::Convert { target, level, round_float } => op_convert(&ctx, &files, *target, *level, *round_float),
                Op::ScanFloat => op_scan_float(&ctx, &files),
                Op::Mp3(mode) => op_mp3(&ctx, &shows, &files, *mode),
                Op::FixSbe => op_fix_sbe(&ctx, &shows, &files),
                Op::CleanWav => op_clean_wav(&ctx, &shows, &files),
                Op::TorrentCheck(torrents) => op_torrent_check(&ctx, &shows, torrents),
                Op::MediaInfo(dvds) => op_media_info(&ctx, &shows, &files, dvds),
                Op::Tag(jobs) => op_tag(&ctx, jobs),
                Op::AfterFlac { roots, sources, trash_sources, trash_checksums, create_ffp } => {
                    op_after_flac(&ctx, roots, sources, *trash_sources, trash_checksums, *create_ffp)
                }
            }),
            Err(e) => format!("Cannot start worker threads: {e}"),
        };
        let summary = if ctx.cancelled() { "Stopped.".to_string() } else { summary };
        let _ = tx.send(Msg::Finished { summary, rescan: ctx.rescan.load(Ordering::SeqCst) });
    });
}

/// Runs `f` on every item in parallel; errors are reported against `path_of(item)`.
fn par_map<T: Sync, R: Send>(
    ctx: &Ctx,
    items: &[T],
    path_of: impl Fn(&T) -> PathBuf + Sync,
    f: impl Fn(&T) -> Result<R, String> + Sync,
) -> Vec<Option<R>> {
    items
        .par_iter()
        .map(|item| {
            if ctx.cancelled() {
                return None;
            }
            let result = f(item);
            ctx.tick();
            match result {
                Ok(value) => Some(value),
                Err(e) => {
                    if !ctx.cancelled() {
                        let path = path_of(item);
                        ctx.status(&path, format!("Error: {e}"), Level::Error, "");
                        ctx.log(format!("{}: {e}", file_name(&path)), Level::Error);
                    }
                    None
                }
            }
        })
        .collect()
}

fn plural(count: usize, singular: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { many })
}

// ---------------------------------------------------------------- Details

const PROBLEMS: [(&str, &str, &str, &str); 5] = [
    ("lossy", "Lossy file (MP3, AAC...): not for lossless trading", "lossy track", "lossy tracks"),
    ("empty", "Empty or truncated: no audio samples", "empty track", "empty tracks"),
    ("gap", "Gap risk if burned: SBE before the last track", "track with gap risk", "tracks with gap risk"),
    ("no_md5", "No MD5 in header: FFP will be zeros, re-encode", "track without MD5", "tracks without MD5"),
    ("unsupported", "Format not supported yet by this version", "unsupported track", "unsupported tracks"),
];

fn problem(key: &str) -> (&'static str, &'static str, &'static str) {
    let p = PROBLEMS.iter().find(|p| p.0 == key).unwrap();
    (p.1, p.2, p.3)
}

pub fn quality_label(info: &AudioInfo) -> String {
    if info.lossy() {
        return format!("{} {} kHz (lossy)", info.name(), audio::trim_float(info.rate as f64 / 1000.0));
    }
    let mut label = if info.is_cd() {
        "CD-ready".to_string()
    } else {
        let float = if info.float { " float" } else { "" };
        let mut l = format!("{}-bit{float}/{} kHz", info.bits, audio::trim_float(info.rate as f64 / 1000.0));
        if !info.float && (info.bits >= 24 || info.rate > 48000) {
            l.push_str(" hi-res");
        }
        l
    };
    if info.format != Format::Flac {
        label.push(' ');
        label.push_str(info.name());
    }
    label
}

fn channel_name(count: u32) -> String {
    match count {
        1 => "mono".into(),
        2 => "stereo".into(),
        n => format!("{n} channels"),
    }
}

/// (Result text, checklist tooltip, level, problem keys)
pub fn describe_details(info: &AudioInfo, last_of_disc: bool) -> (String, String, Level, Vec<&'static str>) {
    let mut checks: Vec<(Option<bool>, String)> = Vec::new();
    let mut problems = Vec::new();
    checks.push((Some(true), format!("Header readable ({} file)", info.name())));
    let quality = if info.lossy() {
        format!("{} (lossy), {}, {}", info.name(), info.rate_label(), channel_name(info.channels))
    } else {
        format!("{}-bit, {}, {}", info.bits, info.rate_label(), channel_name(info.channels))
    };
    if info.lossy() {
        problems.push("lossy");
    }
    checks.push((
        Some(true),
        if info.is_cd() { format!("CD audio format: {quality}") } else { format!("Audio format: {quality} (not the CD audio format)") },
    ));
    if info.samples > 0 {
        checks.push((Some(true), format!("Length {} ({} samples)", info.length(), info.samples)));
    } else {
        checks.push((Some(false), "No audio samples: the file is empty or truncated".into()));
        problems.push("empty");
    }
    match info.md5_set() {
        Some(true) => checks.push((Some(true), "MD5 signature present in the FLAC header: the FFP line will be valid".into())),
        Some(false) => {
            checks.push((Some(false), "No MD5 signature in the FLAC header: its FFP line will be all zeros and decoding cannot be verified. Re-encode the file to add one".into()));
            problems.push("no_md5");
        }
        None => {}
    }
    let mut sbe_note = "";
    match info.sbe() {
        None => checks.push((None, "SBE check not applicable (only for 16-bit/44.1 kHz stereo)".into())),
        Some(false) => checks.push((Some(true), "No sector boundary error: burns to CD without gaps between tracks".into())),
        Some(true) if last_of_disc => {
            checks.push((Some(true), "Sector boundary error on the last track of the disc only, which is normal: still burns without gaps".into()));
            sbe_note = " (SBE on last track only)";
        }
        Some(true) => {
            checks.push((Some(false), "Sector boundary error on a track that is not the last of its disc: burned to CD, it would leave a short gap or click".into()));
            problems.push("gap");
        }
    }
    let (text, level) = if !problems.is_empty() {
        let text = problems.iter().map(|k| problem(k).0).collect::<Vec<_>>().join("; ");
        (text, if problems.contains(&"empty") { Level::Error } else { Level::Warn })
    } else {
        let mut parts = vec![format!("{}{sbe_note}", quality_label(info))];
        if info.md5_set() == Some(true) {
            parts.push("FFP-ready".into());
        }
        let mut text = parts.join(", ");
        if !info.is_cd() {
            text.push_str(" (not for CD)");
        }
        (text, Level::Ok)
    };
    let mut tooltip = String::from(
        "Details (read from the file header; use Verify or Test to check the audio itself):\n",
    );
    if info.is_cd() && problems.is_empty() {
        tooltip.push_str("CD-ready = CD audio format and no gap risk when burned.\n");
    }
    for (state, full) in &checks {
        let mark = match state {
            Some(true) => '✔',
            Some(false) => '✘',
            None => '–',
        };
        tooltip.push_str(&format!("{mark} {full}\n"));
    }
    (text, tooltip.trim_end().to_string(), level, problems)
}

fn last_of_each_disc(shows: &[Show]) -> HashSet<PathBuf> {
    let mut last: HashMap<PathBuf, PathBuf> = HashMap::new();
    for show in shows {
        for f in &show.files {
            if let Some(parent) = f.parent() {
                last.insert(parent.to_path_buf(), f.clone()); // files are in order
            }
        }
    }
    last.into_values().collect()
}

pub fn op_details(ctx: &Ctx, shows: &[Show], files: &[PathBuf], verbose: bool) -> String {
    ctx.add_total(files.len());
    let last = last_of_each_disc(shows);
    let results = par_map(ctx, files, |p| p.clone(), |path| {
        let Some(format) = Format::from_path(path) else { return Err("not an audio file".into()) };
        if !format.supported() {
            let (text, _, _) = problem("unsupported");
            ctx.status(path, text, Level::Warn, "APE, SHN and WavPack support comes in the next version.");
            return Ok((None, vec!["unsupported"], text.to_string(), Level::Warn));
        }
        let info = audio::probe(path)?;
        ctx.info(path, &info);
        let (text, tooltip, level, problems) = describe_details(&info, last.contains(path));
        ctx.status(path, &text, level, tooltip);
        if !problems.is_empty() && !verbose {
            ctx.log(format!("{}: {text}", file_name(path)), level);
        }
        Ok((Some(info), problems, text, level))
    });
    let by_path: HashMap<&PathBuf, &(Option<AudioInfo>, Vec<&str>, String, Level)> =
        files.iter().zip(results.iter()).filter_map(|(p, r)| r.as_ref().map(|r| (p, r))).collect();

    let selected: HashSet<&PathBuf> = files.iter().collect();
    for show in shows {
        let done: Vec<_> = show.files.iter().filter(|f| selected.contains(f)).filter_map(|f| by_path.get(f).copied()).collect();
        if done.is_empty() {
            continue;
        }
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for (_, problems, _, _) in &done {
            for key in problems {
                match counts.iter_mut().find(|(k, _)| k == key) {
                    Some(entry) => entry.1 += 1,
                    None => counts.push((key, 1)),
                }
            }
        }
        let (text, level) = if !counts.is_empty() {
            let text = counts.iter().map(|(k, n)| {
                let (_, one, many) = problem(k);
                plural(*n, one, many)
            }).collect::<Vec<_>>().join(", ");
            let flagged: Vec<String> = show.files.iter().filter(|f| by_path.get(f).is_some_and(|r| !r.1.is_empty())).map(|f| rel_posix(f, &show.root)).collect();
            ctx.status(&show.root, &text, Level::Warn, format!("Tracks to check:\n{}", flagged.join("\n")));
            (text, Level::Warn)
        } else {
            let infos: Vec<&AudioInfo> = done.iter().filter_map(|r| r.0.as_ref()).collect();
            let mut qualities: Vec<String> = infos.iter().map(|i| quality_label(i)).collect();
            qualities.sort();
            qualities.dedup();
            let mut text = format!("{}, {}", plural(infos.len(), "track", "tracks"), qualities.join(" + "));
            if infos.iter().all(|i| i.md5_set() == Some(true)) {
                text.push_str(", FFP-ready");
            }
            ctx.status(&show.root, &text, Level::Ok,
                "Every track has a readable header and audio, FLAC files carry an MD5 signature, and SBEs (if any) are only on the last track of a disc.\nThis does not check the audio itself: use Verify or Test for that.");
            (text, Level::Ok)
        };
        if verbose {
            ctx.log(format!("{}: {text}", file_name(&show.root)), level);
            for f in &show.files {
                if let Some(r) = by_path.get(f) {
                    ctx.log(format!("    {}: {}", rel_posix(f, &show.root), r.2), r.3);
                }
            }
        }
    }
    let fine = by_path.values().filter(|r| r.1.is_empty()).count();
    let unreadable = results.iter().filter(|r| r.is_none()).count();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in by_path.values() {
        for k in &r.1 {
            *counts.entry(k).or_default() += 1;
        }
    }
    let mut issues: Vec<String> = counts.iter().map(|(k, n)| {
        let (_, one, many) = problem(k);
        plural(*n, one, many)
    }).collect();
    if unreadable > 0 {
        issues.push(format!("{unreadable} unreadable"));
    }
    let mut summary = format!("Details read for {} file(s): {fine} without issue", files.len());
    if !issues.is_empty() {
        summary.push_str("; to check: ");
        summary.push_str(&issues.join(", "));
    }
    summary
}

// ---------------------------------------------------------------- Verify

struct VerifyJob {
    root: PathBuf,
    checksum_file: PathBuf,
    kind: String,
    name: String,
    digest: String,
    target: Option<PathBuf>,
}

fn st5_digest(path: &Path, cancel: &AtomicBool) -> Result<String, String> {
    let format = Format::from_path(path).ok_or("not an audio file")?;
    if !format.supported() {
        return Err(format!("{} files are not supported yet", format.name()));
    }
    let info = audio::probe(path)?;
    let (md5, frames) = audio::pcm_md5(path, cancel)?;
    // The FLAC reader skips damaged frames: never fingerprint incomplete audio.
    if info.samples > 0 && !audio::length_ok(&info, frames) {
        return Err(format!("damaged or truncated audio, only {frames} of {} samples decode", info.samples));
    }
    Ok(hex(&md5))
}

pub fn op_verify(ctx: &Ctx, shows: &[Show]) -> String {
    let mut jobs = Vec::new();
    for show in shows {
        let checksum_files: Vec<PathBuf> = files::all_files(&show.root, true).into_iter().filter(|p| files::is_checksum_file(p)).collect();
        if checksum_files.is_empty() {
            ctx.log(format!("{}: no checksum file (.ffp, .md5, .st5, .sfv) found", file_name(&show.root)), Level::Warn);
            ctx.status(&show.root, "No checksum file", Level::Warn, "");
            for f in &show.files {
                ctx.status(f, "No checksum file", Level::Warn, "");
            }
            continue;
        }
        let mut covered = HashSet::new();
        for cs in checksum_files {
            let (kind, entries) = match files::parse_checksum_file(&cs) {
                Ok(v) => v,
                Err(e) => {
                    ctx.log(format!("Cannot read {}: {e}", cs.display()), Level::Error);
                    continue;
                }
            };
            if entries.is_empty() {
                ctx.log(format!("{}: no valid checksum line", file_name(&cs)), Level::Warn);
            }
            let base = cs.parent().unwrap_or(&show.root).to_path_buf();
            for (name, digest) in entries {
                let target = files::resolve_entry(&base, &name);
                if let Some(t) = &target {
                    covered.insert(t.clone());
                }
                jobs.push(VerifyJob { root: show.root.clone(), checksum_file: cs.clone(), kind: kind.clone(), name, digest, target });
            }
        }
        for f in &show.files {
            if !covered.contains(f) {
                ctx.status(f, "Not listed in any checksum file", Level::Warn, "");
            }
        }
    }
    ctx.add_total(jobs.len());
    let marks: Mutex<HashMap<PathBuf, Vec<(String, &str, Level)>>> = Mutex::new(HashMap::new());
    let record = |job: &VerifyJob, word: &'static str, level: Level| {
        let Some(target) = &job.target else { return };
        let mut marks = marks.lock().unwrap();
        let entry = marks.entry(target.clone()).or_default();
        entry.push((job.kind.to_uppercase(), word, level));
        let text = entry.iter().map(|(k, w, _)| format!("{k} {w}")).collect::<Vec<_>>().join(", ");
        let worst = entry.iter().map(|e| e.2).max_by_key(|l| l.rank()).unwrap_or(Level::Info);
        ctx.status(target, text, worst, "");
    };
    let results = par_map(ctx, &jobs, |j| j.target.clone().unwrap_or(j.checksum_file.clone()), |job| {
        let Some(target) = &job.target else {
            ctx.log(format!("{}: {} - file not found", file_name(&job.checksum_file), job.name), Level::Error);
            return Ok(Level::Error);
        };
        let actual = if job.kind == "ffp" && Format::from_path(target) == Some(Format::Flac) {
            let info = audio::probe(target)?;
            let md5 = info.md5.unwrap_or(ZERO_MD5);
            if md5 == ZERO_MD5 {
                record(job, "no MD5 in header", Level::Warn);
                return Ok(Level::Warn);
            }
            hex(&md5)
        } else if job.kind == "ffp" || job.kind == "st5" {
            st5_digest(target, &ctx.cancel)?
        } else if job.kind == "sfv" {
            format!("{:08x}", audio::file_crc32(target, &ctx.cancel)?)
        } else {
            hex(&audio::file_md5(target, &ctx.cancel)?)
        };
        if actual == job.digest {
            record(job, "OK", Level::Ok);
            Ok(Level::Ok)
        } else {
            record(job, "MISMATCH", Level::Error);
            ctx.log(format!("{}: {} - checksum MISMATCH", file_name(&job.checksum_file), job.name), Level::Error);
            Ok(Level::Error)
        }
    });
    for show in shows {
        let mine: Vec<&Option<Level>> = jobs.iter().zip(results.iter()).filter(|(j, _)| j.root == show.root).map(|(_, r)| r).collect();
        if mine.is_empty() {
            continue;
        }
        let problems = mine.iter().filter(|r| !matches!(r, Some(Level::Ok))).count();
        let name = file_name(&show.root);
        if problems > 0 {
            let text = format!("{problems} problem(s) in {} checksums", mine.len());
            ctx.status(&show.root, &text, Level::Error, "");
            ctx.log(format!("{name}: {text}"), Level::Error);
        } else {
            ctx.status(&show.root, format!("All {} checksums OK", mine.len()), Level::Ok, "");
            ctx.log(format!("{name}: all {} checksums OK", mine.len()), Level::Ok);
        }
    }
    summarize(&results, "checksum(s) verified")
}

/// "11 file(s) tested: 9 OK, 1 warning(s), 1 error(s)"; a failed item (None) counts as an error.
fn summarize(results: &[Option<Level>], noun: &str) -> String {
    let ok = results.iter().filter(|r| matches!(r, Some(Level::Ok))).count();
    let warn = results.iter().filter(|r| matches!(r, Some(Level::Warn))).count();
    let bad = results.len() - ok - warn;
    let mut summary = format!("{} {noun}: {ok} OK", results.len());
    if warn > 0 { summary.push_str(&format!(", {warn} warning(s)")); }
    if bad > 0 { summary.push_str(&format!(", {bad} error(s)")); }
    summary
}

// ---------------------------------------------------------------- Create

pub fn op_create(ctx: &Ctx, shows: &[Show]) -> String {
    let mut kinds = Vec::new();
    if ctx.opt.create_ffp { kinds.push("ffp"); }
    if ctx.opt.create_md5 { kinds.push("md5"); }
    if ctx.opt.create_st5 { kinds.push("st5"); }
    if ctx.opt.create_sfv { kinds.push("sfv"); }
    if kinds.is_empty() {
        ctx.log("No checksum type selected in Settings.", Level::Warn);
        return "Nothing to do.".into();
    }
    create_checksums(ctx, shows, &kinds)
}

pub fn create_checksums(ctx: &Ctx, shows: &[Show], kinds: &[&'static str]) -> String {
    let mut plan: Vec<(&Show, &str, PathBuf, Vec<PathBuf>)> = Vec::new();
    for show in shows {
        for &kind in kinds {
            let targets: Vec<PathBuf> = if kind == "ffp" {
                show.files.iter().filter(|f| Format::from_path(f) == Some(Format::Flac)).cloned().collect()
            } else {
                show.files.clone()
            };
            if targets.is_empty() {
                continue;
            }
            let out = show.root.join(format!("{}.{kind}", file_name(&show.root)));
            if out.exists() && !ctx.opt.overwrite {
                ctx.log(format!("{} already exists, skipped (allow overwriting in Settings)", file_name(&out)), Level::Warn);
                continue;
            }
            plan.push((show, kind, out, targets));
        }
    }
    let mut work: Vec<(&str, PathBuf)> = Vec::new();
    for (_, kind, _, targets) in &plan {
        for t in targets {
            if !work.iter().any(|(k, p)| k == kind && p == t) {
                work.push((kind, t.clone()));
            }
        }
    }
    ctx.add_total(work.len());
    let digests = par_map(ctx, &work, |(_, p)| p.clone(), |(kind, path)| match *kind {
        "ffp" => {
            let md5 = audio::probe(path)?.md5.unwrap_or(ZERO_MD5);
            if md5 == ZERO_MD5 {
                ctx.status(path, "No MD5 signature in FLAC header (re-encode it)", Level::Warn, "");
            }
            Ok(hex(&md5))
        }
        "st5" => st5_digest(path, &ctx.cancel),
        "sfv" => Ok(format!("{:08x}", audio::file_crc32(path, &ctx.cancel)?)),
        _ => Ok(hex(&audio::file_md5(path, &ctx.cancel)?)),
    });
    if ctx.cancelled() {
        return "Stopped.".into();
    }
    let lookup: HashMap<(&str, &PathBuf), &String> = work.iter().zip(digests.iter())
        .filter_map(|((k, p), d)| d.as_ref().map(|d| ((*k, p), d))).collect();
    let newline = if ctx.opt.crlf { "\r\n" } else { "\n" };
    let mut written = 0;
    let mut created: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for (show, kind, out, targets) in &plan {
        let mut lines = Vec::new();
        let mut failed = 0;
        for t in targets {
            match lookup.get(&(*kind, t)) {
                Some(d) => {
                    let rel = rel_posix(t, &show.root);
                    lines.push(match *kind {
                        "ffp" => format!("{rel}:{d}"),
                        "st5" => format!("{d}  [shntool]  {rel}"),
                        "sfv" => format!("{rel} {}", d.to_uppercase()),
                        _ => format!("{d}  {rel}"),
                    });
                }
                None => failed += 1,
            }
        }
        if failed > 0 {
            ctx.log(format!("{} not written: {failed} file(s) could not be read", file_name(out)), Level::Error);
            continue;
        }
        let content = lines.join(newline) + newline;
        match fs::write(out, content) {
            Ok(()) => {
                written += 1;
                ctx.log(format!("Created {}", out.display()), Level::Ok);
                match created.iter_mut().find(|(r, _)| r == &show.root) {
                    Some(entry) => entry.1.push(kind.to_uppercase()),
                    None => created.push((show.root.clone(), vec![kind.to_uppercase()])),
                }
            }
            Err(e) => ctx.log(format!("Cannot write {}: {e}", out.display()), Level::Error),
        }
    }
    for (root, kinds) in created {
        ctx.status(&root, format!("Created {}", kinds.join(", ")), Level::Ok, "");
    }
    format!("{written} checksum file(s) created.")
}

// ---------------------------------------------------------------- Lossless?

pub fn op_lossless(ctx: &Ctx, shows: &[Show], files: &[PathBuf]) -> String {
    ctx.add_total(files.len());
    let source = |path: &Path, text: &str, level: Level, tooltip: &str, report: crate::lossless::Report| {
        let _ = ctx.tx.send(Msg::Source {
            path: path.to_path_buf(),
            text: text.into(),
            level,
            tooltip: tooltip.into(),
            report: Arc::new(report),
        });
    };
    let results = par_map(ctx, files, |p| p.clone(), |path| {
        let info = audio::probe(path)?;
        ctx.info(path, &info);
        let verdict = crate::lossless::analyse(path, &info, ctx.opt.aac_check, &ctx.cancel)?;
        let tooltip = format!(
            "Lossless check:\n{}",
            verdict.details.iter().map(|d| format!("\u{2022} {d}")).collect::<Vec<_>>().join("\n")
        );
        let tooltip = format!("{tooltip}\n\nClick to see the full analysis.");
        source(path, &verdict.label, verdict.level, &tooltip, verdict.report.clone());
        ctx.status(path, &verdict.sentence, verdict.level, &tooltip);
        if matches!(verdict.level, Level::Warn | Level::Error) {
            ctx.log(format!("{}: {}", file_name(path), verdict.sentence), verdict.level);
        }
        Ok((verdict.label, verdict.level, verdict.report))
    });
    let by_path: HashMap<&PathBuf, (&String, &crate::lossless::Report)> =
        files.iter().zip(results.iter()).filter_map(|(f, r)| r.as_ref().map(|(label, _, report)| (f, (label, report)))).collect();
    for show in shows {
        let done: Vec<(&PathBuf, &String, &crate::lossless::Report)> =
            show.files.iter().filter_map(|f| by_path.get(f).map(|(l, r)| (f, *l, *r))).collect();
        let labels: Vec<&String> = done.iter().map(|(_, l, _)| *l).collect();
        if labels.is_empty() {
            continue;
        }
        // Counts in order of first appearance, then sorted by count (as the reference).
        let mut counts: Vec<(String, usize)> = Vec::new();
        for label in &labels {
            // "Lossy upstream 16.1 kHz" counts as "Lossy upstream": the frequency is dropped.
            let key = match label.strip_suffix(" kHz").and_then(|l| l.rsplit_once(' ')) {
                Some((head, _)) => head.to_string(),
                None => label.to_string(),
            };
            match counts.iter_mut().find(|(k, _)| *k == key) {
                Some(entry) => entry.1 += 1,
                None => counts.push((key, 1)),
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1));
        let text = counts.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(", ");
        let flagged: usize =
            counts.iter().filter(|(k, _)| !["Lossless", "FM broadcast", crate::lossless::FM_15K, "Unclear", "Silent"].contains(&k.as_str())).map(|(_, n)| n).sum();
        // "Lossy" (encoder holes found) in red; "Lossy?" alone is only a doubt.
        let level = if counts.iter().any(|(k, _)| k == "Lossy") {
            Level::Error
        } else if flagged > 0 {
            Level::Warn
        } else {
            Level::Ok
        };
        let all_lossless = counts.len() == 1 && counts[0].0 == "Lossless";
        let shown = if all_lossless { "All lossless".to_string() } else { text.clone() };
        // Show report: every track, then the reasoning of the most common verdict.
        let main = &counts[0].0;
        let example = done.iter().find(|(_, l, _)| l.starts_with(main.as_str())).map(|(_, _, r)| *r);
        let mut reasoning = vec![format!("{} track(s) analysed: {text}.", done.len())];
        if let Some(r) = example {
            reasoning.push(format!("Most tracks read \"{main}\". Why, for {}:", r.title));
            reasoning.extend(r.reasoning.iter().map(|line| format!("    {line}")));
        }
        let report = crate::lossless::Report {
            heading: "Lossless analysis".into(),
            title: file_name(&show.root),
            label: shown.clone(),
            level,
            sentence: example.map(|r| r.sentence.clone()).unwrap_or_default(),
            measures: Vec::new(),
            tracks: done
                .iter()
                .map(|(f, _, r)| crate::lossless::Measure {
                    name: rel_posix(f, &show.root),
                    value: r.label.clone(),
                    meaning: r.key_facts.clone(),
                    level: r.level,
                })
                .collect(),
            reasoning,
            key_facts: String::new(),
        };
        source(&show.root, &shown, level, &format!("Lossless check of the show:\n{text}\n\nClick to see the full analysis."), report);
    }
    // Lossy sources count as errors (the reference counted them as warnings).
    let levels: Vec<Option<Level>> = results
        .iter()
        .flatten()
        .map(|(_, level, _)| Some(if *level == Level::Info { Level::Warn } else { *level }))
        .collect();
    summarize(&levels, "file(s) checked for lossless origin")
}

// ---------------------------------------------------------------- Conversions

/// The float WAVs among `files`, read through: what a conversion to FLAC would do with them.
pub fn op_scan_float(ctx: &Ctx, files: &[PathBuf]) -> String {
    let floats: Vec<PathBuf> = files
        .iter()
        .filter(|p| Format::from_path(p) == Some(Format::Wav) && audio::probe(p).is_ok_and(|i| i.float))
        .cloned()
        .collect();
    ctx.add_total(floats.len());
    let scans = par_map(ctx, &floats, |p| p.clone(), |path| crate::encode::scan_float(path, &ctx.cancel));
    let found: Vec<(PathBuf, crate::encode::FloatScan)> = floats.iter().zip(scans).filter_map(|(p, s)| s.map(|s| (p.clone(), s))).collect();
    let count = found.len();
    if !ctx.cancelled() {
        let _ = ctx.tx.send(Msg::FloatScan(found));
    }
    format!("{count} float file(s) read")
}

pub fn op_convert(ctx: &Ctx, files: &[PathBuf], target: Target, level: u8, round_float: bool) -> String {
    let todo: Vec<PathBuf> = files.iter().filter(|f| Format::from_path(f) != Some(target.format())).cloned().collect();
    for f in files.iter().filter(|f| Format::from_path(f) == Some(target.format())) {
        ctx.status(f, format!("Already in {}", target.name()), Level::Info, "");
    }
    if todo.is_empty() {
        let text = format!("Nothing to convert: the {} selected file(s) are already in {}.", files.len(), target.name());
        ctx.log(&text, Level::Warn);
        return text;
    }
    if todo.len() < files.len() {
        ctx.log(format!("{} file(s) already in {}, skipped", files.len() - todo.len(), target.name()), Level::Info);
    }
    if target == Target::Flac {
        ctx.log(format!("FLAC compression level {level}"), Level::Info);
    }
    ctx.add_total(todo.len());
    // Per file: None = skipped, Some((level, rounded from float)).
    let results = par_map(ctx, &todo, |p| p.clone(), |path| {
        let out = path.with_extension(match target {
            Target::Flac => "flac",
            Target::Wav => "wav",
        });
        if out.exists() {
            ctx.status(path, format!("Skipped: {} already exists", file_name(&out)), Level::Warn, "");
            return Ok(None);
        }
        let info = audio::probe(path)?;
        if target == Target::Flac && info.lossy() {
            return Err(format!("{} is lossy: a FLAC made from it would be a fake lossless file", info.name()));
        }
        let float = target == Target::Flac && info.float;
        if float {
            let done = crate::encode::float_to_flac(path, &out, level, &audio::source_tags(path), round_float, &ctx.cancel)?;
            ctx.rescan.store(true, Ordering::SeqCst);
            let (text, level) = if done.exact {
                (format!("Converted to {} (verified; the float held {}-bit values: bit-exact)", file_name(&out), done.bits), Level::Ok)
            } else {
                let diff = done.max_diff_db.map(|d| format!(", largest difference {d:.1} dBFS")).unwrap_or_default();
                let mut text = format!("Converted to {} (verified; float rounded to {}-bit{diff})", file_name(&out), done.bits);
                if done.clipped > 0 {
                    text.push_str(&format!("; {} sample(s) above 0 dBFS clipped", done.clipped));
                }
                (text, if done.clipped > 0 { Level::Warn } else { Level::Ok })
            };
            ctx.status(path, text, level, "");
            return Ok(Some((level, !done.exact)));
        }
        match target {
            Target::Flac => crate::encode::to_flac(path, &out, level, &audio::source_tags(path), &ctx.cancel)?,
            Target::Wav => crate::encode::to_wav(path, &out, &ctx.cancel)?,
        };
        ctx.rescan.store(true, Ordering::SeqCst);
        ctx.status(path, format!("Converted to {} (verified)", file_name(&out)), Level::Ok, "");
        Ok(Some((Level::Ok, false)))
    });
    // The originals of rounded float files are not offered for the trash: they hold more.
    let sources: Vec<PathBuf> = todo.iter().zip(&results).filter(|(_, r)| matches!(r, Some(Some((_, false))))).map(|(p, _)| p.clone()).collect();
    let rounded = results.iter().filter(|r| matches!(r, Some(Some((_, true))))).count();
    let failed = todo.len() - sources.len() - rounded;
    if rounded > 0 {
        ctx.log(format!("{rounded} float file(s) rounded to 24-bit: their originals are kept"), Level::Info);
    }
    if !ctx.cancelled() {
        let _ = ctx.tx.send(Msg::Converted { target, sources: sources.clone(), failed, rounded });
    }
    let levels: Vec<Option<Level>> = results.iter().map(|r| r.map(|done| done.map(|(level, _)| level).unwrap_or(Level::Warn))).collect();
    summarize(&levels, &format!("file(s) converted to {}", target.name()))
}

/// Files copied next to the MP3 copies (info file, artwork).
const COMPANION_EXT: [&str; 7] = ["txt", "nfo", "jpg", "jpeg", "png", "gif", "pdf"];

/// MP3 copy of each show in "<show> [MP3 320]", next to the show or in the chosen folder,
/// with its text files and pictures. The originals are not touched.
pub fn op_mp3(ctx: &Ctx, shows: &[Show], files: &[PathBuf], mode: crate::encode::Mp3Mode) -> String {
    let tag = mode.tag();
    let base = ctx.opt.mp3_dir.trim();
    let selected: HashSet<&PathBuf> = files.iter().collect();
    let mut out_roots: HashMap<PathBuf, PathBuf> = HashMap::new();
    let mut jobs: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
    let mut taken: HashMap<PathBuf, PathBuf> = HashMap::new();
    // Per show: files skipped because another file already gives the same MP3.
    let mut duplicates: HashMap<PathBuf, usize> = HashMap::new();
    for show in shows {
        let parent = if base.is_empty() { show.root.parent().map(Path::to_path_buf).unwrap_or_default() } else { PathBuf::from(base) };
        let out_root = parent.join(format!("{} [{tag}]", file_name(&show.root)));
        for f in show.files.iter().filter(|f| selected.contains(f)) {
            let lossy = Format::from_path(f).is_some_and(|fmt| fmt.lossy() || (fmt.container() && audio::probe(f).is_ok_and(|i| i.lossy())));
            if lossy {
                ctx.status(f, "Already lossy: no MP3 copy made", Level::Info, "");
                continue;
            }
            let out = out_root.join(f.strip_prefix(&show.root).unwrap_or(f)).with_extension("mp3");
            // "01.flac" and "01.wav" side by side would both make "01.mp3": the first one wins.
            if let Some(first) = taken.get(&out) {
                ctx.status(f, format!("Skipped: {} already gives {}", file_name(first), file_name(&out)), Level::Warn, "");
                *duplicates.entry(show.root.clone()).or_default() += 1;
                continue;
            }
            taken.insert(out.clone(), f.clone());
            jobs.push((show.root.clone(), f.clone(), out));
        }
        out_roots.insert(show.root.clone(), out_root);
    }
    ctx.add_total(jobs.len());
    let results = par_map(ctx, &jobs, |(_, p, _)| p.clone(), |(root, path, out)| {
        let out_root = &out_roots[root];
        if out.exists() {
            ctx.status(path, format!("Skipped: {} already exists in {}", file_name(&out), file_name(out_root)), Level::Warn, "");
            return Ok(false);
        }
        crate::encode::check_mp3(&audio::probe(path)?)?;
        if let Some(dir) = out.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        crate::encode::to_mp3(path, out, mode, &audio::source_tags(path), &ctx.cancel)?;
        ctx.status(path, format!("{tag} copy written to {}", file_name(out_root)), Level::Ok, "");
        Ok(true)
    });
    // Tally per show: encoded now, already there, failed.
    let mut tally: HashMap<&PathBuf, (usize, usize, usize)> = HashMap::new();
    for ((root, _, _), r) in jobs.iter().zip(&results) {
        let t = tally.entry(root).or_default();
        match r {
            Some(true) => t.0 += 1,
            Some(false) => t.1 += 1,
            None => t.2 += 1,
        }
    }
    let (mut encoded, mut existing, mut same_name, mut failed) = (0, 0, 0, 0);
    for show in shows {
        let (new, old, bad) = tally.get(&show.root).copied().unwrap_or_default();
        let dup = duplicates.get(&show.root).copied().unwrap_or(0);
        encoded += new;
        existing += old;
        same_name += dup;
        failed += bad;
        let out_root = &out_roots[&show.root];
        if !ctx.cancelled() && out_root.exists() {
            for extra in files::all_files(&show.root, true) {
                let rel = extra.strip_prefix(&show.root).unwrap_or(&extra);
                let in_sbe_fixed = rel.components().any(|c| is_output_dir(&c.as_os_str().to_string_lossy()));
                if in_sbe_fixed || !COMPANION_EXT.contains(&ext_lower(&extra).as_str()) {
                    continue;
                }
                let dest = out_root.join(rel);
                if !dest.exists() {
                    let copied = dest.parent().map_or(Ok(()), fs::create_dir_all).and_then(|_| fs::copy(&extra, &dest));
                    if let Err(e) = copied {
                        ctx.log(format!("Cannot copy {}: {e}", rel_posix(&extra, &show.root)), Level::Warn);
                    }
                }
            }
        }
        if new + old + bad + dup == 0 {
            continue;
        }
        let mut parts = Vec::new();
        if new > 0 { parts.push(format!("{new} encoded")); }
        if old > 0 { parts.push(format!("{old} skipped (MP3 already there)")); }
        if dup > 0 { parts.push(format!("{dup} skipped (same name as another file)")); }
        if bad > 0 { parts.push(format!("{bad} failed")); }
        let level = if bad > 0 { Level::Error } else if new == 0 { Level::Warn } else { Level::Ok };
        let place = if out_root.exists() { format!(" -> {}", out_root.display()) } else { ", nothing written".to_string() };
        let text = format!("{tag} copy of {}: {}{place}", file_name(&show.root), parts.join(", "));
        ctx.status(&show.root, parts.join(", "), level, out_root.display().to_string());
        ctx.log(text, level);
    }
    let total = encoded + existing + same_name + failed;
    let mut summary = format!("{total} file(s): {encoded} encoded to {tag}");
    if existing > 0 { summary.push_str(&format!(", {existing} skipped (MP3 already there)")); }
    if same_name > 0 { summary.push_str(&format!(", {same_name} skipped (same name as another file)")); }
    if failed > 0 { summary.push_str(&format!(", {failed} error(s)")); }
    summary
}

/// Folders Bootleg Toolbox writes corrected copies into, inside a show: left out of
/// torrents, MP3 copies and the info file search.
const OUTPUT_DIRS: [&str; 2] = ["SBE-fixed", "WAV-cleaned"];

fn is_output_dir(name: &str) -> bool {
    OUTPUT_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d))
}

/// What is wrong with a WAV file's structure, besides its sound; Err when the sound itself
/// is incomplete (nothing to clean then).
fn wav_issues(path: &Path) -> Result<Vec<String>, String> {
    let l = audio::wav_layout(path)?;
    if let Some(declared) = l.data_declared {
        if declared > l.data_available {
            return Err(format!("truncated: the header announces {declared} bytes of sound, only {} are there", l.data_available));
        }
    }
    let mut issues = Vec::new();
    let extra: Vec<String> = l.chunks.iter().filter(|(id, _)| id != "fmt " && id != "data").map(|(id, n)| format!("{} ({n} bytes)", id.trim())).collect();
    if !extra.is_empty() {
        issues.push(format!("extra chunks: {}", extra.join(", ")));
    }
    if l.format_tag == 0xFFFE && l.bits <= 16 && l.channels <= 2 {
        issues.push("extensible header on 16-bit stereo or mono audio".into());
    }
    if l.format_tag == 1 && l.fmt_len != 16 {
        issues.push(format!("fmt chunk of {} bytes instead of 16", l.fmt_len));
    }
    if !l.riff_size_ok {
        issues.push("RIFF size does not match the file".into());
    }
    if l.data_declared.is_none() {
        issues.push("sound size never filled in".into());
    }
    if l.trailing > 0 {
        issues.push(format!("{} stray byte(s) at the end", l.trailing));
    }
    Ok(issues)
}

/// "Clean WAV headers": a copy with only the format and the sound, in the canonical
/// layout, in "<show>/WAV-cleaned" (verified: same sound). Float WAVs are left alone.
pub fn op_clean_wav(ctx: &Ctx, shows: &[Show], files: &[PathBuf]) -> String {
    let jobs: Vec<(PathBuf, PathBuf)> = shows
        .iter()
        .flat_map(|show| {
            show.files
                .iter()
                .filter(|f| files.contains(f) && Format::from_path(f) == Some(Format::Wav))
                .map(|f| (show.root.clone(), f.clone()))
        })
        .collect();
    if jobs.is_empty() {
        let text = "No WAV file in the selection.".to_string();
        ctx.log(&text, Level::Warn);
        return text;
    }
    ctx.add_total(jobs.len());
    let results = par_map(ctx, &jobs, |(_, p)| p.clone(), |(root, path)| {
        if audio::probe(path)?.float {
            ctx.status(path, "Float WAV: left as it is", Level::Info, "");
            return Ok(Level::Ok);
        }
        let issues = wav_issues(path)?;
        if issues.is_empty() {
            ctx.status(path, "Clean: only the format and the sound, canonical header", Level::Ok, "");
            return Ok(Level::Ok);
        }
        let out = root.join("WAV-cleaned").join(path.strip_prefix(root).unwrap_or(path));
        if out.exists() {
            ctx.status(path, format!("Skipped: {} already exists", rel_posix(&out, root)), Level::Warn, issues.join("\n"));
            return Ok(Level::Warn);
        }
        if let Some(dir) = out.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        crate::encode::to_wav(path, &out, &ctx.cancel)?;
        ctx.rescan.store(true, Ordering::SeqCst);
        let text = format!("Cleaned copy (same sound): {}; fixed: {}", rel_posix(&out, root), issues.join("; "));
        ctx.log(format!("{}: {}", file_name(path), issues.join("; ")), Level::Info);
        ctx.status(path, text, Level::Ok, "");
        Ok(Level::Ok)
    });
    summarize(&results, "WAV file(s) checked")
}

/// SBE fix, disc by disc (CD1, CD2... folders), as `shntool fix`: corrected copies of the
/// tracks that change, in FLAC, in "<show>/SBE-fixed". The originals are not touched.
pub fn op_fix_sbe(ctx: &Ctx, shows: &[Show], files: &[PathBuf]) -> String {
    ctx.add_total(files.len());
    let infos = par_map(ctx, files, |p| p.clone(), |path| {
        let info = audio::probe(path)?;
        ctx.info(path, &info);
        Ok(info)
    });
    let infos: HashMap<&PathBuf, AudioInfo> = files.iter().zip(infos).filter_map(|(f, i)| i.map(|i| (f, i))).collect();
    if ctx.cancelled() {
        return String::new();
    }
    struct Job<'a> {
        root: &'a Path,
        disc: Arc<Vec<(PathBuf, u64)>>,
        cut: crate::sbe::Cut,
        path: PathBuf,
        out: PathBuf,
    }
    let mut jobs: Vec<Job> = Vec::new();
    let mut nothing: Vec<&Show> = Vec::new();
    let mut failures = 0;
    for show in shows {
        let show_files: Vec<&PathBuf> = show.files.iter().filter(|f| infos.contains_key(f)).collect();
        let not_cd: Vec<&&PathBuf> = show_files.iter().filter(|f| !infos[**f].is_cd()).collect();
        for f in &not_cd {
            ctx.status(f, "Not CD audio (16-bit/44.1 kHz stereo): SBEs do not apply", Level::Info, "");
        }
        if !not_cd.is_empty() {
            ctx.log(format!("{}: {} file(s) ignored, as SBEs only concern CD audio (16-bit/44.1 kHz stereo)", file_name(&show.root), not_cd.len()), Level::Info);
        }
        // Discs: the CD audio files of each folder, in track order.
        let mut discs: Vec<(PathBuf, Vec<(PathBuf, u64)>)> = Vec::new();
        for f in show_files.iter().filter(|f| infos[**f].is_cd()) {
            let dir = f.parent().map(Path::to_path_buf).unwrap_or_default();
            let entry = (*f).clone();
            match discs.iter_mut().find(|(d, _)| *d == dir) {
                Some((_, tracks)) => tracks.push((entry, infos[*f].samples)),
                None => discs.push((dir, vec![(entry, infos[*f].samples)])),
            }
        }
        let before = jobs.len();
        let failed_before = failures;
        for (_, tracks) in discs {
            let lengths: Vec<u64> = tracks.iter().map(|(_, n)| *n).collect();
            if !crate::sbe::needs_fix(&lengths) {
                continue;
            }
            let cuts = crate::sbe::plan(&lengths);
            // A track shorter than a sector can end up empty: the disc is left alone.
            if let Some(i) = cuts.iter().position(|c| c.is_some_and(|c| c.end <= c.start)) {
                let msg = "Too short: no audio left once the boundaries are moved to whole sectors";
                ctx.status(&tracks[i].0, msg, Level::Error, "");
                ctx.log(format!("{}: disc not fixed, {} is shorter than a CD sector (1/75 s)", file_name(&show.root), rel_posix(&tracks[i].0, &show.root)), Level::Error);
                failures += 1;
                continue;
            }
            let disc = Arc::new(tracks);
            for (i, cut) in cuts.into_iter().enumerate() {
                let path = disc[i].0.clone();
                let Some(cut) = cut else {
                    ctx.status(&path, "Already on a sector boundary: unchanged", Level::Info, "");
                    continue;
                };
                let out = show.root.join("SBE-fixed").join(path.strip_prefix(&show.root).unwrap_or(&path)).with_extension("flac");
                jobs.push(Job { root: &show.root, disc: disc.clone(), cut, path, out });
            }
        }
        if jobs.len() == before && failures == failed_before {
            nothing.push(show);
        }
    }
    for show in &nothing {
        ctx.status(&show.root, "Nothing to fix", Level::Ok, "No SBE, or only on the last track of a disc, which is normal.");
        ctx.log(format!("{}: nothing to fix (no SBE, or only on the last track of a disc)", file_name(&show.root)), Level::Ok);
    }
    ctx.add_total(jobs.len());
    let results = par_map(ctx, &jobs, |j| j.path.clone(), |job| {
        if let Some(dir) = job.out.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        crate::sbe::write_track(&job.disc, job.cut, &job.out, 8, &audio::source_tags(&job.path), &ctx.cancel)?;
        ctx.rescan.store(true, Ordering::SeqCst);
        ctx.status(&job.path, format!("SBE fixed copy: {}", rel_posix(&job.out, job.root)), Level::Ok, "");
        Ok(())
    });
    let mut fixed = 0;
    for show in shows {
        let mine: Vec<&Option<()>> = jobs.iter().zip(&results).filter(|(j, _)| j.root == show.root).map(|(_, r)| r).collect();
        if mine.is_empty() || ctx.cancelled() {
            continue;
        }
        let ok = mine.iter().filter(|r| r.is_some()).count();
        let out_dir = show.root.join("SBE-fixed");
        if ok == mine.len() {
            fixed += 1;
            ctx.status(&show.root, format!("SBEs fixed: {ok} track(s) in SBE-fixed"), Level::Ok, out_dir.display().to_string());
            ctx.log(format!("{}: {ok} corrected track(s) written to {}. Check them, replace the originals, then create new checksum files.", file_name(&show.root), out_dir.display()), Level::Ok);
        } else {
            let bad = mine.len() - ok;
            ctx.status(&show.root, format!("SBE fix incomplete: {bad} track(s) failed"), Level::Error, "");
            ctx.log(format!("{}: SBE fix incomplete, {bad} track(s) failed; the tracks in {} do not form a whole disc.", file_name(&show.root), out_dir.display()), Level::Error);
        }
    }
    let failed = results.iter().filter(|r| r.is_none()).count() + failures;
    let written = results.iter().filter(|r| r.is_some()).count();
    let mut summary = format!("SBE fix finished: {fixed} show(s) corrected ({written} track(s) written)");
    if failed > 0 {
        summary.push_str(&format!(", {failed} error(s)"));
    }
    if !nothing.is_empty() {
        summary.push_str(&format!(", {} show(s) with nothing to fix", nothing.len()));
    }
    summary
}

/// Checksum files under `roots` that list files with one of `extensions` (".shn"): after a
/// conversion they no longer match the files.
pub fn old_checksum_files(roots: &[PathBuf], extensions: &HashSet<String>) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in roots {
        for cs in files::all_files(root, true).into_iter().filter(|p| files::is_checksum_file(p)) {
            let Ok((_, entries)) = files::parse_checksum_file(&cs) else { continue };
            let lists_old = entries.iter().any(|(name, _)| {
                let name = name.replace('\\', "/");
                Path::new(&name).extension().is_some_and(|e| extensions.contains(&format!(".{}", e.to_string_lossy().to_lowercase())))
            });
            if lists_old {
                found.push(cs);
            }
        }
    }
    found
}

pub fn op_after_flac(ctx: &Ctx, roots: &[PathBuf], sources: &[PathBuf], trash_sources: bool, trash_checksums: &[PathBuf], create_ffp: bool) -> String {
    ctx.add_total(if trash_sources { sources.len() } else { 0 } + trash_checksums.len());
    if trash_sources {
        let mut trashed = 0;
        for path in sources {
            if ctx.cancelled() {
                break;
            }
            if path.exists() && move_to_trash(path) {
                trashed += 1;
            } else {
                ctx.log(format!("Could not move {} to the trash", file_name(path)), Level::Warn);
            }
            ctx.tick();
        }
        ctx.log(format!("{trashed} original file(s) moved to the trash"), Level::Ok);
        ctx.rescan.store(true, Ordering::SeqCst);
    }
    for cs in trash_checksums {
        if move_to_trash(cs) {
            ctx.log(format!("Old checksum file moved to the trash: {}", file_name(cs)), Level::Ok);
        } else {
            ctx.log(format!("Could not move {} to the trash", file_name(cs)), Level::Warn);
        }
        ctx.tick();
    }
    if create_ffp && !ctx.cancelled() {
        let shows: Vec<Show> = files::scan_paths(roots, &HashSet::new()).into_iter().filter(|s| roots.contains(&s.root)).collect();
        create_checksums(ctx, &shows, &["ffp"]);
        ctx.rescan.store(true, Ordering::SeqCst);
    }
    "FLAC conversion finished.".into()
}

// ---------------------------------------------------------------- Speed?

pub fn op_speed(ctx: &Ctx, shows: &[Show], files: &[PathBuf]) -> String {
    ctx.add_total(files.len());
    let send = |path: &Path, v: &crate::speed::SpeedVerdict| {
        let _ = ctx.tx.send(Msg::Speed {
            path: path.to_path_buf(),
            text: v.label.clone(),
            level: v.level,
            tooltip: format!("{}\n\nClick to see the full analysis.", v.sentence),
            report: Arc::new(v.report.clone()),
        });
    };
    let results = par_map(ctx, files, |p| p.clone(), |path| {
        let info = audio::probe(path)?;
        ctx.info(path, &info);
        let facts = crate::speed::analyse(path, &info, &ctx.cancel)?;
        let verdict = crate::speed::track_verdict(file_name(path), &facts);
        send(path, &verdict);
        Ok((facts, verdict))
    });
    let by_path: HashMap<&PathBuf, &(crate::speed::SpeedFacts, crate::speed::SpeedVerdict)> =
        files.iter().zip(results.iter()).filter_map(|(f, r)| r.as_ref().map(|r| (f, r))).collect();
    let mut summary_levels = Vec::new();
    for show in shows {
        let tracks: Vec<(String, crate::speed::SpeedFacts, String, Level, String)> = show
            .files
            .iter()
            .filter_map(|f| by_path.get(f).map(|(facts, v)| (rel_posix(f, &show.root), facts.clone(), v.label.clone(), v.level, v.report.key_facts.clone())))
            .collect();
        if tracks.is_empty() {
            continue;
        }
        let verdict = crate::speed::show_verdict(file_name(&show.root), &tracks);
        ctx.log(format!("{}: speed {}", file_name(&show.root), verdict.sentence.to_lowercase()), verdict.level);
        summary_levels.push(Some(verdict.level));
        send(&show.root, &verdict);
    }
    let levels: Vec<Option<Level>> = if summary_levels.is_empty() {
        results.iter().map(|r| r.as_ref().map(|(_, v)| v.level)).collect()
    } else {
        summary_levels
    };
    let levels: Vec<Option<Level>> = levels.into_iter().map(|l| l.map(|l| if l == Level::Info { Level::Warn } else { l })).collect();
    summarize(&levels, "show(s) checked for tape speed")
}

// ---------------------------------------------------------------- Spectrogram

const SPECTROGRAM_LIMIT: usize = 4;

pub fn op_spectrogram(ctx: &Ctx, files: &[PathBuf]) -> String {
    let targets = &files[..files.len().min(SPECTROGRAM_LIMIT)];
    if files.len() > SPECTROGRAM_LIMIT {
        ctx.log(format!("Spectrograms are limited to the first {SPECTROGRAM_LIMIT} selected files."), Level::Warn);
    }
    ctx.add_total(targets.len());
    let results = par_map(ctx, targets, |p| p.clone(), |path| {
        let image = crate::spectrogram::render(path, &ctx.cancel)?;
        let _ = ctx.tx.send(Msg::Image { path: path.clone(), image });
        Ok(())
    });
    let done = results.iter().filter(|r| r.is_some()).count();
    if done == 1 { "Spectrogram ready.".into() } else { format!("{done} spectrograms ready.") }
}

// ---------------------------------------------------------------- Test

pub fn op_test(ctx: &Ctx, files: &[PathBuf]) -> String {
    ctx.add_total(files.len());
    let results = par_map(ctx, files, |p| p.clone(), |path| {
        let format = Format::from_path(path).ok_or("not an audio file")?;
        if !format.supported() {
            ctx.status(path, "Not tested: format not supported yet", Level::Warn, "");
            return Ok(Level::Warn);
        }
        let info = audio::probe(path)?;
        let (md5, frames) = match audio::pcm_md5(path, &ctx.cancel) {
            Ok(v) => v,
            Err(e) => {
                ctx.status(path, format!("Test FAILED: {e}"), Level::Error, "");
                ctx.log(format!("Test failed: {}: {e}", path.display()), Level::Error);
                return Ok(Level::Error);
            }
        };
        if info.samples > 0 && !audio::length_ok(&info, frames) {
            let text = format!("Test FAILED: damaged or truncated audio, only {frames} of {} samples decode", info.samples);
            ctx.status(path, &text, Level::Error, "");
            ctx.log(format!("{}: {text}", path.display()), Level::Error);
            return Ok(Level::Error);
        }
        match info.md5 {
            Some(stored) if stored == ZERO_MD5 => {
                ctx.status(path, "Test OK: decodes cleanly (no MD5 signature to compare with)", Level::Warn, "");
                Ok(Level::Warn)
            }
            Some(stored) if stored != md5 => {
                ctx.status(path, "Test FAILED: decoded audio does not match the MD5 signature", Level::Error, "");
                ctx.log(format!("Test failed: {}: MD5 signature mismatch", path.display()), Level::Error);
                Ok(Level::Error)
            }
            Some(_) => {
                ctx.status(path, "Test OK: decodes cleanly and matches its MD5 signature", Level::Ok, "");
                Ok(Level::Ok)
            }
            None => {
                ctx.status(path, "Test OK: audio data complete and readable", Level::Ok, "");
                Ok(Level::Ok)
            }
        }
    });
    summarize(&results, "file(s) tested")
}

// ---------------------------------------------------------------- Torrent

enum Bencode {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Bencode>),
    Dict(BTreeMap<Vec<u8>, Bencode>),
}

impl Bencode {
    fn text(s: &str) -> Bencode {
        Bencode::Bytes(s.as_bytes().to_vec())
    }
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Bencode::Int(i) => out.extend_from_slice(format!("i{i}e").as_bytes()),
            Bencode::Bytes(b) => {
                out.extend_from_slice(format!("{}:", b.len()).as_bytes());
                out.extend_from_slice(b);
            }
            Bencode::List(items) => {
                out.push(b'l');
                items.iter().for_each(|i| i.encode(out));
                out.push(b'e');
            }
            Bencode::Dict(map) => {
                out.push(b'd');
                for (k, v) in map {
                    Bencode::Bytes(k.clone()).encode(out);
                    v.encode(out);
                }
                out.push(b'e');
            }
        }
    }
}

fn dict(pairs: Vec<(&str, Bencode)>) -> Bencode {
    Bencode::Dict(pairs.into_iter().map(|(k, v)| (k.as_bytes().to_vec(), v)).collect())
}

const TORRENT_SKIP_NAMES: [&str; 3] = ["thumbs.db", "desktop.ini", "ehthumbs.db"];
const TORRENT_SKIP_EXT: [&str; 2] = ["torrent", "part"];

fn torrent_files(root: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut keep = Vec::new();
    let mut skipped = Vec::new();
    for path in files::all_files(root, false) {
        let rel = path.strip_prefix(root).unwrap_or(&path);
        let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
        let name = parts.last().cloned().unwrap_or_default().to_lowercase();
        let hidden_or_skipped_dir = parts[..parts.len().saturating_sub(1)].iter()
            .any(|p| p.starts_with('.') || is_output_dir(p));
        if hidden_or_skipped_dir || name.starts_with('.') || TORRENT_SKIP_NAMES.contains(&name.as_str())
            || TORRENT_SKIP_EXT.contains(&ext_lower(&path).as_str())
            || fs::symlink_metadata(&path).map(|m| m.file_type().is_symlink()).unwrap_or(false)
        {
            skipped.push(path);
        } else {
            keep.push(path);
        }
    }
    keep.sort_by(|a, b| {
        let ka: Vec<String> = a.strip_prefix(root).unwrap_or(a).components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect();
        let kb: Vec<String> = b.strip_prefix(root).unwrap_or(b).components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect();
        ka.cmp(&kb)
    });
    (keep, skipped)
}

fn auto_piece_length(total: u64) -> u64 {
    let mut length = 1u64 << 15;
    while length < (1 << 24) && total / length > 1500 {
        length <<= 1;
    }
    length
}

fn hash_pieces(ctx: &Ctx, paths: &[PathBuf], piece_length: u64) -> Result<Vec<u8>, String> {
    use sha1::Digest;
    let mut pieces = Vec::new();
    let mut buffer: Vec<u8> = Vec::with_capacity(piece_length as usize);
    let mut chunk = vec![0u8; 1 << 20];
    for path in paths {
        let mut f = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        loop {
            if ctx.cancelled() {
                return Err("cancelled".into());
            }
            let want = (piece_length as usize - buffer.len()).min(chunk.len());
            let n = f.read(&mut chunk[..want]).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..n]);
            if buffer.len() as u64 == piece_length {
                pieces.extend_from_slice(&Sha1::digest(&buffer));
                buffer.clear();
            }
        }
        ctx.tick();
    }
    if !buffer.is_empty() {
        pieces.extend_from_slice(&Sha1::digest(&buffer));
    }
    Ok(pieces)
}

/// One summary per show (its selected files) and per DVD.
fn op_media_info(ctx: &Ctx, shows: &[Show], files: &[PathBuf], dvds: &[PathBuf]) -> String {
    ctx.add_total(files.len() + dvds.len());
    let mut done = 0;
    for show in shows {
        let mine: Vec<PathBuf> = show.files.iter().filter(|f| files.contains(f)).cloned().collect();
        if mine.is_empty() {
            continue;
        }
        let read: Vec<Result<crate::mediainfo::FileInfo, String>> = mine
            .par_iter()
            .map(|f| {
                let r = crate::mediainfo::read_file(f);
                ctx.tick();
                r
            })
            .collect();
        if ctx.cancelled() {
            return String::new();
        }
        let mut ok = Vec::new();
        let mut failed = Vec::new();
        for (path, r) in mine.into_iter().zip(read) {
            match r {
                Ok(info) => ok.push(info),
                Err(e) => failed.push((path, e)),
            }
        }
        let text = crate::mediainfo::audio_text(&show.root, &ok, &failed);
        let _ = ctx.tx.send(Msg::MediaInfo { title: file_name(&show.root), text });
        done += 1;
    }
    for root in dvds {
        if ctx.cancelled() {
            return String::new();
        }
        match crate::dvd::read(root) {
            Ok(dvd) => {
                let text = crate::mediainfo::dvd_text(&dvd);
                let _ = ctx.tx.send(Msg::MediaInfo { title: dvd.name.clone(), text });
                done += 1;
            }
            Err(e) => ctx.log(format!("{}: {e}", file_name(root)), Level::Error),
        }
        ctx.tick();
    }
    format!("Media info: {}.", match done {
        0 => "nothing to show".to_string(),
        1 => "1 summary".to_string(),
        n => format!("{n} summaries"),
    })
}

fn human_size(bytes: u64) -> String {
    let (value, unit) = match bytes {
        b if b >= 1 << 30 => (b as f64 / (1u64 << 30) as f64, "GB"),
        b if b >= 1 << 20 => (b as f64 / (1u64 << 20) as f64, "MB"),
        b => (b as f64 / 1024.0, "KB"),
    };
    format!("{value:.1} {unit}")
}

/// The folder a torrent's files are in: a show of the list named like the torrent, a
/// folder of that name next to the .torrent, or else the only show of the selection.
fn torrent_root(t: &crate::torrent_check::Torrent, torrent: &Path, shows: &[Show]) -> Result<(PathBuf, Option<String>), String> {
    let beside = torrent.parent().map(Path::to_path_buf).unwrap_or_default();
    if t.single_file {
        let mut dirs: Vec<PathBuf> = shows.iter().map(|s| s.root.clone()).collect();
        dirs.push(beside);
        return dirs
            .into_iter()
            .find(|d| d.join(&t.name).is_file())
            .map(|d| (d, None))
            .ok_or_else(|| format!("{} not found next to the torrent or in the shows of the list", t.name));
    }
    if let Some(show) = shows.iter().find(|s| file_name(&s.root) == t.name) {
        return Ok((show.root.clone(), None));
    }
    if beside.join(&t.name).is_dir() {
        return Ok((beside.join(&t.name), None));
    }
    match shows {
        [only] => Ok((only.root.clone(), Some(format!("the torrent's folder is named \"{}\": checked against {} instead", t.name, file_name(&only.root))))),
        [] => Err(format!("no folder \"{}\" next to the torrent: add the show to the list, then check again", t.name)),
        _ => Err(format!("no show named \"{}\" in the list: select the show to check, then check again", t.name)),
    }
}

/// Checks shows against .torrent files: every piece hashed again, differences traced to files.
pub fn op_torrent_check(ctx: &Ctx, shows: &[Show], torrents: &[PathBuf]) -> String {
    use crate::torrent_check as tc;
    let (mut matching, mut differing, mut failed) = (0, 0, 0);
    for torrent in torrents {
        if ctx.cancelled() {
            break;
        }
        let tname = file_name(torrent);
        let t = match tc::read(torrent) {
            Ok(t) => t,
            Err(e) => {
                ctx.log(format!("{tname}: {e}"), Level::Error);
                failed += 1;
                continue;
            }
        };
        let mut about = format!(
            "{tname}: \"{}\", {} file(s), {}, pieces of {}, {}",
            t.name,
            t.files.iter().filter(|(p, _)| !p.is_empty()).count(),
            human_size(t.total()),
            human_size(t.piece_length),
            if t.private { "private" } else { "public" }
        );
        if let Some(source) = &t.source {
            about.push_str(&format!(", source {source}"));
        }
        if let Some(tracker) = &t.tracker {
            about.push_str(&format!(", tracker {tracker}"));
        }
        if let Some(by) = &t.created_by {
            about.push_str(&format!(", made with {by}"));
        }
        about.push_str(&format!(", info hash {}", t.info_hash));
        ctx.log(about, Level::Info);
        let (root, note) = match torrent_root(&t, torrent, shows) {
            Ok(found) => found,
            Err(e) => {
                ctx.log(format!("{tname}: {e}"), Level::Error);
                failed += 1;
                continue;
            }
        };
        if let Some(note) = note {
            ctx.log(format!("{tname}: {note}"), Level::Warn);
        }
        ctx.add_total(t.pieces.len());
        let extra = if t.single_file { Vec::new() } else { torrent_files(&root).0 };
        let check = match tc::check(&t, &root, extra, &ctx.cancel, &|n| ctx.advance(n)) {
            Ok(c) => c,
            Err(e) => {
                if !ctx.cancelled() {
                    ctx.log(format!("{tname}: {e}"), Level::Error);
                    failed += 1;
                }
                continue;
            }
        };
        let mut problems = 0;
        for f in check.files.iter().filter(|f| f.path.is_some()) {
            let path = f.path.as_ref().unwrap();
            let (text, level) = match f.found {
                None => (format!("Missing: listed in {tname}"), Level::Error),
                Some(size) if size != f.expected => (format!("Wrong size: {size} bytes, {tname} says {}", f.expected), Level::Error),
                _ if f.bad_pieces > 0 => (format!("Damaged: {} of its {} piece(s) differ from {tname}", f.bad_pieces, f.pieces), Level::Error),
                _ => (format!("Matches {tname}"), Level::Ok),
            };
            if level != Level::Ok {
                problems += 1;
                ctx.log(format!("{}: {text}", f.rel), level);
            }
            ctx.status(path, text, level, "");
        }
        for path in &check.extra {
            ctx.log(format!("{}: in the folder but not in {tname}", rel_posix(path, &root)), Level::Info);
        }
        let show_text = if problems == 0 {
            matching += 1;
            format!("Matches {tname} ({} pieces checked)", t.pieces.len())
        } else {
            differing += 1;
            format!("{problems} file(s) differ from {tname} ({} of {} pieces)", check.bad_pieces, t.pieces.len())
        };
        let level = if problems == 0 { Level::Ok } else { Level::Error };
        if shows.iter().any(|s| s.root == root) {
            ctx.status(&root, &show_text, level, "");
        }
        ctx.log(format!("{}: {show_text}", file_name(&root)), level);
    }
    let mut summary = format!("{} torrent(s) checked: {matching} match", torrents.len());
    if differing > 0 {
        summary.push_str(&format!(", {differing} differ"));
    }
    if failed > 0 {
        summary.push_str(&format!(", {failed} could not be checked"));
    }
    summary
}

pub fn op_torrent(ctx: &Ctx, shows: &[Show], trackers: &[Tracker]) -> String {
    let mut targets = Vec::new();
    for t in trackers {
        let (label, tag, announce) = (t.label(&ctx.opt), t.tag(&ctx.opt), t.announce(&ctx.opt));
        if tag.is_empty() {
            ctx.log(format!("A tracker has no name (see Settings): skipped{}", if announce.is_empty() { String::new() } else { format!(" ({announce})") }), Level::Error);
        } else if announce.is_empty() {
            ctx.log(format!("No announce URL set for {label} (see Settings)"), Level::Error);
        } else {
            targets.push((tag, announce));
        }
    }
    if targets.is_empty() {
        return "Nothing to do.".into();
    }
    let mut plan = Vec::new();
    for show in shows {
        let name = file_name(&show.root);
        let (paths, skipped) = torrent_files(&show.root);
        for p in &skipped {
            ctx.log(format!("{name}: left out of the torrent: {}", rel_posix(p, &show.root)), Level::Info);
        }
        if paths.is_empty() {
            ctx.log(format!("{name}: no file to put in a torrent"), Level::Error);
            continue;
        }
        if !paths.iter().any(|p| files::is_checksum_file(p)) {
            ctx.log(format!("{name}: no checksum file (.ffp/.st5/.md5) in the folder"), Level::Warn);
        }
        if !paths.iter().any(|p| ext_lower(p) == "txt") {
            ctx.log(format!("{name}: no info .txt file in the folder"), Level::Warn);
        }
        plan.push((show, paths));
    }
    ctx.add_total(plan.iter().map(|(_, p)| p.len()).sum());
    let results = par_map(ctx, &plan, |(s, _)| s.root.clone(), |(show, paths)| {
        let sizes: Vec<u64> = paths.iter().map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0)).collect();
        let piece_length = auto_piece_length(sizes.iter().sum());
        let pieces = hash_pieces(ctx, paths, piece_length)?;
        let out_dir = if ctx.opt.torrent_dir.trim().is_empty() {
            show.root.parent().unwrap_or(&show.root).to_path_buf()
        } else {
            PathBuf::from(ctx.opt.torrent_dir.trim())
        };
        fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
        let file_list: Vec<Bencode> = paths.iter().zip(sizes.iter()).map(|(p, size)| {
            let parts = p.strip_prefix(&show.root).unwrap_or(p).components()
                .map(|c| Bencode::text(&c.as_os_str().to_string_lossy())).collect();
            dict(vec![("length", Bencode::Int(*size as i64)), ("path", Bencode::List(parts))])
        }).collect();
        let name = file_name(&show.root);
        let mut made = Vec::new();
        for (tag, announce) in &targets {
            let mut info = vec![
                ("name", Bencode::text(&name)),
                ("piece length", Bencode::Int(piece_length as i64)),
                ("pieces", Bencode::Bytes(pieces.clone())),
                ("files", Bencode::List(file_list.iter().map(clone_bencode).collect())),
            ];
            if ctx.opt.torrent_private {
                info.push(("private", Bencode::Int(1)));
            }
            if ctx.opt.torrent_source {
                info.push(("source", Bencode::text(tag)));
            }
            let created = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let meta = dict(vec![
                ("announce", Bencode::text(announce)),
                ("created by", Bencode::text(&format!("{APP_NAME} {APP_VERSION}"))),
                ("creation date", Bencode::Int(created as i64)),
                ("encoding", Bencode::text("UTF-8")),
                ("info", dict(info)),
            ]);
            let mut bytes = Vec::new();
            meta.encode(&mut bytes);
            let out = out_dir.join(format!("{name} [{tag}].torrent"));
            fs::write(&out, bytes).map_err(|e| format!("{}: {e}", out.display()))?;
            ctx.log(format!("Created {}", out.display()), Level::Ok);
            made.push(tag.as_str());
        }
        ctx.status(&show.root, format!("Torrent created: {}", made.join(", ")), Level::Ok, "");
        ctx.tick();
        Ok(())
    });
    let done = results.iter().filter(|r| r.is_some()).count();
    if done > 0 {
        ctx.log("Next: upload each .torrent on its site, then download the torrent the site gives back (it holds your passkey) and seed with that one.", Level::Info);
    }
    format!("Torrents created for {done} show(s).")
}

fn clone_bencode(b: &Bencode) -> Bencode {
    match b {
        Bencode::Int(i) => Bencode::Int(*i),
        Bencode::Bytes(v) => Bencode::Bytes(v.clone()),
        Bencode::List(l) => Bencode::List(l.iter().map(clone_bencode).collect()),
        Bencode::Dict(d) => Bencode::Dict(d.iter().map(|(k, v)| (k.clone(), clone_bencode(v))).collect()),
    }
}

// ---------------------------------------------------------------- Tag from info file

/// Writes the tags, renames the files, then brings the checksum files up to date: new names
/// everywhere, new .md5/.sfv digests of the tagged files (the FFP and ST5, made from the
/// audio, do not change).
pub fn op_tag(ctx: &Ctx, jobs: &[crate::tagging::TagJob]) -> String {
    ctx.add_total(jobs.iter().map(|j| j.files.len()).sum());
    ctx.rescan.store(true, Ordering::SeqCst);
    let (mut tagged, mut renamed) = (0, 0);
    for job in jobs {
        if ctx.cancelled() {
            return "Stopped.".into();
        }
        let (t, r) = tag_show(ctx, job);
        tagged += t;
        renamed += r;
    }
    format!("{} tagged, {} renamed.", plural(tagged, "file", "files"), renamed)
}

fn tag_show(ctx: &Ctx, job: &crate::tagging::TagJob) -> (usize, usize) {
    use crate::tagging;
    let show = file_name(&job.root);
    // 1. Tags, FLAC only; the audio signature and length must be the same afterwards.
    let mut tagged = Vec::new();
    let mut untagged = Vec::new();
    for (path, fields, _) in &job.files {
        if ctx.cancelled() {
            return (tagged.len(), 0);
        }
        if !tagging::taggable(path) {
            untagged.push(file_name(path));
            continue;
        }
        let before = audio::probe(path).map(|i| (i.md5, i.samples));
        let result = tagging::write_flac_tags(path, fields).and_then(|()| {
            let after = audio::probe(path).map(|i| (i.md5, i.samples));
            if before.is_ok() && after != before {
                return Err("the audio signature changed after tagging (please report this)".into());
            }
            Ok(())
        });
        match result {
            Ok(()) => tagged.push(path.clone()),
            Err(e) => {
                ctx.status(path, format!("Not tagged: {e}"), Level::Error, "");
                ctx.log(format!("{}: not tagged: {e}", file_name(path)), Level::Error);
            }
        }
    }
    if !untagged.is_empty() {
        ctx.log(format!("{show}: {} not tagged (only FLAC files can be tagged): {}", plural(untagged.len(), "file", "files"), untagged.join(", ")), Level::Warn);
    }

    // 2. Whole-file digests of the tagged files, when a .md5 or .sfv lists them.
    let checksums = tagging::checksum_files(&job.root);
    let whole = checksums.iter().any(|p| matches!(ext_lower(p).as_str(), "md5" | "sfv"));
    let mut digests = HashMap::new();
    if job.update_checksums && whole && !tagged.is_empty() {
        let computed = par_map(ctx, &tagged, |p| p.clone(), |p| {
            Ok((hex(&audio::file_md5(p, &ctx.cancel)?), format!("{:08x}", audio::file_crc32(p, &ctx.cancel)?)))
        });
        for (p, d) in tagged.iter().zip(computed) {
            if let Some(d) = d {
                digests.insert(p.clone(), d);
            }
        }
        if ctx.cancelled() {
            return (tagged.len(), 0);
        }
    } else {
        ctx.advance(tagged.len());
    }

    // 3. New names: through temporary names, so that two files can swap their names.
    let moves: Vec<(PathBuf, PathBuf)> = job
        .files
        .iter()
        .filter_map(|(p, _, name)| name.as_ref().map(|n| (p.clone(), p.with_file_name(n))))
        .filter(|(old, new)| old != new)
        .collect();
    let mut renamed = HashMap::new();
    let mut parked = Vec::new();
    for (i, (old, new)) in moves.iter().enumerate() {
        let temp = old.with_file_name(format!(".bt-rename-{i}-{}", std::process::id()));
        match fs::rename(old, &temp) {
            Ok(()) => parked.push((old, temp, new)),
            Err(e) => ctx.log(format!("Cannot rename {}: {e}", file_name(old)), Level::Error),
        }
    }
    for (old, temp, new) in parked {
        if new.exists() {
            ctx.log(format!("Not renamed: {} already exists", file_name(new)), Level::Error);
            let _ = fs::rename(&temp, old);
            continue;
        }
        match fs::rename(&temp, new) {
            Ok(()) => {
                renamed.insert(old.clone(), new.clone());
            }
            Err(e) => {
                ctx.log(format!("Cannot rename {} to {}: {e}", file_name(old), file_name(new)), Level::Error);
                let _ = fs::rename(&temp, old);
            }
        }
    }

    // 4. Checksum files.
    let changes = tagging::Changes { renamed, digests };
    for sum in &checksums {
        match tagging::update_checksum_file(sum, &changes, &tagged) {
            Ok((0, 0)) => {}
            Ok((changed, stale)) => {
                if changed > 0 {
                    ctx.log(format!("Updated {}: {}", rel_posix(sum, &job.root), plural(changed, "line", "lines")), Level::Ok);
                }
                if stale > 0 {
                    ctx.log(
                        format!("{}: {} of tagged files no longer match (whole-file checksums change with the tags; Create checksums makes them again)", rel_posix(sum, &job.root), plural(stale, "line", "lines")),
                        Level::Warn,
                    );
                }
            }
            Err(e) => ctx.log(format!("Cannot update {}: {e}", rel_posix(sum, &job.root)), Level::Error),
        }
    }
    // A torrent made before no longer matches the files.
    if let Some(parent) = job.root.parent() {
        let old_torrents = fs::read_dir(parent).into_iter().flatten().flatten().any(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with(&show) && name.to_lowercase().ends_with(".torrent")
        });
        if old_torrents && (!tagged.is_empty() || !changes.renamed.is_empty()) {
            ctx.log(format!("{show}: the .torrent files made before no longer match the files; make them again before seeding."), Level::Warn);
        }
    }
    ctx.status(&job.root, format!("Tagged {}, renamed {}", tagged.len(), changes.renamed.len()), Level::Ok, "");
    ctx.log(format!("{show}: {} tagged, {} renamed.", plural(tagged.len(), "file", "files"), changes.renamed.len()), Level::Ok);
    (tagged.len(), changes.renamed.len())
}

/// Text of the .ffp files of a show, for the TTD upload form.
pub fn ffp_text(root: &Path) -> Option<String> {
    let texts: Vec<String> = files::all_files(root, true).into_iter()
        .filter(|p| ext_lower(p) == "ffp")
        .filter_map(|p| files::read_text(&p).ok())
        .map(|t| t.trim().to_string())
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// The FFP of a show built from the MD5 signatures in its FLAC headers (nothing is decoded),
/// as Create would write it, for a show without a .ffp file. Also returns the number of
/// FLAC files without a signature (their line is all zeros).
pub fn ffp_from_headers(show: &Show) -> Result<(String, usize, usize), String> {
    let flacs: Vec<&PathBuf> = show.files.iter().filter(|p| Format::from_path(p) == Some(Format::Flac)).collect();
    if flacs.is_empty() {
        return Err("no FLAC file in the show".into());
    }
    let mut lines = Vec::with_capacity(flacs.len());
    let mut unsigned = 0;
    for path in &flacs {
        let md5 = audio::probe(path).map_err(|e| format!("{}: {e}", file_name(path)))?.md5.unwrap_or(ZERO_MD5);
        if md5 == ZERO_MD5 {
            unsigned += 1;
        }
        lines.push(format!("{}:{}", rel_posix(path, &show.root), hex(&md5)));
    }
    Ok((lines.join("\n"), flacs.len(), unsigned))
}

/// Words in the name of text files that are not the info file of a show.
const NOT_INFO: [&str; 8] = ["ffp", "md5", "st5", "sfv", "checksum", "fingerprint", "audiochecker", "aucdtect"];

/// The text files that may be the info file of a show, the most likely first: those of
/// the show folder (or, when it has none, of its sub-folders), ranked by name (the name of
/// the folder, then "info" in the name), then by size.
pub fn info_candidates(root: &Path) -> Vec<PathBuf> {
    let is_candidate = |p: &PathBuf| {
        let name = file_name(p).to_lowercase();
        matches!(ext_lower(p).as_str(), "txt" | "nfo") && !NOT_INFO.iter().any(|w| name.contains(w))
    };
    let all: Vec<PathBuf> = files::all_files(root, true)
        .into_iter()
        .filter(|p| {
            let rel = p.strip_prefix(root).unwrap_or(p);
            !rel.components().any(|c| is_output_dir(&c.as_os_str().to_string_lossy()))
        })
        .filter(is_candidate)
        .collect();
    let top: Vec<PathBuf> = all.iter().filter(|p| p.parent() == Some(root)).cloned().collect();
    let mut found = if top.is_empty() { all } else { top };
    let folder = file_name(root).to_lowercase();
    let rank = |p: &PathBuf| {
        let stem = p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        let size = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let score = if stem == folder { 2 } else if stem.contains("info") { 1 } else { 0 };
        (std::cmp::Reverse(score), std::cmp::Reverse(size))
    };
    found.sort_by_key(rank);
    found
}

/// Sends a file to the system trash (Windows Recycle Bin or freedesktop trash).
pub fn move_to_trash(path: &Path) -> bool {
    trash::delete(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_file_ranking() {
        let dir = std::env::temp_dir().join(format!("bh-info-{}", std::process::id())).join("Band - 1990-01-01");
        fs::create_dir_all(dir.join("CD1")).unwrap();
        fs::write(dir.join("notes.txt"), "a long text, longer than the others").unwrap();
        fs::write(dir.join("Info.txt"), "info").unwrap();
        fs::write(dir.join("Band - 1990-01-01.txt"), "show").unwrap();
        fs::write(dir.join("Band - 1990-01-01 ffp.txt"), "x").unwrap();
        fs::write(dir.join("AudioChecker.txt"), "x").unwrap();
        fs::write(dir.join("CD1").join("cd1.txt"), "x").unwrap();
        let names: Vec<String> = info_candidates(&dir).iter().map(|p| file_name(p)).collect();
        assert_eq!(names, ["Band - 1990-01-01.txt", "Info.txt", "notes.txt"]);
        // Nothing in the show folder: the sub-folders are searched.
        for n in ["notes.txt", "Info.txt", "Band - 1990-01-01.txt"] {
            fs::remove_file(dir.join(n)).unwrap();
        }
        let names: Vec<String> = info_candidates(&dir).iter().map(|p| file_name(p)).collect();
        assert_eq!(names, ["cd1.txt"]);
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn bencode_sorted_keys() {
        let mut out = Vec::new();
        dict(vec![("b", Bencode::Int(1)), ("a", Bencode::text("x"))]).encode(&mut out);
        assert_eq!(out, b"d1:a1:x1:bi1ee");
    }

    #[test]
    fn piece_length() {
        assert_eq!(auto_piece_length(1000), 1 << 15);
        assert_eq!(auto_piece_length(500 << 20), 512 << 10);
    }
}
