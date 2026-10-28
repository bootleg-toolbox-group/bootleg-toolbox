//! DVD folders: the title set's IFO, and a VOB with the same bytes.
#![no_main]

use bootleg_toolbox::dvd;
use libfuzzer_sys::fuzz_target;

mod common;

fuzz_target!(|data: &[u8]| {
    let ifo = common::write("VIDEO_TS/VTS_01_0.IFO", data);
    common::write("VIDEO_TS/VTS_01_1.VOB", data);
    let _ = dvd::read(ifo.parent().unwrap());
    let _ = dvd::mpa_header(data);
});
