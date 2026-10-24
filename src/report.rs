//! The full analysis behind a verdict ("See more"): shown in its own window, copyable as text.

use crate::ops::Level;

/// The full analysis behind a verdict, shown in its own window and copyable as text.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// "Lossless analysis", "Speed analysis".
    pub heading: String,
    pub title: String,
    pub label: String,
    pub level: Level,
    pub sentence: String,
    pub measures: Vec<Measure>,
    pub reasoning: Vec<String>,
    /// Short figures for the show report ("cut 16.1 kHz, holes 38 dB").
    pub key_facts: String,
    /// A show report: one line per track (verdict, key figures), each with its own report.
    pub tracks: Vec<Measure>,
}

#[derive(Clone, Debug)]
pub struct Measure {
    pub name: String,
    pub value: String,
    /// What is normal, what it means.
    pub meaning: String,
    pub level: Level,
}

impl Report {
    pub fn to_text(&self) -> String {
        let mut out = format!(
            "{}\n{}\n\nVerdict: {}\n{}\n\n{}\n",
            self.title,
            "=".repeat(self.title.chars().count()),
            self.label,
            self.sentence,
            if self.measures.is_empty() { "Tracks" } else { "Measurements" }
        );
        for m in &self.measures {
            out.push_str(&format!("- {}: {}\n", m.name, m.value));
            if !m.meaning.is_empty() {
                out.push_str(&format!("  {}\n", m.meaning));
            }
        }
        if !self.tracks.is_empty() {
            if !self.measures.is_empty() {
                out.push_str("\nTracks\n");
            }
            for t in &self.tracks {
                out.push_str(&format!("- {}: {}{}\n", t.name, t.value, if t.meaning.is_empty() { String::new() } else { format!(" ({})", t.meaning) }));
            }
        }
        if !self.reasoning.is_empty() {
            out.push_str("\nReasoning\n");
            let mut step = 0;
            for line in &self.reasoning {
                // Indented lines belong to the step above them.
                match line.strip_prefix("    ") {
                    Some(sub) => out.push_str(&format!("   - {sub}\n")),
                    None => {
                        step += 1;
                        out.push_str(&format!("{step}. {line}\n"));
                    }
                }
            }
        }
        out.push_str(&format!("\n({}, Bootleg Helper {})\n", self.heading, crate::ops::APP_VERSION));
        out
    }
}

