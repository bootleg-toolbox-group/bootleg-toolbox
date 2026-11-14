//! The lowpass of common lossy encoders, to name the settings a measured cutoff matches.
//!
//! Nominal values from the encoders' sources: LAME 3.100 `optimum_bandwidth` (CBR/ABR) and
//! the VBR table of `lame_init_params`; libvorbis `psych_44.h` (`_psy_lowpass_44`, none from
//! q6 up); FDK-AAC `bandwidth.cpp` (stereo, per channel bitrate; VBR modes); Opus never codes
//! above 20 kHz (CELT band 21). `measured` is where Lossless? puts the cutoff of a file
//! encoded with that setting and decoded (its midpoint of the drop, not the encoder's
//! nominal value), measured on test files at 44.1 kHz.

/// The codec family, as the coding traces tell it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Mp3,
    Aac,
    /// MPEG-1 Layer II: no lowpass setting is tabulated for it (E10).
    Mp2,
    Vorbis,
    Opus,
}

pub struct Setting {
    pub codec: Codec,
    /// "MP3 128 kbps", "Vorbis q3".
    pub name: &'static str,
    /// The encoder's own lowpass, Hz.
    pub nominal: f64,
    /// Where Lossless? measures it, Hz.
    pub measured: f64,
}

const fn s(codec: Codec, name: &'static str, nominal: f64, measured: f64) -> Setting {
    Setting { codec, name, nominal, measured }
}

/// Measured on test files (rolloffbank, 6 to 8 tracks each); the others are the nominal value
/// moved by the offset measured on their neighbours.
pub const SETTINGS: &[Setting] = &[
    s(Codec::Mp3, "MP3 96 kbps", 15100.0, 15000.0),
    s(Codec::Mp3, "MP3 112 kbps", 15600.0, 15400.0),
    s(Codec::Mp3, "MP3 128 kbps", 17000.0, 16700.0),
    s(Codec::Mp3, "MP3 160 kbps", 17500.0, 17400.0),
    s(Codec::Mp3, "MP3 192 kbps", 18600.0, 18750.0),
    s(Codec::Mp3, "MP3 224 kbps", 19400.0, 19500.0),
    s(Codec::Mp3, "MP3 256 kbps", 19700.0, 19800.0),
    s(Codec::Mp3, "MP3 320 kbps", 20500.0, 20500.0),
    s(Codec::Mp3, "MP3 VBR V1", 19500.0, 19600.0),
    s(Codec::Mp3, "MP3 VBR V2", 18500.0, 18750.0),
    s(Codec::Mp3, "MP3 VBR V3", 18000.0, 18100.0),
    s(Codec::Mp3, "MP3 VBR V4", 17500.0, 17400.0),
    s(Codec::Mp3, "MP3 VBR V5", 17000.0, 16600.0),
    s(Codec::Mp3, "MP3 VBR V6", 16500.0, 16200.0),
    s(Codec::Mp3, "MP3 VBR V7", 15600.0, 15400.0),
    s(Codec::Aac, "AAC 96-112 kbps (FDK)", 14260.0, 14300.0),
    s(Codec::Aac, "AAC 128 kbps (FDK)", 15500.0, 15500.0),
    s(Codec::Aac, "AAC 160 kbps (FDK)", 16120.0, 16100.0),
    s(Codec::Aac, "AAC 192 kbps and up (FDK)", 17000.0, 17000.0),
    s(Codec::Aac, "AAC VBR 3 (FDK)", 15750.0, 15800.0),
    s(Codec::Aac, "AAC VBR 4 (FDK)", 16500.0, 16600.0),
    s(Codec::Aac, "AAC VBR 5 (FDK)", 19293.0, 19300.0),
    s(Codec::Aac, "AAC 128 kbps (FFmpeg)", 17300.0, 17300.0),
    s(Codec::Vorbis, "Vorbis q0", 15100.0, 15300.0),
    s(Codec::Vorbis, "Vorbis q1", 15800.0, 16400.0),
    s(Codec::Vorbis, "Vorbis q2", 16500.0, 17000.0),
    s(Codec::Vorbis, "Vorbis q3", 17200.0, 17800.0),
    s(Codec::Vorbis, "Vorbis q4", 18900.0, 19300.0),
    s(Codec::Vorbis, "Vorbis q5", 20100.0, 20600.0),
    s(Codec::Opus, "Opus (any bitrate)", 20000.0, 20200.0),
];

/// How far from `measured` a cutoff still matches a setting. Vorbis's lowpass is softer and
/// lands anywhere within about 0.5 kHz (q1: 16.0-16.9 kHz, q3: 17.4-18.2 kHz).
fn tolerance(codec: Codec) -> f64 {
    match codec {
        Codec::Vorbis => 500.0,
        _ => 250.0,
    }
}

/// The settings whose lowpass matches `cutoff` (Hz), nearest first; only `codec`'s when known.
pub fn matching(cutoff: f64, codec: Option<Codec>) -> Vec<&'static Setting> {
    let mut found: Vec<&Setting> =
        SETTINGS.iter().filter(|s| codec.is_none_or(|c| s.codec == c) && (cutoff - s.measured).abs() <= tolerance(s.codec)).collect();
    found.sort_by(|a, b| (cutoff - a.measured).abs().total_cmp(&(cutoff - b.measured).abs()));
    found
}

/// "MP3 128 kbps, AAC VBR 4 (FDK) or Vorbis q1": the nearest setting of each codec, at most
/// three; None when nothing matches.
pub fn describe(cutoff: f64, codec: Option<Codec>) -> Option<String> {
    let mut names: Vec<&str> = Vec::new();
    let mut codecs: Vec<Codec> = Vec::new();
    for s in matching(cutoff, codec) {
        // One setting per codec, unless the codec is known: then up to two.
        if codec.is_none() && codecs.contains(&s.codec) || names.len() == 3 || codec.is_some() && names.len() == 2 {
            continue;
        }
        codecs.push(s.codec);
        names.push(s.name);
    }
    match names.len() {
        0 => None,
        1 => Some(names[0].to_string()),
        n => Some(format!("{} or {}", names[..n - 1].join(", "), names[n - 1])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_matched_by_cutoff() {
        assert_eq!(describe(16700.0, Some(Codec::Mp3)).as_deref(), Some("MP3 128 kbps or MP3 VBR V5"));
        assert_eq!(describe(20200.0, None).as_deref(), Some("Opus (any bitrate) or Vorbis q5"));
        assert_eq!(describe(18750.0, None).as_deref(), Some("MP3 192 kbps"));
        assert!(describe(12000.0, None).is_none());
        // Each codec's nearest setting, at most three.
        let all = describe(16600.0, None).unwrap();
        assert!(all.contains("MP3") && all.contains("AAC") && all.contains("Vorbis"), "{all}");
    }
}
