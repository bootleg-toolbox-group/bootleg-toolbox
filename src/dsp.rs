//! Signal processing shared by the spectrogram and the lossless check:
//! FFT, Hann window, conversion of PCM bytes to samples, statistics.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::audio::{self, AudioInfo, Format};

/// The processor threads the current operation may keep busy ("Files processed in parallel"),
/// and how many it does: the files being processed, and their decoding threads. A file is
/// decoded in a thread of its own only when one is free (single files, the end of a batch).
pub static THREADS: AtomicUsize = AtomicUsize::new(0);
pub static BUSY: AtomicUsize = AtomicUsize::new(0);

/// One thread counted in `BUSY` while it lives.
pub struct Busy(());

impl Busy {
    /// Always counted (a file being processed).
    pub fn take() -> Busy {
        BUSY.fetch_add(1, Ordering::SeqCst);
        Busy(())
    }

    /// Counted only if the budget has room for one more thread.
    fn spare() -> Option<Busy> {
        let busy = Busy::take();
        (BUSY.load(Ordering::SeqCst) <= THREADS.load(Ordering::SeqCst)).then_some(busy)
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Lowers the calling thread's priority: the analysis then uses the processor time other
/// programs leave, and the computer stays responsive.
#[cfg(windows)]
pub fn lower_priority() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThread() -> isize;
        fn SetThreadPriority(thread: isize, priority: i32) -> i32;
    }
    const THREAD_PRIORITY_BELOW_NORMAL: i32 = -1;
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

/// Lowers the calling thread's priority (nice 10: on Linux, for this thread only).
#[cfg(target_os = "linux")]
pub fn lower_priority() {
    extern "C" {
        fn gettid() -> i32;
        fn setpriority(which: i32, who: u32, prio: i32) -> i32;
    }
    const PRIO_PROCESS: i32 = 0;
    unsafe {
        setpriority(PRIO_PROCESS, gettid() as u32, 10);
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn lower_priority() {}

/// In-place radix-2 complex FFT.
pub struct Fft {
    rev: Vec<usize>,
    /// Per stage (butterflies of 2, 4, 8... points), the twiddles of its butterflies in a
    /// row: exp(-2 pi i k / len), cos and sin apart.
    stages: Vec<(Vec<f64>, Vec<f64>)>,
}

impl Fft {
    pub fn new(n: usize) -> Self {
        assert!(n.is_power_of_two());
        let bits = n.trailing_zeros();
        let rev = (0..n).map(|i| i.reverse_bits() >> (usize::BITS - bits)).collect();
        let twiddles: Vec<(f64, f64)> = (0..n / 2)
            .map(|k| {
                let angle = -2.0 * std::f64::consts::PI * k as f64 / n as f64;
                (angle.cos(), angle.sin())
            })
            .collect();
        let stages = (1..=bits)
            .map(|s| {
                let (len, step) = (1usize << s, n >> s);
                ((0..len / 2).map(|k| twiddles[k * step].0).collect(), (0..len / 2).map(|k| twiddles[k * step].1).collect())
            })
            .collect();
        Fft { rev, stages }
    }

    // A transform always has a size: no is_empty.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.rev.len()
    }

    pub fn run(&self, re: &mut [f64], im: &mut [f64]) {
        #[cfg(all(target_arch = "x86_64", not(bt_no_avx2)))]
        if std::is_x86_feature_detected!("avx2") {
            #[target_feature(enable = "avx2")]
            fn avx2(fft: &Fft, re: &mut [f64], im: &mut [f64]) {
                fft.run_plain(re, im)
            }
            // SAFETY: the processor has AVX2 (checked just above).
            return unsafe { avx2(self, re, im) };
        }
        self.run_plain(re, im)
    }

    /// The butterflies of a stage are independent: computed side by side, each with the same
    /// operations as one at a time.
    #[inline(always)]
    fn run_plain(&self, re: &mut [f64], im: &mut [f64]) {
        let n = self.rev.len();
        for i in 0..n {
            let j = self.rev[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        for (s, (wr, wi)) in self.stages.iter().enumerate() {
            let half = 1usize << s;
            for (re, im) in re[..n].chunks_exact_mut(2 * half).zip(im[..n].chunks_exact_mut(2 * half)) {
                let (re_a, re_b) = re.split_at_mut(half);
                let (im_a, im_b) = im.split_at_mut(half);
                for k in 0..half {
                    let tr = re_b[k] * wr[k] - im_b[k] * wi[k];
                    let ti = re_b[k] * wi[k] + im_b[k] * wr[k];
                    re_b[k] = re_a[k] - tr;
                    im_b[k] = im_a[k] - ti;
                    re_a[k] += tr;
                    im_a[k] += ti;
                }
            }
        }
    }

    /// Power of each bin 0..=n/2 of a real signal (already windowed).
    pub fn power(&self, signal: &[f64], re: &mut Vec<f64>, im: &mut Vec<f64>, out: &mut [f64]) {
        let n = self.len();
        re.clear();
        re.extend_from_slice(&signal[..n]);
        im.clear();
        im.resize(n, 0.0);
        self.run(re, im);
        for (k, p) in out.iter_mut().enumerate().take(n / 2 + 1) {
            *p = re[k] * re[k] + im[k] * im[k];
        }
    }
}

/// Periodic Hann window (spectrogram).
pub fn hann(n: usize) -> Vec<f64> {
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()).collect()
}

/// Symmetric Hann window, as numpy.hanning (lossless check).
pub fn hann_symmetric(n: usize) -> Vec<f64> {
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos()).collect()
}

/// Converts the little-endian bytes of one sample to -1.0..1.0.
pub fn sample_reader(float: bool, width: usize, full_scale: u32) -> impl Fn(&[u8]) -> f64 {
    let scale = 1.0 / (1u64 << (full_scale.clamp(1, 32) - 1)) as f64;
    move |b: &[u8]| {
        if float {
            return match width {
                4 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
                8 => f64::from_le_bytes(b.try_into().unwrap_or([0; 8])),
                _ => 0.0,
            };
        }
        if width == 1 {
            return (b[0] as f64 - 128.0) / 128.0; // 8-bit WAV is unsigned
        }
        int_sample(b) as f64 * scale
    }
}

/// Signed integer value of a little-endian sample of 2 to 4 bytes.
pub fn int_sample(b: &[u8]) -> i32 {
    let width = b.len().min(4);
    let mut v = 0i32;
    for (i, byte) in b.iter().take(4).enumerate() {
        v |= (*byte as i32) << (8 * i);
    }
    let shift = 32 - 8 * width as u32;
    (v << shift) >> shift
}

/// Decodes `path` and calls `frame` with the samples of each sample frame
/// (one value per channel, -1.0..1.0) and its raw bytes per channel.
pub fn for_each_frame(
    path: &Path,
    info: &AudioInfo,
    cancel: &AtomicBool,
    mut frame: impl FnMut(&[f64], &[u8]),
) -> Result<(), String> {
    let channels = info.channels.max(1) as usize;
    let width = info.bits.div_ceil(8) as usize;
    let frame_bytes = channels * width;
    // FLAC samples come out right-aligned on `bits`, WAV samples fill their container.
    let full_scale = if info.format == Format::Flac { info.bits } else { 8 * width as u32 };
    let to_f64 = sample_reader(info.float, width, full_scale);
    let mut pending: Vec<u8> = Vec::new();
    let mut values = vec![0f64; channels];
    let mut hand_out = |chunk: &[u8]| {
        pending.extend_from_slice(chunk);
        let whole = pending.len() / frame_bytes * frame_bytes;
        for bytes in pending[..whole].chunks_exact(frame_bytes) {
            for (c, sample) in bytes.chunks_exact(width).enumerate() {
                values[c] = to_f64(sample);
            }
            frame(&values, bytes);
        }
        pending.drain(..whole);
    };
    let Some(_thread) = Busy::spare() else {
        audio::read_pcm(path, cancel, hand_out)?;
        return Ok(());
    };
    // A thread is free: the file is decoded in it while the frames are handed out here.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
    std::thread::scope(|scope| {
        let decoder = scope.spawn(move || {
            lower_priority();
            audio::read_pcm(path, cancel, |chunk| {
                // The receiver only stops early on a panic, which is passed on below.
                let _ = tx.send(chunk.to_vec());
            })
        });
        for chunk in rx {
            hand_out(&chunk);
        }
        decoder.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })?;
    Ok(())
}

/// Median, as numpy.median (mean of the two middle values).
pub fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}

/// Percentile with linear interpolation, as numpy.percentile.
pub fn percentile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let pos = p / 100.0 * (v.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    Some(v[lo] + (v[hi] - v[lo]) * (pos - lo as f64))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Pseudo-random values in -1..1 (xorshift), the same on every run.
    pub(crate) fn noise(n: usize, seed: u64) -> Vec<f64> {
        let mut s = seed.max(1);
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 11) as f64 / (1u64 << 52) as f64 - 1.0
            })
            .collect()
    }

    /// Two results equal to the bit (the AVX2 and plain versions of a kernel).
    pub(crate) fn same_bits(a: &[f64], b: &[f64]) {
        assert_eq!(a.len(), b.len());
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            assert_eq!(x.to_bits(), y.to_bits(), "value {i}: {x} vs {y}");
        }
    }

    #[test]
    fn fft_plain_and_avx2_agree() {
        for bits in 1..=13 {
            let n = 1 << bits;
            let fft = Fft::new(n);
            let (mut re, mut im) = (noise(n, bits), noise(n, bits + 100));
            let (mut re2, mut im2) = (re.clone(), im.clone());
            fft.run(&mut re, &mut im);
            fft.run_plain(&mut re2, &mut im2);
            same_bits(&re, &re2);
            same_bits(&im, &im2);
        }
    }

    #[test]
    fn fft_matches_naive_dft() {
        let n = 64;
        let input: Vec<f64> = (0..n).map(|i| ((i * 7919) % 97) as f64 / 97.0 - 0.5).collect();
        let (mut re, mut im) = (input.clone(), vec![0.0; n]);
        Fft::new(n).run(&mut re, &mut im);
        for k in 0..n {
            let (mut sr, mut si) = (0.0f64, 0.0f64);
            for (t, x) in input.iter().enumerate() {
                let a = -2.0 * std::f64::consts::PI * (k * t) as f64 / n as f64;
                sr += x * a.cos();
                si += x * a.sin();
            }
            assert!((re[k] - sr).abs() < 1e-9 && (im[k] - si).abs() < 1e-9, "bin {k}");
        }
    }

    #[test]
    fn sample_conversion() {
        let s16 = sample_reader(false, 2, 16);
        assert_eq!(s16(&[0x00, 0x80]), -1.0);
        assert_eq!(s16(&[0x00, 0x40]), 0.5);
        let s24 = sample_reader(false, 3, 24);
        assert_eq!(s24(&[0x00, 0x00, 0xC0]), -0.5);
        let f32le = sample_reader(true, 4, 32);
        assert_eq!(f32le(&0.25f32.to_le_bytes()), 0.25);
    }

    #[test]
    fn numpy_statistics() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0, 5.0], 90.0), Some(4.6));
        assert_eq!(percentile(&[10.0], 90.0), Some(10.0));
    }
}
