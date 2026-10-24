//! Finding shows on disk, natural sorting, and reading checksum files.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use crate::audio::Format;

pub const CHECKSUM_EXT: [&str; 3] = ["ffp", "md5", "st5"];

#[derive(Clone, Debug)]
pub struct Show {
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
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

fn disc_dir_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(cd|disc|disk|dvd|set|d)[\s._-]*\d+").unwrap())
}

/// CD1/CD2/Set 1 sub-folders belong to the show folder above them.
fn show_root_for(dir: &Path, top: Option<&Path>) -> PathBuf {
    let name = file_name(dir);
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
        } else if Format::from_path(&path).is_some() {
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
        if kind == "ffp" {
            if let Some((name, digest)) = line.rsplit_once(':') {
                let digest = digest.trim().to_ascii_lowercase();
                if is_md5_hex(&digest) {
                    entries.push((name.trim().to_string(), digest));
                }
            }
        } else if line.len() > 33 && is_md5_hex(&line[..32]) {
            let mut rest = line[32..].trim_start();
            if let Some(after) = rest.strip_prefix("[shntool]") {
                rest = after.trim_start();
            }
            let rest = rest.strip_prefix('*').unwrap_or(rest).trim_end();
            if !rest.is_empty() {
                entries.push((rest.to_string(), line[..32].to_ascii_lowercase()));
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
    }
}
