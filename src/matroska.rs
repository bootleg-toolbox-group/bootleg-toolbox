//! Matroska (MKA, MKV): the element walk that guards symphonia against damaged sizes, and
//! the list of tracks (codec, language, picture size) that symphonia does not give.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

const EBML: u32 = 0x1A45DFA3;
const SEGMENT: u32 = 0x18538067;
const INFO: u32 = 0x1549A966;
const TRACKS: u32 = 0x1654AE6B;
const CLUSTER: u32 = 0x1F43B675;

/// Matroska elements that hold other elements (the ones symphonia reads).
const MASTERS: [u32; 31] = [
    0x1A45DFA3, 0x18538067, 0x114D9B74, 0x4DBB, 0x1549A966, 0x1654AE6B, 0xAE, 0xE0, 0xE1, 0x6D80, 0x6240, 0x5034,
    0x5035, 0x1F43B675, 0xA0, 0x75A1, 0xA6, 0x1C53BB6B, 0xBB, 0xB7, 0xDB, 0x1043A770, 0x45B9, 0xB6, 0x8F, 0x80,
    0x1254C367, 0x7373, 0x63C0, 0x67C8, 0x1941A469,
];

/// A variable-length integer: (value, width, all value bits set). An element ID keeps its
/// marker bit; for a size, all bits set means "unknown".
fn vint(f: &mut impl Read, keep_marker: bool) -> Option<(u64, u32, bool)> {
    let mut b = [0u8];
    f.read_exact(&mut b).ok()?;
    let width = b[0].leading_zeros() + 1;
    if width > 8 {
        return None;
    }
    let mut value = if keep_marker { b[0] as u64 } else { (b[0] as u64) & (0xFF >> width) };
    let mut all_ones = value == (0xFF >> width) as u64;
    for _ in 1..width {
        f.read_exact(&mut b).ok()?;
        value = value << 8 | b[0] as u64;
        all_ones &= b[0] == 0xFF;
    }
    Some((value, width, all_ones))
}

/// Checks a Matroska file's element sizes before symphonia reads it: symphonia 0.5
/// allocates an element's announced size before reading it, and a damaged size made it ask
/// for terabytes, which ends the program (found by fuzzing). Every element must fit in its
/// parent and in the file; only the headers are read.
pub fn sizes_ok(path: &Path, name: &str) -> Result<(), String> {
    let mut f = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    let len = f.get_ref().metadata().map_err(|e| e.to_string())?.len();
    let damaged = || format!("damaged {name} file: an element is larger than the file");
    // (end of the element being walked), from the outside in.
    let mut ends = vec![len];
    let mut pos = 0u64;
    let mut elements = 0u32;
    while let Some(&end) = ends.last() {
        if pos >= end {
            ends.pop();
            continue;
        }
        elements += 1;
        if elements > 1_000_000 {
            return Ok(());
        }
        f.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
        let Some((id, id_width, _)) = vint(&mut f, true) else { return Err(damaged()) };
        let Some((size, size_width, unknown)) = vint(&mut f, false) else { return Err(damaged()) };
        let start = pos + (id_width + size_width) as u64;
        let stop = if unknown { end } else { start.checked_add(size).filter(|&s| s <= end).ok_or_else(damaged)? };
        if u32::try_from(id).is_ok_and(|id| MASTERS.contains(&id)) {
            ends.push(stop);
            pos = start;
        } else {
            pos = stop;
        }
    }
    Ok(())
}

/// One track of a Matroska file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    /// The TrackNumber, which symphonia uses as the track ID.
    pub number: u64,
    /// 1 video, 2 audio, 17 subtitles.
    pub kind: u64,
    /// "A_AAC", "V_MPEG4/ISO/AVC"...
    pub codec_id: String,
    /// Two-letter code ("en") when the file gives one.
    pub language: String,
    pub name: String,
    pub rate: f64,
    pub channels: u64,
    pub bits: Option<u64>,
    pub width: u64,
    pub height: u64,
}

impl Track {
    pub fn audio(&self) -> bool {
        self.kind == 2
    }

    pub fn video(&self) -> bool {
        self.kind == 1
    }
}

/// What a Matroska file holds, from its Info and Tracks elements.
#[derive(Clone, Debug, Default)]
pub struct Contents {
    pub seconds: Option<f64>,
    pub tracks: Vec<Track>,
}

/// Reads the Info and Tracks elements of a Matroska file (before its first cluster, where
/// muxers put them); the clusters are skipped by their size, not read.
pub fn contents(path: &Path) -> Result<Contents, String> {
    let mut f = BufReader::with_capacity(4096, File::open(path).map_err(|e| e.to_string())?);
    let len = f.get_ref().metadata().map_err(|e| e.to_string())?.len();
    let not_matroska = || "not a Matroska file".to_string();
    let header = |f: &mut BufReader<File>| -> Option<(u32, u64, bool)> {
        let (id, _, _) = vint(f, true)?;
        let (size, _, unknown) = vint(f, false)?;
        Some((u32::try_from(id).ok()?, size, unknown))
    };
    let (id, size, _) = header(&mut f).ok_or_else(not_matroska)?;
    if id != EBML {
        return Err(not_matroska());
    }
    f.seek_relative(size as i64).map_err(|e| e.to_string())?;
    let (id, _, _) = header(&mut f).ok_or_else(not_matroska)?;
    if id != SEGMENT {
        return Err(not_matroska());
    }
    let mut out = Contents::default();
    let mut scale = 1_000_000u64;
    let mut duration = None;
    for _ in 0..10_000 {
        let Some((id, size, unknown)) = header(&mut f) else { break };
        match id {
            INFO | TRACKS if !unknown && size <= 16 << 20 => {
                let mut body = vec![0u8; size as usize];
                f.read_exact(&mut body).map_err(|e| e.to_string())?;
                if id == INFO {
                    for (id, data) in children(&body) {
                        match id {
                            0x2AD7B1 => scale = uint(data).max(1),
                            0x4489 => duration = float(data),
                            _ => {}
                        }
                    }
                } else {
                    out.tracks = children(&body).into_iter().filter(|(id, _)| *id == 0xAE).map(|(_, d)| track(d)).collect();
                }
            }
            CLUSTER if !out.tracks.is_empty() => break,
            _ if unknown => break,
            _ => {
                let here = f.stream_position().map_err(|e| e.to_string())?;
                if here.saturating_add(size) >= len {
                    break;
                }
                f.seek_relative(size as i64).map_err(|e| e.to_string())?;
            }
        }
    }
    if out.tracks.is_empty() {
        return Err("no tracks in this Matroska file".into());
    }
    out.seconds = duration.map(|d| d * scale as f64 / 1e9).filter(|s| s.is_finite() && *s > 0.0);
    Ok(out)
}

/// The child elements of an element's data: (ID, data). An element larger than what is
/// left is cut to it.
fn children(mut d: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    while !d.is_empty() {
        let Some((id, _, _)) = vint(&mut d, true) else { break };
        let Some((size, _, _)) = vint(&mut d, false) else { break };
        let size = (size as usize).min(d.len());
        out.push((id as u32, &d[..size]));
        d = &d[size..];
    }
    out
}

fn uint(d: &[u8]) -> u64 {
    d.iter().take(8).fold(0, |acc, &b| acc << 8 | b as u64)
}

fn float(d: &[u8]) -> Option<f64> {
    match d.len() {
        4 => Some(f32::from_be_bytes(d.try_into().ok()?) as f64),
        8 => Some(f64::from_be_bytes(d.try_into().ok()?)),
        _ => None,
    }
}

fn text(d: &[u8]) -> String {
    String::from_utf8_lossy(d).trim_end_matches('\0').trim().to_string()
}

fn track(d: &[u8]) -> Track {
    let mut t = Track { rate: 8000.0, channels: 1, ..Default::default() };
    let mut bcp47 = None;
    for (id, data) in children(d) {
        match id {
            0xD7 => t.number = uint(data),
            0x83 => t.kind = uint(data),
            0x86 => t.codec_id = text(data),
            0x22B59C => t.language = two_letter(&text(data)),
            0x22B59D => bcp47 = Some(two_letter(&text(data))),
            0x536E => t.name = text(data),
            0xE1 => {
                for (id, data) in children(data) {
                    match id {
                        0xB5 => t.rate = float(data).unwrap_or(t.rate),
                        0x9F => t.channels = uint(data),
                        0x6264 => t.bits = Some(uint(data)),
                        _ => {}
                    }
                }
            }
            0xE0 => {
                for (id, data) in children(data) {
                    match id {
                        0xB0 => t.width = uint(data),
                        0xBA => t.height = uint(data),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(language) = bcp47 {
        t.language = language;
    }
    t
}

/// "eng", "fre", "en-US" -> "en"; "und" (undetermined) -> "".
pub fn two_letter(code: &str) -> String {
    let code = code.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
    let two = match code.as_str() {
        "und" | "zxx" | "mis" | "mul" => "",
        "eng" => "en",
        "fre" | "fra" => "fr",
        "ger" | "deu" => "de",
        "spa" => "es",
        "ita" => "it",
        "dut" | "nld" => "nl",
        "por" => "pt",
        "swe" => "sv",
        "dan" => "da",
        "nor" | "nob" | "nno" => "no",
        "fin" => "fi",
        "pol" => "pl",
        "rus" => "ru",
        "cze" | "ces" => "cs",
        "hun" => "hu",
        "gre" | "ell" => "el",
        "tur" => "tr",
        "jpn" => "ja",
        "chi" | "zho" => "zh",
        "kor" => "ko",
        "heb" => "he",
        "ara" => "ar",
        other => other,
    };
    two.to_string()
}

/// A codec ID's usual name: "A_AC3" -> "AC-3", "V_MPEG4/ISO/AVC" -> "H.264".
pub fn codec_name(id: &str) -> String {
    let name = match id {
        "A_AC3" | "A_AC3/BSID9" | "A_AC3/BSID10" => "AC-3",
        "A_EAC3" => "E-AC-3",
        "A_DTS" | "A_DTS/EXPRESS" | "A_DTS/LOSSLESS" => "DTS",
        "A_TRUEHD" => "Dolby TrueHD",
        "A_MLP" => "MLP",
        "A_OPUS" => "Opus",
        "A_VORBIS" => "Vorbis",
        "A_FLAC" => "FLAC",
        "A_ALAC" => "ALAC",
        "A_TTA1" => "TTA",
        "A_WAVPACK4" => "WavPack",
        "A_MPEG/L3" => "MP3",
        "A_MPEG/L2" => "MP2",
        "A_MPEG/L1" => "MP1",
        "A_PCM/INT/LIT" | "A_PCM/INT/BIG" => "PCM",
        "A_PCM/FLOAT/IEEE" => "PCM (float)",
        id if id.starts_with("A_AAC") => "AAC",
        "V_MPEG4/ISO/AVC" => "H.264 (AVC)",
        "V_MPEGH/ISO/HEVC" => "H.265 (HEVC)",
        "V_AV1" => "AV1",
        "V_VP8" => "VP8",
        "V_VP9" => "VP9",
        "V_MPEG2" => "MPEG-2 Video",
        "V_MPEG1" => "MPEG-1 Video",
        "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/SP" | "V_MPEG4/ISO/AP" => "MPEG-4 Visual",
        "V_THEORA" => "Theora",
        other => return other.trim_start_matches("A_").trim_start_matches("V_").to_string(),
    };
    name.to_string()
}

/// A codec that keeps every bit of the sound (whether or not this version decodes it).
pub fn lossless_codec(id: &str) -> bool {
    id.starts_with("A_PCM") || matches!(id, "A_FLAC" | "A_ALAC" | "A_TTA1" | "A_WAVPACK4" | "A_TRUEHD" | "A_MLP" | "A_DTS/LOSSLESS")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An element: its ID bytes, a one-byte size, then the data.
    fn el(id: &[u8], data: &[u8]) -> Vec<u8> {
        assert!(data.len() < 127);
        let mut v = id.to_vec();
        v.push(0x80 | data.len() as u8);
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn tracks_and_length() {
        let audio = [el(&[0xB5], &48000f32.to_be_bytes()), el(&[0x9F], &[2]), el(&[0x62, 0x64], &[16])].concat();
        let sound = [el(&[0xD7], &[2]), el(&[0x83], &[2]), el(&[0x86], b"A_PCM/INT/LIT"), el(&[0x22, 0xB5, 0x9C], b"fre"), el(&[0xE1], &audio)].concat();
        let picture = [el(&[0xB0], &[0x07, 0x80]), el(&[0xBA], &[0x04, 0x38])].concat();
        let video = [el(&[0xD7], &[1]), el(&[0x83], &[1]), el(&[0x86], b"V_MPEG4/ISO/AVC"), el(&[0xE0], &picture)].concat();
        let tracks = el(&[0x16, 0x54, 0xAE, 0x6B], &[el(&[0xAE], &video), el(&[0xAE], &sound)].concat());
        // A length of 90 000 ticks of 1 000 000 ns: 90 s.
        let info = el(&[0x15, 0x49, 0xA9, 0x66], &[el(&[0x2A, 0xD7, 0xB1], &[0x0F, 0x42, 0x40]), el(&[0x44, 0x89], &90000f64.to_be_bytes())].concat());
        let cluster = el(&[0x1F, 0x43, 0xB6, 0x75], &[0xE7, 0x81, 0x00]);
        let segment = el(&[0x18, 0x53, 0x80, 0x67], &[info, tracks, cluster].concat());
        let file = [el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska")), segment].concat();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("show.mkv");
        std::fs::write(&path, file).unwrap();

        let c = contents(&path).unwrap();
        assert_eq!(c.seconds, Some(90.0));
        assert_eq!(c.tracks.len(), 2);
        assert!(c.tracks[0].video() && c.tracks[0].width == 1920 && c.tracks[0].height == 1080);
        let a = &c.tracks[1];
        assert!(a.audio() && a.number == 2 && a.rate == 48000.0 && a.channels == 2 && a.bits == Some(16));
        assert_eq!((a.codec_id.as_str(), a.language.as_str()), ("A_PCM/INT/LIT", "fr"));
        assert!(sizes_ok(&path, "MKV").is_ok());
        assert_eq!(codec_name(&c.tracks[0].codec_id), "H.264 (AVC)");
        assert!(lossless_codec(&a.codec_id) && !lossless_codec("A_AC3"));

        std::fs::write(&path, b"RIFF....WAVE").unwrap();
        assert!(contents(&path).is_err());
    }
}
