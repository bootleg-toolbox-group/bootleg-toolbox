//! WavPack (.wv) decoding with the `wavicle` crate, one block at a time: WavPack blocks
//! decode independently, so memory stays small whatever the length of the file.
//! Lossless mono/stereo, 16 to 32-bit integer and 32-bit float; hybrid (lossy) files,
//! DSD, more than two channels and streams older than 4.0 are refused with a message.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use wavicle::{BlockHeader, Error, Scope};

const MAGIC: &[u8; 4] = b"wvpk";
const HEADER_LEN: usize = 32;

pub struct WvInfo {
    pub rate: u32,
    pub bits: u32,
    pub channels: u32,
    pub float: bool,
    /// Sample frames announced by the first block (0 if unknown).
    pub samples: u64,
}

fn describe(e: Error) -> String {
    match e {
        Error::OutOfScope(Scope::Hybrid) => {
            "hybrid WavPack (lossy part only; the lossless audio also needs its .wvc correction file): not supported".into()
        }
        Error::OutOfScope(Scope::Dsd) => "DSD WavPack is not supported".into(),
        Error::OutOfScope(other) => format!("this kind of WavPack file is not supported ({other:?})"),
        Error::UnsupportedVersion(v) => format!("WavPack stream version {v:#x} is not supported (4.0 and later only)"),
        Error::CrcMismatch { .. } => "damaged data: a block does not match its checksum".into(),
        Error::OverMagnitude => "damaged data: impossible sample values".into(),
        Error::Truncated { .. } => "damaged or truncated: a block runs past the end of the file".into(),
        Error::NotYetImplemented(what) => format!("not supported: {what}"),
        other => format!("damaged or unsupported WavPack data ({other:?})"),
    }
}

/// The blocks of the stream (bytes from "wvpk" to the end of each block). Leading bytes
/// before the first block and trailing tags after the last one are skipped.
fn blocks(data: &[u8]) -> impl Iterator<Item = Result<&[u8], String>> {
    let mut pos = data.windows(4).position(|w| w == MAGIC).unwrap_or(data.len());
    std::iter::from_fn(move || {
        if pos + HEADER_LEN > data.len() || &data[pos..pos + 4] != MAGIC {
            return None;
        }
        let header = match BlockHeader::parse(&data[pos..]) {
            Ok(h) => h,
            Err(e) => {
                pos = data.len();
                return Some(Err(describe(e)));
            }
        };
        let len = header.block_len();
        if pos + len > data.len() {
            pos = data.len();
            return Some(Err(describe(Error::Truncated { need: len, have: data.len() - pos })));
        }
        let block = &data[pos..pos + len];
        pos += len;
        Some(Ok(block))
    })
}

/// Format and announced length, from the first audio block.
pub fn probe(path: &Path) -> Result<WvInfo, String> {
    let mut start = Vec::new();
    File::open(path).map_err(|e| e.to_string())?.take(4 << 20).read_to_end(&mut start).map_err(|e| e.to_string())?;
    for block in blocks(&start) {
        let block = block?;
        let header = BlockHeader::parse(block).map_err(describe)?;
        if header.block_samples == 0 {
            continue;
        }
        let decoded = wavicle::decode_stream(block).map_err(describe)?;
        return Ok(WvInfo {
            rate: decoded.sample_rate,
            bits: decoded.bits_per_sample,
            channels: decoded.channels,
            float: decoded.is_float,
            samples: header.total_samples.unwrap_or(0),
        });
    }
    Err("not a WavPack file (no audio block found)".into())
}

/// Decodes the whole file block by block; `sink` gets PCM as in a WAV data chunk
/// (little-endian; float samples as 32-bit IEEE). Returns the number of sample frames.
pub fn decode(path: &Path, cancel: &AtomicBool, mut sink: impl FnMut(&[u8])) -> Result<u64, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut frames = 0u64;
    let mut out = Vec::new();
    for block in blocks(&data) {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let block = block?;
        let header = BlockHeader::parse(block).map_err(describe)?;
        if header.block_samples == 0 {
            continue;
        }
        let decoded = wavicle::decode_stream(block).map_err(describe)?;
        let width = (decoded.bits_per_sample as usize / 8).clamp(1, 4);
        out.clear();
        for s in &decoded.samples {
            out.extend_from_slice(&s.to_le_bytes()[..width]);
        }
        frames += (decoded.samples.len() / decoded.channels.max(1) as usize) as u64;
        sink(&out);
    }
    Ok(frames)
}
