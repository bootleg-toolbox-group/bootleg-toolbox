//! Shorten (.shn) decoder, versions 0 to 3, in pure Rust.
//!
//! A Shorten file is a bit stream (most significant bit first) of Rice-coded values:
//! the "ajkg" magic and a version byte, a few parameters, the original WAV header kept
//! verbatim, then commands, each block of samples predicted per channel (fixed
//! polynomials or quantised LPC) from the previous ones, with a running mean.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const TRUNCATED: &str = "truncated: the file ends before the end of the audio";

const FN_DIFF0: u32 = 0;
const FN_DIFF3: u32 = 3;
const FN_QUIT: u32 = 4;
const FN_BLOCKSIZE: u32 = 5;
const FN_BITSHIFT: u32 = 6;
const FN_QLPC: u32 = 7;
const FN_ZERO: u32 = 8;
const FN_VERBATIM: u32 = 9;

const FNSIZE: u32 = 2;
const ENERGYSIZE: u32 = 3;
const BITSHIFTSIZE: u32 = 2;
const LPCQSIZE: u32 = 2;
const LPCQUANT: u32 = 5;
const VERBATIM_CKSIZE_SIZE: u32 = 5;
const VERBATIM_BYTE_SIZE: u32 = 8;
const TYPESIZE: u32 = 4;
const CHANSIZE: u32 = 0;
const NSKIPSIZE: u32 = 1;
const ULONGSIZE: u32 = 2;
const DEFAULT_BLOCK_SIZE: u32 = 256;
const NWRAP: usize = 3;
const MAX_CHANNELS: u32 = 8;
const MAX_BLOCK_SIZE: u32 = 65535;
const MAX_LPC_ORDER: u32 = 32;

/// Sample types (the "file type" of the original audio).
const TYPE_S8: u32 = 1;
const TYPE_U8: u32 = 2;
const TYPE_S16HL: u32 = 3;
const TYPE_S16LH: u32 = 5;

/// What the header tells: format of the original WAV and its announced length.
#[derive(Clone, Debug)]
pub struct ShnHeader {
    pub channels: u32,
    pub rate: u32,
    pub bits: u32,
    /// Sample frames announced by the WAV header kept in the file (0 if unknown).
    pub samples: u64,
}

/// Most significant bit first reader with a 64-bit cache.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    cache: u64,
    avail: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0, cache: 0, avail: 0 }
    }

    fn refill(&mut self) {
        while self.avail <= 56 && self.pos < self.data.len() {
            self.cache |= (self.data[self.pos] as u64) << (56 - self.avail);
            self.pos += 1;
            self.avail += 8;
        }
    }

    fn bits(&mut self, n: u32) -> Result<u32, String> {
        if n == 0 {
            return Ok(0);
        }
        if self.avail < n {
            self.refill();
            if self.avail < n {
                return Err(TRUNCATED.into());
            }
        }
        let v = (self.cache >> (64 - n)) as u32;
        self.cache <<= n;
        self.avail -= n;
        Ok(v)
    }

    /// Unsigned Rice code: a run of zeros ended by a one (the high part), then `n` low bits.
    fn uvar(&mut self, n: u32) -> Result<u32, String> {
        let mut high = 0u32;
        loop {
            if self.avail == 0 {
                self.refill();
                if self.avail == 0 {
                    return Err(TRUNCATED.into());
                }
            }
            let zeros = self.cache.leading_zeros().min(self.avail);
            high += zeros;
            if zeros < self.avail {
                self.cache = self.cache.checked_shl(zeros + 1).unwrap_or(0);
                self.avail -= zeros + 1;
                break;
            }
            self.cache = 0;
            self.avail = 0;
            if high > 1 << 24 {
                return Err("corrupt data (endless code)".into());
            }
        }
        let low = self.bits(n)?;
        Ok(high.wrapping_shl(n) | low)
    }

    /// Signed Rice code: the low bit of the unsigned value is the sign.
    fn svar(&mut self, n: u32) -> Result<i32, String> {
        let u = self.uvar(n + 1)?;
        Ok(if u & 1 == 1 { !((u >> 1) as i32) } else { (u >> 1) as i32 })
    }

    /// From version 1 on, numbers carry their own bit width.
    fn ulong(&mut self, version: u8, n: u32) -> Result<u32, String> {
        let n = if version > 0 { self.uvar(ULONGSIZE)? } else { n };
        if n > 31 {
            return Err("corrupt header".into());
        }
        self.uvar(n)
    }
}

struct Params {
    version: u8,
    ftype: u32,
    channels: usize,
    blocksize: usize,
    nmean: usize,
    nwrap: usize,
    lpcqoffset: i32,
}

fn read_params(bits: &mut Bits) -> Result<Params, String> {
    if bits.bits(32)? != u32::from_be_bytes(*b"ajkg") {
        return Err("not a Shorten file (no ajkg marker)".into());
    }
    let version = bits.bits(8)? as u8;
    if version > 3 {
        return Err(format!("Shorten version {version} is not supported"));
    }
    let ftype = bits.ulong(version, TYPESIZE)?;
    let channels = bits.ulong(version, CHANSIZE)?;
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(format!("unsupported channel count ({channels})"));
    }
    let (mut blocksize, mut maxnlpc, mut nmean) = (DEFAULT_BLOCK_SIZE, 0, None);
    if version > 0 {
        blocksize = bits.ulong(version, DEFAULT_BLOCK_SIZE.ilog2())?;
        maxnlpc = bits.ulong(version, LPCQSIZE)?;
        nmean = Some(bits.ulong(version, 0)?);
        let skip = bits.ulong(version, NSKIPSIZE)?;
        for _ in 0..skip {
            bits.bits(8)?;
        }
    }
    if blocksize == 0 || blocksize > MAX_BLOCK_SIZE || maxnlpc > MAX_LPC_ORDER {
        return Err("corrupt header".into());
    }
    Ok(Params {
        version,
        ftype,
        channels: channels as usize,
        blocksize: blocksize as usize,
        nmean: nmean.unwrap_or(if version < 2 { 0 } else { 4 }) as usize,
        nwrap: NWRAP.max(maxnlpc as usize),
        lpcqoffset: if version > 1 { 1 << LPCQUANT } else { 0 },
    })
}

/// The original WAV header, kept verbatim at the start of the stream.
fn read_verbatim(bits: &mut Bits) -> Result<Vec<u8>, String> {
    if bits.uvar(FNSIZE)? != FN_VERBATIM {
        return Err("corrupt file: no header at the beginning".into());
    }
    let len = bits.uvar(VERBATIM_CKSIZE_SIZE)? as usize;
    if len > 1 << 20 {
        return Err("corrupt header".into());
    }
    (0..len).map(|_| bits.uvar(VERBATIM_BYTE_SIZE).map(|b| b as u8)).collect()
}

/// Rate, bit depth, channels and announced length from the kept WAV (or AIFF) header.
fn parse_wave_header(h: &[u8], channels: u32) -> Result<ShnHeader, String> {
    let u16le = |i: usize| h.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as u32);
    let u32le = |i: usize| h.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if h.len() >= 12 && &h[0..4] == b"RIFF" && &h[8..12] == b"WAVE" {
        let (mut pos, mut fmt) = (12usize, None);
        while pos + 8 <= h.len() {
            let size = u32le(pos + 4).unwrap_or(0) as usize;
            match &h[pos..pos + 4] {
                b"fmt " => {
                    let ch = u16le(pos + 10).unwrap_or(0);
                    let rate = u32le(pos + 12).unwrap_or(0);
                    let align = u16le(pos + 20).unwrap_or(0);
                    let bits = u16le(pos + 22).unwrap_or(0);
                    fmt = Some((ch, rate, align, bits));
                }
                b"data" => {
                    let (ch, rate, align, bits) = fmt.ok_or("corrupt WAV header in the Shorten file")?;
                    let samples = if align > 0 && size != 0 && size != u32::MAX as usize { (size / align as usize) as u64 } else { 0 };
                    return Ok(ShnHeader { channels: ch.max(channels), rate, bits, samples });
                }
                _ => {}
            }
            pos += 8 + size + (size & 1);
        }
        return Err("corrupt WAV header in the Shorten file".into());
    }
    if h.len() >= 12 && &h[0..4] == b"FORM" && (&h[8..12] == b"AIFF" || &h[8..12] == b"AIFC") {
        let u16be = |i: usize| h.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as u32);
        let u32be = |i: usize| h.get(i..i + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        let mut pos = 12usize;
        while pos + 8 <= h.len() {
            let size = u32be(pos + 4).unwrap_or(0) as usize;
            if &h[pos..pos + 4] == b"COMM" && pos + 26 <= h.len() {
                let ch = u16be(pos + 8).unwrap_or(0);
                let frames = u32be(pos + 10).unwrap_or(0) as u64;
                let bits = u16be(pos + 14).unwrap_or(0);
                // 80-bit extended float sample rate
                let exp = u16be(pos + 16).unwrap_or(0) as i32 & 0x7fff;
                let mant = h.get(pos + 18..pos + 26).map(|b| u64::from_be_bytes(b.try_into().unwrap())).unwrap_or(0);
                let rate = (mant as f64 * 2f64.powi(exp - 16383 - 63)).round() as u32;
                return Ok(ShnHeader { channels: ch.max(channels), rate, bits, samples: frames });
            }
            pos += 8 + size + (size & 1);
        }
    }
    Err("unsupported header in the Shorten file (neither WAV nor AIFF)".into())
}

/// Reads the parameters and the kept header only (fast).
pub fn probe(path: &Path) -> Result<ShnHeader, String> {
    let mut start = vec![0u8; 1 << 16];
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let n = file.read(&mut start).map_err(|e| e.to_string())?;
    start.truncate(n);
    let mut bits = Bits::new(&start);
    let params = read_params(&mut bits)?;
    let header = read_verbatim(&mut bits)?;
    let info = parse_wave_header(&header, params.channels as u32)?;
    sample_writer(params.ftype, info.bits)?;
    Ok(info)
}

/// Bytes of one sample as in a WAV data chunk (little-endian; 8-bit unsigned).
fn sample_writer(ftype: u32, bits: u32) -> Result<fn(i32, &mut Vec<u8>), String> {
    match (ftype, bits) {
        (TYPE_S16HL | TYPE_S16LH, 16) => Ok(|v, out| out.extend_from_slice(&(v as i16).to_le_bytes())),
        (TYPE_U8, 8) => Ok(|v, out| out.push((v + 128) as u8)),
        (TYPE_S8, 8) => Ok(|v, out| out.push(v as u8 ^ 0x80)),
        _ => Err(format!("unsupported Shorten sample type ({ftype}, {bits}-bit)")),
    }
}

/// Decodes the whole file; `sink` gets the PCM as in a WAV data chunk. Returns the
/// number of sample frames.
pub fn decode(path: &Path, cancel: &AtomicBool, mut sink: impl FnMut(&[u8])) -> Result<u64, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut bits = Bits::new(&data);
    let p = read_params(&mut bits)?;
    let header = read_verbatim(&mut bits)?;
    let info = parse_wave_header(&header, p.channels as u32)?;
    let write = sample_writer(p.ftype, info.bits)?;

    let mut blocksize = p.blocksize;
    // Per channel: `nwrap` previous samples, then the current block.
    let mut buf: Vec<Vec<i32>> = vec![vec![0; p.nwrap + MAX_BLOCK_SIZE as usize]; p.channels];
    let mut means: Vec<Vec<i32>> = vec![vec![0; p.nmean.max(1)]; p.channels];
    // Output samples of the current block, per channel, with the bit shift in force when
    // that channel was decoded (it may change between the channels of a block).
    let mut shifted: Vec<Vec<i32>> = vec![vec![0; MAX_BLOCK_SIZE as usize]; p.channels];
    let mut bitshift = 0u32;
    let mut channel = 0usize;
    let mut frames = 0u64;
    let mut out = Vec::with_capacity(MAX_BLOCK_SIZE as usize * p.channels * 2);
    // Fixed polynomial predictors of orders 0 to 3.
    let fixed: [[i32; 3]; 4] = [[0, 0, 0], [1, 0, 0], [2, -1, 0], [3, -3, 1]];

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let cmd = bits.uvar(FNSIZE)?;
        match cmd {
            FN_DIFF0..=FN_DIFF3 | FN_QLPC | FN_ZERO => {
                let residual_bits = if cmd == FN_ZERO {
                    0
                } else {
                    let r = bits.uvar(ENERGYSIZE)?;
                    if p.version == 0 { r.wrapping_sub(1) } else { r }
                };
                if residual_bits > 31 {
                    return Err("corrupt data".into());
                }
                // Running mean of the previous blocks.
                let offset = if p.nmean == 0 {
                    means[channel][0]
                } else {
                    let base: i64 = if p.version < 2 { 0 } else { p.nmean as i64 / 2 };
                    let sum: i64 = base + means[channel][..p.nmean].iter().map(|&m| m as i64).sum::<i64>();
                    let mean = (sum / p.nmean as i64) as i32;
                    if p.version >= 2 { mean >> bitshift.min(31) } else { mean }
                };
                let s = &mut buf[channel];
                let w = p.nwrap;
                if cmd == FN_ZERO {
                    s[w..w + blocksize].iter_mut().for_each(|x| *x = 0);
                } else {
                    let mut coeffs = [0i32; MAX_LPC_ORDER as usize];
                    let (order, qshift, init) = if cmd == FN_QLPC {
                        let order = bits.uvar(LPCQSIZE)? as usize;
                        if order > p.nwrap || order > coeffs.len() {
                            return Err("corrupt data (prediction order)".into());
                        }
                        for c in coeffs.iter_mut().take(order) {
                            *c = bits.svar(LPCQUANT)?;
                        }
                        (order, LPCQUANT, if order > 0 { p.lpcqoffset } else { offset })
                    } else {
                        let order = cmd as usize;
                        coeffs[..order].copy_from_slice(&fixed[order][..order]);
                        (order, 0, if order > 0 { 0 } else { offset })
                    };
                    if cmd == FN_QLPC && offset != 0 {
                        for x in &mut s[w - order..w] {
                            *x = x.wrapping_sub(offset);
                        }
                    }
                    for i in w..w + blocksize {
                        let mut sum = init as i64;
                        for (j, c) in coeffs[..order].iter().enumerate() {
                            sum += *c as i64 * s[i - j - 1] as i64;
                        }
                        let residual = bits.svar(residual_bits)?;
                        s[i] = residual.wrapping_add((sum >> qshift) as i32);
                    }
                    if cmd == FN_QLPC && offset != 0 {
                        for x in &mut s[w..w + blocksize] {
                            *x = x.wrapping_add(offset);
                        }
                    }
                }
                // Update the running mean.
                if p.nmean > 0 {
                    let base: i64 = if p.version < 2 { 0 } else { blocksize as i64 / 2 };
                    let sum: i64 = base + s[w..w + blocksize].iter().map(|&x| x as i64).sum::<i64>();
                    let m = &mut means[channel];
                    m.copy_within(1..p.nmean, 0);
                    let mean = sum / blocksize as i64;
                    m[p.nmean - 1] = if p.version < 2 { mean as i32 } else { (mean << bitshift.min(31)) as i32 };
                }
                // Samples come back with their unused low bits.
                for (o, x) in shifted[channel][..blocksize].iter_mut().zip(&s[w..w + blocksize]) {
                    *o = if bitshift >= 32 { 0 } else { x.wrapping_shl(bitshift) };
                }
                // Keep the last samples for the next block's prediction.
                s.copy_within(blocksize..blocksize + w, 0);
                channel += 1;
                if channel == p.channels {
                    channel = 0;
                    out.clear();
                    for i in 0..blocksize {
                        for c in &shifted {
                            write(c[i], &mut out);
                        }
                    }
                    sink(&out);
                    frames += blocksize as u64;
                }
            }
            FN_QUIT => break,
            FN_BLOCKSIZE => {
                let size = bits.ulong(p.version, (blocksize as u32).max(1).ilog2())? as usize;
                if size == 0 || size > MAX_BLOCK_SIZE as usize {
                    return Err("corrupt data (block size)".into());
                }
                blocksize = size;
            }
            FN_BITSHIFT => {
                bitshift = bits.uvar(BITSHIFTSIZE)?;
                if bitshift > 32 {
                    return Err("corrupt data (bit shift)".into());
                }
            }
            FN_VERBATIM => {
                let len = bits.uvar(VERBATIM_CKSIZE_SIZE)?;
                for _ in 0..len {
                    bits.uvar(VERBATIM_BYTE_SIZE)?;
                }
            }
            other => return Err(format!("damaged data: decoding lost its way (unknown command {other})")),
        }
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rice_codes() {
        // uvar(2): "001" + "10" = 2 << 2 | 2 = 10; svar(1) via uvar(2): "1" + "11" = 3 -> !(1) = -2;
        // then 8 plain bits.
        let data = [0b0011_0111, 0b1010_0101, 0b1000_0000];
        let mut b = Bits::new(&data);
        assert_eq!(b.uvar(2).unwrap(), 10);
        assert_eq!(b.svar(1).unwrap(), -2);
        assert_eq!(b.bits(8).unwrap(), 0b1010_0101);
        assert_eq!(b.uvar(0).unwrap(), 0);
        assert!(b.uvar(0).is_err());
    }
}
