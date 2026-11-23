//! Video files read for their sound: QuickTime (.mov) and .m4v.
//!
//! The two files in tests/data were made with ffmpeg (a reference for the tests only): a 24-bit
//! stereo PCM .mov and an AAC .m4v.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use bootleg_toolbox::audio::{self, Codec, Format};
use bootleg_toolbox::{mediainfo, video};
use md5::{Digest, Md5};

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)
}

#[test]
fn mov_and_m4v_are_read_like_mp4() {
    assert_eq!(Format::from_path(Path::new("a.MOV")), Some(Format::Mov));
    assert_eq!(Format::from_path(Path::new("a.m4v")), Some(Format::Mp4));
    assert!(Format::Mov.video() && Format::Mov.container());

    // 0.1 s of 24-bit stereo PCM at 48 kHz.
    let mov = data("video-pcm24.mov");
    let info = audio::probe(&mov).unwrap();
    assert_eq!((info.format, info.codec, info.bits, info.rate, info.channels, info.samples), (Format::Mov, Some(Codec::Pcm), 24, 48000, 2, 4800));
    assert!(!info.lossy());
    // The samples are those ffmpeg decodes from it (`-f s24le`, md5 taken then).
    let mut md5 = Md5::new();
    let frames = audio::read_pcm(&mov, &AtomicBool::new(false), |b| md5.update(b)).unwrap();
    assert_eq!(frames, 4800);
    assert_eq!(audio::hex(&md5.finalize()), "d6fa1f3b9562dcb7cb9ee012876d223f");
    // Extract audio: a lossless track becomes FLAC.
    let tracks = video::audio_tracks(&mov);
    assert_eq!((tracks.len(), tracks[0].output(), tracks[0].lossless), (1, Some("FLAC"), true));
    let text = mediainfo::audio_text(&mov, &[mediainfo::read_file(&mov).unwrap()], &[]);
    assert!(text.contains("PCM (uncompressed), in QuickTime video (MOV)") && text.contains("Bit depth         : 24 bits"), "{text}");

    let m4v = audio::probe(&data("video-aac.m4v")).unwrap();
    assert_eq!((m4v.format, m4v.codec, m4v.lossy()), (Format::Mp4, Some(Codec::Aac), true));
}
