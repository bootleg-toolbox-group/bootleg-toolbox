//! FLAC encoding (any decodable source to FLAC), streamed frame by frame with flacenc,
//! then verified by decoding the result again.
//!
//! The file is written as: "fLaC", STREAMINFO (filled in at the end with the MD5
//! signature, the length and the frame sizes), VORBIS_COMMENT (tags), PADDING, frames.

use std::fs::{self, File};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use flacenc::bitsink::ByteSink;
use md5::{Digest, Md5};
use flacenc::component::{BitRepr, StreamInfo};
use flacenc::config;
use flacenc::error::Verify;
use flacenc::source::{Context, Fill, FrameBuf};

use crate::audio::{self, AudioInfo, Format};

/// Room left for tags edited later, as the reference encoder does.
const PADDING: usize = 8192;

/// flacenc settings close to the reference encoder's -0 ... -8 presets.
fn preset(level: u8) -> config::Encoder {
    let mut c = config::Encoder::default();
    c.multithread = false;
    let level = level.min(8);
    c.block_size = if level <= 2 { 1152 } else { 4096 };
    c.stereo_coding.use_leftside = level >= 1;
    c.stereo_coding.use_rightside = level >= 1;
    c.stereo_coding.use_midside = level >= 1;
    c.subframe_coding.use_lpc = level >= 3;
    c.subframe_coding.qlpc.lpc_order = match level {
        0..=3 => 6,
        4..=6 => 8,
        _ => 12,
    };
    c
}

/// Little-endian PCM bytes (as `read_pcm` gives them) to signed samples.
fn to_samples(bytes: &[u8], width: usize, out: &mut Vec<i32>) {
    if width == 1 {
        out.extend(bytes.iter().map(|b| *b as i32 - 128));
    } else {
        out.extend(bytes.chunks_exact(width).map(crate::dsp::int_sample));
    }
}

pub struct Encoded {
    pub samples: u64,
    pub md5: [u8; 16],
}

/// Encodes `source` to `out` (written through a ".part" file, renamed when complete and
/// verified). `tags` are Vorbis comments ("ARTIST", "Some name").
pub fn to_flac(source: &Path, out: &Path, level: u8, tags: &[(String, String)], cancel: &AtomicBool) -> Result<Encoded, String> {
    let info = audio::probe(source)?;
    check_encodable(&info)?;
    pcm_to_flac(out, &info, level, tags, cancel, |sink| audio::read_pcm(source, cancel, sink).map(|_| ()))
}

/// Encodes the PCM that `feed` pushes into its sink (little-endian, as `read_pcm` gives
/// it, in the format of `info`) to `out`, through a ".part" file, verified.
pub fn pcm_to_flac(
    out: &Path,
    info: &AudioInfo,
    level: u8,
    tags: &[(String, String)],
    cancel: &AtomicBool,
    feed: impl FnOnce(&mut dyn FnMut(&[u8])) -> Result<(), String>,
) -> Result<Encoded, String> {
    let part = part_path(out);
    let result = write_flac(feed, info, &part, level, tags).and_then(|encoded| {
        verify(&part, &encoded, cancel)?;
        Ok(encoded)
    });
    match result {
        Ok(encoded) => {
            fs::rename(&part, out).map_err(|e| e.to_string())?;
            Ok(encoded)
        }
        Err(e) => {
            if std::env::var_os("BH_KEEP_PART").is_none() {
                let _ = fs::remove_file(&part);
            }
            Err(e)
        }
    }
}

pub fn check_encodable(info: &AudioInfo) -> Result<(), String> {
    if info.float {
        return Err("32-bit float audio cannot be stored in FLAC without changing it".into());
    }
    if !(8..=24).contains(&info.bits) {
        return Err(format!("{}-bit audio cannot be encoded to FLAC here (8 to 24 bits)", info.bits));
    }
    if info.channels == 0 || info.channels > 8 {
        return Err(format!("{} channels cannot be encoded to FLAC", info.channels));
    }
    if info.rate == 0 || info.rate > 655_350 {
        return Err("unsupported sample rate".into());
    }
    Ok(())
}

fn part_path(out: &Path) -> PathBuf {
    let mut name = out.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    out.with_file_name(name)
}

fn block(kind: u8, last: bool, data: &[u8]) -> Vec<u8> {
    let len = data.len() as u32;
    let mut b = vec![kind | if last { 0x80 } else { 0 }, (len >> 16) as u8, (len >> 8) as u8, len as u8];
    b.extend_from_slice(data);
    b
}

fn vorbis_comment(tags: &[(String, String)]) -> Vec<u8> {
    let vendor = format!("{} {}", crate::ops::APP_NAME, crate::ops::APP_VERSION);
    let mut d = Vec::new();
    d.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    d.extend_from_slice(vendor.as_bytes());
    d.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (k, v) in tags {
        let entry = format!("{}={v}", k.to_uppercase());
        d.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        d.extend_from_slice(entry.as_bytes());
    }
    d
}

fn write_flac(
    feed: impl FnOnce(&mut dyn FnMut(&[u8])) -> Result<(), String>,
    info: &AudioInfo,
    part: &Path,
    level: u8,
    tags: &[(String, String)],
) -> Result<Encoded, String> {
    let config = preset(level).into_verified().map_err(|e| format!("encoder settings: {e:?}"))?;
    let (channels, bits, rate) = (info.channels as usize, info.bits as usize, info.rate as usize);
    let block_size = config.block_size;
    let mut stream_info = StreamInfo::new(rate, channels, bits).map_err(|e| format!("{e:?}"))?;
    let mut file = BufWriter::new(File::create(part).map_err(|e| e.to_string())?);
    let io = |e: std::io::Error| e.to_string();
    file.write_all(b"fLaC").map_err(io)?;
    file.write_all(&block(0, false, &[0u8; 34])).map_err(io)?;
    file.write_all(&block(4, false, &vorbis_comment(tags))).map_err(io)?;
    file.write_all(&block(1, true, &vec![0u8; PADDING])).map_err(io)?;

    let mut buffers = (FrameBuf::with_size(channels, block_size).map_err(|e| format!("{e:?}"))?, Context::new(bits, channels));
    let width = info.bits.div_ceil(8) as usize;
    let frame_len = block_size * channels;
    let mut pending: Vec<i32> = Vec::with_capacity(frame_len * 2);
    let mut frame_number = 0usize;
    let mut failure: Option<String> = None;
    let mut sink = ByteSink::new();

    let mut encode = |samples: &[i32], buffers: &mut (FrameBuf, Context), stream_info: &mut StreamInfo, file: &mut BufWriter<File>| -> Result<(), String> {
        buffers.fill_interleaved(samples).map_err(|e| format!("{e:?}"))?;
        let frame = flacenc::encode_fixed_size_frame(&config, &buffers.0, frame_number, stream_info).map_err(|e| format!("{e:?}"))?;
        frame_number += 1;
        stream_info.update_frame_info(&frame);
        sink.clear();
        frame.write(&mut sink).map_err(|e| format!("{e:?}"))?;
        file.write_all(sink.as_slice()).map_err(|e| e.to_string())
    };

    let mut leftover: Vec<u8> = Vec::new();
    feed(&mut |chunk: &[u8]| {
        if failure.is_some() {
            return;
        }
        // Keep whole samples only; the rest waits for the next chunk.
        leftover.extend_from_slice(chunk);
        let whole = leftover.len() / width * width;
        to_samples(&leftover[..whole], width, &mut pending);
        leftover.drain(..whole);
        let full = pending.len() / frame_len * frame_len;
        for samples in pending[..full].chunks_exact(frame_len) {
            if let Err(e) = encode(samples, &mut buffers, &mut stream_info, &mut file) {
                failure = Some(e);
                return;
            }
        }
        pending.drain(..full);
    })?;
    if let Some(e) = failure {
        return Err(e);
    }
    let rest = pending.len() / channels * channels;
    if rest > 0 {
        encode(&pending[..rest], &mut buffers, &mut stream_info, &mut file)?;
    }

    let md5 = buffers.1.md5_digest();
    stream_info.set_md5_digest(&md5);
    let samples = stream_info.total_samples() as u64;
    // A fixed-blocksize stream: min = max = the nominal block size (the last, shorter
    // frame does not count); otherwise decoders take it for a variable-blocksize stream.
    stream_info.set_block_sizes(block_size, block_size).map_err(|e| format!("{e:?}"))?;
    let mut info_sink = ByteSink::new();
    stream_info.write(&mut info_sink).map_err(|e| format!("{e:?}"))?;
    let mut file = file.into_inner().map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(8)).map_err(io)?;
    file.write_all(info_sink.as_slice()).map_err(io)?;
    file.sync_all().map_err(io)?;
    Ok(Encoded { samples, md5 })
}

/// What a float WAV holds: FLAC stores integers only.
#[derive(Clone, Copy, Debug, Default)]
pub struct FloatScan {
    /// 16 or 24 when every sample is exactly a 16-bit or 24-bit value (the float only
    /// carries integer audio, which converts bit-exact); None for real float audio.
    pub exact_bits: Option<u32>,
    /// Samples above full scale, which 24-bit integers cannot hold.
    pub clipped: u64,
    /// Largest absolute sample value (1.0 = 0 dBFS).
    pub peak: f64,
}

/// One float sample per call to `each`, from the PCM bytes of a float WAV.
fn float_samples(path: &Path, info: &AudioInfo, cancel: &AtomicBool, mut each: impl FnMut(f64)) -> Result<u64, String> {
    let width = info.bits.div_ceil(8) as usize;
    if !info.float || !(width == 4 || width == 8) {
        return Err("not a 32-bit or 64-bit float WAV".into());
    }
    let mut leftover: Vec<u8> = Vec::new();
    audio::read_pcm(path, cancel, |chunk| {
        leftover.extend_from_slice(chunk);
        let whole = leftover.len() / width * width;
        for b in leftover[..whole].chunks_exact(width) {
            each(if width == 4 { f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64 } else { f64::from_le_bytes(b.try_into().unwrap()) });
        }
        leftover.drain(..whole);
    })
}

/// Reads a float WAV through, to find out how it converts to FLAC.
pub fn scan_float(source: &Path, cancel: &AtomicBool) -> Result<FloatScan, String> {
    let info = audio::probe(source)?;
    let (mut fits16, mut fits24, mut invalid) = (true, true, false);
    let mut scan = FloatScan::default();
    let fits = |v: f64, bits: i32| {
        let x = v * 2f64.powi(bits - 1);
        x == x.round() && x >= -(2f64.powi(bits - 1)) && x < 2f64.powi(bits - 1)
    };
    float_samples(source, &info, cancel, |v| {
        if !v.is_finite() {
            invalid |= v.is_nan();
            scan.clipped += 1;
            return;
        }
        scan.peak = scan.peak.max(v.abs());
        if v * 8_388_608.0 > 8_388_607.5 || v * 8_388_608.0 < -8_388_608.5 {
            scan.clipped += 1;
        }
        if v != 0.0 {
            fits16 = fits16 && fits(v, 16);
            fits24 = fits24 && fits(v, 24);
        }
    })?;
    if invalid {
        return Err("the file holds invalid float values (NaN): it is damaged".into());
    }
    scan.exact_bits = if fits16 { Some(16) } else if fits24 { Some(24) } else { None };
    Ok(scan)
}

/// How a float WAV was converted.
pub struct FloatConverted {
    pub bits: u32,
    /// Bit-exact: the float only held 16-bit or 24-bit values.
    pub exact: bool,
    pub clipped: u64,
    /// Largest difference between the float samples and the FLAC ones, in dBFS (None when
    /// there is none).
    pub max_diff_db: Option<f64>,
}

/// Converts a float WAV to FLAC: bit-exact when it only holds 16-bit or 24-bit values,
/// otherwise rounded to 24 bits when `round` is set (refused when it is not).
pub fn float_to_flac(source: &Path, out: &Path, level: u8, tags: &[(String, String)], round: bool, cancel: &AtomicBool) -> Result<FloatConverted, String> {
    let info = audio::probe(source)?;
    let scan = scan_float(source, cancel)?;
    let bits = match scan.exact_bits {
        Some(bits) => bits,
        None if round => 24,
        None => return Err("real 32-bit float audio: FLAC would round it to 24 bits (not bit-exact), and rounding was not allowed".into()),
    };
    let scale = 2f64.powi(bits as i32 - 1);
    let (lo, hi) = (-scale, scale - 1.0);
    let width = (bits / 8) as usize;
    let (mut clipped, mut max_diff) = (0u64, 0f64);
    let flac_info = AudioInfo { format: Format::Flac, rate: info.rate, bits, channels: info.channels, samples: info.samples, md5: None, float: false };
    pcm_to_flac(out, &flac_info, level, tags, cancel, |sink| {
        let mut buf: Vec<u8> = Vec::with_capacity(1 << 16);
        float_samples(source, &info, cancel, |v| {
            let x = if v.is_finite() { v * scale } else if v > 0.0 { hi } else { lo };
            let rounded = x.round();
            let n = rounded.clamp(lo, hi);
            if n != rounded {
                clipped += 1;
            } else {
                max_diff = max_diff.max((x - n).abs() / scale);
            }
            buf.extend_from_slice(&(n as i32).to_le_bytes()[..width]);
            if buf.len() >= 1 << 16 {
                sink(&buf);
                buf.clear();
            }
        })?;
        sink(&buf);
        Ok(())
    })?;
    let exact = scan.exact_bits.is_some() && clipped == 0 && max_diff == 0.0;
    Ok(FloatConverted { bits, exact, clipped, max_diff_db: (max_diff > 0.0).then(|| 20.0 * max_diff.log10()) })
}

/// Decodes `source` to a WAV file `out` (through a ".part" file), then checks it.
/// WAVE_FORMAT_EXTENSIBLE above 16 bits or 2 channels, as the reference decoder writes.
pub fn to_wav(source: &Path, out: &Path, cancel: &AtomicBool) -> Result<Encoded, String> {
    let info = audio::probe(source)?;
    if info.float {
        return Err("already a float WAV".into());
    }
    let part = part_path(out);
    let result = write_wav(source, &info, &part, cancel).and_then(|encoded| {
        let mut hasher = Md5::new();
        let frames = audio::read_pcm_as(&part, Some(Format::Wav), cancel, |chunk| hasher.update(chunk))?;
        let md5: [u8; 16] = hasher.finalize().into();
        if md5 != encoded.md5 || frames != encoded.samples {
            return Err("verification failed: the WAV file does not hold the source audio".into());
        }
        Ok(encoded)
    });
    match result {
        Ok(encoded) => {
            fs::rename(&part, out).map_err(|e| e.to_string())?;
            Ok(encoded)
        }
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

fn wav_header(info: &AudioInfo, data_len: u64) -> Vec<u8> {
    let (channels, bits, rate) = (info.channels as u16, info.bits as u16, info.rate);
    let container = bits.div_ceil(8) * 8;
    let align = channels * container / 8;
    let extensible = bits > 16 || channels > 2;
    let fmt_len: u32 = if extensible { 40 } else { 16 };
    let mut h = Vec::with_capacity(68);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&((4 + 8 + fmt_len + 8) as u64 + data_len + (data_len & 1)).min(u32::MAX as u64).to_le_bytes()[..4]);
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&fmt_len.to_le_bytes());
    h.extend_from_slice(&(if extensible { 0xFFFEu16 } else { 1 }).to_le_bytes());
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * align as u32).to_le_bytes());
    h.extend_from_slice(&align.to_le_bytes());
    h.extend_from_slice(&container.to_le_bytes());
    if extensible {
        let mask: u32 = match channels {
            1 => 0x4,
            2 => 0x3,
            4 => 0x33,
            6 => 0x3F,
            _ => 0,
        };
        h.extend_from_slice(&22u16.to_le_bytes());
        h.extend_from_slice(&bits.to_le_bytes());
        h.extend_from_slice(&mask.to_le_bytes());
        // KSDATAFORMAT_SUBTYPE_PCM
        h.extend_from_slice(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71]);
    }
    h.extend_from_slice(b"data");
    h.extend_from_slice(&(data_len.min(u32::MAX as u64) as u32).to_le_bytes());
    h
}

fn write_wav(source: &Path, info: &AudioInfo, part: &Path, cancel: &AtomicBool) -> Result<Encoded, String> {
    let io = |e: std::io::Error| e.to_string();
    let mut file = BufWriter::new(File::create(part).map_err(io)?);
    let header_len = wav_header(info, 0).len();
    file.write_all(&wav_header(info, 0)).map_err(io)?;
    let mut hasher = Md5::new();
    let mut data_len = 0u64;
    let mut failure = None;
    let frames = audio::read_pcm(source, cancel, |chunk| {
        if failure.is_none() {
            hasher.update(chunk);
            data_len += chunk.len() as u64;
            if let Err(e) = file.write_all(chunk) {
                failure = Some(e.to_string());
            }
        }
    })?;
    if let Some(e) = failure {
        return Err(e);
    }
    if data_len > u32::MAX as u64 - 64 {
        return Err("too long for a WAV file (over 4 GB)".into());
    }
    if data_len & 1 == 1 {
        file.write_all(&[0]).map_err(io)?;
    }
    let mut file = file.into_inner().map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(0)).map_err(io)?;
    let header = wav_header(info, data_len);
    debug_assert_eq!(header.len(), header_len);
    file.write_all(&header).map_err(io)?;
    file.sync_all().map_err(io)?;
    Ok(Encoded { samples: frames, md5: hasher.finalize().into() })
}

/// MP3 settings: constant 320 kbps, or VBR -V0.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mp3Mode {
    Cbr320,
    V0,
}

impl Mp3Mode {
    /// As in the folder name: "<show> [MP3 320]".
    pub fn tag(self) -> &'static str {
        match self {
            Mp3Mode::Cbr320 => "MP3 320",
            Mp3Mode::V0 => "MP3 V0",
        }
    }
}

/// MP3 copies are made from mono or stereo files only.
pub fn check_mp3(info: &AudioInfo) -> Result<(), String> {
    if info.channels == 0 || info.channels > 2 {
        return Err(format!("{} channels: MP3 copies are made from mono or stereo files only", info.channels));
    }
    Ok(())
}

/// Encodes `source` to MP3 with LAME, through a ".part" file. Above 48 kHz the audio is
/// resampled by LAME (48 or 44.1 kHz); tags become an ID3v2.3 tag.
pub fn to_mp3(source: &Path, out: &Path, mode: Mp3Mode, tags: &[(String, String)], cancel: &AtomicBool) -> Result<u64, String> {
    let info = audio::probe(source)?;
    check_mp3(&info)?;
    let part = part_path(out);
    let result = write_mp3(source, &info, &part, mode, tags, cancel);
    match result {
        Ok(frames) => {
            fs::rename(&part, out).map_err(|e| e.to_string())?;
            Ok(frames)
        }
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

fn write_mp3(source: &Path, info: &AudioInfo, part: &Path, mode: Mp3Mode, tags: &[(String, String)], cancel: &AtomicBool) -> Result<u64, String> {
    use mp3lame_encoder::{Bitrate, Builder, FlushGap, InterleavedPcm, MonoPcm, Quality, VbrMode};
    let lame = |e: &dyn std::fmt::Debug| format!("MP3 encoder: {e:?}");
    let mut b = Builder::new().ok_or("MP3 encoder: cannot start LAME")?;
    b.set_num_channels(info.channels as u8).map_err(|e| lame(&e))?;
    b.set_sample_rate(info.rate).map_err(|e| lame(&e))?;
    if info.rate > 48000 {
        // MP3 stops at 48 kHz.
        let rate = if info.rate % 48000 == 0 { 48000 } else { 44100 };
        b.set_output_sample_rate(std::num::NonZeroU32::new(rate)).map_err(|e| lame(&e))?;
    }
    b.set_quality(Quality::NearBest).map_err(|e| lame(&e))?;
    match mode {
        Mp3Mode::Cbr320 => {
            b.set_vbr_mode(VbrMode::Off).map_err(|e| lame(&e))?;
            b.set_brate(Bitrate::Kbps320).map_err(|e| lame(&e))?;
        }
        Mp3Mode::V0 => {
            b.set_vbr_mode(VbrMode::Mtrh).map_err(|e| lame(&e))?;
            b.set_vbr_quality(Quality::Best).map_err(|e| lame(&e))?;
        }
    }
    // The LAME/Xing header frame (length, VBR seek table) is filled in at the end.
    b.set_to_write_vbr_tag(true).map_err(|e| lame(&e))?;
    let mut encoder = b.build().map_err(|e| lame(&e))?;

    let io = |e: std::io::Error| e.to_string();
    let mut file = BufWriter::new(File::create(part).map_err(io)?);
    let id3 = id3v23(tags);
    file.write_all(&id3).map_err(io)?;
    let channels = info.channels as usize;
    let mut buf: Vec<u8> = Vec::with_capacity(1 << 16);
    let mut samples: Vec<f32> = Vec::with_capacity(1 << 15);
    let mut frames = 0u64;
    let mut failure = None;
    crate::dsp::for_each_frame(source, info, cancel, |values, _| {
        if failure.is_some() {
            return;
        }
        samples.extend(values.iter().map(|v| *v as f32));
        if samples.len() >= 4096 * channels {
            frames += (samples.len() / channels) as u64;
            buf.clear();
            let r = if channels == 2 { encoder.encode_to_vec(InterleavedPcm(&samples), &mut buf) } else { encoder.encode_to_vec(MonoPcm(&samples), &mut buf) };
            samples.clear();
            match r {
                Ok(_) => {
                    if let Err(e) = file.write_all(&buf) {
                        failure = Some(e.to_string());
                    }
                }
                Err(e) => failure = Some(lame(&e)),
            }
        }
    })?;
    if let Some(e) = failure {
        return Err(e);
    }
    frames += (samples.len() / channels) as u64;
    buf.clear();
    if !samples.is_empty() {
        let r = if channels == 2 { encoder.encode_to_vec(InterleavedPcm(&samples), &mut buf) } else { encoder.encode_to_vec(MonoPcm(&samples), &mut buf) };
        r.map_err(|e| lame(&e))?;
    }
    encoder.flush_to_vec::<FlushGap>(&mut buf).map_err(|e| lame(&e))?;
    file.write_all(&buf).map_err(io)?;
    // The crate writes into the spare capacity of the vector.
    let mut tag_frame = Vec::with_capacity(encoder.lame_tag_size());
    let tag_len = encoder.lame_tag_encode_to_vec(&mut tag_frame);
    let mut file = file.into_inner().map_err(|e| e.to_string())?;
    if tag_len.is_some() && !tag_frame.is_empty() {
        file.seek(SeekFrom::Start(id3.len() as u64)).map_err(io)?;
        file.write_all(&tag_frame).map_err(io)?;
    }
    file.sync_all().map_err(io)?;
    Ok(frames)
}

/// An ID3v2.3 tag (text in UTF-16, as Windows Explorer reads it) from Vorbis-style tags.
fn id3v23(tags: &[(String, String)]) -> Vec<u8> {
    let get = |k: &str| tags.iter().find(|(name, _)| name.eq_ignore_ascii_case(k)).map(|(_, v)| v.trim().to_string()).filter(|v| !v.is_empty());
    let with_total = |n: Option<String>, total: Option<String>| match (n, total) {
        (Some(n), Some(t)) if !n.contains('/') => Some(format!("{n}/{t}")),
        (n, _) => n,
    };
    let year = get("DATE").or_else(|| get("YEAR")).map(|d| d.chars().take(4).collect::<String>()).filter(|y| y.len() == 4 && y.chars().all(|c| c.is_ascii_digit()));
    let frames: Vec<(&str, Option<String>)> = vec![
        ("TIT2", get("TITLE")),
        ("TPE1", get("ARTIST")),
        ("TPE2", get("ALBUMARTIST").or_else(|| get("ALBUM ARTIST"))),
        ("TALB", get("ALBUM")),
        ("TYER", year),
        ("TRCK", with_total(get("TRACKNUMBER"), get("TRACKTOTAL").or_else(|| get("TOTALTRACKS")))),
        ("TPOS", with_total(get("DISCNUMBER"), get("DISCTOTAL").or_else(|| get("TOTALDISCS")))),
        ("TCON", get("GENRE")),
        ("COMM", get("COMMENT").or_else(|| get("DESCRIPTION"))),
    ];
    let utf16 = |text: &str| -> Vec<u8> {
        let mut v = vec![0xFF, 0xFE];
        for u in text.encode_utf16() {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v
    };
    let mut body = Vec::new();
    for (id, value) in frames {
        let Some(value) = value else { continue };
        let mut data = vec![1u8]; // UTF-16 with BOM
        if id == "COMM" {
            data.extend_from_slice(b"eng");
            data.extend_from_slice(&utf16(""));
            data.extend_from_slice(&[0, 0]);
        }
        data.extend_from_slice(&utf16(&value));
        body.extend_from_slice(id.as_bytes());
        body.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&data);
    }
    if body.is_empty() {
        return Vec::new();
    }
    let size = body.len() as u32;
    let synchsafe = [(size >> 21) as u8 & 0x7f, (size >> 14) as u8 & 0x7f, (size >> 7) as u8 & 0x7f, size as u8 & 0x7f];
    let mut tag = b"ID3\x03\x00\x00".to_vec();
    tag.extend_from_slice(&synchsafe);
    tag.extend_from_slice(&body);
    tag
}

/// Decodes the new file and checks it gives back exactly the source audio.
fn verify(flac: &Path, encoded: &Encoded, cancel: &AtomicBool) -> Result<(), String> {
    let info = audio::probe_as(flac, Some(Format::Flac))?;
    if info.md5 != Some(encoded.md5) || info.samples != encoded.samples {
        return Err("verification failed: the FLAC header does not match the source".into());
    }
    let mut hasher = Md5::new();
    let frames = audio::read_pcm_as(flac, Some(Format::Flac), cancel, |chunk| hasher.update(chunk))?;
    let md5: [u8; 16] = hasher.finalize().into();
    if md5 != encoded.md5 || frames != encoded.samples {
        return Err("verification failed: the FLAC file does not decode to the source audio".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_wav_to_flac() {
        let dir = std::env::temp_dir().join(format!("bh-encode-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // A 16-bit stereo WAV: a sweep, odd length to test the last short frame.
        let frames = 44100 + 777;
        let mut data = Vec::new();
        for i in 0..frames {
            let t = i as f64 / 44100.0;
            let l = (8000.0 * (2.0 * std::f64::consts::PI * (200.0 + 2000.0 * t) * t).sin()) as i16;
            let r = (l as i32 / 2 + (i % 7) as i32) as i16;
            data.extend_from_slice(&l.to_le_bytes());
            data.extend_from_slice(&r.to_le_bytes());
        }
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&44100u32.to_le_bytes());
        wav.extend_from_slice(&(44100u32 * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        let src = dir.join("in.wav");
        fs::write(&src, &wav).unwrap();
        let cancel = AtomicBool::new(false);
        let (src_md5, _) = audio::pcm_md5(&src, &cancel).unwrap();
        for level in [0, 5, 8] {
            let out = dir.join(format!("out{level}.flac"));
            let encoded = to_flac(&src, &out, level, &[("ARTIST".into(), "Test".into())], &cancel).unwrap();
            assert_eq!(encoded.samples, frames as u64);
            assert_eq!(encoded.md5, src_md5);
            assert!(out.exists() && !part_path(&out).exists());
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}
