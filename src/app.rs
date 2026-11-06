//! The window: show list, toolbar, log, settings.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, Modifiers, RichText};
use egui_extras::{Column, TableBuilder};
use winit::raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
};

use crate::audio::{fmt_total, AudioInfo};
use crate::files::{file_name, rel_posix, scan_paths, Show};
use crate::encode::Mp3Mode;
use crate::ops::{self, Level, Msg, Op, Options, Target, ThemeChoice, Tracker, APP_NAME, APP_VERSION};

const AUTO_DETAILS_LIMIT: usize = 3000;

/// The main components built into the program (About window); THIRD-PARTY.md lists them all.
const COMPONENTS: &[(&str, &str, &str)] = &[
    ("egui, eframe, egui_extras", "user interface", "MIT or Apache-2.0"),
    ("winit", "windows and input", "Apache-2.0"),
    ("Symphonia", "FLAC, MP3, AAC, ALAC and Vorbis decoding; M4A, MP4, MKA, MKV, AIFF and OGG files", "MPL-2.0"),
    ("flacenc", "FLAC encoding", "Apache-2.0"),
    ("ape-decoder", "Monkey's Audio (APE) decoding", "MIT or Apache-2.0"),
    ("wavicle", "WavPack decoding", "MIT or Apache-2.0"),
    ("LAME 3.100", "MP3 encoding (lame.sourceforge.io)", "LGPL"),
    ("mp3lame-encoder", "link to LAME", "LGPL-3.0"),
    ("rayon", "parallel processing", "MIT or Apache-2.0"),
    ("md-5, sha1", "checksums, torrents", "MIT or Apache-2.0"),
    ("regex", "file name patterns", "MIT or Apache-2.0"),
    ("rfd", "file and folder dialogs", "MIT"),
    ("trash", "moving files to the trash", "MIT"),
    ("open", "opening folders", "MIT"),
    ("arboard", "clipboard (images)", "MIT or Apache-2.0"),
    ("ab_glyph, png", "spectrogram text and images", "Apache-2.0 / MIT or Apache-2.0"),
    ("serde", "settings", "MIT or Apache-2.0"),
    ("zbus", "desktop services (Linux)", "MIT"),
    ("ureq, rustls", "update check (HTTPS)", "MIT or Apache-2.0 / Apache-2.0, ISC or MIT"),
    ("ring, rustls-webpki, rustls-platform-verifier", "encryption, certificates checked by the system", "Apache-2.0 and ISC / ISC / MIT or Apache-2.0"),
    ("Noto Sans, Noto Sans Mono, Noto Emoji, Caveat", "interface fonts", "OFL-1.1"),
    ("emoji-icon-font", "interface symbols (from egui)", "MIT"),
    ("Shorten decoder", "SHN decoding (written for Bootleg Toolbox)", "GPL-3.0"),
];

const DVD_TIP: &str = "Add a DVD folder (VIDEO_TS, or the folder above it) to the list: one row per title, with its audio tracks and whether one is lossless (LPCM). Media info (Ctrl+I) then gives its technical summary: video standard, resolution, aspect ratio, bit rates, audio tracks, subtitles, titles and chapters. Dropping a DVD folder on the window does the same.";

/// The result of an update check: (asked from the Help menu, newer version or error).
type UpdateCheck = (bool, Result<Option<String>, String>);

const INFO_TIP: &str = "Copy the show's info file (.txt or .nfo, whatever its name) to the clipboard, for the upload forms. With several text files, a window lets you pick the right one.";

/// The text files that may be a show's info file, the most likely first.
struct InfoChoice {
    root: PathBuf,
    candidates: Vec<PathBuf>,
    texts: Vec<String>,
    chosen: usize,
}

const TAG_TIP: &str = "Read the artist, date, place and titles from the show's info file, check and correct them in a window, \
                       then write them as tags in the FLAC files and, if you like, rename the files. The audio is not touched: \
                       the FFP and ST5 stay valid; the .md5 and .sfv are brought up to date.";

const SETLIST_TIP: &str = "Get the setlist of each show from setlist.fm (artist and date from the info file, the folder name or the tags) \
                           and write it in its folder as \"Artist - YYYY-MM-DD - Venue, City, Country.txt\": tour and numbered songs, \
                           ready for Tag and rename. Needs your own setlist.fm API key (Settings).";

const EXTRACT_TIP: &str = "Extract one audio track of each selected DVD title or video (MKV, MP4) to a single file: \
                           FLAC for a lossless track (LPCM, PCM, FLAC, ALAC: bit for bit, checked), the original stream (.ac3, .dts, .aac...) \
                           for a lossy one, unchanged. A DVD title's track is gathered from all its VOB files into the DVD's folder; \
                           a video's goes beside it. A window lets you pick the track when there are several.";

const SHARE_TIP: &str = "Make a picture (PNG, 1200 × 630) of each selected show to post with it: the show, its format and \
     source, the checks run on it in this session (Lossless?, Speed?, checksums or decode test, SBE) and a spectrogram of its longest \
     track. Saved beside the show folder.";
const UNDO_TIP: &str = "Give shows changed by Tag and rename back their former tags, names and checksum files.";

/// "Tag and rename from info file": what will be written, open for corrections.
#[derive(Clone)]
struct TagDialog {
    show: Show,
    /// The possible info files and their number of titles, the best first.
    infos: Vec<(PathBuf, usize)>,
    info: usize,
    draft: crate::tagging::Draft,
    /// The album follows the date and the place until the user edits it.
    album_auto: bool,
    update_checksums: bool,
    /// The show has .md5 or .sfv files.
    whole_checksums: bool,
    /// For this show only (the defaults are in Settings).
    write_tags: bool,
    rename: bool,
    /// Opened from the batch window to check this row: OK gives the plan back to it.
    batch_row: Option<usize>,
    /// setlist.fm: the search under way, the shows to choose from, what went wrong.
    setlist_search: Option<SetlistSearch>,
    /// The artist and date asked for, and the flag that stops the search.
    setlist_asked: (String, String),
    setlist_cancel: Arc<AtomicBool>,
    setlist_shows: Vec<crate::setlistfm::Show>,
    setlist_note: Option<(String, Level)>,
}

/// Tag and rename of several shows: each plan prepared, the clear ones ticked, the others
/// to check one by one in the Tag and rename window before they are ticked.
struct TagBatch {
    rows: Vec<BatchRow>,
}

struct BatchRow {
    show: Show,
    /// None when the show cannot be tagged (`problem` says why).
    plan: Option<TagDialog>,
    problem: String,
    /// What to check (empty: clear); emptied once checked in the window.
    doubts: Vec<String>,
    checked: bool,
    ticked: bool,
    applied: bool,
}

impl TagDialog {
    /// The plan of a show from its best info file, with the default choices of Settings.
    /// Without info file, `setlist`: a plan from the folder name and the tags, whose titles
    /// setlist.fm can give.
    fn prepare(show: &Show, opt: &Options, setlist: bool) -> Result<TagDialog, String> {
        let infos = crate::tagging::info_files(show);
        let draft = match infos.first() {
            Some((info, _)) => crate::tagging::draft(show, info)?,
            None if setlist => crate::tagging::draft_without_info(show),
            None => return Err("no info file (.txt or .nfo; Get setlist from setlist.fm can write one)".into()),
        };
        let whole_checksums = crate::tagging::checksum_files(&show.root).iter().any(|p| matches!(crate::files::ext_lower(p).as_str(), "md5" | "sfv"));
        Ok(TagDialog {
            show: show.clone(),
            infos,
            info: 0,
            draft,
            album_auto: true,
            update_checksums: true,
            whole_checksums,
            write_tags: opt.tag_write,
            rename: opt.tag_rename,
            batch_row: None,
            setlist_search: None,
            setlist_asked: Default::default(),
            setlist_cancel: Default::default(),
            setlist_shows: Vec::new(),
            setlist_note: None,
        })
    }

    /// The songs of a setlist.fm show as titles, its venue as the place.
    fn use_setlist(&mut self, show: &crate::setlistfm::Show) {
        if show.songs.is_empty() {
            self.setlist_note = Some((format!("setlist.fm has the show ({}), but not its songs.", show.location()), Level::Warn));
            return;
        }
        self.draft.use_setlist(&show.songs, &show.location());
        if self.album_auto {
            self.draft.album = format!("{} {}", self.draft.date.trim(), self.draft.place.trim()).trim().to_string();
        }
        self.setlist_shows.clear();
        self.setlist_note = Some((format!("{} songs from setlist.fm: {}.", show.songs.len(), show.location()), Level::Ok));
    }

    fn names(&self, scheme: crate::tagging::NameScheme) -> crate::tagging::NameScheme {
        if self.rename { scheme } else { crate::tagging::NameScheme::Keep }
    }

    /// What prevents writing the plan with these file names, if anything.
    fn blocked(&self, scheme: crate::tagging::NameScheme) -> Option<String> {
        if !self.write_tags && !self.rename {
            return Some("nothing ticked".into());
        }
        if let Some(c) = self.draft.name_conflicts(self.names(scheme)).first() {
            return Some(format!("cannot rename: {c}"));
        }
        if !self.write_tags && self.draft.tracks.iter().all(|t| self.draft.new_name(t, scheme).is_none()) {
            return Some("nothing to change: the files already have these names".into());
        }
        None
    }

    fn job(&self, scheme: crate::tagging::NameScheme) -> crate::tagging::TagJob {
        self.draft.job(self.names(scheme), self.write_tags, self.update_checksums || !self.whole_checksums)
    }
}

/// The answer of a setlist.fm search under way (shared: the plans are cloned).
type SetlistSearch = Arc<Mutex<Receiver<Result<Vec<crate::setlistfm::Show>, String>>>>;

/// "Extract audio": the DVD titles and videos of the selection, with their audio tracks
/// (summary, what the extraction writes) and the one picked.
struct ExtractDialog {
    titles: Vec<ExtractTitle>,
}

struct ExtractTitle {
    /// A DVD title's first VOB, or a video file.
    source: PathBuf,
    /// Each track's summary, and what it becomes ("FLAC", "original stream"; None: not possible).
    tracks: Vec<(String, Option<&'static str>)>,
    pick: usize,
}

/// setlist.fm has several shows of the artist that day: the one to write, or none.
struct SetlistChoice {
    root: PathBuf,
    shows: Vec<crate::setlistfm::Show>,
    pick: Option<usize>,
}

/// "Undo tagging": the runs of the undo journal, the shows ticked to be rolled back.
struct UndoDialog {
    runs: Vec<(PathBuf, crate::undo::Batch, u64)>,
    ticked: std::collections::HashSet<(usize, usize)>,
    /// "Roll back" was clicked once: the second click confirms.
    confirm: bool,
}

/// Linux: whether the former name (Bootleg Helper) had an applications menu entry.
#[cfg(target_os = "linux")]
fn legacy_menu_entry() -> bool {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .is_some_and(|d| d.join("applications/bootleg-helper.desktop").is_file())
}

/// Linux: a menu entry (~/.local/share/applications) and the icon (hicolor theme) for this
/// executable, for the current user. Without it, the desktop shows a generic icon.
#[cfg(target_os = "linux")]
fn install_desktop_entry() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or("no home folder")?;
    let icon_dir = data.join("icons/hicolor/256x256/apps");
    let apps_dir = data.join("applications");
    for dir in [&icon_dir, &apps_dir] {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(icon_dir.join("bootleg-toolbox.png"), include_bytes!("../assets/icon.png")).map_err(|e| e.to_string())?;
    // Exec: the path quoted, with the characters the specification asks to escape.
    let path = exe.to_string_lossy();
    let mut quoted = String::from("\"");
    for c in path.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        if c == '%' {
            quoted.push('%');
        }
        quoted.push(c);
    }
    quoted.push('"');
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName={APP_NAME}\nGenericName=Lossless concert recordings helper\n\
         Comment=Checksums, decode tests, torrents, conversions and SBE fixing for lossless concert recordings\n\
         Exec={} %F\nIcon=bootleg-toolbox\nTerminal=false\nStartupWMClass=bootleg-toolbox\n\
         Categories=AudioVideo;Audio;Utility;\nKeywords=flac;ffp;md5;st5;shn;ape;bootleg;checksum;torrent;sbe;\n",
        quoted.replace('\\', "\\\\")
    );
    let desktop = apps_dir.join("bootleg-toolbox.desktop");
    std::fs::write(&desktop, entry).map_err(|e| e.to_string())?;
    // Refresh the menus where the tools exist; harmless when they do not.
    for cmd in [vec!["update-desktop-database".to_string(), apps_dir.display().to_string()], vec!["kbuildsycoca6".into()], vec!["kbuildsycoca5".into()]] {
        let _ = std::process::Command::new(&cmd[0])
            .args(&cmd[1..])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    Ok(format!("Added to the applications menu ({}), for {}", desktop.display(), exe.display()))
}

/// The window icon (the cassette), decoded once from the PNG built into the program.
fn app_icon() -> std::sync::Arc<egui::IconData> {
    static ICON: std::sync::OnceLock<std::sync::Arc<egui::IconData>> = std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let decoded = png::Decoder::new(std::io::Cursor::new(include_bytes!("../assets/icon.png").as_slice()))
            .read_info()
            .and_then(|mut reader| {
                let mut rgba = vec![0; reader.output_buffer_size().unwrap_or(0)];
                let frame = reader.next_frame(&mut rgba)?;
                rgba.truncate(frame.buffer_size());
                Ok(egui::IconData { rgba, width: frame.width, height: frame.height })
            });
        std::sync::Arc::new(decoded.unwrap_or_default())
    })
    .clone()
}

/// Settings saved under the program's former name (Bootleg Helper) are taken over once.
fn migrate_settings() {
    let Some(new) = eframe::storage_dir("bootleg-toolbox").map(|d| d.join("app.ron")) else { return };
    // Older versions named the folder after the window title ("Bootleg Helper").
    let old = ["bootleg-helper", "Bootleg Helper"].into_iter().filter_map(eframe::storage_dir).map(|d| d.join("app.ron")).find(|p| p.is_file());
    if let (Some(old), false) = (old, new.exists()) {
        if let Some(dir) = new.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::copy(&old, &new);
    }
}

pub fn run(paths: Vec<PathBuf>) -> eframe::Result<()> {
    migrate_settings();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([1000.0, 465.0])
            .with_min_inner_size([680.0, 320.0])
            .with_drag_and_drop(true)
            .with_app_id("bootleg-toolbox")
            .with_icon(app_icon()),
        // With vsync, the NVIDIA driver under XWayland blocks the frame of a hidden window
        // (show desktop, minimised) until it shows again, which freezes the interface.
        vsync: false,
        ..Default::default()
    };
    // winit has no drag and drop on Wayland: go through XWayland when it is there.
    // BT_WAYLAND=1 keeps the native Wayland window (no drag and drop).
    #[cfg(target_os = "linux")]
    let options = if std::env::var_os("DISPLAY").is_some() && std::env::var_os("BT_WAYLAND").is_none() {
        eframe::NativeOptions {
            event_loop_builder: Some(Box::new(|builder| {
                use winit::platform::x11::EventLoopBuilderExtX11;
                builder.with_x11();
            })),
            ..options
        }
    } else {
        options
    };
    eframe::run_native(APP_NAME, options, Box::new(move |cc| Ok(Box::new(App::new(cc, paths)))))
}

/// The light theme needs darker greens and oranges to stay readable on white.
static DARK_THEME: AtomicBool = AtomicBool::new(false);
/// The accent chosen in the Settings (`Accent` as a number).
static ACCENT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Light/dark preference of the desktop (KDE, GNOME...) through the XDG portal;
/// None when there is no portal or no preference. Waits at most one second.
#[cfg(target_os = "linux")]
fn desktop_prefers_dark() -> Option<bool> {
    fn read() -> zbus::Result<u32> {
        let connection = zbus::blocking::Connection::session()?;
        let reply = connection.call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.Settings"),
            "ReadOne",
            &("org.freedesktop.appearance", "color-scheme"),
        )?;
        let value: zbus::zvariant::OwnedValue = reply.body().deserialize()?;
        Ok(u32::try_from(value)?)
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read().ok());
    });
    // 1 = prefer dark, 2 = prefer light, 0 = no preference
    match rx.recv_timeout(Duration::from_secs(1)).ok().flatten() {
        Some(1) => Some(true),
        Some(2) => Some(false),
        _ => None,
    }
}

/// The colours of the interface, light and dark: warm neutrals and an amber accent.
#[derive(Clone, Copy)]
struct Palette {
    /// Buttons that stand out, icons, links.
    accent: Color32,
    /// Hover and selection.
    tint: Color32,
    tint_strong: Color32,
    /// Menu bar, toolbar and status bar.
    chrome: Color32,
    /// The list and the windows.
    ground: Color32,
    /// The rows of the shows.
    show_row: Color32,
    border: Color32,
    button: Color32,
    button_border: Color32,
    weak: Color32,
}

impl Palette {
    fn of(dark: bool) -> Palette {
        let rgb = Color32::from_rgb;
        // The accent and its pale tints, light and dark.
        let accent = ops::Accent::ALL.get(ACCENT.load(Ordering::Relaxed) as usize).copied().unwrap_or_default();
        let (accent, tint, tint_strong) = match (accent, dark) {
            (ops::Accent::Amber, false) => (rgb(168, 86, 12), rgb(251, 236, 217), rgb(243, 217, 184)),
            (ops::Accent::Amber, true) => (rgb(232, 161, 74), rgb(74, 53, 36), rgb(96, 68, 42)),
            (ops::Accent::Teal, false) => (rgb(31, 111, 102), rgb(226, 242, 239), rgb(196, 228, 222)),
            (ops::Accent::Teal, true) => (rgb(94, 200, 184), rgb(30, 58, 54), rgb(40, 78, 72)),
            (ops::Accent::Indigo, false) => (rgb(63, 74, 168), rgb(232, 234, 250), rgb(208, 213, 243)),
            (ops::Accent::Indigo, true) => (rgb(150, 160, 240), rgb(40, 44, 78), rgb(54, 60, 104)),
            (ops::Accent::Burgundy, false) => (rgb(140, 47, 74), rgb(248, 232, 237), rgb(238, 206, 216)),
            (ops::Accent::Burgundy, true) => (rgb(232, 130, 160), rgb(66, 34, 44), rgb(88, 44, 58)),
        };
        if dark {
            Palette {
                accent,
                tint,
                tint_strong,
                chrome: rgb(38, 36, 31),
                ground: rgb(28, 27, 24),
                show_row: rgb(46, 42, 36),
                border: rgb(58, 54, 47),
                button: rgb(47, 44, 39),
                button_border: rgb(71, 66, 58),
                weak: rgb(163, 157, 146),
            }
        } else {
            Palette {
                accent,
                tint,
                tint_strong,
                chrome: rgb(247, 245, 241),
                ground: Color32::WHITE,
                show_row: rgb(246, 241, 233),
                border: rgb(227, 221, 210),
                button: Color32::WHITE,
                button_border: rgb(221, 214, 202),
                weak: rgb(107, 101, 92),
            }
        }
    }

    fn now() -> Palette {
        Palette::of(DARK_THEME.load(Ordering::Relaxed))
    }

    /// A verdict's badge: (fill, text).
    fn badge(self, level: Level) -> (Color32, Color32) {
        let rgb = Color32::from_rgb;
        let dark = self.ground != Color32::WHITE;
        match (level, dark) {
            (Level::Ok, false) => (rgb(231, 244, 236), rgb(23, 99, 59)),
            (Level::Ok, true) => (rgb(31, 58, 41), rgb(127, 208, 155)),
            (Level::Warn, false) => (rgb(255, 243, 191), rgb(110, 84, 0)),
            (Level::Warn, true) => (rgb(64, 54, 14), rgb(244, 210, 100)),
            (Level::Error, false) => (rgb(253, 236, 235), rgb(161, 37, 28)),
            (Level::Error, true) => (rgb(67, 32, 29), rgb(242, 139, 130)),
            // Inconclusive verdicts ("Unclear"): blue, apart from the others.
            (Level::Info, false) => (rgb(229, 239, 253), rgb(26, 84, 168)),
            (Level::Info, true) => (rgb(24, 40, 66), rgb(140, 185, 250)),
        }
    }
}

/// A rounded badge with the colours of `level`: a verdict, an SBE, a count. Drawn at the
/// size of its text, so that a column fitted to its content shows it whole.
fn badge(ui: &mut egui::Ui, text: impl Into<String>, level: Level) -> egui::Response {
    let (fill, ink) = Palette::now().badge(level);
    pill(ui, text.into(), egui::FontId::proportional(12.0), fill, ink)
}

fn pill(ui: &mut egui::Ui, text: String, font: egui::FontId, fill: Color32, ink: Color32) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text, font, ink);
    let (rect, response) = ui.allocate_exact_size(galley.size() + egui::vec2(16.0, 4.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, rect.height() / 2.0, fill);
    ui.painter().galley(rect.min + egui::vec2(8.0, 2.0), galley, ink);
    response
}

/// A recording source's badge (SBD, AUD...): (fill, text), a hue for each.
fn lineage_colours(kind: &str, dark: bool) -> (Color32, Color32) {
    let rgb = Color32::from_rgb;
    match (kind, dark) {
        ("SBD", false) => (rgb(226, 236, 248), rgb(29, 79, 143)),
        ("SBD", true) => (rgb(29, 49, 80), rgb(169, 200, 240)),
        ("FM" | "Pre-FM", false) => (rgb(239, 230, 247), rgb(91, 45, 134)),
        ("FM" | "Pre-FM", true) => (rgb(56, 36, 72), rgb(213, 184, 239)),
        ("DAT", false) => (rgb(230, 241, 239), rgb(31, 95, 87)),
        ("DAT", true) => (rgb(23, 58, 53), rgb(163, 219, 210)),
        ("MTX", false) => (rgb(236, 235, 245), rgb(63, 58, 120)),
        ("MTX", true) => (rgb(44, 42, 80), rgb(195, 191, 240)),
        (_, false) => (rgb(242, 227, 204), rgb(122, 62, 6)),
        (_, true) => (rgb(74, 46, 18), rgb(242, 201, 149)),
    }
}

/// The recording source of a show: from its folder name, else from the "Source:" lines of
/// its info file (true).
fn show_lineage(root: &Path) -> Option<(&'static str, bool)> {
    if let Some(kind) = crate::files::lineage(&crate::files::show_name(root)) {
        return Some((kind, false));
    }
    ops::info_candidates(root)
        .iter()
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() < 1 << 20))
        .take(3)
        .find_map(|p| crate::files::read_text(p).ok().and_then(|text| crate::files::lineage_of_info(&text)))
        .map(|kind| (kind, true))
}

/// The cassette of the welcome screen and of the drop frame, drawn in a 100 × 64 box scaled
/// to `rect`: `label` handwritten on it, the tape moved by `turn` (the reels turn by it over
/// their radius), `wound` the share of the tape on the right reel.
fn paint_cassette(painter: &egui::Painter, rect: egui::Rect, label: &str, turn: f32, wound: f32) {
    let s = rect.width() / 100.0;
    let at = |x: f32, y: f32| rect.min + egui::vec2(x, y) * s;
    let area = |x0: f32, y0: f32, x1: f32, y1: f32| egui::Rect::from_min_max(at(x0, y0), at(x1, y1));
    let thin = |width: f32, colour: Color32| egui::Stroke::new(width * s, colour);
    let rgb = Color32::from_rgb;
    let (ink, tape) = (rgb(43, 41, 37), rgb(90, 47, 16));
    // The shell, with a lighter edge.
    painter.rect(area(1.0, 1.0, 99.0, 63.0), 5.0 * s, rgb(54, 50, 46), thin(0.6, rgb(20, 19, 18)));
    painter.rect_stroke(area(2.2, 2.2, 97.8, 61.8), 4.2 * s, thin(0.35, rgb(90, 84, 77)));
    // The label: an amber band along the top, a ruled line, the name written by hand.
    painter.rect_filled(area(8.0, 5.0, 92.0, 40.0), 2.5 * s, rgb(243, 230, 204));
    painter.rect_filled(area(8.0, 5.0, 92.0, 11.0), egui::Rounding { nw: 2.5 * s, ne: 2.5 * s, sw: 0.0, se: 0.0 }, rgb(168, 86, 12));
    painter.text(at(88.0, 8.0), egui::Align2::RIGHT_CENTER, "C90 · TYPE II", egui::FontId::new(3.2 * s, crate::fonts::bold()), rgb(251, 233, 207));
    painter.line_segment([at(11.0, 17.0), at(89.0, 17.0)], thin(0.3, rgb(217, 199, 166)));
    let mut job = egui::text::LayoutJob::single_section(label.to_string(), egui::TextFormat::simple(egui::FontId::new(6.4 * s, crate::fonts::hand()), rgb(59, 42, 26)));
    job.wrap = egui::text::TextWrapping { max_width: 76.0 * s, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let galley = painter.layout_job(job);
    painter.galley(at(12.0, 17.6) - egui::vec2(0.0, galley.size().y), galley, Color32::BLACK);
    // The window: the tape on its two reels (the same area of tape in all), then the hubs,
    // an amber dot on each to see them turn.
    let window = area(26.0, 18.0, 74.0, 35.0);
    painter.rect_filled(window, 8.5 * s, rgb(239, 227, 205));
    let (small, big) = (4.6f32, 8.0f32);
    let reel = |share: f32| libm::sqrtf(small * small + share.clamp(0.0, 1.0) * (big * big - small * small));
    let reels = [(38.0, reel(1.0 - wound)), (62.0, reel(wound))];
    let inside = painter.with_clip_rect(window.intersect(painter.clip_rect()));
    for (x, r) in reels {
        inside.circle(at(x, 26.5), r * s, tape, thin(0.35, rgb(61, 30, 8)));
    }
    inside.line_segment([at(34.0, 34.4), at(66.0, 34.4)], thin(0.9, tape));
    painter.rect_stroke(window, 8.5 * s, thin(0.6, ink));
    painter.rect_stroke(area(44.0, 21.0, 56.0, 32.0), 1.0 * s, thin(0.35, rgb(138, 122, 98)));
    for (x, r) in reels {
        let centre = at(x, 26.5);
        painter.circle(centre, 3.6 * s, rgb(244, 239, 230), thin(0.4, ink));
        painter.circle_filled(centre, 1.4 * s, ink);
        let angle = turn / r;
        painter.circle_filled(centre + egui::vec2(libm::sinf(angle), -libm::cosf(angle)) * 2.5 * s, 0.9 * s, rgb(217, 130, 43));
    }
    // The bottom edge, where the tape shows, and the screws.
    painter.add(egui::Shape::convex_polygon(vec![at(23.0, 63.0), at(29.0, 51.0), at(71.0, 51.0), at(77.0, 63.0)], rgb(44, 41, 38), thin(0.5, rgb(20, 19, 18))));
    painter.line_segment([at(29.5, 51.8), at(70.5, 51.8)], thin(0.7, tape));
    for x in [35.0, 65.0] {
        painter.circle_filled(at(x, 57.5), 1.7 * s, rgb(14, 13, 12));
    }
    painter.rect_filled(area(46.5, 54.0, 53.5, 57.4), 0.6 * s, rgb(201, 164, 106));
    for (x, y) in [(5.0, 5.0), (95.0, 5.0), (5.0, 59.0), (95.0, 59.0), (50.0, 45.5)] {
        painter.circle_filled(at(x, y), 1.4 * s, rgb(143, 138, 130));
        painter.line_segment([at(x - 0.9, y), at(x + 0.9, y)], thin(0.4, rgb(74, 70, 63)));
    }
}

/// A group of buttons on one pale track (the status bar's filter and density).
fn segmented(ui: &mut egui::Ui, p: Palette, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(p.chrome.lerp_to_gamma(p.border, 0.45))
        .stroke(egui::Stroke::new(1.0f32, p.button_border))
        .rounding(9.0)
        .inner_margin(2.0)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(add);
        });
}

/// One button of a `segmented` group: white and raised when on. An empty text leaves room
/// for a drawn icon.
fn segment(ui: &mut egui::Ui, p: Palette, text: egui::text::LayoutJob, on: bool) -> egui::Response {
    let button = if text.is_empty() { egui::Button::new("").min_size(egui::vec2(28.0, 22.0)) } else { egui::Button::new(text).min_size(egui::vec2(0.0, 22.0)) };
    let fill = if on { p.button } else { Color32::TRANSPARENT };
    ui.add(button.fill(fill).stroke(if on { egui::Stroke::new(1.0f32, p.button_border) } else { egui::Stroke::NONE }).rounding(7.0))
}

/// The symbol of a verdict in its badge.
fn mark(level: Level) -> &'static str {
    match level {
        Level::Ok => "✔ ",
        Level::Warn => "⚠ ",
        Level::Error => "✖ ",
        Level::Info => "? ",
    }
}

/// The empty list: a drop zone with the three ways to add shows and the formats read.
/// Returns the button clicked: 0 folder, 1 files, 2 DVD. Scrolled when the window is too small.
fn welcome(ui: &mut egui::Ui, p: Palette, busy: bool) -> Option<usize> {
    let mut clicked = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let space = ui.available_rect_before_wrap().shrink(20.0);
        let zone = egui::Rect::from_min_size(space.min, egui::vec2(space.width(), space.height().max(380.0)));
        ui.painter().rect_filled(zone, 16.0, p.ground.lerp_to_gamma(p.show_row, 0.6));
        let dash = egui::Stroke::new(2.0f32, p.button_border.lerp_to_gamma(p.accent, 0.25));
        ui.painter().extend(egui::Shape::dashed_line(&rounded_outline(zone.shrink(1.0), 16.0), dash, 8.0, 6.0));
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(zone), |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(((zone.height() - 360.0) / 2.0).max(16.0));
                let (icon, _) = ui.allocate_exact_size(egui::vec2(150.0, 96.0), egui::Sense::hover());
                paint_cassette(ui.painter(), icon, "Bootleg Toolbox", 0.0, 0.3);
                ui.add_space(14.0);
                ui.label(RichText::new("Drop show folders or audio files here").size(21.0).family(crate::fonts::bold()));
                ui.add_space(4.0);
                ui.label(RichText::new("A new drop replaces the list. Hold Ctrl to add to it instead.").size(14.0).color(p.weak));
                ui.add_space(16.0);
                let buttons = ["Add folder...", "Add files...", "Add DVD..."];
                centered_row(ui, &buttons.map(|b| text_width(ui, b, 14.0) + 36.0), |ui| {
                    for (i, text) in buttons.iter().enumerate() {
                        let label = RichText::new(*text).size(14.0);
                        let button = if i == 0 {
                            egui::Button::new(label.color(p.ground).family(crate::fonts::bold())).fill(p.accent)
                        } else {
                            egui::Button::new(label)
                        };
                        if ui.add_enabled(!busy, button.min_size(egui::vec2(0.0, 38.0))).clicked() {
                            clicked = Some(i);
                        }
                    }
                });
                ui.add_space(18.0);
                let groups: [(&[&str], Level); 3] = [
                    (&["FLAC", "WAV", "AIFF", "APE", "SHN", "WavPack", "ALAC"], Level::Warn),
                    (&["MP3", "MP2", "AAC", "Ogg Vorbis"], Level::Info),
                    (&["M4A", "MKA", "MP4 videos", "MKV videos", "DVD folders"], Level::Ok),
                ];
                for (names, kind) in groups {
                    let widths: Vec<f32> = names.iter().map(|n| text_width(ui, n, 12.0) + 20.0).collect();
                    centered_row(ui, &widths, |ui| {
                        for name in names {
                            // Lossless in amber, lossy in grey, containers and videos in white.
                            let (fill, ink) = match kind {
                                Level::Warn => (p.tint, p.accent),
                                Level::Info => (p.chrome, p.weak),
                                _ => (p.ground, p.weak),
                            };
                            egui::Frame::none()
                                .fill(fill)
                                .stroke(egui::Stroke::new(1.0f32, if kind == Level::Ok { p.button_border } else { fill }))
                                .rounding(egui::Rounding::same(10.0))
                                .inner_margin(egui::Margin::symmetric(8.0, 2.0))
                                .show(ui, |ui| ui.add(egui::Label::new(RichText::new(*name).size(12.0).color(ink)).extend()));
                        }
                    });
                    ui.add_space(2.0);
                }
                ui.add_space(4.0);
                ui.label(RichText::new("Lossless formats in amber, lossy ones in grey, containers and videos in white").size(12.0).color(p.weak));
            });
        });
        ui.allocate_rect(zone, egui::Sense::hover());
    });
    clicked
}

/// The outline of a rounded rectangle, as points for a dashed line.
fn rounded_outline(rect: egui::Rect, radius: f32) -> Vec<egui::Pos2> {
    let r = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let corners = [
        (egui::pos2(rect.right() - r, rect.top() + r), -90.0f32),
        (egui::pos2(rect.right() - r, rect.bottom() - r), 0.0),
        (egui::pos2(rect.left() + r, rect.bottom() - r), 90.0),
        (egui::pos2(rect.left() + r, rect.top() + r), 180.0),
    ];
    let mut points = Vec::new();
    for (centre, start) in corners {
        for step in 0..=8 {
            let a = (start + step as f32 * 90.0 / 8.0).to_radians();
            points.push(centre + egui::vec2(libm::cosf(a), libm::sinf(a)) * r);
        }
    }
    points.push(points[0]);
    points
}

/// The width of a text at this size.
fn text_width(ui: &egui::Ui, text: &str, size: f32) -> f32 {
    ui.fonts(|f| f.layout_no_wrap(text.to_string(), egui::FontId::proportional(size), Color32::WHITE).size().x)
}

/// A row of items of these widths, centred in the space (vertical_centered does not
/// centre a horizontal row).
fn centered_row(ui: &mut egui::Ui, widths: &[f32], add: impl FnOnce(&mut egui::Ui)) {
    let gap = ui.spacing().item_spacing.x;
    let total = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
    ui.allocate_ui(egui::vec2(total.min(ui.available_width()), 0.0), |ui| {
        ui.horizontal_wrapped(add);
    });
}

/// A toolbar menu's title: its icon in the accent colour while it can be used, its name,
/// and the arrow of a menu.
fn tool_title(icon: &str, name: &str, enabled: bool, style: &egui::Style) -> egui::WidgetText {
    use egui::text::{LayoutJob, TextFormat};
    let font = egui::TextStyle::Button.resolve(style);
    // PLACEHOLDER: the button's own text colour (weaker when disabled).
    let icon_color = if enabled { Palette::of(style.visuals.dark_mode).accent } else { Color32::PLACEHOLDER };
    let mut job = LayoutJob::default();
    job.append(&format!("{icon}  "), 0.0, TextFormat::simple(font.clone(), icon_color));
    job.append(&format!("{name}  "), 0.0, TextFormat::simple(font.clone(), Color32::PLACEHOLDER));
    job.append("⏷", 0.0, TextFormat::simple(font, Color32::PLACEHOLDER));
    job.into()
}

/// The room for the dot before a Result text.
const DOT_WIDTH: f32 = 14.0;

fn color(level: Level) -> Option<Color32> {
    let dark = DARK_THEME.load(Ordering::Relaxed);
    match level {
        Level::Info => None,
        Level::Ok if dark => Some(Color32::from_rgb(58, 157, 93)),
        Level::Ok => Some(Color32::from_rgb(25, 128, 56)),
        // Yellow rather than orange, so that a doubt never looks like an error; dark
        // enough to read on white.
        Level::Warn if dark => Some(Color32::from_rgb(240, 196, 60)),
        Level::Warn => Some(Color32::from_rgb(138, 101, 0)),
        Level::Error if dark => Some(Color32::from_rgb(217, 69, 61)),
        Level::Error => Some(Color32::from_rgb(196, 36, 30)),
    }
}

fn colored(text: impl Into<String>, level: Level) -> RichText {
    let rich = RichText::new(text.into());
    match color(level) {
        Some(c) => rich.color(c),
        None => rich,
    }
}

/// A level's colour for a dot, a ring or a mark, which no text has to be read on: the vivid
/// yellow for a doubt, blue for what is only information.
fn vivid(level: Level) -> Color32 {
    let dark = DARK_THEME.load(Ordering::Relaxed);
    match level {
        Level::Warn if !dark => Color32::from_rgb(232, 176, 0),
        Level::Info if dark => Color32::from_rgb(110, 160, 240),
        Level::Info => Color32::from_rgb(52, 120, 220),
        level => color(level).unwrap_or(Color32::GRAY),
    }
}

/// A level's mark in a disc: a tick, a cross, an exclamation mark, a dot.
fn paint_mark(painter: &egui::Painter, centre: egui::Pos2, radius: f32, level: Level, fill: Color32, ink: Color32) {
    painter.circle_filled(centre, radius, fill);
    let s = radius * 0.45;
    let stroke = egui::Stroke::new((radius * 0.2).max(1.4), ink);
    let p = |x: f32, y: f32| centre + egui::vec2(x * s, y * s);
    match level {
        Level::Ok => {
            painter.add(egui::Shape::line(vec![p(-0.95, 0.05), p(-0.3, 0.7), p(1.0, -0.65)], stroke));
        }
        Level::Error => {
            painter.line_segment([p(-0.75, -0.75), p(0.75, 0.75)], stroke);
            painter.line_segment([p(0.75, -0.75), p(-0.75, 0.75)], stroke);
        }
        Level::Warn => {
            painter.line_segment([p(0.0, -1.05), p(0.0, 0.3)], stroke);
            painter.circle_filled(p(0.0, 0.95), stroke.width * 0.65, ink);
        }
        Level::Info => {
            painter.circle_filled(centre, stroke.width, ink);
        }
    }
}

/// A progress ring: the part done in the accent colour over a pale track, clockwise from the top.
fn paint_ring(painter: &egui::Painter, centre: egui::Pos2, radius: f32, fraction: f32, p: Palette) {
    let width = (radius * 0.32).max(1.8);
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction == 0.0 {
        // Not started: a pale dotted circle (a plain one reads as the letter O).
        let points: Vec<egui::Pos2> =
            (0..=40).map(|i| centre + radius * egui::vec2(libm::cosf(std::f32::consts::TAU * i as f32 / 40.0), libm::sinf(std::f32::consts::TAU * i as f32 / 40.0))).collect();
        let dash = std::f32::consts::TAU * radius / 12.0;
        painter.extend(egui::Shape::dashed_line(&points, egui::Stroke::new(width * 0.8, p.weak.gamma_multiply(0.55)), dash * 0.55, dash * 0.45));
        return;
    }
    painter.circle_stroke(centre, radius, egui::Stroke::new(width, p.tint_strong));
    if fraction > 0.0 {
        let steps = (libm::ceilf(48.0 * fraction) as usize).max(2);
        let points: Vec<egui::Pos2> = (0..=steps)
            .map(|i| {
                let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * fraction * i as f32 / steps as f32;
                centre + radius * egui::vec2(libm::cosf(a), libm::sinf(a))
            })
            .collect();
        painter.add(egui::Shape::line(points, egui::Stroke::new(width, p.accent)));
    }
}

/// A file's outline in the Wave column: its peaks, mirrored around the middle, scaled to the
/// loudest.
fn paint_wave(ui: &mut egui::Ui, peaks: &[f32]) -> egui::Response {
    let height = (ui.available_height() - 6.0).clamp(8.0, 20.0);
    let width = ui.available_width().clamp(30.0, 160.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let loudest = peaks.iter().copied().fold(0f32, f32::max).max(1e-6);
    let step = rect.width() / peaks.len() as f32;
    let stroke = egui::Stroke::new((step * 0.7).clamp(0.6, 1.6), Palette::now().weak.gamma_multiply(0.75));
    for (i, peak) in peaks.iter().enumerate() {
        let x = rect.left() + (i as f32 + 0.5) * step;
        let half = (peak / loudest * rect.height() / 2.0).max(0.4);
        ui.painter().line_segment([egui::pos2(x, rect.center().y - half), egui::pos2(x, rect.center().y + half)], stroke);
    }
    response
}

/// The spectrogram of a file in its lossless analysis, with the lowpass found drawn on it.
fn report_plot(ui: &mut egui::Ui, plot: Option<&PlotState>, cutoff: Option<f64>) {
    ui.add_space(4.0);
    let width = ui.available_width();
    let size = egui::vec2(width, (width * 0.36).clamp(140.0, 260.0));
    let Some(PlotState::Ready { texture, nyquist, seconds }) = plot else {
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter().rect_filled(rect, 10.0, Color32::from_rgb(13, 7, 22));
        let text = match plot {
            Some(PlotState::Failed(e)) => format!("No spectrogram: {e}"),
            _ => "Drawing the spectrogram...".to_string(),
        };
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(13.0), Color32::from_rgb(201, 188, 214));
        return;
    };
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    egui::Image::new((texture.id(), size)).rounding(10.0).paint_at(ui, rect);
    let label = |pos: egui::Pos2, align: egui::Align2, text: String| {
        let galley = ui.painter().layout_no_wrap(text, egui::FontId::proportional(11.0), Color32::from_rgb(235, 228, 242));
        let at = align.anchor_size(pos, galley.size());
        ui.painter().rect_filled(at.expand2(egui::vec2(5.0, 1.0)), 4.0, Color32::from_black_alpha(150));
        ui.painter().galley(at.min, galley, Color32::WHITE);
    };
    label(rect.left_top() + egui::vec2(10.0, 8.0), egui::Align2::LEFT_TOP, format!("{} kHz", crate::audio::trim_float(libm::round(nyquist / 100.0) / 10.0)));
    label(rect.left_bottom() + egui::vec2(10.0, -8.0), egui::Align2::LEFT_BOTTOM, "0".to_string());
    label(rect.right_bottom() + egui::vec2(-10.0, -8.0), egui::Align2::RIGHT_BOTTOM, fmt_total(*seconds));
    if let Some(hz) = cutoff.filter(|hz| *hz < *nyquist) {
        let y = rect.bottom() - (hz / nyquist) as f32 * rect.height();
        let gold = Color32::from_rgb(255, 210, 122);
        ui.painter().extend(egui::Shape::dashed_line(&[egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], egui::Stroke::new(2.0f32, gold), 7.0, 5.0));
        let galley = ui.painter().layout_no_wrap(format!("lowpass {:.1} kHz", hz / 1000.0), egui::FontId::new(12.0, crate::fonts::bold()), gold);
        let at = egui::Align2::RIGHT_BOTTOM.anchor_size(egui::pos2(rect.right() - 12.0, y - 5.0), galley.size());
        ui.painter().rect_filled(at.expand2(egui::vec2(7.0, 2.0)), 5.0, Color32::from_rgba_unmultiplied(13, 7, 22, 200));
        ui.painter().galley(at.min, galley, gold);
    }
}

/// The Speed? dial: how fast or slow the recording plays, -3 % to +3 %, green while
/// within 0.3 %, yellow to 1 %.
fn speed_dial(ui: &mut egui::Ui, p: Palette, percent: f64, level: Level) {
    const RANGE: f64 = 3.0;
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 150.0), egui::Sense::hover());
    let radius = 104.0f32;
    let centre = egui::pos2(rect.center().x, rect.top() + 18.0 + radius);
    let at = |value: f64, r: f32| {
        let a = std::f32::consts::FRAC_PI_2 - (value.clamp(-RANGE, RANGE) / RANGE) as f32 * std::f32::consts::FRAC_PI_2;
        centre + egui::vec2(libm::cosf(a), -libm::sinf(a)) * r
    };
    let arc = |from: f64, to: f64, colour: Color32| {
        let points: Vec<egui::Pos2> = (0..=32).map(|i| at(from + (to - from) * i as f64 / 32.0, radius)).collect();
        ui.painter().add(egui::Shape::line(points, egui::Stroke::new(18.0f32, colour)));
    };
    arc(-RANGE, RANGE, p.show_row.lerp_to_gamma(p.border, 0.5));
    let (yellow, _) = p.badge(Level::Warn);
    let (green, _) = p.badge(Level::Ok);
    arc(-1.0, 1.0, yellow);
    arc(-0.3, 0.3, green);
    ui.painter().line_segment([at(0.0, radius - 13.0), at(0.0, radius + 13.0)], egui::Stroke::new(2.0f32, p.weak));
    for (value, text) in [(-RANGE, "−3 %"), (0.0, "0"), (RANGE, "+3 %")] {
        let pos = if value == 0.0 { at(0.0, radius - 24.0) } else { at(value, radius) + egui::vec2(0.0, 18.0) };
        ui.painter().text(pos, egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(11.0), p.weak);
    }
    let needle = vivid(level);
    ui.painter().line_segment([centre, at(percent, radius - 8.0)], egui::Stroke::new(3.5f32, needle));
    ui.painter().circle_filled(centre, 8.0, needle);
    ui.painter().circle_filled(centre, 3.0, p.ground);
    let text = if percent.abs() < 0.05 {
        "at the right speed".to_string()
    } else {
        format!("{:+.1} % · plays {}", percent, if percent > 0.0 { "fast" } else { "slow" })
    };
    let off = if percent.abs() > RANGE { " (off the dial)" } else { "" };
    ui.vertical_centered(|ui| {
        ui.label(colored(format!("{text}{off}"), level).size(17.0).family(crate::fonts::bold()));
    });
}

/// Where a show stands on its row: the share of its files done by the running task, or the
/// worst result of its files.
#[derive(Clone, Copy)]
enum Ring {
    Running(f32),
    Done(Level),
}

/// A message at the bottom right of the window when a task ends.
struct Toast {
    id: u64,
    title: String,
    /// "Done", "2 to check"..., in the colour of `level`.
    outcome: String,
    detail: String,
    level: Level,
    /// How long it has been shown (not counting the time under the pointer), and when last drawn.
    age: Duration,
    seen: Instant,
    /// When it came: it slides in.
    born: Instant,
    /// The task made files: "Open folder" shows the first show's folder.
    folder: Option<PathBuf>,
    /// Files to check or with an error: a link sets the list filter to them.
    filter: Option<(Level, usize)>,
}

impl Toast {
    /// How long it stays: errors until closed.
    fn life(&self) -> Option<Duration> {
        match self.level {
            Level::Error => None,
            Level::Warn => Some(Duration::from_secs(10)),
            _ => Some(Duration::from_secs(6)),
        }
    }
}

#[derive(Clone, Copy)]
enum Row {
    Show(usize),
    /// A disc's folder, by its first file.
    Disc(usize, usize),
    File(usize, usize),
}

enum Action {
    Run(Op),
    Click { index: usize, ctrl: bool, shift: bool },
    RightClick(usize),
    Toggle(PathBuf),
    OpenFolder(PathBuf),
    CopyFfp,
    CopyInfo,
    TagInfo,
    Setlist,
    /// Share cards of these shows, or of the selection.
    ShareCard(Option<Vec<PathBuf>>),
    ExtractAudio,
    /// Pick .torrent files, then check the selection against them.
    TorrentCheck,
    Remove,
    Deselect,
    SelectAll,
    OpenReport(ReportKind, PathBuf),
    ToggleColumn(Col),
    DragStart { index: usize, ctrl: bool },
    DragOver(usize),
    FlacLevel(u8),
    Compact(bool),
    CollapsedView(bool),
    AlwaysOnTop(bool),
    LogOpen(bool),
    Theme(ThemeChoice),
    ShowAllColumns,
}

struct Running {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    label: String,
    done: usize,
    total: usize,
    /// Torrents being made with "open the .ffp and info file" on: the show folders.
    open_after: Vec<PathBuf>,
    /// The files of the task, those with a result already (progress rings), and the show
    /// folders.
    files: HashSet<PathBuf>,
    done_files: HashSet<PathBuf>,
    roots: Vec<PathBuf>,
    /// The task's short name, and the files and folders whose Result it has given already
    /// (the others show their former result faded).
    name: &'static str,
    updated: HashSet<PathBuf>,
    /// Results so far: OK, to check, with an error.
    tally: [usize; 3],
    /// Started without a word in the log (details read by themselves): no notification either.
    quiet: bool,
    /// The task writes files next to the shows.
    makes_files: bool,
    /// The work of each file (its samples, or bytes when unknown) and its length in seconds:
    /// progress and time left go by the work done, not by the number of files (the biggest
    /// files go first).
    started: Instant,
    work: HashMap<PathBuf, (f64, f64)>,
    /// Time left as shown, smoothed, and when it was last updated.
    eta: Option<(Instant, f64)>,
}

impl Running {
    /// Share of the work done, audio seconds done and in all, time left (raw) once it can be told.
    fn progress(&self) -> (f32, f64, f64, Option<f64>) {
        let (mut all, mut done, mut secs, mut secs_done) = (0.0, 0.0, 0.0, 0.0);
        for (path, (w, s)) in &self.work {
            all += w;
            secs += s;
            if self.done_files.contains(path) {
                done += w;
                secs_done += s;
            }
        }
        // Tasks that answer by show (torrents, tagging): by the count they give.
        let fraction = if done > 0.0 && all > 0.0 {
            done / all
        } else if self.total > 0 {
            self.done as f64 / self.total as f64
        } else {
            0.0
        };
        let elapsed = self.started.elapsed().as_secs_f64();
        // Nothing in the first 15 s nor before 5 %: the first (biggest) files are still running.
        let left = (elapsed >= 15.0 && (0.05..1.0).contains(&fraction)).then(|| elapsed * (1.0 - fraction) / fraction);
        (fraction as f32, secs_done, secs, left)
    }
}

/// "about 4 min left", "about 1 h 20 min left", "less than a minute left".
fn time_left(seconds: f64) -> String {
    let minutes = libm::round(seconds / 60.0) as u64;
    match minutes {
        0 => "less than a minute left".to_string(),
        1..=89 => format!("about {minutes} min left"),
        _ => format!("about {} h {:02} min left", minutes / 60, minutes % 60),
    }
}

pub struct App {
    opt: Options,
    roots: Vec<PathBuf>,
    excluded: HashSet<PathBuf>,
    shows: Vec<Show>,
    info: HashMap<PathBuf, AudioInfo>,
    status: HashMap<PathBuf, (String, Level, String)>,
    /// Lossless check verdicts (Source column).
    source: HashMap<PathBuf, (String, Level, String)>,
    collapsed: HashSet<PathBuf>,
    selected: HashSet<PathBuf>,
    anchor: Option<usize>,
    /// Selection by dragging over the rows: the row where it started, and the selection
    /// it adds to (empty unless Ctrl was held).
    drag_select: Option<(usize, HashSet<PathBuf>)>,
    log: Vec<(String, Level)>,
    running: Option<Running>,
    flash: Option<(String, Instant)>,
    settings: Option<Options>,
    about: bool,
    /// Help > Keyboard shortcuts and command line.
    help: bool,
    /// "Copy info file" with several text files: the choice window.
    info_choice: Option<InfoChoice>,
    /// "Tag and rename from info file" window.
    tag_dialog: Option<TagDialog>,
    /// Shows waiting for the user to choose their setlist.fm show.
    setlist_choices: Vec<SetlistChoice>,
    extract_dialog: Option<ExtractDialog>,
    undo_dialog: Option<UndoDialog>,
    tag_batch: Option<TagBatch>,
    /// The saved "Always on top" choice was applied to the window.
    window_level_set: bool,
    /// A file dialog to open, and since when it waits for the window to leave the top.
    dialog: Option<(Dialog, Instant)>,
    /// The main window, parent of the file dialogs.
    parent: Option<ParentWindow>,
    title: String,
    /// A context or menu-bar menu was open during the last frame (Escape closes it
    /// instead of clearing the selection).
    menu_open: bool,
    spectrograms: Vec<Spectrogram>,
    /// Full lossless analyses ("See more"), and the ones open in a window.
    reports: HashMap<(ReportKind, PathBuf), Arc<crate::report::Report>>,
    open_reports: Vec<(ReportKind, PathBuf)>,
    /// Speed check verdicts (Speed column).
    speed: HashMap<PathBuf, (String, Level, String)>,
    /// Fit the columns to their content on the next frame (new files, new results).
    fit_columns: bool,
    /// Result of the last conversion, and the window offered after a conversion to FLAC.
    converted: Option<(Target, Vec<PathBuf>, usize, usize)>,
    /// A conversion to FLAC waiting for the float WAVs to be read, then for the user's choice.
    float_pending: Option<FloatPending>,
    float_dialog: Option<FloatPending>,
    /// .torrent files dropped while something ran: checked when it ends.
    pending_torrents: Option<Vec<PathBuf>>,
    /// Open media info windows: (show or DVD name, text).
    media_windows: Vec<(String, String)>,
    after_flac: Option<AfterFlac>,
    clipboard: Option<arboard::Clipboard>,
    /// A running update check: (asked from the Help menu, result).
    update_check: Option<mpsc::Receiver<UpdateCheck>>,
    /// A newer version found on GitHub.
    newer_version: Option<String>,
    /// Only the files with this result in the list (the status bar's buttons).
    filter: Option<Level>,
    toasts: Vec<Toast>,
    toasts_made: u64,
    /// The task that gave the Result column (the status bar says so).
    results_of: Option<&'static str>,
    /// Top of the status bar and log: the notifications sit just above.
    bottom_top: f32,
    /// The recording source of each show (SBD, AUD...) and whether its info file gave it.
    lineage: HashMap<PathBuf, Option<(&'static str, bool)>>,
    /// The size of the files of the list (its summary in the toolbar).
    list_bytes: u64,
    /// When files and shows got their result from a task: their rows glow for a moment.
    fresh: HashMap<PathBuf, Instant>,
    /// The end of a long task, in a band above the list until closed or another task starts.
    banner: Option<Banner>,
    /// The outline of the files an analysis read (Wave column).
    waves: HashMap<PathBuf, Vec<f32>>,
    /// The spectrograms of the lossless analysis windows.
    plots: HashMap<PathBuf, PlotState>,
    /// Share cards asked from the end-of-task band: made on the next frame.
    pending_share: Option<Vec<PathBuf>>,
}

/// A drawn spectrogram, and the file's outline read on the way.
type PlotDone = (Result<crate::spectrogram::Plot, String>, Option<Vec<f32>>);

enum PlotState {
    Pending(Receiver<PlotDone>),
    Ready { texture: egui::TextureHandle, nyquist: f64, seconds: f64 },
    Failed(String),
}

/// How long a row glows after its result came.
const GLOW: f32 = 1.6;

struct Banner {
    level: Level,
    title: String,
    detail: String,
    /// The show folders of the task (their share cards).
    roots: Vec<PathBuf>,
    /// Files to check or with an error: a link sets the list filter to them.
    filter: Option<(Level, usize)>,
}

/// An open spectrogram window.
/// The table columns; all but Name can be hidden (View menu, right-click on the header).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Col {
    Name,
    Format,
    Length,
    Wave,
    Rate,
    Bits,
    Ch,
    Sbe,
    Source,
    Speed,
    Result,
}

impl Col {
    const ALL: [Col; 11] = [Col::Name, Col::Format, Col::Bits, Col::Rate, Col::Length, Col::Wave, Col::Ch, Col::Sbe, Col::Source, Col::Speed, Col::Result];

    fn title(self) -> &'static str {
        match self {
            Col::Name => "Name",
            Col::Format => "Format",
            Col::Length => "Length",
            Col::Wave => "Wave",
            Col::Rate => "Rate",
            Col::Bits => "Bits",
            Col::Ch => "Ch",
            Col::Sbe => "SBE",
            Col::Source => "Source",
            Col::Speed => "Speed",
            Col::Result => "Result",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Col::Name => "File or show name",
            Col::Format => "Audio format",
            Col::Length => "Length (minutes:seconds.frames for CD audio)",
            Col::Wave => "Outline of the sound (its peaks), once Lossless?, Speed? or a spectrogram has read the file: fades, silences, level jumps",
            Col::Rate => "Sample rate",
            Col::Bits => "Bit depth",
            Col::Ch => "Channels",
            Col::Sbe => "Sector boundary error: a gap if burned to CD (normal on the last track of a disc)",
            Col::Source => "Lossless? verdict: click it for the full analysis",
            Col::Speed => "Speed? verdict: click it for the full analysis",
            Col::Result => "Result of the last operation",
        }
    }

    /// The last visible column takes the remaining width; Name keeps its own (show names are
    /// long); the others fit their content (header included) whenever `fit` is set. All stay
    /// resizable, and a double-click on a separator fits the column on its left.
    fn column(self, last: bool, fit: bool) -> Column {
        if last {
            return Column::remainder().at_least(120.0).clip(true);
        }
        match self {
            Col::Name => Column::initial(300.0).at_least(150.0).clip(true),
            Col::Wave => Column::initial(96.0).at_least(40.0).clip(true),
            _ => Column::auto().at_least(24.0).clip(true).auto_size_this_frame(fit),
        }
    }
}

/// Check boxes for the columns; the menu stays open so several can be changed at once.
fn columns_menu(ui: &mut egui::Ui, visible: &[Col], actions: &mut Vec<Action>) {
    ui.label(RichText::new("Columns").strong());
    for col in Col::ALL {
        let mut shown = visible.contains(&col);
        let response = ui.add_enabled(col != Col::Name, egui::Checkbox::new(&mut shown, col.title())).on_hover_text(col.describe());
        if response.clicked() {
            actions.push(Action::ToggleColumn(col));
        }
    }
    ui.separator();
    if ui.button("Show all columns").clicked() {
        ui.close_menu();
        actions.push(Action::ShowAllColumns);
    }
}

/// The window offered once a conversion to FLAC is done.
/// A conversion to FLAC of a selection with float WAVs.
struct FloatPending {
    level: u8,
    shows: Vec<Show>,
    files: Vec<PathBuf>,
    scans: Vec<(PathBuf, crate::encode::FloatScan)>,
}

struct AfterFlac {
    roots: Vec<PathBuf>,
    sources: Vec<PathBuf>,
    failed: usize,
    /// Float WAVs rounded to 24-bit: converted, but their originals are kept.
    rounded: usize,
    /// Checksum files listing the converted originals.
    old: Vec<PathBuf>,
    trash: bool,
    trash_old: bool,
    ffp: bool,
}

/// The analyses that open a "See more" window.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum ReportKind {
    Lossless,
    Speed,
}

struct Spectrogram {
    source: PathBuf,
    image: crate::spectrogram::Image,
    texture: Option<egui::TextureHandle>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, paths: Vec<PathBuf>) -> Self {
        let mut opt: Options = cc.storage.and_then(|s| eframe::get_value(s, "options")).unwrap_or_default();
        // "Do not rename" was a file name choice before the Rename box.
        if opt.tag_scheme == crate::tagging::NameScheme::Keep {
            opt.tag_scheme = Default::default();
            opt.tag_rename = false;
        }
        let mut app = App {
            opt,
            roots: Vec::new(),
            excluded: HashSet::new(),
            shows: Vec::new(),
            info: HashMap::new(),
            status: HashMap::new(),
            source: HashMap::new(),
            collapsed: HashSet::new(),
            selected: HashSet::new(),
            anchor: None,
            drag_select: None,
            log: vec![(format!("{APP_NAME} {APP_VERSION} ready."), Level::Ok)],
            running: None,
            flash: None,
            settings: None,
            about: false,
            help: false,
            info_choice: None,
            tag_dialog: None,
            setlist_choices: Vec::new(),
            extract_dialog: None,
            undo_dialog: None,
            tag_batch: None,
            window_level_set: false,
            dialog: None,
            parent: None,
            title: String::new(),
            menu_open: false,
            spectrograms: Vec::new(),
            reports: HashMap::new(),
            open_reports: Vec::new(),
            speed: HashMap::new(),
            fit_columns: true,
            converted: None,
            float_pending: None,
            float_dialog: None,
            pending_torrents: None,
            media_windows: Vec::new(),
            after_flac: None,
            clipboard: None,
            update_check: None,
            newer_version: None,
            filter: None,
            toasts: Vec::new(),
            toasts_made: 0,
            results_of: None,
            bottom_top: f32::INFINITY,
            lineage: HashMap::new(),
            list_bytes: 0,
            fresh: HashMap::new(),
            banner: None,
            waves: HashMap::new(),
            plots: HashMap::new(),
            pending_share: None,
        };
        cc.egui_ctx.set_fonts(crate::fonts::definitions());
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.button_padding = egui::vec2(10.0, 5.0);
            style.spacing.interact_size.y = style.spacing.interact_size.y.max(28.0);
            // A new menu is first laid out within this size: at egui's 400 points, the long
            // menus (Tools, View, right-click) scrolled even on a tall screen. They stay
            // within the window (`fit_below`, `fit_window`).
            style.spacing.default_area_size.y = 4000.0;
        });
        // egui's light theme writes text in mid grey (80/255, and fainter for weak text):
        // darker, for reading on white.
        cc.egui_ctx.style_mut_of(egui::Theme::Light, |style| {
            style.visuals.widgets.noninteractive.fg_stroke.color = Color32::from_gray(30);
            style.visuals.widgets.inactive.fg_stroke.color = Color32::from_gray(25);
        });
        app.apply_theme(&cc.egui_ctx);
        // Once: the entries of the former name go; the menu entry comes back under the new one.
        if !app.opt.legacy_cleaned {
            app.opt.legacy_cleaned = true;
            #[cfg(target_os = "linux")]
            let had_menu_entry = legacy_menu_entry();
            if crate::integration::remove_legacy() {
                app.log.push(("Bootleg Helper is now Bootleg Toolbox: the entries of the old name were removed.".into(), Level::Info));
                #[cfg(target_os = "linux")]
                if had_menu_entry {
                    if let Ok(text) = install_desktop_entry() {
                        app.log.push((text, Level::Ok));
                    }
                }
            }
        }
        if app.opt.check_updates {
            app.check_updates(false);
        }
        // Keep the right-click entry pointing at this program file, wherever it now is.
        if app.opt.shell_integration {
            std::thread::spawn(|| {
                let _ = crate::integration::install();
            });
        }
        if !paths.is_empty() {
            app.add_paths(paths, false);
        }
        app
    }

    fn visible_columns(&self) -> Vec<Col> {
        Col::ALL
            .into_iter()
            .filter(|c| *c == Col::Name || !self.opt.hidden_columns.iter().any(|t| t == c.title()))
            // The compact view leaves out what the Format column then gives.
            .filter(|c| !(self.opt.compact && matches!(c, Col::Bits | Col::Rate | Col::Ch)))
            .collect()
    }

    /// "FFP and ST5 files", from the Settings.
    fn checksum_kinds(&self) -> String {
        let kinds: Vec<&str> = [(self.opt.create_ffp, "FFP"), (self.opt.create_md5, "MD5"), (self.opt.create_st5, "ST5"), (self.opt.create_sfv, "SFV")]
            .into_iter()
            .filter_map(|(on, kind)| on.then_some(kind))
            .collect();
        match kinds.as_slice() {
            [] => "no checksum file (none selected in Settings)".into(),
            [one] => format!("{one} files"),
            [rest @ .., last] => format!("{} and {last} files", rest.join(", ")),
        }
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        let preference = match self.opt.theme {
            ThemeChoice::Light => egui::ThemePreference::Light,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
            // winit cannot see the desktop theme under X11: ask the XDG portal.
            #[cfg(target_os = "linux")]
            ThemeChoice::System => match desktop_prefers_dark() {
                Some(true) => egui::ThemePreference::Dark,
                _ => egui::ThemePreference::Light,
            },
            #[cfg(not(target_os = "linux"))]
            ThemeChoice::System => egui::ThemePreference::System,
        };
        ctx.set_theme(preference);
        DARK_THEME.store(ctx.theme() == egui::Theme::Dark, Ordering::Relaxed);
        ACCENT.store(ops::Accent::ALL.iter().position(|a| *a == self.opt.accent).unwrap_or(0) as u8, Ordering::Relaxed);
        // The look: warm neutral panels, the accent, rounded white buttons.
        ctx.all_styles_mut(|style| {
            let p = Palette::of(style.visuals.dark_mode);
            let v = &mut style.visuals;
            v.window_rounding = egui::Rounding::same(10.0);
            v.menu_rounding = egui::Rounding::same(8.0);
            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
                w.rounding = egui::Rounding::same(7.0);
                w.expansion = 0.0;
            }
            v.panel_fill = p.chrome;
            v.window_fill = p.ground;
            v.hyperlink_color = p.accent;
            v.selection.bg_fill = p.tint;
            v.selection.stroke = egui::Stroke::new(1.0f32, p.accent);
            v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0f32, p.border);
            v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0f32, p.button_border);
            v.widgets.inactive.weak_bg_fill = p.button;
            v.widgets.inactive.bg_fill = p.button;
            v.widgets.hovered.weak_bg_fill = p.tint;
            v.widgets.hovered.bg_fill = p.tint;
            v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0f32, p.accent);
            v.widgets.active.weak_bg_fill = p.tint_strong;
            v.widgets.active.bg_fill = p.tint_strong;
            v.widgets.active.bg_stroke = egui::Stroke::new(1.0f32, p.accent);
            v.widgets.open.weak_bg_fill = p.tint;
            v.widgets.open.bg_stroke = egui::Stroke::new(1.0f32, p.accent);
        });
    }

    // ------------------------------------------------------------ list

    fn add_paths(&mut self, paths: Vec<PathBuf>, keep: bool) {
        if self.running.is_some() {
            self.flash("Wait for the current task to finish.");
            return;
        }
        // DVDs join the list like the shows (one row per title); Media info describes them.
        if paths.is_empty() {
            return;
        }
        if !keep {
            self.clear_list();
        }
        for p in paths {
            let p = std::path::absolute(&p).unwrap_or(p);
            self.excluded.retain(|e| e != &p && !e.starts_with(&p));
            if !self.roots.contains(&p) {
                self.roots.push(p);
            }
        }
        self.rebuild();
        self.auto_details();
    }

    /// Asks GitHub for the latest release, in the background.
    fn check_updates(&mut self, manual: bool) {
        if self.update_check.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send((manual, crate::update::newer_version()));
        });
        self.update_check = Some(rx);
    }

    fn poll_update_check(&mut self) {
        let Some(rx) = &self.update_check else { return };
        let Ok((manual, result)) = rx.try_recv() else { return };
        self.update_check = None;
        match result {
            Ok(Some(version)) => {
                self.log(format!("{APP_NAME} {version} is available (you have {APP_VERSION}): Help > Download version {version}."), Level::Ok);
                self.flash(format!("{APP_NAME} {version} is available: see the Help menu."));
                self.newer_version = Some(version);
            }
            Ok(None) if manual => self.log(format!("{APP_NAME} {APP_VERSION} is the latest version."), Level::Ok),
            Err(e) if manual => self.log(format!("Could not check for updates: {e}."), Level::Warn),
            // The automatic check stays silent when there is nothing to say.
            _ => {}
        }
    }

    /// Opens a file dialog at the start of a coming frame. An always-on-top window would
    /// cover it (KDE shows it from another process, whatever its parent): the window
    /// leaves the top first, for as long as the dialog is open.
    fn ask(&mut self, dialog: Dialog) {
        if self.dialog.is_none() {
            self.dialog = Some((dialog, Instant::now()));
        }
    }

    /// Always on top, except while a file dialog waits to open (secondary windows too).
    fn on_top(&self) -> bool {
        self.opt.always_on_top && self.dialog.is_none()
    }

    fn pending_dialog(&mut self, ctx: &egui::Context) {
        let Some((_, since)) = &self.dialog else { return };
        let on_top = self.opt.always_on_top;
        if on_top && since.elapsed() < Duration::from_millis(150) {
            // The window level changes when this frame ends; the dialog waits for it.
            set_window_level(ctx, false);
            ctx.request_repaint_after(Duration::from_millis(30));
            return;
        }
        let Some((dialog, _)) = self.dialog.take() else { return };
        match dialog {
            Dialog::AddFolder => {
                if let Some(dir) = file_dialog(self.parent).pick_folder() {
                    self.add_paths(vec![dir], true);
                }
            }
            Dialog::AddFiles => {
                if let Some(files) = file_dialog(self.parent).add_filter("Audio", &["flac", "wav", "aif", "aiff", "aifc", "ape", "shn", "wv", "mp3", "mp2", "m4a", "mka", "ogg", "oga", "mp4", "mkv"]).pick_files() {
                    self.add_paths(files, true);
                }
            }
            Dialog::Dvd => self.pick_dvd_now(),
            Dialog::SaveLog => self.save_log_now(),
            Dialog::TorrentCheck => {
                let (shows, _) = self.selection();
                let mut dialog = file_dialog(self.parent).add_filter("Torrent", &["torrent"]).set_title("Torrent to check against");
                if let Some(dir) = shows.first().and_then(|s| s.root.parent()) {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(torrents) = dialog.pick_files() {
                    self.run_on_selection(Op::TorrentCheck(torrents));
                }
            }
            Dialog::Mp3Dir | Dialog::TorrentDir => {
                if let (Some(dir), Some(draft)) = (file_dialog(self.parent).pick_folder(), self.settings.as_mut()) {
                    let field = if matches!(dialog, Dialog::Mp3Dir) { &mut draft.mp3_dir } else { &mut draft.torrent_dir };
                    *field = dir.display().to_string();
                }
            }
            Dialog::SaveSpectrogram(source) => {
                let stem = source.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let mut dialog = file_dialog(self.parent)
                    .set_title("Save spectrogram")
                    .set_file_name(format!("{stem} - spectrogram.png"))
                    .add_filter("PNG image", &["png"]);
                if let Some(dir) = source.parent() {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(target) = dialog.save_file() {
                    // The window may have been closed meanwhile.
                    match self.spectrograms.iter().find(|s| s.source == source) {
                        Some(spec) => match crate::spectrogram::save_png(&spec.image, &target) {
                            Ok(()) => self.log(format!("Saved {}", target.display()), Level::Ok),
                            Err(e) => self.log(format!("Cannot save {}: {e}", target.display()), Level::Error),
                        },
                        None => self.log("The spectrogram window was closed: nothing saved.", Level::Warn),
                    }
                }
            }
        }
        if on_top {
            set_window_level(ctx, true);
        }
    }

    fn pick_dvd(&mut self) {
        self.ask(Dialog::Dvd);
    }

    fn pick_dvd_now(&mut self) {
        if let Some(dir) = file_dialog(self.parent).set_title("DVD folder (or the folder above VIDEO_TS)").pick_folder() {
            if crate::dvd::find(&dir).is_empty() {
                self.flash("No DVD in this folder (no VOB or IFO file).");
            } else {
                self.add_paths(vec![dir], true);
            }
        }
    }

    fn clear_list(&mut self) {
        self.roots.clear();
        self.excluded.clear();
        self.shows.clear();
        self.info.clear();
        self.status.clear();
        self.source.clear();
        self.reports.clear();
        self.open_reports.clear();
        self.speed.clear();
        self.selected.clear();
        self.collapsed.clear();
        self.anchor = None;
        self.lineage.clear();
        self.list_bytes = 0;
        self.banner = None;
        self.waves.clear();
        self.plots.clear();
    }

    fn rebuild(&mut self) {
        self.fit_columns = true;
        let before: HashSet<PathBuf> = self.shows.iter().map(|s| s.root.clone()).collect();
        self.shows = scan_paths(&self.roots, &self.excluded);
        // Files, shows and their disc folders.
        let existing: HashSet<&Path> =
            self.shows.iter().flat_map(|s| s.files.iter().flat_map(|f| [f.as_path(), f.parent().unwrap_or(f)]).chain(std::iter::once(s.root.as_path()))).collect();
        self.selected.retain(|p| existing.contains(p.as_path()));
        self.list_bytes = self.shows.iter().flat_map(|s| &s.files).filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
        for show in &self.shows {
            if !self.lineage.contains_key(&show.root) {
                self.lineage.insert(show.root.clone(), show_lineage(&show.root));
            }
        }
        // Shows arrive unfolded, track titles visible, unless the collapsed view is on; only
        // the user folds or unfolds them after that.
        if self.opt.collapsed_view {
            let new: Vec<PathBuf> = self.shows.iter().map(|s| s.root.clone()).filter(|r| !before.contains(r)).collect();
            self.collapsed.extend(new);
        }
    }

    fn auto_details(&mut self) {
        if !self.opt.auto_details || self.running.is_some() {
            return;
        }
        let pending: Vec<PathBuf> = self.shows.iter().flat_map(|s| s.files.iter()).filter(|f| !self.info.contains_key(*f)).cloned().collect();
        if pending.is_empty() {
            return;
        }
        if pending.len() > AUTO_DETAILS_LIMIT {
            self.log(format!("{} files added: details not read automatically (more than {AUTO_DETAILS_LIMIT}). Use Details if needed.", pending.len()), Level::Info);
            return;
        }
        self.start(Op::Details { verbose: false }, self.shows.clone(), pending, true);
    }

    /// The rows in view: every show and its files, or with a filter only the files with that
    /// result (and their shows).
    fn rows(&self) -> Vec<Row> {
        let keep = |p: &PathBuf| self.passes_filter(p);
        let mut rows = Vec::new();
        for (si, show) in self.shows.iter().enumerate() {
            let files: Vec<usize> = (0..show.files.len()).filter(|&fi| keep(&show.files[fi])).collect();
            if self.filter.is_some() && files.is_empty() && !keep(&show.root) {
                continue;
            }
            rows.push(Row::Show(si));
            if self.collapsed.contains(&show.root) {
                continue;
            }
            // A show on several discs (CD1, CD2... folders): a row before each disc.
            let discs = show.files.windows(2).any(|w| w[0].parent() != w[1].parent());
            let mut disc = None;
            for fi in files {
                let folder = show.files[fi].parent();
                if discs && folder != disc {
                    rows.push(Row::Disc(si, fi));
                    disc = folder;
                }
                rows.push(Row::File(si, fi));
            }
        }
        rows
    }

    /// The file (or show folder) has the result the list is filtered on, or there is no filter.
    /// While a task runs, the filter keeps only the results it has given.
    fn passes_filter(&self, path: &Path) -> bool {
        let Some(level) = self.filter else { return true };
        let fresh = self.running.as_ref().is_none_or(|r| r.updated.contains(path));
        fresh && self.status.get(path).map(|s| s.1) == Some(level)
    }

    /// A former result, from before the running task, of a file or show it works on: faded.
    fn stale(&self, path: &Path) -> bool {
        self.running.as_ref().is_some_and(|r| (r.files.contains(path) || r.roots.iter().any(|root| root == path)) && !r.updated.contains(path))
    }

    /// Where a show stands (the ring on its row): the share of its files done by the running
    /// task, or the worst result of its files and folder.
    fn ring(&self, show: &Show) -> Option<Ring> {
        if let Some(r) = &self.running {
            let mine: Vec<&PathBuf> = show.files.iter().filter(|f| r.files.contains(*f)).collect();
            if !mine.is_empty() {
                if r.done_files.contains(&show.root) {
                    return Some(Ring::Running(1.0));
                }
                let done = mine.iter().filter(|f| r.done_files.contains(**f)).count();
                return Some(Ring::Running(done as f32 / mine.len() as f32));
            }
        }
        let rank = |l: &Level| match l {
            Level::Error => 3,
            Level::Warn => 2,
            _ => 1,
        };
        show.files
            .iter()
            .chain(std::iter::once(&show.root))
            .filter_map(|f| self.status.get(f).map(|s| s.1))
            .filter(|l| *l != Level::Info)
            .max_by_key(rank)
            .map(Ring::Done)
    }

    /// Files with each result of the last task (while one runs: of that task so far): OK, to
    /// check, with an error.
    fn tally(&self) -> [usize; 3] {
        let mut tally = [0usize; 3];
        let running = self.running.as_ref();
        for f in self.shows.iter().flat_map(|s| &s.files).filter(|f| running.is_none_or(|r| r.updated.contains(*f))) {
            match self.status.get(f).map(|s| s.1) {
                Some(Level::Ok) => tally[0] += 1,
                Some(Level::Warn) => tally[1] += 1,
                Some(Level::Error) => tally[2] += 1,
                _ => {}
            }
        }
        tally
    }

    fn row_path(&self, row: Row) -> &Path {
        match row {
            Row::Show(si) => &self.shows[si].root,
            Row::Disc(si, fi) => self.shows[si].files[fi].parent().unwrap_or(&self.shows[si].root),
            Row::File(si, fi) => &self.shows[si].files[fi],
        }
    }

    /// The row is selected, or for a file its disc.
    fn row_selected(&self, row: Row) -> bool {
        let path = self.row_path(row);
        self.selected.contains(path) || matches!(row, Row::File(..)) && path.parent().is_some_and(|disc| self.selected.contains(disc))
    }

    /// The row of a file or show that just got its result glows, fading: painted across the
    /// list (`band`) by its first cell, under the others.
    fn glow(&self, ui: &egui::Ui, path: &Path, band: egui::Rect) {
        let Some(when) = self.fresh.get(path) else { return };
        let left = 1.0 - when.elapsed().as_secs_f32() / GLOW;
        if left > 0.0 {
            let rect = egui::Rect::from_x_y_ranges(band.x_range(), ui.max_rect().y_range());
            ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(band).rect_filled(rect, 0.0, Palette::now().tint_strong.gamma_multiply(left * left));
        }
    }

    /// What an action works on: the selected shows and files, or without a selection the whole
    /// list. With a filter, only the files it shows (a selected show gives its files shown).
    fn selection(&self) -> (Vec<Show>, Vec<PathBuf>) {
        if self.selected.is_empty() && self.filter.is_none() {
            let files = self.shows.iter().flat_map(|s| s.files.clone()).collect();
            return (self.shows.clone(), files);
        }
        let mut shows = Vec::new();
        let mut files = Vec::new();
        for show in &self.shows {
            let whole = self.selected.is_empty() || self.selected.contains(&show.root);
            let picked: Vec<PathBuf> =
                show.files.iter().filter(|f| (whole || self.selected.contains(*f) || f.parent().is_some_and(|d| self.selected.contains(d))) && self.passes_filter(f)).cloned().collect();
            if !picked.is_empty() {
                shows.push(show.clone());
                files.extend(picked);
            }
        }
        (shows, files)
    }

    /// What the share card of a show says: from the list, its details and the verdicts and
    /// results of this session (a check not run is left out).
    fn card(&self, show: &Show) -> crate::sharecard::Card {
        use crate::ops::plural;
        let (artist, date, place) = crate::tagging::identity(show);
        let artist = if artist.trim().is_empty() { crate::files::show_name(&show.root) } else { artist };
        let when_where = [date.trim(), place.trim()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
        let files = &show.files;
        let infos: Vec<&AudioInfo> = files.iter().filter_map(|f| self.info.get(f)).collect();
        let mut formats: Vec<String> = infos.iter().map(|i| i.label()).collect();
        formats.sort();
        formats.dedup();
        let mut facts: Vec<String> = formats.into_iter().take(2).collect();
        facts.push(plural(files.len(), "track", "tracks"));
        let (seconds, _) = show_length(files, &self.info);
        if seconds > 0.0 {
            let s = libm::round(seconds) as u64;
            facts.push(if s >= 3600 { format!("{} h {:02} min", s / 3600, s % 3600 / 60) } else { format!("{} min {:02} s", s / 60, s % 60) });
        }
        let n = files.len();
        let mut checks = Vec::new();
        // Checksums or decode test: the last task, if it gave every file of the show a result.
        if let Some(task @ ("Verify checksums" | "Decode test")) = self.results_of {
            let levels: Vec<Level> = files.iter().filter_map(|f| self.status.get(f).map(|s| s.1)).collect();
            if levels.len() == n {
                let bad = levels.iter().filter(|l| **l == Level::Error).count();
                let warn = levels.iter().filter(|l| **l == Level::Warn).count();
                let verify = task == "Verify checksums";
                checks.push(match (bad, warn) {
                    (0, 0) if verify => (Level::Ok, "Checksums verified".to_string(), format!("{} match their checksum files", plural(n, "file", "files"))),
                    (0, 0) => (Level::Ok, "Decodes cleanly".to_string(), format!("no error in {}", plural(n, "file", "files"))),
                    (0, w) => (Level::Warn, if verify { "Checksums to check" } else { "Decoding to check" }.to_string(), format!("{w} of {n} files")),
                    (b, _) => (Level::Error, if verify { "Checksum errors" } else { "Decoding errors" }.to_string(), format!("{b} of {n} files")),
                });
            }
        }
        let verdict = |map: &HashMap<PathBuf, (String, Level, String)>| map.get(&show.root).or_else(|| files.first().filter(|_| n == 1).and_then(|f| map.get(f))).cloned();
        if let Some((text, level, _)) = verdict(&self.source) {
            let title = match level {
                Level::Ok => "Lossless origin",
                Level::Warn => "Origin to check",
                Level::Error => "Lossy origin",
                Level::Info => "Origin unclear",
            };
            checks.push((level, title.to_string(), format!("Lossless?: {text}")));
        }
        if let Some((text, level, _)) = verdict(&self.speed) {
            let title = match level {
                Level::Ok => "Right speed",
                Level::Info => "Speed unclear",
                _ => "Speed to check",
            };
            checks.push((level, title.to_string(), format!("Speed?: {text}")));
        }
        let cd = infos.len() == n && infos.iter().all(|i| !i.lossy() && i.bits == 16 && i.rate == 44100 && i.channels == 2);
        if cd {
            let sbe = (0..n).filter(|&fi| !last_of_disc(show, fi) && self.info.get(&files[fi]).and_then(|i| i.sbe()) == Some(true)).count();
            checks.push(if sbe == 0 {
                (Level::Ok, "CD-ready".to_string(), "no gap risk (SBE) before the last track of a disc".to_string())
            } else {
                (Level::Warn, "Gap risk if burned".to_string(), format!("{} with an SBE before the last track of their disc", plural(sbe, "track", "tracks")))
            });
        }
        let label = match (date.trim(), place.trim()) {
            ("", "") => artist.clone(),
            (date, place) => format!("{artist} – {place} {date}").replace("  ", " ").trim().to_string(),
        };
        let track = crate::sharecard::longest(files, |f| self.info.get(f).map(|i| i.seconds()).unwrap_or(0.0)).map(|path| {
            let report = self.reports.get(&(ReportKind::Lossless, path.clone())).filter(|r| !r.measures.is_empty());
            let cutoff = report.and_then(|r| r.cutoff);
            let caption = match (report, cutoff) {
                (Some(_), Some(hz)) => format!("{} – lowpass at {:.1} kHz", file_name(&path), hz / 1000.0),
                (Some(_), None) => format!("{} – full band, no lowpass", file_name(&path)),
                _ => file_name(&path),
            };
            (path, caption, cutoff)
        });
        crate::sharecard::Card {
            root: show.root.clone(),
            artist,
            when_where,
            lineage: self.lineage.get(&show.root).copied().flatten().map(|(kind, _)| kind),
            facts,
            checks,
            label,
            track,
        }
    }

    fn remove_selected(&mut self) {
        if self.running.is_some() {
            return;
        }
        for p in std::mem::take(&mut self.selected) {
            if let Some(pos) = self.roots.iter().position(|r| r == &p) {
                self.roots.remove(pos);
            } else {
                self.excluded.insert(p);
            }
        }
        self.rebuild();
    }

    // ------------------------------------------------------------ tasks

    fn start(&mut self, op: Op, shows: Vec<Show>, files: Vec<PathBuf>, quiet: bool) {
        if self.running.is_some() {
            return;
        }
        if !quiet {
            self.banner = None;
        }
        let without_files = match &op {
            Op::TorrentCheck(_) | Op::Rollback { .. } => true,
            Op::MediaInfo(dvds) => !dvds.is_empty(),
            _ => false,
        };
        if files.is_empty() && !without_files {
            if !quiet {
                self.flash("Add show folders or audio files first (drag and drop them onto the list).");
            }
            return;
        }
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        if !quiet {
            self.log(format!("--- {} ---", op.label()), Level::Info);
        }
        let label = op.label().to_string();
        let name = op.name();
        let open_after = if matches!(op, Op::Torrent(_)) && self.opt.open_after_torrent { shows.iter().map(|s| s.root.clone()).collect() } else { Vec::new() };
        let makes_files = matches!(op, Op::ShareCard(_) | Op::Create | Op::Torrent(_) | Op::Convert { .. } | Op::Mp3(_) | Op::FixSbe | Op::CleanWav | Op::ExtractAudio(_) | Op::Setlist);
        let task_files = files.iter().cloned().collect();
        let sizes: Vec<(f64, f64)> = files
            .iter()
            .map(|f| match self.info.get(f) {
                Some(i) if i.samples > 0 => ((i.samples * i.channels.max(1) as u64) as f64, i.seconds()),
                // Unknown yet: bytes, about two per sample.
                _ => (std::fs::metadata(f).map(|m| m.len() as f64 / 2.0).unwrap_or(1.0), 0.0),
            })
            .collect();
        // A file costs its samples plus a fixed part (the trace searches look at its first minute
        // whatever its length): as much as an average file. Measured against real runs, the time
        // left then stays within a fraction of a minute on the test sets.
        let mean = (sizes.iter().map(|s| s.0).sum::<f64>() / sizes.len().max(1) as f64).max(1.0);
        let work = files.iter().cloned().zip(sizes.into_iter().map(|(w, secs)| (w / mean + 1.0, secs))).collect();
        let roots = shows.iter().map(|s| s.root.clone()).collect();
        ops::spawn(op, shows, files, self.opt.clone(), tx, cancel.clone());
        self.running = Some(Running {
            rx,
            cancel,
            label,
            name,
            done: 0,
            total: 0,
            open_after,
            files: task_files,
            done_files: HashSet::new(),
            updated: HashSet::new(),
            roots,
            tally: [0; 3],
            quiet,
            makes_files,
            started: Instant::now(),
            work,
            eta: None,
        });
    }

    fn run_on_selection(&mut self, op: Op) {
        let (shows, files) = self.selection();
        // To FLAC with float WAVs: read them first, to say what the conversion will do.
        if let Op::Convert { target: Target::Flac, level, round_float: false } = op {
            let float = |p: &PathBuf| match self.info.get(p) {
                Some(info) => info.float,
                None => ops::catch_panic(|| crate::audio::probe(p)).is_ok_and(|i| i.float),
            };
            if self.running.is_none() && files.iter().any(float) {
                self.float_pending = Some(FloatPending { level, shows: shows.clone(), files: files.clone(), scans: Vec::new() });
                self.start(Op::ScanFloat, shows, files, false);
                return;
            }
        }
        self.start(op, shows, files, false);
    }

    /// After the float WAVs were read: convert at once when they all convert bit-exact,
    /// otherwise ask.
    fn float_scanned(&mut self, stopped: bool) {
        let Some(pending) = self.float_pending.take() else { return };
        if stopped {
            return;
        }
        if pending.scans.iter().all(|(_, s)| s.exact_bits.is_some()) {
            let op = Op::Convert { target: Target::Flac, level: pending.level, round_float: false };
            self.start(op, pending.shows, pending.files, false);
        } else {
            self.float_dialog = Some(pending);
        }
    }

    fn float_window(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.float_dialog.take() else { return };
        let exact16 = pending.scans.iter().filter(|(_, s)| s.exact_bits == Some(16)).count();
        let exact24 = pending.scans.iter().filter(|(_, s)| s.exact_bits == Some(24)).count();
        let real: Vec<&(PathBuf, crate::encode::FloatScan)> = pending.scans.iter().filter(|(_, s)| s.exact_bits.is_none()).collect();
        let clipped: Vec<&(PathBuf, crate::encode::FloatScan)> = pending.scans.iter().filter(|(_, s)| s.clipped > 0).collect();
        let others = pending.files.len() - pending.scans.len();
        let (mut round, mut exact_only, mut cancel) = (false, false, false);
        egui::Window::new("32-bit float files")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label("FLAC stores whole numbers only: 32-bit float audio has to be turned into 16-bit or 24-bit samples.");
                ui.add_space(6.0);
                if exact16 + exact24 > 0 {
                    ui.label(colored(
                        format!("{} file(s) only hold 16-bit or 24-bit values: they convert bit-exact.", exact16 + exact24),
                        Level::Ok,
                    ));
                }
                ui.label(colored(
                    format!(
                        "{} file(s) are real float audio: converting them rounds them to 24-bit. The differences stay \
                         below -144 dBFS (inaudible), but the FLAC is not bit-exact; their originals will be kept.",
                        real.len()
                    ),
                    Level::Warn,
                ));
                for (path, scan) in real.iter().take(8) {
                    ui.label(format!("    {}   (peak {:.2} dBFS)", file_name(path), 20.0 * libm::log10(scan.peak.max(1e-12))));
                }
                if real.len() > 8 {
                    ui.label("    ...");
                }
                if !clipped.is_empty() {
                    let total: u64 = clipped.iter().map(|(_, s)| s.clipped).sum();
                    ui.label(colored(
                        format!("{total} sample(s) in {} file(s) go above 0 dBFS: 24-bit cannot hold them, they will be clipped.", clipped.len()),
                        Level::Error,
                    ));
                }
                if others > 0 {
                    ui.label(format!("The {others} other file(s) convert as usual."));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    round = ui.button("Convert all (round the float files)").clicked();
                    if exact16 + exact24 + others > 0 {
                        exact_only = ui.button("Convert only the bit-exact ones").clicked();
                    }
                    cancel = ui.button("Cancel").clicked();
                });
            });
        cancel |= ctx.input(|i| i.key_pressed(Key::Escape));
        if round || exact_only {
            let mut files = pending.files.clone();
            if exact_only {
                files.retain(|f| !real.iter().any(|(p, _)| p == f));
            }
            let op = Op::Convert { target: Target::Flac, level: pending.level, round_float: round };
            let shows = pending.shows.clone();
            self.start(op, shows, files, false);
        } else if !cancel {
            self.float_dialog = Some(pending);
        }
    }

    fn stop(&mut self) {
        if let Some(r) = &self.running {
            r.cancel.store(true, Ordering::SeqCst);
            self.flash("Stopping...");
        }
    }

    fn poll(&mut self) {
        let mut finished = None;
        if let Some(running) = &mut self.running {
            let started = Instant::now();
            while let Ok(msg) = running.rx.try_recv() {
                match msg {
                    Msg::Log(text, level) => {
                        if level == Level::Error {
                            self.opt.log_open = true;
                        }
                        self.log.push((text, level));
                    }
                    Msg::Status { path, text, level, tooltip } => {
                        match level {
                            Level::Ok => running.tally[0] += 1,
                            Level::Warn => running.tally[1] += 1,
                            Level::Error => running.tally[2] += 1,
                            Level::Info => {}
                        }
                        running.done_files.insert(path.clone());
                        running.updated.insert(path.clone());
                        if !running.quiet {
                            self.fresh.insert(path.clone(), Instant::now());
                        }
                        self.status.insert(path, (text, level, tooltip));
                    }
                    Msg::Info { path, info } => {
                        self.info.insert(path, info);
                    }
                    Msg::Wave { path, peaks } => {
                        self.waves.insert(path, peaks);
                    }
                    Msg::Source { path, text, level, tooltip, report } => {
                        // Reading the files again (Details) must not replace a measured verdict.
                        if report.measures.is_empty() && self.reports.get(&(ReportKind::Lossless, path.clone())).is_some_and(|r| !r.measures.is_empty()) {
                            continue;
                        }
                        self.reports.insert((ReportKind::Lossless, path.clone()), report);
                        running.done_files.insert(path.clone());
                        self.fresh.insert(path.clone(), Instant::now());
                        self.source.insert(path, (text, level, tooltip));
                    }
                    Msg::Speed { path, text, level, tooltip, report } => {
                        self.reports.insert((ReportKind::Speed, path.clone()), report);
                        running.done_files.insert(path.clone());
                        self.fresh.insert(path.clone(), Instant::now());
                        self.speed.insert(path, (text, level, tooltip));
                    }
                    Msg::Image { path, image } => {
                        self.spectrograms.retain(|s| s.source != path);
                        self.spectrograms.push(Spectrogram { source: path, image, texture: None });
                    }
                    Msg::Progress { done, total } => {
                        running.done = done;
                        running.total = total;
                    }
                    Msg::Converted { target, sources, failed, rounded } => {
                        self.converted = Some((target, sources, failed, rounded));
                    }
                    Msg::MediaInfo { title, text } => {
                        self.media_windows.retain(|(t, _)| t != &title);
                        self.media_windows.push((title, text));
                    }
                    Msg::SetlistChoice { root, shows } => {
                        self.setlist_choices.retain(|c| c.root != root);
                        self.setlist_choices.push(SetlistChoice { root, shows, pick: None });
                    }
                    Msg::FloatScan(found) => {
                        if let Some(pending) = &mut self.float_pending {
                            pending.scans = found;
                        }
                    }
                    Msg::Finished { summary, rescan } => {
                        finished = Some((summary, rescan));
                        break;
                    }
                }
                if started.elapsed() > Duration::from_millis(30) {
                    break; // keep the window responsive
                }
            }
        }
        if let Some((summary, rescan)) = finished {
            let ended = self.running.take();
            if let Some(running) = &ended {
                self.open_for_upload(&running.open_after);
                if !running.updated.is_empty() {
                    self.results_of = Some(running.name);
                }
            }
            if self.float_pending.is_some() {
                self.flash(summary.clone());
                self.float_scanned(summary == "Stopped.");
                return;
            }
            self.fit_columns = true;
            self.log(summary.clone(), Level::Info);
            // After a conversion to FLAC: offer to tidy up (before the list changes).
            if let Some((Target::Flac, sources, failed, rounded)) = self.converted.take() {
                if (!sources.is_empty() || rounded > 0) && summary != "Stopped." {
                    let roots: Vec<PathBuf> =
                        self.shows.iter().filter(|s| s.files.iter().any(|f| sources.contains(f))).map(|s| s.root.clone()).collect();
                    let extensions: HashSet<String> = sources
                        .iter()
                        .filter_map(|p| p.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())))
                        .collect();
                    let old = ops::old_checksum_files(&roots, &extensions);
                    self.after_flac = Some(AfterFlac {
                        roots,
                        failed,
                        rounded,
                        trash: self.opt.after_flac_trash,
                        trash_old: self.opt.after_flac_old,
                        ffp: self.opt.after_flac_ffp,
                        old,
                        sources,
                    });
                }
            }
            match ended.filter(|r| !r.quiet) {
                Some(running) => self.notify(&running, &summary),
                None => self.flash(summary),
            }
            if rescan {
                self.rebuild();
                // With the after-FLAC window open, read the details once it is closed.
                if self.after_flac.is_none() {
                    self.auto_details();
                }
            }
            // .torrent files dropped with folders: checked once the folders are read.
            if self.running.is_none() {
                if let Some(torrents) = self.pending_torrents.take() {
                    self.run_on_selection(Op::TorrentCheck(torrents));
                }
            }
        }
    }

    fn log(&mut self, text: impl Into<String>, level: Level) {
        if level == Level::Error {
            self.opt.log_open = true;
        }
        self.log.push((text.into(), level));
    }

    fn flash(&mut self, text: impl Into<String>) {
        self.flash = Some((text.into(), Instant::now()));
    }

    /// The notification of a task that ended.
    fn notify(&mut self, running: &Running, summary: &str) {
        let [_, warn, error] = running.tally;
        let stopped = summary == "Stopped.";
        let (level, outcome, filter) = if stopped {
            (Level::Info, "Stopped".to_string(), None)
        } else if error > 0 {
            (Level::Error, format!("{error} with an error"), Some((Level::Error, error)))
        } else if warn > 0 {
            (Level::Warn, format!("{warn} to check"), Some((Level::Warn, warn)))
        } else {
            (Level::Ok, "Done".to_string(), None)
        };
        // A long task (the user may have walked away): a band above the list that stays.
        let took = running.started.elapsed();
        if took >= Duration::from_secs(60) && !stopped {
            let files: usize = running.tally.iter().sum();
            let title = match (level, files) {
                (Level::Ok, 0) => "Done".to_string(),
                (Level::Ok, n) => {
                    let all = if n == 1 { "The file".to_string() } else { format!("All {n} files") };
                    match running.name {
                        "Lossless?" => format!("{all} lossless"),
                        "Verify checksums" => format!("{all} match their checksums"),
                        "Decode test" => format!("{all} decode without error"),
                        _ => format!("{all} OK"),
                    }
                }
                _ => [(error, "with an error"), (warn, "to check")]
                    .iter()
                    .filter(|(n, _)| *n > 0)
                    .map(|(n, what)| format!("{} {what}", crate::ops::plural(*n, "file", "files")))
                    .collect::<Vec<_>>()
                    .join(", "),
            };
            let seconds = took.as_secs();
            let duration = if seconds >= 3600 {
                format!("{} h {:02} min", seconds / 3600, seconds % 3600 / 60)
            } else {
                format!("{} min {:02} s", seconds / 60, seconds % 60)
            };
            let detail = format!("{}: {} in {duration}. {summary}", running.label, crate::ops::plural(running.roots.len(), "show", "shows"));
            self.banner = Some(Banner { level, title, detail, roots: running.roots.clone(), filter });
            return;
        }
        // "Open folder": the show's, or for a share card (beside it) the card itself, selected.
        let folder = (running.makes_files && !stopped).then(|| running.roots.first().cloned()).flatten();
        let folder = if running.name == "Share card" { folder.map(|root| crate::sharecard::path_of(&root)) } else { folder };
        // A few at a time: the oldest that would close by itself goes first.
        if self.toasts.len() >= 3 {
            let index = self.toasts.iter().position(|t| t.life().is_some()).unwrap_or(0);
            self.toasts.remove(index);
        }
        self.toasts_made += 1;
        self.toasts.push(Toast {
            id: self.toasts_made,
            title: running.label.clone(),
            outcome,
            detail: summary.to_string(),
            level,
            age: Duration::ZERO,
            seen: Instant::now(),
            born: Instant::now(),
            folder,
            filter,
        });
    }

    /// Opens the .ffp and the info file of each show that got its torrent, in the system's
    /// text editor, to copy them into the upload form.
    fn open_for_upload(&mut self, roots: &[PathBuf]) {
        const MAX_SHOWS: usize = 5;
        let made: Vec<&PathBuf> = roots
            .iter()
            .filter(|r| self.status.get(*r).is_some_and(|(text, level, _)| *level == Level::Ok && text.starts_with("Torrent created")))
            .collect();
        if made.len() > MAX_SHOWS {
            self.log(format!("{} shows: their .ffp and info files are not opened (only up to {MAX_SHOWS} at a time).", made.len()), Level::Info);
            return;
        }
        for root in made {
            let name = crate::files::show_name(root);
            let ffp = crate::files::all_files(root, true).into_iter().find(|p| crate::files::ext_lower(p) == "ffp");
            let info = ops::info_candidates(root).into_iter().next();
            for (what, file) in [("no .ffp file", ffp), ("no info file", info)] {
                match file {
                    Some(f) => {
                        if let Err(e) = open::that_detached(&f) {
                            self.log(format!("Cannot open {}: {e}", f.display()), Level::Warn);
                        }
                    }
                    None => self.log(format!("{name}: {what} to open"), Level::Info),
                }
            }
        }
    }

    fn copy_ffp(&mut self, ctx: &egui::Context) {
        let (shows, _) = self.selection();
        if shows.len() != 1 {
            self.flash("Select a single show to copy its FFP.");
            return;
        }
        let show = shows[0].clone();
        let name = crate::files::show_name(&show.root);
        if let Some(text) = ops::ffp_text(&show.root) {
            ctx.copy_text(text);
            let msg = format!("FFP of {name} copied to the clipboard.");
            self.log(msg.clone(), Level::Ok);
            self.flash(msg);
            return;
        }
        // No .ffp file: the same lines, made from the MD5 signatures in the FLAC headers.
        match ops::ffp_from_headers(&show) {
            Ok((text, count, unsigned)) => {
                ctx.copy_text(text);
                let msg = format!("No .ffp file in {name}: FFP of its {count} FLAC file(s) made from their headers and copied to the clipboard.");
                if unsigned > 0 {
                    self.log(format!("{msg} {unsigned} file(s) have no MD5 signature: their lines are all zeros (re-encode them)."), Level::Warn);
                } else {
                    self.log(msg.clone(), Level::Ok);
                }
                self.flash(msg);
            }
            Err(e) => {
                let msg = format!("Nothing copied: no .ffp file in {name}, and {e}.");
                self.log(msg.clone(), Level::Warn);
                self.flash(msg);
            }
        }
    }

    fn copy_info(&mut self, ctx: &egui::Context) {
        let (shows, _) = self.selection();
        if shows.len() != 1 {
            self.flash("Select a single show to copy its info file.");
            return;
        }
        let root = shows[0].root.clone();
        let name = crate::files::show_name(&root);
        let candidates = ops::info_candidates(&root);
        match candidates.len() {
            0 => {
                let msg = format!("Nothing copied: no info file (.txt or .nfo) in {name}.");
                self.log(msg.clone(), Level::Warn);
                self.flash(msg);
            }
            1 => self.copy_info_file(ctx, &root, &candidates[0]),
            _ => {
                let texts = candidates.iter().map(|p| crate::files::read_text(p).unwrap_or_else(|e| format!("(cannot read: {e})"))).collect();
                self.info_choice = Some(InfoChoice { root, candidates, texts, chosen: 0 });
            }
        }
    }

    fn copy_info_file(&mut self, ctx: &egui::Context, root: &std::path::Path, path: &std::path::Path) {
        match crate::files::read_text(path) {
            Ok(text) => {
                ctx.copy_text(text);
                let msg = format!("Info file of {} copied to the clipboard: {}", crate::files::show_name(root), rel_posix(path, root));
                self.log(msg.clone(), Level::Ok);
                self.flash(msg);
            }
            Err(e) => self.log(format!("Cannot read {}: {e}", path.display()), Level::Error),
        }
    }

    /// Several possible info files: the user picks one, with a preview.
    fn info_window(&mut self, ctx: &egui::Context) {
        let Some(mut choice) = self.info_choice.take() else { return };
        let (mut copy, mut close) = (false, false);
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                choice.chosen = (choice.chosen + 1).min(choice.candidates.len() - 1);
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                choice.chosen = choice.chosen.saturating_sub(1);
            }
            copy |= i.consume_key(Modifiers::NONE, Key::Enter);
            close |= i.consume_key(Modifiers::NONE, Key::Escape);
        });
        let mut open = true;
        egui::Window::new("Copy info file")
            .collapsible(false)
            .default_size([640.0, 420.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("{} has several text files. Which one is the info file?", file_name(&choice.root)));
                ui.add_space(4.0);
                for (i, path) in choice.candidates.iter().enumerate() {
                    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                    let label = format!("{}   ({:.1} KB)", rel_posix(path, &choice.root), size as f64 / 1024.0);
                    if ui.radio(choice.chosen == i, label).clicked() {
                        choice.chosen = i;
                    }
                }
                ui.add_space(6.0);
                egui::TopBottomPanel::bottom("info_buttons").show_inside(ui, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        copy |= ui.button("Copy").clicked();
                        close |= ui.button("Cancel").clicked();
                        ui.label(RichText::new("↑ ↓ to choose, Enter to copy").weak());
                    });
                });
                egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(&choice.texts[choice.chosen]).monospace()).extend());
                });
            });
        if copy {
            let path = choice.candidates[choice.chosen].clone();
            self.copy_info_file(ctx, &choice.root, &path);
        } else if open && !close {
            self.info_choice = Some(choice);
        }
    }

    fn open_tag_dialog(&mut self) {
        let (shows, _) = self.selection();
        if shows.len() > 1 {
            self.open_tag_batch(shows);
            return;
        }
        let Some(show) = shows.into_iter().next() else {
            self.flash("Select a show to tag it from its info file.");
            return;
        };
        let name = crate::files::show_name(&show.root);
        match TagDialog::prepare(&show, &self.opt, !self.opt.setlistfm_key.trim().is_empty()) {
            Ok(d) => self.tag_dialog = Some(d),
            Err(e) => {
                let msg = format!("{name}: {e}, nothing to read the tags from.");
                self.log(msg.clone(), Level::Warn);
                self.flash(msg);
            }
        }
    }

    /// Prepares the plan of every show (in parallel), the clear ones first and ticked.
    fn open_tag_batch(&mut self, shows: Vec<Show>) {
        use rayon::prelude::*;
        let opt = self.opt.clone();
        let mut rows: Vec<BatchRow> = shows
            .par_iter()
            .map(|show| {
                let row = BatchRow { show: show.clone(), plan: None, problem: String::new(), doubts: Vec::new(), checked: false, ticked: false, applied: false };
                match TagDialog::prepare(show, &opt, false) {
                    Ok(plan) => {
                        let doubts = crate::tagging::doubts(show, &plan.draft, &plan.infos);
                        let ticked = doubts.is_empty() && plan.blocked(opt.tag_scheme).is_none();
                        BatchRow { plan: Some(plan), doubts, ticked, ..row }
                    }
                    Err(e) => BatchRow { problem: e, ..row },
                }
            })
            .collect();
        // Clear, then to check, then those that cannot be tagged; in the order of the list within each.
        rows.sort_by_key(|r| if r.plan.is_none() { 2 } else if r.doubts.is_empty() { 0 } else { 1 });
        self.tag_batch = Some(TagBatch { rows });
    }

    /// Opens the next row to check of the batch in the Tag and rename window.
    fn review_next(&mut self, after: Option<usize>) {
        let Some(batch) = self.tag_batch.as_ref() else { return };
        let start = after.map_or(0, |i| i + 1);
        let next = (start..batch.rows.len()).chain(0..start).find(|&i| {
            let r = &batch.rows[i];
            r.plan.is_some() && !r.applied && !r.checked && !r.doubts.is_empty()
        });
        self.review(next);
    }

    fn review(&mut self, row: Option<usize>) {
        let Some(i) = row else {
            self.tag_dialog = None;
            return;
        };
        if let Some(plan) = self.tag_batch.as_ref().and_then(|b| b.rows[i].plan.clone()) {
            self.tag_dialog = Some(TagDialog { batch_row: Some(i), ..plan });
        }
    }

    fn tag_batch_window(&mut self, ctx: &egui::Context) {
        use crate::tagging::NameScheme;
        let Some(mut b) = self.tag_batch.take() else { return };
        let (mut open, mut close, mut apply) = (true, false, false);
        let mut review = None;
        let mut next = false;
        let mut scheme = self.opt.tag_scheme;
        let busy = self.running.is_some();
        let id = egui::ViewportId::from_hash_of("tag batch");
        let title = "Tag and rename several shows";
        let builder = egui::ViewportBuilder::default()
            .with_icon(app_icon())
            .with_window_level(window_level(self.on_top()))
            .with_title(title)
            .with_inner_size([1100.0, 640.0])
            .with_min_inner_size([600.0, 360.0]);
        ctx.show_viewport_immediate(id, builder, |vctx, class| {
            let content = |ui: &mut egui::Ui| {
                let live = |r: &BatchRow| r.plan.is_some() && !r.applied;
                let clear = b.rows.iter().filter(|r| live(r) && (r.doubts.is_empty() || r.checked)).count();
                let to_check = b.rows.iter().filter(|r| live(r) && !r.doubts.is_empty() && !r.checked).count();
                let cannot = b.rows.iter().filter(|r| r.plan.is_none()).count();
                ui.label(format!(
                    "{} shows: {clear} clear, {to_check} to check, {cannot} that cannot be tagged. The clear ones are ticked; \
                     check the others in the Tag and rename window (Check) before ticking them. Nothing is written before Apply, \
                     and Tools > Undo tagging gives the files back as they were.",
                    b.rows.len()
                ));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("File names");
                    egui::ComboBox::from_id_salt("batch_scheme").width(240.0).selected_text(scheme.label()).show_ui(ui, |ui| {
                        for s in NameScheme::ALL {
                            ui.selectable_value(&mut scheme, s, s.label());
                        }
                    });
                    ui.separator();
                    ui.label("For every show:");
                    for (label, tags) in [("Write the tags", true), ("Rename the files", false)] {
                        let rows = b.rows.iter().filter_map(|r| r.plan.as_ref().filter(|_| !r.applied));
                        let mut all = rows.clone().all(|p| if tags { p.write_tags } else { p.rename });
                        if ui.checkbox(&mut all, label).changed() {
                            for p in b.rows.iter_mut().filter(|r| !r.applied).filter_map(|r| r.plan.as_mut()) {
                                if tags { p.write_tags = all } else { p.rename = all }
                            }
                        }
                    }
                });
                egui::TopBottomPanel::bottom("batch_buttons").show_inside(ui, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let n = b.rows.iter().filter(|r| r.ticked && !r.applied && r.plan.as_ref().is_some_and(|p| p.blocked(scheme).is_none())).count();
                        apply = ui.add_enabled(n > 0 && !busy, egui::Button::new(RichText::new(format!("Apply to the {} ticked", ops::plural(n, "show", "shows"))).strong())).clicked();
                        next = ui.add_enabled(to_check > 0, egui::Button::new(format!("Check the next show ({to_check} left)"))).clicked();
                        close = ui.button("Close").clicked();
                        if busy {
                            ui.label(RichText::new("Wait for the running task to finish.").weak());
                        }
                    });
                });
                egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                    egui::Grid::new("batch_rows").num_columns(8).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
                        for h in ["", "Show", "Artist", "Date", "Titles", "Tags", "Rename", "Status"] {
                            ui.strong(h);
                        }
                        ui.end_row();
                        for (i, r) in b.rows.iter_mut().enumerate() {
                            let blocked = r.plan.as_ref().and_then(|p| p.blocked(scheme));
                            ui.add_enabled_ui(r.plan.is_some() && !r.applied && blocked.is_none(), |ui| ui.checkbox(&mut r.ticked, ""));
                            ui.label(crate::files::show_name(&r.show.root)).on_hover_text(r.show.root.display().to_string());
                            match r.plan.as_mut() {
                                Some(p) => {
                                    ui.label(&p.draft.artist);
                                    ui.label(&p.draft.date);
                                    ui.label(format!("{}/{}", p.draft.found, p.draft.tracks.len()));
                                    ui.add_enabled_ui(!r.applied, |ui| ui.checkbox(&mut p.write_tags, ""));
                                    ui.add_enabled_ui(!r.applied, |ui| ui.checkbox(&mut p.rename, ""));
                                }
                                None => {
                                    for _ in 0..5 {
                                        ui.label("");
                                    }
                                }
                            }
                            ui.horizontal(|ui| {
                                let (text, level) = if r.applied {
                                    ("Applied".to_string(), Level::Info)
                                } else if r.plan.is_none() {
                                    (format!("Cannot be tagged: {}", r.problem), Level::Error)
                                } else if let Some(why) = &blocked {
                                    (format!("Not ready: {why}"), Level::Error)
                                } else if r.checked {
                                    ("Checked".to_string(), Level::Ok)
                                } else if r.doubts.is_empty() {
                                    ("Clear".to_string(), Level::Ok)
                                } else {
                                    (format!("To check: {}", r.doubts.join("; ")), Level::Warn)
                                };
                                if r.plan.is_some() && !r.applied && ui.small_button("Check...").on_hover_text("Open it in the Tag and rename window").clicked() {
                                    review = Some(i);
                                }
                                if r.applied { ui.label(RichText::new(text).weak()) } else { ui.label(colored(text, level)) };
                            });
                            ui.end_row();
                        }
                    });
                });
            };
            if class == egui::ViewportClass::Embedded {
                egui::Window::new(title).id(egui::Id::new(id)).collapsible(false).default_size([1080.0, 560.0]).open(&mut open).show(vctx, content);
            } else {
                egui::CentralPanel::default().show(vctx, content);
                if vctx.input(|i| i.viewport().close_requested()) {
                    open = false;
                }
            }
        });
        self.opt.tag_scheme = scheme;
        if apply {
            let ready: Vec<usize> = (0..b.rows.len())
                .filter(|&i| b.rows[i].ticked && !b.rows[i].applied && b.rows[i].plan.as_ref().is_some_and(|p| p.blocked(scheme).is_none()))
                .collect();
            let jobs = ready.iter().filter_map(|&i| b.rows[i].plan.as_ref()).map(|p| p.job(scheme)).collect();
            let shows: Vec<Show> = ready.iter().map(|&i| b.rows[i].show.clone()).collect();
            let files = shows.iter().flat_map(|s| s.files.clone()).collect();
            for &i in &ready {
                b.rows[i].applied = true;
            }
            self.start(Op::Tag(jobs), shows, files, false);
        }
        if !open || close {
            // The window of a row of this batch goes with it.
            if self.tag_dialog.as_ref().is_some_and(|d| d.batch_row.is_some()) {
                self.tag_dialog = None;
            }
            return;
        }
        self.tag_batch = Some(b);
        if next {
            self.review_next(None);
        } else if review.is_some() {
            self.review(review);
        }
    }

    fn tag_window(&mut self, ctx: &egui::Context) {
        use crate::tagging::NameScheme;
        let Some(mut d) = self.tag_dialog.take() else { return };
        let (mut apply, mut cancel, mut then_next) = (false, false, false);
        // The setlist.fm search, once it has answered.
        if let Some(rx) = &d.setlist_search {
            let answer = rx.lock().map(|rx| rx.try_recv()).unwrap_or(Err(mpsc::TryRecvError::Disconnected));
            match answer {
                Ok(_) if d.setlist_asked != (d.draft.artist.trim().to_string(), d.draft.date.trim().to_string()) => {
                    d.setlist_search = None;
                    d.setlist_note = Some(("The artist or the date changed while setlist.fm was asked: ask again.".into(), Level::Warn));
                }
                Ok(answer) => {
                    d.setlist_search = None;
                    match answer {
                        Ok(shows) if shows.is_empty() => d.setlist_note = Some((format!("{} on {}: not on setlist.fm.", d.draft.artist.trim(), d.draft.date.trim()), Level::Warn)),
                        Ok(shows) => {
                            let pick = if shows.len() == 1 { shows.first() } else { crate::setlistfm::pick_by_place(&d.draft.place, &shows) };
                            match pick.cloned() {
                                Some(show) => d.use_setlist(&show),
                                None => {
                                    d.setlist_note = Some((format!("setlist.fm has {} shows of {} that day: choose one.", shows.len(), d.draft.artist.trim()), Level::Warn));
                                    d.setlist_shows = shows;
                                }
                            }
                        }
                        Err(e) => d.setlist_note = Some((format!("setlist.fm: {e}."), Level::Error)),
                    }
                }
                Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(100)),
                Err(mpsc::TryRecvError::Disconnected) => d.setlist_search = None,
            }
        }
        let mut ask_setlist = false;
        let mut chosen_setlist = None;
        let setlist_key = self.opt.setlistfm_key.trim().to_string();
        // Checking a show of a batch: nothing is written here, the plan goes back to its row.
        let in_batch = d.batch_row.is_some();
        let mut reload = None;
        let mut open = true;
        let mut scheme = self.opt.tag_scheme;
        let busy = self.running.is_some();
        let root = d.draft.root.clone();
        let info_label = |(path, titles): &(PathBuf, usize)| format!("{}   ({} titles)", rel_posix(path, &root), titles);
        let conflicts = d.draft.name_conflicts(if d.rename { scheme } else { NameScheme::Keep });
        // Renaming only, to the names the files already have: nothing would change.
        let same_names = !d.write_tags && d.rename && d.draft.tracks.iter().all(|t| d.draft.new_name(t, scheme).is_none());
        let files = d.draft.tracks.len();
        let not_flac = d.draft.tracks.iter().filter(|t| !crate::tagging::taggable(&t.path)).count();
        // A system window of its own: it may be larger than the main window and go past it.
        let id = egui::ViewportId::from_hash_of("tag and rename");
        let title = "Tag and rename from info file";
        let builder = egui::ViewportBuilder::default()
            .with_icon(app_icon())
            .with_window_level(window_level(self.on_top()))
            .with_title(title)
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([520.0, 360.0]);
        ctx.show_viewport_immediate(id, builder, |vctx, class| {
            let content = |ui: &mut egui::Ui| {
                    ui.label(RichText::new(crate::files::show_name(&root)).strong());
                    ui.add_space(4.0);
                    egui::Grid::new("tag_fields").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("Info file");
                        if d.infos.is_empty() {
                            ui.label(RichText::new("None: the titles can come from setlist.fm").weak());
                        } else {
                            egui::ComboBox::from_id_salt("tag_info").width(560.0).selected_text(info_label(&d.infos[d.info])).show_ui(ui, |ui| {
                                for (i, info) in d.infos.iter().enumerate() {
                                    if ui.selectable_label(i == d.info, info_label(info)).clicked() {
                                        reload = Some(i);
                                    }
                                }
                            });
                        }
                        ui.end_row();
                        ui.label("Artist");
                        ui.add(egui::TextEdit::singleline(&mut d.draft.artist).desired_width(560.0));
                        ui.end_row();
                        ui.label("Date");
                        let date = ui.add(egui::TextEdit::singleline(&mut d.draft.date).hint_text("YYYY-MM-DD").desired_width(120.0));
                        ui.end_row();
                        ui.label("Place");
                        let place = ui.add(egui::TextEdit::singleline(&mut d.draft.place).hint_text("Venue, City, Country").desired_width(560.0));
                        ui.end_row();
                        ui.label("Album");
                        let album = ui.add(egui::TextEdit::singleline(&mut d.draft.album).desired_width(560.0))
                            .on_hover_text("The date and the place by default (the eTree custom); edit it to write something else.");
                        ui.end_row();
                        if album.changed() {
                            d.album_auto = false;
                        }
                        if d.album_auto && (date.changed() || place.changed()) {
                            d.draft.album = format!("{} {}", d.draft.date.trim(), d.draft.place.trim()).trim().to_string();
                        }
                        ui.label("Change");
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut d.write_tags, "Write the tags").on_hover_text("Untick it to rename the files only: their content is not touched.");
                            ui.checkbox(&mut d.rename, "Rename the files").on_hover_text("Untick it to write the tags only: the names stay as they are.");
                        });
                        ui.end_row();
                        ui.label("File names");
                        ui.add_enabled_ui(d.rename, |ui| {
                            egui::ComboBox::from_id_salt("tag_scheme").width(240.0).selected_text(scheme.label()).show_ui(ui, |ui| {
                                for s in NameScheme::ALL {
                                    ui.selectable_value(&mut scheme, s, s.label());
                                }
                            });
                        });
                        ui.end_row();
                    });
                    if d.whole_checksums && d.write_tags {
                        ui.checkbox(&mut d.update_checksums, "Update the .md5 and .sfv files (whole-file checksums change with the tags)");
                    }
                    ui.label(RichText::new("The audio is not touched: FFP and ST5 stay valid. Renamed files are renamed in every checksum file.").weak());
                    // Titles from setlist.fm: the search, then the choice when there are several shows.
                    ui.horizontal(|ui| {
                        let searching = d.setlist_search.is_some();
                        let tip = if setlist_key.is_empty() {
                            "Enter your setlist.fm API key in Settings first (a free key, from setlist.fm)."
                        } else {
                            "Ask setlist.fm for the show of this artist on this date: its songs become the titles, in order, and its venue the place."
                        };
                        let ready = !setlist_key.is_empty() && !searching && !d.draft.artist.trim().is_empty() && crate::tagging::full_date(&d.draft.date);
                        ask_setlist = ui.add_enabled(ready, egui::Button::new("Titles from setlist.fm")).on_hover_text(tip).on_disabled_hover_text(tip).clicked();
                        if searching {
                            ui.spinner();
                            ui.label(RichText::new("Asking setlist.fm...").weak());
                        } else if let Some((text, level)) = &d.setlist_note {
                            ui.label(colored(text.clone(), *level));
                        }
                    });
                    for (i, show) in d.setlist_shows.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.button("Use").clicked() {
                                chosen_setlist = Some(i);
                            }
                            let tour = if show.tour.is_empty() { String::new() } else { format!(", {}", show.tour) };
                            ui.label(format!("{}   ({} songs{tour})", show.location(), show.songs.len()));
                            if !show.url.is_empty() {
                                ui.hyperlink_to("setlist.fm", &show.url);
                            }
                        });
                    }
                    if d.draft.titles_from_names {
                        ui.label(colored("No tracklist in this info file: the titles below come from the file names. Check them.", Level::Warn));
                    } else if d.draft.setlist && d.draft.found != files {
                        ui.label(colored(format!(
                            "setlist.fm lists {} songs for {files} files: check that each title is on the right file (intros, solos and medleys are often split or joined).",
                            d.draft.found
                        ), Level::Warn));
                    } else if d.infos.is_empty() && !d.draft.setlist {
                        ui.label(colored("No info file: type the titles below, or get them from setlist.fm.", Level::Warn));
                    } else if d.draft.found != files {
                        let text = if d.draft.found == 0 {
                            "No numbered tracklist found in this info file: type the titles below, or choose another file.".to_string()
                        } else {
                            format!("The info file lists {} titles for {files} files: check that each title is on the right file.", d.draft.found)
                        };
                        ui.label(colored(text, Level::Warn));
                    }
                    // Titles from the file names, for the files whose name has one.
                    let named: Vec<Option<String>> = d.draft.tracks.iter().map(|t| crate::tagging::title_from_name(&t.path)).collect();
                    let usable = named.iter().flatten().count();
                    if usable > 0 && named.iter().zip(&d.draft.tracks).any(|(n, t)| n.as_ref().is_some_and(|n| *n != t.title)) {
                        let tip = format!("\"01 - Title.flac\" gives \"Title\". {usable} of the {files} file names have a title; the other titles stay as they are.");
                        if ui.button("Titles from file names").on_hover_text(tip).clicked() {
                            for (t, n) in d.draft.tracks.iter_mut().zip(named) {
                                if let Some(n) = n {
                                    t.title = n;
                                }
                            }
                        }
                    }
                    // Titles in capitals: "IT'S SO EASY" -> "It's So Easy", on request only.
                    if d.draft.tracks.iter().any(|t| crate::tagging::title_case(&t.title) != t.title) {
                        let tip = "\"IT'S SO EASY\" becomes \"It's So Easy\". Only words written in capitals change; initials (N.I.B.), Roman numerals (II) and AC/DC are kept.";
                        if ui.button("Title Case").on_hover_text(tip).clicked() {
                            for t in d.draft.tracks.iter_mut() {
                                t.title = crate::tagging::title_case(&t.title);
                            }
                        }
                    }
                    if !d.draft.date.trim().is_empty() && !crate::tagging::full_date(&d.draft.date) {
                        ui.label(colored("The date is not written YYYY-MM-DD (1986-05-17, for instance): players sort and show dates best that way.", Level::Warn));
                    }
                    if let Some(folder) = d.draft.folder_date.as_ref().filter(|f| **f != d.draft.date.trim()) {
                        ui.label(colored(format!("The folder name says {folder}: check the date."), Level::Warn));
                    }
                    if !d.draft.extra.is_empty() {
                        ui.label(colored(format!("Not used (no file left for them): {}", d.draft.extra.join(" / ")), Level::Warn));
                    }
                    if not_flac > 0 {
                        ui.label(colored(format!("{not_flac} file(s) are not FLAC: they can be renamed, not tagged."), Level::Warn));
                    }
                    for c in &conflicts {
                        ui.label(colored(format!("Cannot rename: {c}."), Level::Error));
                    }
                    ui.add_space(4.0);
                    egui::TopBottomPanel::bottom("tag_buttons").show_inside(ui, |ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            let ready = conflicts.is_empty() && (!busy || in_batch) && (d.write_tags || d.rename) && !same_names;
                            if in_batch {
                                let tip = "Nothing is written yet: the show is ticked in the batch window, written with the others by its Apply.";
                                then_next = ui.add_enabled(ready, egui::Button::new(RichText::new("OK, next show to check").strong())).on_hover_text(tip).clicked();
                                apply |= then_next || ui.add_enabled(ready, egui::Button::new("OK")).on_hover_text(tip).clicked();
                            } else {
                                apply |= ui.add_enabled(ready, egui::Button::new(RichText::new("Apply").strong())).clicked();
                            }
                            cancel |= ui.button("Cancel").clicked();
                            if busy && !in_batch {
                                ui.label(RichText::new("Wait for the running task to finish.").weak());
                            } else if !d.write_tags && !d.rename {
                                ui.label(RichText::new("Tick Write the tags, Rename the files or both.").weak());
                            } else if same_names {
                                ui.label(RichText::new("Nothing to change: the files already have these names. Choose another file name format, or tick Write the tags.").weak());
                            }
                        });
                    });
                    let width = d.draft.width();
                    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                        egui::Grid::new("tag_tracks").num_columns(4).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
                            ui.strong("#");
                            ui.strong("File");
                            ui.strong("Title");
                            ui.strong("New name");
                            ui.end_row();
                            let artist = d.draft.artist.trim().to_string();
                            let date = d.draft.date.trim().to_string();
                            let shown = if d.rename { scheme } else { NameScheme::Keep };
                            for t in d.draft.tracks.iter_mut() {
                                ui.label(match t.disc {
                                    Some(disc) => format!("d{disc}t{:02}", t.track),
                                    None => format!("{:02}", t.track),
                                });
                                ui.label(rel_posix(&t.path, &root));
                                // Sized by hand: inside a grid, a text field shrinks to a few letters.
                                let height = ui.spacing().interact_size.y;
                                ui.add_sized([320.0, height], egui::TextEdit::singleline(&mut t.title));
                                let new = crate::tagging::new_name(shown, t, &artist, &date, width).filter(|n| *n != file_name(&t.path));
                                ui.label(new.unwrap_or_else(|| "(unchanged)".into()));
                                ui.end_row();
                            }
                        });
                    });
            };
            if class == egui::ViewportClass::Embedded {
                egui::Window::new(title).id(egui::Id::new(id)).collapsible(false).default_size([940.0, 560.0]).open(&mut open).show(vctx, content);
            } else {
                egui::CentralPanel::default().show(vctx, content);
                if vctx.input(|i| i.viewport().close_requested()) {
                    open = false;
                }
            }
        });
        self.opt.tag_scheme = scheme;
        if let Some(show) = chosen_setlist.and_then(|i| d.setlist_shows.get(i).cloned()) {
            d.use_setlist(&show);
        }
        if ask_setlist {
            let (tx, rx) = mpsc::channel();
            let (artist, date) = (d.draft.artist.trim().to_string(), d.draft.date.trim().to_string());
            d.setlist_asked = (artist.clone(), date.clone());
            d.setlist_cancel = Arc::new(AtomicBool::new(false));
            let cancel = d.setlist_cancel.clone();
            std::thread::spawn(move || {
                let _ = tx.send(crate::setlistfm::find(&setlist_key, &artist, &date, &cancel).map_err(|e| e.text));
            });
            d.setlist_search = Some(Arc::new(Mutex::new(rx)));
            d.setlist_shows.clear();
            d.setlist_note = None;
        }
        if let Some(i) = reload {
            match crate::tagging::draft(&d.show, &d.infos[i].0) {
                Ok(draft) => {
                    d.draft = draft;
                    d.info = i;
                    d.album_auto = true;
                }
                Err(e) => self.log(e, Level::Error),
            }
        }
        if apply && in_batch {
            let i = d.batch_row.unwrap_or_default();
            if let Some(row) = self.tag_batch.as_mut().and_then(|b| b.rows.get_mut(i)) {
                row.plan = Some(TagDialog { batch_row: None, ..d });
                row.checked = true;
                row.ticked = true;
            }
            if then_next {
                self.review_next(Some(i));
            }
        } else if apply {
            d.setlist_cancel.store(true, Ordering::SeqCst);
            let job = d.job(scheme);
            self.start(Op::Tag(vec![job]), vec![d.show.clone()], d.show.files.clone(), false);
        } else if open && !cancel {
            self.tag_dialog = Some(d);
        } else {
            // Closed: a setlist.fm search still under way stops.
            d.setlist_cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Several shows of the artist on setlist.fm that day: the user picks one per show.
    fn setlist_window(&mut self, ctx: &egui::Context) {
        if self.setlist_choices.is_empty() {
            return;
        }
        let (mut open, mut write, mut close) = (true, false, false);
        egui::Window::new("Choose the setlist").collapsible(false).default_size([760.0, 420.0]).open(&mut open).show(ctx, |ui| {
            ui.label("setlist.fm has several shows of the artist that day. Choose the right one for each show, or leave it out.");
            ui.add_space(4.0);
            egui::TopBottomPanel::bottom("setlist_buttons").show_inside(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let any = self.setlist_choices.iter().any(|c| c.pick.is_some());
                    write = ui.add_enabled(any, egui::Button::new(RichText::new("Write the chosen setlists").strong())).clicked();
                    close = ui.button("Cancel").clicked();
                });
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for c in self.setlist_choices.iter_mut() {
                    ui.label(RichText::new(crate::files::show_name(&c.root)).strong());
                    for (i, show) in c.shows.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let songs = match show.songs.len() {
                                0 => "no songs".to_string(),
                                n => format!("{n} songs"),
                            };
                            let tour = if show.tour.is_empty() { String::new() } else { format!(", {}", show.tour) };
                            ui.radio_value(&mut c.pick, Some(i), format!("{}   ({songs}{tour})", show.location()));
                            if !show.url.is_empty() {
                                ui.hyperlink_to("setlist.fm", &show.url);
                            }
                        });
                    }
                    ui.radio_value(&mut c.pick, None, "None of these");
                    ui.add_space(6.0);
                }
            });
        });
        if write {
            for c in std::mem::take(&mut self.setlist_choices) {
                let Some(show) = c.pick.and_then(|i| c.shows.get(i)) else { continue };
                let (text, level, tip, _) = ops::setlist_outcome(&c.root, show);
                self.log(format!("{}: {text}", crate::files::show_name(&c.root)), level);
                self.status.insert(c.root.clone(), (text, level, tip));
            }
        } else if close || !open {
            self.setlist_choices.clear();
        }
    }

    /// The DVD titles and videos of the selection and their tracks; the first track that
    /// extracts to FLAC picked, else the first that extracts at all.
    fn open_extract_dialog(&mut self) {
        use crate::audio::Format;
        let (_, files) = self.selection();
        let titles: Vec<ExtractTitle> = files
            .into_iter()
            .filter_map(|source| {
                let tracks: Vec<(String, Option<&'static str>)> = match Format::from_path(&source)? {
                    Format::Vob => crate::dvd::dvd_audio(&crate::dvd::title_of(&source))
                        .iter()
                        .map(|t| (t.summary(), Some(if t.lossless() { "FLAC" } else { "original stream" })))
                        .collect(),
                    f if f.video() => crate::video::audio_tracks(&source).iter().map(|t| (t.summary(), t.output())).collect(),
                    _ => return None,
                };
                let pick = tracks.iter().position(|t| t.1 == Some("FLAC")).or_else(|| tracks.iter().position(|t| t.1.is_some())).unwrap_or(0);
                Some(ExtractTitle { source, tracks, pick })
            })
            .filter(|t| !t.tracks.is_empty())
            .collect();
        if titles.is_empty() {
            self.flash("Select DVD titles or videos (MKV, MP4): no audio to extract in the selection.");
            return;
        }
        self.extract_dialog = Some(ExtractDialog { titles });
    }

    fn extract_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.extract_dialog.take() else { return };
        let (mut open, mut go, mut close) = (true, false, false);
        let busy = self.running.is_some();
        egui::Window::new("Extract audio").collapsible(false).default_size([720.0, 380.0]).open(&mut open).show(ctx, |ui| {
            ui.label("One file per DVD title or video: a DVD title's from all its VOB files, in the DVD's folder; a video's beside it. \
                      A lossless track (LPCM, PCM, FLAC, ALAC) becomes a FLAC file, bit for bit; a lossy one (AC-3, DTS, AAC, MPEG) \
                      is saved as its original stream, unchanged: converting it would only lose more.");
            ui.add_space(4.0);
            egui::TopBottomPanel::bottom("extract_buttons").show_inside(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    go = ui.add_enabled(!busy, egui::Button::new(RichText::new("Extract").strong())).clicked();
                    close = ui.button("Cancel").clicked();
                    if busy {
                        ui.label(RichText::new("Wait for the running task to finish.").weak());
                    }
                });
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for t in d.titles.iter_mut() {
                    let title = if crate::audio::Format::from_path(&t.source) == Some(crate::audio::Format::Vob) {
                        format!("{}   ({})", crate::files::show_name(&crate::dvd::output_dir(&t.source)), file_name(&t.source))
                    } else {
                        file_name(&t.source)
                    };
                    ui.label(RichText::new(title).strong());
                    for (i, (summary, out)) in t.tracks.iter().enumerate() {
                        let label = format!("Audio {}: {summary}  →  {}", i + 1, out.unwrap_or("cannot be extracted yet"));
                        ui.add_enabled_ui(out.is_some(), |ui| ui.radio_value(&mut t.pick, i, label));
                    }
                    ui.add_space(6.0);
                }
            });
        });
        if go {
            let tracks = d.titles.iter().map(|t| (t.source.clone(), t.pick)).collect();
            let files: Vec<PathBuf> = d.titles.iter().map(|t| t.source.clone()).collect();
            self.start(Op::ExtractAudio(tracks), Vec::new(), files, false);
        } else if open && !close {
            self.extract_dialog = Some(d);
        }
    }

    fn open_undo_dialog(&mut self) {
        let runs: Vec<(PathBuf, crate::undo::Batch, u64)> = crate::undo::list().into_iter().map(|(run, batch)| {
            let size = crate::undo::size(&run);
            (run, batch, size)
        }).collect();
        self.undo_dialog = Some(UndoDialog { runs, ticked: Default::default(), confirm: false });
    }

    fn undo_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.undo_dialog.take() else { return };
        let (mut open, mut close, mut go) = (true, false, false);
        let busy = self.running.is_some();
        egui::Window::new("Undo tagging").collapsible(false).default_size([720.0, 480.0]).open(&mut open).show(ctx, |ui| {
            ui.label(format!(
                "Each Tag and rename is kept {} days: rolling back gives the files their former tags and names, byte for byte, \
                 and the checksum files as they were (the checksums and torrents made before match again).",
                crate::undo::KEEP_DAYS
            ));
            ui.add_space(4.0);
            egui::TopBottomPanel::bottom("undo_buttons").show_inside(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let n = d.ticked.len();
                    let text = if d.confirm { format!("Click again to roll back {}", ops::plural(n, "show", "shows")) } else { format!("Roll back {}", ops::plural(n, "ticked show", "ticked shows")) };
                    if ui.add_enabled(n > 0 && !busy, egui::Button::new(RichText::new(text).strong())).clicked() {
                        go = d.confirm;
                        d.confirm = true;
                    }
                    close = ui.button("Close").clicked();
                    if busy {
                        ui.label(RichText::new("Wait for the running task to finish.").weak());
                    }
                });
            });
            if d.runs.is_empty() {
                ui.label(RichText::new("Nothing to undo: no Tag and rename in the last days.").weak());
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for (r, (_, batch, size)) in d.runs.iter().enumerate() {
                    let open_shows: Vec<usize> = (0..batch.shows.len()).filter(|&s| !batch.shows[s].rolled_back).collect();
                    // The show itself when there is only one, so that runs of the same minute differ.
                    let what = match batch.shows.as_slice() {
                        [one] => one.name.clone(),
                        shows => ops::plural(shows.len(), "show", "shows"),
                    };
                    let done = if open_shows.is_empty() { ", rolled back" } else { "" };
                    let title = format!("{} ({}): {what}  [{}{done}]", crate::undo::date(batch.created), crate::undo::age(batch.created), ops::human_size(*size));
                    let title = if open_shows.is_empty() { RichText::new(title).weak() } else { RichText::new(title).strong() };
                    egui::CollapsingHeader::new(title).id_salt(("undo run", r)).default_open(!open_shows.is_empty()).show(ui, |ui| {
                        if open_shows.len() > 1 {
                            let mut all = open_shows.iter().all(|&s| d.ticked.contains(&(r, s)));
                            if ui.checkbox(&mut all, "All the shows of this run").changed() {
                                for &s in &open_shows {
                                    if all { d.ticked.insert((r, s)); } else { d.ticked.remove(&(r, s)); }
                                }
                                d.confirm = false;
                            }
                        }
                        for (s, show) in batch.shows.iter().enumerate() {
                            let renamed = show.files.iter().filter(|f| f.old != f.new).count();
                            let tagged = show.files.iter().filter(|f| f.head.is_some()).count();
                            let what = format!("   ({tagged} tagged, {renamed} renamed)");
                            if show.rolled_back {
                                ui.label(RichText::new(format!("{}: rolled back", show.name)).weak());
                                continue;
                            }
                            let mut on = d.ticked.contains(&(r, s));
                            if ui.checkbox(&mut on, format!("{}{what}", show.name)).on_hover_text(show.root.display().to_string()).changed() {
                                if on { d.ticked.insert((r, s)); } else { d.ticked.remove(&(r, s)); }
                                d.confirm = false;
                            }
                        }
                    });
                }
            });
        });
        if go {
            // One run at a time: the first run with ticked shows (the others stay ticked).
            let mut runs: Vec<usize> = d.ticked.iter().map(|(r, _)| *r).collect();
            runs.sort_unstable();
            if let Some(&r) = runs.first() {
                let mut shows: Vec<usize> = d.ticked.iter().filter(|(rr, _)| *rr == r).map(|(_, s)| *s).collect();
                shows.sort_unstable();
                self.start(Op::Rollback { run: d.runs[r].0.clone(), shows }, Vec::new(), Vec::new(), false);
            }
            self.undo_dialog = None;
            return;
        }
        if open && !close {
            self.undo_dialog = Some(d);
        }
    }

    fn save_log(&mut self) {
        self.ask(Dialog::SaveLog);
    }

    fn save_log_now(&mut self) {
        if let Some(path) = file_dialog(self.parent).set_file_name("bootleg-toolbox.log").save_file() {
            let text: String = self.log.iter().map(|(t, _)| format!("{t}\n")).collect();
            if let Err(e) = std::fs::write(&path, text) {
                self.log(format!("Cannot save the log: {e}"), Level::Error);
            }
        }
    }

    // ------------------------------------------------------------ UI parts

    fn menu_and_toolbar(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let busy = self.running.is_some();
        let has = !self.shows.is_empty();
        let scope = match (self.selected.is_empty(), self.filter.is_some()) {
            (true, true) => "the files shown (nothing selected)",
            (true, false) => "the whole list (nothing selected)",
            _ => "the selection",
        };
        let mut menu_open = false;
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| fit_below(ui, |ui| {
                    menu_open = true;
                    if ui.add_enabled(!busy, egui::Button::new("Add folder...")).clicked() {
                        ui.close_menu();
                        self.ask(Dialog::AddFolder);
                    }
                    if ui.add_enabled(!busy, egui::Button::new("Add files...")).clicked() {
                        ui.close_menu();
                        self.ask(Dialog::AddFiles);
                    }
                    if ui.add_enabled(!busy, egui::Button::new("Add DVD...")).on_hover_text(DVD_TIP).clicked() {
                        ui.close_menu();
                        self.pick_dvd();
                    }
                    ui.separator();
                    if ui.add_enabled(has, egui::Button::new("Select all").shortcut_text("Ctrl+A")).clicked() {
                        ui.close_menu();
                        actions.push(Action::SelectAll);
                    }
                    if ui.add_enabled(!busy && has, egui::Button::new("Remove from list").shortcut_text("Del")).clicked() {
                        ui.close_menu();
                        actions.push(Action::Remove);
                    }
                    if ui.add_enabled(!busy && has, egui::Button::new("Clear list")).clicked() {
                        ui.close_menu();
                        self.clear_list();
                    }
                    ui.separator();
                    if ui.button("Save log...").clicked() {
                        ui.close_menu();
                        self.save_log();
                    }
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }));
                ui.menu_button("Tools", |ui| fit_below(ui, |ui| {
                    menu_open = true;
                    ops_menu(ui, busy || !has, self.opt.flac_level, &tracker_entries(&self.opt), actions);
                    ui.separator();
                    if ui.add_enabled(!busy && has, egui::Button::new("🖼 Share card...")).on_hover_text(SHARE_TIP).clicked() {
                        ui.close_menu();
                        actions.push(Action::ShareCard(None));
                    }
                    if ui.add_enabled(!busy, egui::Button::new("↩ Undo tagging...")).on_hover_text(UNDO_TIP).clicked() {
                        ui.close_menu();
                        self.open_undo_dialog();
                    }
                    if ui.add_enabled(busy, egui::Button::new("Stop")).clicked() {
                        ui.close_menu();
                        self.stop();
                    }
                }));
                if ui.add_enabled(!busy, egui::Button::new("Settings").frame(false)).clicked() {
                    self.settings = Some(self.opt.clone());
                }
                ui.menu_button("View", |ui| fit_below(ui, |ui| {
                    menu_open = true;
                    let cols = self.visible_columns();
                    columns_menu(ui, &cols, actions);
                    ui.separator();
                    let mut compact = self.opt.compact;
                    if ui.checkbox(&mut compact, "Compact view").on_hover_text("Fewer columns (Format then gives bits and rate) and tighter rows").clicked() {
                        actions.push(Action::Compact(compact));
                    }
                    let mut collapsed = self.opt.collapsed_view;
                    if ui.checkbox(&mut collapsed, "Collapsed view").on_hover_text("One row per show, the tracks folded: handy with many shows (a show still unfolds with its arrow)").clicked() {
                        actions.push(Action::CollapsedView(collapsed));
                    }
                    let mut log_open = self.opt.log_open;
                    if ui.checkbox(&mut log_open, "Show the log").clicked() {
                        actions.push(Action::LogOpen(log_open));
                    }
                    let mut on_top = self.opt.always_on_top;
                    if ui.checkbox(&mut on_top, "Always on top").on_hover_text("Keep the window above the other windows").clicked() {
                        actions.push(Action::AlwaysOnTop(on_top));
                    }
                    ui.separator();
                    ui.label(RichText::new("Theme").strong());
                    for (choice, text) in [(ThemeChoice::System, "Follow the system"), (ThemeChoice::Light, "Light"), (ThemeChoice::Dark, "Dark")] {
                        if ui.radio(self.opt.theme == choice, text).clicked() {
                            actions.push(Action::Theme(choice));
                        }
                    }
                }));
                ui.menu_button("Help", |ui| fit_below(ui, |ui| {
                    menu_open = true;
                    if let Some(version) = self.newer_version.clone() {
                        if ui.button(RichText::new(format!("⬇ Download version {version}")).strong()).on_hover_text("Open the release page on GitHub").clicked() {
                            ui.close_menu();
                            // Detached: the browser may take a while to start. The page address goes
                            // to the log, to open by hand when no browser answers.
                            let url = format!("{}/latest", crate::update::RELEASES);
                            match open::that_detached(&url) {
                                Ok(()) => self.log(format!("Opening the release page in the web browser: {url}"), Level::Info),
                                Err(e) => self.log(format!("Could not open the web browser ({e}). Release page: {url}"), Level::Error),
                            }
                        }
                    }
                    if ui
                        .add_enabled(self.update_check.is_none(), egui::Button::new("Check for updates"))
                        .on_hover_text("Ask github.com whether a newer version has been released. Nothing is sent but the program's name and version.")
                        .clicked()
                    {
                        ui.close_menu();
                        self.check_updates(true);
                    }
                    ui.separator();
                    #[cfg(target_os = "linux")]
                    if ui
                        .button("Add to the applications menu")
                        .on_hover_text("Adds Bootleg Toolbox, with its icon, to the menu of your desktop, for this program file where it is now. Use it again if you move the program.")
                        .clicked()
                    {
                        ui.close_menu();
                        match install_desktop_entry() {
                            Ok(text) => self.log(text, Level::Ok),
                            Err(e) => self.log(format!("Could not add the menu entry: {e}"), Level::Error),
                        }
                    }
                    if ui.add(egui::Button::new("Keyboard shortcuts and command line").shortcut_text("F1")).clicked() {
                        ui.close_menu();
                        self.help = true;
                    }
                    if ui.button(format!("About {APP_NAME}")).clicked() {
                        ui.close_menu();
                        self.about = true;
                    }
                }));
            });
        });
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                // Grouped in a few menus, as compact as Trader's Little Helper; shortcuts unchanged.
                ui.add_enabled_ui(!busy, |ui| {
                    ui.menu_button(tool_title("🗀", "Add", !busy, ui.style()), |ui| fit_below(ui, |ui| {
                        menu_open = true;
                        if ui.button("Add folder...").clicked() {
                            ui.close_menu();
                            self.ask(Dialog::AddFolder);
                        }
                        if ui.button("Add files...").clicked() {
                            ui.close_menu();
                            self.ask(Dialog::AddFiles);
                        }
                        if ui.button("📀 Add DVD...").on_hover_text(DVD_TIP).clicked() {
                            ui.close_menu();
                            self.pick_dvd();
                        }
                        ui.separator();
                        if ui.add_enabled(has, egui::Button::new("🗑 Clear list")).clicked() {
                            ui.close_menu();
                            self.clear_list();
                        }
                    }))
                    .response
                    .on_hover_text("Add show folders or audio files (or drag and drop them onto the list)");
                });
                let on = !busy && has;
                let kinds = self.checksum_kinds();
                ui.add_enabled_ui(on, |ui| {
                    ui.menu_button(tool_title("✔", "Check", on, ui.style()), |ui| fit_below(ui, |ui| {
                        menu_open = true;
                        let items: [(&str, &str, Op, String); 7] = [
                            ("🛡 Verify checksums", "F5", Op::Verify, format!("Check the files of {scope} against their .ffp, .md5, .st5 and .sfv files.\nAn FFP line only compares the MD5 stored in the FLAC header: use Decode test to find damaged audio.")),
                            ("✔ Decode test", "F7", Op::Test, format!("Decode every file of {scope} completely to find damaged or truncated audio; FLAC files are also checked against their MD5 signature.\nNeeds no checksum file.")),
                            ("🔍 Lossless?", "Ctrl+L", Op::Lossless, format!("Analyse the spectrum of every file of {scope} to find lossy (MP3/AAC) sources, FM broadcasts, upsampled files and fake 24-bit.\nThe verdict goes to the Source column; click it for the full analysis.")),
                            ("⏱ Speed?", "Ctrl+P", Op::Speed, format!("Estimate whether the tape of {scope} runs too fast or too slow, and the correction to apply.\nThe verdict goes to the Speed column; click it for the full analysis.")),
                            ("📊 Spectrogram", "F9", Op::Spectrogram, format!("Show the spectrogram of the first files of {scope}, up to 4, to see lossy sources for yourself")),
                            ("ℹ Show details", "F8", Op::Details { verbose: true }, format!("Read the format, length and SBEs of {scope} again, and list them in the log")),
                            ("📋 Media info", "Ctrl+I", Op::MediaInfo(Vec::new()), format!("A technical summary of {scope} (format, encoder, length, bit rate, tags; for a DVD title, the whole DVD: video, audio tracks, subtitles, titles and chapters), as text to copy.")),
                        ];
                        for (i, (label, key, op, tip)) in items.into_iter().enumerate() {
                            if i == 4 {
                                ui.separator();
                            }
                            if ui.add(egui::Button::new(label).shortcut_text(key)).on_hover_text(tip).clicked() {
                                ui.close_menu();
                                actions.push(Action::Run(op));
                            }
                            if i == 1 {
                                torrent_check_button(ui, false, actions);
                            }
                        }
                    }))
                    .response
                    .on_hover_text(format!("Verify, test and analyse {scope}"));
                    ui.menu_button(tool_title("✚", "Create", on, ui.style()), |ui| fit_below(ui, |ui| {
                        menu_open = true;
                        if ui
                            .add(egui::Button::new("✚ Create checksums").shortcut_text("F6"))
                            .on_hover_text(format!("Create {kinds} for {scope}.\nThe types of files to create can be changed in Settings."))
                            .clicked()
                        {
                            ui.close_menu();
                            actions.push(Action::Run(Op::Create));
                        }
                        if ui
                            .button("📋 Copy FFP contents")
                            .on_hover_text("Copy the show's .ffp file to the clipboard (for the TTD upload form)")
                            .clicked()
                        {
                            ui.close_menu();
                            actions.push(Action::CopyFfp);
                        }
                        if ui.button("📋 Copy info file").on_hover_text(INFO_TIP).clicked() {
                            ui.close_menu();
                            actions.push(Action::CopyInfo);
                        }
                    }))
                    .response
                    .on_hover_text(format!("Create checksum files ({kinds}) for {scope}"));
                });
                ui.separator();
                ui.add_enabled_ui(on, |ui| {
                    let level = self.opt.flac_level;
                    ui.menu_button(tool_title("🎵", "Convert", on, ui.style()), |ui| fit_below(ui, |ui| {
                        menu_open = true;
                        ui.label(RichText::new("To FLAC (verified), level:").strong());
                        // The levels on one row: a sub-menu would stay open over the other entries (egui 0.29).
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            for l in 0..=8u8 {
                                let tip = match l {
                                    0 => "Level 0: fastest".to_string(),
                                    5 => "Level 5: the flac default".to_string(),
                                    8 => "Level 8: smallest files".to_string(),
                                    _ => format!("Level {l}"),
                                };
                                if ui.selectable_label(l == level, format!(" {l} ")).on_hover_text(tip).clicked() {
                                    ui.close_menu();
                                    actions.push(Action::FlacLevel(l));
                                    actions.push(Action::Run(Op::Convert { target: Target::Flac, level: l, round_float: false }));
                                }
                            }
                        });
                        ui.separator();
                        if ui.button("To WAV").clicked() {
                            ui.close_menu();
                            actions.push(Action::Run(Op::Convert { target: Target::Wav, level, round_float: false }));
                        }
                        ui.separator();
                        ui.label(RichText::new("MP3 copy (separate folder)").strong());
                        mp3_buttons(ui, false, actions);
                        ui.separator();
                        sbe_button(ui, false, actions);
                        clean_wav_button(ui, false, actions);
                        ui.separator();
                        tag_button(ui, false, actions);
                        setlist_button(ui, false, actions);
                        extract_button(ui, false, actions);
                    }))
                    .response
                    .on_hover_text(format!("Convert {scope} to FLAC (each file is decoded again and checked) or to WAV, make an MP3 copy of the shows in a separate folder, or fix SBEs. The originals are kept."));
                });
                ui.separator();
                let trackers = tracker_entries(&self.opt);
                ui.add_enabled_ui(on, |ui| {
                    let torrent = ui.menu_button(tool_title("🌐", "Torrent", on, ui.style()), |ui| fit_below(ui, |ui| {
                        menu_open = true;
                        torrent_menu(ui, &trackers, actions)
                    }));
                    torrent.response.on_hover_text(format!("Create torrents for the shows of {scope}"));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!busy, egui::Button::new(RichText::new("⚙").size(16.0))).on_hover_text("Settings").clicked() {
                        self.settings = Some(self.opt.clone());
                    }
                    // Red while a task runs, so that it can be stopped at a glance.
                    let stop = if busy { RichText::new("⏹ Stop").color(color(Level::Error).unwrap_or_default()) } else { RichText::new("⏹ Stop") };
                    if ui.add_enabled(busy, egui::Button::new(stop)).on_hover_text("Stop the running task (Esc)").clicked() {
                        self.stop();
                    }
                    // The whole list in a few words.
                    if !self.shows.is_empty() {
                        ui.add_space(8.0);
                        let files: Vec<PathBuf> = self.shows.iter().flat_map(|s| s.files.iter().cloned()).collect();
                        let (seconds, complete) = show_length(&files, &self.info);
                        let mut parts = vec![crate::ops::plural(self.shows.len(), "show", "shows"), crate::ops::plural(files.len(), "file", "files")];
                        if seconds > 0.0 {
                            let minutes = libm::round(seconds / 60.0) as u64;
                            parts.push(format!("{} h {:02} min{}", minutes / 60, minutes % 60, if complete { "" } else { " +" }));
                        }
                        parts.push(ops::human_size(self.list_bytes));
                        let tip = if complete { "The whole list: shows, files, length and size" } else { "The whole list: shows, files, length (+: some lengths not read yet) and size" };
                        ui.add(egui::Label::new(RichText::new(parts.join(" · ")).color(Palette::now().weak)).truncate()).on_hover_text(tip);
                    }
                });
            });
            ui.add_space(3.0);
        });
        self.menu_open |= menu_open;
    }

    fn bottom(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let p = Palette::now();
        // The filter buttons: all files, or only those with one result of the last task.
        let tally = self.tally();
        if let Some(level) = self.filter {
            let n = match level {
                Level::Ok => tally[0],
                Level::Warn => tally[1],
                _ => tally[2],
            };
            if n == 0 {
                self.filter = None;
            }
        }
        let mut pick = None;
        let status_bar = egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if tally.iter().sum::<usize>() > 0 {
                    let all: usize = self.shows.iter().map(|s| s.files.len()).sum();
                    // Which task the results come from: the running one (so far), or the last.
                    let (task, tip) = match (&self.running, self.results_of) {
                        (Some(r), _) => (r.name, format!("Results of {} so far; the former results of its files are faded until it gives theirs", r.name)),
                        (None, Some(name)) => (name, format!("Results of the last task: {name}")),
                        (None, None) => ("", String::new()),
                    };
                    if !task.is_empty() {
                        ui.label(RichText::new(format!("{task}:")).color(p.weak)).on_hover_text(tip);
                    }
                    let segments = [(None, "All", all), (Some(Level::Ok), "OK", tally[0]), (Some(Level::Warn), "To check", tally[1]), (Some(Level::Error), "Errors", tally[2])];
                    segmented(ui, p, |ui| {
                        for (level, name, n) in segments {
                            if level.is_some() && n == 0 {
                                continue;
                            }
                            let on = self.filter == level;
                            let mut job = egui::text::LayoutJob::default();
                            let font = egui::FontId::proportional(12.5);
                            let dot = level.map(vivid).unwrap_or(p.weak);
                            job.append("● ", 0.0, egui::TextFormat::simple(egui::FontId::proportional(10.0), dot));
                            job.append(&format!("{name} "), 0.0, egui::TextFormat::simple(font.clone(), ui.visuals().text_color()));
                            job.append(&n.to_string(), 0.0, egui::TextFormat::simple(egui::FontId::new(12.5, crate::fonts::bold()), ui.visuals().strong_text_color()));
                            let tip = match level {
                                None => "Show every file",
                                Some(Level::Ok) => "Show only the files that came out OK in the last task",
                                Some(Level::Warn) => "Show only the files to check",
                                _ => "Show only the files with an error",
                            };
                            if segment(ui, p, job, on).on_hover_text(tip).clicked() {
                                pick = Some(if on { None } else { level });
                            }
                        }
                    });
                    ui.add_space(6.0);
                }
                if let Some(r) = &mut self.running {
                    let (fraction, secs_done, secs, left) = r.progress();
                    // The time left moves smoothly: updated once a second, half-way to the new estimate.
                    if let Some(raw) = left {
                        let now = Instant::now();
                        r.eta = Some(match r.eta {
                            Some((when, shown)) if when.elapsed() < Duration::from_secs(1) => (when, shown),
                            Some((when, shown)) => (now, 0.5 * (shown - when.elapsed().as_secs_f64()).max(0.0) + 0.5 * raw),
                            None => (now, raw),
                        });
                    }
                    let audio = |seconds: f64| format!("{} h {:02} min", (seconds / 3600.0) as u64, ((seconds % 3600.0) / 60.0) as u64);
                    ui.add(egui::ProgressBar::new(fraction).desired_width(220.0).text(format!("{}/{}", r.done, r.total)));
                    // A vivid colour, so that a running operation cannot be missed.
                    let busy = if DARK_THEME.load(Ordering::Relaxed) { Color32::from_rgb(196, 128, 255) } else { Color32::from_rgb(128, 36, 204) };
                    ui.add(egui::Spinner::new().color(busy));
                    ui.label(RichText::new(format!("{}...", r.label)).color(busy).strong());
                    // How much music is done, when the lengths are known.
                    if secs > 0.0 {
                        ui.label(RichText::new(format!("{} of {} of audio", audio(secs_done), audio(secs))).color(busy));
                    }
                    // Time left: said to be coming until it can be told (15 s, 5 % of the work).
                    let eta = match (r.eta, left.is_some()) {
                        (Some((_, shown)), true) => time_left(shown),
                        _ => "estimating time left...".to_string(),
                    };
                    ui.label(RichText::new(eta).color(busy));
                } else if let Some((text, when)) = &self.flash {
                    if when.elapsed() < Duration::from_secs(15) {
                        // At most half the bar, wrapped, so that the selection stays readable too.
                        let half = ui.available_width() / 2.0;
                        ui.scope(|ui| {
                            ui.set_max_width(half);
                            ui.add(egui::Label::new(text.as_str()).wrap());
                        });
                    }
                }
                if !self.shows.is_empty() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Row density: compact (more rows, fewer columns) or comfortable.
                        segmented(ui, p, |ui| {
                            for (compact, tip) in [(true, "Compact view: tighter rows, fewer columns (also in View)"), (false, "Comfortable rows")] {
                                let on = self.opt.compact == compact;
                                let response = segment(ui, p, egui::text::LayoutJob::default(), on).on_hover_text(tip);
                                let r = response.rect;
                                let lines = if compact { 4 } else { 3 };
                                let ink = ui.visuals().text_color();
                                for i in 0..lines {
                                    let y = r.top() + r.height() * (i as f32 + 1.0) / (lines as f32 + 1.0);
                                    ui.painter().line_segment([egui::pos2(r.center().x - 6.0, y), egui::pos2(r.center().x + 6.0, y)], egui::Stroke::new(1.5f32, ink));
                                }
                                if response.clicked() && !on {
                                    actions.push(Action::Compact(compact));
                                }
                            }
                        });
                        // Collapsed view: one row per show.
                        ui.add_space(4.0);
                        segmented(ui, p, |ui| {
                            let on = self.opt.collapsed_view;
                            let response = segment(ui, p, egui::text::LayoutJob::default(), on)
                                .on_hover_text("Collapsed view: one row per show, the tracks folded (also in View)");
                            let r = response.rect;
                            let ink = ui.visuals().text_color();
                            for i in 0..3 {
                                let y = r.top() + r.height() * (i as f32 + 1.0) / 4.0;
                                let x = r.center().x - 7.0;
                                ui.painter().add(egui::Shape::convex_polygon(
                                    vec![egui::pos2(x, y - 2.5), egui::pos2(x + 3.0, y), egui::pos2(x, y + 2.5)],
                                    ink,
                                    egui::Stroke::NONE,
                                ));
                                ui.painter().line_segment([egui::pos2(x + 5.5, y), egui::pos2(r.center().x + 7.0, y)], egui::Stroke::new(1.5f32, ink));
                            }
                            if response.clicked() {
                                actions.push(Action::CollapsedView(!on));
                            }
                        });
                        ui.add_space(8.0);
                        let (shows, files) = self.selection();
                        let counts = format!("{}, {}", crate::ops::plural(shows.len(), "show", "shows"), crate::ops::plural(files.len(), "file", "files"));
                        let text = if self.selected.is_empty() && self.filter.is_some() {
                            colored(format!("No selection: actions apply to the files shown ({counts})"), Level::Warn)
                        } else if self.selected.is_empty() {
                            colored(format!("No selection: actions apply to the whole list ({counts})"), Level::Warn)
                        } else {
                            RichText::new(format!("Selected: {counts}"))
                        };
                        ui.add(egui::Label::new(text).wrap());
                    });
                }
            });
        });
        if let Some(filter) = pick {
            self.filter = filter;
        }
        // The log folds to one line (its last message); it unfolds by itself on an error.
        let open = self.opt.log_open;
        let panel = if open {
            egui::TopBottomPanel::bottom("log").resizable(true).default_height(170.0).min_height(60.0)
        } else {
            egui::TopBottomPanel::bottom("log folded").resizable(false)
        };
        let log_panel = panel.show(ctx, |ui| {
            let (mut clear, mut save, mut toggle) = (false, false, false);
            ui.horizontal(|ui| {
                let arrow = if open { "⏷" } else { "⏵" };
                toggle = ui
                    .add(egui::Label::new(RichText::new(format!("{arrow} Log ({})", crate::ops::plural(self.log.len(), "line", "lines"))).weak()).sense(egui::Sense::click()))
                    .on_hover_text(if open { "Fold the log" } else { "Unfold the log" })
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked();
                if !open {
                    if let Some((text, level)) = self.log.last() {
                        ui.add(egui::Label::new(colored(text.clone(), *level)).truncate());
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if open {
                        save = ui.small_button("💾 Save log...").on_hover_text("Save the log to a text file").clicked();
                        clear = ui.small_button("🗑 Clear").on_hover_text("Empty the log (the table and its results stay)").clicked();
                    }
                });
            });
            if toggle {
                self.opt.log_open = !open;
            }
            if clear {
                self.log.clear();
            }
            if save {
                self.save_log();
            }
            if !open {
                return;
            }
            // Long lines wrap: each line is measured, only those in view are drawn.
            let font = egui::TextStyle::Monospace.resolve(ui.style());
            let gap = 2.0;
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
                let width = ui.available_width();
                let heights: Vec<f32> = ui.fonts(|f| {
                    self.log.iter().map(|(text, _)| f.layout(text.clone(), font.clone(), Color32::WHITE, width).size().y + gap).collect()
                });
                let (rect, _) = ui.allocate_exact_size(egui::vec2(width, heights.iter().sum()), egui::Sense::hover());
                let mut top = 0.0;
                for ((text, level), height) in self.log.iter().zip(heights) {
                    if top + height >= viewport.min.y && top <= viewport.max.y {
                        let line = egui::Rect::from_min_size(rect.min + egui::vec2(0.0, top), egui::vec2(width, height));
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(line).layout(egui::Layout::top_down(egui::Align::Min)));
                        child.add(egui::Label::new(colored(text.clone(), *level).monospace()).wrap());
                    }
                    top += height;
                }
            });
        });
        self.bottom_top = status_bar.response.rect.top().min(log_panel.response.rect.top());
    }

    /// The end of a long task, in a band above the list: its outcome, how long it took, a
    /// link to the files to check.
    fn banner(&mut self, ctx: &egui::Context) {
        let Some(b) = &self.banner else { return };
        let p = Palette::now();
        let (fill, ink) = p.badge(b.level);
        let (mut close, mut filter, mut share) = (false, None, false);
        egui::TopBottomPanel::top("banner").frame(egui::Frame::none().fill(fill).inner_margin(egui::Margin::symmetric(14.0, 8.0))).show(ctx, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
                let mark_ink = if b.level == Level::Warn { Color32::from_rgb(70, 50, 0) } else { Color32::WHITE };
                paint_mark(ui.painter(), rect.center(), 13.0, b.level, vivid(b.level), mark_ink);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new(&b.title).size(15.0).family(crate::fonts::bold()).color(ink));
                    ui.add(egui::Label::new(RichText::new(&b.detail).size(12.0).color(ink.gamma_multiply(0.85))).truncate());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui.add(egui::Button::new(RichText::new("×").size(18.0).color(ink)).frame(false)).on_hover_text("Close").clicked();
                    share = ui.add_enabled(self.running.is_none(), egui::Button::new("🖼 Share card...")).on_hover_text(SHARE_TIP).clicked();
                    if let Some((level, n)) = b.filter {
                        if ui.button(format!("Show the {n}")).clicked() {
                            filter = Some(level);
                        }
                    }
                });
            });
        });
        if close {
            self.banner = None;
        }
        if share {
            let roots = self.banner.as_ref().map(|b| b.roots.clone());
            self.pending_share = roots;
        }
        if filter.is_some() {
            self.filter = filter;
        }
    }

    /// The notifications of the tasks that ended, at the bottom right, newest at the bottom.
    fn toasts(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if self.toasts.is_empty() {
            return;
        }
        let p = Palette::now();
        let now = Instant::now();
        let screen = ctx.screen_rect();
        let mut bottom = self.bottom_top.min(screen.bottom()) - 12.0;
        let mut closed = Vec::new();
        let mut filter = None;
        let mut sliding = false;
        for (i, t) in self.toasts.iter_mut().enumerate().rev() {
            // Slides in from the right in a quarter of a second, easing out.
            let arrived = (t.born.elapsed().as_secs_f32() / 0.25).min(1.0);
            sliding |= arrived < 1.0;
            let ease = 1.0 - (1.0 - arrived) * (1.0 - arrived) * (1.0 - arrived);
            let area = egui::Area::new(egui::Id::new(("toast", t.id)))
                .order(egui::Order::Foreground)
                .pivot(egui::Align2::RIGHT_BOTTOM)
                .fixed_pos(egui::pos2(screen.right() - 16.0 + 48.0 * (1.0 - ease), bottom))
                .show(ctx, |ui| {
                    ui.set_opacity(ease);
                    egui::Frame::none()
                        .fill(p.ground)
                        .stroke(egui::Stroke::new(1.0f32, p.border))
                        .rounding(12.0)
                        .shadow(egui::epaint::Shadow { offset: egui::vec2(0.0, 8.0), blur: 24.0, spread: 0.0, color: Color32::from_black_alpha(40) })
                        .inner_margin(egui::Margin { left: 14.0, right: 8.0, top: 12.0, bottom: 14.0 })
                        .show(ui, |ui| {
                            ui.set_width(330.0);
                            ui.horizontal_top(|ui| {
                                let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
                                let (fill, ink) = p.badge(t.level);
                                paint_mark(ui.painter(), rect.center(), 11.0, t.level, fill, ink);
                                ui.vertical(|ui| {
                                    ui.set_width(262.0);
                                    ui.spacing_mut().item_spacing.y = 3.0;
                                    ui.label(RichText::new(&t.title).family(crate::fonts::bold()));
                                    ui.label(colored(&t.outcome, t.level));
                                    ui.add(egui::Label::new(RichText::new(&t.detail).size(12.0).color(p.weak)).wrap());
                                    ui.add_space(3.0);
                                    ui.horizontal(|ui| {
                                        if let Some(folder) = &t.folder {
                                            if ui.link("Open folder").clicked() {
                                                actions.push(Action::OpenFolder(folder.clone()));
                                            }
                                        }
                                        if let Some((level, n)) = t.filter {
                                            if ui.link(format!("Show the {n}")).clicked() {
                                                filter = Some(level);
                                            }
                                        }
                                        if ui.link("Show in log").clicked() {
                                            actions.push(Action::LogOpen(true));
                                        }
                                    });
                                });
                                if ui.add(egui::Button::new(RichText::new("×").size(18.0).color(p.weak)).frame(false)).on_hover_text("Close").clicked() {
                                    closed.push(i);
                                }
                            });
                        });
                });
            let rect = area.response.rect;
            // Time left, as a thin bar along the bottom; it stops under the pointer.
            if let Some(life) = t.life() {
                if !ctx.pointer_hover_pos().is_some_and(|pos| rect.contains(pos)) {
                    t.age += now.saturating_duration_since(t.seen);
                }
                let left = 1.0 - t.age.as_secs_f32() / life.as_secs_f32();
                if left <= 0.0 {
                    closed.push(i);
                }
                let bar = egui::Rect::from_min_size(egui::pos2(rect.left() + 14.0, rect.bottom() - 6.0), egui::vec2((rect.width() - 28.0) * left.max(0.0), 2.5));
                ctx.layer_painter(area.response.layer_id).rect_filled(bar, 1.25, vivid(t.level));
            }
            t.seen = now;
            bottom = rect.top() - 10.0;
        }
        closed.sort_unstable();
        closed.dedup();
        for i in closed.into_iter().rev() {
            self.toasts.remove(i);
        }
        if let Some(level) = filter {
            self.filter = Some(level);
        }
        if sliding {
            ctx.request_repaint();
        } else if self.toasts.iter().any(|t| t.life().is_some()) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn table(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let palette = Palette::now();
        egui::CentralPanel::default().frame(egui::Frame::central_panel(&ctx.style()).fill(palette.ground)).show(ctx, |ui| {
            if self.shows.is_empty() {
                let busy = self.running.is_some();
                match welcome(ui, palette, busy) {
                    Some(0) => self.ask(Dialog::AddFolder),
                    Some(1) => self.ask(Dialog::AddFiles),
                    Some(_) => self.pick_dvd(),
                    None => {}
                }
                return;
            }
            let rows = self.rows();
            let modifiers = ctx.input(|i| i.modifiers);
            let mut menu_open = false;
            // Selectable labels would take the clicks (text cursor) instead of the row.
            ui.style_mut().interaction.selectable_labels = false;
            let cols = self.visible_columns();
            let fit = std::mem::take(&mut self.fit_columns);
            if self.drag_select.is_some() && !ctx.input(|i| i.pointer.primary_down()) {
                self.drag_select = None;
            }
            let dragging = self.drag_select.is_some();
            let compact = self.opt.compact;
            // Same windows taking the keys as for the shortcuts, plus the settings.
            let keys_taken = ctx.wants_keyboard_input() || self.info_choice.is_some() || self.float_dialog.is_some() || self.settings.is_some();
            // A little air between columns fitted to their content.
            ui.spacing_mut().item_spacing.x = 16.0;
            // The column edges (resize handles) barely show until hovered.
            ui.visuals_mut().widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0f32, palette.ground.lerp_to_gamma(palette.border, 0.25));
            // The Result column wraps its text: a row grows to show it all.
            let mut result_width: f32 = 0.0;
            egui::ScrollArea::horizontal().show(ui, |ui| {
                let mut table = TableBuilder::new(ui)
                    // Each set of visible columns keeps its own widths. The number changes with
                    // the default widths, so that widths saved before are not kept over them.
                    .id_salt((2, cols.iter().map(|c| c.title()).collect::<Vec<_>>()))
                    .resizable(true)
                    .sense(egui::Sense::click_and_drag())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
                for (i, col) in cols.iter().enumerate() {
                    table = table.column(col.column(i + 1 == cols.len(), fit));
                }
                table
                    .min_scrolled_height(0.0)
                    .header(30.0, |mut header| {
                        for col in &cols {
                            header.col(|ui| {
                                // The part in view: the list may be wider than the window.
                                if *col == Col::Result {
                                    result_width = visible_width(ui);
                                }
                                let title = RichText::new(col.title().to_uppercase()).size(11.0).family(crate::fonts::bold()).color(palette.weak);
                                ui.label(title).on_hover_text(col.describe());
                                // Right-click on a header: choose the columns.
                                let all = ui.interact(ui.max_rect(), ui.id().with("columns menu"), egui::Sense::click());
                                all.context_menu(|ui| {
                                    menu_open = true;
                                    columns_menu(ui, &cols, actions);
                                });
                            });
                        }
                    })
                    .body(|mut body| {
                        // Dragging a selection near or past the top/bottom edge scrolls the list,
                        // faster the further the pointer goes.
                        let visible = body.ui_mut().clip_rect();
                        let pointer = ctx.input(|i| i.pointer.latest_pos());
                        let mut edge = None;
                        if let (true, Some(p)) = (dragging, pointer) {
                            const ZONE: f32 = 24.0;
                            let speed = |depth: f32| (depth / ZONE * 8.0).clamp(2.0, 40.0);
                            if p.y > visible.bottom() - ZONE {
                                body.ui_mut().scroll_with_delta(egui::vec2(0.0, -speed(p.y - (visible.bottom() - ZONE))));
                                edge = Some(false);
                            } else if p.y < visible.top() + ZONE {
                                body.ui_mut().scroll_with_delta(egui::vec2(0.0, speed(visible.top() + ZONE - p.y)));
                                edge = Some(true);
                            }
                            if edge.is_some() {
                                ctx.request_repaint();
                            }
                        }
                        let (mut first_shown, mut last_shown) = (usize::MAX, 0usize);
                        let row_height: f32 = if compact { 20.0 } else { 27.0 };
                        let font = egui::TextStyle::Body.resolve(&ctx.style());
                        let heights: Vec<f32> = rows
                            .iter()
                            .map(|r| match self.status.get(self.row_path(*r)) {
                                Some((text, _, _)) if result_width > DOT_WIDTH => {
                                    let galley = ctx.fonts(|f| f.layout(text.clone(), font.clone(), Color32::WHITE, result_width - DOT_WIDTH));
                                    row_height.max(galley.size().y + 6.0)
                                }
                                _ => row_height,
                            })
                            .collect();
                        // Page Up / Page Down scroll by a screen (keeping one row in view), Home / End
                        // go to the top / bottom of the list.
                        if !keys_taken {
                            let page = (visible.height() - row_height).max(row_height);
                            let all = heights.iter().sum::<f32>() + visible.height();
                            let delta = ctx.input_mut(|i| {
                                if i.consume_key(Modifiers::NONE, Key::PageDown) { -page }
                                else if i.consume_key(Modifiers::NONE, Key::PageUp) { page }
                                else if i.consume_key(Modifiers::NONE, Key::End) { -all }
                                else if i.consume_key(Modifiers::NONE, Key::Home) { all }
                                else { 0.0 }
                            });
                            if delta != 0.0 {
                                body.ui_mut().scroll_with_delta(egui::vec2(0.0, delta));
                            }
                        }
                        body.heterogeneous_rows(heights.into_iter(), |mut row| {
                            let index = row.index();
                            first_shown = first_shown.min(index);
                            last_shown = last_shown.max(index);
                            let r = rows[index];
                            let path = self.row_path(r).to_path_buf();
                            row.set_selected(self.row_selected(r));
                            let mut responses = Vec::new();
                            match r {
                                Row::Show(si) => self.show_cells(&mut row, si, &cols, visible, &mut responses, actions),
                                Row::Disc(si, fi) => self.disc_cells(&mut row, si, fi, &cols, visible, &mut responses),
                                Row::File(si, fi) => self.file_cells(&mut row, (si, fi), &cols, visible, &mut responses, actions),
                            }
                            let Some(mut response) = responses.pop() else { return };
                            for other in responses {
                                response = response.union(other);
                            }
                            if response.clicked() {
                                actions.push(Action::Click { index, ctrl: modifiers.command, shift: modifiers.shift });
                            }
                            // Drag over the rows to select a range (Ctrl: add it to the selection).
                            if response.drag_started_by(egui::PointerButton::Primary) {
                                actions.push(Action::DragStart { index, ctrl: modifiers.command });
                            } else if dragging && response.contains_pointer() {
                                actions.push(Action::DragOver(index));
                            }
                            if response.double_clicked() {
                                if let Row::Show(si) = r {
                                    actions.push(Action::Toggle(self.shows[si].root.clone()));
                                }
                            }
                            if response.secondary_clicked() {
                                actions.push(Action::RightClick(index));
                            }
                            // A show opens its folder; a file opens its folder with the file selected.
                            let folder = match r {
                                Row::Show(si) => self.shows[si].root.clone(),
                                Row::Disc(..) => path.clone(),
                                Row::File(si, fi) => self.shows[si].files[fi].clone(),
                            };
                            let busy = self.running.is_some();
                            let flac_level = self.opt.flac_level;
                            let trackers = tracker_entries(&self.opt);
                            let has_lossless = self.reports.contains_key(&(ReportKind::Lossless, path.clone()));
                            let has_speed = self.reports.contains_key(&(ReportKind::Speed, path.clone()));
                            // Set from inside: egui 0.29 only returns Some when the menu closes.
                            response.context_menu(|ui| fit_window(ui, |ui| {
                                menu_open = true;
                                if ui.button("📁 Open folder").on_hover_text("Open the folder in the file manager (for a file, with the file selected)").clicked() {
                                    ui.close_menu();
                                    actions.push(Action::OpenFolder(folder.clone()));
                                }
                                if ui.add_enabled(!busy, egui::Button::new("🖼 Share card...")).on_hover_text(SHARE_TIP).clicked() {
                                    ui.close_menu();
                                    actions.push(Action::ShareCard(None));
                                }
                                ui.separator();
                                ops_menu(ui, busy, flac_level, &trackers, actions);
                                ui.separator();
                                for (kind, has, text, first) in [
                                    (ReportKind::Lossless, has_lossless, "🔍 See lossless analysis...", "Run Lossless? first"),
                                    (ReportKind::Speed, has_speed, "⏱ See speed analysis...", "Run Speed? first"),
                                ] {
                                    if ui.add_enabled(has, egui::Button::new(text)).on_disabled_hover_text(first).clicked() {
                                        ui.close_menu();
                                        actions.push(Action::OpenReport(kind, path.clone()));
                                    }
                                }
                                if ui.add_enabled(!busy, egui::Button::new("Remove from list")).clicked() {
                                    ui.close_menu();
                                    actions.push(Action::Remove);
                                }
                            }));
                        });
                        // Pointer above or below the rows while dragging: the selection reaches
                        // the first or last row shown (which moves as the list scrolls).
                        if let (Some(above), true) = (edge, first_shown <= last_shown) {
                            let outside = pointer.is_some_and(|p| !visible.contains(p));
                            if outside {
                                actions.push(Action::DragOver(if above { first_shown } else { last_shown }));
                            }
                        }
                    });
            });
            self.menu_open |= menu_open;
            // The empty space below the rows: a click clears the selection; a drag selects from
            // the last row up to the row the pointer reaches.
            let rest = ui.available_rect_before_wrap();
            if rest.height() > 0.0 {
                let response = ui.allocate_rect(rest, egui::Sense::click_and_drag());
                if response.clicked() {
                    actions.push(Action::Deselect);
                }
                let rows = self.rows().len();
                if response.drag_started_by(egui::PointerButton::Primary) && rows > 0 {
                    actions.push(Action::DragStart { index: rows - 1, ctrl: modifiers.command });
                }
            }
        });
    }

    /// The Result cell: wrapped to the width in view (as `table` measures the rows), truncated
    /// when hardly any of it is in view.
    fn status_cell(&self, ui: &mut egui::Ui, path: &Path) -> egui::Response {
        let wrap = visible_width(ui);
        match self.status.get(path) {
            Some((text, level, tooltip)) => {
                let tip = if tooltip.is_empty() { text } else { tooltip };
                // A dot of the level's colour, then the text in the ordinary colour, in a
                // column of its own (a wrapped label would come back under the dot).
                let left = ui.cursor().left();
                ui.add_space(DOT_WIDTH);
                let stale = self.stale(path);
                let faded = ui.visuals().weak_text_color().gamma_multiply(0.8);
                let label = egui::Label::new(if stale { RichText::new(text).color(faded) } else { RichText::new(text) });
                let one_line = wrap > DOT_WIDTH && text_width(ui, text, egui::TextStyle::Body.resolve(ui.style()).size) <= wrap - DOT_WIDTH;
                let response = if one_line || wrap <= DOT_WIDTH {
                    // On one line: centred in the row like the other cells.
                    ui.add(label.truncate())
                } else {
                    ui.vertical(|ui| {
                        ui.set_max_width(wrap - DOT_WIDTH);
                        ui.add(label.wrap())
                    })
                    .inner
                };
                let line = ui.text_style_height(&egui::TextStyle::Body);
                let dot = if stale { faded.gamma_multiply(0.5) } else { vivid(*level) };
                ui.painter().circle_filled(egui::pos2(left + 4.0, response.rect.top() + line / 2.0), 4.0, dot);
                let tip = if stale { format!("Before {}: {tip}", self.running.as_ref().map(|r| r.name).unwrap_or("")) } else { tip.to_string() };
                response.on_hover_text(tip)
            }
            None => ui.label(""),
        }
    }

    /// A Source or Speed cell: its verdict is a link to the full analysis. The click is read on
    /// the cell itself (part of the row), never on a clickable label, which would steal the row's clicks.
    fn analysis_col(&self, kind: ReportKind, row: &mut egui_extras::TableRow, path: &Path, responses: &mut Vec<egui::Response>, actions: &mut Vec<Action>) {
        let verdicts = match kind {
            ReportKind::Lossless => &self.source,
            ReportKind::Speed => &self.speed,
        };
        let mut cell = row.col(|ui| {
            Self::verdict_cell(ui, verdicts.get(path));
        }).1;
        if self.reports.contains_key(&(kind, path.to_path_buf())) {
            cell = cell.on_hover_cursor(egui::CursorIcon::PointingHand);
            if cell.clicked() {
                actions.push(Action::OpenReport(kind, path.to_path_buf()));
            }
        }
        responses.push(cell);
    }

    fn verdict_cell(ui: &mut egui::Ui, verdict: Option<&(String, Level, String)>) -> egui::Response {
        match verdict {
            Some((text, level, tooltip)) => {
                let response = badge(ui, format!("{}{text}", mark(*level)), *level);
                let tip = if tooltip.is_empty() { text } else { tooltip };
                response.on_hover_text(tip)
            }
            None => ui.label(""),
        }
    }

    /// `band`: the part of the list in view, across which a show row is tinted.
    fn show_cells(&self, row: &mut egui_extras::TableRow, si: usize, cols: &[Col], band: egui::Rect, responses: &mut Vec<egui::Response>, actions: &mut Vec<Action>) {
        let show = &self.shows[si];
        let infos: Vec<&AudioInfo> = show.files.iter().filter_map(|f| self.info.get(f)).collect();
        let expanded = !self.collapsed.contains(&show.root);
        // The show rows stand out on their own tint across the list (the selection colour
        // when selected), painted by the first cell under all the others.
        let fill = (!self.selected.contains(&show.root)).then(|| Palette::now().show_row);
        let shade = |ui: &mut egui::Ui| {
            if let Some(fill) = fill {
                let rect = egui::Rect::from_x_y_ranges(band.x_range(), ui.max_rect().y_range());
                // The cell's own painter is clipped to the cell: the layer's, to the list.
                ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(band).rect_filled(rect, 0.0, fill);
            }
            self.glow(ui, &show.root, band);
        };
        for &col in cols {
            match col {
                Col::Name => responses.push(row.col(|ui| {
                    shade(ui);
                    let arrow = if expanded { "⏷" } else { "⏵" };
                    if ui.add(egui::Label::new(arrow).sense(egui::Sense::click())).clicked() {
                        actions.push(Action::Toggle(show.root.clone()));
                    }
                    if let Some(ring) = self.ring(show) {
                        let size = if self.opt.compact { 13.0 } else { 16.0 };
                        let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
                        let tip = match ring {
                            Ring::Running(fraction) => {
                                paint_ring(ui.painter(), rect.center(), size / 2.0 - 2.0, fraction, Palette::now());
                                format!("{}: {:.0} % of this show done", self.running.as_ref().map(|r| r.label.as_str()).unwrap_or(""), fraction * 100.0)
                            }
                            Ring::Done(level) => {
                                // Dark ink on the yellow, white on the others.
                                let ink = if level == Level::Warn { Color32::from_rgb(70, 50, 0) } else { Color32::WHITE };
                                paint_mark(ui.painter(), rect.center(), size / 2.0, level, vivid(level), ink);
                                let count = |l: Level| show.files.iter().filter(|f| self.status.get(*f).is_some_and(|s| s.1 == l)).count();
                                match level {
                                    Level::Error => format!("{} with an error", crate::ops::plural(count(Level::Error), "file", "files")),
                                    Level::Warn => format!("{} to check", crate::ops::plural(count(Level::Warn), "file", "files")),
                                    _ => "No problem in the last task".to_string(),
                                }
                            }
                        };
                        response.on_hover_text(tip);
                    }
                    if let Some(Some((kind, from_info))) = self.lineage.get(&show.root) {
                        let (fill, ink) = lineage_colours(kind, DARK_THEME.load(Ordering::Relaxed));
                        let size = if self.opt.compact { 10.0 } else { 11.0 };
                        let whence = if *from_info { "its info file" } else { "the folder name" };
                        let what = match *kind {
                            "SBD" => "soundboard",
                            "AUD" => "audience",
                            "MTX" => "matrix (soundboard and audience mixed)",
                            "FM" => "FM broadcast",
                            "Pre-FM" => "pre-FM (the feed before broadcast)",
                            _ => "DAT master",
                        };
                        pill(ui, kind.to_string(), egui::FontId::new(size, crate::fonts::bold()), fill, ink)
                            .on_hover_text(format!("Recording source, as {whence} says: {what}"));
                    }
                    let parent_is_show = show.root.parent().is_some_and(|p| self.shows.iter().any(|s| s.root == p))
                        && !crate::files::is_output_dir(&file_name(&show.root));
                    let label = if parent_is_show {
                        format!("{}/{}", show.root.parent().map(file_name).unwrap_or_default(), file_name(&show.root))
                    } else {
                        crate::files::show_name(&show.root)
                    };
                    ui.label(RichText::new(label).family(crate::fonts::bold())).on_hover_text(show.root.display().to_string());
                }).1),
                Col::Format => {
                    let mut labels: Vec<String> = infos.iter().map(|i| i.label()).collect();
                    labels.sort();
                    labels.dedup();
                    responses.push(row.col(|ui| { ui.label(labels.join(", ")); }).1);
                }
                Col::Length => responses.push(row.col(|ui| {
                    if !infos.is_empty() {
                        let (total, complete) = show_length(&show.files, &self.info);
                        let suffix = if complete { "" } else { " +" };
                        ui.label(format!("{}{suffix}", fmt_total(total)));
                    }
                }).1),
                Col::Rate | Col::Bits | Col::Ch | Col::Wave => responses.push(row.col(|_| {}).1),
                Col::Sbe => responses.push(row.col(|ui| {
                    // Only the SBEs that matter: not those on the last track of a disc.
                    let sbe = (0..show.files.len())
                        .filter(|&fi| !last_of_disc(show, fi))
                        .filter(|&fi| self.info.get(&show.files[fi]).and_then(|i| i.sbe()) == Some(true))
                        .count();
                    if sbe > 0 {
                        badge(ui, sbe.to_string(), Level::Warn)
                            .on_hover_text(format!("{sbe} track(s) with a sector boundary error before the last track of a disc: Fix SBE corrects them"));
                    }
                }).1),
                Col::Source => self.analysis_col(ReportKind::Lossless, row, &show.root, responses, actions),
                Col::Speed => self.analysis_col(ReportKind::Speed, row, &show.root, responses, actions),
                Col::Result => responses.push(row.col(|ui| { self.status_cell(ui, &show.root); }).1),
            }
        }
    }

    /// A disc of a show: its folder name, number of tracks and length, under a rule.
    fn disc_cells(&self, row: &mut egui_extras::TableRow, si: usize, fi: usize, cols: &[Col], band: egui::Rect, responses: &mut Vec<egui::Response>) {
        let p = Palette::now();
        let show = &self.shows[si];
        let folder = show.files[fi].parent().unwrap_or(&show.root);
        let files: Vec<PathBuf> = show.files.iter().filter(|f| f.parent() == Some(folder)).cloned().collect();
        for &col in cols {
            match col {
                Col::Name => responses.push(row.col(|ui| {
                    let top = ui.max_rect().top() + 0.5;
                    ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(band).hline(band.x_range(), top, egui::Stroke::new(1.0f32, p.border));
                    ui.add_space(22.0);
                    let name = if folder == show.root { "Show folder".to_string() } else { file_name(folder) };
                    ui.label(RichText::new(name.to_uppercase()).size(11.0).family(crate::fonts::bold()).color(p.accent)).on_hover_text(folder.display().to_string());
                    ui.label(RichText::new(crate::ops::plural(files.len(), "track", "tracks")).size(12.0).color(p.weak));
                }).1),
                Col::Length => responses.push(row.col(|ui| {
                    let (total, complete) = show_length(&files, &self.info);
                    if total > 0.0 {
                        ui.label(RichText::new(format!("{}{}", fmt_total(total), if complete { "" } else { " +" })).size(12.0).color(p.weak));
                    }
                }).1),
                _ => responses.push(row.col(|_| {}).1),
            }
        }
    }

    fn file_cells(&self, row: &mut egui_extras::TableRow, (si, fi): (usize, usize), cols: &[Col], band: egui::Rect, responses: &mut Vec<egui::Response>, actions: &mut Vec<Action>) {
        let show = &self.shows[si];
        let path = &show.files[fi];
        let info = self.info.get(path);
        for &col in cols {
            match col {
                Col::Name => responses.push(row.col(|ui| {
                    self.glow(ui, path, band);
                    // A file the running task has not reached yet: a small empty ring.
                    let left = ui.cursor().left();
                    ui.add_space(22.0);
                    if self.running.as_ref().is_some_and(|r| r.files.contains(path) && !r.done_files.contains(path)) {
                        let centre = egui::pos2(left + 13.0, ui.max_rect().center().y);
                        ui.painter().circle_stroke(centre, 3.5, egui::Stroke::new(1.5f32, Palette::now().weak.gamma_multiply(0.6)));
                    }
                    ui.label(rel_posix(path, &show.root)).on_hover_text(path.display().to_string());
                }).1),
                Col::Format => {
                    let format = match (self.opt.compact, info) {
                        (true, Some(i)) => i.label(),
                        // A container: what it holds, and whether that loses information.
                        (false, Some(i)) if i.format.container() && i.codec.is_some() => {
                            format!("{} · {} ({})", i.format.name(), i.name(), if i.lossy() { "lossy" } else { "lossless" })
                        }
                        _ => crate::audio::Format::from_path(path).map(|f| f.name()).unwrap_or("").to_string(),
                    };
                    responses.push(row.col(|ui| { ui.label(format); }).1);
                }
                Col::Length => responses.push(row.col(|ui| { ui.label(info.map(|i| i.length()).unwrap_or_default()); }).1),
                Col::Wave => responses.push(row.col(|ui| {
                    if let Some(peaks) = self.waves.get(path) {
                        paint_wave(ui, peaks).on_hover_text("Outline of the sound (its peaks, scaled to the loudest), from the last analysis that read the file");
                    }
                }).1),
                Col::Rate => responses.push(row.col(|ui| { ui.label(info.map(|i| i.rate_label()).unwrap_or_default()); }).1),
                Col::Bits => responses.push(row.col(|ui| {
                    ui.label(info.map(|i| if i.lossy() { "-".to_string() } else { i.bits.to_string() }).unwrap_or_default());
                }).1),
                Col::Ch => responses.push(row.col(|ui| { ui.label(info.map(|i| i.channels.to_string()).unwrap_or_default()); }).1),
                Col::Sbe => responses.push(row.col(|ui| {
                    if let Some(i) = info {
                        match i.sbe() {
                            None => { ui.label("-"); }
                            Some(true) if last_of_disc(show, fi) => {
                                ui.label("Last").on_hover_text("Sector boundary error on the last track of the disc: normal, it burns without gaps");
                            }
                            Some(true) => {
                                badge(ui, "Yes", Level::Warn).on_hover_text("Sector boundary error before the last track of the disc: a short gap or click if burned. Fix SBE corrects it");
                            }
                            Some(false) => { ui.label("No"); }
                        }
                    }
                }).1),
                Col::Source => self.analysis_col(ReportKind::Lossless, row, path, responses, actions),
                Col::Speed => self.analysis_col(ReportKind::Speed, row, path, responses, actions),
                Col::Result => responses.push(row.col(|ui| { self.status_cell(ui, path); }).1),
            }
        }
    }

    fn apply(&mut self, ctx: &egui::Context, actions: Vec<Action>) {
        let rows = self.rows();
        for action in actions {
            match action {
                Action::Run(op) => self.run_on_selection(op),
                Action::Toggle(root) => {
                    if !self.collapsed.remove(&root) {
                        self.collapsed.insert(root);
                    }
                }
                Action::DragStart { index, ctrl } => {
                    let base = if ctrl { self.selected.clone() } else { HashSet::new() };
                    self.selected = base.clone();
                    self.selected.insert(self.row_path(rows[index]).to_path_buf());
                    self.anchor = Some(index);
                    self.drag_select = Some((index, base));
                }
                Action::DragOver(index) => {
                    if let Some((start, base)) = &self.drag_select {
                        let (a, b) = if *start <= index { (*start, index) } else { (index, *start) };
                        let mut selected = base.clone();
                        selected.extend(rows[a..=b.min(rows.len() - 1)].iter().map(|r| self.row_path(*r).to_path_buf()));
                        self.selected = selected;
                    }
                }
                Action::Click { index, ctrl, shift } => {
                    let path = self.row_path(rows[index]).to_path_buf();
                    if shift {
                        let from = self.anchor.unwrap_or(index);
                        let (a, b) = if from <= index { (from, index) } else { (index, from) };
                        if !ctrl {
                            self.selected.clear();
                        }
                        for r in &rows[a..=b] {
                            self.selected.insert(self.row_path(*r).to_path_buf());
                        }
                    } else if ctrl {
                        if !self.selected.remove(&path) {
                            self.selected.insert(path);
                        }
                        self.anchor = Some(index);
                    } else {
                        self.selected.clear();
                        self.selected.insert(path);
                        self.anchor = Some(index);
                    }
                }
                Action::RightClick(index) => {
                    let path = self.row_path(rows[index]).to_path_buf();
                    if !self.selected.contains(&path) {
                        self.selected.clear();
                        self.selected.insert(path);
                        self.anchor = Some(index);
                    }
                }
                Action::OpenFolder(path) => {
                    if let Err(e) = show_in_folder(&path) {
                        self.log(format!("Cannot open {}: {e}", path.display()), Level::Error);
                    }
                }
                Action::CopyFfp => self.copy_ffp(ctx),
                Action::CopyInfo => self.copy_info(ctx),
                Action::TagInfo => self.open_tag_dialog(),
                Action::ExtractAudio => self.open_extract_dialog(),
                Action::ShareCard(roots) => {
                    let shows: Vec<Show> = match roots {
                        Some(roots) => self.shows.iter().filter(|s| roots.contains(&s.root)).cloned().collect(),
                        None => self.selection().0,
                    };
                    const MAX: usize = 20;
                    if shows.len() > MAX {
                        self.log(format!("Share cards are made for the first {MAX} shows."), Level::Warn);
                    }
                    let cards: Vec<crate::sharecard::Card> = shows.iter().take(MAX).map(|show| self.card(show)).collect();
                    let files = shows.iter().take(MAX).flat_map(|s| s.files.clone()).collect();
                    self.start(Op::ShareCard(cards), shows.into_iter().take(MAX).collect(), files, false);
                }
                Action::Setlist => {
                    if self.opt.setlistfm_key.trim().is_empty() {
                        self.flash("Enter your setlist.fm API key in Settings first (a free key, from setlist.fm).");
                        self.settings = Some(self.opt.clone());
                    } else {
                        self.run_on_selection(Op::Setlist);
                    }
                }
                Action::TorrentCheck => self.ask(Dialog::TorrentCheck),
                Action::Remove => self.remove_selected(),
                Action::ToggleColumn(col) => {
                    self.fit_columns = true;
                    let title = col.title().to_string();
                    if !self.opt.hidden_columns.contains(&title) {
                        self.opt.hidden_columns.push(title);
                    } else {
                        self.opt.hidden_columns.retain(|t| *t != title);
                    }
                }
                Action::FlacLevel(level) => self.opt.flac_level = level,
                Action::Compact(on) => {
                    self.opt.compact = on;
                    self.fit_columns = true;
                }
                Action::CollapsedView(on) => {
                    self.opt.collapsed_view = on;
                    self.collapsed = if on { self.shows.iter().map(|s| s.root.clone()).collect() } else { HashSet::new() };
                }
                Action::LogOpen(open) => self.opt.log_open = open,
                Action::AlwaysOnTop(on) => {
                    self.opt.always_on_top = on;
                    set_window_level(ctx, on);
                }
                Action::Theme(choice) => {
                    self.opt.theme = choice;
                    self.apply_theme(ctx);
                }
                Action::ShowAllColumns => {
                    self.opt.hidden_columns.clear();
                    self.fit_columns = true;
                }
                Action::OpenReport(kind, path) => {
                    if !self.open_reports.contains(&(kind, path.clone())) {
                        self.open_reports.push((kind, path));
                    }
                }
                Action::SelectAll => {
                    // The rows in view: with a filter, not the files it hides.
                    self.selected = self.rows().into_iter().map(|r| self.row_path(r).to_path_buf()).collect();
                    self.anchor = None;
                }
                Action::Deselect => {
                    self.selected.clear();
                    self.anchor = None;
                }
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        // The info file choice window takes the keys (arrows, Enter, Escape). The Tag and
        // rename windows are system windows with keys of their own: they stay open while
        // the list is used (Ctrl+A, Delete...).
        if ctx.wants_keyboard_input() || self.info_choice.is_some() || self.float_dialog.is_some() {
            return;
        }
        let busy = self.running.is_some();
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F1)) {
            self.help = true;
        }
        ctx.input_mut(|i| {
            if !busy && !self.shows.is_empty() {
                if i.consume_key(Modifiers::NONE, Key::F5) { actions.push(Action::Run(Op::Verify)); }
                if i.consume_key(Modifiers::NONE, Key::F6) { actions.push(Action::Run(Op::Create)); }
                if i.consume_key(Modifiers::NONE, Key::F7) { actions.push(Action::Run(Op::Test)); }
                if i.consume_key(Modifiers::NONE, Key::F8) { actions.push(Action::Run(Op::Details { verbose: true })); }
                if i.consume_key(Modifiers::NONE, Key::F9) { actions.push(Action::Run(Op::Spectrogram)); }
                if i.consume_key(Modifiers::COMMAND, Key::L) { actions.push(Action::Run(Op::Lossless)); }
                if i.consume_key(Modifiers::COMMAND, Key::P) { actions.push(Action::Run(Op::Speed)); }
                if i.consume_key(Modifiers::COMMAND, Key::I) { actions.push(Action::Run(Op::MediaInfo(Vec::new()))); }
                if i.consume_key(Modifiers::COMMAND, Key::A) { actions.push(Action::SelectAll); }
                if i.consume_key(Modifiers::NONE, Key::F10) { actions.push(Action::Run(Op::Torrent(Tracker::all(&self.opt)))); }
                if i.consume_key(Modifiers::NONE, Key::Delete) { actions.push(Action::Remove); }
            }
        });
        // Escape stops a running operation, otherwise clears the selection
        // (unless it is closing an open menu).
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if busy {
                self.stop();
            } else if self.help || self.about {
                self.help = false;
                self.about = false;
            } else if !self.menu_open && !ctx.memory(|m| m.any_popup_open()) {
                actions.push(Action::Deselect);
            }
        }
    }

    /// Files dragged over the window: a dashed frame over the faded list, the cassette
    /// turning with the first folder's name on it, and what the drop will do.
    fn drop_frame(&self, ctx: &egui::Context, ctrl: bool) {
        let p = Palette::now();
        let hovered: Vec<PathBuf> = ctx.input(|i| i.raw.hovered_files.iter().filter_map(|f| f.path.clone()).collect());
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop")));
        let screen = ctx.screen_rect();
        painter.rect_filled(screen, 0.0, p.ground.gamma_multiply(0.7));
        let frame = screen.shrink(14.0);
        painter.rect_filled(frame, 14.0, p.tint.gamma_multiply(0.88));
        painter.extend(egui::Shape::dashed_line(&rounded_outline(frame, 14.0), egui::Stroke::new(2.5f32, p.accent), 10.0, 7.0));
        let label = match hovered.first() {
            Some(first) if first.is_dir() => file_name(first),
            Some(first) => first.parent().map(file_name).unwrap_or_default(),
            None => "New shows".to_string(),
        };
        let size = egui::vec2(220.0, 141.0).min(frame.size() * 0.45);
        let cassette = egui::Rect::from_center_size(frame.center() - egui::vec2(0.0, 40.0), egui::vec2(size.x, size.x * 0.64));
        paint_cassette(&painter, cassette, &label, ctx.input(|i| i.time) as f32 * 12.0, 0.35);
        let torrents = !hovered.is_empty() && hovered.iter().all(|f| crate::files::ext_lower(f) == "torrent");
        let what = if hovered.iter().all(|f| f.is_dir()) {
            crate::ops::plural(hovered.len(), "folder", "folders")
        } else if hovered.iter().all(|f| f.is_file()) {
            crate::ops::plural(hovered.len(), "file", "files")
        } else {
            crate::ops::plural(hovered.len(), "item", "items")
        };
        let (headline, hint) = match (torrents, hovered.is_empty(), ctrl) {
            (true, _, _) => (format!("Drop to check the list against {}", crate::ops::plural(hovered.len(), "torrent", "torrents")), String::new()),
            (_, true, false) => ("Drop to replace the list".to_string(), "Hold Ctrl to add to it instead".to_string()),
            (_, true, true) => ("Drop to add to the list".to_string(), String::new()),
            (_, false, false) => (format!("Drop to open {what}"), "They replace the list · hold Ctrl to add them to it".to_string()),
            (_, false, true) => (format!("Drop to add {what} to the list"), String::new()),
        };
        let mut y = cassette.bottom() + 22.0;
        let headline = painter.layout_no_wrap(headline, egui::FontId::new(21.0, crate::fonts::bold()), p.accent);
        painter.galley(egui::pos2(frame.center().x - headline.size().x / 2.0, y), headline.clone(), p.accent);
        y += headline.size().y + 6.0;
        if !hint.is_empty() {
            let hint = painter.layout_no_wrap(hint, egui::FontId::proportional(14.0), p.weak);
            painter.galley(egui::pos2(frame.center().x - hint.size().x / 2.0, y), hint, p.weak);
        }
        ctx.request_repaint();
    }

    fn drops(&mut self, ctx: &egui::Context) {
        let (dropped, ctrl, hovering) = ctx.input(|i| {
            let paths: Vec<PathBuf> = i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect();
            (paths, i.modifiers.command, !i.raw.hovered_files.is_empty())
        });
        if hovering {
            self.drop_frame(ctx, ctrl);
        }
        // Dropped .torrent files are checked against the shows (or the folders beside them).
        let (torrents, dropped): (Vec<PathBuf>, Vec<PathBuf>) =
            dropped.into_iter().partition(|p| crate::files::ext_lower(p) == "torrent" && p.is_file());
        if !dropped.is_empty() {
            self.add_paths(dropped, ctrl);
        }
        if !torrents.is_empty() {
            if self.running.is_some() {
                self.pending_torrents = Some(torrents);
            } else {
                self.run_on_selection(Op::TorrentCheck(torrents));
            }
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.settings.take() else { return };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        // The settings scroll when the application window is too small for them; the buttons
        // stay right under them, always visible.
        let room = (ctx.screen_rect().height() - 130.0).max(120.0);
        egui::Window::new("Settings")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(room).auto_shrink([true, true]).show(ui, |ui| {
                ui.heading("Checksum files to create");
                ui.checkbox(&mut draft.create_ffp, "FFP (FLAC fingerprints)");
                ui.checkbox(&mut draft.create_md5, "MD5 (whole files)");
                ui.checkbox(&mut draft.create_st5, "ST5 (audio data fingerprints, shntool format)");
                ui.checkbox(&mut draft.create_sfv, "SFV (CRC32 of whole files, found on older sets)");
                ui.checkbox(&mut draft.overwrite, "Overwrite existing checksum files");
                ui.checkbox(&mut draft.crlf, "Windows line endings (CRLF)");
                ui.separator();
                ui.heading("MP3 copies");
                egui::Grid::new("mp3_grid").num_columns(2).show(ui, |ui| {
                    ui.label("Save MP3 copies in");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut draft.mp3_dir).hint_text("Next to the show folder").desired_width(320.0));
                        if ui.button("...").clicked() {
                            self.ask(Dialog::Mp3Dir);
                        }
                    });
                    ui.end_row();
                });
                ui.separator();
                ui.heading("Torrents");
                egui::Grid::new("torrent_grid").num_columns(2).show(ui, |ui| {
                    ui.label("DIME announce URL");
                    ui.add(egui::TextEdit::singleline(&mut draft.announce_dime).desired_width(380.0));
                    ui.end_row();
                    ui.label("The Traders' Den announce URL");
                    ui.add(egui::TextEdit::singleline(&mut draft.announce_ttd).desired_width(380.0));
                    ui.end_row();
                    ui.label("Save torrents in");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut draft.torrent_dir).hint_text("Next to the show folder").desired_width(320.0));
                        if ui.button("...").clicked() {
                            self.ask(Dialog::TorrentDir);
                        }
                    });
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.label(RichText::new("Other trackers").strong())
                    .on_hover_text("Each one gets its own torrent, named \"<show> [Name].torrent\", and its entry in the Torrent menus.");
                let mut remove = None;
                // Sizes are forced: inside a grid, text fields would otherwise shrink to a few letters.
                let field_height = ui.spacing().interact_size.y;
                egui::Grid::new("custom_trackers").num_columns(2).show(ui, |ui| {
                    for (i, t) in draft.custom_trackers.iter_mut().enumerate() {
                        ui.add_sized([220.0, field_height], egui::TextEdit::singleline(&mut t.name).hint_text("Tracker name"));
                        ui.horizontal(|ui| {
                            ui.add_sized([340.0, field_height], egui::TextEdit::singleline(&mut t.announce).hint_text("Tracker announce URL"));
                            if ui.button("🗑").on_hover_text("Remove this tracker").clicked() {
                                remove = Some(i);
                            }
                        });
                        ui.end_row();
                    }
                });
                if let Some(i) = remove {
                    draft.custom_trackers.remove(i);
                }
                if ui.button("+ Add a tracker").clicked() {
                    draft.custom_trackers.push(ops::CustomTracker::default());
                }
                ui.add_space(4.0);
                ui.checkbox(&mut draft.torrent_private, "Private torrent (no DHT or peer exchange)");
                ui.checkbox(&mut draft.torrent_source, "Tag each torrent with its site, so the same show can be seeded on several sites at once");
                ui.checkbox(&mut draft.open_after_torrent, "Open the .ffp and the info file once the torrent is made, to copy them into the upload form");
                ui.separator();
                ui.heading("setlist.fm");
                ui.horizontal(|ui| {
                    ui.label("API key");
                    ui.add(egui::TextEdit::singleline(&mut draft.setlistfm_key).password(true).hint_text("Your own key").desired_width(320.0))
                        .on_hover_text("Used by Tools > Get setlist from setlist.fm and by the Tag and rename window. \
                                        setlist.fm is asked only then, with the artist and the date of the show.");
                    ui.hyperlink_to("Get a free key", "https://www.setlist.fm/settings/api");
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Files processed in parallel");
                    ui.add(egui::DragValue::new(&mut draft.jobs).range(1..=64)).on_hover_text(
                        "Also the most processor threads used: when fewer files are left, the free threads help \
                         with the ones still running. The work runs at a low priority, so the computer stays \
                         responsive.",
                    );
                });
                ui.checkbox(&mut draft.auto_details, "Read file details automatically when adding files");
                ui.checkbox(&mut draft.aac_check, "Lossless?: look for AAC and Opus coding traces in every file (slower)")
                    .on_hover_text("Finds files decoded from an AAC (M4A, iTunes, streams) or an Opus stream (YouTube) and \
                                    converted back to a lossless format, even at high bitrates and resampled to 44.1 kHz.\n\
                                    Without this option they are looked for only in the files with a doubtful lowpass \
                                    (Lossy?, Unsure, 15 kHz lowpass). With it, in every file: the analysis then takes \
                                    several times longer, about 10 seconds of processor time per track, spread over the \
                                    files processed in parallel.\n\
                                    MP3 coding traces are always looked for.");
                ui.horizontal(|ui| {
                    ui.label("Tag and rename from info file:");
                    ui.checkbox(&mut draft.tag_write, "write the tags");
                    ui.checkbox(&mut draft.tag_rename, "rename the files");
                })
                .response
                .on_hover_text("What the Tag and rename window does by default. Each show can still be changed in the window.");
                ui.checkbox(&mut draft.check_updates, "Check for a new version at startup")
                    .on_hover_text("Asks github.com once at each start whether a newer version has been released. \
                                    Nothing is sent but the program's name and version. \
                                    Otherwise the program only connects to setlist.fm, when you ask for a setlist.");
                ui.checkbox(&mut draft.shell_integration, "Add \"Open in Bootleg Toolbox\" to the file manager's right-click menu")
                    .on_hover_text("For folders and audio files, for your user only (no administrator rights). \
                                    Windows: Explorer (on Windows 11, under Show more options). Linux: Dolphin, Nautilus and Nemo.\n\
                                    Untick it to remove the entry.");
                ui.horizontal(|ui| {
                    ui.label("Appearance");
                    ui.radio_value(&mut draft.theme, ThemeChoice::System, "Follow the system");
                    ui.radio_value(&mut draft.theme, ThemeChoice::Light, "Light");
                    ui.radio_value(&mut draft.theme, ThemeChoice::Dark, "Dark");
                });
                ui.horizontal(|ui| {
                    ui.label("Accent");
                    let dark = DARK_THEME.load(Ordering::Relaxed);
                    for accent in ops::Accent::ALL {
                        // Each name in its own colour.
                        let saved = ACCENT.swap(ops::Accent::ALL.iter().position(|a| *a == accent).unwrap_or(0) as u8, Ordering::Relaxed);
                        let colour = Palette::of(dark).accent;
                        ACCENT.store(saved, Ordering::Relaxed);
                        ui.radio_value(&mut draft.accent, accent, RichText::new(accent.name()).color(colour).family(crate::fonts::bold()));
                    }
                })
                .response
                .on_hover_text("The colour of buttons, links, selection and progress (the status colours stay green, yellow and red)");
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    save = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                if ui.button("Reset to defaults").clicked() {
                    // The API key is the user's own, not a setting.
                    draft = Options { setlistfm_key: draft.setlistfm_key.clone(), ..Options::default() };
                }
            });
        });
        cancel |= ctx.input(|i| i.key_pressed(Key::Escape));
        if save {
            if draft.announce_dime.trim().is_empty() {
                draft.announce_dime = Tracker::Dime.default_announce().into();
            }
            if draft.announce_ttd.trim().is_empty() {
                draft.announce_ttd = Tracker::Ttd.default_announce().into();
            }
            draft.custom_trackers.retain(|t| !t.name.trim().is_empty() || !t.announce.trim().is_empty());
            if draft.shell_integration != self.opt.shell_integration {
                let done = if draft.shell_integration { crate::integration::install() } else { crate::integration::remove() };
                match done {
                    Ok(text) => self.log(text, Level::Ok),
                    Err(e) => {
                        self.log(format!("Right-click menu entry: {e}"), Level::Error);
                        draft.shell_integration = !draft.shell_integration;
                    }
                }
            }
            self.opt = draft;
            self.apply_theme(ctx);
        } else if open && !cancel {
            self.settings = Some(draft);
        }
    }

    fn after_flac_window(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.after_flac.take() else { return };
        let (mut apply, mut dismiss) = (false, false);
        egui::Window::new("Conversion to FLAC finished")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!("{} file(s) converted to FLAC and verified.", dialog.sources.len() + dialog.rounded));
                if dialog.rounded > 0 {
                    ui.label(format!("{} of them were 32-bit float rounded to 24-bit: their originals are kept, and not offered for the trash.", dialog.rounded));
                }
                if dialog.failed > 0 {
                    ui.label(colored(
                        format!("{} file(s) were not converted (error, or FLAC already there): their originals are kept.", dialog.failed),
                        Level::Warn,
                    ));
                }
                ui.add_space(6.0);
                if !dialog.sources.is_empty() {
                    ui.checkbox(&mut dialog.trash, format!("Move the {} original file(s) to the trash (only the ones converted successfully)", dialog.sources.len()));
                }
                if !dialog.old.is_empty() {
                    let mut names: Vec<String> = dialog.old.iter().take(8).map(|p| format!("    {}", file_name(p))).collect();
                    if dialog.old.len() > 8 {
                        names.push("    ...".into());
                    }
                    ui.checkbox(&mut dialog.trash_old, format!("Move the old checksum files to the trash, as they list the original files:\n{}", names.join("\n")));
                }
                ui.checkbox(&mut dialog.ffp, "Create an FFP file for each show");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    apply = ui.button("Apply").clicked();
                    dismiss = ui.button("Do nothing").clicked();
                });
            });
        if apply {
            self.opt.after_flac_trash = dialog.trash;
            self.opt.after_flac_ffp = dialog.ffp;
            if !dialog.old.is_empty() {
                self.opt.after_flac_old = dialog.trash_old;
            }
            let trash_checksums = if dialog.trash_old { dialog.old.clone() } else { Vec::new() };
            if dialog.trash || !trash_checksums.is_empty() || dialog.ffp {
                let op = Op::AfterFlac {
                    roots: dialog.roots.clone(),
                    sources: dialog.sources.clone(),
                    trash_sources: dialog.trash,
                    trash_checksums,
                    create_ffp: dialog.ffp,
                };
                self.start(op, Vec::new(), dialog.sources, false);
            } else {
                self.auto_details();
            }
        } else if dismiss {
            self.auto_details();
        } else {
            self.after_flac = Some(dialog);
        }
    }

    fn help_window(&mut self, ctx: &egui::Context) {
        use crate::help;
        let mut open = self.help;
        let size = ctx.screen_rect().size();
        egui::Window::new("Keyboard shortcuts and command line")
            .collapsible(false)
            .open(&mut open)
            .default_size([(size.x - 40.0).min(760.0), (size.y - 80.0).min(620.0)])
            .max_height(size.y - 60.0)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("📋 Copy").on_hover_text("Copy this help as text").clicked() {
                        ui.ctx().copy_text(help::text());
                    }
                    ui.label(RichText::new("Also printed by bootleg-toolbox --help").weak());
                });
                ui.separator();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    let grid = |ui: &mut egui::Ui, id: &str, rows: &[(&str, &str)], mono: bool| {
                        egui::Grid::new(id).num_columns(2).striped(true).spacing([16.0, 4.0]).show(ui, |ui| {
                            for (key, text) in rows {
                                let key = RichText::new(*key).strong();
                                ui.label(if mono { key.monospace() } else { key });
                                ui.add(egui::Label::new(*text).wrap());
                                ui.end_row();
                            }
                        });
                    };
                    let heading = |ui: &mut egui::Ui, text: &str| {
                        ui.add_space(8.0);
                        ui.label(RichText::new(text).size(16.0).strong());
                        ui.add_space(2.0);
                    };
                    heading(ui, "Keyboard shortcuts");
                    grid(ui, "help keys", help::SHORTCUTS, false);
                    heading(ui, "Mouse and drag and drop");
                    grid(ui, "help mouse", help::MOUSE, false);
                    heading(ui, "Command line");
                    ui.label(RichText::new(help::CLI_USAGE).monospace());
                    ui.add_space(4.0);
                    for note in help::CLI_NOTES {
                        ui.label(*note);
                    }
                    heading(ui, "Commands");
                    grid(ui, "help commands", help::COMMANDS, true);
                    heading(ui, "Environment variables");
                    grid(ui, "help variables", help::VARIABLES, true);
                    heading(ui, "Examples");
                    for example in help::CLI_EXAMPLES {
                        ui.label(RichText::new(*example).monospace());
                    }
                });
            });
        self.help = open;
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        let mut open = self.about;
        egui::Window::new(format!("About {APP_NAME}")).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            ui.heading(format!("{APP_NAME} {APP_VERSION}"));
            ui.label("Checksums, decode tests, torrents, conversions (FLAC, WAV, MP3) and SBE fixing for lossless concert recordings.");
            ui.label("A single program with no external tool.");
            ui.add_space(6.0);
            ui.label("By Rattlehead80. Free software under the GNU General Public License v3.0 or later, with no warranty.");
            ui.add_space(8.0);
            ui.label(RichText::new("Built with these open-source components").strong());
            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                egui::Grid::new("about_components").num_columns(3).striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                    for (name, role, licence) in COMPONENTS {
                        ui.label(*name);
                        ui.label(*role);
                        ui.label(RichText::new(*licence).weak());
                        ui.end_row();
                    }
                });
            });
            ui.add_space(4.0);
            ui.label(RichText::new("With the libraries these rely on: the full list and the licence texts are in THIRD-PARTY.md, in the source code.").weak());
        });
        self.about = open;
    }
}

/// A white card with a thin border, on the pale ground of a window.
fn card<R>(ui: &mut egui::Ui, p: Palette, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::none()
        .fill(p.ground)
        .stroke(egui::Stroke::new(1.0f32, p.border))
        .rounding(12.0)
        .inner_margin(egui::Margin::symmetric(16.0, 12.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// The section of a lossless report a measurement belongs to, by its name.
fn report_section(name: &str) -> &'static str {
    let name = name.to_lowercase();
    if name.contains("coding traces") || name.contains("lossywav") {
        "Coding traces"
    } else if ["frequenc", "15 khz", "cutoff", "lowpass", "hi-res", "holes"].iter().any(|k| name.contains(k)) {
        "Frequency content"
    } else {
        "Stereo, FM and bits"
    }
}

/// One measurement: a dot of its level, its name, its value and what it means.
fn measure_line(ui: &mut egui::Ui, p: Palette, m: &crate::report::Measure, extra: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal_top(|ui| {
        let line = ui.text_style_height(&egui::TextStyle::Body);
        let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, line), egui::Sense::hover());
        let fill = if m.level == Level::Info { p.weak.gamma_multiply(0.45) } else { vivid(m.level) };
        ui.painter().circle_filled(dot.center(), 4.0, fill);
        ui.allocate_ui_with_layout(egui::vec2(200.0, line), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_width(200.0);
            ui.add(egui::Label::new(RichText::new(&m.name).color(p.weak)).wrap());
        });
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::Label::new(colored(&m.value, m.level).family(crate::fonts::bold())).wrap());
                extra(ui);
            });
            if !m.meaning.is_empty() {
                ui.add(egui::Label::new(RichText::new(&m.meaning).size(12.0).color(p.weak)).wrap());
            }
        });
    });
    ui.add_space(4.0);
}

/// "See more" as cards: the verdict, then the measurements grouped in sections that fold
/// (open when they hold a doubt or an error), the tracks of a show, the reasoning.
fn report_cards(
    ui: &mut egui::Ui,
    p: Palette,
    report: &crate::report::Report,
    (kind, path): (ReportKind, &Path),
    visual: impl FnOnce(&mut egui::Ui),
    opened: &mut Vec<(ReportKind, PathBuf)>,
) {
    ui.spacing_mut().item_spacing.y = 10.0;
    ui.add_space(4.0);
    card(ui, p, |ui| {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
            let (fill, ink) = p.badge(report.level);
            paint_mark(ui.painter(), rect.center(), 19.0, report.level, fill, ink);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add(egui::Label::new(RichText::new(&report.title).color(p.weak)).truncate());
                ui.label(colored(&report.label, report.level).size(21.0).family(crate::fonts::bold()));
            });
        });
        ui.add_space(2.0);
        ui.add(egui::Label::new(&report.sentence).wrap());
        if !report.key_facts.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for fact in report.key_facts.split(", ") {
                    egui::Frame::none().fill(p.show_row).rounding(8.0).inner_margin(egui::Margin::symmetric(9.0, 3.0)).show(ui, |ui| {
                        ui.label(RichText::new(fact).size(12.0));
                    });
                }
            });
        }
        visual(ui);
    });
    // Sections in a fixed order; a report with none of the named ones (Speed?) has one.
    const ORDER: [&str; 3] = ["Coding traces", "Frequency content", "Stereo, FM and bits"];
    let lossless = report.measures.iter().any(|m| report_section(&m.name) != ORDER[2]);
    let mut sections: Vec<(&str, Vec<&crate::report::Measure>)> = Vec::new();
    for m in &report.measures {
        let name = if lossless { report_section(&m.name) } else { "Measurements" };
        match sections.iter_mut().find(|(n, _)| *n == name) {
            Some((_, list)) => list.push(m),
            None => sections.push((name, vec![m])),
        }
    }
    sections.sort_by_key(|(n, _)| ORDER.iter().position(|o| o == n).unwrap_or(0));
    let rank = |l: Level| match l {
        Level::Error => 3,
        Level::Warn => 2,
        Level::Ok => 1,
        Level::Info => 0,
    };
    for (name, list) in sections {
        let worst = list.iter().map(|m| m.level).max_by_key(|l| rank(*l)).unwrap_or(Level::Info);
        let summary = match worst {
            Level::Error | Level::Warn => list.iter().find(|m| m.level == worst).map(|m| m.name.clone()).unwrap_or_default(),
            Level::Ok => "Nothing unusual".to_string(),
            Level::Info => "For information".to_string(),
        };
        card(ui, p, |ui| {
            let id = ui.make_persistent_id(("report section", kind, path, name));
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, rank(worst) >= 2)
                .show_header(ui, |ui| {
                    ui.label(RichText::new(name).family(crate::fonts::bold()).size(14.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        badge(ui, summary, worst);
                    });
                })
                .body(|ui| {
                    ui.add_space(4.0);
                    for m in list {
                        measure_line(ui, p, m, |_| {});
                    }
                });
        });
    }
    if !report.tracks.is_empty() {
        card(ui, p, |ui| {
            ui.label(RichText::new("Tracks").family(crate::fonts::bold()).size(14.0));
            ui.add_space(4.0);
            for m in &report.tracks {
                measure_line(ui, p, m, |ui| {
                    if ui.small_button("See more").clicked() {
                        opened.push((kind, path.join(&m.name)));
                    }
                });
            }
        });
    }
    if !report.reasoning.is_empty() {
        card(ui, p, |ui| {
            let id = ui.make_persistent_id(("report reasoning", kind, path));
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
                .show_header(ui, |ui| {
                    ui.label(RichText::new("Reasoning").family(crate::fonts::bold()).size(14.0));
                })
                .body(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    let mut step = 0;
                    for line in &report.reasoning {
                        // Indented lines belong to the step above them.
                        match line.strip_prefix("    ") {
                            Some(sub) => {
                                ui.indent(egui::Id::new(("sub", line)), |ui| {
                                    ui.label(sub);
                                });
                            }
                            None => {
                                step += 1;
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(RichText::new(format!("{step}.")).strong());
                                    ui.label(line);
                                });
                            }
                        }
                    }
                });
        });
    }
}

impl App {
    /// "See more": the full analysis (lossless, speed) of a file or a show, in its own window.
    fn report_windows(&mut self, ctx: &egui::Context) {
        self.prepare_plots(ctx);
        let mut closed = Vec::new();
        let mut opened = Vec::new();
        let mut spectrogram = None;
        for (index, (kind, path)) in self.open_reports.iter().enumerate() {
            let Some(report) = self.reports.get(&(*kind, path.clone())).cloned() else {
                closed.push(index);
                continue;
            };
            let is_show = self.shows.iter().any(|s| &s.root == path);
            let id = egui::ViewportId::from_hash_of(("report", *kind, path));
            let window_title = format!("{} - {}", report.heading, report.title);
            let builder = egui::ViewportBuilder::default()
                .with_icon(app_icon())
                .with_window_level(window_level(self.on_top()))
                .with_title(&window_title)
                .with_inner_size([760.0, 640.0])
                .with_min_inner_size([420.0, 300.0]);
            ctx.show_viewport_immediate(id, builder, |vctx, class| {
                let content = |ui: &mut egui::Ui| {
                    egui::TopBottomPanel::bottom(egui::Id::new(("report buttons", index))).show_inside(ui, |ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if !is_show && ui.button("📊 Spectrogram").clicked() {
                                spectrogram = Some(path.clone());
                            }
                            if ui.button("📋 Copy report").on_hover_text("Copy this analysis as text (for a forum post or an info file)").clicked() {
                                vctx.copy_text(report.to_text());
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let p = Palette::now();
                                let close = egui::Button::new(RichText::new("Close").color(Color32::WHITE).family(crate::fonts::bold())).fill(p.accent).min_size(egui::vec2(72.0, 0.0));
                                if ui.add(close).clicked() {
                                    closed.push(index);
                                }
                            });
                        });
                        ui.add_space(2.0);
                    });
                    let p = Palette::now();
                    egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).fill(p.chrome)).show_inside(ui, |ui| {
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            let visual = |ui: &mut egui::Ui| match (*kind, report.speed) {
                                (ReportKind::Speed, Some(percent)) => speed_dial(ui, p, percent, report.level),
                                (ReportKind::Lossless, _) if !is_show && !report.measures.is_empty() => {
                                    report_plot(ui, self.plots.get(path), report.cutoff);
                                }
                                _ => {}
                            };
                            report_cards(ui, p, &report, (*kind, path), visual, &mut opened);
                        });
                    });
                };
                if class == egui::ViewportClass::Embedded {
                    let mut open = true;
                    egui::Window::new(&window_title)
                        .id(egui::Id::new(id))
                        .open(&mut open)
                        .default_width(700.0)
                        .show(vctx, content);
                    if !open {
                        closed.push(index);
                    }
                } else {
                    egui::CentralPanel::default().show(vctx, content);
                    if vctx.input(|i| i.viewport().close_requested() || i.key_pressed(Key::Escape)) {
                        closed.push(index);
                    }
                }
            });
        }
        closed.sort_unstable();
        closed.dedup();
        for index in closed.into_iter().rev() {
            self.open_reports.remove(index);
        }
        for key in opened {
            if self.reports.contains_key(&key) && !self.open_reports.contains(&key) {
                self.open_reports.push(key);
            }
        }
        if let Some(path) = spectrogram {
            self.start(Op::Spectrogram, Vec::new(), vec![path], false);
        }
    }

    /// The spectrograms of the open lossless analyses of files: drawn in a thread of their
    /// own, then turned into textures. The file's outline comes with them.
    fn prepare_plots(&mut self, ctx: &egui::Context) {
        let wanted: Vec<PathBuf> = self
            .open_reports
            .iter()
            .filter(|(kind, path)| *kind == ReportKind::Lossless && self.reports.get(&(*kind, path.clone())).is_some_and(|r| !r.measures.is_empty()))
            .map(|(_, path)| path.clone())
            .collect();
        for path in wanted {
            match self.plots.get(&path) {
                None => {
                    let (tx, rx) = mpsc::channel();
                    let source = path.clone();
                    let repaint = ctx.clone();
                    std::thread::spawn(move || {
                        crate::dsp::lower_priority();
                        let plot = crate::spectrogram::plot(&source, &AtomicBool::new(false));
                        let _ = tx.send((plot, crate::dsp::take_wave(&source)));
                        repaint.request_repaint();
                    });
                    self.plots.insert(path, PlotState::Pending(rx));
                }
                Some(PlotState::Pending(rx)) => {
                    let Ok((plot, wave)) = rx.try_recv() else { continue };
                    if let Some(peaks) = wave {
                        self.waves.insert(path.clone(), peaks);
                    }
                    let state = match plot {
                        Ok(plot) => {
                            let image = egui::ColorImage::from_rgba_unmultiplied([plot.width, plot.height], &plot.rgba);
                            let texture = ctx.load_texture(format!("plot {}", path.display()), image, egui::TextureOptions::LINEAR);
                            PlotState::Ready { texture, nyquist: plot.nyquist, seconds: plot.seconds }
                        }
                        Err(e) => PlotState::Failed(e),
                    };
                    self.plots.insert(path, state);
                }
                Some(_) => {}
            }
        }
    }

    /// Media info summaries, each in its own system window, copyable as text.
    fn media_windows(&mut self, ctx: &egui::Context) {
        let mut closed = Vec::new();
        for (index, (title, text)) in self.media_windows.iter().enumerate() {
            let id = egui::ViewportId::from_hash_of(("media info", title));
            let window_title = format!("Media info - {title}");
            let builder = egui::ViewportBuilder::default()
                .with_icon(app_icon())
                .with_window_level(window_level(self.on_top()))
                .with_title(&window_title)
                .with_inner_size([720.0, 600.0])
                .with_min_inner_size([420.0, 300.0]);
            ctx.show_viewport_immediate(id, builder, |vctx, class| {
                let content = |ui: &mut egui::Ui| {
                    egui::TopBottomPanel::bottom(egui::Id::new(("media buttons", index))).show_inside(ui, |ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.button("📋 Copy").on_hover_text("Copy this summary as text (for a forum post or an info file)").clicked() {
                                vctx.copy_text(text.clone());
                            }
                            if ui.button("Close").clicked() {
                                closed.push(index);
                            }
                        });
                        ui.add_space(2.0);
                    });
                    egui::CentralPanel::default().show_inside(ui, |ui| {
                        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                            ui.add(egui::Label::new(RichText::new(text).monospace()).selectable(true).extend());
                        });
                    });
                };
                if class == egui::ViewportClass::Embedded {
                    let mut open = true;
                    egui::Window::new(&window_title).id(egui::Id::new(id)).open(&mut open).default_width(680.0).show(vctx, content);
                    if !open {
                        closed.push(index);
                    }
                } else {
                    egui::CentralPanel::default().show(vctx, content);
                    if vctx.input(|i| i.viewport().close_requested() || i.key_pressed(Key::Escape)) {
                        closed.push(index);
                    }
                }
            });
        }
        closed.sort_unstable();
        closed.dedup();
        for index in closed.into_iter().rev() {
            self.media_windows.remove(index);
        }
    }

    /// Each spectrogram is a separate system window, free to leave the main one.
    fn spectrogram_windows(&mut self, ctx: &egui::Context) {
        let level = window_level(self.on_top());
        let mut closed = Vec::new();
        let mut saved = Vec::new();
        let mut copied = Vec::new();
        let ppp = ctx.pixels_per_point();
        for (index, spec) in self.spectrograms.iter_mut().enumerate() {
            let texture = spec
                .texture
                .get_or_insert_with(|| {
                    let image = egui::ColorImage::from_rgba_unmultiplied([spec.image.width, spec.image.height], &spec.image.rgba);
                    ctx.load_texture(format!("spectrogram {}", spec.source.display()), image, egui::TextureOptions::LINEAR)
                })
                .clone();
            // Image size in points: shown pixel for pixel when the window keeps its first size.
            let size = egui::vec2(spec.image.width as f32, spec.image.height as f32) / ppp;
            let title = format!("Spectrogram - {}", file_name(&spec.source));
            let id = egui::ViewportId::from_hash_of(("spectrogram", &spec.source));
            let builder = egui::ViewportBuilder::default()
                .with_icon(app_icon())
                .with_window_level(level)
                .with_title(&title)
                .with_inner_size(size + egui::vec2(0.0, 44.0))
                .with_min_inner_size([420.0, 280.0]);
            ctx.show_viewport_immediate(id, builder, |vctx, class| {
                let content = |ui: &mut egui::Ui| {
                    egui::TopBottomPanel::bottom(egui::Id::new(("spectrogram buttons", index))).show_inside(ui, |ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.button("📋 Copy").on_hover_text("Copy the image to the clipboard").clicked() {
                                copied.push(index);
                            }
                            if ui.button("💾 Save image...").clicked() {
                                saved.push(index);
                            }
                            if ui.button("Close").clicked() {
                                closed.push(index);
                            }
                        });
                        ui.add_space(2.0);
                    });
                    egui::CentralPanel::default().frame(egui::Frame::none()).show_inside(ui, |ui| {
                        let available = ui.available_size();
                        let scale = (available.x / size.x).min(available.y / size.y).max(0.05);
                        ui.centered_and_justified(|ui| {
                            ui.image((texture.id(), size * scale));
                        });
                    });
                };
                if class == egui::ViewportClass::Embedded {
                    // Backend without multiple windows: fall back to a window inside the main one.
                    let mut open = true;
                    egui::Window::new(&title).id(egui::Id::new(id)).open(&mut open).default_width(780.0).show(vctx, content);
                    if !open {
                        closed.push(index);
                    }
                } else {
                    egui::CentralPanel::default().show(vctx, content);
                    if vctx.input(|i| i.viewport().close_requested() || i.key_pressed(Key::Escape)) {
                        closed.push(index);
                    }
                }
            });
        }
        for index in copied {
            let spec = &self.spectrograms[index];
            let image = arboard::ImageData {
                width: spec.image.width,
                height: spec.image.height,
                bytes: std::borrow::Cow::Borrowed(&spec.image.rgba),
            };
            // The clipboard object is kept: on Linux the copied image lives as long as it does.
            let result = match &mut self.clipboard {
                Some(clipboard) => clipboard.set_image(image),
                None => arboard::Clipboard::new().and_then(|mut clipboard| {
                    let result = clipboard.set_image(image);
                    self.clipboard = Some(clipboard);
                    result
                }),
            };
            match result {
                Ok(()) => self.flash(format!("Spectrogram of {} copied to the clipboard.", file_name(&spec.source))),
                Err(e) => self.log(format!("Cannot copy the spectrogram: {e}"), Level::Error),
            }
        }
        if let Some(&index) = saved.first() {
            self.ask(Dialog::SaveSpectrogram(self.spectrograms[index].source.clone()));
        }
        closed.sort_unstable();
        closed.dedup();
        for index in closed.into_iter().rev() {
            self.spectrograms.remove(index);
        }
    }
}

/// The Torrent menu entries: each tracker, then all of them (F10).
fn tracker_entries(opt: &Options) -> Vec<(String, &'static str, Vec<Tracker>)> {
    let all = Tracker::all(opt);
    let mut entries: Vec<(String, &'static str, Vec<Tracker>)> =
        all.iter().map(|t| (format!("For {}", t.label(opt)), "", vec![*t])).filter(|(label, _, _)| label != "For ").collect();
    let both = if all.len() > 2 { "For all trackers" } else { "For both sites" };
    entries.push((both.into(), "F10", all));
    entries
}

/// "Always on top" also applies to the secondary windows (spectrograms, reports, media
/// info), or they would open behind the main window.
/// The file dialogs, opened through `App::ask`.
enum Dialog {
    AddFolder,
    AddFiles,
    Dvd,
    SaveLog,
    TorrentCheck,
    Mp3Dir,
    TorrentDir,
    /// The spectrogram of this file.
    SaveSpectrogram(PathBuf),
}

/// The main window's native handles. Given to the file dialogs as their parent, so that
/// they open above the window, even when it is always on top.
#[derive(Clone, Copy)]
struct ParentWindow {
    window: RawWindowHandle,
    display: RawDisplayHandle,
}

impl ParentWindow {
    fn of(frame: &eframe::Frame) -> Option<Self> {
        Some(ParentWindow { window: frame.window_handle().ok()?.as_raw(), display: frame.display_handle().ok()?.as_raw() })
    }
}

// SAFETY (both): the main window and its display connection live as long as the program.
impl HasWindowHandle for ParentWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Ok(unsafe { WindowHandle::borrow_raw(self.window) })
    }
}

impl HasDisplayHandle for ParentWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(unsafe { DisplayHandle::borrow_raw(self.display) })
    }
}

/// A file dialog that belongs to the main window.
fn file_dialog(parent: Option<ParentWindow>) -> rfd::FileDialog {
    match parent {
        Some(p) => rfd::FileDialog::new().set_parent(&p),
        None => rfd::FileDialog::new(),
    }
}

fn window_level(on_top: bool) -> egui::WindowLevel {
    if on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal }
}

/// "Always on top" (honoured by Windows and by X11 desktops; native Wayland has no such request).
fn set_window_level(ctx: &egui::Context, on_top: bool) {
    ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(window_level(on_top)));
}

/// The content of a right-click menu, scrolling when it is taller than the window (egui
/// would cut it off).
fn fit_window<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let room = (ui.ctx().screen_rect().height() - 24.0).max(80.0);
    egui::ScrollArea::vertical().max_height(room).auto_shrink([true, true]).show(ui, add_contents).inner
}

/// Opens a folder in the file manager; for a file, its folder with the file selected
/// (Explorer on Windows, the freedesktop FileManager1 service on Linux), or just its folder
/// when the file manager cannot do that.
fn show_in_folder(path: &Path) -> std::io::Result<()> {
    if path.is_file() {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Explorer wants the path quoted inside the argument: /select,"C:\a b\c.flac"
            if std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn().is_ok() {
                return Ok(());
            }
        }
        #[cfg(target_os = "linux")]
        if reveal_freedesktop(path) {
            return Ok(());
        }
    }
    let dir = if path.is_file() { path.parent().unwrap_or(path) } else { path };
    open::that_detached(dir)
}

/// org.freedesktop.FileManager1.ShowItems (Dolphin, Nautilus, Nemo...), within a second.
#[cfg(target_os = "linux")]
fn reveal_freedesktop(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let call = || -> zbus::Result<()> {
            let connection = zbus::blocking::Connection::session()?;
            connection.call_method(
                Some("org.freedesktop.FileManager1"),
                "/org/freedesktop/FileManager1",
                Some("org.freedesktop.FileManager1"),
                "ShowItems",
                &(vec![uri.as_str()], ""),
            )?;
            Ok(())
        };
        let _ = tx.send(call().is_ok());
    });
    rx.recv_timeout(Duration::from_secs(1)).unwrap_or(false)
}

/// The content of a drop-down menu, scrolling when it is taller than the room left under
/// it: otherwise egui moves the whole menu up, over the menu bar and the toolbar.
fn fit_below<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let id = ui.id().with("menu_top");
    let top = ui.ctx().memory_mut(|m| *m.data.get_temp_mut_or_insert_with(id, || ui.max_rect().top()));
    let room = (ui.ctx().screen_rect().bottom() - top - 16.0).max(80.0);
    egui::ScrollArea::vertical().max_height(room).auto_shrink([true, true]).show(ui, add_contents).inner
}

fn mp3_buttons(ui: &mut egui::Ui, disabled: bool, actions: &mut Vec<Action>) {
    for (label, mode) in [("🎧 To MP3 320 kbps (CBR)", Mp3Mode::Cbr320), ("🎧 To MP3 VBR V0 (about 245 kbps)", Mp3Mode::V0)] {
        let tip = format!("Writes \"<show> [{}]\" next to the show folder (or in the folder set in Settings), with the text files and pictures", mode.tag());
        if ui.add_enabled(!disabled, egui::Button::new(label)).on_hover_text(tip).clicked() {
            ui.close_menu();
            actions.push(Action::Run(Op::Mp3(mode)));
        }
    }
}

/// A show's length, each track once: a FLAC and a WAV of the same track (during or after
/// a conversion) count once. False when a track's length is not known yet.
fn show_length(files: &[PathBuf], info: &HashMap<PathBuf, AudioInfo>) -> (f64, bool) {
    let mut tracks: HashMap<PathBuf, Option<f64>> = HashMap::new();
    for f in files {
        let seconds = info.get(f).map(|i| i.seconds());
        let entry = tracks.entry(f.with_extension("")).or_default();
        *entry = entry.or(seconds);
    }
    (tracks.values().flatten().sum(), tracks.values().all(Option::is_some))
}

/// The width of a table cell in view (the list may be wider than the window), 0 when
/// hardly any of it is.
fn visible_width(ui: &egui::Ui) -> f32 {
    let visible = ui.clip_rect().intersect(ui.max_rect()).width();
    if visible >= 60.0 { visible } else { 0.0 }
}

/// "Clean WAV headers": copies with only the format and the sound.
fn clean_wav_button(ui: &mut egui::Ui, disabled: bool, actions: &mut Vec<Action>) {
    let tip = "Rewrite WAV files with only their format and their sound, in the canonical layout: extra chunks (LIST, id3, bext...), \
               non-standard headers and stray bytes are left out. Copies go to a WAV-cleaned folder and are checked to hold the same \
               sound; the originals are not touched. Some burning and SBE tools stumble on WAVs that are not canonical.";
    if ui.add_enabled(!disabled, egui::Button::new("✨ Clean WAV headers")).on_hover_text(tip).clicked() {
        ui.close_menu();
        actions.push(Action::Run(Op::CleanWav));
    }
}

/// "Tag and rename from info file...": opens the window where the tags are checked.
fn tag_button(ui: &mut egui::Ui, disabled: bool, actions: &mut Vec<Action>) {
    if ui.add_enabled(!disabled, egui::Button::new("🏷 Tag and rename from info file...")).on_hover_text(TAG_TIP).clicked() {
        ui.close_menu();
        actions.push(Action::TagInfo);
    }
}

/// "Extract audio...": one track of each DVD title or video to one file.
fn extract_button(ui: &mut egui::Ui, disabled: bool, actions: &mut Vec<Action>) {
    if ui.add_enabled(!disabled, egui::Button::new("📀 Extract audio...")).on_hover_text(EXTRACT_TIP).clicked() {
        ui.close_menu();
        actions.push(Action::ExtractAudio);
    }
}

/// "Get setlist from setlist.fm": a text file with the setlist in each show folder.
fn setlist_button(ui: &mut egui::Ui, disabled: bool, actions: &mut Vec<Action>) {
    if ui.add_enabled(!disabled, egui::Button::new("📝 Get setlist from setlist.fm")).on_hover_text(SETLIST_TIP).clicked() {
        ui.close_menu();
        actions.push(Action::Setlist);
    }
}

/// "Check against torrent...": every piece of the files hashed again and compared.
fn torrent_check_button(ui: &mut egui::Ui, disabled: bool, actions: &mut Vec<Action>) {
    let tip = "Check the show against a .torrent file: every piece is hashed again, and damaged, missing or resized files are named. \
               The torrent's folder is found by its name in the list or next to the .torrent. You can also drop .torrent files on the window.";
    if ui.add_enabled(!disabled, egui::Button::new("🔗 Check against torrent...")).on_hover_text(tip).clicked() {
        ui.close_menu();
        actions.push(Action::TorrentCheck);
    }
}

/// "Fix SBE": corrected copies, as `shntool fix`.
/// The last file of its folder (CD1, CD2...) in the show: an SBE there is normal.
fn last_of_disc(show: &crate::files::Show, fi: usize) -> bool {
    match show.files.get(fi + 1) {
        Some(next) => next.parent() != show.files[fi].parent(),
        None => true,
    }
}

fn sbe_button(ui: &mut egui::Ui, disabled: bool, actions: &mut Vec<Action>) {
    let tip = "Fix sector boundary errors, disc by disc (CD1, CD2... folders), as shntool fix: boundaries move back to \
               the previous CD sector, the last track is padded with silence. Corrected FLAC copies of the tracks that \
               change go to an SBE-fixed folder; the originals are not touched.";
    if ui.add_enabled(!disabled, egui::Button::new("🔧 Fix SBE")).on_hover_text(tip).clicked() {
        ui.close_menu();
        actions.push(Action::Run(Op::FixSbe));
    }
}

fn ops_menu(ui: &mut egui::Ui, disabled: bool, flac_level: u8, trackers: &[(String, &'static str, Vec<Tracker>)], actions: &mut Vec<Action>) {
    let items: [(&str, &str, Op); 7] = [
        ("🛡 Verify checksums", "F5", Op::Verify),
        ("✚ Create checksums", "F6", Op::Create),
        ("✔ Decode test", "F7", Op::Test),
        ("🔍 Lossless?", "Ctrl+L", Op::Lossless),
        ("⏱ Speed?", "Ctrl+P", Op::Speed),
        ("ℹ Show details", "F8", Op::Details { verbose: true }),
        ("📋 Media info", "Ctrl+I", Op::MediaInfo(Vec::new())),
    ];
    for (label, key, op) in items {
        if ui.add_enabled(!disabled, egui::Button::new(label).shortcut_text(key)).clicked() {
            ui.close_menu();
            actions.push(Action::Run(op));
        }
    }
    ui.separator();
    for (label, target) in [(format!("🎵 To FLAC (level {flac_level})"), Target::Flac), ("🎵 To WAV".to_string(), Target::Wav)] {
        if ui.add_enabled(!disabled, egui::Button::new(label)).clicked() {
            ui.close_menu();
            actions.push(Action::Run(Op::Convert { target, level: flac_level, round_float: false }));
        }
    }
    mp3_buttons(ui, disabled, actions);
    sbe_button(ui, disabled, actions);
    clean_wav_button(ui, disabled, actions);
    tag_button(ui, disabled, actions);
    setlist_button(ui, disabled, actions);
    extract_button(ui, disabled, actions);
    ui.separator();
    if ui.add_enabled(!disabled, egui::Button::new("📊 Spectrogram").shortcut_text("F9")).clicked() {
        ui.close_menu();
        actions.push(Action::Run(Op::Spectrogram));
    }
    // Flat entries: an egui 0.29 sub-menu stays open while hovering plain buttons.
    ui.separator();
    for (label, key, list) in trackers {
        let label = format!("🌐 Torrent for {}", label.strip_prefix("For ").unwrap_or(label));
        if ui.add_enabled(!disabled, egui::Button::new(label).shortcut_text(*key)).clicked() {
            ui.close_menu();
            actions.push(Action::Run(Op::Torrent(list.clone())));
        }
    }
    ui.separator();
    torrent_check_button(ui, disabled, actions);
    if ui.add_enabled(!disabled, egui::Button::new("📋 Copy FFP contents")).on_hover_text("Copy the show's .ffp file to the clipboard (for the TTD upload form)").clicked() {
        ui.close_menu();
        actions.push(Action::CopyFfp);
    }
    if ui.add_enabled(!disabled, egui::Button::new("📋 Copy info file")).on_hover_text(INFO_TIP).clicked() {
        ui.close_menu();
        actions.push(Action::CopyInfo);
    }
}

fn torrent_menu(ui: &mut egui::Ui, trackers: &[(String, &'static str, Vec<Tracker>)], actions: &mut Vec<Action>) {
    for (label, key, list) in trackers {
        if ui.add(egui::Button::new(label.as_str()).shortcut_text(*key)).clicked() {
            ui.close_menu();
            actions.push(Action::Run(Op::Torrent(list.clone())));
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.parent.is_none() {
            self.parent = ParentWindow::of(frame);
        }
        self.pending_dialog(ctx);
        // The saved "Always on top" choice, once the window exists.
        if !self.window_level_set {
            self.window_level_set = true;
            if self.opt.always_on_top {
                set_window_level(ctx, true);
            }
        }
        self.poll();
        self.poll_update_check();
        if self.update_check.is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        self.drops(ctx);
        let mut actions = Vec::new();
        self.shortcuts(ctx, &mut actions);
        self.menu_open = false;
        self.menu_and_toolbar(ctx, &mut actions);
        self.banner(ctx);
        if let Some(roots) = self.pending_share.take() {
            actions.push(Action::ShareCard(Some(roots)));
        }
        self.bottom(ctx, &mut actions);
        self.table(ctx, &mut actions);
        self.toasts(ctx, &mut actions);
        self.settings_window(ctx);
        self.about_window(ctx);
        self.help_window(ctx);
        self.info_window(ctx);
        self.tag_batch_window(ctx);
        self.tag_window(ctx);
        self.undo_window(ctx);
        self.setlist_window(ctx);
        self.extract_window(ctx);
        self.float_window(ctx);
        self.after_flac_window(ctx);
        self.spectrogram_windows(ctx);
        self.report_windows(ctx);
        self.media_windows(ctx);
        self.apply(ctx, actions);

        let file_count: usize = self.shows.iter().map(|s| s.files.len()).sum();
        let title = if self.shows.is_empty() {
            APP_NAME.to_string()
        } else {
            format!("{APP_NAME} - {}, {}", crate::ops::plural(self.shows.len(), "show", "shows"), crate::ops::plural(file_count, "file", "files"))
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        self.fresh.retain(|_, when| when.elapsed().as_secs_f32() < GLOW);
        if !self.fresh.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
        if self.running.is_some() {
            ctx.request_repaint_after(Duration::from_millis(80));
        } else if self.flash.as_ref().is_some_and(|(_, t)| t.elapsed() < Duration::from_secs(16)) {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "options", &self.opt);
    }
}
