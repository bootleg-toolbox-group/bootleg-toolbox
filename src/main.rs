// No console window behind the GUI on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod audio;
mod dsp;
mod encode;
mod dvd;
mod files;
mod fonts;
mod help;
mod integration;
mod lossless;
mod aactrace;
mod mp3trace;
mod mediainfo;
mod ops;
mod report;
mod sbe;
mod torrent_check;
mod update;
mod shn;
mod speed;
mod wv;
mod spectrogram;
mod tagging;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use ops::{Level, Msg, Op, Options, Tracker};

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("--help" | "-h" | "/?")) {
        attach_console();
        print!("{}", help::text());
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--cli") {
        attach_console();
        std::process::exit(run_cli(&args[1..]));
    }
    let paths: Vec<PathBuf> = args.iter().filter(|a| !a.starts_with("--")).map(PathBuf::from).collect();
    app::run(paths)
}

/// The Windows build has no console of its own (GUI subsystem): in text mode, write to the
/// console of the command prompt that started it, if any.
#[cfg(windows)]
fn attach_console() {
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_console() {}

/// Text mode, handy for scripting and testing:
///   bootleg-toolbox --cli <command> <paths...> (the commands: `help::COMMANDS`)
fn run_cli(args: &[String]) -> i32 {
    let Some(command) = args.first() else {
        eprintln!("usage: {}\n(bootleg-toolbox --cli help lists the commands)", help::CLI_USAGE);
        return 2;
    };
    if command == "help" {
        print!("{}", help::text());
        return 0;
    }
    // Extra trackers for torrent-all: BT_TRACKERS="Name=announce;Other=announce".
    let custom_trackers: Vec<ops::CustomTracker> = std::env::var("BT_TRACKERS")
        .unwrap_or_default()
        .split(';')
        .filter_map(|t| t.split_once('='))
        .map(|(name, announce)| ops::CustomTracker { name: name.into(), announce: announce.into() })
        .collect();
    // FLAC level for to-flac: BT_LEVEL (default 8).
    // Float WAVs that do not hold exact 16/24-bit values: BT_FLOAT=round rounds them to 24 bits.
    let round_float = std::env::var("BT_FLOAT").is_ok_and(|v| v == "round");
    let level = std::env::var("BT_LEVEL").ok().and_then(|l| l.parse().ok()).unwrap_or(8);
    if command == "check-update" {
        return match update::newer_version() {
            Ok(Some(version)) => {
                println!("Version {version} is available (this is {}): {}/latest", ops::APP_VERSION, update::RELEASES);
                0
            }
            Ok(None) => {
                println!("{} is the latest version.", ops::APP_VERSION);
                0
            }
            Err(e) => {
                eprintln!("Could not check for updates: {e}.");
                1
            }
        };
    }
    // The file manager's right-click entry (the Settings option, for scripts and tests).
    if let "shell-menu-add" | "shell-menu-remove" = command.as_str() {
        let done = if command == "shell-menu-add" {
            integration::install()
        } else {
            // The entries of the former name (Bootleg Helper) too.
            integration::remove_legacy();
            integration::remove()
        };
        return match done {
            Ok(text) => {
                println!("{text}");
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    let op = match command.as_str() {
        "details" => Op::Details { verbose: true },
        "verify" => Op::Verify,
        "create" => Op::Create,
        "test" => Op::Test,
        "torrent-dime" => Op::Torrent(vec![Tracker::Dime]),
        "torrent-ttd" => Op::Torrent(vec![Tracker::Ttd]),
        "torrent-both" => Op::Torrent(vec![Tracker::Dime, Tracker::Ttd]),
        "torrent-all" => Op::Torrent((0..2 + custom_trackers.len()).map(|i| match i {
            0 => Tracker::Dime,
            1 => Tracker::Ttd,
            i => Tracker::Custom(i - 2),
        }).collect()),
        "spectrogram" => Op::Spectrogram,
        "lossless" => Op::Lossless,
        "speed" => Op::Speed,
        "to-flac" => Op::Convert { target: ops::Target::Flac, level, round_float },
        "to-wav" => Op::Convert { target: ops::Target::Wav, level, round_float },
        "mp3-320" => Op::Mp3(encode::Mp3Mode::Cbr320),
        "mp3-v0" => Op::Mp3(encode::Mp3Mode::V0),
        "fix-sbe" => Op::FixSbe,
        "clean-wav" => Op::CleanWav,
        "tag-info" => match tag_jobs(&args[1..]) {
            Some(jobs) => Op::Tag(jobs),
            None => return 0,
        },
        // .torrent files among the paths, checked against the folders (or the folders next to them).
        "torrent-check" => Op::TorrentCheck(args[1..].iter().map(PathBuf::from).filter(|p| crate::files::ext_lower(p) == "torrent").collect()),
        // DVDs (VIDEO_TS folders, VOB files) found in the paths as well as the audio files.
        "media-info" => Op::MediaInfo(args[1..].iter().flat_map(|p| dvd::find(Path::new(p))).collect()),
        other => {
            eprintln!("unknown command: {other} (bootleg-toolbox --cli help lists the commands)");
            return 2;
        }
    };
    let paths: Vec<PathBuf> = args[1..].iter().map(PathBuf::from).collect();
    let shows = files::scan_paths(&paths, &Default::default());
    let all: Vec<PathBuf> = shows.iter().flat_map(|s| s.files.clone()).collect();
    // Folder for mp3-320/mp3-v0: BT_MP3_DIR (default: next to the show).
    let mp3_dir = std::env::var("BT_MP3_DIR").unwrap_or_default();
    let mut opt = Options { custom_trackers: custom_trackers.clone(), mp3_dir, ..Options::default() };
    if std::env::var("BT_OVERWRITE").is_ok() {
        opt.overwrite = true;
    }
    if std::env::var("BT_SFV").is_ok() {
        opt.create_sfv = true;
    }
    if std::env::var("BT_MD5").is_ok() {
        opt.create_md5 = true;
    }
    let (tx, rx) = mpsc::channel();
    ops::spawn(op, shows, all, opt, tx, Arc::new(AtomicBool::new(false)));
    let mut errors = 0;
    for msg in rx {
        match msg {
            Msg::Log(text, level) => {
                if level == Level::Error {
                    errors += 1;
                }
                println!("{}{text}", prefix(level));
            }
            Msg::Status { path, text, level, .. } => println!("  {}[{}] {text}", prefix(level), path.display()),
            Msg::Source { path, text, level, tooltip, report } => {
                println!("  {}[{}] Source: {text}", prefix(level), path.display());
                if std::env::var_os("BT_REPORT").is_some() {
                    println!("{}", report.to_text());
                    continue;
                }
                for line in tooltip.lines().skip(1).take_while(|l| !l.is_empty()) {
                    println!("      {}: {line}", path.file_name().unwrap_or_default().to_string_lossy());
                }
            }
            Msg::Speed { path, text, level, report, .. } => {
                println!("  {}[{}] Speed: {text}", prefix(level), path.display());
                if std::env::var_os("BT_REPORT").is_some() {
                    println!("{}", report.to_text());
                }
            }
            Msg::Image { path, image } => {
                // Saved in the current directory, never next to the audio files.
                let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let out = PathBuf::from(format!("{stem} - spectrogram.png"));
                match spectrogram::save_png(&image, &out) {
                    Ok(()) => println!("{}Saved {}", prefix(Level::Ok), out.display()),
                    Err(e) => {
                        errors += 1;
                        println!("{}Cannot save {}: {e}", prefix(Level::Error), out.display());
                    }
                }
            }
            Msg::MediaInfo { text, .. } => println!("{text}"),
            Msg::Finished { summary, .. } => {
                println!("{summary}");
                break;
            }
            _ => {}
        }
    }
    if errors > 0 { 1 } else { 0 }
}

/// tag-info: the plan of each show, read from its most likely info file and printed;
/// BT_RENAME=number|disc|etree|keep (default number); BT_DRY=1 prints without writing.
fn tag_jobs(paths: &[String]) -> Option<Vec<tagging::TagJob>> {
    use tagging::NameScheme;
    let scheme = match std::env::var("BT_RENAME").unwrap_or_default().as_str() {
        "disc" => NameScheme::DiscTrackTitle,
        "etree" => NameScheme::Etree,
        "keep" => NameScheme::Keep,
        _ => NameScheme::NumberTitle,
    };
    let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let mut jobs = Vec::new();
    for show in files::scan_paths(&paths, &Default::default()) {
        let name = files::file_name(&show.root);
        let Some((info, _)) = tagging::info_files(&show).into_iter().next() else {
            println!("{}{name}: no info file", prefix(Level::Warn));
            continue;
        };
        let draft = match tagging::draft(&show, &info) {
            Ok(d) => d,
            Err(e) => {
                println!("{}{name}: {e}", prefix(Level::Error));
                continue;
            }
        };
        println!("{name}");
        println!("  info file: {}", files::file_name(&info));
        println!("  artist: {}\n  date: {}\n  place: {}\n  album: {}", draft.artist, draft.date, draft.place, draft.album);
        println!("  titles found: {} for {} files", draft.found, draft.tracks.len());
        if let Some(d) = draft.folder_date.as_ref().filter(|d| **d != draft.date) {
            println!("  {}the folder name says {d}", prefix(Level::Warn));
        }
        for t in &draft.tracks {
            let disc = t.disc.map(|d| format!("d{d}")).unwrap_or_default();
            let new = draft.new_name(t, scheme).map(|n| format!(" -> {n}")).unwrap_or_default();
            println!("  {disc}t{:02} {:?}  [{}]{new}", t.track, t.title, files::file_name(&t.path));
        }
        for e in &draft.extra {
            println!("  {}extra title: {e}", prefix(Level::Warn));
        }
        let conflicts = draft.name_conflicts(scheme);
        for c in &conflicts {
            println!("  {}{c}", prefix(Level::Error));
        }
        if conflicts.is_empty() {
            jobs.push(draft.job(scheme, true));
        }
    }
    (std::env::var_os("BT_DRY").is_none() && !jobs.is_empty()).then_some(jobs)
}

fn prefix(level: Level) -> &'static str {
    match level {
        Level::Info => "",
        Level::Ok => "OK   ",
        Level::Warn => "WARN ",
        Level::Error => "ERR  ",
    }
}
