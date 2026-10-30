//! Bootleg Toolbox as a library: everything but the entry point (src/main.rs), so that
//! tests and fuzz targets can reach the file readers.

pub mod app;
pub mod audio;
pub mod dsp;
pub mod encode;
pub mod dvd;
pub mod files;
pub mod fonts;
pub mod help;
pub mod integration;
pub mod lossless;
pub mod aactrace;
pub mod mp3trace;
pub mod mediainfo;
pub mod ops;
pub mod report;
pub mod sbe;
pub mod setlistfm;
pub mod torrent_check;
pub mod update;
pub mod shn;
pub mod speed;
pub mod wv;
pub mod spectrogram;
pub mod tagging;
pub mod undo;
