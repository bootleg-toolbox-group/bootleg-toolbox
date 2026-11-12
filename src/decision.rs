//! The decision of "Lossless?": the verdict of a file from its measures (`lossless::Facts`),
//! and the verdict of a show from those of its files. Pure functions: the same measures always
//! give the same verdict, without reading the audio again.

use std::path::Path;

use crate::lossless::{
    build_report, doubtful, upsampled_from, Facts, Measure, Report, Verdict, ANALOG_COHERENCE, ANALOG_STEP, FM_15K, LIKELY_LOSSLESS,
    LOSSLESS_PADDED, LOSSLESS_UPSAMPLED, LOSSY_UPSTREAM, TREBLE_LIKELY, TREBLE_PLAIN, TREBLE_UNCLEAR,
};
use crate::ops::Level;

/// The lines of the tooltip: the measures that matter, in the order they are looked at.
fn details(f: &Facts) -> Vec<String> {
    let mut details = Vec::new();
    if let Some(t) = f.mp3_trace.filter(|t| t.found()) {
        details.push(format!("MP3 coding traces found ({:.1} % of the bands, {:.0} standard deviations above chance)", t.likelihood * 100.0, t.contrast));
    }
    if f.frames == 0 {
        return details;
    }
    let nyquist = f.rate / 2.0;
    if let Some(e) = f.edge {
        details.push(match f.cutoff {
            Some(_) => format!("Edge of the lowpass: {e:.0} dB per octave"),
            None => format!("Treble roll-off (10 to 16 kHz): {e:.0} dB per octave"),
        });
    }
    match f.cutoff {
        Some(c) => details.push(format!("Steep lowpass at {:.1} kHz ({:.0} dB drop)", c / 1000.0, f.drop)),
        None => details.push(format!("No brick-wall lowpass between 10 kHz and {:.1} kHz", (nyquist - 700.0) / 1000.0)),
    }
    if let (Some(c), Some(holes)) = (f.cutoff, f.holes) {
        details.push(format!("Encoder holes just below the cutoff: {holes:.0} dB"));
        if let Some(a) = f.above_floor {
            details.push(format!("Floor above the cutoff: {a:+.0} dB over digital silence"));
        }
        if let Some(h) = f.high_floor.filter(|_| c < 18000.0) {
            details.push(format!("Floor at 18-19.5 kHz: {h:+.0} dB over digital silence"));
        }
    }
    if let Some(m) = f.masked.filter(|m| m.dithered()) {
        details.push(format!(
            "The music stops at {:.1} kHz; above it only steady noise ({:+.0} dB over digital silence{}), as the noise-shaped dither of a conversion to 16-bit leaves",
            m.cutoff / 1000.0,
            m.floor_above,
            m.rise.map(|r| format!(", rising {r:.0} dB toward 19 kHz")).unwrap_or_default()
        ));
    }
    if let Some(r) = f.roll {
        details.push(format!(
            "Edge of the treble: drops by {:.0} dB at most between 300 Hz bands; the band above follows the music at {:.2}",
            r.0, r.1
        ));
    }
    if let Some(h) = f.hf {
        details.push(format!("15-19.5 kHz level: {h:.0} dB relative to 1-4 kHz"));
    }
    if f.pilot_on_19k {
        details.push("19 kHz FM stereo pilot tone found".into());
    } else if let (Some(hz), Some(s)) = (f.shifted_pilot, &f.speed) {
        details.push(format!("19 kHz FM stereo pilot tone found at {:.2} kHz: {s}", hz / 1000.0));
    }
    if let Some(hz) = f.line_whistle {
        details.push(format!("Tone at {:.2} kHz: TV line frequency (TV simulcast, or a TV set near the recorder)", hz / 1000.0));
    }
    if let Some(gap) = f.hires_gap {
        details.push(format!("Level 27-30 kHz vs 18.5-20.5 kHz: {gap:.0} dB"));
    }
    if let Some(c) = f.cutoff.filter(|_| f.from_44k) {
        details.push(format!("Content stops at {:.1} kHz with digital silence above: possibly upsampled from a 44.1 kHz source", c / 1000.0));
    }
    if let Some(p) = &f.padded {
        details.push(p.clone());
    } else if let Some(c) = f.comb.filter(|c| c.1 < 23.0) {
        details.push(format!("About {:.0} bits really used (1 sample value in {:.0} occurs)", c.1, 1.0 / c.0));
    }
    if let Some(w) = f.waste.filter(|_| f.lossywav) {
        details.push(format!("lossyWAV: lowest bits zeroed in {:.0} % of the blocks ({:.1} bits on average)", w.0 * 100.0, w.1));
    }
    details
}

/// The verdict for a file from its measures.
pub(crate) fn decide(path: &Path, f: &Facts) -> Verdict {
    let details = details(f);
    if f.frames == 0 {
        let mut verdict = Verdict::new("Silent", "Digital silence only, nothing to analyse", Level::Info, details);
        verdict.report = Report {
            heading: "Lossless analysis".into(),
            title: crate::files::file_name(path),
            label: verdict.label.clone(),
            level: verdict.level,
            sentence: verdict.sentence.clone(),
            reasoning: vec!["Every sample is zero: there is no audio to analyse.".into()],
            ..Default::default()
        };
        return verdict;
    }
    let (rate, cutoff, silent_above, upsampled, origin, from_44k, lossywav) = (f.rate, f.cutoff, f.silent_above, f.upsampled, f.origin, f.from_44k, f.lossywav);
    let (mp3_trace, aac_trace, opus_trace, masked, roll) = (f.mp3_trace, f.aac_trace, f.opus_trace, f.masked, f.roll);
    let (padded, speed, shifted_pilot) = (f.padded.clone(), f.speed.clone(), f.shifted_pilot);
    let aac_found = aac_trace.filter(crate::aactrace::found);
    let opus_found = opus_trace.filter(|t| t.found());
    let fm_pilot = f.pilot_on_19k || shifted_pilot.is_some();
    let traces: Option<(f64, Option<f64>, Option<f64>)> = f.holes.map(|h| (h, f.above_floor, f.high_floor));
    let holes = traces.map(|t| t.0).unwrap_or(0.0);
    // Holes this deep are proof of an encoder on their own; shallower ones (10-15 dB) only
    // with a second clue: digital silence above the cutoff, which no analog stage leaves.
    let encoder_holes = holes >= 15.0;
    // Encoder holes, yet steady hiss above the cutoff: the hiss was added after the encoder,
    // so this is a capture (FM, analog line) of a program already coded upstream (a radio's
    // digital link or internet stream), not a lossy file converted back to lossless.
    let hiss_above = traces.is_some_and(|(_, a, _)| a.is_some_and(|a| a >= 10.0));
    let upstream = |c: f64, details: Vec<String>| {
        Verdict::new(
            format!("{LOSSY_UPSTREAM} {:.1} kHz", c / 1000.0),
            if encoder_holes {
                format!(
                    "Lossy source upstream (e.g. MiniDisc, broadcast MPEG, MP3): encoder holes below the {:.1} kHz lowpass, with steady noise above it that came later (an analog stage such as a tuner or a sound card, or dither added when converting): the content itself was lossy-coded",
                    c / 1000.0
                )
            } else {
                format!(
                    "Probably a lossy source upstream (e.g. MiniDisc, broadcast MPEG, MP3): encoder holes of {holes:.0} dB below the {:.1} kHz lowpass, with steady noise above it that came later (an analog stage, or dither added when converting); holes this shallow are not proof on their own",
                    c / 1000.0
                )
            },
            if encoder_holes { Level::Error } else { Level::Warn },
            details,
        )
    };
    // Hiss above the cutoff and no holes: not an encoder's lowpass.
    let analog_above = traces.is_some_and(|(holes, a, _)| holes < 8.0 && a.is_some_and(|a| a >= 10.0));
    let fm_like = traces.is_some_and(|(holes, a, h)| holes <= 8.0 && a.is_some_and(|a| a >= 10.0) && h.is_some_and(|h| h >= 10.0));
    let khz = |c: f64| format!("{:.1}", c / 1000.0);
    let mut verdict = if fm_pilot {
        let sentence = match &speed {
            Some(s) => format!("FM broadcast recording (19 kHz pilot tone found, shifted: {s}): lossless capture of an analog source"),
            None => "FM broadcast recording (19 kHz pilot tone found): lossless capture of an analog source".into(),
        };
        Verdict::new("FM broadcast", sentence, Level::Ok, details)
    } else if let Some(c) = cutoff.filter(|c| (14500.0..=16000.0).contains(c)) {
        // The FM band stops at 15 kHz (TV sound and some transmitters a little higher): the
        // reference could only say "FM or MP3" here.
        if fm_like {
            Verdict::new(
                FM_15K,
                format!(
                    "FM broadcast: the lowpass at {} kHz looks lossy at first sight, but it is the FM limit; hiss goes on above it and no encoder holes: lossless capture",
                    khz(c)
                ),
                Level::Ok,
                details,
            )
        } else if encoder_holes && hiss_above {
            upstream(c, details)
        } else if encoder_holes {
            Verdict::new(
                format!("Lossy {} kHz", khz(c)),
                format!(
                    "Lossy source likely: lowpass at {} kHz with encoder holes below it (bands switched off and on): a lossy encoder, even if the show was broadcast on FM",
                    khz(c)
                ),
                Level::Error,
                details,
            )
        } else if silent_above {
            Verdict::new(
                "15 kHz lowpass",
                format!(
                    "Steep lowpass at {} kHz with digital silence above it: a lossy encoder or a digital filter, not a plain FM capture",
                    khz(c)
                ),
                Level::Warn,
                details,
            )
        } else if hiss_above && traces.is_some_and(|(_, _, h)| h.is_some_and(|h| h >= 10.0)) {
            // Hiss up to 19 kHz: no decoder came last. Moderate holes may come from the
            // station's own digital link.
            Verdict::new(
                "15 kHz lowpass",
                format!(
                    "FM broadcast likely: lowpass at {} kHz with hiss above it up to 19 kHz, which no lossy file converted back has; but some encoder holes ({holes:.0} dB) hint that the station's digital link was lossy",
                    khz(c)
                ),
                Level::Warn,
                details,
            )
        } else {
            Verdict::new(
                "15 kHz lowpass",
                format!("Steep lowpass at {} kHz: typical of an FM broadcast, but also of a low-bitrate MP3", khz(c)),
                Level::Warn,
                details,
            )
        }
    } else if let Some(c) = cutoff.filter(|c| (19500.0..21500.0).contains(c) && rate <= 48000.0 && analog_above && holes < 5.0) {
        // A converter's anti-alias filter: steady hiss goes on above it, no holes.
        Verdict::new(
            "Lossless",
            format!("No sign of lossy encoding: the lowpass at {} kHz is the converter's anti-alias filter (hiss goes on above it, no encoder holes)", khz(c)),
            Level::Ok,
            details,
        )
    } else if let Some(c) = cutoff.filter(|c| *c < 20500.0) {
        // Encoder holes are the encoder's fingerprint. A clean brick wall alone may also be
        // a digital lowpass filter (some remasters): then only a doubt, in orange.
        // Hiss above the cutoff cannot come out of a decoder: with encoder holes, the
        // coding was upstream of an analog stage (a stream captured through a sound card).
        if holes >= 8.0 && hiss_above && traces.is_some_and(|(_, _, h)| h.is_none_or(|h| h >= 10.0)) {
            upstream(c, details)
        } else if holes >= 10.0 && !encoder_holes && !silent_above {
            Verdict::new(
                format!("Lossy? {} kHz", khz(c)),
                format!(
                    "Probably lossy: brick-wall lowpass at {} kHz with encoder holes of {holes:.0} dB below it, but holes this shallow are not proof on their own, and noise goes on above the cutoff (no digital silence to confirm a decoder)",
                    khz(c)
                ),
                Level::Warn,
                details,
            )
        } else if holes >= 10.0 {
            Verdict::new(
                format!("Lossy {} kHz", khz(c)),
                format!(
                    "Lossy source (high confidence): brick-wall lowpass at {} kHz with encoder holes below it, as a lossy encoder leaves",
                    khz(c)
                ),
                Level::Error,
                details,
            )
        } else {
            let above = if silent_above { "nothing above it" } else { "hiss still above it" };
            Verdict::new(
                format!("Lossy? {} kHz", khz(c)),
                format!(
                    "Possibly lossy: brick-wall lowpass at {} kHz with {above}, as a lossy encoder leaves; but no encoder holes, and a digital lowpass filter (some remasters) looks the same",
                    khz(c)
                ),
                Level::Warn,
                details,
            )
        }
    } else if let Some(c) = cutoff.filter(|c| *c < 21500.0 && rate <= 48000.0 && analog_above) {
        Verdict::new(
            "Lossless",
            format!("No sign of lossy encoding: the lowpass at {} kHz is the converter's anti-alias filter (hiss goes on above it, no encoder holes)", khz(c)),
            Level::Ok,
            details,
        )
    } else if let Some(c) = cutoff.filter(|c| *c < 21500.0 && rate <= 48000.0) {
        Verdict::new(
            format!("Unsure {} kHz", khz(c)),
            format!(
                "Possible lossy source: lowpass at {} kHz, where high-bitrate lossy encoders also cut, but so do some recorders' anti-alias filters",
                khz(c)
            ),
            Level::Warn,
            details,
        )
    } else if upsampled {
        // Only reached without a lossy lowpass. Unlike the reference, say so, and keep
        // the 20.5-21.5 kHz doubt (high-bitrate lossy) that hi-res rates otherwise skip.
        let origin = upsampled_from(rate, cutoff, origin);
        match cutoff.filter(|c| *c < 21500.0) {
            Some(c) => Verdict::new(
                "Unsure (upsampled)",
                format!(
                    "Upsampled, and possibly lossy: lowpass at {} kHz, where high-bitrate lossy encoders also cut, but so do some recorders' anti-alias filters; {origin}",
                    khz(c)
                ),
                Level::Warn,
                details,
            ),
            None => Verdict::new(
                LOSSLESS_UPSAMPLED,
                format!("Lossless, but upsampled: no sign of lossy encoding; {origin}"),
                Level::Warn,
                details,
            ),
        }
    } else if let Some(p) = &padded {
        Verdict::new(
            LOSSLESS_PADDED,
            format!("Lossless, but not true 24-bit: no sign of lossy encoding; {p}"),
            Level::Warn,
            details,
        )
    } else if matches!(f.hf, Some(d) if d < if mp3_trace.is_some() { TREBLE_UNCLEAR } else { TREBLE_PLAIN }) {
        Verdict::new(
            "Unclear",
            "Inconclusive: almost no content above 15 kHz (common with audience and analog-tape recordings), so encoder traces cannot be seen",
            Level::Info,
            details,
        )
    } else if mp3_trace.is_some() && matches!(f.hf, Some(d) if d < TREBLE_LIKELY) {
        Verdict::new(
            LIKELY_LOSSLESS,
            "Probably lossless: no encoder lowpass and no MP3 coding traces, but little treble above 15 kHz, where an encoder's lowpass shows best",
            Level::Ok,
            details,
        )
    } else {
        Verdict::new(
            "Lossless",
            "No sign of lossy encoding or upsampling: no encoder lowpass, high frequencies present",
            Level::Ok,
            details,
        )
    };
    // A doubtful lowpass that is a gradual roll-off with the music still above it, and no
    // encoder holes: often tape, microphones or an analog filter, but lossy files can roll off
    // like this too, so the verdict stays neutral. Still looked at for AAC and Opus traces
    // (`doubt`), which would overrule it.
    let analog = cutoff.is_some_and(|_| doubtful(&verdict.label) && !verdict.label.contains("upsampled"))
        && holes < 8.0
        && roll.is_some_and(|r| r.0 <= ANALOG_STEP && r.1 >= ANALOG_COHERENCE);
    if let (true, Some(c), Some(r)) = (analog, cutoff, roll) {
        let mut unclear = Verdict::new(
            "Unclear",
            format!(
                "Gradual roll-off, no coding trace: the treble fades out around {} kHz (at most {:.0} dB between neighbouring bands) with the music still above it, as tape, microphones and analog filters leave, but some lossy files look the same",
                khz(c),
                r.0
            ),
            Level::Info,
            verdict.details.clone(),
        );
        unclear.report = std::mem::take(&mut verdict.report);
        verdict = unclear;
    }
    if from_44k && (verdict.label.starts_with("Unsure") || verdict.label == "Lossless" || verdict.label == LIKELY_LOSSLESS) {
        verdict.sentence.push_str(&format!(
            "; the content stops at {} kHz with digital silence above, as a 44.1 kHz source upsampled to 48 kHz leaves",
            khz(cutoff.unwrap_or(0.0))
        ));
    }
    // A lowpass hidden under noise-shaped dither: no FM capture (no pilot tone) and no
    // plain lossless file leaves the music stopping dead under such a noise.
    let hidden = masked.filter(|m| m.dithered() && !fm_pilot && matches!(verdict.label.as_str(), "Lossless" | LIKELY_LOSSLESS | FM_15K));
    if let Some(m) = hidden {
        let c = m.cutoff;
        let (label, doubt) = if c < 20500.0 {
            (format!("Lossy? {} kHz", khz(c)), "as a lossy encoder leaves".to_string())
        } else {
            (format!("Unsure {} kHz", khz(c)), "where high-bitrate lossy encoders also cut, but so do some converters' filters".to_string())
        };
        let sentence = format!(
            "Possibly lossy: the music stops at {} kHz, {doubt}; the band above holds only steady noise rising toward 19 kHz, the noise-shaped dither of a conversion to 16-bit, which hides the lowpass",
            khz(c)
        );
        let mut doubtful = Verdict::new(label, sentence, Level::Warn, verdict.details.clone());
        doubtful.report = std::mem::take(&mut verdict.report);
        verdict = doubtful;
    }
    if let Some(share) = f.intensity.filter(|_| verdict.level != Level::Error) {
        verdict.details.push(format!("Treble locked between the channels (intensity stereo) in {:.0} % of the loud frames", share * 100.0));
        let mut doubtful = Verdict::new(
            "Lossy? (Opus)",
            format!(
                "Possibly a decoded Opus stream (YouTube, streaming): in {:.0} % of the loud frames the treble above 14 kHz is one signal only panned between the channels, as Opus codes it at moderate bitrates, while the lower frequencies are real stereo",
                share * 100.0
            ),
            Level::Warn,
            verdict.details.clone(),
        );
        doubtful.report = std::mem::take(&mut verdict.report);
        verdict = doubtful;
    }
    if (verdict.label.starts_with("Unsure") || verdict.label.starts_with("Lossy?")) && mp3_trace.is_some_and(|t| !t.found()) && aac_trace.is_some() && aac_found.is_none() {
        if opus_trace.is_some_and(|t| !t.found()) {
            verdict.sentence.push_str("; no MP3, AAC or Opus coding traces, so not an MP3, AAC or Opus converted straight back");
        } else {
            verdict.sentence.push_str("; no MP3 or AAC coding traces, so not an MP3 or AAC converted straight back");
        }
    }
    if let Some(t) = opus_found {
        let sentence = format!(
            "Decoded from an Opus stream (YouTube, streaming): {} of the {} frames analysed are exactly Opus codewords, which only a lossy file converted back leaves",
            t.clean, t.frames
        );
        let label = if verdict.label.starts_with("Lossy ") && !verdict.label.starts_with(LOSSY_UPSTREAM) { verdict.label.clone() } else { "Lossy (Opus traces)".into() };
        let mut lossy = Verdict::new(label, sentence, Level::Error, verdict.details.clone());
        lossy.details.push(format!("Opus coding traces found ({} of {} frames)", t.clean, t.frames));
        lossy.report = std::mem::take(&mut verdict.report);
        verdict = lossy;
    }
    if let Some(t) = aac_found {
        let sentence = format!(
            "Decoded from an AAC: the file still holds the rounding pattern of an AAC encoder ({:.1} % of the bands match it, {:.0} standard deviations above chance), which only a lossy file converted back leaves",
            t.likelihood * 100.0,
            t.contrast
        );
        let label = if verdict.label.starts_with("Lossy ") && !verdict.label.starts_with(LOSSY_UPSTREAM) { verdict.label.clone() } else { "Lossy (AAC traces)".into() };
        let mut lossy = Verdict::new(label, sentence, Level::Error, verdict.details.clone());
        lossy.details.push(format!("AAC coding traces found ({:.1} % of the bands, {:.0} standard deviations above chance)", t.likelihood * 100.0, t.contrast));
        lossy.report = std::mem::take(&mut verdict.report);
        verdict = lossy;
    }
    if let Some(t) = mp3_trace.filter(|t| t.found()) {
        let sentence = format!(
            "Decoded from an MP3: the file still holds the rounding pattern of an MP3 encoder ({:.1} % of the bands match it, {:.0} standard deviations above chance), which only a lossy file converted back leaves",
            t.likelihood * 100.0,
            t.contrast
        );
        let label = if verdict.label.starts_with("Lossy ") && !verdict.label.starts_with(LOSSY_UPSTREAM) { verdict.label.clone() } else { "Lossy (MP3 traces)".into() };
        let mut lossy = Verdict::new(label, sentence, Level::Error, verdict.details.clone());
        lossy.report = std::mem::take(&mut verdict.report);
        verdict = lossy;
    }
    if lossywav {
        let w = f.waste.map(|w| (w.0 * 100.0, w.1)).unwrap_or_default();
        let mut lossy = Verdict::new(
            "Lossy (lossyWAV)",
            format!(
                "Processed by lossyWAV: in {:.0} % of the blocks the lowest bits ({:.1} on average) are zero, block by block, the mark lossyWAV leaves when it discards detail to make smaller FLAC files",
                w.0, w.1
            ),
            Level::Error,
            verdict.details.clone(),
        );
        lossy.report = std::mem::take(&mut verdict.report);
        verdict = lossy;
    }
    if padded.is_some() && verdict.label != LOSSLESS_PADDED {
        verdict.sentence.push_str("; also 16-bit audio in a 24-bit file");
        if verdict.level == Level::Ok {
            verdict.level = Level::Warn;
        }
    }
    let facts = Facts { analog, ..f.clone() };
    verdict.report = build_report(crate::files::file_name(path), &verdict, &facts);
    if std::env::var_os("BT_FACTS").is_some() {
        verdict.facts = serde_json::to_string(&facts).ok();
    }
    // An FM pilot tone survives an Opus or AAC stream at a high bitrate: with the music
    // stopping dead around 20 kHz (FM itself stops at 15 kHz), the origin is open too.
    let stream_lowpass = cutoff.is_some_and(|c| (19500.0..20800.0).contains(&c));
    verdict.doubt = doubtful(&verdict.label) || (fm_pilot && stream_lowpass) || analog || (verdict.label.starts_with(LOSSY_UPSTREAM) && verdict.level == Level::Warn);
    verdict
}

/// One analysed file of a show.
pub(crate) struct TrackResult<'a> {
    /// Its path in the show, with "/".
    pub name: String,
    pub label: &'a str,
    pub report: &'a Report,
}

/// The verdict of a show.
pub(crate) struct ShowDecision {
    /// What the Source column shows.
    pub label: String,
    /// The counts of the labels, "12 Lossless, 2 Unclear".
    pub text: String,
    pub level: Level,
    pub report: Report,
}

/// The verdict of a show from those of its files (None without any).
pub(crate) fn aggregate(title: String, done: &[TrackResult]) -> Option<ShowDecision> {
    if done.is_empty() {
        return None;
    }
    // Counts in order of first appearance, then sorted by count (as the reference).
    let mut counts: Vec<(String, usize)> = Vec::new();
    for t in done {
        // "Lossy upstream 16.1 kHz" counts as "Lossy upstream": the frequency is dropped.
        let key = match t.label.strip_suffix(" kHz").and_then(|l| l.rsplit_once(' ')) {
            Some((head, _)) => head.to_string(),
            None => t.label.to_string(),
        };
        match counts.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 += 1,
            None => counts.push((key, 1)),
        }
    }
    counts.sort_by_key(|a| std::cmp::Reverse(a.1));
    let text = counts.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(", ");
    let flagged: usize =
        counts.iter().filter(|(k, _)| !["Lossless", LIKELY_LOSSLESS, "FM broadcast", FM_15K, "Unclear", "Silent"].contains(&k.as_str())).map(|(_, n)| n).sum();
    // "Lossy" (encoder holes found) and the lossy formats or traces ("Lossy (MP3)",
    // "Lossy (MP3 traces)") in red; "Lossy?" alone is only a doubt.
    let level = if counts.iter().any(|(k, _)| k == "Lossy" || k.starts_with("Lossy (") || k == LOSSY_UPSTREAM) {
        Level::Error
    } else if flagged > 0 {
        Level::Warn
    } else {
        Level::Ok
    };
    let all_lossless = counts.len() == 1 && counts[0].0 == "Lossless";
    let shown = if all_lossless { "All lossless".to_string() } else { text.clone() };
    // Show report: every track, then the reasoning of the most common verdict.
    let main = &counts[0].0;
    let example = done.iter().find(|t| t.label.starts_with(main.as_str())).map(|t| t.report);
    let mut reasoning = vec![format!("{} track(s) analysed: {text}.", done.len())];
    if let Some(r) = example {
        reasoning.push(format!("Most tracks read \"{main}\". Why, for {}:", r.title));
        reasoning.extend(r.reasoning.iter().map(|line| format!("    {line}")));
    }
    let report = Report {
        heading: "Lossless analysis".into(),
        title,
        label: shown.clone(),
        level,
        sentence: example.map(|r| r.sentence.clone()).unwrap_or_default(),
        measures: Vec::new(),
        tracks: done
            .iter()
            .map(|t| Measure { name: t.name.clone(), value: t.report.label.clone(), meaning: t.report.key_facts.clone(), level: t.report.level })
            .collect(),
        reasoning,
        key_facts: String::new(),
        ..Default::default()
    };
    Some(ShowDecision { label: shown, text, level, report })
}
