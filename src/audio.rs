//! Audio file inspection and decoding, with no external tool.
//!
//! FLAC and WAV are handled natively. APE, SHN and WavPack are recognised but
//! not decoded yet (planned for step 2).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

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
    /// Lossy: read for checks and analyses, never converted to FLAC.
    Mp3,
    Mp2,
    /// Containers: what they hold (AAC, ALAC...) is in `AudioInfo::codec`.
    M4a,
    /// MPEG-4 video files: their audio track only (concert videos).
    Mp4,
    Mka,
    /// DVD-Video: one title (its first VOB stands for the title set), described from its
    /// IFO and stream headers; the audio is not decoded.
    Vob,
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
            "mp3" => Some(Format::Mp3),
            "mp2" => Some(Format::Mp2),
            "m4a" => Some(Format::M4a),
            "mp4" => Some(Format::Mp4),
            "vob" => Some(Format::Vob),
            "mka" => Some(Format::Mka),
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
            Format::Mp3 => "MP3",
            Format::Mp2 => "MP2",
            Format::M4a => "M4A",
            Format::Mp4 => "MP4",
            Format::Vob => "VOB",
            Format::Mka => "MKA",
        }
    }

    /// Formats this version can decode.
    pub fn supported(self) -> bool {
        true
    }

    /// Lossy by the format itself. A container depends on its codec: see `AudioInfo::lossy`.
    pub fn lossy(self) -> bool {
        matches!(self, Format::Mp3 | Format::Mp2)
    }

    pub fn container(self) -> bool {
        matches!(self, Format::M4a | Format::Mp4 | Format::Mka | Format::Vob)
    }
}

/// The codec inside an M4A or MKA file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    Aac,
    Alac,
    Flac,
    Mp3,
    Mp2,
    Vorbis,
    Pcm,
    /// DVD audio.
    Ac3,
    Dts,
    Lpcm,
}

impl Codec {
    fn from_type(codec: symphonia::core::codecs::CodecType) -> Option<Codec> {
        use symphonia::core::codecs::*;
        Some(match codec {
            CODEC_TYPE_AAC => Codec::Aac,
            CODEC_TYPE_ALAC => Codec::Alac,
            CODEC_TYPE_FLAC => Codec::Flac,
            CODEC_TYPE_MP3 => Codec::Mp3,
            CODEC_TYPE_MP2 => Codec::Mp2,
            CODEC_TYPE_VORBIS => Codec::Vorbis,
            CODEC_TYPE_PCM_S16LE | CODEC_TYPE_PCM_S16BE | CODEC_TYPE_PCM_S24LE | CODEC_TYPE_PCM_S24BE
            | CODEC_TYPE_PCM_S32LE | CODEC_TYPE_PCM_S32BE | CODEC_TYPE_PCM_U8 | CODEC_TYPE_PCM_S8 => Codec::Pcm,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Codec::Aac => "AAC",
            Codec::Alac => "ALAC",
            Codec::Flac => "FLAC",
            Codec::Mp3 => "MP3",
            Codec::Mp2 => "MP2",
            Codec::Vorbis => "Vorbis",
            Codec::Pcm => "PCM",
            Codec::Ac3 => "AC-3",
            Codec::Dts => "DTS",
            Codec::Lpcm => "LPCM",
        }
    }

    /// For media info.
    pub fn long_name(self) -> &'static str {
        match self {
            Codec::Aac => "AAC (Advanced Audio Coding)",
            Codec::Alac => "ALAC (Apple Lossless)",
            Codec::Flac => "FLAC (Free Lossless Audio Codec)",
            Codec::Mp3 => "MPEG Audio Layer 3 (MP3)",
            Codec::Mp2 => "MPEG Audio Layer 2 (MP2)",
            Codec::Vorbis => "Vorbis",
            Codec::Pcm => "PCM (uncompressed)",
            Codec::Ac3 => "AC-3 (Dolby Digital)",
            Codec::Dts => "DTS",
            Codec::Lpcm => "LPCM (uncompressed PCM)",
        }
    }

    /// A DVD's coding name ("AC-3", "MPEG Audio Layer 2"...).
    pub fn from_dvd(coding: &str) -> Option<Codec> {
        Some(match coding {
            "AC-3" => Codec::Ac3,
            "DTS" => Codec::Dts,
            "LPCM" => Codec::Lpcm,
            c if c.starts_with("MPEG") => Codec::Mp2,
            _ => return None,
        })
    }

    pub fn lossy(self) -> bool {
        matches!(self, Codec::Aac | Codec::Mp3 | Codec::Mp2 | Codec::Vorbis | Codec::Ac3 | Codec::Dts)
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
    /// M4A and MKA only: the codec inside.
    pub codec: Option<Codec>,
}

impl AudioInfo {
    pub fn lossy(&self) -> bool {
        self.format.lossy() || self.codec.is_some_and(Codec::lossy)
    }

    /// The codec for a container ("AAC"), the format otherwise ("FLAC").
    pub fn name(&self) -> &'static str {
        self.codec.map_or(self.format.name(), Codec::name)
    }

    pub fn is_cd(&self) -> bool {
        !self.lossy() && self.rate == 44100 && self.bits == 16 && self.channels == 2
    }

    /// Some(true/false) for CD audio, None when the notion does not apply.
    pub fn sbe(&self) -> Option<bool> {
        self.is_cd().then_some(!self.samples.is_multiple_of(588))
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
        if self.lossy() {
            return format!("{} {}", self.name(), trim_float(self.rate as f64 / 1000.0));
        }
        let float = if self.float { " float" } else { "" };
        format!("{} {}{float}/{}", self.name(), self.bits, trim_float(self.rate as f64 / 1000.0))
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
/// A header announcing an impossible sample rate is refused here, before anything is
/// decoded (a damaged 79-byte APE file claimed 855 MHz and 18 million samples).
pub fn probe_as(path: &Path, format: Option<Format>) -> Result<AudioInfo, String> {
    let info = probe_header(path, format)?;
    if !(1..=MAX_RATE).contains(&info.rate) {
        return Err(format!("damaged header: sample rate {} Hz", info.rate));
    }
    Ok(info)
}

/// What the audio operations say of a DVD title.
pub const DVD_NOT_DECODED: &str = "DVD title: its audio is not decoded here (Media info describes its tracks)";

/// The highest sample rate of real audio files (DXD is 705.6 and 768 kHz).
pub const MAX_RATE: u32 = 768_000;

fn probe_header(path: &Path, format: Option<Format>) -> Result<AudioInfo, String> {
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
            codec: None,
        }),
        Some(Format::Wav) => probe_wav(path).map(|(info, _, _)| info),
        Some(f @ (Format::Mp3 | Format::Mp2)) => probe_mpa(path, f),
        Some(f @ (Format::M4a | Format::Mp4 | Format::Mka)) => probe_container(path, f),
        Some(Format::Vob) => crate::dvd::probe_title(path),
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
                codec: None,
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
            codec: None,
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
        codec: None,
    })
}

/// Returns the info plus the offset and the readable length of the WAV data chunk.
/// `info.samples` is what the header announces: a truncated file has fewer
/// readable bytes, which `read_pcm` reports.
/// What a WAV file holds besides its sound, for "Clean WAV headers".
pub struct WavLayout {
    /// Every chunk in order: id and size as written.
    pub chunks: Vec<(String, u64)>,
    /// fmt chunk: format tag (1 = PCM, 3 = float, 0xFFFE = extensible) and its length.
    pub format_tag: u16,
    pub fmt_len: u64,
    pub bits: u32,
    pub channels: u32,
    /// The RIFF size field agrees with the file length.
    pub riff_size_ok: bool,
    /// The data size field: None when it is 0 or 0xFFFFFFFF (never filled in).
    pub data_declared: Option<u64>,
    /// Bytes of sound actually present.
    pub data_available: u64,
    /// Bytes left after the last complete chunk.
    pub trailing: u64,
}

/// Walks every chunk of a WAV file (unlike `probe_wav`, which stops at the sound).
pub fn wav_layout(path: &Path) -> Result<WavLayout, String> {
    let mut f = BufReader::new(File::open(path).map_err(io_err)?);
    let file_len = f.get_ref().metadata().map_err(io_err)?.len();
    let mut riff = [0u8; 12];
    f.read_exact(&mut riff).map_err(|_| "file too short".to_string())?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let riff_size = u32::from_le_bytes(riff[4..8].try_into().unwrap()) as u64;
    let mut layout = WavLayout {
        chunks: Vec::new(),
        format_tag: 0,
        fmt_len: 0,
        bits: 0,
        channels: 0,
        riff_size_ok: riff_size + 8 == file_len,
        data_declared: None,
        data_available: 0,
        trailing: 0,
    };
    let mut pos = 12u64;
    while pos + 8 <= file_len {
        let mut head = [0u8; 8];
        f.read_exact(&mut head).map_err(io_err)?;
        let id = String::from_utf8_lossy(&head[0..4]).into_owned();
        let size = u32::from_le_bytes(head[4..8].try_into().unwrap()) as u64;
        pos += 8;
        let available = file_len - pos;
        if id == "fmt " {
            let mut fmt = vec![0u8; size.min(64).min(available) as usize];
            f.read_exact(&mut fmt).map_err(io_err)?;
            if fmt.len() >= 16 {
                layout.format_tag = u16::from_le_bytes([fmt[0], fmt[1]]);
                layout.channels = u16::from_le_bytes([fmt[2], fmt[3]]) as u32;
                layout.bits = u16::from_le_bytes([fmt[14], fmt[15]]) as u32;
            }
            layout.fmt_len = size;
            f.seek(SeekFrom::Current((size + (size & 1)).saturating_sub(fmt.len() as u64) as i64)).map_err(io_err)?;
        } else if id == "data" {
            let filled = size != 0 && size != 0xFFFF_FFFF;
            layout.data_declared = filled.then_some(size);
            let len = if filled { size } else { available };
            layout.data_available = len.min(available);
            layout.chunks.push((id, size));
            pos += len + (len & 1);
            if pos > file_len {
                break;
            }
            f.seek(SeekFrom::Start(pos)).map_err(io_err)?;
            continue;
        } else {
            f.seek(SeekFrom::Current((size + (size & 1)) as i64)).map_err(io_err)?;
        }
        layout.chunks.push((id, size));
        pos += size + (size & 1);
    }
    layout.trailing = file_len.saturating_sub(pos);
    Ok(layout)
}

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
                let info = AudioInfo { format: Format::Wav, rate, bits, channels, samples, md5: None, float, codec: None };
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
        Some(f @ (Format::Mp3 | Format::Mp2)) => decode_mpa(path, f, cancel, sink),
        Some(f @ (Format::M4a | Format::Mp4 | Format::Mka)) => decode_container(path, f, cancel, sink),
        Some(Format::Vob) => Err(DVD_NOT_DECODED.into()),
        None => Err("not an audio file".into()),
    }
}

fn decode_flac(path: &Path, cancel: &AtomicBool, sink: impl FnMut(&[u8])) -> Result<u64, String> {
    let file = File::open(path).map_err(io_err)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("flac");
    let probed = probe_for(Format::Flac)
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("cannot open FLAC stream: {e}"))?;
    let reader = probed.format;
    let track = reader.default_track().ok_or("no audio track")?.clone();
    let bits = track.codec_params.bits_per_sample.ok_or("unknown bit depth")?;
    decode_track(reader, &track, Some(bits), cancel, sink)
}

/// Decodes one track to little-endian PCM: `bits` wide, or 16-bit for lossy codecs (None).
fn decode_track(
    mut reader: Box<dyn symphonia::core::formats::FormatReader>,
    track: &symphonia::core::formats::Track,
    bits: Option<u32>,
    cancel: &AtomicBool,
    mut sink: impl FnMut(&[u8]),
) -> Result<u64, String> {
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions { verify: false })
        .map_err(|e| format!("cannot create decoder: {e}"))?;
    let (width, shift) = bits.map_or((2, 0), |b| (b.div_ceil(8) as usize, 32 - b));
    let mut frames = 0u64;
    let mut out: Vec<u8> = Vec::with_capacity(1 << 16);
    let mut wide: Option<SampleBuffer<i32>> = None;
    let mut narrow: Option<SampleBuffer<i16>> = None;
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
        let channels = spec.channels.count().max(1);
        let needed = decoded.capacity();
        out.clear();
        let samples = if bits.is_some() {
            if wide.as_ref().is_none_or(|b| b.capacity() < needed * channels) {
                wide = Some(SampleBuffer::<i32>::new(needed as u64, spec));
            }
            let buf = wide.as_mut().unwrap();
            buf.copy_interleaved_ref(decoded);
            for &s in buf.samples() {
                out.extend_from_slice(&(s >> shift).to_le_bytes()[..width]);
            }
            buf.samples().len()
        } else {
            if narrow.as_ref().is_none_or(|b| b.capacity() < needed * channels) {
                narrow = Some(SampleBuffer::<i16>::new(needed as u64, spec));
            }
            let buf = narrow.as_mut().unwrap();
            buf.copy_interleaved_ref(decoded);
            for &s in buf.samples() {
                out.extend_from_slice(&s.to_le_bytes());
            }
            buf.samples().len()
        };
        frames += (samples / channels) as u64;
        sink(&out);
    }
    Ok(frames)
}

/// An MPEG audio (MP3, MP2) stream, gapless: the encoder delay and padding the LAME
/// header records are left out, as players do.
fn open_mpa(path: &Path, format: Format) -> Result<Box<dyn symphonia::core::formats::FormatReader>, String> {
    let file = File::open(path).map_err(io_err)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(if format == Format::Mp2 { "mp2" } else { "mp3" });
    let options = FormatOptions { enable_gapless: true, ..Default::default() };
    probe_for(format)
        .format(&hint, mss, &options, &MetadataOptions::default())
        .map(|p| p.format)
        .map_err(|e| format!("cannot open {} stream: {e}", format.name()))
}

/// Format and length of an MP3/MP2 file. Decoded as 16-bit samples, like CD audio; the
/// length comes from the LAME/Xing header, or by counting the frames when there is none.
fn probe_mpa(path: &Path, format: Format) -> Result<AudioInfo, String> {
    let mut reader = open_mpa(path, format)?;
    let track = reader.default_track().ok_or("no audio track")?.clone();
    let p = &track.codec_params;
    let rate = p.sample_rate.ok_or("unknown sample rate")?;
    let channels = p.channels.map(|c| c.count() as u32).unwrap_or(2);
    let samples = match p.n_frames {
        Some(n) => n,
        None => {
            let mut n = 0u64;
            while let Ok(packet) = reader.next_packet() {
                if packet.track_id() == track.id {
                    n += packet.dur;
                }
            }
            n
        }
    };
    Ok(AudioInfo { format, rate, bits: 16, channels, samples, md5: None, float: false, codec: None })
}

fn decode_mpa(path: &Path, format: Format, cancel: &AtomicBool, sink: impl FnMut(&[u8])) -> Result<u64, String> {
    let reader = open_mpa(path, format)?;
    let track = reader.default_track().ok_or("no audio track")?.clone();
    decode_track(reader, &track, None, cancel, sink)
}

/// Symphonia's prober with the one reader of `format`: the default one tries every
/// reader on any file, so a damaged .flac or .m4a could reach the MKV reader (below).
fn probe_for(format: Format) -> &'static symphonia::core::probe::Probe {
    use symphonia::core::probe::Probe;
    use symphonia::default::formats::{FlacReader, IsoMp4Reader, MkvReader, MpaReader};
    static PROBES: OnceLock<[Probe; 4]> = OnceLock::new();
    let probes = PROBES.get_or_init(|| {
        let mut flac = Probe::default();
        flac.register_all::<FlacReader>();
        let mut mpa = Probe::default();
        mpa.register_all::<MpaReader>();
        let mut mp4 = Probe::default();
        mp4.register_all::<IsoMp4Reader>();
        let mut mkv = Probe::default();
        mkv.register_all::<MkvReader>();
        [flac, mpa, mp4, mkv]
    });
    match format {
        Format::Mp3 | Format::Mp2 => &probes[1],
        Format::M4a | Format::Mp4 => &probes[2],
        Format::Mka => &probes[3],
        _ => &probes[0],
    }
}

/// Matroska elements that hold other elements (the ones symphonia reads).
const EBML_MASTERS: [u32; 31] = [
    0x1A45DFA3, 0x18538067, 0x114D9B74, 0x4DBB, 0x1549A966, 0x1654AE6B, 0xAE, 0xE0, 0xE1, 0x6D80, 0x6240, 0x5034,
    0x5035, 0x1F43B675, 0xA0, 0x75A1, 0xA6, 0x1C53BB6B, 0xBB, 0xB7, 0xDB, 0x1043A770, 0x45B9, 0xB6, 0x8F, 0x80,
    0x1254C367, 0x7373, 0x63C0, 0x67C8, 0x1941A469,
];

/// Checks an MKA file's element sizes before symphonia reads it: symphonia 0.5 allocates
/// an element's announced size before reading it, and a damaged size made it ask for
/// terabytes, which ends the program (found by fuzzing). Every element must fit in its
/// parent and in the file; only the headers are read.
fn ebml_sizes_ok(path: &Path) -> Result<(), String> {
    let mut f = BufReader::new(File::open(path).map_err(io_err)?);
    let len = f.get_ref().metadata().map_err(io_err)?.len();
    // A variable-length integer: (value, width); for a size, all ones means "unknown".
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
    let damaged = || "damaged MKA file: an element is larger than the file".to_string();
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
        f.seek(SeekFrom::Start(pos)).map_err(io_err)?;
        let Some((id, id_width, _)) = vint(&mut f, true) else { return Err(damaged()) };
        let Some((size, size_width, unknown)) = vint(&mut f, false) else { return Err(damaged()) };
        let start = pos + (id_width + size_width) as u64;
        let stop = if unknown { end } else { start.checked_add(size).filter(|&s| s <= end).ok_or_else(damaged)? };
        if u32::try_from(id).is_ok_and(|id| EBML_MASTERS.contains(&id)) {
            ends.push(stop);
            pos = start;
        } else {
            pos = stop;
        }
    }
    Ok(())
}

/// An M4A or MKA file, with its first audio track this version can decode.
fn open_container(
    path: &Path,
    format: Format,
) -> Result<(Box<dyn symphonia::core::formats::FormatReader>, symphonia::core::formats::Track, Codec), String> {
    if format == Format::Mka {
        ebml_sizes_ok(path)?;
    }
    let file = File::open(path).map_err(io_err)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(if format == Format::Mka { "mka" } else { "m4a" });
    let options = FormatOptions { enable_gapless: true, ..Default::default() };
    let reader = probe_for(format)
        .format(&hint, mss, &options, &MetadataOptions::default())
        .map_err(|e| format!("cannot open {} file: {e}", format.name()))?
        .format;
    let found = reader.tracks().iter().find_map(|t| Codec::from_type(t.codec_params.codec).map(|c| (t.clone(), c)));
    match found {
        Some((mut track, codec)) => {
            let p = &mut track.codec_params;
            if codec == Codec::Alac {
                // The sample entry holds the rate as 16.16 fixed point (96 kHz does not fit):
                // the ALAC "magic cookie" has it whole, at offset 20.
                if let Some(rate) = p.extra_data.as_ref().and_then(|c| c.get(20..24)).map(|b| u32::from_be_bytes(b.try_into().unwrap())) {
                    p.with_sample_rate(rate);
                }
            }
            if codec == Codec::Pcm && p.max_frames_per_packet.is_none() {
                // Matroska does not give it; 2 seconds is far more than any block holds.
                let rate = p.sample_rate.unwrap_or(48000) as u64;
                p.with_max_frames_per_packet(2 * rate);
            }
            Ok((reader, track, codec))
        }
        None => Err(match mka_codec_id(path) {
            Some(id) => format!("{} audio is not supported yet", mka_codec_name(&id)),
            None => "no audio track this version can decode".into(),
        }),
    }
}

/// The bit depth a lossless codec decodes to; None for lossy codecs (decoded as 16-bit).
fn container_bits(track: &symphonia::core::formats::Track, codec: Codec) -> Result<Option<u32>, String> {
    if codec.lossy() {
        return Ok(None);
    }
    let p = &track.codec_params;
    let bits = match codec {
        // The ALAC "magic cookie": frame length (4 bytes), version, bit depth.
        Codec::Alac => p.extra_data.as_ref().and_then(|cookie| cookie.get(5)).map(|&b| b as u32),
        _ => p.bits_per_sample.or_else(|| {
            // FLAC in MKA: the decoder reads it from STREAMINFO.
            symphonia::default::get_codecs().make(p, &DecoderOptions::default()).ok()?.codec_params().bits_per_sample
        }),
    };
    bits.filter(|b| (8..=32).contains(b)).map(Some).ok_or_else(|| "unknown bit depth".into())
}

fn probe_container(path: &Path, format: Format) -> Result<AudioInfo, String> {
    let (mut reader, track, codec) = open_container(path, format)?;
    let bits = container_bits(&track, codec)?;
    let p = &track.codec_params;
    let rate = p.sample_rate.ok_or("unknown sample rate")?;
    let channels = p.channels.map(|c| c.count() as u32).unwrap_or(2);
    // The length in the container's own time units (MKA: milliseconds). Lossy codecs decode
    // a little more (the encoder's delay and padding): see `length_ok`.
    let samples = match (p.n_frames, p.time_base) {
        (Some(n), Some(base)) => {
            let t = base.calc_time(n);
            ((t.seconds as f64 + t.frac) * rate as f64).round() as u64
        }
        _ => {
            let mut n = 0u64;
            while let Ok(packet) = reader.next_packet() {
                if packet.track_id() == track.id {
                    n += packet.dur;
                }
            }
            n
        }
    };
    Ok(AudioInfo { format, rate, bits: bits.unwrap_or(16), channels, samples, md5: None, float: false, codec: Some(codec) })
}

/// Whether `frames` decoded samples match the announced length. In an M4A or MKA, lossy
/// codecs also decode the encoder's delay and padding (a few thousand samples); an MKA
/// gives its length to the millisecond only.
pub fn length_ok(info: &AudioInfo, frames: u64) -> bool {
    if info.format.container() && info.lossy() {
        frames + 4096 >= info.samples && frames <= info.samples + 8192
    } else if info.format == Format::Mka {
        frames.abs_diff(info.samples) <= info.rate as u64 / 1000 + 1
    } else {
        frames == info.samples
    }
}

fn decode_container(path: &Path, format: Format, cancel: &AtomicBool, sink: impl FnMut(&[u8])) -> Result<u64, String> {
    let (reader, track, codec) = open_container(path, format)?;
    let bits = container_bits(&track, codec)?;
    decode_track(reader, &track, bits, cancel, sink)
}

/// The codec ID of the first audio track of an MKA file ("A_AC3"), read from its first 64 KB.
pub fn mka_codec_id(path: &Path) -> Option<String> {
    let mut head = Vec::new();
    File::open(path).ok()?.take(1 << 16).read_to_end(&mut head).ok()?;
    // CodecID element: 0x86, a one-byte size, then the text.
    head.windows(4).enumerate().find_map(|(i, w)| {
        (w[0] == 0x86 && w[1] & 0x80 != 0 && &w[2..4] == b"A_").then(|| {
            let len = (w[1] & 0x7f) as usize;
            String::from_utf8_lossy(head.get(i + 2..i + 2 + len)?).into_owned().into()
        })?
    })
}

/// "A_AC3" -> "AC-3".
pub fn mka_codec_name(id: &str) -> String {
    match id {
        "A_AC3" | "A_AC3/BSID9" | "A_AC3/BSID10" => "AC-3".into(),
        "A_EAC3" => "E-AC-3".into(),
        "A_DTS" | "A_DTS/EXPRESS" | "A_DTS/LOSSLESS" => "DTS".into(),
        "A_TRUEHD" => "Dolby TrueHD".into(),
        "A_OPUS" => "Opus".into(),
        "A_ALAC" => "ALAC (in MKA)".into(),
        "A_TTA1" => "TTA".into(),
        "A_WAVPACK4" => "WavPack (in MKA)".into(),
        other => other.trim_start_matches("A_").to_string(),
    }
}

pub fn open_ape(path: &Path) -> Result<ape_decoder::ApeDecoder<BufReader<File>>, String> {
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
        Some(f @ (Format::M4a | Format::Mp4 | Format::Mka)) => return container_tags(path, f),
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

/// The tags of an M4A (iTunes) or MKA file, under their Vorbis names.
fn container_tags(path: &Path, format: Format) -> Vec<(String, String)> {
    use symphonia::core::meta::StandardTagKey as K;
    if format == Format::Mka {
        return mka_tags(path);
    }
    let Ok((mut reader, _, _)) = open_container(path, format) else { return Vec::new() };
    let Some(revision) = reader.metadata().skip_to_latest().cloned() else { return Vec::new() };
    revision
        .tags()
        .iter()
        .filter_map(|tag| {
            let name = match tag.std_key? {
                K::Artist => "ARTIST",
                K::AlbumArtist => "ALBUMARTIST",
                K::Album => "ALBUM",
                K::TrackTitle => "TITLE",
                K::TrackNumber => "TRACKNUMBER",
                K::DiscNumber => "DISCNUMBER",
                K::Date | K::ReleaseDate => "DATE",
                K::Genre => "GENRE",
                K::Comment => "COMMENT",
                _ => return None,
            };
            Some((name.to_string(), tag.value.to_string()))
        })
        .collect()
}

/// The SimpleTag elements of an MKA file (name 0x45A3, text 0x4487), found in its first and
/// last 256 KB, where muxers put them.
fn mka_tags(path: &Path) -> Vec<(String, String)> {
    const KEEP: [&str; 8] = ["ARTIST", "ALBUMARTIST", "ALBUM", "TITLE", "TRACKNUMBER", "DATE", "GENRE", "COMMENT"];
    let Ok(mut file) = File::open(path) else { return Vec::new() };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut data = Vec::new();
    let _ = file.by_ref().take(1 << 18).read_to_end(&mut data);
    if len > 1 << 19 && file.seek(SeekFrom::Start(len - (1 << 18))).is_ok() {
        let _ = file.read_to_end(&mut data);
    }
    // An EBML element: its ID, a size (vint), then the data.
    let element = |at: usize, id: &[u8]| -> Option<(String, usize)> {
        let rest = data.get(at..)?.strip_prefix(id)?;
        let first = *rest.first()?;
        let width = first.leading_zeros() as usize + 1;
        if width > 8 {
            return None;
        }
        let size = rest.get(1..width)?.iter().fold((first as u64) & (0xff >> width), |acc, &b| (acc << 8) | b as u64) as usize;
        let start = at + id.len() + width;
        Some((String::from_utf8_lossy(data.get(start..start + size)?).into_owned(), start + size))
    };
    let mut tags: Vec<(String, String)> = Vec::new();
    let mut at = 0;
    while let Some(found) = data.get(at..).and_then(|d| d.windows(2).position(|w| w == [0x45, 0xA3])) {
        at += found;
        if let Some((name, next)) = element(at, &[0x45, 0xA3]) {
            if let Some((value, _)) = element(next, &[0x44, 0x87]) {
                let name = match name.to_ascii_uppercase().as_str() {
                    "PART_NUMBER" => "TRACKNUMBER".to_string(),
                    "DATE_RECORDED" | "DATE_RELEASED" => "DATE".to_string(),
                    other => other.to_string(),
                };
                if KEEP.contains(&name.as_str()) && !tags.iter().any(|(n, _)| *n == name) {
                    tags.push((name, value));
                }
            }
        }
        at += 2;
    }
    // The title can be the segment's own (Info > Title, 0x7BA9) rather than a tag.
    if !tags.iter().any(|(n, _)| n == "TITLE") {
        let title = data.windows(2).position(|w| w == [0x7B, 0xA9]).and_then(|at| element(at, &[0x7B, 0xA9]));
        if let Some((title, _)) = title.filter(|(t, _)| !t.trim().is_empty()) {
            tags.push(("TITLE".into(), title));
        }
    }
    tags
}

/// The VORBIS_COMMENT block of a FLAC file ("ARTIST", "Some name").
fn flac_tags(path: &Path) -> Result<Vec<(String, String)>, String> {
    flac_comment(path).map(|d| parse_vorbis_comment(&d))
}

/// The encoder a FLAC file names in its VORBIS_COMMENT block ("reference libFLAC 1.4.3 20230623").
pub fn flac_vendor(path: &Path) -> Option<String> {
    let d = flac_comment(path).ok()?;
    let len = u32::from_le_bytes(d.get(..4)?.try_into().ok()?) as usize;
    let vendor = String::from_utf8_lossy(d.get(4..4 + len)?).trim().to_string();
    (!vendor.is_empty()).then_some(vendor)
}

/// The raw VORBIS_COMMENT block (empty if there is none).
fn flac_comment(path: &Path) -> Result<Vec<u8>, String> {
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
            return Ok(data);
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

/// A decoded APE frame: its index and its PCM bytes (or the error).
type DecodedFrame = (u32, Result<Vec<u8>, String>);

/// Monkey's Audio: decoded frame by frame (each frame's CRC is checked), as in a WAV
/// data chunk (little-endian; files made from AIFF are big-endian and get swapped).
/// Monkey's Audio frames decode independently, and the stronger modes are slow (about
/// twice the time of FLAC per frame on one core): the frames of a batch are shared out
/// between one decoder per thread, then handed to `sink` in order.
fn decode_ape(path: &Path, cancel: &AtomicBool, mut sink: impl FnMut(&[u8])) -> Result<u64, String> {
    let first = open_ape(path)?;
    let (big_endian, width, channels, total) = {
        let i = first.info();
        (i.is_big_endian, (i.bits_per_sample as usize).div_ceil(8), i.channels.max(1) as usize, i.total_frames)
    };
    let (frame_len, rate, samples) = {
        let i = first.info();
        (i.blocks_per_frame as u64, i.sample_rate.max(1) as f64, i.total_samples)
    };
    // The seek table holds 4 bytes per frame: a damaged header can announce billions of
    // frames in a small file, each failing in turn (found by fuzzing: hours on 96 bytes).
    let size = std::fs::metadata(path).map_err(io_err)?.len();
    if total as u64 * 4 > size {
        return Err(format!("damaged header: {total} frames announced in a file of {size} bytes"));
    }
    // Monkey's Audio writes at most 294,912 blocks per frame (Extra High, Insane): a larger
    // value is a damaged header, each frame of which takes seconds to fail (fuzzing).
    if frame_len > 1 << 21 {
        return Err(format!("damaged header: {frame_len} samples per frame"));
    }
    if !(1..=4).contains(&width) {
        return Err(format!("damaged header: {} bits per sample", first.info().bits_per_sample));
    }
    // Frames that fail: the rest is still decoded (not passed on), to tell how much is lost.
    let mut damaged: Vec<u32> = Vec::new();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8).min(total.max(1) as usize);
    let mut decoders = vec![first];
    for _ in 1..threads {
        decoders.push(open_ape(path)?);
    }
    let batch = (threads * 2) as u32;
    let mut bytes = 0u64;
    let mut start = 0u32;
    while start < total {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let end = (start + batch).min(total);
        let decoded: Vec<Vec<DecodedFrame>> = std::thread::scope(|scope| {
            let handles: Vec<_> = decoders
                .iter_mut()
                .enumerate()
                .map(|(t, decoder)| {
                    scope.spawn(move || {
                        (start + t as u32..end)
                            .step_by(threads)
                            .map(|f| (f, decoder.decode_frame(f).map_err(|e| e.to_string())))
                            .collect()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
        });
        let mut frames: Vec<(u32, Result<Vec<u8>, String>)> = decoded.into_iter().flatten().collect();
        frames.sort_by_key(|(f, _)| *f);
        if frames.len() != (end - start) as usize {
            return Err("damaged data: a frame could not be decoded".into());
        }
        for (f, frame) in frames {
            let Ok(mut pcm) = frame else {
                damaged.push(f);
                continue;
            };
            if !damaged.is_empty() {
                continue;
            }
            if big_endian && width > 1 {
                for sample in pcm.chunks_exact_mut(width) {
                    sample.reverse();
                }
            }
            bytes += pcm.len() as u64;
            sink(&pcm);
        }
        start = end;
    }
    if let Some(&first_bad) = damaged.first() {
        let at = |frame: u32| {
            let t = ((frame as u64 * frame_len).min(samples)) as f64 / rate;
            format!("{}:{:04.1}", (t / 60.0) as u64, t % 60.0)
        };
        let readable = total as usize - damaged.len();
        return Err(if damaged.len() == 1 {
            format!(
                "damaged audio in frame {} of {total} ({} to {}): this part cannot be decoded; the other {readable} frames are readable",
                first_bad + 1,
                at(first_bad),
                at(first_bad + 1)
            )
        } else {
            format!(
                "damaged audio in {} of {total} frames, the first from {} to {}: these parts cannot be decoded; the other {readable} frames are readable",
                damaged.len(),
                at(first_bad),
                at(first_bad + 1)
            )
        });
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
/// Reads a whole file through `each`, in 1 MiB chunks.
fn read_file(path: &Path, cancel: &AtomicBool, mut each: impl FnMut(&[u8])) -> Result<(), String> {
    let mut f = File::open(path).map_err(io_err)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let n = f.read(&mut buf).map_err(io_err)?;
        if n == 0 {
            return Ok(());
        }
        each(&buf[..n]);
    }
}

pub fn file_md5(path: &Path, cancel: &AtomicBool) -> Result<[u8; 16], String> {
    let mut hasher = Md5::new();
    read_file(path, cancel, |chunk| hasher.update(chunk))?;
    Ok(hasher.finalize().into())
}

/// CRC32 of the whole file, as SFV files store it.
pub fn file_crc32(path: &Path, cancel: &AtomicBool) -> Result<u32, String> {
    let mut hasher = crc32fast::Hasher::new();
    read_file(path, cancel, |chunk| hasher.update(chunk))?;
    Ok(hasher.finalize())
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
