//! Checksum files (.ffp, .md5, .st5, .sfv), in any encoding.
#![no_main]

use bootleg_toolbox::files;
use libfuzzer_sys::fuzz_target;

mod common;

const EXTENSIONS: [&str; 4] = ["ffp", "md5", "st5", "sfv"];

fuzz_target!(|data: &[u8]| {
    let Some((&pick, data)) = data.split_first() else { return };
    let path = common::write(&format!("input.{}", EXTENSIONS[pick as usize % EXTENSIONS.len()]), data);
    let _ = files::parse_checksum_file(&path);
});
