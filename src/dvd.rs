//! DVD-Video technical summary, read-only: the IFO file of each title set (video standard,
//! aspect ratio, audio and subtitle languages, titles with their chapters and lengths) and
//! the first megabytes of its VOB files (MPEG video sequence header, AC-3, DTS, LPCM and
//! MPEG audio headers, bit rates). Loose VOB files without IFO are read the same way, their
//! length taken from the first and last video timestamps.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::files::{ext_lower, file_name, natural_path_cmp};

/// How much of the first VOB is read for the stream headers, and of the last one for the
/// final timestamp.
const HEAD_BYTES: u64 = 6 << 20;
const TAIL_BYTES: u64 = 2 << 20;
const SECTOR: u64 = 2048;

pub struct Dvd {
    /// The name to show: the folder above VIDEO_TS, the folder itself, or the file.
    pub name: String,
    /// Every file of the folder, menus and backups included.
    pub total_bytes: u64,
    pub sets: Vec<TitleSet>,
}

pub struct TitleSet {
    /// 1-99 for VTS_nn files, 0 for loose VOB files.
    pub number: u32,
    /// The title VOBs (VTS_nn_1..9), or the loose file.
    pub vobs: Vec<PathBuf>,
    pub vob_bytes: u64,
    pub ifo: Option<Result<Ifo, String>>,
    pub scan: Result<VobScan, String>,
}

impl TitleSet {
    /// Playing time of the VOB content: every cell counted once (IFO), else the timestamps.
    pub fn seconds(&self) -> Option<f64> {
        match &self.ifo {
            Some(Ok(ifo)) if ifo.content_seconds > 0.0 => Some(ifo.content_seconds),
            _ => self.scan.as_ref().ok().and_then(|s| s.pts_seconds),
        }
    }
}

pub struct Ifo {
    pub video: VideoAttr,
    pub audio: Vec<AudioAttr>,
    /// Subtitle languages ("en"), empty when not given.
    pub subtitles: Vec<String>,
    pub titles: Vec<Pgc>,
    /// Every cell of the title set counted once.
    pub content_seconds: f64,
}

pub struct VideoAttr {
    pub mpeg: u8,
    pub standard: &'static str,
    pub aspect: &'static str,
    pub width: u32,
    pub height: u32,
    pub letterboxed: bool,
}

pub struct AudioAttr {
    pub coding: &'static str,
    pub channels: u32,
    pub rate: u32,
    /// LPCM only.
    pub bits: Option<u32>,
    pub language: String,
    /// "Director's comments" and the like; empty for a normal track.
    pub purpose: &'static str,
}

/// A program chain: one title as the player sees it.
pub struct Pgc {
    pub seconds: f64,
    pub chapters: u32,
}

#[derive(Default)]
pub struct VobScan {
    pub video: Option<VideoEs>,
    /// In stream order (substream or stream id).
    pub audio: Vec<AudioEs>,
    pub subpictures: usize,
    /// From the first and last video timestamps (VOB files without IFO).
    pub pts_seconds: Option<f64>,
}

pub struct VideoEs {
    pub mpeg2: bool,
    pub width: u32,
    pub height: u32,
    pub aspect: &'static str,
    pub fps: f64,
    /// The maximum the sequence header announces.
    pub max_kbps: f64,
    pub profile: Option<String>,
    pub progressive: Option<bool>,
    pub chroma: Option<&'static str>,
}

pub struct AudioEs {
    pub id: u8,
    pub coding: &'static str,
    pub channels: String,
    pub rate: u32,
    pub kbps: Option<f64>,
    pub bits: Option<u32>,
}

// ---------------------------------------------------------------- finding DVDs

fn is_vob(path: &Path) -> bool {
    ext_lower(path) == "vob"
}

fn holds_dvd_files(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().any(|e| matches!(ext_lower(&e.path()).as_str(), "vob" | "ifo")))
        .unwrap_or(false)
}

/// The DVDs in a path: VIDEO_TS folders, folders holding VOB files, single VOB files, up to
/// three folders deep (a show folder with DVD1/VIDEO_TS and DVD2/VIDEO_TS).
pub fn find(path: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
        if holds_dvd_files(dir) {
            out.push(dir.to_path_buf());
            return;
        }
        if depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut subdirs: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| p.is_dir() && !file_name(p).starts_with('.')).collect();
        subdirs.sort_by(|a, b| natural_path_cmp(a, b));
        for sub in subdirs {
            walk(&sub, depth - 1, out);
        }
    }
    let mut out = Vec::new();
    if path.is_file() {
        if is_vob(path) {
            out.push(path.to_path_buf());
        }
    } else if path.is_dir() {
        walk(path, 3, &mut out);
    }
    out
}

// ---------------------------------------------------------------- reading a DVD

/// The files of a title set: IFO, BUP, title VOBs.
type SetFiles = (Option<PathBuf>, Option<PathBuf>, Vec<PathBuf>);

pub fn read(root: &Path) -> Result<Dvd, String> {
    let named = |p: &Path| file_name(p).eq_ignore_ascii_case("VIDEO_TS");
    let name = match root.parent() {
        Some(parent) if named(root) => file_name(parent),
        _ => file_name(root),
    };
    if root.is_file() {
        let size = std::fs::metadata(root).map(|m| m.len()).unwrap_or(0);
        let vobs = vec![root.to_path_buf()];
        let scan = scan_vobs(&vobs);
        return Ok(Dvd { name, total_bytes: size, sets: vec![TitleSet { number: 0, vobs, vob_bytes: size, ifo: None, scan }] });
    }
    let entries = std::fs::read_dir(root).map_err(|e| e.to_string())?;
    let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    files.sort_by(|a, b| natural_path_cmp(a, b));
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let total_bytes = files.iter().map(|p| size(p)).sum();

    // VTS_nn_0.IFO (or .BUP), VTS_nn_k.VOB (k = 0: menu, 1-9: title).
    let re = regex::Regex::new(r"(?i)^VTS_(\d\d)_(\d)\.(VOB|IFO|BUP)$").unwrap();
    let mut by_set: BTreeMap<u32, SetFiles> = BTreeMap::new();
    let mut loose = Vec::new();
    for f in &files {
        let fname = file_name(f);
        if let Some(c) = re.captures(&fname) {
            let set: u32 = c[1].parse().unwrap_or(0);
            let part: u32 = c[2].parse().unwrap_or(0);
            let entry = by_set.entry(set).or_default();
            match c[3].to_ascii_uppercase().as_str() {
                "IFO" => entry.0 = Some(f.clone()),
                "BUP" => entry.1 = Some(f.clone()),
                _ if part > 0 => entry.2.push(f.clone()),
                _ => {}
            }
        } else if is_vob(f) && !fname.eq_ignore_ascii_case("VIDEO_TS.VOB") {
            loose.push(f.clone());
        }
    }
    let mut sets = Vec::new();
    for (number, (ifo, bup, vobs)) in by_set {
        // A title set with no title VOB is only a menu; it has nothing to show.
        if !vobs.is_empty() {
            sets.push(title_set(number, ifo.or(bup), vobs));
        }
    }
    for vob in loose {
        sets.push(title_set(0, None, vec![vob]));
    }
    if sets.is_empty() {
        return Err("no title VOB file".into());
    }
    Ok(Dvd { name, total_bytes, sets })
}

fn title_set(number: u32, ifo: Option<PathBuf>, vobs: Vec<PathBuf>) -> TitleSet {
    let ifo = ifo.map(|p| read_ifo(&p).map_err(|e| format!("{}: {e}", file_name(&p))));
    let vob_bytes = vobs.iter().map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)).sum();
    let scan = scan_vobs(&vobs);
    TitleSet { number, vobs, vob_bytes, ifo, scan }
}

/// "VTS_01_1.VOB" -> (1, 1); None for a VOB of another name.
fn vts_parts(name: &str) -> Option<(u32, u32)> {
    let up = name.to_ascii_uppercase();
    let rest = up.strip_prefix("VTS_")?.strip_suffix(".VOB")?;
    let (set, part) = rest.split_once('_')?;
    (set.len() == 2 && part.len() == 1).then_some(())?;
    Some((set.parse().ok()?, part.parse().ok()?))
}

/// The VOB that stands for its title in the list: the first title VOB of a title set
/// (VTS_nn_1), or a loose VOB; not menus (VIDEO_TS.VOB, VTS_nn_0) nor the other parts.
pub fn title_vob(path: &Path) -> bool {
    let name = file_name(path);
    is_vob(path) && !name.eq_ignore_ascii_case("VIDEO_TS.VOB") && vts_parts(&name).is_none_or(|(_, part)| part == 1)
}

/// The title set a VOB of the list stands for: its title VOBs and IFO beside it, or the
/// loose file alone.
pub fn title_of(vob: &Path) -> TitleSet {
    let Some((set, _)) = vts_parts(&file_name(vob)) else { return title_set(0, None, vec![vob.to_path_buf()]) };
    let dir = vob.parent().unwrap_or(Path::new("."));
    let mut vobs = Vec::new();
    let (mut ifo, mut bup) = (None, None);
    for p in std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()) {
        let name = file_name(&p).to_ascii_uppercase();
        if vts_parts(&name).is_some_and(|(s, part)| s == set && part > 0) {
            vobs.push(p);
        } else if name == format!("VTS_{set:02}_0.IFO") {
            ifo = Some(p);
        } else if name == format!("VTS_{set:02}_0.BUP") {
            bup = Some(p);
        }
    }
    vobs.sort_by(|a, b| natural_path_cmp(a, b));
    title_set(set, ifo.or(bup), vobs)
}

/// One audio track of a DVD title set.
pub struct DvdAudio {
    /// "AC-3", "DTS", "LPCM", "MPEG Audio Layer 2"...
    pub coding: &'static str,
    /// The stream in the VOBs: a private stream 1 substream (0x80 AC-3, 0x88 DTS, 0xA0
    /// LPCM, + track) or an MPEG audio stream (0xC0 + track).
    pub id: u8,
    /// "2", "5.1"
    pub channels: String,
    pub rate: u32,
    pub bits: Option<u32>,
    pub kbps: Option<f64>,
    /// "en", empty when not given.
    pub language: String,
    pub purpose: &'static str,
}

impl DvdAudio {
    pub fn lossless(&self) -> bool {
        self.coding == "LPCM"
    }

    /// "AC-3 (lossy), 5.1 ch, English": for the list.
    pub fn summary(&self) -> String {
        let mut p = vec![format!("{} ({})", self.coding, if self.lossless() { "lossless" } else { "lossy" }), format!("{} ch", self.channels)];
        if let Some(bits) = self.bits {
            p.push(format!("{bits}-bit"));
        }
        if !self.language.is_empty() {
            p.push(crate::mediainfo::language_name(&self.language));
        }
        p.join(", ")
    }
}

/// The audio tracks of a title set: from its IFO (languages) matched with the streams of
/// its VOBs (layouts, bit rates), or from the streams alone.
pub fn dvd_audio(set: &TitleSet) -> Vec<DvdAudio> {
    let streams = set.scan.as_ref().map(|s| s.audio.as_slice()).unwrap_or(&[]);
    match &set.ifo {
        Some(Ok(i)) if !i.audio.is_empty() => {
            let mut used = Vec::new();
            i.audio
                .iter()
                .enumerate()
                .map(|(n, a)| {
                    // Stream n is substream 0x80 + n (AC-3), 0x88 + n (DTS), 0xA0 + n (LPCM),
                    // or MPEG stream 0xC0 + n; some authoring tools number each kind from 0.
                    let id = match a.coding {
                        "AC-3" => 0x80,
                        "DTS" => 0x88,
                        "LPCM" => 0xa0,
                        _ => 0xc0,
                    } + n as u8;
                    let es = streams
                        .iter()
                        .find(|s| s.id == id)
                        .or_else(|| streams.iter().find(|s| !used.contains(&s.id) && a.coding.starts_with(s.coding.split(' ').next().unwrap_or(""))));
                    if let Some(s) = es {
                        used.push(s.id);
                    }
                    DvdAudio {
                        coding: a.coding,
                        id: es.map_or(id, |s| s.id),
                        channels: es.map_or_else(|| a.channels.to_string(), |s| s.channels.clone()),
                        rate: es.map_or(a.rate, |s| s.rate),
                        bits: es.and_then(|s| s.bits).or(a.bits),
                        kbps: es.and_then(|s| s.kbps),
                        language: a.language.clone(),
                        purpose: a.purpose,
                    }
                })
                .collect()
        }
        _ => streams
            .iter()
            .map(|s| DvdAudio { coding: s.coding, id: s.id, channels: s.channels.clone(), rate: s.rate, bits: s.bits, kbps: s.kbps, language: String::new(), purpose: "" })
            .collect(),
    }
}

/// The header of a DVD title for the list: its best audio track (an LPCM one if there is
/// one, since it can be extracted without loss), and its length.
pub fn probe_title(vob: &Path) -> Result<crate::audio::AudioInfo, String> {
    read_title(vob).map(|(info, _)| info)
}

/// The header of a DVD title and all its audio tracks.
pub fn read_title(vob: &Path) -> Result<(crate::audio::AudioInfo, Vec<DvdAudio>), String> {
    let set = title_of(vob);
    if let (Err(e), None | Some(Err(_))) = (&set.scan, &set.ifo) {
        return Err(e.clone());
    }
    let tracks = dvd_audio(&set);
    let best = tracks.iter().find(|t| t.lossless()).or(tracks.first()).ok_or("DVD title without audio")?;
    // "5.1" -> 6 channels.
    let channels = best.channels.split('.').filter_map(|n| n.parse::<u32>().ok()).sum::<u32>().max(1);
    let samples = set.seconds().map_or(0, |s| (s * best.rate as f64).round() as u64);
    let info = crate::audio::AudioInfo {
        format: crate::audio::Format::Vob,
        rate: best.rate,
        bits: best.bits.unwrap_or(16),
        channels,
        samples,
        md5: None,
        float: false,
        codec: crate::audio::Codec::from_dvd(best.coding),
    };
    Ok((info, tracks))
}

// ---------------------------------------------------------------- extracting a track

/// The folder a title's extracted audio goes to: the DVD's (above VIDEO_TS), or the folder
/// of a loose VOB.
pub fn output_dir(vob: &Path) -> PathBuf {
    let dir = vob.parent().unwrap_or(Path::new(".")).to_path_buf();
    match dir.parent() {
        Some(parent) if file_name(&dir).eq_ignore_ascii_case("VIDEO_TS") => parent.to_path_buf(),
        _ => dir,
    }
}

/// The file an extracted track is written to: "<DVD> - title 1 - audio 2 LPCM French.flac"
/// (FLAC for LPCM; the stream as it is for the lossy codecs).
pub fn extract_path(vob: &Path, track: usize, t: &DvdAudio) -> PathBuf {
    let dir = output_dir(vob);
    let head = match vts_parts(&file_name(vob)) {
        Some((set, _)) => format!("{} - title {set}", file_name(&dir)),
        None => vob.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    let coding = match t.coding {
        c if c.starts_with("MPEG") => "MPEG audio",
        c => c,
    };
    let language = if t.language.is_empty() { String::new() } else { format!(" {}", crate::mediainfo::language_name(&t.language)) };
    let ext = match t.coding {
        "LPCM" => "flac",
        "AC-3" => "ac3",
        "DTS" => "dts",
        _ => "mp2",
    };
    dir.join(crate::tagging::safe_name(&format!("{head} - audio {} {coding}{language}", track + 1)) + "." + ext)
}

/// The payload of one track, in stream order, through every title VOB of its title set;
/// `progress` gets the bytes read. Packs are 2048 bytes and a VOB file holds whole packs,
/// so the files are read in pack-aligned pieces.
fn track_payload(vobs: &[PathBuf], id: u8, cancel: &std::sync::atomic::AtomicBool, progress: &mut dyn FnMut(u64), each: &mut dyn FnMut(&[u8])) -> Result<(), String> {
    const PIECE: usize = 512 * SECTOR as usize;
    let mut buf = vec![0u8; PIECE];
    for vob in vobs {
        let mut f = File::open(vob).map_err(|e| format!("{}: {e}", file_name(vob)))?;
        loop {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("stopped".into());
            }
            let mut got = 0;
            while got < PIECE {
                match f.read(&mut buf[got..]).map_err(|e| format!("{}: {e}", file_name(vob)))? {
                    0 => break,
                    n => got += n,
                }
            }
            if got == 0 {
                break;
            }
            packets(&buf[..got], |p| match (p.id, id) {
                // Private stream 1: the first byte names the substream; AC-3 and DTS have
                // 3 more header bytes, LPCM 6.
                (0xbd, 0x80..=0x8f) if p.payload.first() == Some(&id) => each(p.payload.get(4..).unwrap_or(&[])),
                (0xbd, 0xa0..=0xaf) if p.payload.first() == Some(&id) => each(p.payload.get(1..).unwrap_or(&[])),
                (mpeg, _) if mpeg == id && (0xc0..=0xdf).contains(&mpeg) => each(p.payload),
                _ => {}
            });
            progress(got as u64);
            if got < PIECE {
                break;
            }
        }
    }
    Ok(())
}

/// DVD LPCM to little-endian PCM: big-endian 16-bit samples; 20 and 24-bit ones in groups
/// of two sample frames, split into units of 4 samples (high 16 bits, then low bits).
struct LpcmDecoder {
    bits: u32,
    channels: usize,
    pending: Vec<u8>,
}

impl LpcmDecoder {
    /// Bytes of one group: one sample frame at 16 bits, two at 20 and 24.
    fn group(&self) -> usize {
        match self.bits {
            16 => 2 * self.channels,
            20 => 5 * self.channels,
            _ => 6 * self.channels,
        }
    }

    fn push(&mut self, data: &[u8], out: &mut Vec<u8>) {
        self.pending.extend_from_slice(data);
        let group = self.group();
        let whole = self.pending.len() / group * group;
        let ch = self.channels;
        for g in self.pending[..whole].chunks_exact(group) {
            match self.bits {
                16 => {
                    for [a, b] in g.as_chunks::<2>().0 {
                        out.extend_from_slice(&[*b, *a]);
                    }
                }
                // The 2 x channels samples of the group, in order, go by units of 4 (2 for
                // mono): their high 16 bits, then their low bits (a byte each at 24 bits,
                // a nibble each at 20).
                bits => {
                    let mut rest = g;
                    let mut left = 2 * ch;
                    while left > 0 {
                        let k = left.min(4);
                        let low_len = if bits == 20 { k / 2 } else { k };
                        let (high, after) = rest.split_at(2 * k);
                        let (low, after) = after.split_at(low_len);
                        for i in 0..k {
                            if bits == 20 {
                                let nibble = (low[i / 2] >> if i % 2 == 0 { 4 } else { 0 }) & 0x0f;
                                let v = (i16::from_be_bytes([high[2 * i], high[2 * i + 1]]) as i32) << 4 | nibble as i32;
                                out.extend_from_slice(&v.to_le_bytes()[..3]);
                            } else {
                                out.extend_from_slice(&[low[i], high[2 * i + 1], high[2 * i]]);
                            }
                        }
                        rest = after;
                        left -= k;
                    }
                }
            }
        }
        self.pending.drain(..whole);
    }
}

/// What an extraction wrote.
pub struct Extraction {
    pub path: PathBuf,
    /// LPCM: the sample frames in the FLAC file; other codecs: None.
    pub samples: Option<u64>,
    pub bytes: u64,
}

/// Extracts track `track` (as `dvd_audio` lists them) of the title `vob` stands for, from
/// all its VOBs, to one file beside the DVD: FLAC for LPCM (verified by decoding it
/// again), the stream as it is for AC-3, DTS and MPEG audio. An existing file is kept.
pub fn extract(vob: &Path, track: usize, cancel: &std::sync::atomic::AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<Extraction, String> {
    let set = title_of(vob);
    let tracks = dvd_audio(&set);
    let t = tracks.get(track).ok_or_else(|| format!("no audio track {}", track + 1))?;
    let out = extract_path(vob, track, t);
    if out.exists() {
        return Err(format!("{} exists already", file_name(&out)));
    }
    if t.lossless() {
        let bits = t.bits.ok_or("LPCM track of unknown bit depth")?;
        let channels: u32 = t.channels.parse().map_err(|_| format!("LPCM track with {} channels", t.channels))?;
        let info = crate::audio::AudioInfo {
            format: crate::audio::Format::Flac,
            rate: t.rate,
            bits,
            channels,
            samples: 0,
            md5: None,
            float: false,
            codec: None,
        };
        crate::encode::check_encodable(&info)?;
        let mut decoder = LpcmDecoder { bits, channels: channels as usize, pending: Vec::new() };
        let comment = format!("Extracted from {}, audio {} (LPCM)", file_name(&output_dir(vob)), track + 1);
        let tags = [("COMMENT".to_string(), comment)];
        let encoded = crate::encode::pcm_to_flac(&out, &info, 8, &tags, cancel, |sink| {
            let mut pcm = Vec::new();
            let mut format = None;
            let mut changed = false;
            track_payload(&set.vobs, t.id, cancel, progress, &mut |payload: &[u8]| {
                // 3 header bytes before the samples: their format (bits, rate, channels),
                // which must stay the same through the track.
                let Some((head, samples)) = payload.get(3..6).zip(payload.get(6..)) else { return };
                if *format.get_or_insert(head[1]) != head[1] {
                    changed = true;
                }
                if changed {
                    return;
                }
                pcm.clear();
                decoder.push(samples, &mut pcm);
                sink(&pcm);
            })?;
            if changed {
                return Err("the LPCM format changes within the track".into());
            }
            Ok(())
        })?;
        if encoded.samples == 0 {
            let _ = std::fs::remove_file(&out);
            return Err("no audio found for this track".into());
        }
        let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        return Ok(Extraction { path: out, samples: Some(encoded.samples), bytes });
    }
    let mut part = out.clone().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    let result = (|| {
        let mut file = std::io::BufWriter::new(File::create(&part).map_err(|e| e.to_string())?);
        let mut written = 0u64;
        let mut failure = None;
        track_payload(&set.vobs, t.id, cancel, progress, &mut |payload: &[u8]| {
            if failure.is_none() {
                match std::io::Write::write_all(&mut file, payload) {
                    Ok(()) => written += payload.len() as u64,
                    Err(e) => failure = Some(e.to_string()),
                }
            }
        })?;
        if let Some(e) = failure {
            return Err(e);
        }
        std::io::Write::flush(&mut file).map_err(|e| e.to_string())?;
        if written == 0 {
            return Err("no audio found for this track".to_string());
        }
        Ok(written)
    })();
    match result {
        Ok(bytes) => {
            std::fs::rename(&part, &out).map_err(|e| e.to_string())?;
            Ok(Extraction { path: out, samples: None, bytes })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

// ---------------------------------------------------------------- IFO

fn be16(d: &[u8], i: usize) -> Option<usize> {
    d.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
}

fn be32(d: &[u8], i: usize) -> Option<usize> {
    d.get(i..i + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

fn bcd(b: u8) -> u32 {
    (b >> 4) as u32 * 10 + (b & 0x0f) as u32
}

/// hh mm ss ff in BCD, the two high bits of ff giving the frame rate.
fn dvd_time(t: &[u8]) -> f64 {
    let fps = match t[3] >> 6 {
        1 => 25.0,
        3 => 30000.0 / 1001.0,
        _ => 0.0,
    };
    let frames = if fps > 0.0 { bcd(t[3] & 0x3f) as f64 / fps } else { 0.0 };
    (bcd(t[0]) * 3600 + bcd(t[1]) * 60 + bcd(t[2])) as f64 + frames
}

fn language(code: &[u8]) -> String {
    if code.len() == 2 && code.iter().all(u8::is_ascii_alphabetic) {
        String::from_utf8_lossy(code).to_ascii_lowercase()
    } else {
        String::new()
    }
}

fn parse_video(b0: u8, b1: u8) -> VideoAttr {
    let pal = (b0 >> 4) & 3 == 1;
    let height = if pal { 576 } else { 480 };
    let (width, height) = match (b1 >> 2) & 3 {
        0 => (720, height),
        1 => (704, height),
        2 => (352, height),
        _ => (352, height / 2),
    };
    VideoAttr {
        mpeg: if b0 >> 6 == 0 { 1 } else { 2 },
        standard: if pal { "PAL" } else { "NTSC" },
        aspect: if (b0 >> 2) & 3 == 3 { "16:9" } else { "4:3" },
        width,
        height,
        letterboxed: (b1 >> 1) & 1 == 1,
    }
}

fn parse_audio(d: &[u8]) -> AudioAttr {
    let coding = match d[0] >> 5 {
        0 => "AC-3",
        2 => "MPEG-1 Audio",
        3 => "MPEG-2 Audio",
        4 => "LPCM",
        6 => "DTS",
        _ => "unknown",
    };
    let bits = (coding == "LPCM").then(|| match d[1] >> 6 {
        0 => 16,
        1 => 20,
        _ => 24,
    });
    AudioAttr {
        coding,
        channels: (d[1] & 7) as u32 + 1,
        rate: if (d[1] >> 4) & 3 == 1 { 96000 } else { 48000 },
        bits,
        language: if (d[0] >> 2) & 3 == 1 { language(&d[2..4]) } else { String::new() },
        purpose: match d[5] {
            2 => "for the visually impaired",
            3 => "director's comments",
            4 => "alternate director's comments",
            _ => "",
        },
    }
}

fn read_ifo(path: &Path) -> Result<Ifo, String> {
    let mut data = Vec::new();
    File::open(path).and_then(|f| f.take(16 << 20).read_to_end(&mut data)).map_err(|e| e.to_string())?;
    if !data.starts_with(b"DVDVIDEO-VTS") {
        return Err("not a title set IFO file".into());
    }
    let bad = || "damaged IFO file".to_string();
    let video = parse_video(*data.get(0x200).ok_or_else(bad)?, *data.get(0x201).ok_or_else(bad)?);
    let audio = (0..be16(&data, 0x202).ok_or_else(bad)?.min(8))
        .filter_map(|i| data.get(0x204 + 8 * i..0x204 + 8 * i + 8).map(parse_audio))
        .collect();
    let subtitles = (0..be16(&data, 0x254).ok_or_else(bad)?.min(32))
        .filter_map(|i| data.get(0x256 + 6 * i..0x256 + 6 * i + 6))
        .map(|d| if d[0] & 3 == 1 { language(&d[2..4]) } else { String::new() })
        .collect();

    let mut titles = Vec::new();
    // Every cell once, keyed by its first sector: titles often share cells.
    let mut cells: BTreeMap<usize, f64> = BTreeMap::new();
    let base = be32(&data, 0xcc).ok_or_else(bad)? * SECTOR as usize;
    if base > 0 {
        let count = be16(&data, base).ok_or_else(bad)?;
        for k in 0..count {
            let Some(offset) = be32(&data, base + 8 + 8 * k + 4) else { break };
            let pgc = base + offset;
            let Some(head) = data.get(pgc..pgc + 0xec) else { continue };
            titles.push(Pgc { seconds: dvd_time(&head[4..8]), chapters: head[2] as u32 });
            let table = pgc + be16(&data, pgc + 0xe8).unwrap_or(0);
            for c in 0..head[3] as usize {
                let Some(cell) = data.get(table + 24 * c..table + 24 * c + 24) else { break };
                let first_sector = be32(cell, 8).unwrap_or(0);
                cells.entry(first_sector).or_insert_with(|| dvd_time(&cell[4..8]));
            }
        }
    }
    Ok(Ifo { video, audio, subtitles, titles, content_seconds: cells.values().sum() })
}

// ---------------------------------------------------------------- VOB (MPEG program stream)

struct Packet<'a> {
    id: u8,
    pts: Option<u64>,
    payload: &'a [u8],
}

/// The PES packets of an MPEG-2 program stream, from the start of a pack.
fn packets<'a>(data: &'a [u8], mut each: impl FnMut(Packet<'a>)) {
    let mut i = 0;
    while i + 6 <= data.len() {
        if data[i..i + 3] != [0, 0, 1] {
            i += 1;
            continue;
        }
        let id = data[i + 3];
        match id {
            0xba => {
                // MPEG-2 pack header: 14 bytes and stuffing; MPEG-1: 12 bytes.
                i += if data[i + 4] & 0xc0 == 0x40 { 14 + data.get(i + 13).map_or(0, |b| (b & 7) as usize) } else { 12 };
            }
            0xbb..=0xff => {
                let len = u16::from_be_bytes([data[i + 4], data[i + 5]]) as usize;
                let end = i + 6 + len;
                let Some(body) = data.get(i + 6..end) else { break };
                if matches!(id, 0xbd | 0xc0..=0xef) && body.len() >= 3 && body[0] & 0xc0 == 0x80 {
                    let flags = body[1];
                    let start = 3 + body[2] as usize;
                    let pts = (flags & 0x80 != 0).then(|| body.get(3..8).map(pts)).flatten();
                    if let Some(payload) = body.get(start..) {
                        each(Packet { id, pts, payload });
                    }
                }
                i = end;
            }
            _ => i += 4,
        }
    }
}

fn pts(b: &[u8]) -> u64 {
    ((b[0] as u64 >> 1) & 7) << 30 | (b[1] as u64) << 22 | (b[2] as u64 >> 1) << 15 | (b[3] as u64) << 7 | b[4] as u64 >> 1
}

fn read_part(path: &Path, from_end: bool, bytes: u64) -> Result<Vec<u8>, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    if from_end && len > bytes {
        // Packs are 2048 bytes long: start on one.
        f.seek(SeekFrom::Start((len - bytes) / SECTOR * SECTOR)).map_err(|e| e.to_string())?;
    }
    let mut data = Vec::new();
    f.take(bytes).read_to_end(&mut data).map_err(|e| e.to_string())?;
    Ok(data)
}

fn scan_vobs(vobs: &[PathBuf]) -> Result<VobScan, String> {
    let head = read_part(&vobs[0], false, HEAD_BYTES)?;
    let mut video_es = Vec::new();
    let mut streams: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    let mut subpictures = std::collections::BTreeSet::new();
    let mut first_pts = None;
    packets(&head, |p| match p.id {
        0xe0 => {
            if first_pts.is_none() {
                first_pts = p.pts;
            }
            if video_es.len() < 1 << 20 {
                video_es.extend_from_slice(p.payload);
            }
        }
        0xbd => {
            let Some(&sub) = p.payload.first() else { return };
            match sub {
                0x20..=0x3f => {
                    subpictures.insert(sub);
                }
                0x80..=0x8f | 0xa0..=0xa7 => {
                    let buf = streams.entry(sub).or_default();
                    if buf.len() < 64 << 10 {
                        // LPCM keeps its 7-byte header (it describes the samples).
                        let skip = if sub >= 0xa0 { 0 } else { 4 };
                        if sub < 0xa0 || buf.is_empty() {
                            buf.extend_from_slice(p.payload.get(skip..).unwrap_or(&[]));
                        }
                    }
                }
                _ => {}
            }
        }
        0xc0..=0xdf => {
            let buf = streams.entry(p.id).or_default();
            if buf.len() < 64 << 10 {
                buf.extend_from_slice(p.payload);
            }
        }
        _ => {}
    });
    if first_pts.is_none() && streams.is_empty() {
        return Err("no MPEG program stream found".into());
    }
    let mut last_pts = None;
    if let Some(last) = vobs.last() {
        if let Ok(tail) = read_part(last, true, TAIL_BYTES) {
            packets(&tail, |p| {
                if p.id == 0xe0 && p.pts.is_some() {
                    last_pts = p.pts;
                }
            });
        }
    }
    let video = video_header(&video_es);
    // The last timestamp starts the last frame: add its length.
    let frame = video.as_ref().filter(|v| v.fps > 0.0).map_or(0.0, |v| 1.0 / v.fps);
    let pts_seconds = match (first_pts, last_pts) {
        (Some(a), Some(b)) if b > a => Some((b - a) as f64 / 90000.0 + frame),
        _ => None,
    };
    Ok(VobScan {
        video,
        audio: streams.iter().filter_map(|(&id, data)| audio_header(id, data)).collect(),
        subpictures: subpictures.len(),
        pts_seconds,
    })
}

fn find_code(data: &[u8], code: u8, from: usize) -> Option<usize> {
    data.get(from..)?.windows(4).position(|w| w == [0, 0, 1, code]).map(|p| p + from)
}

fn video_header(es: &[u8]) -> Option<VideoEs> {
    let at = find_code(es, 0xb3, 0)?;
    let h = es.get(at + 4..at + 12)?;
    let width = (h[0] as u32) << 4 | (h[1] as u32) >> 4;
    let height = ((h[1] & 0x0f) as u32) << 8 | h[2] as u32;
    let fps = match h[3] & 0x0f {
        1 => 24000.0 / 1001.0,
        2 => 24.0,
        3 => 25.0,
        4 => 30000.0 / 1001.0,
        5 => 30.0,
        6 => 50.0,
        7 => 60000.0 / 1001.0,
        8 => 60.0,
        _ => 0.0,
    };
    let rate_value = (h[4] as u32) << 10 | (h[5] as u32) << 2 | (h[6] as u32) >> 6;
    let mut video = VideoEs {
        mpeg2: false,
        width,
        height,
        aspect: match h[3] >> 4 {
            1 => "square pixels",
            2 => "4:3",
            3 => "16:9",
            4 => "2.21:1",
            _ => "?",
        },
        fps,
        max_kbps: rate_value as f64 * 0.4,
        profile: None,
        progressive: None,
        chroma: None,
    };
    // The sequence extension (MPEG-2) follows the header.
    let mut from = at + 12;
    while let Some(ext) = find_code(es, 0xb5, from) {
        if ext > at + 400 {
            break;
        }
        if let Some(e) = es.get(ext + 4..ext + 10) {
            if e[0] >> 4 == 1 {
                video.mpeg2 = true;
                let pl = (e[0] & 0x0f) << 4 | e[1] >> 4;
                let profile = match (pl >> 4) & 7 {
                    1 => "High",
                    2 => "Spatial",
                    3 => "SNR",
                    4 => "Main",
                    5 => "Simple",
                    _ => "",
                };
                let level = match pl & 0x0f {
                    4 => "High",
                    6 => "High 1440",
                    8 => "Main",
                    10 => "Low",
                    _ => "",
                };
                if !profile.is_empty() && !level.is_empty() {
                    video.profile = Some(format!("{profile}@{level}"));
                }
                video.progressive = Some(e[1] & 0x08 != 0);
                video.chroma = Some(match (e[1] >> 1) & 3 {
                    2 => "4:2:2",
                    3 => "4:4:4",
                    _ => "4:2:0",
                });
                break;
            }
        }
        from = ext + 4;
    }
    Some(video)
}

const AC3_KBPS: [u32; 19] = [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640];
const DTS_KBPS: [f64; 29] = [
    32.0, 56.0, 64.0, 96.0, 112.0, 128.0, 192.0, 224.0, 256.0, 320.0, 384.0, 448.0, 512.0, 576.0, 640.0, 768.0, 960.0,
    1024.0, 1152.0, 1280.0, 1344.0, 1408.0, 1411.2, 1472.0, 1536.0, 1920.0, 2048.0, 3072.0, 3840.0,
];

fn channel_layout(main: u32, lfe: bool) -> String {
    if lfe {
        format!("{main}.1")
    } else {
        main.to_string()
    }
}

fn audio_header(id: u8, d: &[u8]) -> Option<AudioEs> {
    match id {
        0x80..=0x87 => {
            let at = d.windows(2).position(|w| w == [0x0b, 0x77])?;
            let h = d.get(at..at + 8)?;
            let rate = match h[4] >> 6 {
                0 => 48000,
                1 => 44100,
                2 => 32000,
                _ => return None,
            };
            let kbps = *AC3_KBPS.get((h[4] & 0x3f) as usize >> 1)? as f64;
            // acmod, then the mix levels it implies, then lfeon.
            let bits = u16::from_be_bytes([h[6], h[7]]);
            let acmod = (bits >> 13) as u32;
            let mut pos = 3;
            if acmod & 1 != 0 && acmod != 1 {
                pos += 2;
            }
            if acmod & 4 != 0 {
                pos += 2;
            }
            if acmod == 2 {
                pos += 2;
            }
            let lfe = bits >> (15 - pos) & 1 == 1;
            let main = [2, 1, 2, 3, 3, 4, 4, 5][acmod as usize];
            Some(AudioEs { id, coding: "AC-3", channels: channel_layout(main, lfe), rate, kbps: Some(kbps), bits: None })
        }
        0x88..=0x8f => {
            let at = d.windows(4).position(|w| w == [0x7f, 0xfe, 0x80, 0x01])?;
            let h = d.get(at + 4..at + 12)?;
            let word = u64::from_be_bytes([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]]);
            let field = |from: u32, len: u32| ((word >> (64 - from - len)) & ((1 << len) - 1)) as usize;
            // ftype 1, short 5, cpf 1, nblks 7, fsize 14, amode 6, sfreq 4, rate 5,
            // then 10 flag bits and lff 2.
            let amode = field(28, 6);
            let rate = [0, 8000, 16000, 32000, 0, 0, 11025, 22050, 44100, 0, 0, 12000, 24000, 48000].get(field(34, 4)).copied()?;
            let kbps = DTS_KBPS.get(field(38, 5)).copied();
            let lfe = field(53, 2) != 0;
            let main = [1, 2, 2, 2, 2, 3, 3, 4, 4, 5, 6, 6, 6, 7, 8, 8].get(amode).copied().unwrap_or(0);
            Some(AudioEs { id, coding: "DTS", channels: channel_layout(main, lfe), rate, kbps, bits: None })
        }
        0xa0..=0xa7 => {
            // substream, frames, pointer (2), emphasis/mute/frame, quantization/rate/channels.
            let b = *d.get(5)?;
            let bits = [16, 20, 24, 0][(b >> 6) as usize];
            let rate = if (b >> 4) & 3 == 1 { 96000 } else { 48000 };
            let channels = (b & 7) as u32 + 1;
            let kbps = (bits > 0).then(|| (rate * bits * channels) as f64 / 1000.0);
            Some(AudioEs { id, coding: "LPCM", channels: channels.to_string(), rate, kbps, bits: (bits > 0).then_some(bits) })
        }
        0xc0..=0xdf => {
            let h = (0..d.len().saturating_sub(4)).find_map(|i| mpa_header(&d[i..]))?;
            Some(AudioEs {
                id,
                coding: if h.layer == 2 { "MPEG Audio Layer 2" } else { "MPEG Audio" },
                channels: if h.mode == 3 { "1".into() } else { "2".into() },
                rate: h.rate,
                kbps: Some(h.kbps as f64),
                bits: None,
            })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------- MPEG audio frames

/// An MPEG audio frame header (also used for MP3/MP2 files).
#[derive(Clone, Copy, Debug)]
pub struct MpaHeader {
    /// 1, 2 or 25 (MPEG-2.5).
    pub version: u8,
    pub layer: u8,
    pub kbps: u32,
    pub rate: u32,
    /// 0 stereo, 1 joint stereo, 2 dual channel, 3 mono.
    pub mode: u8,
    pub frame_len: usize,
}

pub fn mpa_header(b: &[u8]) -> Option<MpaHeader> {
    if b.len() < 4 || b[0] != 0xff || b[1] & 0xe0 != 0xe0 {
        return None;
    }
    let version = match (b[1] >> 3) & 3 {
        0 => 25,
        2 => 2,
        3 => 1,
        _ => return None,
    };
    let layer = match (b[1] >> 1) & 3 {
        1 => 3,
        2 => 2,
        3 => 1,
        _ => return None,
    };
    let index = (b[2] >> 4) as usize;
    if index == 0 || index == 15 {
        return None;
    }
    let kbps = match (version, layer) {
        (1, 1) => [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448][index],
        (1, 2) => [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384][index],
        (1, 3) => [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320][index],
        (_, 1) => [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256][index],
        _ => [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160][index],
    };
    let base = match (b[2] >> 2) & 3 {
        0 => 44100,
        1 => 48000,
        2 => 32000,
        _ => return None,
    };
    let rate = match version {
        1 => base,
        2 => base / 2,
        _ => base / 4,
    };
    let padding = ((b[2] >> 1) & 1) as usize;
    let frame_len = match layer {
        1 => (12 * kbps as usize * 1000 / rate as usize + padding) * 4,
        3 if version != 1 => 72 * kbps as usize * 1000 / rate as usize + padding,
        _ => 144 * kbps as usize * 1000 / rate as usize + padding,
    };
    Some(MpaHeader { version, layer, kbps, rate, mode: b[3] >> 6, frame_len })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lpcm_reordering() {
        let mut out = Vec::new();
        // 16-bit stereo: big-endian words.
        LpcmDecoder { bits: 16, channels: 2, pending: Vec::new() }.push(&[0x12, 0x34, 0xfe, 0xdc], &mut out);
        assert_eq!(out, [0x34, 0x12, 0xdc, 0xfe]);
        // 24-bit stereo: L0 R0 L1 R1 high words, then their low bytes; split across two
        // packets.
        out.clear();
        let mut d = LpcmDecoder { bits: 24, channels: 2, pending: Vec::new() };
        let group = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0xa1, 0xa2, 0xa3, 0xa4];
        d.push(&group[..5], &mut out);
        assert!(out.is_empty());
        d.push(&group[5..], &mut out);
        assert_eq!(out, [0xa1, 0x22, 0x11, 0xa2, 0x44, 0x33, 0xa3, 0x66, 0x55, 0xa4, 0x88, 0x77]);
        // 24-bit, 6 channels: 12 samples by units of 4 (4 high words, then 4 low bytes).
        out.clear();
        let mut group = Vec::new();
        for unit in 0..3u8 {
            for i in 0..4u8 {
                group.extend_from_slice(&[unit, i]);
            }
            group.extend_from_slice(&[0xb0 + unit; 4]);
        }
        LpcmDecoder { bits: 24, channels: 6, pending: Vec::new() }.push(&group, &mut out);
        let samples: Vec<[u8; 3]> = out.as_chunks::<3>().0.to_vec();
        assert_eq!(samples[5], [0xb1, 1, 1]);
        assert_eq!(samples[11], [0xb2, 3, 2]);
        // 20-bit mono: two high words, then one byte of two nibbles.
        out.clear();
        LpcmDecoder { bits: 20, channels: 1, pending: Vec::new() }.push(&[0x80, 0x00, 0x00, 0x01, 0x5a], &mut out);
        assert_eq!(out, [0x05, 0x00, 0xf8, 0x1a, 0x00, 0x00]);
    }

    #[test]
    fn dvd_time_bcd() {
        // 1:23:45 and 12 frames at 25 fps.
        assert!((dvd_time(&[0x01, 0x23, 0x45, 0x40 | 0x12]) - (5025.0 + 12.0 / 25.0)).abs() < 1e-9);
    }

    #[test]
    fn ifo_attributes() {
        // MPEG-2, PAL, 16:9, 720x576.
        let v = parse_video(0b0101_1100, 0);
        assert_eq!((v.mpeg, v.standard, v.aspect, v.width, v.height), (2, "PAL", "16:9", 720, 576));
        // AC-3 with language, 48 kHz, 6 channels, English, director's comments.
        let a = parse_audio(&[0b0000_0100, 0b0000_0101, b'e', b'n', 0, 3, 0, 0]);
        assert_eq!((a.coding, a.channels, a.rate, a.language.as_str(), a.purpose), ("AC-3", 6, 48000, "en", "director's comments"));
        // LPCM 24 bits 96 kHz stereo.
        let a = parse_audio(&[0b1000_0000, 0b1001_0001, 0, 0, 0, 0, 0, 0]);
        assert_eq!((a.coding, a.bits, a.rate, a.channels), ("LPCM", Some(24), 96000, 2));
    }

    #[test]
    fn mp3_frame_header() {
        // MPEG-1 Layer 3, 320 kb/s, 44.1 kHz, joint stereo.
        let h = mpa_header(&[0xff, 0xfb, 0xe0, 0x40]).unwrap();
        assert_eq!((h.version, h.layer, h.kbps, h.rate, h.mode, h.frame_len), (1, 3, 320, 44100, 1, 1044));
    }
}
