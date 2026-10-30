//! Audio files of every format: header, decoding, media info, WAV layout.
//!
//! The program turns a decoder's panic into an error for that file (ops::catch_panic), so
//! panics inside the third-party decoders are expected here (symphonia: AAC/ADTS reader;
//! wavicle: block size overflow, a debug assertion only): they are let through to
//! catch_panic. Any other panic is a crash.
#![no_main]

use std::sync::atomic::AtomicBool;
use std::sync::Once;

use bootleg_toolbox::ops::catch_panic;
use bootleg_toolbox::{audio, mediainfo};
use libfuzzer_sys::fuzz_target;

mod common;

const EXTENSIONS: [&str; 10] = ["flac", "wav", "ape", "shn", "wv", "mp3", "mp2", "m4a", "mka", "mp4"];

fn hook() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            let trace = std::backtrace::Backtrace::force_capture().to_string();
            if ["symphonia", "wavicle"].iter().any(|c| trace.contains(c)) {
                return;
            }
            eprintln!("{info}");
            std::process::abort();
        }));
    });
}

fuzz_target!(|data: &[u8]| {
    hook();
    let Some((&pick, data)) = data.split_first() else { return };
    let ext = EXTENSIONS[pick as usize % EXTENSIONS.len()];
    let path = common::write(&format!("input.{ext}"), data);
    if catch_panic(|| audio::probe(&path)).is_ok() {
        let _ = catch_panic(|| audio::read_pcm(&path, &AtomicBool::new(false), |_| {}));
    }
    let _ = catch_panic(|| mediainfo::read_file(&path));
    if ext == "wav" {
        let _ = audio::wav_layout(&path);
    }
});
