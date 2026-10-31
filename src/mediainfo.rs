//! A technical summary in the manner of MediaInfo, as plain text to copy into a forum post
//! or an info file: for the audio files of a show (format, encoder, length, bit rate, tags)
//! and for DVDs (see `dvd`). Only file and folder names appear, never full paths.

use std::path::{Path, PathBuf};

use crate::audio::{self, AudioInfo, Format};
use crate::dvd::{self, Dvd, TitleSet};
use crate::files::file_name;

/// Key column width, as MediaInfo aligns its values.
const KEY: usize = 18;

struct Doc(String);

impl Doc {
    fn heading(&mut self, text: &str, rule: char) {
        if !self.0.is_empty() {
            self.0.push('\n');
        }
        self.0.push_str(text);
        self.0.push('\n');
        self.0.push_str(&rule.to_string().repeat(text.chars().count().max(4)));
        self.0.push('\n');
    }
    fn line(&mut self, key: &str, value: impl AsRef<str>) {
        let value = value.as_ref();
        if !value.is_empty() {
            self.0.push_str(&format!("{key:<KEY$}: {value}\n"));
        }
    }
    fn text(&mut self, text: &str) {
        self.0.push_str(text);
        self.0.push('\n');
    }
}

pub fn size(bytes: u64) -> String {
    let (value, unit) = match bytes {
        b if b >= 1 << 30 => (b as f64 / (1u64 << 30) as f64, "GiB"),
        b if b >= 1 << 20 => (b as f64 / (1u64 << 20) as f64, "MiB"),
        b => (b as f64 / 1024.0, "KiB"),
    };
    format!("{value:.2} {unit}")
}

/// "1 411 kb/s"
fn kbps(value: f64) -> String {
    let digits = format!("{:.0}", value);
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    format!("{out} kb/s")
}

/// "1:52:03" or "4:12"
fn hms(seconds: f64) -> String {
    let total = seconds.round() as u64;
    let (h, m, s) = (total / 3600, total % 3600 / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn khz(rate: u32) -> String {
    format!("{} kHz", audio::trim_float(rate as f64 / 1000.0))
}

/// "1 channel", "2 channels", "5.1 channels"
fn channels(layout: &str) -> String {
    format!("{layout} channel{}", if layout == "1" { "" } else { "s" })
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

// ---------------------------------------------------------------- audio

/// What the summary says about one audio file.
pub struct FileInfo {
    pub path: PathBuf,
    pub info: AudioInfo,
    pub bytes: u64,
    lines: Vec<(&'static str, String)>,
    tags: Vec<(String, String)>,
}

/// Reads one audio file (headers and tags only, quick).
pub fn read_file(path: &Path) -> Result<FileInfo, String> {
    let info = audio::probe(path)?;
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut lines: Vec<(&'static str, String)> = Vec::new();
    let mut tags = audio::source_tags(path);
    let format = match info.format {
        Format::Flac => "FLAC (Free Lossless Audio Codec)".to_string(),
        Format::Wav if info.float => "WAV (PCM, IEEE float)".into(),
        Format::Wav => "WAV (PCM)".into(),
        Format::Ape => "Monkey's Audio (APE)".into(),
        Format::Shn => "Shorten (SHN)".into(),
        Format::WavPack => "WavPack".into(),
        Format::Mp3 => "MPEG Audio Layer 3 (MP3)".into(),
        Format::Mp2 => "MPEG Audio Layer 2 (MP2)".into(),
        Format::Vob => format!("{}, in DVD-Video (VOB)", info.codec.map_or("unknown", audio::Codec::long_name)),
        Format::M4a | Format::Mp4 | Format::Mka | Format::Aiff | Format::Ogg => {
            let container = match info.format {
                Format::M4a => "MPEG-4 (M4A)",
                Format::Mp4 => "MPEG-4 video (MP4), audio track",
                Format::Aiff => "AIFF",
                Format::Ogg => "Ogg",
                _ => "Matroska (MKA)",
            };
            format!("{}, in {container}", info.codec.map_or("unknown", audio::Codec::long_name))
        }
    };
    lines.push(("Format", format));
    match info.format {
        Format::Flac => {
            if let Some(vendor) = audio::flac_vendor(path) {
                lines.push(("Writing library", vendor));
            }
        }
        Format::Ape => {
            if let Ok(decoder) = audio::open_ape(path) {
                let i = decoder.info();
                let level = match i.compression_level {
                    1000 => "Fast",
                    2000 => "Normal",
                    3000 => "High",
                    4000 => "Extra High",
                    5000 => "Insane",
                    _ => "",
                };
                lines.push(("Format version", format!("{}.{:02}", i.version / 1000, i.version % 1000 / 10)));
                if !level.is_empty() {
                    lines.push(("Compression level", level.into()));
                }
            }
        }
        Format::Mp3 | Format::Mp2 => {
            if let Some(mp3) = mpa_details(path) {
                let version = match mp3.header.version {
                    1 => "MPEG-1",
                    2 => "MPEG-2",
                    _ => "MPEG-2.5",
                };
                lines.push(("Format version", format!("{version}, Layer {}", mp3.header.layer)));
                lines.push(("Bit rate mode", mp3.mode.into()));
                if mp3.mode == "CBR" {
                    lines.push(("Bit rate", kbps(mp3.header.kbps as f64)));
                }
                lines.push((
                    "Channel mode",
                    ["Stereo", "Joint stereo", "Dual channel", "Mono"][mp3.header.mode as usize].into(),
                ));
                if let Some(encoder) = mp3.encoder {
                    lines.push(("Writing library", encoder));
                }
                if let Some(preset) = mp3.preset {
                    lines.push(("Encoder settings", preset));
                }
                tags = mp3.tags;
            }
        }
        _ => {}
    }
    Ok(FileInfo { path: path.to_path_buf(), info, bytes, lines, tags })
}

fn write_file(doc: &mut Doc, f: &FileInfo) {
    let i = &f.info;
    doc.heading(&file_name(&f.path), '-');
    for (key, value) in f.lines.iter().take(1) {
        doc.line(key, value);
    }
    doc.line("Compression mode", if i.lossy() { "Lossy" } else if i.format == Format::Wav || i.codec == Some(audio::Codec::Pcm) { "None (uncompressed)" } else { "Lossless" });
    for (key, value) in f.lines.iter().skip(1) {
        doc.line(key, value);
    }
    doc.line("File size", size(f.bytes));
    doc.line("Duration", i.length());
    if i.seconds() > 0.0 {
        let label = if i.format == Format::Mp4 { "Overall bit rate (with the video)" } else { "Overall bit rate" };
        doc.line(label, kbps(f.bytes as f64 * 8.0 / i.seconds() / 1000.0));
    }
    doc.line("Channels", if i.channels == 1 { "1 channel".to_string() } else { format!("{} channels", i.channels) });
    doc.line("Sampling rate", khz(i.rate));
    if !i.lossy() {
        doc.line("Bit depth", format!("{} bits{}", i.bits, if i.float { " (float)" } else { "" }));
    }
    doc.line("Samples", i.samples.to_string());
    if let Some(sbe) = i.sbe() {
        doc.line("CD audio", if sbe { "yes, sector boundary error (length not a multiple of 588 samples)" } else { "yes, no sector boundary error" });
    }
    match i.md5_set() {
        Some(true) => doc.line("Audio MD5", audio::hex(&i.md5.unwrap_or_default())),
        Some(false) => doc.line("Audio MD5", "not set"),
        None => {}
    }
    for (key, label) in [("ARTIST", "Artist"), ("ALBUM", "Album"), ("TITLE", "Title"), ("TRACKNUMBER", "Track"), ("DATE", "Date"), ("GENRE", "Genre")] {
        if let Some((_, value)) = f.tags.iter().find(|(k, _)| k == key) {
            doc.line(label, value.trim());
        }
    }
}

/// The summary of a show's files (or of the selected ones).
pub fn audio_text(show: &Path, files: &[FileInfo], failed: &[(PathBuf, String)]) -> String {
    let mut doc = Doc(String::new());
    doc.heading(&file_name(show), '=');
    let mut formats: Vec<&str> = files.iter().map(|f| f.info.name()).collect();
    formats.dedup();
    formats.sort_unstable();
    formats.dedup();
    doc.line("Files", format!("{} ({})", files.len(), formats.join(", ")));
    let seconds: f64 = files.iter().map(|f| f.info.seconds()).sum();
    let bytes: u64 = files.iter().map(|f| f.bytes).sum();
    doc.line("Total duration", hms(seconds));
    doc.line("Total size", size(bytes));
    if seconds > 0.0 {
        let video = files.iter().any(|f| f.info.format == Format::Mp4);
        doc.line(if video { "Average bit rate (with the video)" } else { "Average bit rate" }, kbps(bytes as f64 * 8.0 / seconds / 1000.0));
    }
    let quality = |i: &AudioInfo| {
        let mut q = format!("{}, {}", khz(i.rate), if i.channels == 1 { "mono".to_string() } else { format!("{} channels", i.channels) });
        if !i.lossy() {
            q = format!("{} bits{}, {q}", i.bits, if i.float { " float" } else { "" });
        }
        q
    };
    let mut qualities: Vec<String> = files.iter().map(|f| quality(&f.info)).collect();
    qualities.sort();
    qualities.dedup();
    let cd = !files.is_empty() && files.iter().all(|f| f.info.is_cd());
    doc.line("Audio", if qualities.len() == 1 { format!("{}{}", qualities[0], if cd { " (CD audio)" } else { "" }) } else { format!("mixed: {}", qualities.join("; ")) });
    if cd && files.len() > 1 {
        // The last track may end anywhere.
        let sbe = files[..files.len() - 1].iter().filter(|f| f.info.sbe() == Some(true)).count();
        doc.line("Sector boundaries", match sbe {
            0 => "no error (last track not counted)".to_string(),
            n => format!("{} not on a sector boundary (last track not counted)", plural(n, "track")),
        });
    }
    for (path, e) in failed {
        doc.line("Unreadable", format!("{}: {e}", file_name(path)));
    }
    for f in files {
        write_file(&mut doc, f);
    }
    footer(&mut doc);
    doc.0
}

fn footer(doc: &mut Doc) {
    doc.text(&format!("\n(Media info, Bootleg Toolbox {})", crate::ops::APP_VERSION));
}

// ---------------------------------------------------------------- MP3 and MP2 files

struct MpaDetails {
    header: dvd::MpaHeader,
    /// "CBR", "VBR", "ABR".
    mode: &'static str,
    encoder: Option<String>,
    /// LAME's VBR quality ("-V 0").
    preset: Option<String>,
    tags: Vec<(String, String)>,
}

fn syncsafe(b: &[u8]) -> usize {
    b.iter().fold(0usize, |acc, &x| acc << 7 | (x & 0x7f) as usize)
}

fn mpa_details(path: &Path) -> Option<MpaDetails> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let mut tags = Vec::new();
    let mut head = [0u8; 10];
    file.read_exact(&mut head).ok()?;
    if head.starts_with(b"ID3") {
        // The tag may hold pictures: read its text, then go past it for the audio.
        let size = syncsafe(&head[6..10]);
        let mut body = Vec::new();
        (&mut file).take(size.min(16 << 20) as u64).read_to_end(&mut body).ok()?;
        tags = id3v2_tags(head[3], head[5], &body);
        let footer = if head[5] & 0x10 != 0 { 10 } else { 0 };
        file.seek(SeekFrom::Start((10 + size + footer) as u64)).ok()?;
    } else {
        file.seek(SeekFrom::Start(0)).ok()?;
    }
    let mut data = Vec::new();
    file.take(256 << 10).read_to_end(&mut data).ok()?;
    // The first frame whose follower is a frame too.
    let (at, header) = (0..data.len().saturating_sub(4)).find_map(|i| {
        let h = dvd::mpa_header(&data[i..])?;
        dvd::mpa_header(data.get(i + h.frame_len..)?).map(|_| (i, h))
    })?;
    let side = match (header.version == 1, header.mode == 3) {
        (true, false) => 32,
        (true, true) | (false, false) => 17,
        (false, true) => 9,
    };
    let mut mode = "CBR";
    let mut encoder = None;
    let mut preset = None;
    let xing = at + 4 + side;
    match data.get(xing..xing + 4) {
        Some(tag @ (b"Xing" | b"Info")) => {
            if tag == b"Xing" {
                mode = "VBR";
            }
            let flags = u32::from_be_bytes(data.get(xing + 4..xing + 8)?.try_into().ok()?);
            let mut pos = xing + 8;
            let mut quality = None;
            for (bit, len) in [(1, 4), (2, 4), (4, 100), (8, 4)] {
                if flags & bit != 0 {
                    if bit == 8 {
                        quality = data.get(pos..pos + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
                    }
                    pos += len;
                }
            }
            if let Some(name) = data.get(pos..pos + 9) {
                let name = String::from_utf8_lossy(name).trim_end_matches(['\0', ' ']).to_string();
                if name.starts_with("LAME") || name.starts_with("Lavc") || name.starts_with("Lavf") {
                    // LAME's own note of how it was run.
                    match data.get(pos + 9).map(|b| b & 0x0f) {
                        Some(1 | 8) => mode = "CBR",
                        Some(2 | 9) => mode = "ABR",
                        Some(3..=6) => {
                            mode = "VBR";
                            // LAME stores 100 - 10 x V - q.
                            if let Some(q) = quality.filter(|q| *q <= 100) {
                                preset = Some(format!("-V {}", (100 - q) / 10));
                            }
                        }
                        _ => {}
                    }
                    encoder = Some(name.replacen("LAME", "LAME ", 1));
                }
            }
        }
        _ if data.get(at + 36..at + 40) == Some(b"VBRI") => mode = "VBR",
        _ => {}
    }
    Some(MpaDetails { header, mode, encoder, preset, tags })
}

/// The text frames of an ID3v2.3/2.4 tag, as Vorbis comment names.
fn id3v2_tags(version: u8, flags: u8, body: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if !(3..=4).contains(&version) {
        return out;
    }
    let mut pos = 0;
    if flags & 0x40 != 0 {
        // Extended header: its size counts itself in 2.4, not in 2.3.
        let Some(b) = body.get(0..4) else { return out };
        pos = if version == 4 { syncsafe(b) } else { 4 + u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize };
    }
    while let Some(h) = body.get(pos..pos + 10) {
        if h[0] == 0 {
            break;
        }
        let id = String::from_utf8_lossy(&h[..4]).to_string();
        let len = if version == 4 { syncsafe(&h[4..8]) } else { u32::from_be_bytes([h[4], h[5], h[6], h[7]]) as usize };
        let Some(content) = body.get(pos + 10..pos + 10 + len) else { break };
        pos += 10 + len;
        let name = match id.as_str() {
            "TPE1" => "ARTIST",
            "TALB" => "ALBUM",
            "TIT2" => "TITLE",
            "TRCK" => "TRACKNUMBER",
            "TYER" | "TDRC" => "DATE",
            "TCON" => "GENRE",
            _ => continue,
        };
        if let Some((&encoding, text)) = content.split_first() {
            let value = id3_text(encoding, text);
            if !value.is_empty() {
                out.push((name.to_string(), value));
            }
        }
    }
    out
}

fn id3_text(encoding: u8, b: &[u8]) -> String {
    let utf16 = |b: &[u8], big: bool| {
        let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|&c| if big { u16::from_be_bytes(c) } else { u16::from_le_bytes(c) }).collect();
        String::from_utf16_lossy(&units)
    };
    let text = match encoding {
        0 => b.iter().map(|&c| c as char).collect(),
        1 => match b {
            [0xfe, 0xff, rest @ ..] => utf16(rest, true),
            [0xff, 0xfe, rest @ ..] => utf16(rest, false),
            _ => utf16(b, false),
        },
        2 => utf16(b, true),
        _ => String::from_utf8_lossy(b).into_owned(),
    };
    // Several values are separated by NULs; the tag ends with one.
    text.trim_end_matches('\0').replace('\0', " / ").trim().to_string()
}

// ---------------------------------------------------------------- DVD

pub fn language_name(code: &str) -> String {
    let name = match code {
        "en" => "English",
        "fr" => "French",
        "de" => "German",
        "es" => "Spanish",
        "it" => "Italian",
        "nl" => "Dutch",
        "pt" => "Portuguese",
        "sv" => "Swedish",
        "da" => "Danish",
        "no" => "Norwegian",
        "fi" => "Finnish",
        "pl" => "Polish",
        "ru" => "Russian",
        "cs" => "Czech",
        "hu" => "Hungarian",
        "el" => "Greek",
        "tr" => "Turkish",
        "ja" => "Japanese",
        "zh" => "Chinese",
        "ko" => "Korean",
        "he" => "Hebrew",
        "ar" => "Arabic",
        "" => return String::new(),
        other => return other.to_string(),
    };
    name.to_string()
}

/// The codec with its usual name and whether it loses information.
fn codec(coding: &str) -> String {
    match coding {
        "AC-3" => "AC-3 (Dolby Digital, lossy)".into(),
        "DTS" => "DTS (lossy)".into(),
        "LPCM" => "LPCM (uncompressed PCM, lossless)".into(),
        "unknown" => "unknown codec".into(),
        other => format!("{other} (lossy)"),
    }
}

fn vob_range(set: &TitleSet) -> String {
    let first = file_name(&set.vobs[0]);
    match set.vobs.len() {
        1 => first,
        n => format!("{first} to {}, {n} files", file_name(&set.vobs[n - 1])),
    }
}

pub fn dvd_text(d: &Dvd) -> String {
    let mut doc = Doc(String::new());
    doc.heading(&d.name, '=');
    let is_dvd = d.sets.iter().any(|s| s.number > 0);
    // DVD-5 holds 4.7 billion bytes, DVD-9 8.5 billion.
    let kind = match d.total_bytes {
        _ if !is_dvd => "VOB files (no DVD structure)",
        b if b <= 4_700_372_992 => "DVD-Video, fits a DVD-5 (single layer)",
        b if b <= 8_543_666_176 => "DVD-Video, needs a DVD-9 (dual layer)",
        _ => "DVD-Video, larger than a DVD-9",
    };
    doc.line("Type", kind);
    doc.line("Total size", size(d.total_bytes));
    if is_dvd {
        doc.line("Title sets", d.sets.iter().filter(|s| s.number > 0).count().to_string());
        let main = d
            .sets
            .iter()
            .filter_map(|s| match &s.ifo {
                Some(Ok(ifo)) => ifo.titles.iter().enumerate().map(|(i, t)| (s.number, i + 1, t)).max_by(|a, b| a.2.seconds.total_cmp(&b.2.seconds)),
                _ => None,
            })
            .max_by(|a, b| a.2.seconds.total_cmp(&b.2.seconds));
        if let Some((set, title, pgc)) = main {
            doc.line("Main title", format!("title set {set}, title {title}: {}, {}", hms(pgc.seconds), plural(pgc.chapters as usize, "chapter")));
        }
    }
    for set in &d.sets {
        write_set(&mut doc, set);
    }
    footer(&mut doc);
    doc.0
}

fn write_set(doc: &mut Doc, set: &TitleSet) {
    let heading = if set.number > 0 { format!("Title set {} ({})", set.number, vob_range(set)) } else { file_name(&set.vobs[0]) };
    doc.heading(&heading, '-');
    let ifo = match &set.ifo {
        Some(Ok(ifo)) => Some(ifo),
        Some(Err(e)) => {
            doc.line("IFO", format!("unreadable ({e})"));
            None
        }
        None => None,
    };
    let scan = match &set.scan {
        Ok(scan) => Some(scan),
        Err(e) => {
            doc.line("VOB", format!("unreadable ({e})"));
            None
        }
    };
    doc.line("Size", size(set.vob_bytes));
    let seconds = set.seconds();
    if let Some(s) = seconds {
        doc.line("Duration", if ifo.is_some() { hms(s) } else { format!("{} (from the timestamps)", hms(s)) });
    }
    let overall = seconds.filter(|s| *s > 0.0).map(|s| set.vob_bytes as f64 * 8.0 / s / 1000.0);
    if let Some(o) = overall {
        doc.line("Overall bit rate", kbps(o));
    }

    // Video: the stream's own header first, the IFO otherwise.
    let video = scan.and_then(|s| s.video.as_ref());
    let standard = ifo.map(|i| i.video.standard).or_else(|| {
        video.and_then(|v| match v.height {
            576 | 288 => Some("PAL"),
            480 | 240 => Some("NTSC"),
            _ => None,
        })
    });
    let mut parts = Vec::new();
    match (video, ifo) {
        (Some(v), _) => {
            parts.push(match &v.profile {
                Some(p) => format!("MPEG-{} {p}", if v.mpeg2 { 2 } else { 1 }),
                None => format!("MPEG-{}", if v.mpeg2 { 2 } else { 1 }),
            });
            parts.push(format!("{}x{}", v.width, v.height));
            if let Some(s) = standard {
                parts.push(s.to_string());
            }
            if v.fps > 0.0 {
                parts.push(format!("{} fps", audio::trim_float((v.fps * 1000.0).round() / 1000.0)));
            }
            parts.push(ifo.map(|i| i.video.aspect).unwrap_or(v.aspect).to_string());
            if let Some(p) = v.progressive {
                parts.push(if p { "progressive" } else { "interlaced" }.into());
            }
            if let Some(c) = v.chroma {
                parts.push(c.into());
            }
        }
        (None, Some(i)) => {
            let v = &i.video;
            parts.push(format!("MPEG-{}", v.mpeg));
            parts.push(format!("{}x{}", v.width, v.height));
            parts.push(v.standard.into());
            parts.push(v.aspect.into());
        }
        (None, None) => {}
    }
    if ifo.is_some_and(|i| i.video.letterboxed) {
        parts.push("letterboxed".into());
    }
    doc.line("Video", parts.join(", "));

    // Audio: the IFO gives languages, the streams their bit rates and layouts.
    let tracks = dvd::dvd_audio(set);
    let mut audio_kbps = Some(0.0);
    let mut lines = Vec::new();
    let mut lossless = Vec::new();
    for t in &tracks {
        let mut p = vec![codec(t.coding), channels(&t.channels), khz(t.rate)];
        lossless.push(t.lossless());
        if let Some(bits) = t.bits {
            p.push(format!("{bits} bits"));
        }
        match t.kbps {
            Some(k) => {
                p.push(kbps(k));
                audio_kbps = audio_kbps.map(|a| a + k);
            }
            None => audio_kbps = None,
        }
        if !t.language.is_empty() {
            p.push(language_name(&t.language));
        }
        if !t.purpose.is_empty() {
            p.push(t.purpose.into());
        }
        lines.push(p.join(", "));
    }
    // The video's share: what the audio leaves of the overall rate (packets cost ~2%).
    if let (Some(o), Some(a)) = (overall, audio_kbps) {
        let v = o * 0.98 - a;
        if v > 0.0 {
            let max = video.map(|v| format!(" (maximum announced: {})", kbps(v.max_kbps))).unwrap_or_default();
            doc.line("Video bit rate", format!("about {}{max}", kbps(v)));
        }
    }
    if lines.is_empty() {
        doc.line("Audio", "none");
    }
    for (n, line) in lines.iter().enumerate() {
        doc.line(&if lines.len() == 1 { "Audio".to_string() } else { format!("Audio {}", n + 1) }, line);
    }
    // What extracting the audio can give.
    let pcm: Vec<String> = lossless.iter().enumerate().filter(|(_, l)| **l).map(|(n, _)| (n + 1).to_string()).collect();
    if !lines.is_empty() {
        doc.line("Audio extraction", match (pcm.len(), lines.len()) {
            (0, _) => "lossy only: keep the original stream (a FLAC of it is not lossless)".to_string(),
            (_, 1) => "lossless (LPCM): extracts to FLAC without any loss".to_string(),
            (_, _) => format!("track {} lossless (LPCM): extracts to FLAC without any loss", pcm.join(", ")),
        });
    }

    let subtitles = match ifo {
        Some(i) if !i.subtitles.is_empty() => {
            i.subtitles.iter().map(|l| if l.is_empty() { "unnamed".to_string() } else { language_name(l) }).collect::<Vec<_>>().join(", ")
        }
        Some(_) => "none".into(),
        None => match scan.map_or(0, |s| s.subpictures) {
            0 => "none".into(),
            n => plural(n, "stream"),
        },
    };
    doc.line("Subtitles", subtitles);

    if let Some(i) = ifo {
        doc.line("Titles", i.titles.len().to_string());
        const SHOWN: usize = 30;
        for (n, t) in i.titles.iter().enumerate().take(SHOWN) {
            doc.line(&format!("  Title {}", n + 1), format!("{}, {}", hms(t.seconds), plural(t.chapters as usize, "chapter")));
        }
        if i.titles.len() > SHOWN {
            doc.text(&format!("  ... and {} more", i.titles.len() - SHOWN));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_formats() {
        assert_eq!(kbps(1411.2), "1 411 kb/s");
        assert_eq!(kbps(448.0), "448 kb/s");
        assert_eq!(hms(6723.4), "1:52:03");
        assert_eq!(hms(252.0), "4:12");
    }

    #[test]
    fn id3_frames() {
        // ID3v2.3: TPE1 "Band" in Latin-1, TIT2 in UTF-16 with BOM.
        let mut body = Vec::new();
        body.extend_from_slice(b"TPE1\0\0\0\x05\0\0\0Band");
        body.extend_from_slice(b"TIT2\0\0\0\x07\0\0\x01\xff\xfeA\0b\0");
        let tags = id3v2_tags(3, 0, &body);
        assert_eq!(tags, vec![("ARTIST".into(), "Band".into()), ("TITLE".into(), "Ab".into())]);
    }
}
