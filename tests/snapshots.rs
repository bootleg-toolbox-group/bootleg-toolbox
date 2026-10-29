//! Snapshot tests (insta): the text of every `--cli` operation, and the checksums of every
//! file the operations write, compared with the reviewed snapshots in tests/snapshots/.
//! A change shows as a diff: `cargo insta review` accepts it when it is wanted.
//!
//! - `synthetic_shows`: shows made of generated signals (run everywhere, also by GitHub);
//! - `testdata_shows`: the copies of real shows in testdata/ (local only, `cargo test --
//!   --ignored`); its snapshots stay in testdata/snapshots, which is never published.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use bootleg_toolbox::audio::hex;
use md5::{Digest, Md5};

const BIN: &str = env!("CARGO_BIN_EXE_bootleg-toolbox");

/// Runs `bootleg-toolbox --cli <command> <paths>` in `dir` and returns its normalised output.
fn cli(dir: &Path, command: &str, paths: &[&str], env: &[(&str, &str)]) -> String {
    let out = Command::new(BIN)
        .current_dir(dir)
        .arg("--cli")
        .arg(command)
        .args(paths)
        .envs(env.iter().copied())
        .output()
        .expect("the executable runs");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    normalise(&text, dir)
}

/// Lines in a fixed order (files are processed in parallel), without the temporary folder
/// nor the program's version;
/// the growing "MD5 OK, ST5 OK" lines of Verify are reduced to their final state.
fn normalise(text: &str, dir: &Path) -> String {
    let dir = dir.display().to_string();
    let mut finals: Vec<(String, String)> = Vec::new();
    let mut lines = Vec::new();
    for line in text.lines() {
        let line = line.replace(&dir, "<dir>").replace(env!("CARGO_PKG_VERSION"), "<version>");
        if let Some((head, checks)) = verify_marks(&line) {
            match finals.iter_mut().find(|(h, _)| *h == head) {
                Some((_, c)) if checks.len() > c.len() => *c = checks,
                Some(_) => {}
                None => finals.push((head, checks)),
            }
        } else {
            lines.push(line);
        }
    }
    lines.extend(finals.into_iter().map(|(h, c)| format!("{h}{c}")));
    lines.sort();
    lines.join("\n")
}

/// "  OK   [file] MD5 OK, ST5 OK": the head and the checks, sorted.
fn verify_marks(line: &str) -> Option<(String, String)> {
    let (head, rest) = line.split_once("] ")?;
    let marks: Vec<&str> = rest.split(", ").collect();
    let is_mark = |m: &&str| m.ends_with(" OK") && m.split(' ').count() == 2 && m.chars().next().is_some_and(|c| c.is_ascii_uppercase());
    if !marks.iter().all(is_mark) {
        return None;
    }
    let mut marks = marks;
    marks.sort();
    Some((format!("{head}] "), marks.join(", ")))
}

/// MD5 of every file under `dir` (relative paths, sorted), with the program's version
/// replaced (FLAC vendor field, torrent "created by"). In a .torrent, the creation date is
/// left out; MP3 files are left out (LAME's float maths may differ between CPUs).
fn file_sums(dir: &Path) -> String {
    let mut files = Vec::new();
    walk(dir, &mut files);
    let mut lines: Vec<String> = files
        .iter()
        .filter(|p| !p.extension().is_some_and(|e| e == "mp3"))
        .map(|p| {
            let mut data = fs::read(p).unwrap();
            if p.extension().is_some_and(|e| e == "torrent") {
                data = strip_creation_date(&data);
            }
            data = replace_bytes(&data, env!("CARGO_PKG_VERSION").as_bytes(), b"<version>");
            let rel = p.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
            format!("{}  {rel}", hex(&Md5::digest(&data)))
        })
        .collect();
    lines.sort();
    lines.join("\n")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn replace_bytes(data: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

fn strip_creation_date(data: &[u8]) -> Vec<u8> {
    let key = b"13:creation datei";
    match data.windows(key.len()).position(|w| w == key) {
        Some(at) => {
            let end = data[at + key.len()..].iter().position(|&b| b == b'e').map_or(data.len(), |e| at + key.len() + e + 1);
            [&data[..at], &data[end..]].concat()
        }
        None => data.to_vec(),
    }
}

// ---------------------------------------------------------------- generated shows

/// A deterministic noise source (no crate, so that the signals never change).
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

/// Stereo music-like signal: chords of sines whose levels move, plus full-band hiss
/// (`hiss` false: nothing above 15.5 kHz, like a lossy file).
fn signal(frames: usize, rate: u32, seed: u64, hiss: bool) -> Vec<[f64; 2]> {
    let mut noise = Noise(seed);
    let tones: Vec<(f64, f64)> = (0..24).map(|i| (110.0 * 1.27f64.powi(i) % 15_000.0 + 80.0, 0.3 + 0.7 * noise.next().abs())).collect();
    (0..frames)
        .map(|n| {
            let t = n as f64 / rate as f64;
            let mut s = [0.0; 2];
            for (k, (f, a)) in tones.iter().enumerate() {
                let level = a * (0.5 + 0.5 * (t * 0.7 + k as f64).sin()) / 24.0;
                let v = level * (std::f64::consts::TAU * f * t).sin();
                s[0] += v;
                s[1] += v * (0.6 + 0.4 * (k as f64).cos());
            }
            if hiss {
                s[0] += 0.01 * noise.next();
                s[1] += 0.01 * noise.next();
            }
            s
        })
        .collect()
}

/// A PCM WAV file, with an extra LIST chunk when `extra` is set.
fn write_wav(path: &Path, samples: &[[f64; 2]], rate: u32, bits: u16, extra: bool) {
    let width = bits as usize / 8;
    let mut data = Vec::with_capacity(samples.len() * 2 * width);
    let scale = ((1i64 << (bits - 1)) - 1) as f64;
    for frame in samples {
        for v in frame {
            let x = (v.clamp(-1.0, 1.0) * scale).round() as i32;
            data.extend_from_slice(&x.to_le_bytes()[..width]);
        }
    }
    let list: &[u8] = if extra { b"LIST\x0c\x00\x00\x00INFOISFT\x00\x00\x00\x00" } else { b"" };
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    // "WAVE", the fmt chunk (8 + 16) and the data chunk's header: 36 bytes.
    out.extend_from_slice(&((36 + list.len() + data.len()) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2 * width as u32).to_le_bytes());
    out.extend_from_slice(&((2 * width) as u16).to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(list);
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, out).unwrap();
}

const SHOW: &str = "Synthetic Band 1990-01-01";
const HIRES: &str = "Hires Band 2000-02-02";
const MUFFLED: &str = "Muffled Band 2001-03-03";
const WAVS: &str = "Wav Band 2002-04-04";

/// The shows: a two-disc CD show with sector boundary errors and an info file, a 24/96
/// show, a show without high frequencies, and WAV files with an extra chunk.
fn make_shows(dir: &Path) {
    let cd = |name: &str, frames: usize, seed: u64| write_wav(&dir.join(SHOW).join(name), &signal(frames, 44100, seed, true), 44100, 16, false);
    cd("CD1/01 - Opening.wav", 441_000 + 123, 1);
    cd("CD1/02 - Second Song.wav", 330_750 + 7, 2);
    cd("CD1/03 - Long Jam.wav", 529_200 + 300, 3);
    cd("CD2/01 - Encore.wav", 264_600 + 41, 4);
    fs::write(
        dir.join(SHOW).join("info.txt"),
        "Synthetic Band\n1990-01-01\nThe Hall, Springfield\n\nCD1\n01. Opening\n02. Second Song\n03. Long Jam\n\nCD2\n01. Encore\n",
    )
    .unwrap();
    write_wav(&dir.join(HIRES).join("01 - Hires.wav"), &signal(480_000, 96000, 5, true), 96000, 24, false);
    write_wav(&dir.join(MUFFLED).join("01 - Muffled.wav"), &signal(441_000, 44100, 6, false), 44100, 16, false);
    write_wav(&dir.join(WAVS).join("01 - Extra chunk.wav"), &signal(220_500, 44100, 7, true), 44100, 16, true);
    write_wav(&dir.join(WAVS).join("02 - Plain.wav"), &signal(220_500, 44100, 8, true), 44100, 16, false);
}

/// The FLAC files written by to-flac hold the program's version (vendor field): it is
/// overwritten with as many "x", so that the checksums, torrents and SBE copies made from
/// them do not change with each version (only when the version's length changes).
fn unversion_flacs(dir: &Path) {
    let version = env!("CARGO_PKG_VERSION").as_bytes();
    let mut files = Vec::new();
    walk(dir, &mut files);
    for f in files.iter().filter(|p| p.extension().is_some_and(|e| e == "flac")) {
        let data = replace_bytes(&fs::read(f).unwrap(), version, &vec![b'x'; version.len()]);
        fs::write(f, data).unwrap();
    }
}

fn remove_wavs(dir: &Path) {
    let mut files = Vec::new();
    walk(dir, &mut files);
    for f in files.iter().filter(|p| p.extension().is_some_and(|e| e == "wav")) {
        fs::remove_file(f).unwrap();
    }
}

#[test]
fn synthetic_shows() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    make_shows(dir);
    let all = [SHOW, HIRES, MUFFLED, WAVS];
    let snap = |name: &str, text: String| insta::assert_snapshot!(name, text);

    snap("details_wav", cli(dir, "details", &all, &[]));
    snap("to_flac", cli(dir, "to-flac", &[SHOW, HIRES, MUFFLED], &[]));
    for show in [SHOW, HIRES, MUFFLED] {
        remove_wavs(&dir.join(show));
    }
    unversion_flacs(dir);
    snap("details", cli(dir, "details", &all, &[]));
    snap("test", cli(dir, "test", &all, &[]));
    snap("create", cli(dir, "create", &[SHOW, HIRES], &[("BT_MD5", "1"), ("BT_SFV", "1")]));
    snap("verify", cli(dir, "verify", &[SHOW, HIRES], &[]));
    snap("lossless", cli(dir, "lossless", &all, &[("BT_AAC", "1"), ("BT_REPORT", "1")]));
    snap("speed", cli(dir, "speed", &[SHOW], &[]));
    snap("media_info", cli(dir, "media-info", &[SHOW], &[]));
    snap("tag_info", cli(dir, "tag-info", &[SHOW], &[("BT_DRY", "1")]));
    snap("torrent_both", cli(dir, "torrent-both", &[SHOW], &[]));
    snap("torrent_check", cli(dir, "torrent-check", &[&format!("{SHOW} [DIME].torrent"), &format!("{SHOW} [TTD].torrent")], &[]));
    snap("fix_sbe", cli(dir, "fix-sbe", &[SHOW], &[]));
    snap("clean_wav", cli(dir, "clean-wav", &[WAVS], &[]));
    snap("mp3_v0", cli(dir, "mp3-v0", &[MUFFLED], &[]));
    snap("details_mp3", cli(dir, "details", &[&format!("{MUFFLED} [MP3 V0]")], &[]));
    snap("spectrogram", cli(dir, "spectrogram", &[&format!("{HIRES}/01 - Hires.flac")], &[]));
    snap("files", file_sums(dir));
}

// ---------------------------------------------------------------- real shows (local)

/// The copies of real shows in testdata/, as the before/after comparison used them.
#[test]
#[ignore = "needs testdata/ (local copies of real shows)"]
fn testdata_shows() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let sets = ["flac", "ape", "shn", "wavpack", "dvd", "tcheck", "edge", "sbegui"];
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    for set in sets {
        copy_dir(&root.join(set), &dir.join(set));
    }
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path(root.join("snapshots"));
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| {
        let snap = |name: &str, text: String| insta::assert_snapshot!(name, text);
        let read = ["flac", "ape", "shn", "wavpack", "edge"];
        snap("real_details", cli(dir, "details", &read, &[]));
        snap("real_test", cli(dir, "test", &read, &[]));
        snap("real_verify", cli(dir, "verify", &read, &[]));
        snap("real_lossless", cli(dir, "lossless", &["flac"], &[("BT_AAC", "1"), ("BT_REPORT", "1")]));
        snap("real_media_info", cli(dir, "media-info", &["flac", "ape", "dvd"], &[]));
        let torrents: Vec<String> = fs::read_dir(dir.join("tcheck"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "torrent"))
            .map(|p| p.strip_prefix(dir).unwrap().to_string_lossy().into_owned())
            .collect();
        snap("real_torrent_check", cli(dir, "torrent-check", &torrents.iter().map(String::as_str).collect::<Vec<_>>(), &[]));
        snap("real_create", cli(dir, "create", &["flac", "ape"], &[("BT_MD5", "1"), ("BT_SFV", "1"), ("BT_OVERWRITE", "1")]));
        snap("real_torrent_both", cli(dir, "torrent-both", &["flac"], &[]));
        snap("real_fix_sbe", cli(dir, "fix-sbe", &["sbegui"], &[]));
        snap("real_to_flac", cli(dir, "to-flac", &["ape"], &[]));
        snap("real_mp3_v0", cli(dir, "mp3-v0", &["shn"], &[]));
        snap("real_files", file_sums(dir));
    });
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            // Outputs of earlier runs are made again by the test.
            if !matches!(path.file_name().and_then(|n| n.to_str()), Some("SBE-fixed" | "WAV-cleaned")) {
                copy_dir(&path, &target);
            }
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}
