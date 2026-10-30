//! Setlists from setlist.fm, only when the user asks for them and with their own API key
//! (Settings). One search per show (artist and date): the venue, the tour and the songs,
//! written to a text file in the show folder or used as titles by Tag and rename.
//! https://api.setlist.fm/docs/1.0/index.html

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::tagging::same_name;

/// The last line of a setlist file, which tells it from the show's own info file.
const FOOTER: &str = "Setlist from setlist.fm";

/// Why a search failed; `fatal` when it holds for every show (key refused, daily limit,
/// stopped), so that a batch stops asking.
#[derive(Debug)]
pub struct Failure {
    pub text: String,
    pub fatal: bool,
}

impl Failure {
    fn show(text: impl Into<String>) -> Failure {
        Failure { text: text.into(), fatal: false }
    }
    fn all(text: impl Into<String>) -> Failure {
        Failure { text: text.into(), fatal: true }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

const SEARCH: &str = "https://api.setlist.fm/rest/1.0/search/setlists";
/// setlist.fm allows 2 requests per second: one per second here.
const DELAY: Duration = Duration::from_secs(1);
/// Retries after "too many requests", server and network errors (waits 2, 4, 8 s).
const RETRIES: u32 = 3;
/// Pages of 20 setlists read at most per search (other artists of the same name...).
const MAX_PAGES: u32 = 5;

/// A show on setlist.fm.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Show {
    pub artist: String,
    /// YYYY-MM-DD.
    pub date: String,
    pub venue: String,
    pub city: String,
    pub country: String,
    pub tour: String,
    pub songs: Vec<String>,
    pub url: String,
}

impl Show {
    /// "Venue, City, Country" (`file_name` makes it safe for a file name).
    pub fn location(&self) -> String {
        let parts: Vec<String> = [&self.venue, &self.city, &self.country]
            .into_iter()
            .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|p| !p.is_empty())
            .collect();
        parts.join(", ").trim_matches(['.', ' ']).to_string()
    }

    /// "Artist - YYYY-MM-DD - Venue, City, Country.txt", within the 255 bytes file systems
    /// allow for a name.
    pub fn file_name(&self) -> String {
        let safe = crate::tagging::safe_name;
        let mut stem = format!("{} - {} - {}", safe(&self.artist), safe(&self.date), safe(&self.location()));
        while stem.len() > 240 {
            stem.pop();
        }
        format!("{}.txt", stem.trim_end_matches([' ', ',', '-', '.']))
    }

    /// The text file: the show, the tour, the numbered songs and where they come from.
    pub fn text(&self) -> String {
        let mut lines = vec![format!("{} - {} - {}", self.artist, self.date, self.location())];
        if !self.tour.is_empty() {
            lines.push(format!("Tour: {}", self.tour));
        }
        lines.push(String::new());
        let width = if self.songs.len() >= 100 { 3 } else { 2 };
        lines.extend(self.songs.iter().enumerate().map(|(i, s)| format!("{:0width$}. {s}", i + 1)));
        lines.push(String::new());
        lines.push(if self.url.is_empty() { FOOTER.to_string() } else { format!("{FOOTER}: {}", self.url) });
        lines.join("\n") + "\n"
    }
}

// The parts of setlist.fm's answer that are used.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Page {
    setlist: Vec<Setlist>,
    total: usize,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Setlist {
    artist: Named,
    venue: Venue,
    tour: Named,
    sets: Sets,
    event_date: String,
    url: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Named {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Venue {
    name: String,
    city: City,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct City {
    name: String,
    country: Named,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Sets {
    set: Vec<Set>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Set {
    song: Vec<Named>,
}

impl Setlist {
    fn show(self) -> Show {
        Show {
            artist: self.artist.name.trim().to_string(),
            // "dd-MM-yyyy" -> "yyyy-MM-dd"
            date: crate::tagging::find_date(&self.event_date).unwrap_or(self.event_date),
            venue: self.venue.name.trim().to_string(),
            city: self.venue.city.name.trim().to_string(),
            country: self.venue.city.country.name.trim().to_string(),
            tour: self.tour.name.trim().to_string(),
            songs: self.sets.set.into_iter().flat_map(|s| s.song).map(|s| s.name.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            url: self.url,
        }
    }
}

/// The shows of that artist on that day (YYYY-MM-DD), each venue once (the most complete
/// setlist when setlist.fm has the show twice). setlist.fm's search also returns other
/// artists whose name contains the one asked for: only the same name is kept.
pub fn find(key: &str, artist: &str, date: &str, cancel: &AtomicBool) -> Result<Vec<Show>, Failure> {
    let all = search(key, artist, date, cancel)?;
    let mut shows: Vec<Show> = Vec::new();
    let wanted = simple(artist);
    for s in all.into_iter().filter(|s| same_name(&s.artist, artist) || simple(&s.artist) == wanted) {
        // The same show entered twice (one setlist the start of the other) counts once, the
        // more complete; an early and a late show at one venue stay two.
        let twice = |o: &Show| o.location() == s.location() && (o.songs.starts_with(&s.songs) || s.songs.starts_with(&o.songs));
        match shows.iter_mut().find(|o| twice(o)) {
            Some(o) if s.songs.len() > o.songs.len() => *o = s,
            Some(_) => {}
            None => shows.push(s),
        }
    }
    Ok(shows)
}

/// Letters and digits only, without accents: "AC/DC" = "ACDC", "Mötley Crüe" = "Motley Crue".
fn simple(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'ç' => 'c',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ñ' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ý' | 'ÿ' => 'y',
            c => c,
        })
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// Every setlist of that day whose artist matches the name, as setlist.fm gives them.
/// The answers are kept for the session: asking again costs no request.
fn search(key: &str, artist: &str, date: &str, cancel: &AtomicBool) -> Result<Vec<Show>, Failure> {
    let (key, artist, date) = (key.trim(), artist.trim(), date.trim());
    if key.is_empty() {
        return Err(Failure::all("no setlist.fm API key (Settings > setlist.fm)"));
    }
    if artist.is_empty() {
        return Err(Failure::show("no artist name (folder name, tags or info file)"));
    }
    if !crate::tagging::full_date(date) {
        return Err(Failure::show(format!("no full date (YYYY-MM-DD) for the show: \"{date}\"")));
    }
    let cache_key = (artist.to_lowercase(), date.to_string());
    type Answers = HashMap<(String, String), Vec<Show>>;
    static CACHE: OnceLock<Mutex<Answers>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(shows) = cache.lock().ok().and_then(|c| c.get(&cache_key).cloned()) {
        return Ok(shows);
    }
    // setlist.fm wants "dd-MM-yyyy".
    let day = date.split('-').rev().collect::<Vec<_>>().join("-");
    let mut shows = Vec::new();
    for number in 1..=MAX_PAGES {
        let Some(page) = request(key, artist, &day, number, cancel)? else { break };
        let empty = page.setlist.is_empty();
        shows.extend(page.setlist.into_iter().map(Setlist::show));
        if empty || shows.len() >= page.total {
            break;
        }
    }
    if let Ok(mut c) = cache.lock() {
        c.insert(cache_key, shows.clone());
    }
    Ok(shows)
}

/// One page of results (None: nothing found), retried after "too many requests", server
/// and network errors.
fn request(key: &str, artist: &str, day: &str, page: u32, cancel: &AtomicBool) -> Result<Option<Page>, Failure> {
    // One agent for the session (its connection is kept); no redirects, so the API key is
    // never sent anywhere but to setlist.fm.
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| crate::update::agent(Duration::from_secs(20)));
    let stopped = || Failure::all("stopped");
    // BT_SETLISTFM: another address, for tests.
    let url = std::env::var("BT_SETLISTFM").unwrap_or_else(|_| SEARCH.to_string());
    let mut error = String::new();
    for attempt in 0..=RETRIES {
        if cancel.load(Ordering::Relaxed) {
            return Err(stopped());
        }
        pace();
        let mut wait = Duration::from_secs(2 << attempt);
        let answer = agent
            .get(&url)
            .query("artistName", artist)
            .query("date", day)
            .query("p", page.to_string())
            .header("x-api-key", key)
            .header("Accept", "application/json")
            .header("Accept-Language", "en")
            .call();
        match answer {
            Ok(mut response) => match response.status().as_u16() {
                200 => {
                    let text = response.body_mut().read_to_string().map_err(|e| Failure::show(format!("cannot read setlist.fm's answer ({e})")))?;
                    return serde_json::from_str(&text).map(Some).map_err(|e| Failure::show(format!("unexpected answer from setlist.fm ({e})")));
                }
                404 => return Ok(None),
                401 | 403 => return Err(Failure::all("setlist.fm refused the API key (check it in Settings > setlist.fm)")),
                429 => {
                    let after: Option<u64> = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
                    match after {
                        Some(s) if s > 60 => return Err(Failure::all("setlist.fm's daily limit for this API key is reached: try again tomorrow")),
                        Some(s) => wait = Duration::from_secs(s + 1),
                        None => {}
                    }
                    error = "setlist.fm says too many requests".into();
                }
                code @ 500.. => error = format!("setlist.fm server error {code}"),
                code => return Err(Failure::show(format!("setlist.fm answered {code}"))),
            },
            Err(e) => error = format!("cannot reach setlist.fm ({e})"),
        }
        if attempt < RETRIES {
            let until = Instant::now() + wait;
            while Instant::now() < until {
                if cancel.load(Ordering::Relaxed) {
                    return Err(stopped());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    Err(Failure::show(error))
}

/// One request per second at most, whatever asks (several windows, a batch).
fn pace() {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let Ok(mut last) = LAST.lock() else { return };
    if let Some(t) = *last {
        let next = t + DELAY;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
    }
    *last = Some(Instant::now());
}

/// Several shows that day: the one whose venue appears in the place already known (folder
/// name or info file), if exactly one does.
pub fn pick_by_place<'a>(place: &str, shows: &'a [Show]) -> Option<&'a Show> {
    let place = simple(place);
    if place.is_empty() {
        return None;
    }
    let hits: Vec<&Show> = shows
        .iter()
        .filter(|s| {
            let venue = simple(&s.venue);
            !venue.is_empty() && place.contains(&venue)
        })
        .collect();
    if hits.len() == 1 { Some(hits[0]) } else { None }
}

/// The text of a file written by Get setlist (its last line names setlist.fm).
pub fn is_setlist_text(text: &str) -> bool {
    text.lines().rev().find(|l| !l.trim().is_empty()).is_some_and(|l| l.trim_start().starts_with(FOOTER))
}

/// A file written by Get setlist.
pub fn is_setlist_file(path: &Path) -> bool {
    crate::files::ext_lower(path) == "txt"
        && std::fs::metadata(path).is_ok_and(|m| m.len() < 64 * 1024)
        && crate::files::read_text(path).is_ok_and(|t| is_setlist_text(&t))
}

/// A setlist file of that show already in the folder ("Artist - YYYY-MM-DD - ....txt",
/// the artist written in any way: "Guns N' Roses" = "Guns N’ Roses").
pub fn existing_file(folder: &Path, artist: &str, date: &str) -> Option<PathBuf> {
    let (artist, middle) = (simple(artist), format!(" - {} - ", date.trim()));
    std::fs::read_dir(folder).ok()?.flatten().map(|e| e.path()).find(|p| {
        let name = crate::files::file_name(p);
        p.is_file() && crate::files::ext_lower(p) == "txt" && name.find(&middle).is_some_and(|i| simple(&name[..i]) == artist)
    })
}

/// Writes the setlist file in the folder: its path, or None when a setlist file of that
/// show is there already (kept as it is).
pub fn write(folder: &Path, show: &Show) -> Result<Option<PathBuf>, String> {
    if existing_file(folder, &show.artist, &show.date).is_some() {
        return Ok(None);
    }
    let (name, mut text) = (show.file_name(), show.text());
    if cfg!(windows) {
        text = text.replace('\n', "\r\n");
    }
    let path = folder.join(&name);
    std::fs::write(&path, text).map_err(|e| format!("cannot write {name}: {e}"))?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = r#"{"type":"setlists","itemsPerPage":20,"page":1,"total":2,"setlist":[
        {"id":"1","eventDate":"30-08-1984","artist":{"name":"Honeymoon Suite"},
         "venue":{"name":"The Decade","city":{"name":"Pittsburgh","stateCode":"PA","country":{"code":"US","name":"United States"}}},
         "tour":{"name":"First Tour"},
         "sets":{"set":[{"song":[{"name":"New Girl Now"},{"name":""},{"name":"Burning in Love"}]},{"encore":1,"song":[{"name":"Wave Babies","tape":true}]}]},
         "url":"https://www.setlist.fm/setlist/x.html"},
        {"id":"2","eventDate":"30-08-1984","artist":{"name":"Honeymoon Suite Tribute"},"venue":{"name":"Elsewhere","city":{"name":"Paris","country":{"name":"France"}}},"sets":{"set":[]}}]}"#;

    #[test]
    fn reads_setlist_fm() {
        let page: Page = serde_json::from_str(ANSWER).unwrap();
        let shows: Vec<Show> = page.setlist.into_iter().map(Setlist::show).collect();
        let s = &shows[0];
        assert_eq!(s.date, "1984-08-30");
        assert_eq!(s.location(), "The Decade, Pittsburgh, United States");
        assert_eq!(s.songs, ["New Girl Now", "Burning in Love", "Wave Babies"]);
        assert_eq!(s.file_name(), "Honeymoon Suite - 1984-08-30 - The Decade, Pittsburgh, United States.txt");
        assert_eq!(
            s.text(),
            "Honeymoon Suite - 1984-08-30 - The Decade, Pittsburgh, United States\nTour: First Tour\n\n01. New Girl Now\n02. Burning in Love\n03. Wave Babies\n\nSetlist from setlist.fm: https://www.setlist.fm/setlist/x.html\n"
        );
        assert!(shows[1].songs.is_empty() && shows[1].tour.is_empty());
    }

    #[test]
    fn names_and_places() {
        assert_eq!(simple("Mötley Crüe"), "motleycrue");
        assert_eq!(simple("AC/DC"), "acdc");
        let show = |venue: &str| Show { venue: venue.into(), ..Default::default() };
        let shows = [show("Movistar Arena"), show("Estadio Nacional")];
        assert_eq!(pick_by_place("Movistar Arena, Santiago, Chile", &shows), Some(&shows[0]));
        assert_eq!(pick_by_place("Santiago, Chile", &shows), None);
        let odd = Show { artist: "A/B".into(), date: "2000-01-02".into(), venue: "Club: \"X\"".into(), ..Default::default() };
        assert_eq!(odd.file_name(), "A-B - 2000-01-02 - Club- X.txt");
        let long = Show { artist: "Ä".repeat(150), date: "2000-01-02".into(), venue: "V".repeat(150), ..Default::default() };
        assert!(long.file_name().len() <= 244);
    }

    #[test]
    fn finds_the_file_already_there() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Guns N’ Roses - 1991-05-29 - Deer Creek Music Center, Noblesville, United States.txt"), "x").unwrap();
        assert!(existing_file(tmp.path(), "Guns N' Roses", "1991-05-29").is_some());
        assert!(existing_file(tmp.path(), "Guns N' Roses", "1991-05-30").is_none());
        assert!(existing_file(tmp.path(), "Roses", "1991-05-29").is_none());
        let show = Show { artist: "A".into(), date: "2000-01-02".into(), venue: "V".into(), songs: vec!["S".into()], ..Default::default() };
        let path = write(tmp.path(), &show).unwrap().unwrap();
        assert!(is_setlist_file(&path));
        assert!(!is_setlist_file(&tmp.path().join("Guns N’ Roses - 1991-05-29 - Deer Creek Music Center, Noblesville, United States.txt")));
    }
}
