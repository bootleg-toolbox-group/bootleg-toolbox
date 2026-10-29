//! Keyboard shortcuts and command line, shown in Help > Keyboard shortcuts and command
//! line, and printed by `--help` (one source for both).

pub const SHORTCUTS: &[(&str, &str)] = &[
    ("F1", "This help"),
    ("F5", "Verify checksums"),
    ("F6", "Create checksums"),
    ("F7", "Decode test"),
    ("F8", "Show details (read the files again and list them in the log)"),
    ("F9", "Spectrogram (first 4 files of the selection)"),
    ("F10", "Torrents for every tracker"),
    ("Ctrl+L", "Lossless? (lossless origin check)"),
    ("Ctrl+P", "Speed? (tape speed check)"),
    ("Ctrl+I", "Media info"),
    ("Ctrl+A", "Select everything"),
    ("Page Up / Page Down", "Scroll the list by a screen"),
    ("Home / End", "Go to the top or the bottom of the list"),
    ("Delete", "Remove the selection from the list (the files are not touched)"),
    ("Escape", "Stop the running task; otherwise clear the selection, or close a menu or window"),
];

pub const MOUSE: &[(&str, &str)] = &[
    ("Drop", "Folders or files dropped on the window replace the list"),
    ("Ctrl + drop", "Add them to the list instead"),
    ("Drop a .torrent", "Check the show against it"),
    ("Drop a DVD folder", "Open its media info"),
    ("Click", "Select a row"),
    ("Ctrl + click", "Add a row to the selection, or take it out"),
    ("Shift + click", "Select every row up to this one"),
    ("Drag over rows", "Select a range (also from the empty area below the rows)"),
    ("Right-click", "Every action, with Open folder first"),
    ("Double-click a column edge", "Fit the column to its content"),
    ("Up / Down, Enter", "In the info file choice: pick a file, copy it"),
];

pub const COMMANDS: &[(&str, &str)] = &[
    ("details", "File details"),
    ("verify", "Verify the checksum files (.ffp, .st5, .md5, .sfv)"),
    ("create", "Create the checksum files"),
    ("test", "Decode test"),
    ("torrent-dime", "Torrent for Dime a Dozen"),
    ("torrent-ttd", "Torrent for The Traders' Den"),
    ("torrent-both", "Torrents for both"),
    ("torrent-all", "Torrents for both and for the trackers in BT_TRACKERS"),
    ("torrent-check", "Check the .torrent files given against their shows"),
    ("lossless", "Lossless origin check"),
    ("speed", "Tape speed check"),
    ("spectrogram", "Spectrograms, saved as PNG in the current folder"),
    ("to-flac", "Convert to FLAC"),
    ("to-wav", "Convert to WAV"),
    ("mp3-320", "MP3 copies, 320 kbps CBR"),
    ("mp3-v0", "MP3 copies, VBR V0"),
    ("fix-sbe", "Fix sector boundary errors (copies in an SBE-fixed folder)"),
    ("clean-wav", "Clean WAV headers (copies in a WAV-cleaned folder)"),
    ("tag-info", "Tag and rename the FLAC files from the info file (prints what it read first)"),
    ("undo-tagging", "Roll back the latest tag-info or Tag and rename (every show of it)"),
    ("media-info", "Media info of the shows, and of the DVDs (VIDEO_TS folders, VOB files)"),
    ("check-update", "Look for a newer release on GitHub"),
    ("shell-menu-add", "Add \"Open in Bootleg Toolbox\" to the file manager's right-click menu"),
    ("shell-menu-remove", "Remove it"),
    ("help", "This help"),
];

pub const VARIABLES: &[(&str, &str)] = &[
    ("BT_OVERWRITE=1", "create: replace existing checksum files"),
    ("BT_MD5=1", "create: also write a .md5 file"),
    ("BT_SFV=1", "create: also write a .sfv file"),
    ("BT_LEVEL=0..8", "to-flac: compression level (8 by default)"),
    ("BT_FLOAT=round", "to-flac: round float WAVs to 24 bits when they are not exact"),
    ("BT_MP3_DIR=folder", "mp3-320, mp3-v0: write the copies there (default: next to the show)"),
    ("BT_TRACKERS=\"Name=URL;...\"", "torrent-all: extra trackers, with their announce URLs"),
    ("BT_RENAME=number|disc|etree|keep", "tag-info: \"01 - Title\" (default), \"d1t01 - Title\", \"Artist YYYY-MM-DD d1t01\", or no renaming"),
    ("BT_DRY=1", "tag-info: only print what would be written"),
    ("BT_AAC=1", "lossless: also look for AAC coding traces (slower)"),
    ("BT_REPORT=1", "lossless, speed: print the full report"),
    ("BT_WAYLAND=1", "Linux, window mode: native Wayland window (no drag and drop)"),
];

pub const CLI_USAGE: &str = "bootleg-toolbox --cli <command> <folders or files...>";

pub const CLI_NOTES: &[&str] = &[
    "Every operation of the window also runs without it, for scripts and batch work. Give show folders or files; folders are searched like a drop on the window.",
    "Settings are not read in this mode: the defaults apply, changed with the variables below.",
    "Exit code: 0 when everything went well, 1 when something failed, 2 for a wrong command.",
    "Windows: run it from a Command Prompt or PowerShell. The prompt may come back before the results are printed: wait for the final summary line.",
];

pub const CLI_EXAMPLES: &[&str] = &[
    "bootleg-toolbox --cli verify \"gd1977-05-08\"",
    "bootleg-toolbox --cli media-info \"Concert DVD\"",
    "Linux:       BT_LEVEL=5 bootleg-toolbox --cli to-flac \"gd1977-05-08\"",
    "Windows cmd: set \"BT_LEVEL=5\" && bootleg-toolbox.exe --cli to-flac \"D:\\Shows\\gd1977-05-08\"",
    "PowerShell:  $env:BT_LEVEL=5; .\\bootleg-toolbox.exe --cli to-flac \"D:\\Shows\\gd1977-05-08\"",
];

fn table(out: &mut String, rows: &[(&str, &str)]) {
    let width = rows.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
    for (key, text) in rows {
        out.push_str(&format!("  {key:<width$}  {text}\n"));
    }
}

/// The whole help as text (Copy button, `--help`).
pub fn text() -> String {
    let mut out = format!("{} {}\n\nKEYBOARD SHORTCUTS\n", crate::ops::APP_NAME, crate::ops::APP_VERSION);
    table(&mut out, SHORTCUTS);
    out.push_str("\nMOUSE AND DRAG AND DROP\n");
    table(&mut out, MOUSE);
    out.push_str(&format!("\nCOMMAND LINE\n  {CLI_USAGE}\n\n"));
    for note in CLI_NOTES {
        out.push_str(&format!("  {note}\n"));
    }
    out.push_str("\nCommands\n");
    table(&mut out, COMMANDS);
    out.push_str("\nEnvironment variables\n");
    table(&mut out, VARIABLES);
    out.push_str("\nExamples\n");
    for example in CLI_EXAMPLES {
        out.push_str(&format!("  {example}\n"));
    }
    out
}
