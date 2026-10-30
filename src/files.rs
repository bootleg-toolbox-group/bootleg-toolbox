//! Finding shows on disk, natural sorting, and reading checksum files.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use crate::audio::Format;

pub const CHECKSUM_EXT: [&str; 4] = ["ffp", "md5", "st5", "sfv"];

#[derive(Clone, Debug)]
pub struct Show {
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
}

/// Folders that only hold the audio of a show ("AUDIO", "music", "24-bit"): its name and
/// its info file are in the folder above.
pub fn holder_folder(name: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)^(audio|music|flac|wav|files|tracks|songs|lossless|(16|24)[\s_-]?bits?( version)?)$").unwrap()).is_match(name.trim())
}

/// The folder above a show folder that names the show: above an "AUDIO" folder, or a
/// folder with a date ("Artist - 1995-11-21 - Place [taper]/1995-11-21 - Madrid (Version 2)").
pub fn outer_folder(root: &Path) -> Option<&Path> {
    static DATE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let date = DATE.get_or_init(|| regex::Regex::new(r"(19|20)\d\d[-.]\d\d[-.]\d\d").unwrap());
    let parent = root.parent()?;
    let name = file_name(parent);
    (!name.is_empty() && (holder_folder(&file_name(root)) || date.is_match(&name))).then_some(parent)
}

/// The name of a show for the list and the messages: its folder, after the folder above
/// when that one names the show (see `outer_folder`).
pub fn show_name(root: &Path) -> String {
    match outer_folder(root) {
        Some(parent) => format!("{}/{}", file_name(parent), file_name(root)),
        None => file_name(root),
    }
}

pub fn ext_lower(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn is_checksum_file(path: &Path) -> bool {
    CHECKSUM_EXT.contains(&ext_lower(path).as_str())
}

pub fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Path relative to `root`, with forward slashes.
pub fn rel_posix(path: &Path, root: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

// ---------------------------------------------------------------- natural sort

#[derive(PartialEq, Eq)]
enum Piece {
    Num(u128),
    Text(String),
}

fn pieces(text: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut digits = String::new();
    let mut letters = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            if !letters.is_empty() {
                out.push(Piece::Text(std::mem::take(&mut letters).to_lowercase()));
            }
            digits.push(ch);
        } else {
            if !digits.is_empty() {
                out.push(Piece::Num(std::mem::take(&mut digits).parse().unwrap_or(u128::MAX)));
            }
            letters.push(ch);
        }
    }
    if !digits.is_empty() {
        out.push(Piece::Num(digits.parse().unwrap_or(u128::MAX)));
    }
    if !letters.is_empty() {
        out.push(Piece::Text(letters.to_lowercase()));
    }
    out
}

/// "track 2" before "track 10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (pa, pb) = (pieces(a), pieces(b));
    for (x, y) in pa.iter().zip(pb.iter()) {
        let ord = match (x, y) {
            (Piece::Num(m), Piece::Num(n)) => m.cmp(n),
            (Piece::Text(s), Piece::Text(t)) => s.cmp(t),
            (Piece::Num(_), Piece::Text(_)) => Ordering::Less,
            (Piece::Text(_), Piece::Num(_)) => Ordering::Greater,
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    pa.len().cmp(&pb.len())
}

pub fn natural_path_cmp(a: &Path, b: &Path) -> Ordering {
    natural_cmp(&a.to_string_lossy(), &b.to_string_lossy())
}

// ---------------------------------------------------------------- scanning

/// A disc, set or tape side of a show: "CD1", "Disc 2 - Encore", "Disc Two", "D1", "Set B", "Side A",
/// "Part 2", "Tape 1", "Vol 1", or a bare letter or number ("A", "B", "1", "2").
fn disc_dir_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^(?:(?:cd|disc|disk|dvd|set|d|part|pt|side|tape|vol|volume)[\s._-]*\d+|(?:cd|disc|disk|set|part|side|tape|vol|volume)[\s._-]+(?:one|two|three|four|five|six|seven|eight|nine|ten)(?:$|[\s._-])|(?:cd|disc|disk|set|part|side|tape)[\s._-]+[a-z](?:$|[\s._-])|[a-z]$|\d{1,2}$)",
        )
        .unwrap()
    })
}

/// CD1/CD2/Set 1/A/B sub-folders belong to the show folder above them.
fn show_root_for(dir: &Path, top: Option<&Path>) -> PathBuf {
    let name = file_name(dir);
    // A DVD's VIDEO_TS folder: the DVD is the folder above (itself maybe "DVD1" of a show).
    if name.eq_ignore_ascii_case("VIDEO_TS") {
        if let Some(parent) = dir.parent() {
            return show_root_for(parent, top);
        }
    }
    if top != Some(dir) && disc_dir_re().is_match(&name) {
        if let Some(parent) = dir.parent() {
            return parent.to_path_buf();
        }
    }
    dir.to_path_buf()
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn walk(dir: &Path, top: &Path, excluded: &HashSet<PathBuf>, shows: &mut BTreeMap<PathBuf, Vec<PathBuf>>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut audio = Vec::new();
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = file_name(&path);
        if name.starts_with('.') {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            subdirs.push(path);
        } else if Format::from_path(&path).is_some_and(|f| f != Format::Vob || crate::dvd::title_vob(&path)) {
            audio.push(path);
        }
    }
    if !audio.is_empty() {
        let root = show_root_for(dir, Some(top));
        if !excluded.contains(&root) {
            let entry = shows.entry(root).or_default();
            entry.extend(audio.into_iter().filter(|p| !excluded.contains(p)));
        }
    }
    for sub in subdirs {
        if !excluded.contains(&sub) {
            walk(&sub, top, excluded, shows);
        }
    }
}

pub fn scan_paths(paths: &[PathBuf], excluded: &HashSet<PathBuf>) -> Vec<Show> {
    let mut shows: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for raw in paths {
        let path = absolute(raw);
        if path.is_file() {
            if Format::from_path(&path).is_some() && !excluded.contains(&path) {
                if let Some(parent) = path.parent() {
                    let root = show_root_for(parent, None);
                    if !excluded.contains(&root) {
                        shows.entry(root).or_default().push(path);
                    }
                }
            }
        } else if path.is_dir() {
            walk(&path, &path, excluded, &mut shows);
        }
    }
    let mut list: Vec<Show> = shows
        .into_iter()
        .map(|(root, mut files)| {
            // A video beside the audio of a show is a bonus, not a track: MP4 files count
            // only in a show made of them (a concert video).
            if files.iter().any(|f| Format::from_path(f) != Some(Format::Mp4)) {
                files.retain(|f| Format::from_path(f) != Some(Format::Mp4));
            }
            (root, files)
        })
        .filter(|(_, files)| !files.is_empty())
        .map(|(root, mut files)| {
            files.sort_by(|a, b| natural_path_cmp(a, b));
            files.dedup();
            Show { root, files }
        })
        .collect();
    list.sort_by(|a, b| natural_path_cmp(&a.root, &b.root));
    list
}

/// Every file below `root`, recursively (hidden entries skipped when `skip_hidden`).
pub fn all_files(root: &Path, skip_hidden: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if skip_hidden && file_name(&path).starts_with('.') {
                continue;
            }
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(t) if t.is_file() => out.push(path),
                _ => {}
            }
        }
    }
    out.sort_by(|a, b| natural_path_cmp(a, b));
    out
}

// ---------------------------------------------------------------- checksum files

/// Windows-1252 characters for bytes 0x80-0x9F (checksum files made on Windows).
const CP1252_HIGH: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}',
    '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
];

pub fn read_text(path: &Path) -> std::io::Result<String> {
    let data = fs::read(path)?;
    let data = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&data);
    Ok(match std::str::from_utf8(data) {
        Ok(text) => text.to_string(),
        Err(_) => data
            .iter()
            .map(|&b| if (0x80..0xA0).contains(&b) { CP1252_HIGH[(b - 0x80) as usize] } else { b as char })
            .collect(),
    })
}

fn is_md5_hex(text: &str) -> bool {
    text.len() == 32 && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// (kind, [(file name as written, lowercase hex digest)])
pub fn parse_checksum_file(path: &Path) -> std::io::Result<(String, Vec<(String, String)>)> {
    let kind = ext_lower(path);
    let mut entries = Vec::new();
    for line in read_text(path)?.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if kind == "sfv" {
            // "name with spaces.flac 1A2B3C4D": the CRC32 is the last word.
            if let Some((name, crc)) = line.rsplit_once(char::is_whitespace) {
                let crc = crc.trim().to_ascii_lowercase();
                if crc.len() == 8 && crc.chars().all(|c| c.is_ascii_hexdigit()) && !name.trim().is_empty() {
                    entries.push((name.trim().to_string(), crc));
                }
            }
        } else if kind == "ffp" {
            if let Some((name, digest)) = line.rsplit_once(':') {
                let digest = digest.trim().to_ascii_lowercase();
                if is_md5_hex(&digest) {
                    entries.push((name.trim().to_string(), digest));
                }
            }
        } else if let Some((digest, rest)) = line.split_at_checked(32).filter(|(d, _)| line.len() > 33 && is_md5_hex(d)) {
            let mut rest = rest.trim_start();
            if let Some(after) = rest.strip_prefix("[shntool]") {
                rest = after.trim_start();
            }
            let rest = rest.strip_prefix('*').unwrap_or(rest).trim_end();
            if !rest.is_empty() {
                entries.push((rest.to_string(), digest.to_ascii_lowercase()));
            }
        }
    }
    Ok((kind, entries))
}

/// Find the file a checksum line refers to (Windows paths, case differences...).
pub fn resolve_entry(base: &Path, name: &str) -> Option<PathBuf> {
    let mut rel = name.replace('\\', "/");
    while let Some(stripped) = rel.strip_prefix("./") {
        rel = stripped.to_string();
    }
    // Same path, the exact case first, then ignoring case. Names are always taken from the
    // directory listing: on Windows a path with the wrong case opens fine, but would not be
    // the path of the file in the list.
    let mut current = base.to_path_buf();
    let mut matched = true;
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        let found = fs::read_dir(&current).ok().and_then(|entries| {
            let names: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            let exact = names.iter().find(|p| file_name(p) == part).cloned();
            exact.or_else(|| names.into_iter().find(|p| file_name(p).to_lowercase() == part.to_lowercase()))
        });
        match found {
            Some(p) => current = p,
            None => {
                matched = false;
                break;
            }
        }
    }
    if matched && current.is_file() {
        return Some(current);
    }
    // Same file name anywhere below.
    let target = rel.rsplit('/').next().unwrap_or(&rel).to_lowercase();
    all_files(base, false).into_iter().find(|p| file_name(p).to_lowercase() == target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dvd_titles_join_their_show() {
        let tmp = tempfile::tempdir().unwrap();
        let ts = tmp.path().join("Band - 1990-01-01 (DVD)").join("VIDEO_TS");
        fs::create_dir_all(&ts).unwrap();
        for name in ["VIDEO_TS.VOB", "VIDEO_TS.IFO", "VTS_01_0.VOB", "VTS_01_0.IFO", "VTS_01_1.VOB", "VTS_01_2.VOB", "VTS_02_1.VOB"] {
            fs::write(ts.join(name), b"x").unwrap();
        }
        let shows = scan_paths(&[tmp.path().to_path_buf()], &HashSet::new());
        assert_eq!(shows.len(), 1);
        assert_eq!(file_name(&shows[0].root), "Band - 1990-01-01 (DVD)");
        let names: Vec<String> = shows[0].files.iter().map(|f| file_name(f)).collect();
        assert_eq!(names, ["VTS_01_1.VOB", "VTS_02_1.VOB"]);
    }

    #[test]
    fn videos_are_tracks_only_on_their_own() {
        let tmp = tempfile::tempdir().unwrap();
        let (audio, video) = (tmp.path().join("Band - 1990-01-01"), tmp.path().join("Band - 1991-01-01 (video)"));
        for (dir, names) in [(&audio, &["01 - Intro.flac", "bonus.mp4"][..]), (&video, &["Band 1991.mp4"][..])] {
            fs::create_dir(dir).unwrap();
            for name in names {
                fs::write(dir.join(name), b"x").unwrap();
            }
        }
        let shows = scan_paths(&[tmp.path().to_path_buf()], &HashSet::new());
        let names: Vec<Vec<String>> = shows.iter().map(|s| s.files.iter().map(|f| file_name(f)).collect()).collect();
        assert_eq!(names, [vec!["01 - Intro.flac"], vec!["Band 1991.mp4"]]);
    }

    #[test]
    fn show_names() {
        let show = "Iron Maiden - 1995-11-21 - Real Madrid Pavilion, Madrid, Spain [taped by EGH] NFT [1h53m16s]";
        assert_eq!(show_name(&Path::new("/m").join(show).join("AUDIO")), format!("{show}/AUDIO"));
        assert_eq!(show_name(&Path::new("/m").join(show)), show);
        assert_eq!(show_name(Path::new("/m/Show/CD1")), "CD1");
        let outer = "Iron Maiden - 1995-11-21 - Real Madrid Pavilion, Madrid, Spain [taped by MetalPriest] [AUD] [1h53m35s]";
        let inner = "1995-11-21 - Madrid (Version 2)";
        assert_eq!(show_name(&Path::new("/m").join(outer).join(inner)), format!("{outer}/{inner}"));
        assert_eq!(show_name(&Path::new("/m/Iron Maiden").join(inner)), inner);
    }

    #[test]
    fn resolve_keeps_the_real_name() {
        let dir = std::env::temp_dir().join(format!("bh-resolve-{}", std::process::id()));
        fs::create_dir_all(dir.join("CD1")).unwrap();
        fs::write(dir.join("CD1").join("04 - Chatter.flac"), b"x").unwrap();
        let found = resolve_entry(&dir, "cd1\\04 - chatter.FLAC").unwrap();
        assert_eq!(found, dir.join("CD1").join("04 - Chatter.flac"));
        assert_eq!(resolve_entry(&dir, "./CD1/04 - Chatter.flac").unwrap(), dir.join("CD1").join("04 - Chatter.flac"));
        assert_eq!(resolve_entry(&dir, "elsewhere/04 - CHATTER.flac").unwrap(), dir.join("CD1").join("04 - Chatter.flac"));
        assert!(resolve_entry(&dir, "05.flac").is_none());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Found by fuzzing: a line with accented letters was cut inside a character.
    #[test]
    fn checksum_lines_with_accents() {
        let dir = std::env::temp_dir().join(format!("bt-accents-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("show.md5");
        let digest = "0123456789abcdef0123456789abcdef";
        fs::write(&file, format!("{digest} *01 - Café.flac\n0123456789abcdef0123456789abcdé ok\néééééééééééééééééééééééééé.flac\n")).unwrap();
        let (_, entries) = parse_checksum_file(&file).unwrap();
        assert_eq!(entries, vec![("01 - Café.flac".to_string(), digest.to_string())]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sfv_lines() {
        let dir = std::env::temp_dir().join(format!("bh-sfv-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let sfv = dir.join("set.sfv");
        fs::write(&sfv, "; made by some tool\r\n;  12345  file.flac\r\nCD1\\01 - A b.flac 1a2B3c4D\r\nbad line\r\n02.flac\tDEADBEEF\r\n").unwrap();
        let (kind, entries) = parse_checksum_file(&sfv).unwrap();
        assert_eq!(kind, "sfv");
        assert_eq!(entries, vec![("CD1\\01 - A b.flac".to_string(), "1a2b3c4d".to_string()), ("02.flac".to_string(), "deadbeef".to_string())]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["d1t10.flac", "d1t2.flac", "d1t01.flac", "D1T03.flac"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["d1t01.flac", "d1t2.flac", "D1T03.flac", "d1t10.flac"]);
    }

    #[test]
    fn disc_folders() {
        assert!(disc_dir_re().is_match("CD1"));
        assert!(disc_dir_re().is_match("Disc 2"));
        assert!(disc_dir_re().is_match("set_1"));
        assert!(!disc_dir_re().is_match("Concert"));
        for disc in ["A", "b", "1", "02", "Side A", "Set B", "Part 2", "Tape 1", "Vol. 1", "CD A", "Disc 2 - Encore", "Disc One", "CD Two", "disc_three"] {
            assert!(disc_dir_re().is_match(disc), "{disc}");
        }
        for show in ["Dave Matthews Band", "Doors 1968", "Setlist", "Tapes", "Sideshow", "AB", "Part of it", "1977-05-08", "123", "Disco Inferno", "Side Oneida"] {
            assert!(!disc_dir_re().is_match(show), "{show}");
        }
    }
}
