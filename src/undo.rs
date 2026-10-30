//! Undo journal of "Tag and rename from info file": before a show is changed, what can bring
//! it back exactly as it was (the FLAC headers, the checksum files, the former names) is kept
//! in the program's data folder, one folder per run. Rolling back gives the files back
//! byte for byte, so the checksums and torrents made before are valid again.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::files::file_name;

/// Runs older than this are deleted when a new one starts.
pub const KEEP_DAYS: u64 = 30;
/// Bytes read at the start of a file to see whether it changed since (the tags are there).
const STAMP_BYTES: u64 = 1 << 20;

/// One run of "Tag and rename" (the shows of one Apply).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Batch {
    /// Seconds since 1970 (UTC).
    pub created: u64,
    /// Nanoseconds within that second: orders two runs of the same second (absent from
    /// older journals).
    #[serde(default)]
    pub nanos: u32,
    pub shows: Vec<ShowEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ShowEntry {
    pub root: PathBuf,
    pub name: String,
    pub files: Vec<FileEntry>,
    /// Checksum files as they were, and the copy that holds them.
    pub checksums: Vec<(PathBuf, String)>,
    pub rolled_back: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileEntry {
    /// The name before, the name after (the same when only the tags changed).
    pub old: PathBuf,
    pub new: PathBuf,
    /// The copy of the FLAC header before tagging (none when only renamed).
    pub head: Option<String>,
    /// Size before.
    pub size: u64,
    /// The file just after the change, to see whether something else changed it since.
    pub after: Option<Stamp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub size: u64,
    pub crc: u32,
}

pub fn stamp(path: &Path) -> Result<Stamp, String> {
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    let mut start = Vec::new();
    (&mut file).take(STAMP_BYTES).read_to_end(&mut start).map_err(|e| e.to_string())?;
    Ok(Stamp { size, crc: crc32fast::hash(&start) })
}

/// Where the runs are kept: `undo` in the program's data folder (BT_UNDO_DIR for the tests).
pub fn dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("BT_UNDO_DIR") {
        return Some(PathBuf::from(dir));
    }
    eframe::storage_dir("bootleg-toolbox").map(|d| d.join("undo"))
}

/// The runs, the latest first, with their folder.
pub fn list() -> Vec<(PathBuf, Batch)> {
    dir().map(|dir| runs_in(&dir)).unwrap_or_default()
}

fn runs_in(dir: &Path) -> Vec<(PathBuf, Batch)> {
    let mut runs: Vec<(PathBuf, Batch)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let text = fs::read_to_string(e.path().join("journal.ron")).ok()?;
            Some((e.path(), ron::from_str(&text).ok()?))
        })
        .filter(|(_, b): &(PathBuf, Batch)| !b.shows.is_empty())
        .collect();
    // The folder name last: two older journals of the same second (no nanoseconds) in a
    // fixed order, whatever order the file system lists them in.
    runs.sort_by(|(pa, a), (pb, b)| (b.created, b.nanos, pb).cmp(&(a.created, a.nanos, pa)));
    runs
}

/// A folder of its own for a new run: "<seconds>-<nanoseconds>-<process>", never one that
/// exists (two runs of one process in the same second would otherwise share it).
fn new_run(dir: &Path, since: Duration) -> Option<PathBuf> {
    fs::create_dir_all(dir).ok()?;
    let base = format!("{}-{:09}-{}", since.as_secs(), since.subsec_nanos(), std::process::id());
    (0..100).find_map(|i| {
        let run = dir.join(if i == 0 { base.clone() } else { format!("{base}-{i}") });
        fs::create_dir(&run).ok().map(|()| run)
    })
}

/// Bytes taken by a run.
pub fn size(run: &Path) -> u64 {
    fs::read_dir(run).into_iter().flatten().flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()
}

pub fn save(run: &Path, batch: &Batch) -> Result<(), String> {
    let text = ron::ser::to_string_pretty(batch, Default::default()).map_err(|e| e.to_string())?;
    let tmp = run.join("journal.ron.part");
    fs::write(&tmp, text).map_err(|e| e.to_string())?;
    fs::rename(&tmp, run.join("journal.ron")).map_err(|e| e.to_string())
}

/// Time since 1970 (UTC).
fn now() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default()
}

/// "2026-10-29 18:58:07 UTC".
pub fn date(secs: u64) -> String {
    // Days since 1970 to a civil date (Howard Hinnant's algorithm).
    let days = (secs / 86400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC", secs % 86400 / 3600, secs % 3600 / 60, secs % 60)
}

/// "3 hours ago".
pub fn age(secs: u64) -> String {
    let d = now().as_secs().saturating_sub(secs);
    match d {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", d / 60),
        3600..86400 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86400),
    }
}

/// Writes the journal of the run being made, show after show.
pub struct Recorder {
    run: Option<PathBuf>,
    batch: Batch,
    show: Option<ShowEntry>,
    blobs: usize,
}

impl Recorder {
    /// A new run, after deleting those older than `KEEP_DAYS`. Without a data folder
    /// nothing is kept (`active` is false).
    pub fn new() -> Recorder {
        let since = now();
        let (created, nanos) = (since.as_secs(), since.subsec_nanos());
        let run = dir().and_then(|dir| {
            for (old, batch) in runs_in(&dir) {
                if created.saturating_sub(batch.created) > KEEP_DAYS * 86400 {
                    let _ = fs::remove_dir_all(old);
                }
            }
            new_run(&dir, since)
        });
        Recorder { run, batch: Batch { created, nanos, shows: Vec::new() }, show: None, blobs: 0 }
    }

    pub fn active(&self) -> bool {
        self.run.is_some()
    }

    fn blob(&mut self, data: &[u8]) -> Result<String, String> {
        let run = self.run.as_ref().ok_or("no undo folder")?;
        self.blobs += 1;
        let name = format!("{:05}.bin", self.blobs);
        fs::write(run.join(&name), data).map_err(|e| format!("cannot save the undo copy: {e}"))?;
        Ok(name)
    }

    /// Starts a show: its checksum files are saved as they are.
    pub fn begin_show(&mut self, root: &Path, checksums: &[PathBuf]) -> Result<(), String> {
        let mut entry = ShowEntry { root: root.to_path_buf(), name: crate::files::show_name(root), ..Default::default() };
        if self.active() {
            for sum in checksums {
                let data = fs::read(sum).map_err(|e| format!("cannot read {}: {e}", file_name(sum)))?;
                entry.checksums.push((sum.clone(), self.blob(&data)?));
            }
        }
        self.show = Some(entry);
        Ok(())
    }

    /// Saves the FLAC header of a file about to be tagged; returns where it is kept.
    pub fn save_head(&mut self, path: &Path) -> Result<Option<String>, String> {
        if !self.active() {
            return Ok(None);
        }
        let head = crate::tagging::flac_head(path)?;
        self.blob(&head).map(Some)
    }

    /// A file changed: its name before and after, its saved header.
    pub fn changed(&mut self, old: &Path, new: &Path, head: Option<String>, size: u64) {
        let Some(show) = self.show.as_mut() else { return };
        let after = stamp(new).ok();
        match show.files.iter_mut().find(|f| f.new == old) {
            Some(f) => {
                f.new = new.to_path_buf();
                f.after = after;
                if head.is_some() {
                    f.head = head;
                }
            }
            None => show.files.push(FileEntry { old: old.to_path_buf(), new: new.to_path_buf(), head, size, after }),
        }
    }

    /// Ends the show and writes the journal (a show where nothing changed is left out).
    pub fn end_show(&mut self) -> Result<(), String> {
        let Some(show) = self.show.take() else { return Ok(()) };
        if show.files.is_empty() {
            return Ok(());
        }
        self.batch.shows.push(show);
        match &self.run {
            Some(run) => save(run, &self.batch),
            None => Ok(()),
        }
    }

    /// Removes the run folder when nothing was changed.
    pub fn finish(self) {
        if let Some(run) = self.run {
            if self.batch.shows.is_empty() {
                let _ = fs::remove_dir_all(run);
            }
        }
    }
}

impl Default for Recorder {
    fn default() -> Self {
        Recorder::new()
    }
}

/// What rolling back one show did.
#[derive(Debug, Default)]
pub struct Outcome {
    pub restored: usize,
    /// Files left as they are, with the reason.
    pub skipped: Vec<(PathBuf, String)>,
    pub checksums: usize,
}

/// Gives the files of a show back as they were: headers, names, then the checksum files
/// (only when every file could be given back, otherwise they would not match).
pub fn roll_back(run: &Path, show: &ShowEntry, cancel: &std::sync::atomic::AtomicBool) -> Outcome {
    let mut out = Outcome::default();
    let mut ready = Vec::new();
    for f in &show.files {
        let check = || -> Result<(), String> {
            let now = stamp(&f.new).map_err(|_| "not found".to_string())?;
            if f.after.is_some_and(|a| a != now) {
                return Err("changed since (by another program?)".into());
            }
            if f.old != f.new && f.old.exists() {
                return Err(format!("{} exists already", file_name(&f.old)));
            }
            Ok(())
        };
        match check() {
            Ok(()) => ready.push(f),
            Err(e) => out.skipped.push((f.new.clone(), e)),
        }
    }
    // Headers first, at the current names; the audio must be the same afterwards.
    let mut done = Vec::new();
    for f in ready {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            out.skipped.push((f.new.clone(), "stopped".into()));
            continue;
        }
        let result = match &f.head {
            None => Ok(()),
            Some(blob) => fs::read(run.join(blob)).map_err(|e| format!("undo copy missing: {e}")).and_then(|head| {
                let before = crate::audio::probe(&f.new).map(|i| (i.md5, i.samples));
                crate::tagging::restore_flac_head(&f.new, &head)?;
                let after = crate::audio::probe(&f.new).map(|i| (i.md5, i.samples));
                if before.is_ok() && after != before {
                    return Err("the audio signature changed (please report this)".into());
                }
                let size = fs::metadata(&f.new).map(|m| m.len()).unwrap_or(0);
                if size != f.size {
                    return Err(format!("size {size} instead of {} (please report this)", f.size));
                }
                Ok(())
            }),
        };
        match result {
            Ok(()) => done.push(f),
            Err(e) => out.skipped.push((f.new.clone(), e)),
        }
    }
    // Former names, through temporary names (two files may swap their names).
    let mut parked = Vec::new();
    for (i, f) in done.iter().enumerate() {
        if f.old == f.new {
            out.restored += 1;
            continue;
        }
        let temp = f.new.with_file_name(format!(".bt-undo-{i}-{}", std::process::id()));
        match fs::rename(&f.new, &temp) {
            Ok(()) => parked.push((f, temp)),
            Err(e) => out.skipped.push((f.new.clone(), format!("cannot rename: {e}"))),
        }
    }
    for (f, temp) in parked {
        if let Some(dir) = f.old.parent() {
            let _ = fs::create_dir_all(dir);
        }
        match fs::rename(&temp, &f.old) {
            Ok(()) => out.restored += 1,
            Err(e) => {
                let _ = fs::rename(&temp, &f.new);
                out.skipped.push((f.new.clone(), format!("cannot rename: {e}")));
            }
        }
    }
    if out.skipped.is_empty() {
        let mut saved: HashMap<&Path, &str> = HashMap::new();
        for (path, blob) in &show.checksums {
            saved.insert(path, blob);
        }
        for (path, blob) in saved {
            match fs::read(run.join(blob)).and_then(|data| fs::write(path, data)) {
                Ok(()) => out.checksums += 1,
                Err(e) => out.skipped.push((path.to_path_buf(), format!("checksum file not restored: {e}"))),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(date(1_793_300_000), "2026-10-29 18:53:20 UTC");
        assert_eq!(date(951_782_400), "2000-02-29 00:00:00 UTC");
    }

    #[test]
    fn latest_run_first_within_a_second() {
        let tmp = tempfile::tempdir().unwrap();
        let show = ShowEntry::default();
        // Written in this order so that the directory order cannot give the answer.
        // Older journals have no nanoseconds (0): the folder name orders them.
        for (name, created, nanos) in [("100-2", 100, 900_000_000), ("100-1", 100, 100_000_000), ("99-3", 99, 999_999_999), ("98-1", 98, 0), ("98-2", 98, 0)] {
            let run = tmp.path().join(name);
            fs::create_dir(&run).unwrap();
            save(&run, &Batch { created, nanos, shows: vec![show.clone()] }).unwrap();
        }
        let order: Vec<String> = runs_in(tmp.path()).iter().map(|(p, _)| crate::files::file_name(p)).collect();
        assert_eq!(order, ["100-2", "100-1", "99-3", "98-2", "98-1"]);
    }

    #[test]
    fn runs_of_one_second_get_their_own_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let since = Duration::new(100, 5);
        let (a, b) = (new_run(tmp.path(), since).unwrap(), new_run(tmp.path(), since).unwrap());
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
    }
}
