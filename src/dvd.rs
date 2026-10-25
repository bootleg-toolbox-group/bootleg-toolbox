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
    let mut by_set: BTreeMap<u32, (Option<PathBuf>, Option<PathBuf>, Vec<PathBuf>)> = BTreeMap::new();
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
        let ifo = ifo.or(bup).map(|p| read_ifo(&p).or_else(|e| Err(format!("{}: {e}", file_name(&p)))));
        // A title set with no title VOB is only a menu; it has nothing to show.
        if vobs.is_empty() {
            continue;
        }
        let vob_bytes = vobs.iter().map(|p| size(p)).sum();
        let scan = scan_vobs(&vobs);
        sets.push(TitleSet { number, vobs, vob_bytes, ifo, scan });
    }
    for vob in loose {
        let vob_bytes = size(&vob);
        let vobs = vec![vob];
        let scan = scan_vobs(&vobs);
        sets.push(TitleSet { number: 0, vobs, vob_bytes, ifo: None, scan });
    }
    if sets.is_empty() {
        return Err("no title VOB file".into());
    }
    Ok(Dvd { name, total_bytes, sets })
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
