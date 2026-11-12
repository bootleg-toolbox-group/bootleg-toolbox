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
    /// A track's lowpass (Hz), drawn on its spectrogram in the analysis window.
    pub cutoff: Option<f64>,
    /// The speed error the verdict gives (%, positive: plays fast), on the Speed? dial.
    pub speed: Option<f64>,
    /// The clues of a verdict, by family (Lossless? of a file).
    pub evidence: Vec<Evidence>,
    /// How sure the verdict is (Lossless? of a file).
    pub confidence: Option<Confidence>,
}

/// One clue of a verdict: its family, the side it takes and how much it weighs.
#[derive(Clone, Debug)]
pub struct Evidence {
    pub family: Family,
    pub side: Side,
    pub strength: Strength,
    pub text: String,
}

/// Kinds of clues, each measuring something of a different nature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    /// The rounding grid of an encoder, lossyWAV's zeroed bits.
    Coding,
    /// Holes below the lowpass, what lies above it, the stereo of the treble.
    Structure,
    /// Digital processing that came last (a resampler, dither): caps the verdict.
    Processing,
    /// Where the recording comes from (FM, upsampled, 16-bit): said, never a lossy clue.
    Origin,
    /// The shape of the lowpass: shown, never decides.
    Shape,
}

impl Family {
    pub fn name(self) -> &'static str {
        match self {
            Family::Coding => "Coding traces",
            Family::Structure => "Structure of the spectrum",
            Family::Processing => "Digital processing",
            Family::Origin => "Origin",
            Family::Shape => "Shape of the lowpass",
        }
    }
}

/// What a clue says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Lossy,
    Lossless,
    /// Neither way.
    Neutral,
    /// Lowers the best verdict a file can get.
    Cap,
    /// About the origin only.
    Origin,
}

impl Side {
    pub fn name(self) -> &'static str {
        match self {
            Side::Lossy => "lossy",
            Side::Lossless => "lossless",
            Side::Neutral => "neutral",
            Side::Cap => "limits the verdict",
            Side::Origin => "origin",
        }
    }
}

/// How sure a verdict is, said in words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    pub fn name(self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Strength {
    /// Proof, or close to it: coding traces, encoder holes, a lossy format.
    Strong,
    /// Points one way, but another cause can give it.
    Moderate,
    /// A hint only.
    Weak,
}

impl Strength {
    pub fn name(self) -> &'static str {
        match self {
            Strength::Strong => "strong",
            Strength::Moderate => "moderate",
            Strength::Weak => "weak",
        }
    }
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
    /// The clues family by family, the strongest first in each.
    pub fn by_family(&self) -> Vec<(Family, Vec<&Evidence>)> {
        let mut families: Vec<Family> = self.evidence.iter().map(|e| e.family).collect();
        families.sort();
        families.dedup();
        families
            .into_iter()
            .map(|family| {
                let mut items: Vec<&Evidence> = self.evidence.iter().filter(|e| e.family == family).collect();
                items.sort_by_key(|e| e.strength);
                (family, items)
            })
            .collect()
    }

    pub fn to_text(&self) -> String {
        let mut out = format!(
            "{}\n{}\n\nVerdict: {}{}\n{}\n\n{}\n",
            self.title,
            "=".repeat(self.title.chars().count()),
            self.label,
            self.confidence.map(|c| format!(" (confidence: {})", c.name())).unwrap_or_default(),
            self.sentence,
            if self.measures.is_empty() { "Tracks" } else { "Measurements" }
        );
        for m in &self.measures {
            out.push_str(&format!("- {}: {}\n", m.name, m.value));
            if !m.meaning.is_empty() {
                out.push_str(&format!("  {}\n", m.meaning));
            }
        }
        for (family, items) in self.by_family() {
            out.push_str(&format!("\n{}\n", family.name()));
            for e in items {
                out.push_str(&format!("- [{}, {}] {}\n", e.side.name(), e.strength.name(), e.text));
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
        out.push_str(&format!("\n({}, Bootleg Toolbox {})\n", self.heading, crate::ops::APP_VERSION));
        out
    }
}

