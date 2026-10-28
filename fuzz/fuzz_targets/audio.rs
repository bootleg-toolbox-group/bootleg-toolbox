//! Audio files of every format: header, decoding, media info, WAV layout.
#![no_main]

use std::sync::atomic::AtomicBool;

use bootleg_toolbox::{audio, mediainfo};
use libfuzzer_sys::fuzz_target;

mod common;

const EXTENSIONS: [&str; 9] = ["flac", "wav", "ape", "shn", "wv", "mp3", "mp2", "m4a", "mka"];

fuzz_target!(|data: &[u8]| {
    let Some((&pick, data)) = data.split_first() else { return };
    let ext = EXTENSIONS[pick as usize % EXTENSIONS.len()];
    let path = common::write(&format!("input.{ext}"), data);
    if audio::probe(&path).is_ok() {
        let _ = audio::read_pcm(&path, &AtomicBool::new(false), |_| {});
    }
    let _ = mediainfo::read_file(&path);
    if ext == "wav" {
        let _ = audio::wav_layout(&path);
    }
});
