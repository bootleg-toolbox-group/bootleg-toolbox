//! The window: show list, toolbar, log, settings.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, Modifiers, RichText};
use egui_extras::{Column, TableBuilder};

use crate::audio::{fmt_total, AudioInfo};
use crate::files::{file_name, rel_posix, scan_paths, Show};
use crate::encode::Mp3Mode;
use crate::ops::{self, Level, Msg, Op, Options, Target, ThemeChoice, Tracker, APP_NAME, APP_VERSION};

const AUTO_DETAILS_LIMIT: usize = 3000;

/// The main components built into the program (About window); THIRD-PARTY.md lists them all.
const COMPONENTS: &[(&str, &str, &str)] = &[
    ("egui, eframe, egui_extras", "user interface", "MIT or Apache-2.0"),
    ("winit", "windows and input", "Apache-2.0"),
    ("Symphonia", "FLAC and WAV decoding", "MPL-2.0"),
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
    ("Hack, Ubuntu, Noto Emoji fonts", "interface fonts (from egui)", "MIT / UFL / OFL"),
    ("Shorten decoder", "SHN decoding (written for Bootleg Helper)", "GPL-3.0"),
];

const DVD_TIP: &str = "Pick a DVD folder (VIDEO_TS, or the folder above it) for a technical summary: video standard, resolution, aspect ratio, bit rates, audio tracks, subtitles, titles and chapters. Dropping a DVD folder on the window does the same.";

/// The name a DVD is known by: the folder above VIDEO_TS, else its own name.
fn dvd_name(path: &Path) -> String {
    match path.parent() {
        Some(parent) if file_name(path).eq_ignore_ascii_case("VIDEO_TS") => file_name(parent),
        _ => file_name(path),
    }
}

const INFO_TIP: &str = "Copy the show's info file (.txt or .nfo, whatever its name) to the clipboard, for the upload forms. With several text files, a window lets you pick the right one.";

/// The text files that may be a show's info file, the most likely first.
struct InfoChoice {
    root: PathBuf,
    candidates: Vec<PathBuf>,
    texts: Vec<String>,
    chosen: usize,
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
    std::fs::write(icon_dir.join("bootleg-helper.png"), include_bytes!("../assets/icon.png")).map_err(|e| e.to_string())?;
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
         Exec={} %F\nIcon=bootleg-helper\nTerminal=false\nStartupWMClass=bootleg-helper\n\
         Categories=AudioVideo;Audio;Utility;\nKeywords=flac;ffp;md5;st5;shn;ape;bootleg;checksum;torrent;sbe;\n",
        quoted.replace('\\', "\\\\")
    );
    let desktop = apps_dir.join("bootleg-helper.desktop");
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

pub fn run(paths: Vec<PathBuf>) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([1000.0, 465.0])
            .with_min_inner_size([620.0, 320.0])
            .with_drag_and_drop(true)
            .with_app_id("bootleg-helper")
            .with_icon(app_icon()),
        ..Default::default()
    };
    // winit has no drag and drop on Wayland: go through XWayland when it is there.
    // BH_WAYLAND=1 keeps the native Wayland window (no drag and drop).
    #[cfg(target_os = "linux")]
    let options = if std::env::var_os("DISPLAY").is_some() && std::env::var_os("BH_WAYLAND").is_none() {
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

fn color(level: Level) -> Option<Color32> {
    let dark = DARK_THEME.load(Ordering::Relaxed);
    match level {
        Level::Info => None,
        Level::Ok if dark => Some(Color32::from_rgb(58, 157, 93)),
        Level::Ok => Some(Color32::from_rgb(25, 128, 56)),
        Level::Warn if dark => Some(Color32::from_rgb(200, 138, 18)),
        Level::Warn => Some(Color32::from_rgb(176, 104, 0)),
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

#[derive(Clone, Copy)]
enum Row {
    Show(usize),
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
    /// "Copy info file" with several text files: the choice window.
    info_choice: Option<InfoChoice>,
    /// The saved "Always on top" choice was applied to the window.
    window_level_set: bool,
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
    /// DVDs added (they are not shown in the list): their media info is read when free.
    pending_dvds: Vec<PathBuf>,
    /// Open media info windows: (show or DVD name, text).
    media_windows: Vec<(String, String)>,
    after_flac: Option<AfterFlac>,
    clipboard: Option<arboard::Clipboard>,
}

/// An open spectrogram window.
/// The table columns; all but Name can be hidden (View menu, right-click on the header).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Col {
    Name,
    Format,
    Length,
    Rate,
    Bits,
    Ch,
    Sbe,
    Source,
    Speed,
    Result,
}

impl Col {
    const ALL: [Col; 10] = [Col::Name, Col::Format, Col::Bits, Col::Rate, Col::Length, Col::Ch, Col::Sbe, Col::Source, Col::Speed, Col::Result];

    fn title(self) -> &'static str {
        match self {
            Col::Name => "Name",
            Col::Format => "Format",
            Col::Length => "Length",
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
        let opt: Options = cc.storage.and_then(|s| eframe::get_value(s, "options")).unwrap_or_default();
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
            info_choice: None,
            window_level_set: false,
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
            pending_dvds: Vec::new(),
            media_windows: Vec::new(),
            after_flac: None,
            clipboard: None,
        };
        // A more modern look: roomier, rounded buttons, a blue tint on hover.
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.button_padding = egui::vec2(10.0, 5.0);
            style.spacing.interact_size.y = style.spacing.interact_size.y.max(26.0);
            let v = &mut style.visuals;
            v.window_rounding = egui::Rounding::same(8.0);
            v.menu_rounding = egui::Rounding::same(6.0);
            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
                w.rounding = egui::Rounding::same(6.0);
                w.expansion = 0.0;
            }
            let dark = v.dark_mode;
            let accent = if dark { Color32::from_rgb(95, 130, 200) } else { Color32::from_rgb(90, 130, 210) };
            v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0f32, if dark { Color32::from_gray(70) } else { Color32::from_gray(205) });
            v.widgets.inactive.weak_bg_fill = if dark { Color32::from_gray(52) } else { Color32::from_gray(236) };
            v.widgets.hovered.weak_bg_fill = if dark { Color32::from_rgb(52, 62, 84) } else { Color32::from_rgb(222, 232, 248) };
            v.widgets.hovered.bg_fill = v.widgets.hovered.weak_bg_fill;
            v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0f32, accent);
            v.widgets.active.weak_bg_fill = if dark { Color32::from_rgb(62, 78, 110) } else { Color32::from_rgb(200, 216, 242) };
            v.widgets.active.bg_fill = v.widgets.active.weak_bg_fill;
            v.widgets.active.bg_stroke = egui::Stroke::new(1.0f32, accent);
            v.widgets.open.weak_bg_fill = v.widgets.hovered.weak_bg_fill;
            v.widgets.open.bg_stroke = egui::Stroke::new(1.0f32, accent);
        });
        // egui's light theme writes text in mid grey (80/255, and fainter for weak text):
        // darker, for reading on white.
        cc.egui_ctx.style_mut_of(egui::Theme::Light, |style| {
            style.visuals.widgets.noninteractive.fg_stroke.color = Color32::from_gray(30);
            style.visuals.widgets.inactive.fg_stroke.color = Color32::from_gray(25);
        });
        app.apply_theme(&cc.egui_ctx);
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
    }

    // ------------------------------------------------------------ list

    fn add_paths(&mut self, paths: Vec<PathBuf>, keep: bool) {
        if self.running.is_some() {
            self.flash("Wait for the current task to finish.");
            return;
        }
        // DVDs open their media info instead of joining the list; a drop of DVDs only
        // leaves the list as it is.
        let mut dvds = Vec::new();
        let mut audio = Vec::new();
        for p in paths {
            let found = crate::dvd::find(&p);
            if found.is_empty() {
                audio.push(p);
            } else {
                if !scan_paths(std::slice::from_ref(&p), &HashSet::new()).is_empty() {
                    audio.push(p);
                }
                dvds.extend(found);
            }
        }
        if !dvds.is_empty() {
            const MAX: usize = 4;
            if dvds.len() > MAX {
                self.log(format!("{} DVDs found: media info for the first {MAX} only (add the others one by one).", dvds.len()), Level::Warn);
                dvds.truncate(MAX);
            }
            self.log(format!("DVD found: {} (DVDs are not added to the list; their media info opens in a window).", dvds.iter().map(|d| dvd_name(d)).collect::<Vec<_>>().join(", ")), Level::Info);
            self.pending_dvds.extend(dvds);
        }
        if audio.is_empty() {
            self.start_pending_dvds();
            return;
        }
        if !keep {
            self.clear_list();
        }
        for p in audio {
            let p = std::path::absolute(&p).unwrap_or(p);
            self.excluded.retain(|e| e != &p && !e.starts_with(&p));
            if !self.roots.contains(&p) {
                self.roots.push(p);
            }
        }
        self.rebuild();
        self.auto_details();
        self.start_pending_dvds();
    }

    fn start_pending_dvds(&mut self) {
        if self.running.is_none() && !self.pending_dvds.is_empty() {
            let dvds = std::mem::take(&mut self.pending_dvds);
            self.start(Op::MediaInfo(dvds), Vec::new(), Vec::new(), false);
        }
    }

    fn pick_dvd(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().set_title("DVD folder (or the folder above VIDEO_TS)").pick_folder() {
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
    }

    fn rebuild(&mut self) {
        self.fit_columns = true;
        self.shows = scan_paths(&self.roots, &self.excluded);
        let existing: HashSet<&PathBuf> = self.shows.iter().flat_map(|s| s.files.iter().chain(std::iter::once(&s.root))).collect();
        self.selected.retain(|p| existing.contains(p));
        // Shows always arrive unfolded, track titles visible; only the user folds them.
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

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (si, show) in self.shows.iter().enumerate() {
            rows.push(Row::Show(si));
            if !self.collapsed.contains(&show.root) {
                rows.extend((0..show.files.len()).map(|fi| Row::File(si, fi)));
            }
        }
        rows
    }

    fn row_path(&self, row: Row) -> &Path {
        match row {
            Row::Show(si) => &self.shows[si].root,
            Row::File(si, fi) => &self.shows[si].files[fi],
        }
    }

    fn selection(&self) -> (Vec<Show>, Vec<PathBuf>) {
        if self.selected.is_empty() {
            let files = self.shows.iter().flat_map(|s| s.files.clone()).collect();
            return (self.shows.clone(), files);
        }
        let mut shows = Vec::new();
        let mut files = Vec::new();
        for show in &self.shows {
            let whole = self.selected.contains(&show.root);
            let picked: Vec<PathBuf> = show.files.iter().filter(|f| whole || self.selected.contains(*f)).cloned().collect();
            if !picked.is_empty() {
                shows.push(show.clone());
                files.extend(picked);
            }
        }
        (shows, files)
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
        let without_files = match &op {
            Op::TorrentCheck(_) => true,
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
        ops::spawn(op, shows, files, self.opt.clone(), tx, cancel.clone());
        self.running = Some(Running { rx, cancel, label, done: 0, total: 0 });
    }

    fn run_on_selection(&mut self, op: Op) {
        let (shows, files) = self.selection();
        // To FLAC with float WAVs: read them first, to say what the conversion will do.
        if let Op::Convert { target: Target::Flac, level, round_float: false } = op {
            let float = |p: &PathBuf| match self.info.get(p) {
                Some(info) => info.float,
                None => crate::audio::probe(p).is_ok_and(|i| i.float),
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
                    ui.label(format!("    {}   (peak {:.2} dBFS)", file_name(path), 20.0 * scan.peak.max(1e-12).log10()));
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
                        self.status.insert(path, (text, level, tooltip));
                    }
                    Msg::Info { path, info } => {
                        self.info.insert(path, info);
                    }
                    Msg::Source { path, text, level, tooltip, report } => {
                        self.reports.insert((ReportKind::Lossless, path.clone()), report);
                        self.source.insert(path, (text, level, tooltip));
                    }
                    Msg::Speed { path, text, level, tooltip, report } => {
                        self.reports.insert((ReportKind::Speed, path.clone()), report);
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
            self.running = None;
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
            self.flash(summary);
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
            self.start_pending_dvds();
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

    fn copy_ffp(&mut self, ctx: &egui::Context) {
        let (shows, _) = self.selection();
        if shows.len() != 1 {
            self.flash("Select a single show to copy its FFP.");
            return;
        }
        let show = shows[0].clone();
        let name = file_name(&show.root);
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
        let name = file_name(&root);
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
                let msg = format!("Info file of {} copied to the clipboard: {}", file_name(root), rel_posix(path, root));
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

    fn save_log(&mut self) {
        if let Some(path) = rfd::FileDialog::new().set_file_name("bootleg-helper.log").save_file() {
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
        let scope = if self.selected.is_empty() { "the whole list (nothing selected)" } else { "the selection" };
        let mut menu_open = false;
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| fit_below(ui, |ui| {
                    menu_open = true;
                    if ui.add_enabled(!busy, egui::Button::new("Add folder...")).clicked() {
                        ui.close_menu();
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            self.add_paths(vec![dir], true);
                        }
                    }
                    if ui.add_enabled(!busy, egui::Button::new("Add files...")).clicked() {
                        ui.close_menu();
                        if let Some(files) = rfd::FileDialog::new().add_filter("Audio", &["flac", "wav", "ape", "shn", "wv", "mp3", "mp2"]).pick_files() {
                            self.add_paths(files, true);
                        }
                    }
                    if ui.add_enabled(!busy, egui::Button::new("DVD media info...")).on_hover_text(DVD_TIP).clicked() {
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
                    #[cfg(target_os = "linux")]
                    if ui
                        .button("Add to the applications menu")
                        .on_hover_text("Adds Bootleg Helper, with its icon, to the menu of your desktop, for this program file where it is now. Use it again if you move the program.")
                        .clicked()
                    {
                        ui.close_menu();
                        match install_desktop_entry() {
                            Ok(text) => self.log(text, Level::Ok),
                            Err(e) => self.log(format!("Could not add the menu entry: {e}"), Level::Error),
                        }
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
                    ui.menu_button("📂 Add ⏷", |ui| fit_below(ui, |ui| {
                        menu_open = true;
                        if ui.button("Add folder...").clicked() {
                            ui.close_menu();
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                self.add_paths(vec![dir], true);
                            }
                        }
                        if ui.button("Add files...").clicked() {
                            ui.close_menu();
                            if let Some(files) = rfd::FileDialog::new().add_filter("Audio", &["flac", "wav", "ape", "shn", "wv", "mp3", "mp2"]).pick_files() {
                                self.add_paths(files, true);
                            }
                        }
                        if ui.button("📀 DVD media info...").on_hover_text(DVD_TIP).clicked() {
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
                    ui.menu_button("✔ Check ⏷", |ui| fit_below(ui, |ui| {
                        menu_open = true;
                        let items: [(&str, &str, Op, String); 7] = [
                            ("🛡 Verify checksums", "F5", Op::Verify, format!("Check the files of {scope} against their .ffp, .md5, .st5 and .sfv files.\nAn FFP line only compares the MD5 stored in the FLAC header: use Decode test to find damaged audio.")),
                            ("✔ Decode test", "F7", Op::Test, format!("Decode every file of {scope} completely to find damaged or truncated audio; FLAC files are also checked against their MD5 signature.\nNeeds no checksum file.")),
                            ("🔍 Lossless?", "Ctrl+L", Op::Lossless, format!("Analyse the spectrum of every file of {scope} to find lossy (MP3/AAC) sources, FM broadcasts, upsampled files and fake 24-bit.\nThe verdict goes to the Source column; click it for the full analysis.")),
                            ("⏱ Speed?", "Ctrl+P", Op::Speed, format!("Estimate whether the tape of {scope} runs too fast or too slow, and the correction to apply.\nThe verdict goes to the Speed column; click it for the full analysis.")),
                            ("📊 Spectrogram", "F9", Op::Spectrogram, format!("Show the spectrogram of the first files of {scope}, up to 4, to see lossy sources for yourself")),
                            ("ℹ Show details", "F8", Op::Details { verbose: true }, format!("Read the format, length and SBEs of {scope} again, and list them in the log")),
                            ("📋 Media info", "Ctrl+I", Op::MediaInfo(Vec::new()), format!("A technical summary of {scope} (format, encoder, length, bit rate, tags), as text to copy.\nFor a DVD, add its folder: the summary opens by itself.")),
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
                    ui.menu_button("✚ Create ⏷", |ui| fit_below(ui, |ui| {
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
                    ui.menu_button("🎵 Convert ⏷", |ui| fit_below(ui, |ui| {
                        menu_open = true;
                        ui.label(RichText::new("To FLAC (verified)").strong());
                        for l in 0..=8u8 {
                            let text = match l {
                                0 => "Level 0 (fastest)".to_string(),
                                5 => "Level 5 (flac default)".to_string(),
                                8 => "Level 8 (smallest)".to_string(),
                                _ => format!("Level {l}"),
                            };
                            let text = if l == level { format!("{text}  ✔") } else { text };
                            if ui.button(text).clicked() {
                                ui.close_menu();
                                actions.push(Action::FlacLevel(l));
                                actions.push(Action::Run(Op::Convert { target: Target::Flac, level: l, round_float: false }));
                            }
                        }
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
                    }))
                    .response
                    .on_hover_text(format!("Convert {scope} to FLAC (each file is decoded again and checked) or to WAV, make an MP3 copy of the shows in a separate folder, or fix SBEs. The originals are kept."));
                });
                ui.separator();
                let trackers = tracker_entries(&self.opt);
                ui.add_enabled_ui(on, |ui| {
                    let torrent = ui.menu_button("🌐 Torrent ⏷", |ui| fit_below(ui, |ui| {
                        menu_open = true;
                        torrent_menu(ui, &trackers, actions)
                    }));
                    torrent.response.on_hover_text(format!("Create torrents for the shows of {scope}"));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!busy, egui::Button::new("⚙ Settings")).clicked() {
                        self.settings = Some(self.opt.clone());
                    }
                    if ui.add_enabled(busy, egui::Button::new("⏹ Stop")).on_hover_text("Stop the running task (Esc)").clicked() {
                        self.stop();
                    }
                });
            });
            ui.add_space(3.0);
        });
        self.menu_open |= menu_open;
    }

    fn bottom(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(r) = &self.running {
                    let fraction = if r.total > 0 { r.done as f32 / r.total as f32 } else { 0.0 };
                    ui.add(egui::ProgressBar::new(fraction).desired_width(220.0).text(format!("{}/{}", r.done, r.total)));
                    // A vivid colour, so that a running operation cannot be missed.
                    let busy = if DARK_THEME.load(Ordering::Relaxed) { Color32::from_rgb(196, 128, 255) } else { Color32::from_rgb(128, 36, 204) };
                    ui.add(egui::Spinner::new().color(busy));
                    ui.label(RichText::new(format!("{}...", r.label)).color(busy).strong());
                } else if let Some((text, when)) = &self.flash {
                    if when.elapsed() < Duration::from_secs(15) {
                        ui.label(text);
                    }
                }
                if !self.shows.is_empty() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (shows, files) = self.selection();
                        let counts = format!("{} show(s), {} file(s)", shows.len(), files.len());
                        if self.selected.is_empty() {
                            ui.label(colored(format!("No selection: actions apply to the whole list ({counts})"), Level::Warn));
                        } else {
                            ui.label(format!("Selected: {counts}"));
                        }
                    });
                }
            });
        });
        // The log folds to one line (its last message); it unfolds by itself on an error.
        let open = self.opt.log_open;
        let panel = if open {
            egui::TopBottomPanel::bottom("log").resizable(true).default_height(170.0).min_height(60.0)
        } else {
            egui::TopBottomPanel::bottom("log folded").resizable(false)
        };
        panel.show(ctx, |ui| {
            let (mut clear, mut save, mut toggle) = (false, false, false);
            ui.horizontal(|ui| {
                let arrow = if open { "⏷" } else { "⏵" };
                toggle = ui
                    .add(egui::Label::new(RichText::new(format!("{arrow} Log ({} lines)", self.log.len())).weak()).sense(egui::Sense::click()))
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
            let row_height = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
            egui::ScrollArea::both().stick_to_bottom(true).auto_shrink([false, false]).show_rows(ui, row_height, self.log.len(), |ui, range| {
                for (text, level) in &self.log[range] {
                    ui.add(egui::Label::new(colored(text.clone(), *level).monospace()).extend());
                }
            });
        });
    }

    fn table(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.shows.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("Drop show folders or audio files here\n(FLAC, WAV, APE, SHN, WV)\n\nA new drop replaces the list. Hold Ctrl to add to it instead.").size(15.0).weak());
                });
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
            // A little air between columns fitted to their content.
            ui.spacing_mut().item_spacing.x = 16.0;
            egui::ScrollArea::horizontal().show(ui, |ui| {
                let mut table = TableBuilder::new(ui)
                    // Each set of visible columns keeps its own widths. The number changes with
                    // the default widths, so that widths saved before are not kept over them.
                    .id_salt((2, cols.iter().map(|c| c.title()).collect::<Vec<_>>()))
                    .striped(true)
                    .resizable(true)
                    .sense(egui::Sense::click_and_drag())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
                for (i, col) in cols.iter().enumerate() {
                    table = table.column(col.column(i + 1 == cols.len(), fit));
                }
                table
                    .min_scrolled_height(0.0)
                    .header(22.0, |mut header| {
                        for col in &cols {
                            header.col(|ui| {
                                ui.strong(col.title()).on_hover_text(col.describe());
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
                        let row_height = if compact { 18.0 } else { 20.0 };
                        body.rows(row_height, rows.len(), |mut row| {
                            let index = row.index();
                            first_shown = first_shown.min(index);
                            last_shown = last_shown.max(index);
                            let r = rows[index];
                            let path = self.row_path(r).to_path_buf();
                            row.set_selected(self.selected.contains(&path));
                            let mut responses = Vec::new();
                            match r {
                                Row::Show(si) => self.show_cells(&mut row, si, &cols, &mut responses, actions),
                                Row::File(si, fi) => self.file_cells(&mut row, si, fi, &cols, &mut responses, actions),
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

    fn status_cell(&self, ui: &mut egui::Ui, path: &Path) -> egui::Response {
        Self::verdict_cell(ui, self.status.get(path))
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
                // "Lossless (upsampled)", "Lossless, but ...": the lossless part in green,
                // the remark in orange.
                let split = (*level == Level::Warn && text.starts_with("Lossless") && text.len() > 8).then(|| text.split_at(8));
                let response = match split {
                    Some((head, rest)) => {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let head = ui.label(colored(head, Level::Ok));
                            head.union(ui.label(colored(rest, Level::Warn)))
                        })
                        .inner
                    }
                    None => ui.label(colored(text.clone(), *level)),
                };
                let tip = if tooltip.is_empty() { text } else { tooltip };
                response.on_hover_text(tip)
            }
            None => ui.label(""),
        }
    }

    fn show_cells(&self, row: &mut egui_extras::TableRow, si: usize, cols: &[Col], responses: &mut Vec<egui::Response>, actions: &mut Vec<Action>) {
        let show = &self.shows[si];
        let infos: Vec<&AudioInfo> = show.files.iter().filter_map(|f| self.info.get(f)).collect();
        let expanded = !self.collapsed.contains(&show.root);
        for &col in cols {
            match col {
                Col::Name => responses.push(row.col(|ui| {
                    let arrow = if expanded { "⏷" } else { "⏵" };
                    if ui.add(egui::Label::new(arrow).sense(egui::Sense::click())).clicked() {
                        actions.push(Action::Toggle(show.root.clone()));
                    }
                    let parent_is_show = show.root.parent().is_some_and(|p| self.shows.iter().any(|s| s.root == p));
                    let label = if parent_is_show {
                        format!("{}/{}", show.root.parent().map(file_name).unwrap_or_default(), file_name(&show.root))
                    } else {
                        file_name(&show.root)
                    };
                    ui.label(RichText::new(label).strong()).on_hover_text(show.root.display().to_string());
                }).1),
                Col::Format => {
                    let mut labels: Vec<String> = infos.iter().map(|i| i.label()).collect();
                    labels.sort();
                    labels.dedup();
                    responses.push(row.col(|ui| { ui.label(labels.join(", ")); }).1);
                }
                Col::Length => responses.push(row.col(|ui| {
                    if !infos.is_empty() {
                        let total: f64 = infos.iter().map(|i| i.seconds()).sum();
                        let suffix = if infos.len() < show.files.len() { " +" } else { "" };
                        ui.label(format!("{}{suffix}", fmt_total(total)));
                    }
                }).1),
                Col::Rate | Col::Bits | Col::Ch => responses.push(row.col(|_| {}).1),
                Col::Sbe => responses.push(row.col(|ui| {
                    // Only the SBEs that matter: not those on the last track of a disc.
                    let sbe = (0..show.files.len())
                        .filter(|&fi| !last_of_disc(show, fi))
                        .filter(|&fi| self.info.get(&show.files[fi]).and_then(|i| i.sbe()) == Some(true))
                        .count();
                    if sbe > 0 {
                        ui.label(colored(&sbe.to_string(), Level::Warn))
                            .on_hover_text(format!("{sbe} track(s) with a sector boundary error before the last track of a disc: Fix SBE corrects them"));
                    }
                }).1),
                Col::Source => self.analysis_col(ReportKind::Lossless, row, &show.root, responses, actions),
                Col::Speed => self.analysis_col(ReportKind::Speed, row, &show.root, responses, actions),
                Col::Result => responses.push(row.col(|ui| { self.status_cell(ui, &show.root); }).1),
            }
        }
    }

    fn file_cells(&self, row: &mut egui_extras::TableRow, si: usize, fi: usize, cols: &[Col], responses: &mut Vec<egui::Response>, actions: &mut Vec<Action>) {
        let show = &self.shows[si];
        let path = &show.files[fi];
        let info = self.info.get(path);
        for &col in cols {
            match col {
                Col::Name => responses.push(row.col(|ui| {
                    ui.add_space(22.0);
                    ui.label(rel_posix(path, &show.root)).on_hover_text(path.display().to_string());
                }).1),
                Col::Format => {
                    let format = match (self.opt.compact, info) {
                        (true, Some(i)) => i.label(),
                        _ => crate::audio::Format::from_path(path).map(|f| f.name()).unwrap_or("").to_string(),
                    };
                    responses.push(row.col(|ui| { ui.label(format); }).1);
                }
                Col::Length => responses.push(row.col(|ui| { ui.label(info.map(|i| i.length()).unwrap_or_default()); }).1),
                Col::Rate => responses.push(row.col(|ui| { ui.label(info.map(|i| i.rate_label()).unwrap_or_default()); }).1),
                Col::Bits => responses.push(row.col(|ui| {
                    ui.label(info.map(|i| if i.format.lossy() { "-".to_string() } else { i.bits.to_string() }).unwrap_or_default());
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
                                ui.label(colored("Yes", Level::Warn)).on_hover_text("Sector boundary error before the last track of the disc: a short gap or click if burned. Fix SBE corrects it");
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
                Action::TorrentCheck => {
                    let (shows, _) = self.selection();
                    let mut dialog = rfd::FileDialog::new().add_filter("Torrent", &["torrent"]).set_title("Torrent to check against");
                    if let Some(dir) = shows.first().and_then(|s| s.root.parent()) {
                        dialog = dialog.set_directory(dir);
                    }
                    if let Some(torrents) = dialog.pick_files() {
                        self.run_on_selection(Op::TorrentCheck(torrents));
                    }
                }
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
                    self.selected = self.shows.iter().flat_map(|s| s.files.iter().chain(std::iter::once(&s.root))).cloned().collect();
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
        // The info file choice window takes the keys (arrows, Enter, Escape).
        if ctx.wants_keyboard_input() || self.info_choice.is_some() || self.float_dialog.is_some() {
            return;
        }
        let busy = self.running.is_some();
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
            } else if !self.menu_open && !ctx.memory(|m| m.any_popup_open()) {
                actions.push(Action::Deselect);
            }
        }
    }

    fn drops(&mut self, ctx: &egui::Context) {
        let (dropped, ctrl, hovering) = ctx.input(|i| {
            let paths: Vec<PathBuf> = i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect();
            (paths, i.modifiers.command, !i.raw.hovered_files.is_empty())
        });
        if hovering {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop")));
            let rect = ctx.screen_rect();
            painter.rect_filled(rect, 0.0, Color32::from_black_alpha(140));
            painter.text(rect.center(), egui::Align2::CENTER_CENTER,
                "Drop to replace the list\n(hold Ctrl to add to it)", egui::FontId::proportional(22.0), Color32::WHITE);
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
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                draft.mp3_dir = dir.display().to_string();
                            }
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
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                draft.torrent_dir = dir.display().to_string();
                            }
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
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Files processed in parallel");
                    ui.add(egui::DragValue::new(&mut draft.jobs).range(1..=64));
                });
                ui.checkbox(&mut draft.auto_details, "Read file details automatically when adding files");
                ui.checkbox(&mut draft.shell_integration, "Add \"Open in Bootleg Helper\" to the file manager's right-click menu")
                    .on_hover_text("For folders and audio files, for your user only (no administrator rights). \
                                    Windows: Explorer (on Windows 11, under Show more options). Linux: Dolphin, Nautilus and Nemo.\n\
                                    Untick it to remove the entry.");
                ui.horizontal(|ui| {
                    ui.label("Appearance");
                    ui.radio_value(&mut draft.theme, ThemeChoice::System, "Follow the system");
                    ui.radio_value(&mut draft.theme, ThemeChoice::Light, "Light");
                    ui.radio_value(&mut draft.theme, ThemeChoice::Dark, "Dark");
                });
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
                    draft = Options::default();
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

impl App {
    /// "See more": the full analysis (lossless, speed) of a file or a show, in its own window.
    fn report_windows(&mut self, ctx: &egui::Context) {
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
                            if ui.button("Close").clicked() {
                                closed.push(index);
                            }
                        });
                        ui.add_space(2.0);
                    });
                    egui::CentralPanel::default().show_inside(ui, |ui| {
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            ui.label(RichText::new(&report.title).size(17.0).strong());
                            ui.add_space(6.0);
                            ui.label(colored(&report.label, report.level).size(16.0).strong());
                            ui.label(&report.sentence);
                            ui.add_space(10.0);
                            for (heading, list, links) in [("Measurements", &report.measures, false), ("Tracks", &report.tracks, true)] {
                                if list.is_empty() {
                                    continue;
                                }
                                ui.label(RichText::new(heading).size(15.0).strong());
                                ui.separator();
                                for m in list {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(RichText::new(format!("{}:", m.name)).strong());
                                        ui.label(colored(&m.value, m.level));
                                        if links && ui.small_button("See more").clicked() {
                                            opened.push((*kind, path.join(&m.name)));
                                        }
                                    });
                                    if !m.meaning.is_empty() {
                                        ui.indent(egui::Id::new(("meaning", heading, &m.name)), |ui| {
                                            ui.label(RichText::new(&m.meaning).weak());
                                        });
                                    }
                                    ui.add_space(4.0);
                                }
                                ui.add_space(6.0);
                            }
                            if !report.reasoning.is_empty() {
                                ui.add_space(6.0);
                                ui.label(RichText::new("Reasoning").size(15.0).strong());
                                ui.separator();
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
                                    ui.add_space(3.0);
                                }
                            }
                            ui.add_space(8.0);
                            ui.label(RichText::new("Conclusion").size(15.0).strong());
                            ui.separator();
                            ui.label(colored(&report.sentence, report.level));
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

    /// Media info summaries, each in its own system window, copyable as text.
    fn media_windows(&mut self, ctx: &egui::Context) {
        let mut closed = Vec::new();
        for (index, (title, text)) in self.media_windows.iter().enumerate() {
            let id = egui::ViewportId::from_hash_of(("media info", title));
            let window_title = format!("Media info - {title}");
            let builder = egui::ViewportBuilder::default()
                .with_icon(app_icon())
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
        for index in saved {
            let spec = &self.spectrograms[index];
            let stem = spec.source.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let mut dialog = rfd::FileDialog::new()
                .set_title("Save spectrogram")
                .set_file_name(format!("{stem} - spectrogram.png"))
                .add_filter("PNG image", &["png"]);
            if let Some(dir) = spec.source.parent() {
                dialog = dialog.set_directory(dir);
            }
            if let Some(target) = dialog.save_file() {
                match crate::spectrogram::save_png(&spec.image, &target) {
                    Ok(()) => self.log(format!("Saved {}", target.display()), Level::Ok),
                    Err(e) => self.log(format!("Cannot save {}: {e}", target.display()), Level::Error),
                }
            }
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

/// "320 kbps (CBR)" and "VBR V0" entries.
/// "Always on top" (honoured by Windows and by X11 desktops; native Wayland has no such request).
fn set_window_level(ctx: &egui::Context, on_top: bool) {
    let level = if on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
    ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
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

/// "Clean WAV headers": copies with only the format and the sound.
fn clean_wav_button(ui: &mut egui::Ui, disabled: bool, actions: &mut Vec<Action>) {
    let tip = "Rewrite WAV files with only their format and their sound, in the canonical layout: extra chunks (LIST, id3, bext...), \
               non-standard headers and stray bytes are left out. Copies go to a WAV-cleaned folder and are checked to hold the same \
               sound; the originals are not touched. Some burning and SBE tools stumble on WAVs that are not canonical.";
    if ui.add_enabled(!disabled, egui::Button::new("🧹 Clean WAV headers")).on_hover_text(tip).clicked() {
        ui.close_menu();
        actions.push(Action::Run(Op::CleanWav));
    }
}

/// "Check against torrent...": every piece of the files hashed again and compared.
fn torrent_check_button(ui: &mut egui::Ui, disabled: bool, actions: &mut Vec<Action>) {
    let tip = "Check the show against a .torrent file: every piece is hashed again, and damaged, missing or resized files are named. \
               The torrent's folder is found by its name in the list or next to the .torrent. You can also drop .torrent files on the window.";
    if ui.add_enabled(!disabled, egui::Button::new("🧲 Check against torrent...")).on_hover_text(tip).clicked() {
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // The saved "Always on top" choice, once the window exists.
        if !self.window_level_set {
            self.window_level_set = true;
            if self.opt.always_on_top {
                set_window_level(ctx, true);
            }
        }
        self.poll();
        self.drops(ctx);
        let mut actions = Vec::new();
        self.shortcuts(ctx, &mut actions);
        self.menu_open = false;
        self.menu_and_toolbar(ctx, &mut actions);
        self.bottom(ctx);
        self.table(ctx, &mut actions);
        self.settings_window(ctx);
        self.about_window(ctx);
        self.info_window(ctx);
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
            format!("{APP_NAME} - {} show(s), {file_count} file(s)", self.shows.len())
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
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
