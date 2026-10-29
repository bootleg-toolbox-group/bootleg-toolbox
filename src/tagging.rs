//! "Tag and rename from info file": the artist, date, place and titles of a show read from
//! its info file (eTree-style text), checked and corrected by the user in a window, then
//! written to the FLAC files (Vorbis comments). The audio frames are never rewritten: the
//! FFP and ST5 stay valid; the .md5 and .sfv (whole files) are computed again, and renamed
//! files are renamed in every checksum file.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::audio::{self, Format};
use crate::files::{self, ext_lower, file_name};

/// What the info file says about the show.
#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    /// As written: an "Artist:" field, or the first line (empty when that line is not one).
    pub artist: String,
    /// YYYY-MM-DD, or empty.
    pub date: String,
    /// Venue, city, country as written.
    pub place: String,
    /// The tracklist: (disc, when the list gives discs; title).
    pub titles: Vec<(Option<u32>, String)>,
}

fn track_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // "01 - Title", "1. Title", "01) Title", "[01] Title", "d1t01 Title", "d2 t03 - Title".
    RE.get_or_init(|| Regex::new(r"(?i)^\s*[\[(]?(?:d(\d{1,2})[\s._-]*t?|t)?(\d{1,3})(\s*[.):\]]|\s*-|\s)\s*(.*\S)\s*$").unwrap())
}

fn disc_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // "Disc 1", "CD2:", "Disc #1", "-- CD 2 --", "Set 2", "Set II", "Disc Two".
    RE.get_or_init(|| {
        Regex::new(r"(?i)^[\s\-=*#\[(]*(?:cd|disc|disk|d|set|part|side|tape)[\s._#-]*(?:\d{1,2}|one|two|three|four|five|six|i{1,3}|iv|vi?)\b").unwrap()
    })
}

fn time_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // A running time at the end of a title: "(06:37 min)", "[5:23]", "- 5:23", "{1:02:10}".
    RE.get_or_init(|| Regex::new(r"(?i)[\s\-–]*[(\[{]?\s*\d{1,2}:\d{2}(?::\d{2})?(?:\.\d+)?\s*(?:min\.?|mins\.?)?\s*[)\]}]?\s*$").unwrap())
}

fn lead_time_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // A running time before the title: "[04:45] Title", "(5:23) Title".
    RE.get_or_init(|| Regex::new(r"^[(\[{]?\d{1,2}:\d{2}(?::\d{2})?[)\]}]?\s+").unwrap())
}

fn file_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Lines of a fingerprint or checksum list ("D1T01 - Title.flac:d2e4...").
    RE.get_or_init(|| Regex::new(r"(?i)\.(flac|wav|shn|ape|wv|mp3|m4a)\b").unwrap())
}

/// A track line: (disc given by a "d1t01" prefix, number, title).
fn track_line(line: &str) -> Option<(Option<u32>, u32, String)> {
    let c = track_re().captures(line)?;
    let sep = c.get(3)?;
    let rest = c.get(4)?;
    // "56:38 min", "44.1 kHz": a number, not a track ("9. 22 Acacia Avenue" is one).
    if matches!(sep.as_str().trim(), "." | ":") && rest.start() == sep.end() && rest.as_str().starts_with(|ch: char| ch.is_ascii_digit()) {
        return None;
    }
    // "26 January 2008" is a date; "D1T01 - Title.flac:d2e4..." a fingerprint.
    if file_name_re().is_match(rest.as_str()) || date_in(line.trim()).is_some_and(|(_, r)| r.start == 0) {
        return None;
    }
    let title = clean_title(rest.as_str());
    if title.is_empty() || !title.chars().any(|ch| ch.is_alphanumeric()) {
        return None;
    }
    let disc = c.get(1).and_then(|m| m.as_str().parse().ok());
    Some((disc, c.get(2)?.as_str().parse().ok()?, title))
}

/// The title without its running time and footnote marks ("Paranoid*" -> "Paranoid").
fn clean_title(text: &str) -> String {
    let mut t = text.trim().trim_start_matches(['-', '–', '.', ')', ':']).trim().to_string();
    t = lead_time_re().replace(&t, "").to_string();
    t = time_re().replace(&t, "").to_string();
    t.trim_end_matches(['*', '#', '^', '+', '†', ' ', '-']).trim().to_string()
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

fn ymd(year: &str, month: u32, day: u32) -> Option<String> {
    let mut y: u32 = year.parse().ok()?;
    if year.len() == 2 {
        y += if y >= 50 { 1900 } else { 2000 };
    }
    ((1900..2100).contains(&y) && (1..=12).contains(&month) && (1..=31).contains(&day)).then(|| format!("{y:04}-{month:02}-{day:02}"))
}

fn month_number(name: &str) -> Option<u32> {
    let name = name.to_lowercase();
    MONTHS.iter().position(|m| name.starts_with(m)).map(|i| i as u32 + 1)
}

/// The first date in a line, as YYYY-MM-DD: "1971-03-26", "19710326", "Jan. 19, 1974",
/// "Jan 23 & 24, 2026", "March 9. 1979", "19 January 1974", "19.01.1974" (day first),
/// "3/26/71" (month first, unless the first number is above 12), "15-1-91" (day first).
pub fn find_date(line: &str) -> Option<String> {
    date_in(line).map(|(d, _)| d)
}

/// The date and where it is in the line.
fn date_in(line: &str) -> Option<(String, std::ops::Range<usize>)> {
    static RES: OnceLock<[Regex; 5]> = OnceLock::new();
    let res = RES.get_or_init(|| {
        [
            Regex::new(r"\b((?:19|20)\d\d)[-./](\d{1,2})[-./](\d{1,2})\b").unwrap(),
            Regex::new(r"\b((?:19|20)\d\d)(0[1-9]|1[0-2])([0-3]\d)\b").unwrap(),
            Regex::new(r"(?i)\b(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?\s+(\d{1,2})(?:st|nd|rd|th)?(?:\s*(?:&|-|/|and)\s*\d{1,2}(?:st|nd|rd|th)?)?[.,]?\s+((?:19|20)\d\d)\b").unwrap(),
            Regex::new(r"(?i)\b(\d{1,2})(?:st|nd|rd|th)?\.?\s+(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?,?\s+((?:19|20)\d\d)\b").unwrap(),
            Regex::new(r"\b(\d{1,2})([./-])(\d{1,2})[./-]((?:19|20)?\d\d)\b").unwrap(),
        ]
    });
    let at = |c: &regex::Captures| c.get(0).map(|m| m.range()).unwrap_or_default();
    if let Some(c) = res[0].captures(line).or_else(|| res[1].captures(line)) {
        return Some((ymd(&c[1], c[2].parse().ok()?, c[3].parse().ok()?)?, at(&c)));
    }
    if let Some(c) = res[2].captures(line) {
        return Some((ymd(&c[3], month_number(&c[1])?, c[2].parse().ok()?)?, at(&c)));
    }
    if let Some(c) = res[3].captures(line) {
        return Some((ymd(&c[3], month_number(&c[2])?, c[1].parse().ok()?)?, at(&c)));
    }
    let c = res[4].captures(line)?;
    let (a, b): (u32, u32) = (c[1].parse().ok()?, c[3].parse().ok()?);
    // US style with slashes, day first otherwise.
    let date = if &c[2] == "/" && a <= 12 { ymd(&c[4], a, b) } else { ymd(&c[4], b, a) };
    Some((date?, at(&c)))
}

/// Lines that start the description of the recording, after the header of the show.
fn source_line(line: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(recording|recorded|aud|audience|sbd|soundboard|matrix|fm|broadcast|gen|generation|tape|cassette|dat|lineage|source|transfer|transferred|taped|taper|mics?|flac|wav|shn|seeded|running time|total time|setlist|tracklist|quality|version)\b").unwrap()
    })
    .is_match(line)
}

/// "Artist: ...", "Venue: ...", "Show Venue:" (the value on the next lines).
fn field_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(?:show\s+)?(artist|band|performer|group|date|venue|location|city|state|country|place)\s*[:=]\s*(.*)$").unwrap())
}

/// A line with words: not a separator ("-----", "=====").
fn has_words(line: &str) -> bool {
    line.chars().filter(|c| c.is_alphanumeric()).count() >= 2
}

/// Same name, ignoring case and a trailing "(UK)".
pub fn same_name(a: &str, b: &str) -> bool {
    let base = |s: &str| {
        let s = s.trim();
        let s = match (s.ends_with(')'), s.rfind(" (")) {
            (true, Some(i)) => &s[..i],
            _ => s,
        };
        s.trim().to_lowercase()
    };
    !a.trim().is_empty() && base(a) == base(b)
}

/// Reads an info file: the artist, date and place ("Artist:" fields, otherwise the lines at
/// the top, or a block of lines under the date), and the tracklist (as many titles as the
/// files when a list has that many: `files` is their number). `hint` is the artist as the
/// folder name or the tags give it, to recognise it in the text.
pub fn parse_info(text: &str, files: usize, hint: Option<&str>) -> Parsed {
    let lines: Vec<&str> = text.lines().map(|l| l.trim()).collect();
    let titles = tracklist(&lines, files);
    let mut parsed = Parsed { titles, ..Parsed::default() };
    // The header is before the tracklist.
    let list_start = lines.iter().position(|l| track_line(l).is_some()).unwrap_or(lines.len()).min(150);
    let head = &lines[..list_start];

    // 1. "Key: value" fields.
    let mut places: Vec<(usize, String)> = Vec::new();
    let mut i = 0;
    while i < head.len() {
        if let Some(c) = field_re().captures(head[i]) {
            let key = c[1].to_lowercase();
            let value = c[2].trim().to_string();
            let rank = ["venue", "place", "city", "state", "location", "country"].iter().position(|k| *k == key);
            match key.as_str() {
                "artist" | "band" | "performer" | "group" if parsed.artist.is_empty() && has_words(&value) => parsed.artist = value.clone(),
                "date" if parsed.date.is_empty() => parsed.date = find_date(&value).unwrap_or_default(),
                _ => {}
            }
            if let Some(rank) = rank {
                if !value.is_empty() {
                    places.push((rank, value.trim_end_matches(',').to_string()));
                } else {
                    // The value on the next lines, up to a blank line.
                    while i + 1 < head.len() && !head[i + 1].is_empty() && places.len() < 8 {
                        i += 1;
                        match find_date(head[i]) {
                            Some(d) if parsed.date.is_empty() => parsed.date = d,
                            Some(_) => {}
                            None => places.push((rank, head[i].trim_end_matches(',').to_string())),
                        }
                    }
                }
            }
        }
        i += 1;
    }
    places.sort_by_key(|p| p.0);
    let field_place: Vec<String> = places.into_iter().map(|p| p.1).collect();

    // 2. The lines at the top: the artist on the first line with words. A first line that
    //    holds the date too ("1983-11-27 San Sebastian, Velodromo, Spain") gives the place.
    let mut line_place = String::new();
    let first = head.iter().position(|l| has_words(l));
    if let Some(first) = first {
        let line = head[first];
        match date_in(line) {
            Some((date, range)) => {
                if parsed.date.is_empty() {
                    parsed.date = date;
                }
                let mut rest = format!("{} {}", &line[..range.start], &line[range.end..]);
                if let Some(h) = hint.filter(|h| rest.trim().to_lowercase().starts_with(&h.to_lowercase())) {
                    rest = rest.trim()[h.len()..].to_string();
                    if parsed.artist.is_empty() {
                        parsed.artist = h.to_string();
                    }
                }
                line_place = strip_source_words(&rest);
            }
            None => {
                if parsed.artist.is_empty() {
                    parsed.artist = line.trim_end_matches([',', '-', ':']).trim().to_string();
                }
            }
        }
    }

    // 3. Header blocks (lines between blank lines): the first one, one that starts with the
    //    artist (a header repeated further down) or with the date. The longest place wins.
    let artist = parsed.artist.clone();
    let is_artist = |l: &str| same_name(l, &artist) || hint.is_some_and(|h| same_name(l, h));
    let mut best: Option<(String, String)> = None;
    let mut at = first.unwrap_or(0);
    let mut first_block = true;
    while at < head.len() {
        if head[at].is_empty() || !has_words(head[at]) {
            at += 1;
            continue;
        }
        let end = head[at..].iter().position(|l| l.is_empty()).map_or(head.len(), |n| at + n);
        let block = &head[at..end];
        let start = block.iter().position(|l| is_artist(l)).map(|p| p + 1);
        let dated = find_date(block[0]).is_some() && block[0].len() <= 40;
        let skip = match (start, first_block, dated) {
            (Some(p), _, _) => Some(p),
            (None, true, _) => Some(1),
            (None, false, true) => Some(0),
            _ => None,
        };
        if let Some(skip) = skip {
            let (mut date, mut place) = (String::new(), Vec::new());
            for line in block.iter().skip(skip).take(8) {
                if source_line(line) || field_re().is_match(line) || line.ends_with(':') {
                    break;
                }
                if let Some(d) = find_date(line) {
                    if date.is_empty() {
                        date = d;
                    }
                } else if !line.to_lowercase().contains("tour") && line.len() <= 80 && has_words(line) {
                    place.push(line.trim_end_matches(',').to_string());
                }
            }
            let place = place.join(", ");
            if !date.is_empty() && best.as_ref().is_none_or(|(_, p)| place.len() > p.len()) {
                best = Some((date, place));
            }
        }
        first_block = false;
        at = end;
    }
    if let Some((date, place)) = best {
        if parsed.date.is_empty() {
            parsed.date = date;
        }
        parsed.place = place;
    }
    if !field_place.is_empty() {
        parsed.place = field_place.join(", ");
    } else if parsed.place.is_empty() {
        parsed.place = line_place;
    }
    parsed
}

/// "Paradiso, Amsterdam, The Netherlands FM" -> "Paradiso, Amsterdam, The Netherlands".
fn strip_source_words(text: &str) -> String {
    let mut words: Vec<&str> = text.split_whitespace().collect();
    while words.last().is_some_and(|w| ["fm", "aud", "sbd", "matrix", "pre-fm", "master", "-", "–"].contains(&w.to_lowercase().as_str())) {
        words.pop();
    }
    words.join(" ").trim_matches([' ', ',', '-']).to_string()
}

/// A tracklist: (disc, when the list gives discs; title).
type Titles = Vec<(Option<u32>, String)>;

/// Blocks of short lines without numbers ("Raw Power", "Five Foot One"...), with whether a
/// "Setlist:" heading comes just before.
fn plain_lists(lines: &[&str]) -> Vec<(Titles, bool)> {
    static HEADING: OnceLock<Regex> = OnceLock::new();
    let heading = HEADING.get_or_init(|| Regex::new(r"(?i)\b(set\s*list|track\s*list|tracks|songs)\b").unwrap());
    let title_like = |l: &str| {
        has_words(l) && l.chars().count() <= 70 && !l.ends_with(':') && !field_re().is_match(l) && find_date(l).is_none()
            && track_line(l).is_none() && !file_name_re().is_match(l) && !l.contains("://")
    };
    let mut lists = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        if !title_like(lines[at]) {
            at += 1;
            continue;
        }
        let end = lines[at..].iter().position(|l| !title_like(l)).map_or(lines.len(), |n| at + n);
        if end - at >= 3 {
            let headed = lines[..at].iter().rev().filter(|l| !l.is_empty()).take(1).any(|l| heading.is_match(l));
            lists.push((lines[at..end].iter().map(|l| (None, clean_title(l))).collect(), headed));
        }
        at = end;
    }
    lists
}

/// The numbered lists of the text; numbering that starts again at 1 after a disc or set
/// heading (or with a "d2" prefix) continues the same list as a new disc. Without one that
/// fits, a block of plain titles.
fn tracklist(lines: &[&str], files: usize) -> Vec<(Option<u32>, String)> {
    // A list with a single disc and no "d1" prefixes gives no disc numbers.
    let finish = |run: Vec<(Option<u32>, String)>, explicit: bool| -> Vec<(Option<u32>, String)> {
        if explicit { run } else { run.into_iter().map(|(_, t)| (None, t)).collect() }
    };
    let mut runs: Vec<Vec<(Option<u32>, String)>> = Vec::new();
    let mut run: Vec<(Option<u32>, String)> = Vec::new();
    let (mut last, mut disc, mut heading, mut explicit, mut after_track) = (0u32, 1u32, false, false, false);
    for line in lines {
        if line.is_empty() {
            continue;
        }
        match track_line(line) {
            Some((d, n, title)) => {
                let follows = !run.is_empty() && (n == 1 || n == last + 1);
                let new_disc = follows && (d.is_some_and(|d| d != disc) || heading && d.is_none());
                // The same number twice in a row: a typo in the list ("[22] Panama", "[22] Guitar Solo").
                let repeated = !run.is_empty() && n == last && after_track && d.is_none_or(|d| d == disc);
                if !run.is_empty() && (n == last + 1 || repeated) && !new_disc {
                    // Next track of the same disc.
                } else if new_disc {
                    disc = d.unwrap_or(disc + 1);
                } else {
                    if !run.is_empty() {
                        runs.push(finish(std::mem::take(&mut run), explicit));
                    }
                    disc = d.unwrap_or(1);
                    explicit = false;
                }
                explicit |= d.is_some() || disc > 1;
                run.push((Some(disc), title));
                last = n;
                heading = false;
                after_track = true;
            }
            None => {
                heading |= disc_re().is_match(line);
                after_track = false;
            }
        }
    }
    if !run.is_empty() {
        runs.push(finish(run, explicit));
    }
    runs.retain(|r| r.len() >= 2 || files == 1);
    let plain = plain_lists(lines);
    if let Some(i) = runs.iter().position(|r| r.len() == files) {
        return runs.swap_remove(i);
    }
    if let Some(p) = plain.iter().position(|(l, _)| l.len() == files) {
        return plain.into_iter().nth(p).map(|(l, _)| l).unwrap_or_default();
    }
    // The longest numbered list, or plain list under a "Setlist" heading.
    runs.into_iter().chain(plain.into_iter().filter(|(_, headed)| *headed).map(|(l, _)| l)).max_by_key(|l| l.len()).unwrap_or_default()
}

/// How files are renamed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum NameScheme {
    /// "01 - Title" (numbered within each folder).
    #[default]
    NumberTitle,
    /// "d1t01 - Title".
    DiscTrackTitle,
    /// "Artist 1974-01-19 d1t01" (eTree style).
    Etree,
    /// No new names (the Rename box unticked; the command line's BT_RENAME=keep).
    Keep,
}

impl NameScheme {
    /// The choices of the window (not renaming is the Rename box).
    pub const ALL: [NameScheme; 3] = [NameScheme::NumberTitle, NameScheme::DiscTrackTitle, NameScheme::Etree];
    pub fn label(self) -> &'static str {
        match self {
            NameScheme::NumberTitle => "01 - Title",
            NameScheme::DiscTrackTitle => "d1t01 - Title",
            NameScheme::Etree => "Artist YYYY-MM-DD d1t01",
            NameScheme::Keep => "Do not rename",
        }
    }
}

/// One file of the show and what it becomes.
#[derive(Clone, Debug)]
pub struct TrackPlan {
    pub path: PathBuf,
    pub title: String,
    pub disc: Option<u32>,
    pub track: u32,
    /// Position in its folder (for "01 - Title").
    pub index: u32,
}

/// Disc and track numbers of the files: discs from the CD1/CD2 folders, otherwise from the
/// tracklist; tracks numbered within each disc.
pub fn number_tracks(files: &[PathBuf], titles: &[(Option<u32>, String)]) -> Vec<TrackPlan> {
    let mut folders: Vec<&Path> = Vec::new();
    for f in files {
        let parent = f.parent().unwrap_or(Path::new(""));
        if !folders.contains(&parent) {
            folders.push(parent);
        }
    }
    let by_folder = folders.len() > 1;
    let mut counters: HashMap<Option<u32>, u32> = HashMap::new();
    let mut in_folder: HashMap<&Path, u32> = HashMap::new();
    files
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let parent = path.parent().unwrap_or(Path::new(""));
            let disc = if by_folder {
                folders.iter().position(|f| *f == parent).map(|p| p as u32 + 1)
            } else {
                titles.get(i).and_then(|t| t.0)
            };
            let track = counters.entry(disc).or_insert(0);
            *track += 1;
            let index = in_folder.entry(parent).or_insert(0);
            *index += 1;
            TrackPlan { path: path.clone(), title: titles.get(i).map(|t| t.1.clone()).unwrap_or_default(), disc, track: *track, index: *index }
        })
        .collect()
}

/// A file name part without the characters file systems refuse (and segue marks).
fn safe_name(text: &str) -> String {
    let mut out: String = text
        .chars()
        .map(|c| if c == ':' || c == '/' || c == '\\' || c == '|' { '-' } else { c })
        .filter(|c| !"*?\"<>".contains(*c) && !c.is_control())
        .collect();
    out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let out: String = out.chars().take(120).collect();
    out.trim_end_matches([' ', '-']).trim().to_string()
}

/// The new file name of a track (its extension kept), or None to keep the name.
pub fn new_name(scheme: NameScheme, t: &TrackPlan, artist: &str, date: &str, width: usize) -> Option<String> {
    let ext = t.path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let disc = t.disc.unwrap_or(1);
    let stem = match scheme {
        NameScheme::Keep => return None,
        _ if t.title.trim().is_empty() => return None,
        NameScheme::NumberTitle => format!("{:0width$} - {}", t.index, safe_name(&t.title)),
        NameScheme::DiscTrackTitle => format!("d{disc}t{:02} - {}", t.track, safe_name(&t.title)),
        NameScheme::Etree => safe_name(&format!("{artist} {date} d{disc}t{:02}", t.track)),
    };
    Some(format!("{stem}{ext}"))
}

// ---------------------------------------------------------------- FLAC tags

/// Writes tags in a FLAC file: the given fields replace those of the same name, the others
/// are kept. Only the metadata is written, in place when the padding leaves room for it
/// (the audio frames are not touched); otherwise the file is written again with new
/// padding and the same frames, then put in place of the original.
pub fn write_flac_tags(path: &Path, fields: &[(&str, String)]) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    let mut file = File::open(path).map_err(io)?;
    let (start, blocks, audio_start) = read_flac_head(&mut file)?;
    let old_meta = (audio_start - start - 4) as usize;
    write_flac_blocks(path, file, start, blocks, audio_start, old_meta, fields)
}

/// Metadata blocks of a FLAC file: type, content.
type Blocks = Vec<(u8, Vec<u8>)>;

/// Where the metadata of a FLAC file starts (after an ID3v2 tag some taggers put in front),
/// its blocks (type, content), and where the audio frames start.
fn read_flac_head(file: &mut File) -> Result<(u64, Blocks, u64), String> {
    let io = |e: std::io::Error| e.to_string();
    file.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).map_err(io)?;
    // An ID3v2 tag in front (some taggers put one) is kept as it is.
    let mut start = 0u64;
    if &magic[..3] == b"ID3" {
        let mut rest = [0u8; 6];
        file.read_exact(&mut rest).map_err(io)?;
        let size = rest[2..6].iter().fold(0u64, |acc, b| (acc << 7) | (*b as u64 & 0x7f));
        start = 10 + size + if rest[1] & 0x10 != 0 { 10 } else { 0 };
        file.seek(SeekFrom::Start(start)).map_err(io)?;
        file.read_exact(&mut magic).map_err(io)?;
    }
    if &magic != b"fLaC" {
        return Err("not a FLAC file".into());
    }
    let mut blocks: Blocks = Vec::new();
    loop {
        let mut header = [0u8; 4];
        file.read_exact(&mut header).map_err(|_| "damaged FLAC header".to_string())?;
        let len = u32::from_be_bytes([0, header[1], header[2], header[3]]) as usize;
        let mut data = vec![0u8; len];
        file.read_exact(&mut data).map_err(|_| "damaged FLAC header".to_string())?;
        blocks.push((header[0] & 0x7f, data));
        if header[0] & 0x80 != 0 || blocks.len() > 1000 {
            break;
        }
    }
    let audio_start = file.stream_position().map_err(io)?;
    if blocks.first().map(|b| b.0) != Some(0) {
        return Err("damaged FLAC header (no STREAMINFO first)".into());
    }
    Ok((start, blocks, audio_start))
}

/// Everything before the audio frames of a FLAC file (an ID3v2 tag in front included): what
/// tagging changes, saved by the undo journal.
pub fn flac_head(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let (_, _, audio_start) = read_flac_head(&mut file)?;
    let mut head = vec![0u8; audio_start as usize];
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    file.read_exact(&mut head).map_err(|e| e.to_string())?;
    Ok(head)
}

/// Puts `head` back in front of the audio frames of a FLAC file: in place when it has the
/// size of the current one, otherwise by writing the file again beside it.
pub fn restore_flac_head(path: &Path, head: &[u8]) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    let mut file = File::open(path).map_err(io)?;
    let (_, _, audio_start) = read_flac_head(&mut file)?;
    drop(file);
    if head.len() as u64 == audio_start {
        let mut f = OpenOptions::new().write(true).open(path).map_err(io)?;
        f.write_all(head).map_err(io)?;
        return f.sync_all().map_err(io);
    }
    replace_head(path, head, audio_start)
}

/// Writes `head` then the audio frames of `path` (from `audio_start`) beside it, checks the
/// copy, then puts it in place of the original.
fn replace_head(path: &Path, head: &[u8], audio_start: u64) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    let part = path.with_file_name(format!(".{}.part", file_name(path)));
    let result = (|| -> Result<(), String> {
        let mut src = File::open(path).map_err(io)?;
        let mut out = File::create(&part).map_err(io)?;
        out.write_all(head).map_err(io)?;
        src.seek(SeekFrom::Start(audio_start)).map_err(io)?;
        let copied = std::io::copy(&mut src, &mut out).map_err(io)?;
        out.sync_all().map_err(io)?;
        let total = src.metadata().map_err(io)?.len();
        if copied != total - audio_start || out.metadata().map_err(io)?.len() != head.len() as u64 + copied {
            return Err("the copy is incomplete".into());
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, path).map_err(|e| {
        let _ = fs::remove_file(&part);
        e.to_string()
    })
}

fn write_flac_blocks(path: &Path, file: File, start: u64, blocks: Blocks, audio_start: u64, old_meta: usize, fields: &[(&str, String)]) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();

    // The new VORBIS_COMMENT: the encoder's vendor string and the other fields kept.
    let old = blocks.iter().find(|b| b.0 == 4).map(|b| b.1.clone()).unwrap_or_default();
    let vendor = vorbis_vendor(&old).unwrap_or_else(|| format!("{} {}", crate::ops::APP_NAME, crate::ops::APP_VERSION));
    let replaced: Vec<String> = fields.iter().map(|(k, _)| k.to_uppercase()).collect();
    let mut entries: Vec<String> = fields.iter().filter(|(_, v)| !v.trim().is_empty()).map(|(k, v)| format!("{}={}", k.to_uppercase(), v.trim())).collect();
    entries.extend(vorbis_entries(&old).into_iter().filter(|e| {
        let key = e.split('=').next().unwrap_or("").to_uppercase();
        !replaced.contains(&key)
    }));
    let mut comment = Vec::new();
    comment.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    comment.extend_from_slice(vendor.as_bytes());
    comment.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for e in &entries {
        comment.extend_from_slice(&(e.len() as u32).to_le_bytes());
        comment.extend_from_slice(e.as_bytes());
    }
    if comment.len() >= 1 << 24 {
        return Err("tags too large".into());
    }
    // Same blocks in the same order, the comment where it was (after STREAMINFO when there
    // was none), the padding last.
    let mut kept: Vec<(u8, Vec<u8>)> = Vec::new();
    for (kind, data) in blocks {
        match kind {
            1 => {}
            4 if !comment.is_empty() => kept.push((4, std::mem::take(&mut comment))),
            4 => {}
            _ => kept.push((kind, data)),
        }
    }
    if !comment.is_empty() {
        kept.insert(1, (4, comment));
    }
    let needed: usize = kept.iter().map(|b| 4 + b.1.len()).sum();
    let in_place = old_meta == needed || old_meta >= needed + 4 && old_meta - needed - 4 < 1 << 24;
    let padding = if in_place { old_meta.saturating_sub(needed + 4) } else { 8192 };
    if !(in_place && old_meta == needed) {
        kept.push((1, vec![0u8; padding]));
    }
    let mut meta = Vec::with_capacity(needed + padding + 4);
    let count = kept.len();
    for (i, (kind, data)) in kept.iter().enumerate() {
        let len = data.len() as u32;
        meta.push(*kind | if i + 1 == count { 0x80 } else { 0 });
        meta.extend_from_slice(&len.to_be_bytes()[1..]);
        meta.extend_from_slice(data);
    }
    drop(file);
    if in_place {
        let mut f = OpenOptions::new().write(true).open(path).map_err(io)?;
        f.seek(SeekFrom::Start(start + 4)).map_err(io)?;
        f.write_all(&meta).map_err(io)?;
        f.sync_all().map_err(io)?;
        return Ok(());
    }
    // Written again beside the original, checked, then put in its place.
    let mut head = vec![0u8; start as usize + 4];
    File::open(path).and_then(|mut src| src.read_exact(&mut head)).map_err(io)?;
    head.extend_from_slice(&meta);
    replace_head(path, &head, audio_start)
}

fn vorbis_vendor(d: &[u8]) -> Option<String> {
    let len = u32::from_le_bytes(d.get(..4)?.try_into().ok()?) as usize;
    Some(String::from_utf8_lossy(d.get(4..4 + len)?).into_owned())
}

fn vorbis_entries(d: &[u8]) -> Vec<String> {
    let u32le = |i: usize| d.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let mut out = Vec::new();
    let Some(vendor) = u32le(0) else { return out };
    let mut pos = 4 + vendor;
    let Some(count) = u32le(pos) else { return out };
    pos += 4;
    for _ in 0..count.min(10_000) {
        let Some(len) = u32le(pos) else { break };
        let Some(entry) = d.get(pos + 4..pos + 4 + len) else { break };
        pos += 4 + len;
        out.push(String::from_utf8_lossy(entry).into_owned());
    }
    out
}

// ---------------------------------------------------------------- checksum files

/// Where the file name and the digest are in a line of a checksum file (byte ranges).
fn checksum_line(kind: &str, line: &str) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let trimmed = line.trim_end();
    if trimmed.trim_start().is_empty() || trimmed.trim_start().starts_with([';', '#']) {
        return None;
    }
    let lead = trimmed.len() - trimmed.trim_start().len();
    match kind {
        "sfv" => {
            let (name, crc) = trimmed.rsplit_once(char::is_whitespace)?;
            (crc.len() == 8 && crc.chars().all(|c| c.is_ascii_hexdigit())).then(|| (lead..name.trim_end().len(), trimmed.len() - 8..trimmed.len()))
        }
        "ffp" => {
            let colon = trimmed.rfind(':')?;
            let digest = trimmed[colon + 1..].trim();
            (digest.len() == 32).then(|| (lead..colon, colon + 1..colon + 1 + 32))
        }
        _ => {
            let body = &trimmed[lead..];
            if body.len() <= 33 || !body[..32].chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            let mut rest = body[32..].trim_start();
            if let Some(after) = rest.strip_prefix("[shntool]") {
                rest = after.trim_start();
            }
            let rest = rest.strip_prefix('*').unwrap_or(rest);
            let name_start = trimmed.len() - rest.len();
            Some((name_start..trimmed.len(), lead..lead + 32))
        }
    }
}

/// The file a name of a checksum file refers to, among `known` (same path ignoring case,
/// otherwise the only file of that name).
fn match_entry<'a>(base: &Path, name: &str, known: &'a [PathBuf]) -> Option<&'a PathBuf> {
    let rel = name.trim().replace('\\', "/");
    let rel = rel.trim_start_matches("./");
    let full = base.join(rel).to_string_lossy().to_lowercase();
    if let Some(p) = known.iter().find(|p| p.to_string_lossy().to_lowercase() == full) {
        return Some(p);
    }
    let last = rel.rsplit('/').next().unwrap_or(rel).to_lowercase();
    let mut same = known.iter().filter(|p| file_name(p).to_lowercase() == last);
    match (same.next(), same.next()) {
        (Some(p), None) => Some(p),
        _ => None,
    }
}

/// What changed in a show, for its checksum files.
pub struct Changes {
    /// Old path -> new path.
    pub renamed: HashMap<PathBuf, PathBuf>,
    /// Tagged files (old paths) -> new whole-file MD5 and CRC32 (lowercase hex), when computed.
    pub digests: HashMap<PathBuf, (String, String)>,
}

/// Rewrites one checksum file: renamed files get their new name (the folder part as
/// written), tagged files their new MD5 (.md5) or CRC32 (.sfv). Returns the number of
/// lines changed, and of lines that still hold an old digest (not computed).
pub fn update_checksum_file(path: &Path, changes: &Changes, tagged: &[PathBuf]) -> Result<(usize, usize), String> {
    let kind = ext_lower(path);
    let base = path.parent().unwrap_or(Path::new(""));
    let text = files::read_text(path).map_err(|e| e.to_string())?;
    let known: Vec<PathBuf> = changes.renamed.keys().chain(tagged.iter()).cloned().collect();
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let (mut changed, mut stale) = (0, 0);
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((name_r, digest_r)) = checksum_line(&kind, line) else {
            out.push(line.to_string());
            continue;
        };
        let Some(old) = match_entry(base, &line[name_r.clone()], &known) else {
            out.push(line.to_string());
            continue;
        };
        let mut digest = line[digest_r.clone()].to_string();
        if tagged.contains(old) && matches!(kind.as_str(), "md5" | "sfv") {
            match changes.digests.get(old) {
                Some((md5, crc)) => {
                    let new = if kind == "md5" { md5.clone() } else { crc.clone() };
                    digest = if digest.chars().any(|c| c.is_ascii_uppercase()) { new.to_uppercase() } else { new };
                }
                None => stale += 1,
            }
        }
        let mut name = line[name_r.clone()].to_string();
        if let Some(new) = changes.renamed.get(old) {
            let cut = name.rfind(['/', '\\']).map_or(0, |i| i + 1);
            name = format!("{}{}", &name[..cut], file_name(new));
        }
        // Rebuilt piece by piece, in the order of the line.
        let mut parts = [(name_r, name), (digest_r, digest)];
        parts.sort_by_key(|p| p.0.start);
        let mut rebuilt = String::new();
        let mut at = 0;
        for (range, value) in &parts {
            rebuilt.push_str(&line[at..range.start]);
            rebuilt.push_str(value);
            at = range.end;
        }
        rebuilt.push_str(&line[at..]);
        if rebuilt != line {
            changed += 1;
        }
        out.push(rebuilt);
    }
    if changed > 0 {
        let mut content = out.join(newline);
        if text.ends_with('\n') {
            content.push_str(newline);
        }
        fs::write(path, content).map_err(|e| e.to_string())?;
    }
    Ok((changed, stale))
}

/// The checksum files of a show (in its folder and sub-folders).
pub fn checksum_files(root: &Path) -> Vec<PathBuf> {
    files::all_files(root, true).into_iter().filter(|p| matches!(ext_lower(p).as_str(), "ffp" | "md5" | "st5" | "sfv")).collect()
}

/// Whether a file can be tagged (FLAC only).
pub fn taggable(path: &Path) -> bool {
    Format::from_path(path) == Some(Format::Flac)
}

/// The tags already in the first FLAC file of a show (ARTIST, DATE...), to prefill the window.
pub fn existing_tags(files: &[PathBuf]) -> Vec<(String, String)> {
    files.iter().find(|p| taggable(p)).map(|p| audio::source_tags(p)).unwrap_or_default()
}

// ---------------------------------------------------------------- plan

/// What the window shows and lets the user correct before anything is written.
#[derive(Clone, Debug)]
pub struct Draft {
    pub root: PathBuf,
    pub artist: String,
    pub date: String,
    pub place: String,
    pub album: String,
    pub tracks: Vec<TrackPlan>,
    /// Titles found in the info file (can differ from the number of files).
    pub found: usize,
    /// Titles of the list beyond the last file.
    pub extra: Vec<String>,
    /// The date in the folder name, when there is one.
    pub folder_date: Option<String>,
}

/// The folder whose name describes the show: the show folder, or the one above it when the
/// show folder is an "AUDIO" folder or has no "Artist - date" name of its own.
fn named_folder(show: &files::Show) -> PathBuf {
    match files::outer_folder(&show.root) {
        Some(parent) if files::holder_folder(&file_name(&show.root)) || folder_fields(&file_name(&show.root)).0.is_none() => parent.to_path_buf(),
        _ => show.root.clone(),
    }
}

/// "Artist - 1974-01-19 - Place [54m07s]": the artist, the date, the place (without the
/// bracketed and parenthesised parts).
fn folder_fields(name: &str) -> (Option<String>, Option<String>, Option<String>) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^(.+?)\s+-\s+((?:19|20)\d\d[-.]\d\d[-.]\d\d)\b\s*(?:-\s*(.*))?$").unwrap());
    let Some(c) = re.captures(name.trim()) else { return (None, find_date(name), None) };
    static BRACKETS: OnceLock<Regex> = OnceLock::new();
    let brackets = BRACKETS.get_or_init(|| Regex::new(r"\[[^\]]*\]|\([^()]*(?:\([^()]*\)[^()]*)*\)").unwrap());
    let place = c.get(3).map(|m| brackets.replace_all(m.as_str(), "").split_whitespace().collect::<Vec<_>>().join(" ")).map(|p| p.trim_matches([' ', ',', '-']).to_string()).filter(|p| !p.is_empty());
    (Some(c[1].trim().to_string()), find_date(&c[2]), place)
}

/// The text files of a show that may be its info file, the best for tagging first: the one
/// whose tracklist has as many titles as there are files, then the longest tracklist, then
/// the order of `ops::info_candidates`. With the number of titles of each. When the audio
/// is in an "AUDIO" folder, the info file is looked for above it.
pub fn info_files(show: &files::Show) -> Vec<(PathBuf, usize)> {
    let mut candidates = crate::ops::info_candidates(&show.root);
    let above = named_folder(show);
    if candidates.is_empty() && above != show.root {
        candidates = crate::ops::info_candidates(&above).into_iter().filter(|p| p.parent() == Some(above.as_path())).collect();
    }
    let n = show.files.len();
    let mut found: Vec<(PathBuf, usize)> = candidates
        .into_iter()
        .map(|p| {
            let titles = files::read_text(&p).map(|t| parse_info(&t, n, None).titles.len()).unwrap_or(0);
            (p, titles)
        })
        .collect();
    found.sort_by_key(|(_, t)| (*t != n, std::cmp::Reverse(*t)));
    found
}

/// Reads the info file of a show and prepares what will be written.
pub fn draft(show: &files::Show, info: &Path) -> Result<Draft, String> {
    let text = files::read_text(info).map_err(|e| format!("cannot read {}: {e}", file_name(info)))?;
    let existing = existing_tags(&show.files);
    let tag = |name: &str| existing.iter().find(|(k, _)| k == name).map(|(_, v)| v.trim().to_string()).filter(|v| !v.is_empty());
    let (folder_artist, folder_date, folder_place) = folder_fields(&file_name(&named_folder(show)));
    let hint = folder_artist.clone().or_else(|| tag("ARTIST"));
    let parsed = parse_info(&text, show.files.len(), hint.as_deref());
    // The artist of the info file, unless the folder name says otherwise (a first line
    // that is not the artist); its spelling from the folder name or the tags when they
    // agree ("BLACK SABBATH" -> "Black Sabbath", "Paradise Lost (UK)" -> "Paradise Lost").
    let info_artist = parsed.artist.trim().to_string();
    let artist = if let Some(same) = [folder_artist.clone(), tag("ARTIST")].into_iter().flatten().find(|a| same_name(a, &info_artist)) {
        same
    } else if let Some(folder) = folder_artist.filter(|_| !info_artist.is_empty()) {
        folder
    } else if !info_artist.is_empty() && info_artist.chars().count() <= 60 {
        info_artist
    } else {
        hint.unwrap_or_default()
    };
    let date = Some(parsed.date.clone())
        .filter(|d| !d.is_empty())
        .or_else(|| folder_date.clone())
        .or_else(|| tag("DATE").and_then(|d| find_date(&d)))
        .unwrap_or_default();
    let place = Some(parsed.place.clone()).filter(|p| !p.is_empty()).or(folder_place).unwrap_or_default();
    let album = format!("{date} {place}").trim().to_string();
    let tracks = number_tracks(&show.files, &parsed.titles);
    let extra = parsed.titles.iter().skip(show.files.len()).map(|t| t.1.clone()).collect();
    Ok(Draft { root: show.root.clone(), artist, date, place, album, found: parsed.titles.len(), extra, tracks, folder_date })
}

/// A file, the fields to write in it, its new name.
pub type FileJob = (PathBuf, Vec<(&'static str, String)>, Option<String>);

/// The finished plan of one show, sent to the background task.
#[derive(Clone, Debug)]
pub struct TagJob {
    pub root: PathBuf,
    pub files: Vec<FileJob>,
    /// Compute the .md5 and .sfv lines of the tagged files again.
    pub update_checksums: bool,
}

impl Draft {
    /// Digits of the numbers in "01 - Title" names.
    pub fn width(&self) -> usize {
        self.tracks.iter().map(|t| t.index).max().unwrap_or(1).to_string().len().max(2)
    }

    pub fn new_name(&self, t: &TrackPlan, scheme: NameScheme) -> Option<String> {
        new_name(scheme, t, self.artist.trim(), self.date.trim(), self.width()).filter(|n| *n != file_name(&t.path))
    }

    /// The plan to carry out: the tags unless `write_tags` is false (no fields then), the new
    /// names of `scheme` (none with `NameScheme::Keep`).
    pub fn job(&self, scheme: NameScheme, write_tags: bool, update_checksums: bool) -> TagJob {
        let discs: Vec<Option<u32>> = {
            let mut d: Vec<Option<u32>> = self.tracks.iter().map(|t| t.disc).collect();
            d.dedup();
            d
        };
        let several = discs.iter().flatten().count() > 1;
        let files = self
            .tracks
            .iter()
            .map(|t| {
                // Numbers as in the new names: within each disc for "d1t01", within each folder
                // otherwise (the same when the discs are folders).
                let by_disc = matches!(scheme, NameScheme::DiscTrackTitle | NameScheme::Etree);
                let (number, total) = if by_disc {
                    (t.track, self.tracks.iter().filter(|o| o.disc == t.disc).count())
                } else {
                    (t.index, self.tracks.iter().filter(|o| o.path.parent() == t.path.parent()).count())
                };
                let mut fields: Vec<(&'static str, String)> = vec![
                    ("ARTIST", self.artist.trim().to_string()),
                    ("ALBUM", self.album.trim().to_string()),
                    ("DATE", self.date.trim().to_string()),
                    ("TRACKNUMBER", number.to_string()),
                    ("TRACKTOTAL", total.to_string()),
                ];
                // An empty field would remove the tag: an empty title keeps the one there.
                if !t.title.trim().is_empty() {
                    fields.insert(0, ("TITLE", t.title.trim().to_string()));
                }
                fields.retain(|(_, v)| !v.is_empty());
                if several {
                    fields.push(("DISCNUMBER", t.disc.unwrap_or(1).to_string()));
                    fields.push(("DISCTOTAL", discs.len().to_string()));
                }
                if !write_tags {
                    fields.clear();
                }
                (t.path.clone(), fields, self.new_name(t, scheme))
            })
            .collect();
        TagJob { root: self.root.clone(), files, update_checksums }
    }

    /// Names given twice in a folder, or taken by a file that is not renamed: the problems
    /// that prevent renaming.
    pub fn name_conflicts(&self, scheme: NameScheme) -> Vec<String> {
        let mut problems = Vec::new();
        let mut targets: HashMap<(PathBuf, String), usize> = HashMap::new();
        for t in &self.tracks {
            let name = self.new_name(t, scheme).unwrap_or_else(|| file_name(&t.path));
            *targets.entry((t.path.parent().unwrap_or(Path::new("")).to_path_buf(), name.to_lowercase())).or_insert(0) += 1;
        }
        let sources: Vec<String> = self.tracks.iter().map(|t| t.path.to_string_lossy().to_lowercase()).collect();
        for ((dir, name), count) in &targets {
            let path = dir.join(name);
            if *count > 1 {
                problems.push(format!("\"{name}\" would be given to {count} files"));
            } else if !sources.contains(&path.to_string_lossy().to_lowercase()) && fs::read_dir(dir).is_ok_and(|entries| entries.flatten().any(|e| e.file_name().to_string_lossy().to_lowercase() == *name)) {
                problems.push(format!("\"{name}\" already exists"));
            }
        }
        problems.sort();
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(find_date("Jan. 19, 1974").as_deref(), Some("1974-01-19"));
        assert_eq!(find_date("Jan 23 & 24, 2026").as_deref(), Some("2026-01-23"));
        assert_eq!(find_date("1971-03-26 ").as_deref(), Some("1971-03-26"));
        assert_eq!(find_date("BS 19710326 Dallas").as_deref(), Some("1971-03-26"));
        assert_eq!(find_date("19th January 1974").as_deref(), Some("1974-01-19"));
        assert_eq!(find_date("19.01.1974").as_deref(), Some("1974-01-19"));
        assert_eq!(find_date("3/26/71").as_deref(), Some("1971-03-26"));
        assert_eq!(find_date("Montreal 15-1-91").as_deref(), Some("1991-01-15"));
        assert_eq!(find_date("FLAC (16/44.1)"), None);
        assert_eq!(find_date("CHEAP TRICK Paradiso, Amsterdam March 9. 1979 FM").as_deref(), Some("1979-03-09"));
        assert_eq!(find_date("Date: 5th September 1984").as_deref(), Some("1984-09-05"));
        assert_eq!(find_date("56:38 min"), None);
    }

    #[test]
    fn track_lines() {
        assert_eq!(track_line("01 - How Long"), Some((None, 1, "How Long".into())));
        assert_eq!(track_line("1. N.I.B. (06:37 min)"), Some((None, 1, "N.I.B.".into())));
        assert_eq!(track_line("08 - Sabbra Cadabra >"), Some((None, 8, "Sabbra Cadabra >".into())));
        assert_eq!(track_line("10 - banter*"), Some((None, 10, "banter".into())));
        assert_eq!(track_line("d2t03 Dark Star [12:45]"), Some((Some(2), 3, "Dark Star".into())));
        assert_eq!(track_line("4) Iron Man - 6:49"), Some((None, 4, "Iron Man".into())));
        assert_eq!(track_line("56:38 min"), None);
        assert_eq!(track_line("2nd gen cassette"), None);
        assert_eq!(track_line("16/44.1"), None);
        assert_eq!(track_line("26 January 2008"), None);
        assert_eq!(track_line("9. 22, Acacia Avenue (6:21)"), Some((None, 9, "22, Acacia Avenue".into())));
        assert_eq!(track_line("[01] [04:45] Intro/You Really Got Me"), Some((None, 1, "Intro/You Really Got Me".into())));
        assert_eq!(track_line("D1T01 - You Really Got Me.flac:d2e4bce6b1b55895a239fd453d97f8aa"), None);
        assert_eq!(track_line("11. -Band presentation- (1:51)"), Some((None, 11, "Band presentation".into())));
    }

    #[test]
    fn info_files() {
        let text = "BLACK SABBATH\nSindelfingen (near Stuttgart, GER)\nMessehalle\nJan. 19, 1974\n\n\nSome story.\n\nlineage: 2nd gen cassette > Zoom H2 > wav > FLAC\n\nSetlist (tt 51:08):\n\n01 - tuning\n02 - Tomorrows Dream\n03 - Sweet Leaf\n\n* Ozzy is pissed\n";
        let p = parse_info(text, 3, None);
        assert_eq!(p.artist, "BLACK SABBATH");
        assert_eq!(p.date, "1974-01-19");
        assert_eq!(p.place, "Sindelfingen (near Stuttgart, GER), Messehalle");
        assert_eq!(p.titles.iter().map(|t| t.1.as_str()).collect::<Vec<_>>(), ["tuning", "Tomorrows Dream", "Sweet Leaf"]);
        assert!(p.titles.iter().all(|t| t.0.is_none()));

        let text = "Black Sabbath\n1971-03-26 \nDallas State Fair Music Hall\nDallas, TX, USA\nAudience recording\nMikesline tape\n\nTracklist:\n1. N.I.B. (06:37 min)\n2. War Pigs (07:50 min)\n";
        let p = parse_info(text, 2, None);
        assert_eq!((p.date.as_str(), p.place.as_str()), ("1971-03-26", "Dallas State Fair Music Hall, Dallas, TX, USA"));

        // A header repeated after a description, more precise: it wins.
        let text = "Blue Rodeo\nLost Together: The 40th Anniversary Tour\nJan 23 & 24, 2026\nMassey Hall\n\nA long text.\n\nBlue Rodeo\nLost Together: The 40th Anniversary Tour\nJan 23 2026\nMassey Hall, Toronto, ON, Canada\n\n01 - How Long\n02 - Rose-Coloured Glasses\n";
        let p = parse_info(text, 2, None);
        assert_eq!((p.date.as_str(), p.place.as_str()), ("2026-01-23", "Massey Hall, Toronto, ON, Canada"));
    }

    #[test]
    fn discs_in_the_list() {
        let text = "Band\n1990-01-01\nPlace\n\nDisc 1\n01 - A\n02 - B\n\nDisc 2\n01 - C\n02 - D\n03 - E\n\nLineage:\n1. mics\n";
        let p = parse_info(text, 5, None);
        assert_eq!(p.titles, vec![(Some(1), "A".into()), (Some(1), "B".into()), (Some(2), "C".into()), (Some(2), "D".into()), (Some(2), "E".into())]);
        let text = "d1t01 A\nd1t02 B\nd2t01 C\n";
        assert_eq!(parse_info(text, 3, None).titles.iter().map(|t| t.0).collect::<Vec<_>>(), [Some(1), Some(1), Some(2)]);
        // Numbering on through the discs.
        let text = "CD1\n1. A\n2. B\nCD2\n3. C\n";
        assert_eq!(parse_info(text, 3, None).titles.len(), 3);
    }

    #[test]
    fn numbering_and_names() {
        let files: Vec<PathBuf> = ["/s/CD1/a.flac", "/s/CD1/b.flac", "/s/CD2/c.flac"].iter().map(PathBuf::from).collect();
        let titles = vec![(None, "A: one".to_string()), (None, "B >".to_string()), (None, "C?".to_string())];
        let plan = number_tracks(&files, &titles);
        assert_eq!(plan.iter().map(|t| (t.disc, t.track, t.index)).collect::<Vec<_>>(), [(Some(1), 1, 1), (Some(1), 2, 2), (Some(2), 1, 1)]);
        assert_eq!(new_name(NameScheme::NumberTitle, &plan[0], "X", "1990-01-01", 2).as_deref(), Some("01 - A- one.flac"));
        let nib = TrackPlan { title: "N.I.B.".into(), ..plan[0].clone() };
        assert_eq!(new_name(NameScheme::NumberTitle, &nib, "X", "", 2).as_deref(), Some("01 - N.I.B..flac"));
        assert_eq!(new_name(NameScheme::DiscTrackTitle, &plan[1], "X", "1990-01-01", 2).as_deref(), Some("d1t02 - B.flac"));
        assert_eq!(new_name(NameScheme::Etree, &plan[2], "X", "1990-01-01", 2).as_deref(), Some("X 1990-01-01 d2t01.flac"));
        assert_eq!(new_name(NameScheme::Keep, &plan[2], "X", "1990-01-01", 2), None);
    }

    #[test]
    fn checksum_lines() {
        let dir = std::env::temp_dir().join(format!("bt-tagsum-{}", std::process::id()));
        fs::create_dir_all(dir.join("CD1")).unwrap();
        let old = dir.join("CD1").join("01.flac");
        let new = dir.join("CD1").join("01 - A.flac");
        let changes = Changes {
            renamed: HashMap::from([(old.clone(), new.clone())]),
            digests: HashMap::from([(old.clone(), ("0123456789abcdef0123456789abcdef".to_string(), "89abcdef".to_string()))]),
        };
        let tagged = vec![old.clone()];
        let md5 = dir.join("s.md5");
        fs::write(&md5, "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF *CD1\\01.flac\r\nffffffffffffffffffffffffffffffff  other.flac\r\n").unwrap();
        assert_eq!(update_checksum_file(&md5, &changes, &tagged).unwrap(), (1, 0));
        assert_eq!(fs::read_to_string(&md5).unwrap(), "0123456789ABCDEF0123456789ABCDEF *CD1\\01 - A.flac\r\nffffffffffffffffffffffffffffffff  other.flac\r\n");
        let ffp = dir.join("s.ffp");
        fs::write(&ffp, "CD1/01.flac:ffffffffffffffffffffffffffffffff\n").unwrap();
        update_checksum_file(&ffp, &changes, &tagged).unwrap();
        assert_eq!(fs::read_to_string(&ffp).unwrap(), "CD1/01 - A.flac:ffffffffffffffffffffffffffffffff\n");
        let st5 = dir.join("s.st5");
        fs::write(&st5, "ffffffffffffffffffffffffffffffff  [shntool]  CD1/01.flac\n").unwrap();
        update_checksum_file(&st5, &changes, &tagged).unwrap();
        assert_eq!(fs::read_to_string(&st5).unwrap(), "ffffffffffffffffffffffffffffffff  [shntool]  CD1/01 - A.flac\n");
        let sfv = dir.join("s.sfv");
        fs::write(&sfv, "; comment\nCD1\\01.flac 01234567\n").unwrap();
        update_checksum_file(&sfv, &changes, &tagged).unwrap();
        assert_eq!(fs::read_to_string(&sfv).unwrap(), "; comment\nCD1\\01 - A.flac 89abcdef\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tags_only_or_names_only() {
        let files = vec![PathBuf::from("/s/a.flac"), PathBuf::from("/s/b.flac")];
        let titles = vec![(None, "One".to_string()), (None, "Two".to_string())];
        let draft = Draft {
            root: "/s".into(),
            artist: "Band".into(),
            date: "1974-01-19".into(),
            place: "Here".into(),
            album: "1974-01-19 Here".into(),
            tracks: number_tracks(&files, &titles),
            found: 2,
            extra: Vec::new(),
            folder_date: None,
        };
        let both = draft.job(NameScheme::NumberTitle, true, true);
        assert!(both.files.iter().all(|(_, fields, name)| !fields.is_empty() && name.is_some()));
        let names = draft.job(NameScheme::NumberTitle, false, true);
        assert!(names.files.iter().all(|(_, fields, _)| fields.is_empty()));
        assert_eq!(names.files[1].2.as_deref(), Some("02 - Two.flac"));
        let tags = draft.job(NameScheme::Keep, true, true);
        assert!(tags.files.iter().all(|(_, fields, name)| !fields.is_empty() && name.is_none()));
    }
}
