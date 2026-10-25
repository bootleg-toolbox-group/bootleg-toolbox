// No console window behind the GUI on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod audio;
mod dsp;
mod encode;
mod files;
mod lossless;
mod ops;
mod report;
mod sbe;
mod shn;
mod speed;
mod wv;
mod spectrogram;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use ops::{Level, Msg, Op, Options, Tracker};

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
///   bootleg-helper --cli details|verify|create|test|torrent-dime|torrent-ttd|torrent-both|torrent-all|spectrogram|lossless|speed|to-flac|to-wav|mp3-320|mp3-v0|fix-sbe <paths...>
fn run_cli(args: &[String]) -> i32 {
    let Some(command) = args.first() else {
        eprintln!("usage: bootleg-helper --cli details|verify|create|test|torrent-dime|torrent-ttd|torrent-both|torrent-all|spectrogram|lossless|speed|to-flac|to-wav|mp3-320|mp3-v0|fix-sbe <paths...>");
        return 2;
    };
    // Extra trackers for torrent-all: BH_TRACKERS="Name=announce;Other=announce".
    let custom_trackers: Vec<ops::CustomTracker> = std::env::var("BH_TRACKERS")
        .unwrap_or_default()
        .split(';')
        .filter_map(|t| t.split_once('='))
        .map(|(name, announce)| ops::CustomTracker { name: name.into(), announce: announce.into() })
        .collect();
    // FLAC level for to-flac: BH_LEVEL (default 8).
    // Float WAVs that do not hold exact 16/24-bit values: BH_FLOAT=round rounds them to 24 bits.
    let round_float = std::env::var("BH_FLOAT").is_ok_and(|v| v == "round");
    let level = std::env::var("BH_LEVEL").ok().and_then(|l| l.parse().ok()).unwrap_or(8);
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
        other => {
            eprintln!("unknown command: {other}");
            return 2;
        }
    };
    let paths: Vec<PathBuf> = args[1..].iter().map(PathBuf::from).collect();
    let shows = files::scan_paths(&paths, &Default::default());
    let all: Vec<PathBuf> = shows.iter().flat_map(|s| s.files.clone()).collect();
    // Folder for mp3-320/mp3-v0: BH_MP3_DIR (default: next to the show).
    let mp3_dir = std::env::var("BH_MP3_DIR").unwrap_or_default();
    let mut opt = Options { custom_trackers: custom_trackers.clone(), mp3_dir, ..Options::default() };
    if std::env::var("BH_OVERWRITE").is_ok() {
        opt.overwrite = true;
    }
    if std::env::var("BH_MD5").is_ok() {
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
                if std::env::var_os("BH_REPORT").is_some() {
                    println!("{}", report.to_text());
                    continue;
                }
                for line in tooltip.lines().skip(1).take_while(|l| !l.is_empty()) {
                    println!("      {}: {line}", path.file_name().unwrap_or_default().to_string_lossy());
                }
            }
            Msg::Speed { path, text, level, report, .. } => {
                println!("  {}[{}] Speed: {text}", prefix(level), path.display());
                if std::env::var_os("BH_REPORT").is_some() {
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
            Msg::Finished { summary, .. } => {
                println!("{summary}");
                break;
            }
            _ => {}
        }
    }
    if errors > 0 { 1 } else { 0 }
}

fn prefix(level: Level) -> &'static str {
    match level {
        Level::Info => "",
        Level::Ok => "OK   ",
        Level::Warn => "WARN ",
        Level::Error => "ERR  ",
    }
}
