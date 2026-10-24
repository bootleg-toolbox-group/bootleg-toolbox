//! Audio file inspection and decoding, with no external tool.
//!
//! FLAC and WAV are handled natively. APE, SHN and WavPack are recognised but
//! not decoded yet (planned for step 2).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use md5::{Digest, Md5};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const ZERO_MD5: [u8; 16] = [0; 16];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Flac,
    Wav,
    Ape,
    Shn,
    WavPack,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "flac" => Some(Format::Flac),
            "wav" => Some(Format::Wav),
            "ape" => Some(Format::Ape),
            "shn" => Some(Format::Shn),
            "wv" => Some(Format::WavPack),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Flac => "FLAC",
            Format::Wav => "WAV",
            Format::Ape => "APE",
            Format::Shn => "SHN",
            Format::WavPack => "WV",
        }
    }

    /// Formats this version can decode.
    pub fn supported(self) -> bool {
        matches!(self, Format::Flac | Format::Wav | Format::Shn | Format::Ape | Format::WavPack)
    }
}

#[derive(Clone, Debug)]
pub struct AudioInfo {
    pub format: Format,
    pub rate: u32,
    pub bits: u32,
    pub channels: u32,
    pub samples: u64,
    /// FLAC only: MD5 signature stored in the STREAMINFO block.
    pub md5: Option<[u8; 16]>,
    /// WAV only: IEEE float samples instead of integers.
    pub float: bool,
}

impl AudioInfo {
    pub fn is_cd(&self) -> bool {
        self.rate == 44100 && self.bits == 16 && self.channels == 2
    }

    /// Some(true/false) for CD audio, None when the notion does not apply.
    pub fn sbe(&self) -> Option<bool> {
        self.is_cd().then(|| self.samples % 588 != 0)
    }

    pub fn md5_set(&self) -> Option<bool> {
        self.md5.map(|m| m != ZERO_MD5)
    }

    pub fn seconds(&self) -> f64 {
        if self.rate == 0 {
            0.0
        } else {
            self.samples as f64 / self.rate as f64
        }
    }

    /// m:ss.ff (CD frames) for CD audio, m:ss.mmm otherwise.
    pub fn length(&self) -> String {
        if self.rate == 0 {
            return String::new();
        }
        let rate = self.rate as u64;
        let whole = self.samples / rate;
        let rem = self.samples % rate;
        let (m, s) = (whole / 60, whole % 60);
        if self.is_cd() {
            format!("{m}:{s:02}.{:02}", rem * 75 / rate)
        } else {
            format!("{m}:{s:02}.{:03}", rem * 1000 / rate)
        }
    }

    pub fn rate_label(&self) -> String {
        format!("{} kHz", trim_float(self.rate as f64 / 1000.0))
    }

    /// "FLAC 16/44.1"
    pub fn label(&self) -> String {
        format!("{} {}/{}", self.format.name(), self.bits, trim_float(self.rate as f64 / 1000.0))
    }
}

pub fn trim_float(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    text.to_string()
}

/// "1h28m48s" / "59m33s", like the folder names of the collection.
pub fn fmt_total(seconds: f64) -> String {
    let total = seconds.round() as u64;
    let (h, m, s) = (total / 3600, total % 3600 / 60, total % 60);
    if h > 0 {
        format!("{h}h{m:02}m{s:02}s")
    } else {
        format!("{m}m{s:02}s")
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------- probing

pub fn probe(path: &Path) -> Result<AudioInfo, String> {
    probe_as(path, Format::from_path(path))
}

/// As `probe`, the format given rather than taken from the extension (".part" files).
pub fn probe_as(path: &Path, format: Option<Format>) -> Result<AudioInfo, String> {
    match format {
        Some(Format::Flac) => probe_flac(path),
        Some(Format::Shn) => crate::shn::probe(path).map(|h| AudioInfo {
            format: Format::Shn,
            rate: h.rate,
            bits: h.bits,
            channels: h.channels,
            samples: h.samples,
            md5: None,
            float: false,
        }),
        Some(Format::Wav) => probe_wav(path).map(|(info, _, _)| info),
        Some(Format::Ape) => {
            let decoder = open_ape(path)?;
            let i = decoder.info();
            Ok(AudioInfo {
                format: Format::Ape,
                rate: i.sample_rate,
                bits: i.bits_per_sample as u32,
                channels: i.channels as u32,
                samples: i.total_samples,
                md5: None,
                float: i.is_floating_point,
            })
        }
        Some(Format::WavPack) => crate::wv::probe(path).map(|w| AudioInfo {
            format: Format::WavPack,
            rate: w.rate,
            bits: w.bits,
            channels: w.channels,
            samples: w.samples,
            md5: None,
            float: w.float,
        }),
        None => Err("not an audio file".into()),
    }
}

fn io_err(e: std::io::Error) -> String {
    e.to_string()
}

fn probe_flac(path: &Path) -> Result<AudioInfo, String> {
    let mut f = BufReader::new(File::open(path).map_err(io_err)?);
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).map_err(|_| "file too short".to_string())?;
    if &magic[..3] == b"ID3" {
        // Skip an ID3v2 tag some tools put in front of FLAC files.
        let mut rest = [0u8; 6];
        f.read_exact(&mut rest).map_err(io_err)?;
        let size = rest[2..6].iter().fold(0u64, |acc, b| (acc << 7) | (*b as u64 & 0x7f));
        let footer = if rest[1] & 0x10 != 0 { 10 } else { 0 };
        f.seek(SeekFrom::Start(10 + size + footer)).map_err(io_err)?;
        f.read_exact(&mut magic).map_err(io_err)?;
    }
    if &magic != b"fLaC" {
        return Err("not a valid FLAC file (no fLaC marker)".into());
    }
    let mut header = [0u8; 4];
    f.read_exact(&mut header).map_err(io_err)?;
    if header[0] & 0x7f != 0 {
        return Err("first FLAC metadata block is not STREAMINFO".into());
    }
    let mut si = [0u8; 34];
    f.read_exact(&mut si).map_err(|_| "truncated STREAMINFO".to_string())?;
    let v = u64::from_be_bytes(si[10..18].try_into().unwrap());
    let mut md5 = [0u8; 16];
    md5.copy_from_slice(&si[18..34]);
    Ok(AudioInfo {
        format: Format::Flac,
        rate: (v >> 44) as u32,
        channels: ((v >> 41) & 0x7) as u32 + 1,
        bits: ((v >> 36) & 0x1f) as u32 + 1,
        samples: v & 0xF_FFFF_FFFF,
        md5: Some(md5),
        float: false,
    })
}

/// Returns the info plus the offset and the readable length of the WAV data chunk.
/// `info.samples` is what the header announces: a truncated file has fewer
/// readable bytes, which `read_pcm` reports.
pub fn probe_wav(path: &Path) -> Result<(AudioInfo, u64, u64), String> {
    let mut f = BufReader::new(File::open(path).map_err(io_err)?);
    let file_len = f.get_ref().metadata().map_err(io_err)?.len();
    let mut riff = [0u8; 12];
    f.read_exact(&mut riff).map_err(|_| "file too short".to_string())?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let (mut channels, mut rate, mut bits, mut block_align) = (0u32, 0u32, 0u32, 0u32);
    let mut float = false;
    let mut pos = 12u64;
    loop {
        let mut chunk = [0u8; 8];
        if f.read_exact(&mut chunk).is_err() {
            return Err("no data chunk found".into());
        }
        let size = u32::from_le_bytes(chunk[4..8].try_into().unwrap()) as u64;
        pos += 8;
        match &chunk[0..4] {
            b"fmt " => {
                let mut fmt = vec![0u8; size.min(64) as usize];
                f.read_exact(&mut fmt).map_err(io_err)?;
                if fmt.len() < 16 {
                    return Err("invalid fmt chunk".into());
                }
                channels = u16::from_le_bytes([fmt[2], fmt[3]]) as u32;
                rate = u32::from_le_bytes(fmt[4..8].try_into().unwrap());
                block_align = u16::from_le_bytes([fmt[12], fmt[13]]) as u32;
                bits = u16::from_le_bytes([fmt[14], fmt[15]]) as u32;
                // WAVE_FORMAT_IEEE_FLOAT, directly or as the sub-format of WAVE_FORMAT_EXTENSIBLE
                let mut tag = u16::from_le_bytes([fmt[0], fmt[1]]);
                if tag == 0xFFFE && fmt.len() >= 26 {
                    tag = u16::from_le_bytes([fmt[24], fmt[25]]);
                }
                float = tag == 3;
                let skip = size + (size & 1) - fmt.len() as u64;
                f.seek(SeekFrom::Current(skip as i64)).map_err(io_err)?;
            }
            b"data" => {
                if block_align == 0 {
                    return Err("data chunk before fmt chunk".into());
                }
                let available = file_len.saturating_sub(pos);
                // 0 and 0xFFFFFFFF: size never filled in (streamed or >4 GB recordings)
                let declared = if size == 0 || size == 0xFFFF_FFFF { available } else { size };
                let len = declared.min(available);
                let samples = declared / block_align as u64;
                let info = AudioInfo { format: Format::Wav, rate, bits, channels, samples, md5: None, float };
                return Ok((info, pos, len));
            }
            _ => {
                f.seek(SeekFrom::Current((size + (size & 1)) as i64)).map_err(io_err)?;
            }
        }
        pos += size + (size & 1);
    }
}

// ---------------------------------------------------------------- decoding

/// Feeds the raw PCM data (little-endian signed, as in a WAV data chunk) to `sink`.
/// For FLAC the samples are decoded; for WAV the data chunk is read as is.
/// Returns the number of sample frames.
pub fn read_pcm(path: &Path, cancel: &AtomicBool, sink: impl FnMut(&[u8])) -> Result<u64, String> {
    read_pcm_as(path, Format::from_path(path), cancel, sink)
}

/// As `read_pcm`, the format given rather than taken from the extension.
pub fn read_pcm_as(
    path: &Path,
    format: Option<Format>,
    cancel: &AtomicBool,
    mut sink: impl FnMut(&[u8]),
) -> Result<u64, String> {
    match format {
        Some(Format::Wav) => {
            let (info, offset, len) = probe_wav(path)?;
            let block_align = (info.channels * info.bits.div_ceil(8)).max(1) as u64;
            let mut f = File::open(path).map_err(io_err)?;
            f.seek(SeekFrom::Start(offset)).map_err(io_err)?;
            let mut left = len;
            let mut buf = vec![0u8; 1 << 20];
            while left > 0 {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                let want = left.min(buf.len() as u64) as usize;
                let got = f.read(&mut buf[..want]).map_err(io_err)?;
                if got == 0 {
                    return Err("unexpected end of file".into());
                }
                sink(&buf[..got]);
                left -= got as u64;
            }
            Ok(len / block_align)
        }
        Some(Format::Flac) => decode_flac(path, cancel, sink),
        Some(Format::Shn) => crate::shn::decode(path, cancel, sink),
        Some(Format::Ape) => decode_ape(path, cancel, sink),
        Some(Format::WavPack) => crate::wv::decode(path, cancel, sink),
        None => Err("not an audio file".into()),
    }
}

fn decode_flac(path: &Path, cancel: &AtomicBool, mut sink: impl FnMut(&[u8])) -> Result<u64, String> {
    let file = File::open(path).map_err(io_err)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("flac");
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("cannot open FLAC stream: {e}"))?;
    let mut reader = probed.format;
    let track = reader.default_track().ok_or("no audio track")?.clone();
    let bits = track.codec_params.bits_per_sample.ok_or("unknown bit depth")?;
    let width = bits.div_ceil(8) as usize;
    let shift = 32 - bits;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions { verify: false })
        .map_err(|e| format!("cannot create decoder: {e}"))?;
    let mut frames = 0u64;
    let mut out: Vec<u8> = Vec::with_capacity(1 << 16);
    let mut sample_buf: Option<SampleBuffer<i32>> = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let packet = match reader.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("read error: {e}")),
        };
        if packet.track_id() != track.id {
            continue;
        }
        let decoded = decoder.decode(&packet).map_err(|e| format!("decoding error: {e}"))?;
        let spec = *decoded.spec();
        let needed = decoded.capacity() as u64;
        let channels = spec.channels.count();
        if sample_buf.as_ref().map_or(true, |b| b.capacity() < needed as usize * channels) {
            sample_buf = Some(SampleBuffer::<i32>::new(needed, spec));
        }
        let buf = sample_buf.as_mut().unwrap();
        buf.copy_interleaved_ref(decoded);
        out.clear();
        for &s in buf.samples() {
            let v = s >> shift;
            out.extend_from_slice(&v.to_le_bytes()[..width]);
        }
        frames += (buf.samples().len() / spec.channels.count()) as u64;
        sink(&out);
    }
    Ok(frames)
}

fn open_ape(path: &Path) -> Result<ape_decoder::ApeDecoder<BufReader<File>>, String> {
    let file = BufReader::new(File::open(path).map_err(io_err)?);
    ape_decoder::ApeDecoder::new(file).map_err(|e| match e {
        ape_decoder::ApeError::UnsupportedVersion(v) => {
            format!("Monkey's Audio {}.{:02} files are not supported (3.95 and later only)", v / 1000, v % 1000 / 10)
        }
        other => other.to_string(),
    })
}

/// Tags of a source file as Vorbis comments, to keep them when converting to FLAC.
/// FLAC carries Vorbis comments, Monkey's Audio and WavPack APEv2 tags; WAV and SHN none.
pub fn source_tags(path: &Path) -> Vec<(String, String)> {
    match Format::from_path(path) {
        Some(Format::Flac) => return flac_tags(path).unwrap_or_default(),
        Some(Format::Ape | Format::WavPack) => {}
        _ => return Vec::new(),
    }
    let Ok(mut file) = File::open(path) else { return Vec::new() };
    let Ok(Some(tag)) = ape_decoder::read_tag(&mut file) else { return Vec::new() };
    let text: Vec<(String, String)> = tag
        .fields
        .iter()
        .filter(|f| f.field_type() == ape_decoder::TagFieldType::TextUtf8)
        .filter_map(|f| f.value_as_str().map(|v| (f.name.clone(), v.to_string())))
        .collect();
    ape_to_vorbis(&text)
}

/// The VORBIS_COMMENT block of a FLAC file ("ARTIST", "Some name").
fn flac_tags(path: &Path) -> Result<Vec<(String, String)>, String> {
    let mut f = BufReader::new(File::open(path).map_err(io_err)?);
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).map_err(io_err)?;
    if &magic[..3] == b"ID3" {
        let mut rest = [0u8; 6];
        f.read_exact(&mut rest).map_err(io_err)?;
        let size = rest[2..6].iter().fold(0u64, |acc, b| (acc << 7) | (*b as u64 & 0x7f));
        let footer = if rest[1] & 0x10 != 0 { 10 } else { 0 };
        f.seek(SeekFrom::Start(10 + size + footer)).map_err(io_err)?;
        f.read_exact(&mut magic).map_err(io_err)?;
    }
    if &magic != b"fLaC" {
        return Err("not a FLAC file".into());
    }
    loop {
        let mut header = [0u8; 4];
        f.read_exact(&mut header).map_err(io_err)?;
        let len = u32::from_be_bytes([0, header[1], header[2], header[3]]) as usize;
        if header[0] & 0x7f == 4 {
            let mut data = vec![0u8; len];
            f.read_exact(&mut data).map_err(io_err)?;
            return Ok(parse_vorbis_comment(&data));
        }
        if header[0] & 0x80 != 0 {
            return Ok(Vec::new());
        }
        f.seek(SeekFrom::Current(len as i64)).map_err(io_err)?;
    }
}

fn parse_vorbis_comment(d: &[u8]) -> Vec<(String, String)> {
    let u32le = |i: usize| d.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let mut out = Vec::new();
    let Some(vendor) = u32le(0) else { return out };
    let mut pos = 4 + vendor;
    let Some(count) = u32le(pos) else { return out };
    pos += 4;
    for _ in 0..count.min(10_000) {
        let Some(len) = u32le(pos) else { break };
        let Some(entry) = d.get(pos + 4..pos + 4 + len) else { break };
        pos += 4 + len;
        if let Some((k, v)) = String::from_utf8_lossy(entry).split_once('=') {
            out.push((k.to_uppercase(), v.to_string()));
        }
    }
    out
}

/// APEv2 item names to Vorbis comment names; values with several entries (NUL-separated)
/// become several comments.
fn ape_to_vorbis(items: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, value) in items {
        let key = match name.to_lowercase().as_str() {
            "year" => "DATE".to_string(),
            "track" => "TRACKNUMBER".to_string(),
            "disc" => "DISCNUMBER".to_string(),
            "album artist" | "albumartist" => "ALBUMARTIST".to_string(),
            _ => name.to_uppercase(),
        };
        // Vorbis names: printable ASCII without '='.
        if key.is_empty() || !key.chars().all(|c| (' '..='}').contains(&c) && c != '=') {
            continue;
        }
        for v in value.split('\0').map(str::trim).filter(|v| !v.is_empty()) {
            // "3/12": number and total.
            if let (true, Some((n, total))) = (key == "TRACKNUMBER" || key == "DISCNUMBER", v.split_once('/')) {
                out.push((key.clone(), n.trim().to_string()));
                let total_key = if key == "TRACKNUMBER" { "TRACKTOTAL" } else { "DISCTOTAL" };
                if !total.trim().is_empty() {
                    out.push((total_key.to_string(), total.trim().to_string()));
                }
            } else {
                out.push((key.clone(), v.to_string()));
            }
        }
    }
    out
}

/// Monkey's Audio: decoded frame by frame (each frame's CRC is checked), as in a WAV
/// data chunk (little-endian; files made from AIFF are big-endian and get swapped).
fn decode_ape(path: &Path, cancel: &AtomicBool, mut sink: impl FnMut(&[u8])) -> Result<u64, String> {
    let mut decoder = open_ape(path)?;
    let (big_endian, width, channels) = {
        let i = decoder.info();
        (i.is_big_endian, (i.bits_per_sample as usize).div_ceil(8), i.channels.max(1) as usize)
    };
    let mut bytes = 0u64;
    for frame in decoder.frames() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let mut pcm = frame.map_err(|e| format!("damaged data: {e}"))?;
        if big_endian && width > 1 {
            for sample in pcm.chunks_exact_mut(width) {
                sample.reverse();
            }
        }
        bytes += pcm.len() as u64;
        sink(&pcm);
    }
    Ok(bytes / (width * channels) as u64)
}

/// MD5 of the decoded audio data: the ST5 fingerprint, and for FLAC also the
/// value its STREAMINFO MD5 signature must match.
pub fn pcm_md5(path: &Path, cancel: &AtomicBool) -> Result<([u8; 16], u64), String> {
    let mut hasher = Md5::new();
    let frames = read_pcm(path, cancel, |chunk| hasher.update(chunk))?;
    Ok((hasher.finalize().into(), frames))
}

/// MD5 of the whole file (classic .md5 files).
pub fn file_md5(path: &Path, cancel: &AtomicBool) -> Result<[u8; 16], String> {
    let mut f = File::open(path).map_err(io_err)?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let n = f.read(&mut buf).map_err(io_err)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ape_tags_to_vorbis() {
        let items: Vec<(String, String)> = [("Artist", "Iron Maiden"), ("Year", "1988"), ("Track", "3/12"), ("Album Artist", "IM"), ("Comment", "a\0b"), ("Bad=Name", "x")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let v = ape_to_vorbis(&items);
        let get = |k: &str| v.iter().filter(|(a, _)| a == k).map(|(_, b)| b.as_str()).collect::<Vec<_>>();
        assert_eq!(get("ARTIST"), ["Iron Maiden"]);
        assert_eq!(get("DATE"), ["1988"]);
        assert_eq!(get("TRACKNUMBER"), ["3"]);
        assert_eq!(get("TRACKTOTAL"), ["12"]);
        assert_eq!(get("ALBUMARTIST"), ["IM"]);
        assert_eq!(get("COMMENT"), ["a", "b"]);
        assert!(get("BAD=NAME").is_empty());
    }
}
