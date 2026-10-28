//! Info files (.txt), as "Tag and rename from info file" reads them.
#![no_main]

use bootleg_toolbox::tagging;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&files, data)) = data.split_first() else { return };
    let text = String::from_utf8_lossy(data);
    let _ = tagging::parse_info(&text, files as usize % 40, Some("Band"));
    for line in text.lines() {
        let _ = tagging::find_date(line);
    }
});
