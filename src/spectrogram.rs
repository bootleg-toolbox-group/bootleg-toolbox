//! Spectrogram of a whole track, computed and drawn natively: FFT, colours and
//! legend (frequency, time, dBFS scale) in an RGBA image that can be saved as PNG.

use std::borrow::Cow;
use std::f32::consts::PI;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;

use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use eframe::egui;

use crate::audio::{self, AudioInfo};
use crate::dsp::{self, Fft};
use crate::files::file_name;

const FFT_SIZE: usize = 1024;
/// Frequency rows (FFT bins from 0 Hz up to just below Nyquist).
const ROWS: usize = FFT_SIZE / 2;
/// Time columns, whatever the length of the track.
const COLS: usize = 1024;
const FLOOR_DB: f32 = -120.0;

const LEFT: usize = 74;
const RIGHT: usize = 96;
const TOP: usize = 70;
const BOTTOM: usize = 50;
/// Legend text sizes, in pixels.
const TITLE_SIZE: f32 = 22.0;
const SUBTITLE_SIZE: f32 = 17.0;
const LABEL_SIZE: f32 = 17.0;

const BACKGROUND: [u8; 3] = [24, 24, 24];
const TEXT: [u8; 3] = [240, 240, 240];
const DIM: [u8; 3] = [175, 175, 175];

pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

pub fn render(path: &Path, cancel: &AtomicBool) -> Result<Image, String> {
    let info = audio::probe(path)?;
    if info.channels == 0 || info.rate == 0 || info.bits == 0 {
        return Err("invalid audio header".into());
    }
    let levels = analyse(path, &info, cancel)?;
    Ok(draw(path, &info, &levels))
}

pub fn save_png(image: &Image, path: &Path) -> Result<(), String> {
    let file = File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), image.width as u32, image.height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&image.rgba).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- analysis

/// Average power of every (column, frequency row), in dBFS, column after column.
fn analyse(path: &Path, info: &AudioInfo, cancel: &AtomicBool) -> Result<Vec<f32>, String> {
    let channels = info.channels.max(1) as usize;
    let total = info.samples.max(1);
    // Long tracks: half-overlapping windows averaged per column; short ones: about one per column.
    let hop = ((total / COLS as u64) as usize).clamp(1, FFT_SIZE / 2);

    let fft = Fft::new(FFT_SIZE);
    let window = dsp::hann(FFT_SIZE);
    let mut history: Vec<Vec<f64>> = vec![Vec::with_capacity(FFT_SIZE * 8); channels];
    let mut offset = 0usize; // start of the next window in `history`
    let mut start = 0u64; // its position in the track, in sample frames
    let mut power = vec![0f64; COLS * ROWS];
    let mut counts = vec![0u32; COLS];
    let (mut re, mut im) = (Vec::new(), Vec::new());
    let mut windowed = vec![0f64; FFT_SIZE];
    let mut bins = vec![0f64; FFT_SIZE / 2 + 1];
    let mut windows = 0usize;

    let mut add_window = |history: &[Vec<f64>], offset: usize, start: u64| {
        let center = start + FFT_SIZE as u64 / 2;
        let col = ((center * COLS as u64 / total) as usize).min(COLS - 1);
        for samples in history {
            for i in 0..FFT_SIZE {
                windowed[i] = samples[offset + i] * window[i];
            }
            fft.power(&windowed, &mut re, &mut im, &mut bins);
            let row = &mut power[col * ROWS..(col + 1) * ROWS];
            for (p, b) in row.iter_mut().zip(&bins) {
                *p += b / channels as f64;
            }
        }
        counts[col] += 1;
    };

    dsp::for_each_frame(path, info, cancel, |values, _| {
        for (h, v) in history.iter_mut().zip(values) {
            h.push(*v);
        }
        if history[0].len() - offset >= FFT_SIZE {
            add_window(&history, offset, start);
            windows += 1;
            offset += hop;
            start += hop as u64;
            if offset > FFT_SIZE * 4 {
                for h in history.iter_mut() {
                    h.drain(..offset);
                }
                offset = 0;
            }
        }
    })?;
    if windows == 0 {
        // Shorter than one window: pad with silence.
        for h in history.iter_mut() {
            h.resize(offset + FFT_SIZE, 0.0);
        }
        add_window(&history, offset, start);
    }

    // A full-scale sine through a Hann window peaks at FFT_SIZE / 4.
    let reference = (FFT_SIZE as f64 / 4.0).powi(2);
    let mut levels = vec![FLOOR_DB; COLS * ROWS];
    let mut last = None;
    for col in 0..COLS {
        // Columns without a window of their own (rounding, end of track) repeat their neighbour.
        let source = if counts[col] > 0 { Some(col) } else { last.or_else(|| (col..COLS).find(|&c| counts[c] > 0)) };
        let Some(src) = source else { continue };
        if counts[col] > 0 {
            last = Some(col);
        }
        for k in 0..ROWS {
            let p = power[src * ROWS + k] / counts[src] as f64 / reference;
            levels[col * ROWS + k] = if p > 0.0 { (10.0 * p.log10()) as f32 } else { FLOOR_DB };
        }
    }
    Ok(levels)
}

// ---------------------------------------------------------------- drawing

/// 0.0 (floor) .. 1.0 (0 dBFS): black, purple, red, orange, yellow, white.
fn palette(level: f32) -> [u8; 3] {
    let l = level.clamp(0.0, 1.0);
    let r = if l < 0.13 { 0.0 } else if l < 0.73 { ((l - 0.13) / 0.60 * PI / 2.0).sin() } else { 1.0 };
    let g = if l < 0.60 { 0.0 } else if l < 0.91 { ((l - 0.60) / 0.31 * PI / 2.0).sin() } else { 1.0 };
    let b = if l < 0.60 { 0.5 * (l / 0.60 * PI).sin() } else if l < 0.78 { 0.0 } else { (l - 0.78) / 0.22 };
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}

fn draw(path: &Path, info: &AudioInfo, levels: &[f32]) -> Image {
    let width = LEFT + COLS + RIGHT;
    let height = TOP + ROWS + BOTTOM;
    let mut canvas = Canvas { width, height, rgba: Vec::with_capacity(width * height * 4) };
    for _ in 0..width * height {
        canvas.rgba.extend_from_slice(&[BACKGROUND[0], BACKGROUND[1], BACKGROUND[2], 255]);
    }

    for col in 0..COLS {
        for k in 0..ROWS {
            let level = (levels[col * ROWS + k] - FLOOR_DB) / -FLOOR_DB;
            canvas.set(LEFT + col, TOP + ROWS - 1 - k, palette(level));
        }
    }
    canvas.rect_outline(LEFT - 1, TOP - 1, COLS + 2, ROWS + 2, DIM);

    let font = font();
    // Title: file name, then the format.
    canvas.text(&font, LEFT as f32, 8.0, TITLE_SIZE, TEXT, &file_name(path));
    let channels = match info.channels {
        1 => "mono".to_string(),
        2 => "stereo".to_string(),
        n => format!("{n} channels"),
    };
    let float = if info.float { " float" } else { "" };
    let subtitle = format!(
        "{}, {}-bit{float}, {} kHz, {channels}, {}",
        info.format.name(),
        info.bits,
        audio::trim_float(info.rate as f64 / 1000.0),
        info.length()
    );
    canvas.text(&font, LEFT as f32, 38.0, SUBTITLE_SIZE, DIM, &subtitle);

    // Frequency axis (kHz).
    let nyquist = info.rate as f64 / 2.0;
    let step = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0].into_iter().find(|s| nyquist / 1000.0 / s <= 12.0).unwrap_or(100.0);
    let mut khz = 0.0;
    while khz * 1000.0 <= nyquist {
        let y = TOP + ROWS - 1 - ((khz * 1000.0 / nyquist) * (ROWS - 1) as f64).round() as usize;
        canvas.hline(LEFT - 7, LEFT - 1, y, DIM);
        let label = audio::trim_float(khz);
        let w = canvas.text_width(&font, LABEL_SIZE, &label);
        canvas.text(&font, LEFT as f32 - 11.0 - w, y as f32 - LABEL_SIZE * 0.55, LABEL_SIZE, TEXT, &label);
        khz += step;
    }
    let w = canvas.text_width(&font, LABEL_SIZE, "kHz");
    canvas.text(&font, LEFT as f32 - 11.0 - w, TOP as f32 + ROWS as f32 + 10.0, LABEL_SIZE, DIM, "kHz");

    // Time axis.
    let seconds = info.seconds();
    if seconds > 0.0 {
        let step = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0]
            .into_iter()
            .find(|s| seconds / s <= 10.0)
            .unwrap_or(7200.0);
        let mut t = 0.0;
        while t <= seconds {
            let x = LEFT + ((t / seconds) * (COLS - 1) as f64).round() as usize;
            canvas.vline(x, TOP + ROWS, TOP + ROWS + 7, DIM);
            let label = clock(t);
            let w = canvas.text_width(&font, LABEL_SIZE, &label);
            let left = (x as f32 - w / 2.0).clamp(LEFT as f32 - 4.0, (LEFT + COLS) as f32 - w + 4.0);
            canvas.text(&font, left, (TOP + ROWS + 12) as f32, LABEL_SIZE, TEXT, &label);
            t += step;
        }
    }

    // dBFS colour scale.
    let bar = LEFT + COLS + 14;
    for y in 0..ROWS {
        let color = palette(1.0 - y as f32 / (ROWS - 1) as f32);
        for x in bar..bar + 12 {
            canvas.set(x, TOP + y, color);
        }
    }
    canvas.rect_outline(bar - 1, TOP - 1, 14, ROWS + 2, DIM);
    canvas.text(&font, bar as f32 - 4.0, TOP as f32 - LABEL_SIZE - 8.0, LABEL_SIZE, DIM, "dBFS");
    let mut db = 0.0f32;
    while db >= FLOOR_DB {
        let y = TOP + ((db / FLOOR_DB) * (ROWS - 1) as f32).round() as usize;
        canvas.hline(bar + 12, bar + 17, y, DIM);
        canvas.text(&font, (bar + 21) as f32, y as f32 - LABEL_SIZE * 0.55, LABEL_SIZE, TEXT, &format!("{db}"));
        db -= 20.0;
    }

    Image { width, height, rgba: canvas.rgba }
}

fn clock(seconds: f64) -> String {
    let s = seconds.round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// egui's own proportional font, so the legend needs no font file.
fn font() -> FontRef<'static> {
    static BYTES: OnceLock<&'static [u8]> = OnceLock::new();
    let bytes = BYTES.get_or_init(|| {
        let mut defs = egui::FontDefinitions::default();
        let name = defs
            .families
            .get(&egui::FontFamily::Proportional)
            .and_then(|names| names.first().cloned())
            .unwrap_or_default();
        match defs.font_data.remove(&name).map(|d| d.font) {
            Some(Cow::Borrowed(b)) => b,
            Some(Cow::Owned(v)) => Box::leak(v.into_boxed_slice()),
            None => &[],
        }
    });
    FontRef::try_from_slice(bytes).expect("egui ships a valid default font")
}

struct Canvas {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

impl Canvas {
    fn set(&mut self, x: usize, y: usize, c: [u8; 3]) {
        if x < self.width && y < self.height {
            let i = (y * self.width + x) * 4;
            self.rgba[i..i + 3].copy_from_slice(&c);
        }
    }

    fn blend(&mut self, x: i32, y: i32, c: [u8; 3], alpha: f32) {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return;
        }
        let i = (y as usize * self.width + x as usize) * 4;
        let a = alpha.clamp(0.0, 1.0);
        for k in 0..3 {
            self.rgba[i + k] = (self.rgba[i + k] as f32 * (1.0 - a) + c[k] as f32 * a).round() as u8;
        }
    }

    fn hline(&mut self, x0: usize, x1: usize, y: usize, c: [u8; 3]) {
        for x in x0..=x1 {
            self.set(x, y, c);
        }
    }

    fn vline(&mut self, x: usize, y0: usize, y1: usize, c: [u8; 3]) {
        for y in y0..=y1 {
            self.set(x, y, c);
        }
    }

    fn rect_outline(&mut self, x: usize, y: usize, w: usize, h: usize, c: [u8; 3]) {
        self.hline(x, x + w - 1, y, c);
        self.hline(x, x + w - 1, y + h - 1, c);
        self.vline(x, y, y + h - 1, c);
        self.vline(x + w - 1, y, y + h - 1, c);
    }

    fn text_width(&self, font: &FontRef<'static>, size: f32, text: &str) -> f32 {
        let scaled = font.as_scaled(PxScale::from(size));
        let mut width = 0.0;
        let mut prev = None;
        for ch in text.chars() {
            let id = font.glyph_id(ch);
            if let Some(p) = prev {
                width += scaled.kern(p, id);
            }
            width += scaled.h_advance(id);
            prev = Some(id);
        }
        width
    }

    /// Draws `text` with its top-left corner at (x, y).
    fn text(&mut self, font: &FontRef<'static>, x: f32, y: f32, size: f32, color: [u8; 3], text: &str) {
        let scale = PxScale::from(size);
        let scaled = font.as_scaled(scale);
        let baseline = y + scaled.ascent();
        let mut caret = x;
        let mut prev = None;
        for ch in text.chars() {
            let id = font.glyph_id(ch);
            if let Some(p) = prev {
                caret += scaled.kern(p, id);
            }
            let glyph = id.with_scale_and_position(scale, point(caret, baseline));
            caret += scaled.h_advance(id);
            prev = Some(id);
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, coverage| {
                    self.blend(bounds.min.x as i32 + gx as i32, bounds.min.y as i32 + gy as i32, color, coverage);
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_sine_is_0_dbfs() {
        let bin = 64;
        let window = dsp::hann(FFT_SIZE);
        let signal: Vec<f64> =
            (0..FFT_SIZE).map(|i| (2.0 * std::f64::consts::PI * (bin * i) as f64 / FFT_SIZE as f64).sin() * window[i]).collect();
        let mut bins = vec![0.0; FFT_SIZE / 2 + 1];
        Fft::new(FFT_SIZE).power(&signal, &mut Vec::new(), &mut Vec::new(), &mut bins);
        let power = bins[bin] / (FFT_SIZE as f64 / 4.0).powi(2);
        assert!((10.0 * power.log10()).abs() < 0.01);
    }
}
