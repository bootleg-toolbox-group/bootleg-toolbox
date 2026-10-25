//! Reading a .torrent file and checking files on disk against it: every piece is hashed
//! again (SHA-1) and compared, then the pieces that differ are traced back to their files.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use sha1::{Digest, Sha1};

/// A decoded bencode value; `Dict` keeps the byte span of each value, for the info hash.
#[derive(Debug)]
enum Value {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(BTreeMap<Vec<u8>, (Value, (usize, usize))>),
}

fn parse(data: &[u8], pos: usize, depth: usize) -> Result<(Value, usize), String> {
    let bad = || "not a valid .torrent file".to_string();
    if depth > 64 {
        return Err(bad());
    }
    match data.get(pos).ok_or_else(bad)? {
        b'i' => {
            let end = pos + 1 + data[pos + 1..].iter().position(|&b| b == b'e').ok_or_else(bad)?;
            let n = std::str::from_utf8(&data[pos + 1..end]).ok().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
            Ok((Value::Int(n), end + 1))
        }
        b'l' => {
            let (mut items, mut p) = (Vec::new(), pos + 1);
            while *data.get(p).ok_or_else(bad)? != b'e' {
                let (v, next) = parse(data, p, depth + 1)?;
                items.push(v);
                p = next;
            }
            Ok((Value::List(items), p + 1))
        }
        b'd' => {
            let (mut map, mut p) = (BTreeMap::new(), pos + 1);
            while *data.get(p).ok_or_else(bad)? != b'e' {
                let (key, next) = parse(data, p, depth + 1)?;
                let Value::Bytes(key) = key else { return Err(bad()) };
                let (v, end) = parse(data, next, depth + 1)?;
                map.insert(key, (v, (next, end)));
                p = end;
            }
            Ok((Value::Dict(map), p + 1))
        }
        b'0'..=b'9' => {
            let colon = pos + data[pos..].iter().position(|&b| b == b':').ok_or_else(bad)?;
            let len: usize = std::str::from_utf8(&data[pos..colon]).ok().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
            let end = colon.checked_add(1 + len).filter(|&e| e <= data.len()).ok_or_else(bad)?;
            Ok((Value::Bytes(data[colon + 1..end].to_vec()), end))
        }
        _ => Err(bad()),
    }
}

/// What a .torrent says.
pub struct Torrent {
    pub name: String,
    /// Path inside the torrent (relative to the folder `name` for a multi-file torrent) + length.
    pub files: Vec<(Vec<String>, u64)>,
    pub single_file: bool,
    pub piece_length: u64,
    pub pieces: Vec<[u8; 20]>,
    pub private: bool,
    pub source: Option<String>,
    /// Tracker address only: the passkey in an announce URL is personal, never shown.
    pub tracker: Option<String>,
    pub created_by: Option<String>,
    pub info_hash: String,
}

impl Torrent {
    pub fn total(&self) -> u64 {
        self.files.iter().map(|(_, n)| n).sum()
    }
}

fn text(v: Option<&(Value, (usize, usize))>) -> Option<String> {
    match v {
        Some((Value::Bytes(b), _)) => Some(String::from_utf8_lossy(b).into_owned()),
        _ => None,
    }
}

/// "http://tracker.example.org:2710/abcdef123/announce" -> "tracker.example.org"
fn tracker_host(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?']).next().unwrap_or(rest);
    host.rsplit('@').next().unwrap_or(host).split(':').next().unwrap_or(host).to_string()
}

pub fn read(path: &Path) -> Result<Torrent, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let (top, _) = parse(&data, 0, 0)?;
    let Value::Dict(top) = top else { return Err("not a valid .torrent file".into()) };
    let Some((Value::Dict(info), (start, end))) = top.get(b"info".as_slice()) else {
        return Err("not a valid .torrent file (no info section)".into());
    };
    let info_hash: String = Sha1::digest(&data[*start..*end]).iter().map(|b| format!("{b:02x}")).collect();
    let name = text(info.get(b"name.utf-8".as_slice())).or_else(|| text(info.get(b"name".as_slice()))).ok_or("the torrent has no name")?;
    let piece_length = match info.get(b"piece length".as_slice()) {
        Some((Value::Int(n), _)) if *n > 0 => *n as u64,
        _ => return Err("the torrent has no piece length".into()),
    };
    let pieces = match info.get(b"pieces".as_slice()) {
        Some((Value::Bytes(b), _)) if b.len() % 20 == 0 => b.chunks_exact(20).map(|c| c.try_into().unwrap()).collect(),
        _ => return Err("the torrent has no piece hashes".into()),
    };
    let (files, single_file) = match (info.get(b"files".as_slice()), info.get(b"length".as_slice())) {
        (Some((Value::List(list), _)), _) => {
            let mut files = Vec::new();
            for f in list {
                let Value::Dict(f) = f else { return Err("not a valid .torrent file".into()) };
                let len = match f.get(b"length".as_slice()) {
                    Some((Value::Int(n), _)) if *n >= 0 => *n as u64,
                    _ => return Err("a file of the torrent has no length".into()),
                };
                let parts = match f.get(b"path.utf-8".as_slice()).or_else(|| f.get(b"path".as_slice())) {
                    Some((Value::List(parts), _)) => parts
                        .iter()
                        .map(|p| match p {
                            Value::Bytes(b) => Ok(String::from_utf8_lossy(b).into_owned()),
                            _ => Err("not a valid .torrent file".to_string()),
                        })
                        .collect::<Result<Vec<String>, String>>()?,
                    _ => return Err("a file of the torrent has no path".into()),
                };
                // Padding files (BEP 47) only align pieces: they are not on disk.
                let padding = matches!(f.get(b"attr".as_slice()), Some((Value::Bytes(a), _)) if a.contains(&b'p'));
                if parts.iter().any(|p| p == ".." || p.contains(['/', '\\'])) {
                    return Err("the torrent has an unsafe file path".into());
                }
                files.push((if padding { Vec::new() } else { parts }, len));
            }
            (files, false)
        }
        (_, Some((Value::Int(n), _))) if *n >= 0 => (vec![(vec![name.clone()], *n as u64)], true),
        _ => return Err("the torrent lists no files".into()),
    };
    if name.contains(['/', '\\']) || name == ".." {
        return Err("the torrent has an unsafe name".into());
    }
    let private = matches!(info.get(b"private".as_slice()), Some((Value::Int(1), _)));
    let announce = text(top.get(b"announce".as_slice())).or_else(|| match top.get(b"announce-list".as_slice()) {
        Some((Value::List(tiers), _)) => tiers.iter().find_map(|t| match t {
            Value::List(urls) => urls.iter().find_map(|u| match u {
                Value::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
                _ => None,
            }),
            _ => None,
        }),
        _ => None,
    });
    Ok(Torrent {
        name,
        files,
        single_file,
        piece_length,
        pieces,
        private,
        source: text(info.get(b"source".as_slice())),
        tracker: announce.map(|a| tracker_host(&a)),
        created_by: text(top.get(b"created by".as_slice())),
        info_hash,
    })
}

/// One file of the torrent, as found on disk.
pub struct FileCheck {
    /// Path inside the torrent ("CD1/01 Track.flac").
    pub rel: String,
    /// Where it is (or would be) on disk; None for a padding file.
    pub path: Option<PathBuf>,
    pub expected: u64,
    /// Size on disk; None when missing.
    pub found: Option<u64>,
    /// Pieces of this file that do not match.
    pub bad_pieces: usize,
    pub pieces: usize,
}

pub struct Check {
    pub files: Vec<FileCheck>,
    pub bad_pieces: usize,
    /// Files in the folder that the torrent does not list.
    pub extra: Vec<PathBuf>,
}

/// The file on disk for a path of the torrent: exact name first, then any letter case
/// (a torrent made on Windows, checked on Linux).
fn locate(root: &Path, parts: &[String]) -> PathBuf {
    let mut current = root.to_path_buf();
    for part in parts {
        let exact = current.join(part);
        if exact.exists() {
            current = exact;
            continue;
        }
        let lower = part.to_lowercase();
        let found = std::fs::read_dir(&current)
            .ok()
            .and_then(|entries| entries.flatten().map(|e| e.path()).find(|p| crate::files::file_name(p).to_lowercase() == lower));
        current = found.unwrap_or(exact);
    }
    current
}

/// Checks the data under `root` (the torrent's folder; for a single-file torrent, the
/// folder that holds the file) against `torrent`, hashing pieces in parallel. `progress`
/// is called after each batch with the number of pieces it hashed.
pub fn check(torrent: &Torrent, root: &Path, extra_candidates: Vec<PathBuf>, cancel: &AtomicBool, progress: &(dyn Fn(usize) + Sync)) -> Result<Check, String> {
    // Every file's place in the torrent's byte stream.
    let mut offset = 0u64;
    let mut spans: Vec<(Option<PathBuf>, u64, u64)> = Vec::new();
    let mut files = Vec::new();
    for (parts, len) in &torrent.files {
        let path = (!parts.is_empty()).then(|| locate(root, parts));
        let found = path.as_ref().and_then(|p| std::fs::metadata(p).ok()).filter(|m| m.is_file()).map(|m| m.len());
        spans.push((path.clone(), offset, *len));
        files.push(FileCheck {
            rel: if parts.is_empty() { "(padding)".into() } else { parts.join("/") },
            path,
            expected: *len,
            found: if parts.is_empty() { Some(*len) } else { found },
            bad_pieces: 0,
            pieces: 0,
        });
        offset += len;
    }
    let total = offset;
    let expected_pieces = total.div_ceil(torrent.piece_length) as usize;
    if expected_pieces != torrent.pieces.len() {
        return Err(format!("damaged torrent: {} piece hashes for {} pieces of data", torrent.pieces.len(), expected_pieces));
    }
    // Hash the pieces in batches, each batch reading its own byte range.
    let batch = 16usize;
    let batches: Vec<usize> = (0..torrent.pieces.len()).step_by(batch).collect();
    let results: Vec<Vec<bool>> = batches
        .par_iter()
        .map(|&first| {
            let last = (first + batch).min(torrent.pieces.len());
            let mut ok = Vec::with_capacity(last - first);
            let mut buf = Vec::with_capacity(torrent.piece_length as usize);
            for piece in first..last {
                if cancel.load(Ordering::Relaxed) {
                    ok.push(false);
                    continue;
                }
                let start = piece as u64 * torrent.piece_length;
                let end = (start + torrent.piece_length).min(total);
                buf.clear();
                let complete = read_range(&spans, start, end, &mut buf);
                ok.push(complete && Sha1::digest(&buf).as_slice() == torrent.pieces[piece]);
            }
            progress(last - first);
            ok
        })
        .collect();
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let ok: Vec<bool> = results.into_iter().flatten().collect();
    // Trace pieces to files.
    for (i, (_, start, len)) in spans.iter().enumerate() {
        if *len == 0 {
            continue;
        }
        let first = (start / torrent.piece_length) as usize;
        let last = ((start + len - 1) / torrent.piece_length) as usize;
        files[i].pieces = last - first + 1;
        files[i].bad_pieces = (first..=last).filter(|&p| !ok[p]).count();
    }
    // A piece shared with a missing or wrong-size neighbour is that neighbour's fault.
    for i in 0..files.len() {
        if files[i].found != Some(files[i].expected) || files[i].bad_pieces == 0 {
            continue;
        }
        let (start, len) = (spans[i].1, spans[i].2);
        let first = (start / torrent.piece_length) as usize;
        let last = ((start + len - 1) / torrent.piece_length) as usize;
        let inner_bad = (first..=last)
            .filter(|&p| !ok[p])
            .filter(|&p| {
                let (ps, pe) = (p as u64 * torrent.piece_length, (p as u64 + 1) * torrent.piece_length);
                // Pieces that also cover a broken neighbour are not counted against this file.
                !spans.iter().enumerate().any(|(j, (_, s, l))| {
                    j != i && *l > 0 && *s < pe && s + l > ps && files[j].found != Some(files[j].expected)
                })
            })
            .count();
        files[i].bad_pieces = inner_bad;
    }
    let listed: std::collections::HashSet<PathBuf> = files.iter().filter_map(|f| f.path.clone()).collect();
    let extra = extra_candidates.into_iter().filter(|p| !listed.contains(p)).collect();
    Ok(Check { bad_pieces: ok.iter().filter(|o| !**o).count(), files, extra })
}

/// The torrent's bytes [start, end) from the files; false when a file is missing or short.
fn read_range(spans: &[(Option<PathBuf>, u64, u64)], start: u64, end: u64, buf: &mut Vec<u8>) -> bool {
    let mut complete = true;
    for (path, fstart, len) in spans {
        let fend = fstart + len;
        if fend <= start || *fstart >= end || *len == 0 {
            continue;
        }
        let (from, to) = (start.max(*fstart) - fstart, end.min(fend) - fstart);
        let want = (to - from) as usize;
        let Some(path) = path else {
            buf.resize(buf.len() + want, 0); // padding file: zeros
            continue;
        };
        let read = File::open(path).and_then(|mut f| {
            f.seek(SeekFrom::Start(from))?;
            let at = buf.len();
            buf.resize(at + want, 0);
            f.read_exact(&mut buf[at..])
        });
        if read.is_err() {
            complete = false;
        }
    }
    complete
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passkey_hidden() {
        assert_eq!(tracker_host("http://tracker.example.org:2710/abc123/announce"), "tracker.example.org");
        assert_eq!(tracker_host("https://user:pw@t.example.net/announce.php?passkey=x"), "t.example.net");
    }

    #[test]
    fn bencode_spans() {
        let data = b"d8:announce3:url4:infod4:name1:xee";
        let (v, end) = parse(data, 0, 0).unwrap();
        assert_eq!(end, data.len());
        let Value::Dict(m) = v else { panic!() };
        let (_, (s, e)) = &m[b"info".as_slice()];
        assert_eq!(&data[*s..*e], b"d4:name1:xe");
    }
}
