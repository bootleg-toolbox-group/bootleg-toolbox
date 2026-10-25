//! The interface fonts, built into the executable: Noto Sans Light and Noto Sans Mono (SIL Open
//! Font Licence), with egui's emoji fonts (Noto Emoji, OFL; emoji-icon-font, MIT). They
//! replace egui's defaults, whose Ubuntu and Hack fonts are not under OSI-approved licences.

use eframe::egui::{FontData, FontDefinitions, FontFamily, FontTweak};

/// The text font of the interface (and of the spectrogram legend).
pub const SANS: &[u8] = include_bytes!("../assets/fonts/NotoSans-Light.ttf");
const MONO: &[u8] = include_bytes!("../assets/fonts/NotoSansMono-Regular.ttf");
const EMOJI: &[u8] = include_bytes!("../assets/fonts/NotoEmoji-Regular.ttf");
const EMOJI_ICONS: &[u8] = include_bytes!("../assets/fonts/emoji-icon-font.ttf");

pub fn definitions() -> FontDefinitions {
    let mut defs = FontDefinitions::empty();
    defs.font_data.insert("sans".into(), FontData::from_static(SANS));
    defs.font_data.insert("mono".into(), FontData::from_static(MONO));
    // Emoji and symbols, a little smaller, as egui sets them.
    defs.font_data.insert("emoji".into(), FontData::from_static(EMOJI).tweak(FontTweak { scale: 0.81, ..Default::default() }));
    defs.font_data.insert("emoji-icons".into(), FontData::from_static(EMOJI_ICONS).tweak(FontTweak { scale: 0.90, ..Default::default() }));
    let fallbacks = ["emoji", "emoji-icons"].map(String::from);
    defs.families.insert(FontFamily::Proportional, [vec!["sans".into(), "mono".into()], fallbacks.to_vec()].concat());
    defs.families.insert(FontFamily::Monospace, [vec!["mono".into(), "sans".into()], fallbacks.to_vec()].concat());
    defs
}
