//! Concert videos (MKV, MP4): their audio tracks, and saving one of them on its own.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::audio::{self, Codec, Format};
use crate::dvd::Extraction;
use crate::matroska;

/// What an audio track becomes once extracted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Out {
    /// Decoded and encoded to FLAC, checked: lossless codecs this version decodes.
    Flac,
    /// AAC frames, each given the 7-byte ADTS header a stand-alone .aac file needs.
    Adts,
    /// The stream as it is, under this extension.
    Stream(&'static str),
    /// Not possible yet (Vorbis or Opus need an Ogg file; ALAC, TTA... are not decoded).
    None,
}

/// One audio track of a video file.
#[derive(Clone, Debug)]
pub struct VideoAudio {
    /// The track ID symphonia uses (Matroska TrackNumber, MP4 track ID).
    pub id: u32,
    pub codec: String,
    pub lossless: bool,
    pub channels: u32,
    pub bits: Option<u32>,
    /// Two-letter code ("en"), when the file gives one.
    pub language: String,
    out: Out,
}

impl VideoAudio {
    /// "PCM (lossless), 2 ch, 16-bit, English", as for DVD tracks.
    pub fn summary(&self) -> String {
        let mut p = vec![format!("{} ({})", self.codec, if self.lossless { "lossless" } else { "lossy" }), format!("{} ch", self.channels)];
        if let Some(bits) = self.bits {
            p.push(format!("{bits}-bit"));
        }
        if !self.language.is_empty() {
            p.push(crate::mediainfo::language_name(&self.language));
        }
        p.join(", ")
    }

    /// What the extraction writes: "FLAC", "original stream", or None when it is not possible.
    pub fn output(&self) -> Option<&'static str> {
        match self.out {
            Out::Flac => Some("FLAC"),
            Out::Adts | Out::Stream(_) => Some("original stream"),
            Out::None => None,
        }
    }

    fn extension(&self) -> &'static str {
        match self.out {
            Out::Flac => "flac",
            Out::Adts => "aac",
            Out::Stream(ext) => ext,
            Out::None => "",
        }
    }
}

/// The audio tracks of an MKV or MP4 file, in file order.
pub fn audio_tracks(path: &Path) -> Vec<VideoAudio> {
    match Format::from_path(path) {
        Some(Format::Mkv) => matroska::contents(path)
            .map(|c| {
                c.tracks
                    .iter()
                    .filter(|t| t.audio())
                    .map(|t| {
                        let id = t.codec_id.as_str();
                        let out = match id {
                            "A_PCM/INT/LIT" | "A_PCM/INT/BIG" | "A_FLAC" => Out::Flac,
                            _ if id.starts_with("A_AAC") => Out::Adts,
                            "A_AC3" | "A_AC3/BSID9" | "A_AC3/BSID10" => Out::Stream("ac3"),
                            "A_EAC3" => Out::Stream("eac3"),
                            "A_DTS" | "A_DTS/EXPRESS" | "A_DTS/LOSSLESS" => Out::Stream("dts"),
                            "A_TRUEHD" => Out::Stream("thd"),
                            "A_MPEG/L3" => Out::Stream("mp3"),
                            "A_MPEG/L2" => Out::Stream("mp2"),
                            _ => Out::None,
                        };
                        VideoAudio {
                            id: t.number as u32,
                            codec: matroska::codec_name(id),
                            lossless: matroska::lossless_codec(id),
                            channels: t.channels as u32,
                            bits: t.bits.filter(|_| matroska::lossless_codec(id)).map(|b| b as u32),
                            language: t.language.clone(),
                            out,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Some(Format::Mp4) => audio::container_tracks(path)
            .map(|tracks| {
                tracks
                    .iter()
                    .filter(|t| t.codec_params.sample_rate.is_some())
                    .filter_map(|t| {
                        let codec = Codec::from_type(t.codec_params.codec)?;
                        let out = match codec {
                            Codec::Alac | Codec::Flac | Codec::Pcm => Out::Flac,
                            Codec::Aac => Out::Adts,
                            Codec::Mp3 => Out::Stream("mp3"),
                            Codec::Mp2 => Out::Stream("mp2"),
                            _ => Out::None,
                        };
                        Some(VideoAudio {
                            id: t.id,
                            codec: codec.name().into(),
                            lossless: !codec.lossy(),
                            channels: t.codec_params.channels.map_or(2, |c| c.count() as u32),
                            bits: t.codec_params.bits_per_sample.filter(|_| !codec.lossy()),
                            language: t.language.as_deref().map(matroska::two_letter).unwrap_or_default(),
                            out,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The file track `index` (as `audio_tracks` lists them) is written to, beside the video:
/// "<video> - audio 1 PCM English.flac".
pub fn extract_path(video: &Path, index: usize, t: &VideoAudio) -> PathBuf {
    let stem = video.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let language = if t.language.is_empty() { String::new() } else { format!(" {}", crate::mediainfo::language_name(&t.language)) };
    let name = crate::tagging::safe_name(&format!("{stem} - audio {} {}{language}", index + 1, t.codec));
    video.parent().unwrap_or(Path::new(".")).join(name + "." + t.extension())
}

/// Extracts audio track `index` of a video file to its own file beside it: FLAC for a
/// lossless track (decoded, encoded, then checked by decoding the FLAC again), the stream
/// as it is for a lossy one. An existing file is kept.
pub fn extract(video: &Path, index: usize, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<Extraction, String> {
    let tracks = audio_tracks(video);
    let t = tracks.get(index).ok_or_else(|| format!("no audio track {}", index + 1))?;
    if t.output().is_none() {
        return Err(format!("{} audio cannot be saved on its own yet: keep the video file", t.codec));
    }
    let out = extract_path(video, index, t);
    if out.exists() {
        return Err(format!("{} exists already", crate::files::file_name(&out)));
    }
    let size = std::fs::metadata(video).map(|m| m.len()).unwrap_or(0);
    if t.out == Out::Flac {
        let info = audio::probe_track(video, t.id)?;
        crate::encode::check_encodable(&info)?;
        let mut tags = audio::source_tags(video);
        tags.retain(|(k, _)| k != "COMMENT");
        tags.push(("COMMENT".into(), format!("Extracted from {}, audio {} ({})", crate::files::file_name(video), index + 1, t.codec)));
        // Progress as a share of the video file, from the frames decoded.
        let frame = info.channels as u64 * info.bits.div_ceil(8) as u64;
        let (mut bytes, mut told) = (0u64, 0u64);
        let encoded = crate::encode::pcm_to_flac(&out, &info, 8, &tags, cancel, |sink| {
            audio::read_track_pcm(video, t.id, cancel, |pcm| {
                bytes += pcm.len() as u64;
                let share = (bytes / frame.max(1)).saturating_mul(size) / info.samples.max(1);
                progress(share.saturating_sub(told));
                told = told.max(share);
                sink(pcm);
            })
            .map(|_| ())
        })?;
        if !audio::length_ok(&info, encoded.samples) {
            let _ = std::fs::remove_file(&out);
            return Err(format!("only {} of {} samples decode: the video file is damaged or truncated", encoded.samples, info.samples));
        }
        let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        return Ok(Extraction { path: out, samples: Some(encoded.samples), bytes });
    }
    let adts = match t.out {
        Out::Adts => Some(adts_config(video, t.id)?),
        _ => None,
    };
    let mut part = out.clone().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    let result = (|| {
        let mut file = std::io::BufWriter::new(File::create(&part).map_err(|e| e.to_string())?);
        let mut written = 0u64;
        audio::track_packets(
            video,
            t.id,
            cancel,
            &mut |data: &[u8]| {
                // Some muxers keep the ADTS headers in the packets.
                if let Some(config) = adts.filter(|_| !data.starts_with(&[0xFF]) || data.get(1).is_none_or(|b| b & 0xF0 != 0xF0)) {
                    file.write_all(&adts_header(config, data.len())?).map_err(|e| e.to_string())?;
                    written += 7;
                }
                file.write_all(data).map_err(|e| e.to_string())?;
                written += data.len() as u64;
                Ok(())
            },
            progress,
        )?;
        file.flush().map_err(|e| e.to_string())?;
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

/// The sampling rates an ADTS header can name, by index.
const ADTS_RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

/// (profile, rate index, channel configuration) for the ADTS headers of an AAC track,
/// from its AudioSpecificConfig, or from its rate and channels when the file has none.
fn adts_config(video: &Path, id: u32) -> Result<(u8, u8, u8), String> {
    let track = audio::container_tracks(video)?.into_iter().find(|t| t.id == id).ok_or("audio track not found")?;
    let p = &track.codec_params;
    match p.extra_data.as_deref() {
        Some(asc) if asc.len() >= 2 => parse_asc(asc),
        _ => {
            let rate = p.sample_rate.ok_or("unknown sample rate")?;
            let index = ADTS_RATES.iter().position(|&r| r == rate).ok_or_else(|| format!("AAC at {rate} Hz cannot be saved as an .aac stream"))?;
            let channels = p.channels.map_or(2, |c| c.count()) as u8;
            Ok((1, index as u8, if channels == 8 { 7 } else { channels }))
        }
    }
}

/// Reads an AudioSpecificConfig (ISO 14496-3): object type, rate index, channels. With
/// SBR or PS signalled explicitly (HE-AAC), the core object type and rate are the ones
/// an ADTS header carries.
fn parse_asc(asc: &[u8]) -> Result<(u8, u8, u8), String> {
    let mut at = 0usize;
    let mut bits = |n: usize| -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *asc.get(at / 8)?;
            v = v << 1 | ((byte >> (7 - at % 8)) & 1) as u32;
            at += 1;
        }
        Some(v)
    };
    let short = || "AAC track with an unreadable configuration".to_string();
    let object = |bits: &mut dyn FnMut(usize) -> Option<u32>| -> Option<u32> {
        let o = bits(5)?;
        if o == 31 { Some(32 + bits(6)?) } else { Some(o) }
    };
    let rate = |bits: &mut dyn FnMut(usize) -> Option<u32>| -> Option<u32> {
        let i = bits(4)?;
        if i == 15 {
            let explicit = bits(24)?;
            Some(ADTS_RATES.iter().position(|&r| r == explicit).map_or(15, |p| p as u32))
        } else {
            Some(i)
        }
    };
    let mut kind = object(&mut bits).ok_or_else(short)?;
    let index = rate(&mut bits).ok_or_else(short)?;
    let channels = bits(4).ok_or_else(short)?;
    if kind == 5 || kind == 29 {
        rate(&mut bits).ok_or_else(short)?;
        kind = object(&mut bits).ok_or_else(short)?;
    }
    if !(1..=4).contains(&kind) || index > 12 || channels == 0 || channels > 7 {
        return Err("AAC of this kind cannot be saved as an .aac stream".into());
    }
    Ok(((kind - 1) as u8, index as u8, channels as u8))
}

/// The 7-byte ADTS header of one AAC frame of `len` bytes (no CRC).
fn adts_header((profile, index, channels): (u8, u8, u8), len: usize) -> Result<[u8; 7], String> {
    let total = len + 7;
    if total >= 1 << 13 {
        return Err("AAC frame too large for an .aac stream".into());
    }
    Ok([
        0xFF,
        0xF1,
        profile << 6 | index << 2 | channels >> 2,
        (channels & 3) << 6 | (total >> 11) as u8,
        (total >> 3) as u8,
        ((total & 7) as u8) << 5 | 0x1F,
        0xFC,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adts_headers() {
        // AAC LC, 48 kHz, stereo: 00010 0011 0010 000.
        assert_eq!(parse_asc(&[0x11, 0x90]), Ok((1, 3, 2)));
        // HE-AAC signalled explicitly: SBR (5), 24 kHz core, stereo, 48 kHz output, then LC.
        assert_eq!(parse_asc(&[0x2B, 0x11, 0x88, 0x00]), Ok((1, 6, 2)));
        assert!(parse_asc(&[0x11]).is_err());
        // A 371-byte frame: 378 bytes with its header.
        assert_eq!(adts_header((1, 3, 2), 371), Ok([0xFF, 0xF1, 0x4C, 0x80, 0x2F, 0x5F, 0xFC]));
        assert!(adts_header((1, 3, 2), 9000).is_err());
    }
}
