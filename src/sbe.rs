//! Sector boundary errors (SBE): fixing a disc as `shntool fix` does.
//!
//! The tracks of a disc are seen as one stream. Every boundary between two tracks moves
//! back to the previous CD sector (588 sample frames): the audio past it goes to the start
//! of the next track. The last track ends with silence up to the end of its sector.
//! Each new track only depends on the lengths of the tracks, so they are written in
//! parallel: a track takes its end from the source tracks it overlaps.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::audio::{self, AudioInfo, Format};
use crate::encode::{self, Encoded};

/// Sample frames in a CD sector (2352 bytes of 16-bit stereo).
pub const SECTOR: u64 = 588;
const FRAME_BYTES: u64 = 4;

/// One new track: frames [start, end) of the disc stream, then `pad` frames of silence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cut {
    pub start: u64,
    pub end: u64,
    pub pad: u64,
}

impl Cut {
    pub fn frames(&self) -> u64 {
        self.end - self.start + self.pad
    }
}

/// The new tracks from the track lengths: None for a track that stays as it is.
pub fn plan(lengths: &[u64]) -> Vec<Option<Cut>> {
    let mut starts = Vec::with_capacity(lengths.len() + 1);
    let mut total = 0;
    for len in lengths {
        starts.push(total);
        total += len;
    }
    starts.push(total);
    let last = lengths.len().saturating_sub(1);
    (0..lengths.len())
        .map(|i| {
            let start = starts[i] / SECTOR * SECTOR;
            let (end, pad) = if i == last {
                (total, (SECTOR - total % SECTOR) % SECTOR)
            } else {
                (starts[i + 1] / SECTOR * SECTOR, 0)
            };
            let unchanged = start == starts[i] && end == starts[i + 1] && pad == 0;
            (!unchanged).then_some(Cut { start, end, pad })
        })
        .collect()
}

/// Whether a disc needs fixing: an SBE on a track other than the last one.
pub fn needs_fix(lengths: &[u64]) -> bool {
    let n = lengths.len();
    lengths.iter().take(n.saturating_sub(1)).any(|len| len % SECTOR != 0)
}

/// Writes the new track `cut` of a disc (`tracks`: path and length in frames) to `out`
/// as FLAC, verified. Every source track it overlaps is decoded in full and must give
/// the length its header announces.
pub fn write_track(
    tracks: &[(PathBuf, u64)],
    cut: Cut,
    out: &Path,
    level: u8,
    tags: &[(String, String)],
    cancel: &AtomicBool,
) -> Result<Encoded, String> {
    if cut.end <= cut.start {
        return Err("the track is shorter than a CD sector once its boundaries are moved".into());
    }
    let info = AudioInfo { format: Format::Flac, rate: 44100, bits: 16, channels: 2, samples: cut.frames(), md5: None, float: false };
    encode::pcm_to_flac(out, &info, level, tags, cancel, |sink| {
        let mut offset = 0u64;
        for (path, len) in tracks {
            let (first, past) = (offset, offset + len);
            offset = past;
            if past <= cut.start || first >= cut.end {
                continue;
            }
            // Byte range of this source inside the new track.
            let from = (cut.start.max(first) - first) * FRAME_BYTES;
            let to = (cut.end.min(past) - first) * FRAME_BYTES;
            let mut pos = 0u64;
            let frames = audio::read_pcm(path, cancel, |chunk| {
                let (a, b) = (pos, pos + chunk.len() as u64);
                pos = b;
                if b > from && a < to {
                    sink(&chunk[(from.max(a) - a) as usize..(to.min(b) - a) as usize]);
                }
            })?;
            if frames != *len {
                return Err(format!("{}: decoded {frames} samples, header says {len}", crate::files::file_name(path)));
            }
        }
        let silence = vec![0u8; 4096 * FRAME_BYTES as usize];
        let mut left = cut.pad * FRAME_BYTES;
        while left > 0 {
            let n = left.min(silence.len() as u64) as usize;
            sink(&silence[..n]);
            left -= n as u64;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries_move_back() {
        // 1000 = 588 + 412: the 412 frames move to the next track.
        let p = plan(&[1000, 1000, 1000]);
        assert_eq!(p[0], Some(Cut { start: 0, end: 588, pad: 0 }));
        assert_eq!(p[1], Some(Cut { start: 588, end: 1764, pad: 0 }));
        // 3000 frames in all: the last track is padded to 3528 (6 sectors).
        assert_eq!(p[2], Some(Cut { start: 1764, end: 3000, pad: 528 }));
        assert_eq!(p.iter().flatten().map(Cut::frames).sum::<u64>(), 3528);
    }

    #[test]
    fn aligned_tracks_stay() {
        let p = plan(&[588 * 3, 588 * 2 + 5, 588 * 4, 100]);
        assert_eq!(p[0], None);
        assert_eq!(p[1], Some(Cut { start: 1764, end: 2940, pad: 0 }));
        assert_eq!(p[2], Some(Cut { start: 2940, end: 5292, pad: 0 }));
        assert_eq!(p[3], Some(Cut { start: 5292, end: 5397, pad: 483 }));
        assert!(needs_fix(&[588 * 3, 588 * 2 + 5, 588 * 4, 100]));
        // Only the last track off the grid: normal, nothing to fix.
        assert!(!needs_fix(&[588, 588 * 2, 1000]));
    }
}
