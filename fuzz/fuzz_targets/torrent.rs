//! .torrent files (bencode), as "Check against torrent" reads them.
#![no_main]

use bootleg_toolbox::torrent_check;
use libfuzzer_sys::fuzz_target;

mod common;

fuzz_target!(|data: &[u8]| {
    let _ = torrent_check::read(&common::write("input.torrent", data));
});
