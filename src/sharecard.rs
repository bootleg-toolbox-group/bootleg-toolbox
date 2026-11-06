//! The share card of a show: a 1200 × 630 PNG to post with it (DIME, The Traders' Den,
//! forums) that says what was checked: the show, its format and source, the checks done and
//! their outcome, the cassette with its handwritten label, a spectrogram of one track.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ab_glyph::{Font, FontRef, ScaleFont};

use crate::ops::Level;
use crate::spectrogram::{Canvas, Image};

pub const WIDTH: usize = 1200;
pub const HEIGHT: usize = 630;

/// What the card says, gathered from the list by the window.
#[derive(Clone, Debug)]
pub struct Card {
    pub root: PathBuf,
    pub artist: String,
    /// "1987-08-31 · Sheffield City Hall".
    pub when_where: String,
    /// SBD, AUD...: its badge comes first.
    pub lineage: Option<&'static str>,
    /// "FLAC 16/44.1", "9 tracks", "45 min 42 s".
    pub facts: Vec<String>,
    /// The checks done: outcome, title, detail.
    pub checks: Vec<(Level, String, String)>,
    /// Handwritten on the cassette.
    pub label: String,
    /// The track whose spectrogram is shown, its caption and its lowpass (Hz).
    pub track: Option<(PathBuf, String, Option<f64>)>,
}

type Rgb = [u8; 3];

const GROUND: Rgb = [247, 245, 241];
const INK: Rgb = [31, 29, 26];
const WEAK: Rgb = [107, 101, 92];
const ACCENT: Rgb = [168, 86, 12];
const PILL: Rgb = [236, 230, 220];

/// Draws the card; the spectrogram's track is decoded for it.
pub fn render(card: &Card, cancel: &AtomicBool) -> Image {
    let mut c = Canvas { width: WIDTH, height: HEIGHT, rgba: [GROUND[0], GROUND[1], GROUND[2], 255].repeat(WIDTH * HEIGHT) };
    let sans = font(crate::fonts::SANS);
    let bold = font(crate::fonts::SANS_BOLD);
    let hand = font(crate::fonts::HAND);

    // Left: the show, its badges, the checks.
    let left = 56.0;
    let column = 640.0;
    c.circle(left + 7.0, 62.0, 7.0, ACCENT);
    c.text(&bold, left + 22.0, 48.0, 22.0, ACCENT, "Bootleg Toolbox");
    let artist = fit(&c, &bold, 60.0, column, &card.artist);
    c.text(&bold, left, 92.0, 60.0, INK, &artist);
    let when_where = fit(&c, &sans, 26.0, column, &card.when_where);
    c.text(&sans, left, 172.0, 26.0, WEAK, &when_where);
    let mut x = left;
    let pills = card.lineage.map(|l| (l.to_string(), lineage_colours(l))).into_iter().chain(card.facts.iter().map(|f| (f.clone(), (PILL, INK))));
    for (text, (fill, ink)) in pills {
        let font = if card.lineage.is_some_and(|l| l == text) { &bold } else { &sans };
        let w = c.text_width(font, 20.0, &text) + 28.0;
        if x + w > left + column {
            break;
        }
        c.round_rect(x, 222.0, w, 38.0, 19.0, fill);
        c.text(font, x + 14.0, 228.0, 20.0, ink, &text);
        x += w + 10.0;
    }
    let mut y = 292.0;
    for (level, title, detail) in card.checks.iter().take(4) {
        mark(&mut c, left + 18.0, y + 22.0, 18.0, *level);
        c.text(&bold, left + 52.0, y, 24.0, INK, &fit(&c, &bold, 24.0, column - 52.0, title));
        c.text(&sans, left + 52.0, y + 32.0, 18.0, WEAK, &fit(&c, &sans, 18.0, column - 52.0, detail));
        y += 66.0;
    }
    if card.checks.is_empty() {
        c.text(&sans, left, y, 20.0, WEAK, "No check run yet in Bootleg Toolbox");
    }
    let footer = format!(
        "Checked with Bootleg Toolbox {} · free and open source · github.com/bootleg-toolbox-group/bootleg-toolbox",
        crate::ops::APP_VERSION
    );
    c.text(&sans, left, 586.0, 16.0, WEAK, &footer);

    // Right: the cassette, and the spectrogram of a track under it.
    let right = 730.0;
    let width = WIDTH as f32 - 56.0 - right;
    let plot = card.track.as_ref().and_then(|(path, _, _)| crate::spectrogram::plot(path, cancel).ok());
    let cassette_width = if plot.is_some() { 330.0 } else { width };
    cassette(&mut c, &bold, &hand, right + (width - cassette_width) / 2.0, 44.0, cassette_width, &card.label);
    if let (Some(plot), Some((_, caption, cutoff))) = (plot, &card.track) {
        let (top, height) = (276.0, 230.0);
        spectrogram(&mut c, &plot, (right, top, width, height), *cutoff, &bold);
        c.text(&sans, right, top + height + 10.0, 17.0, WEAK, &fit(&c, &sans, 17.0, width, caption));
    }
    Image { width: WIDTH, height: HEIGHT, rgba: c.rgba }
}

/// Saves the card beside the show folder (never inside it): "<folder> - card.png".
pub fn save(card: &Card, image: &Image) -> Result<PathBuf, String> {
    let path = path_of(&card.root);
    crate::spectrogram::save_png(image, &path)?;
    Ok(path)
}

/// Where the card of the show in `root` goes: beside the folder, so that it stays out of the
/// show's torrent and checksums.
pub fn path_of(root: &Path) -> PathBuf {
    root.parent().unwrap_or(root).join(format!("{} - card.png", crate::files::file_name(root)))
}

fn font(data: &'static [u8]) -> FontRef<'static> {
    FontRef::try_from_slice(data).expect("the built-in font is valid")
}

/// `text` cut with an ellipsis to fit `width`.
fn fit(c: &Canvas, font: &FontRef<'static>, size: f32, width: f32, text: &str) -> String {
    if c.text_width(font, size, text) <= width {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let cut = format!("{}…", chars.iter().collect::<String>().trim_end());
        if c.text_width(font, size, &cut) <= width {
            return cut;
        }
    }
    String::new()
}

/// The badge colours of a recording source (fill, text), as in the list.
fn lineage_colours(kind: &str) -> (Rgb, Rgb) {
    match kind {
        "SBD" => ([226, 236, 248], [29, 79, 143]),
        "FM" | "Pre-FM" => ([239, 230, 247], [91, 45, 134]),
        "DAT" => ([230, 241, 239], [31, 95, 87]),
        "MTX" => ([236, 235, 245], [63, 58, 120]),
        _ => ([242, 227, 204], [122, 62, 6]),
    }
}

/// A check's outcome in a disc: a tick, an exclamation mark, a cross.
fn mark(c: &mut Canvas, x: f32, y: f32, r: f32, level: Level) {
    let (fill, ink): (Rgb, Rgb) = match level {
        Level::Ok => ([31, 138, 76], [255, 255, 255]),
        Level::Warn => ([232, 178, 20], [70, 50, 0]),
        Level::Error => ([196, 36, 30], [255, 255, 255]),
        Level::Info => ([46, 108, 196], [255, 255, 255]),
    };
    c.circle(x, y, r, fill);
    let w = r * 0.2;
    let at = |dx: f32, dy: f32| (x + dx * r, y + dy * r);
    match level {
        Level::Ok => {
            c.segment(at(-0.45, 0.02), at(-0.12, 0.34), w, ink);
            c.segment(at(-0.12, 0.34), at(0.45, -0.3), w, ink);
        }
        Level::Error => {
            c.segment(at(-0.36, -0.36), at(0.36, 0.36), w, ink);
            c.segment(at(0.36, -0.36), at(-0.36, 0.36), w, ink);
        }
        Level::Warn => {
            c.segment(at(0.0, -0.5), at(0.0, 0.12), w, ink);
            c.circle(x, y + 0.45 * r, w * 0.6, ink);
        }
        Level::Info => c.circle(x, y, w, ink),
    }
}

/// The cassette of the welcome screen (as `app::paint_cassette`), `width` wide.
fn cassette(c: &mut Canvas, bold: &FontRef<'static>, hand: &FontRef<'static>, x: f32, y: f32, width: f32, label: &str) {
    let s = width / 100.0;
    let at = |px: f32, py: f32| (x + px * s, y + py * s);
    let rect = |c: &mut Canvas, x0: f32, y0: f32, x1: f32, y1: f32, r: f32, colour: Rgb| {
        let (ax, ay) = at(x0, y0);
        c.round_rect(ax, ay, (x1 - x0) * s, (y1 - y0) * s, r * s, colour);
    };
    let (ink, tape) = ([43, 41, 37], [90, 47, 16]);
    rect(c, 1.0, 1.0, 99.0, 63.0, 5.0, [20, 19, 18]);
    rect(c, 1.6, 1.6, 98.4, 62.4, 4.5, [54, 50, 46]);
    rect(c, 8.0, 5.0, 92.0, 40.0, 2.5, [243, 230, 204]);
    rect(c, 8.0, 5.0, 92.0, 11.0, 2.5, ACCENT);
    rect(c, 8.0, 8.5, 92.0, 11.0, 0.0, ACCENT);
    let tag = "C90 · TYPE II";
    let size = 3.2 * s;
    let (tx, ty) = at(88.0, 8.0);
    c.text(bold, tx - c.text_width(bold, size, tag), ty - 0.62 * size, size, [251, 233, 207], tag);
    let (lx, ly) = at(11.0, 17.0);
    c.round_rect(lx, ly, 78.0 * s, (0.3 * s).max(1.0), 0.0, [217, 199, 166]);
    let size = 6.4 * s;
    let text = fit(c, hand, size, 76.0 * s, label);
    let (hx, hy) = at(12.0, 16.4);
    c.text(hand, hx, hy - hand.as_scaled(size).ascent(), size, [59, 42, 26], &text);
    // The window, the reels (a little more tape on the left), the hubs.
    rect(c, 25.7, 17.7, 74.3, 35.3, 8.8, ink);
    rect(c, 26.3, 18.3, 73.7, 34.7, 8.2, [239, 227, 205]);
    for (cx, r) in [(38.0, 7.0), (62.0, 5.9)] {
        let (px, py) = at(cx, 26.5);
        c.circle(px, py, r * s, [61, 30, 8]);
        c.circle(px, py, (r - 0.35) * s, tape);
        c.circle(px, py, 3.6 * s, ink);
        c.circle(px, py, 3.2 * s, [244, 239, 230]);
        c.circle(px, py, 1.4 * s, ink);
        c.circle(px, py - 2.5 * s, 0.9 * s, [217, 130, 43]);
    }
    let (sx, sy) = at(36.0, 34.4);
    c.segment((sx, sy), (sx + 28.0 * s, sy), 0.9 * s, tape);
    // The bottom edge and the screws.
    let bottom = [at(23.0, 63.0), at(29.0, 51.0), at(71.0, 51.0), at(77.0, 63.0)];
    c.polygon(&bottom, [44, 41, 38]);
    let (bx, by) = at(29.5, 51.8);
    c.segment((bx, by), (bx + 41.0 * s, by), 0.7 * s, tape);
    for hx in [35.0, 65.0] {
        let (px, py) = at(hx, 57.5);
        c.circle(px, py, 1.7 * s, [14, 13, 12]);
    }
    rect(c, 46.5, 54.0, 53.5, 57.4, 0.6, [201, 164, 106]);
    for (px, py) in [(5.0, 5.0), (95.0, 5.0), (5.0, 59.0), (95.0, 59.0), (50.0, 45.5)] {
        let (qx, qy) = at(px, py);
        c.circle(qx, qy, 1.4 * s, [143, 138, 130]);
        c.segment((qx - 0.9 * s, qy), (qx + 0.9 * s, qy), 0.4 * s, [74, 70, 63]);
    }
}

/// The track's spectrogram, scaled into the box with rounded corners, and its lowpass.
fn spectrogram(c: &mut Canvas, plot: &crate::spectrogram::Plot, (x, y, w, h): (f32, f32, f32, f32), cutoff: Option<f64>, bold: &FontRef<'static>) {
    let radius = 12.0;
    for py in y as usize..(y + h) as usize {
        for px in x as usize..(x + w) as usize {
            let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
            let cover = (0.5 - round_rect_distance(cx, cy, x, y, w, h, radius)).clamp(0.0, 1.0);
            if cover <= 0.0 {
                continue;
            }
            // The average of the plot's pixels under this one.
            let (u0, u1) = (((cx - 0.5 - x) / w * plot.width as f32) as usize, ((cx + 0.5 - x) / w * plot.width as f32) as usize);
            let (v0, v1) = (((cy - 0.5 - y) / h * plot.height as f32) as usize, ((cy + 0.5 - y) / h * plot.height as f32) as usize);
            let mut sum = [0u32; 3];
            let mut n = 0;
            for v in v0..v1.max(v0 + 1).min(plot.height) {
                for u in u0..u1.max(u0 + 1).min(plot.width) {
                    let i = (v * plot.width + u) * 4;
                    for (total, value) in sum.iter_mut().zip(&plot.rgba[i..i + 3]) {
                        *total += *value as u32;
                    }
                    n += 1;
                }
            }
            let n = n.max(1);
            c.blend(px as i32, py as i32, [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8], cover);
        }
    }
    if let Some(hz) = cutoff.filter(|hz| *hz < plot.nyquist) {
        let gold = [255, 210, 122];
        let line = y + h - (hz / plot.nyquist) as f32 * h;
        let mut dash = x;
        while dash < x + w {
            c.segment((dash, line), ((dash + 9.0).min(x + w), line), 2.5, gold);
            dash += 15.0;
        }
        let text = format!("lowpass {:.1} kHz", hz / 1000.0);
        let tw = c.text_width(bold, 15.0, &text);
        c.round_rect(x + w - tw - 26.0, line - 32.0, tw + 16.0, 24.0, 6.0, [13, 7, 22]);
        c.text(bold, x + w - tw - 18.0, line - 30.0, 15.0, gold, &text);
    }
}

/// Signed distance from (px, py) to a rounded rectangle (negative inside).
fn round_rect_distance(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let r = r.min(w / 2.0).min(h / 2.0);
    let qx = (px - (x + w / 2.0)).abs() - (w / 2.0 - r);
    let qy = (py - (y + h / 2.0)).abs() - (h / 2.0 - r);
    let outside = libm::hypotf(qx.max(0.0), qy.max(0.0));
    outside + qx.max(qy).min(0.0) - r
}

impl Canvas {
    /// Fills the pixels around a shape given by its signed distance, smoothed over a pixel.
    fn shape(&mut self, (x0, y0, x1, y1): (f32, f32, f32, f32), colour: Rgb, distance: impl Fn(f32, f32) -> f32) {
        let (xa, ya) = ((x0 - 1.0).max(0.0) as usize, (y0 - 1.0).max(0.0) as usize);
        let (xb, yb) = (((x1 + 2.0).max(0.0) as usize).min(self.width), ((y1 + 2.0).max(0.0) as usize).min(self.height));
        for py in ya..yb {
            for px in xa..xb {
                let cover = (0.5 - distance(px as f32 + 0.5, py as f32 + 0.5)).clamp(0.0, 1.0);
                if cover > 0.0 {
                    self.blend(px as i32, py as i32, colour, cover);
                }
            }
        }
    }

    fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, colour: Rgb) {
        self.shape((x, y, x + w, y + h), colour, |px, py| round_rect_distance(px, py, x, y, w, h, r));
    }

    fn circle(&mut self, x: f32, y: f32, r: f32, colour: Rgb) {
        self.shape((x - r, y - r, x + r, y + r), colour, |px, py| libm::hypotf(px - x, py - y) - r);
    }

    /// A line `width` wide with round ends.
    fn segment(&mut self, (ax, ay): (f32, f32), (bx, by): (f32, f32), width: f32, colour: Rgb) {
        let half = width / 2.0;
        let (dx, dy) = (bx - ax, by - ay);
        let length2 = (dx * dx + dy * dy).max(1e-6);
        self.shape((ax.min(bx) - half, ay.min(by) - half, ax.max(bx) + half, ay.max(by) + half), colour, |px, py| {
            let t = (((px - ax) * dx + (py - ay) * dy) / length2).clamp(0.0, 1.0);
            libm::hypotf(px - (ax + t * dx), py - (ay + t * dy)) - half
        });
    }

    /// A convex polygon, its corners in either order.
    fn polygon(&mut self, points: &[(f32, f32)], colour: Rgb) {
        let n = points.len() as f32;
        let centre = (points.iter().map(|p| p.0).sum::<f32>() / n, points.iter().map(|p| p.1).sum::<f32>() / n);
        let edges: Vec<(f32, f32, f32)> = points
            .iter()
            .zip(points.iter().cycle().skip(1))
            .map(|(a, b)| {
                let (nx, ny) = (b.1 - a.1, a.0 - b.0);
                let length = libm::hypotf(nx, ny).max(1e-6);
                let (nx, ny) = (nx / length, ny / length);
                // Outward: away from the centre.
                let sign = if (centre.0 - a.0) * nx + (centre.1 - a.1) * ny > 0.0 { -1.0 } else { 1.0 };
                (nx * sign, ny * sign, -(a.0 * nx + a.1 * ny) * sign)
            })
            .collect();
        let bounds = points.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |b, p| (b.0.min(p.0), b.1.min(p.1), b.2.max(p.0), b.3.max(p.1)));
        self.shape(bounds, colour, |px, py| edges.iter().map(|(nx, ny, d)| px * nx + py * ny + d).fold(f32::MIN, f32::max));
    }
}

/// The track a card shows: the longest of the show.
pub fn longest(files: &[PathBuf], seconds: impl Fn(&Path) -> f64) -> Option<PathBuf> {
    files.iter().max_by(|a, b| seconds(a).total_cmp(&seconds(b))).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_draws_without_audio() {
        let card = Card {
            root: PathBuf::from("/nowhere/Show"),
            artist: "An artist with a very long name that will not fit on the card at all, really".into(),
            when_where: "1987-08-31 · A hall".into(),
            lineage: Some("AUD"),
            facts: vec!["FLAC 16/44.1".into(), "9 tracks".into()],
            checks: vec![(Level::Ok, "Lossless origin".into(), "9 tracks".into()), (Level::Warn, "Speed to check".into(), "+1.9 %".into())],
            label: "An artist – A hall 1987-08-31".into(),
            track: None,
        };
        let image = render(&card, &AtomicBool::new(false));
        assert_eq!((image.width, image.height, image.rgba.len()), (WIDTH, HEIGHT, WIDTH * HEIGHT * 4));
        // Something was drawn on the ground.
        assert!(image.rgba.as_chunks::<4>().0.iter().any(|p| p[..3] != GROUND));
    }
}
