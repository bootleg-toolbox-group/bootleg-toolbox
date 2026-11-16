//! The Stop button during Lossless?: a file stopped has no verdict, never a partial one, and the
//! analysis notices the request within a fraction of a second (the searches check it in their loops).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bootleg_toolbox::{audio, lossless};

/// A 16-bit stereo WAV of `seconds` of noisy tones (a stand-in for music: the searches have
/// something to chew on), by a fixed generator.
fn write_wav(path: &std::path::Path, seconds: usize) {
    let rate = 44100usize;
    let frames = rate * seconds;
    let mut data = Vec::with_capacity(frames * 4);
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut low = 0.0f64;
    for i in 0..frames {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let noise = (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5;
        // Noise smoothed (a spectrum falling with frequency) under a few tones.
        low += 0.2 * (noise - low);
        let t = i as f64 / rate as f64;
        let tones = 0.2 * libm::sin(440.0 * std::f64::consts::TAU * t) + 0.1 * libm::sin(1760.0 * std::f64::consts::TAU * t);
        let envelope = 0.6 + 0.4 * libm::sin(t * 0.7);
        for k in 0..2 {
            let v = (envelope * (tones * (1.0 + 0.1 * k as f64) + 1.5 * low) * 20000.0).clamp(-32768.0, 32767.0) as i16;
            data.extend_from_slice(&v.to_le_bytes());
        }
    }
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&(rate as u32).to_le_bytes());
    out.extend_from_slice(&(rate as u32 * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    std::fs::write(path, out).unwrap();
}

#[test]
fn a_stopped_analysis_has_no_verdict_and_stops_fast() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("track.wav");
    write_wav(&path, 40);
    let info = audio::probe(&path).unwrap();
    // The searches for AAC, MP2 and Opus are made too (the slow ones).
    let run = |cancel: &AtomicBool| lossless::analyse(&path, &info, true, false, cancel);
    let whole = run(&AtomicBool::new(false)).unwrap();
    // A request made before: nothing is measured.
    let before = AtomicBool::new(true);
    assert_eq!(run(&before).err().as_deref(), Some("cancelled"));
    // A request at any moment: no verdict, or the very verdict of the whole analysis, and the
    // analysis ends soon after the request.
    let mut stopped = 0;
    let mut slowest = Duration::ZERO;
    for delay_ms in [1u64, 5, 20, 60, 150, 400, 900, 1500, 2500, 4000] {
        let cancel = AtomicBool::new(false);
        let asked = std::sync::Mutex::new(None);
        let result = std::thread::scope(|scope| {
            let (flag, asked_ref) = (&cancel, &asked);
            scope.spawn(move || {
                std::thread::sleep(Duration::from_millis(delay_ms));
                *asked_ref.lock().unwrap() = Some(Instant::now());
                flag.store(true, Ordering::SeqCst);
            });
            let result = run(&cancel);
            if let Some(t) = *asked.lock().unwrap() {
                if result.is_err() {
                    slowest = slowest.max(t.elapsed());
                }
            }
            result
        });
        match result {
            Err(e) => {
                assert_eq!(e, "cancelled", "after {delay_ms} ms");
                stopped += 1;
            }
            Ok(v) => assert_eq!((&v.label, &v.sentence, &v.details), (&whole.label, &whole.sentence, &whole.details), "after {delay_ms} ms"),
        }
    }
    eprintln!("{stopped} analyses stopped, the slowest ended {slowest:?} after the request");
    assert!(stopped >= 3, "only {stopped} analyses were stopped");
    assert!(slowest < Duration::from_secs(1), "an analysis took {slowest:?} to stop");
}
