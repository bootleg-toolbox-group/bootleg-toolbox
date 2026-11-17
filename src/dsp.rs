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

/// Lowers the calling thread's priority to the background: the analysis then uses the processor
/// and disk time other programs leave, and the computer stays responsive (Windows: background mode,
/// which lowers the processor, disk and memory priorities of the thread).
#[cfg(windows)]
pub fn lower_priority() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThread() -> isize;
        fn SetThreadPriority(thread: isize, priority: i32) -> i32;
    }
    const THREAD_PRIORITY_BELOW_NORMAL: i32 = -1;
    const THREAD_MODE_BACKGROUND_BEGIN: i32 = 0x0001_0000;
    // SAFETY: plain calls on the current thread's pseudo-handle. Below normal first: where the
    // background mode is refused (Wine), the thread still runs below the others.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
    }
}

/// Lowers the calling thread's priority to the background, for this thread only (a thread it
/// starts inherits it): nice 10, the idle scheduling policy (the thread runs only when a core
/// would otherwise be idle) and the idle I/O class. Each step is left alone where the system
/// refuses it (a container).
#[cfg(target_os = "linux")]
pub fn lower_priority() {
    #[repr(C)]
    struct SchedParam {
        priority: i32,
    }
    extern "C" {
        fn gettid() -> i32;
        fn setpriority(which: i32, who: u32, prio: i32) -> i32;
        fn sched_setscheduler(pid: i32, policy: i32, param: *const SchedParam) -> i32;
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        fn syscall(number: i64, ...) -> i64;
    }
    const PRIO_PROCESS: i32 = 0;
    const SCHED_IDLE: i32 = 5;
    // ioprio_set(IOPRIO_WHO_PROCESS, tid, IOPRIO_CLASS_IDLE << 13).
    #[cfg(target_arch = "x86_64")]
    const SYS_IOPRIO_SET: i64 = 251;
    #[cfg(target_arch = "aarch64")]
    const SYS_IOPRIO_SET: i64 = 30;
    // SAFETY: calls on the calling thread by its own id; the parameter outlives the call.
    unsafe {
        let tid = gettid();
        setpriority(PRIO_PROCESS, tid as u32, 10);
        sched_setscheduler(tid, SCHED_IDLE, &SchedParam { priority: 0 });
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        syscall(SYS_IOPRIO_SET, 1i64, tid as i64, 3i64 << 13);
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn lower_priority() {}

/// The memory the system can give to programs without swapping, in bytes (None where unknown).
pub fn available_memory() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = text.lines().find(|l| l.starts_with("MemAvailable:"))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kib * 1024)
    }
    #[cfg(windows)]
    {
        #[repr(C)]
        struct MemoryStatus {
            length: u32,
            load: u32,
            total_phys: u64,
            avail_phys: u64,
            total_page: u64,
            avail_page: u64,
            total_virtual: u64,
            avail_virtual: u64,
            avail_extended: u64,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32;
        }
        let mut status = MemoryStatus { length: std::mem::size_of::<MemoryStatus>() as u32, load: 0, total_phys: 0, avail_phys: 0, total_page: 0, avail_page: 0, total_virtual: 0, avail_virtual: 0, avail_extended: 0 };
        // SAFETY: the structure is the one the function fills, its length set.
        (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.avail_phys)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        None
    }
}

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
                (libm::cos(angle), libm::sin(angle))
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
        for (i, &j) in self.rev.iter().enumerate() {
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
    (0..n).map(|i| 0.5 - 0.5 * libm::cos(2.0 * std::f64::consts::PI * i as f64 / n as f64)).collect()
}

/// Symmetric Hann window, as numpy.hanning (lossless check).
pub fn hann_symmetric(n: usize) -> Vec<f64> {
    (0..n).map(|i| 0.5 - 0.5 * libm::cos(2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64)).collect()
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

/// Points of a file's outline (`take_wave`).
pub const WAVE_POINTS: usize = 120;

fn waves() -> &'static std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, Vec<f32>>> {
    static WAVES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, Vec<f32>>>> = std::sync::OnceLock::new();
    WAVES.get_or_init(Default::default)
}

/// The outline of a file decoded whole by `for_each_frame` (an analysis that read it): the
/// loudness (RMS, 0..1) of each of `WAVE_POINTS` stretches, for the list's mini waveforms.
/// Once.
pub fn take_wave(path: &Path) -> Option<Vec<f32>> {
    waves().lock().ok()?.remove(path)
}

/// Decodes `path` and calls `frame` with the samples of each sample frame
/// (one value per channel, -1.0..1.0) and its raw bytes per channel. Its outline is kept
/// on the way (`take_wave`), which changes nothing to what `frame` gets.
pub fn for_each_frame(
    path: &Path,
    info: &AudioInfo,
    cancel: &AtomicBool,
    mut frame: impl FnMut(&[f64], &[u8]),
) -> Result<(), String> {
    let total = info.samples;
    // The sum of the squares of each stretch and its number of values.
    let mut power = vec![(0f64, 0u64); WAVE_POINTS];
    let (mut count, mut point, mut next) = (0u64, 0usize, total / WAVE_POINTS as u64);
    let result = decode_frames(path, info, cancel, |values, bytes| {
        if total > 0 {
            if count == next && point + 1 < WAVE_POINTS {
                point += 1;
                next = (point as u64 + 1) * total / WAVE_POINTS as u64;
            }
            count += 1;
            let stretch = &mut power[point];
            stretch.0 += values.iter().map(|v| v * v).sum::<f64>();
            stretch.1 += values.len() as u64;
        }
        frame(values, bytes);
    });
    // Kept only when read to the end (not a part, not stopped).
    if result.is_ok() && total > 0 && count >= total - total / 100 {
        let levels = power.iter().map(|(sum, n)| libm::sqrt(sum / (*n).max(1) as f64) as f32).collect();
        if let Ok(mut waves) = waves().lock() {
            // Outlines nobody took (conversions): not kept for ever.
            if waves.len() >= 5000 {
                waves.clear();
            }
            waves.insert(path.to_path_buf(), levels);
        }
    }
    result
}

fn decode_frames(
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
    // BT_PROFILE: the time the decoder works, the time the frames take, the time each waits for the other.
    let profiled = crate::prof::enabled();
    let micros = |t: Option<std::time::Instant>| t.map_or(0, |t| t.elapsed().as_micros() as u64);
    let now = || profiled.then(std::time::Instant::now);
    let Some(_thread) = Busy::spare() else {
        crate::prof::decoded("inline");
        let (start, cpu0) = (now(), crate::prof::cpu_now());
        let mut frames_us = 0u64;
        audio::read_pcm(path, cancel, |chunk| {
            let t = now();
            hand_out(chunk);
            frames_us += micros(t);
        })?;
        if profiled {
            let (total, cpu) = (micros(start), crate::prof::cpu_now().saturating_sub(cpu0));
            crate::prof::add("decode", total.saturating_sub(frames_us), 0);
            // Inline, the thread's CPU time is that of the decoding and the frames together.
            crate::prof::add("frames", frames_us, cpu);
        }
        return Ok(());
    };
    crate::prof::decoded("thread");
    // A thread is free: the file is decoded in it while the frames are handed out here.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
    let cpu0 = crate::prof::cpu_now();
    let (mut waiting_us, mut frames_us) = (0u64, 0u64);
    let (result, (decode_wall, blocked_us, decode_cpu)) = std::thread::scope(|scope| {
        let decoder = scope.spawn(move || {
            lower_priority();
            let (start, cpu0) = (now(), crate::prof::cpu_now());
            let mut blocked_us = 0u64;
            let result = audio::read_pcm(path, cancel, |chunk| {
                let t = now();
                // The receiver only stops early on a panic, which is passed on below.
                let _ = tx.send(chunk.to_vec());
                blocked_us += micros(t);
            });
            (result, (micros(start), blocked_us, crate::prof::cpu_now().saturating_sub(cpu0)))
        });
        loop {
            let t = now();
            let Ok(chunk) = rx.recv() else { break };
            waiting_us += micros(t);
            let t = now();
            hand_out(&chunk);
            frames_us += micros(t);
        }
        decoder.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    });
    result?;
    if profiled {
        crate::prof::add("decode", decode_wall.saturating_sub(blocked_us), decode_cpu);
        crate::prof::add("decoder_blocked", blocked_us, 0);
        crate::prof::add("frames", frames_us, crate::prof::cpu_now().saturating_sub(cpu0));
        crate::prof::add("waiting_for_decoder", waiting_us, 0);
    }
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
    let (lo, hi) = (libm::floor(pos) as usize, libm::ceil(pos) as usize);
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

    /// The priority of the calling thread, as the system reports it.
    #[cfg(windows)]
    fn thread_priority() -> i32 {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetCurrentThread() -> isize;
            fn GetThreadPriority(thread: isize) -> i32;
        }
        // SAFETY: plain calls on the current thread's pseudo-handle.
        unsafe { GetThreadPriority(GetCurrentThread()) }
    }

    #[cfg(target_os = "linux")]
    fn thread_priority() -> i32 {
        extern "C" {
            fn gettid() -> i32;
            fn getpriority(which: i32, who: u32) -> i32;
        }
        // SAFETY: reads this thread's nice value.
        unsafe { getpriority(0, gettid() as u32) }
    }

    /// The scheduling policy of the calling thread (0 normal, 5 idle), and its I/O class (0 none, 3 idle).
    #[cfg(target_os = "linux")]
    fn thread_policy() -> (i32, i64) {
        extern "C" {
            fn gettid() -> i32;
            fn sched_getscheduler(pid: i32) -> i32;
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            fn syscall(number: i64, ...) -> i64;
        }
        #[cfg(target_arch = "x86_64")]
        const SYS_IOPRIO_GET: i64 = 252;
        #[cfg(target_arch = "aarch64")]
        const SYS_IOPRIO_GET: i64 = 31;
        // SAFETY: reads this thread's policy and I/O priority.
        unsafe {
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            let io = syscall(SYS_IOPRIO_GET, 1i64, gettid() as i64) >> 13;
            #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
            let io = 3;
            (sched_getscheduler(gettid()), io)
        }
    }

    #[test]
    fn priority_is_lowered_for_this_thread_only() {
        let own = thread_priority();
        let (before, after) = std::thread::spawn(|| {
            let before = thread_priority();
            lower_priority();
            (before, thread_priority())
        })
        .join()
        .unwrap();
        // Windows: below normal at most (the background mode may lower it further); Linux: nice 10, or
        // lower already (a process started by a build tool can run at nice 1).
        if cfg!(windows) {
            assert!(after <= -1, "priority before {before}, after {after}");
        } else {
            assert_eq!(after, before.max(10), "priority before {before}, after {after}");
        }
        // The thread that started it (the window's, in the application) keeps its own.
        assert_eq!(thread_priority(), own, "the calling thread's priority changed (lowered one: {before} -> {after})");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn background_threads_run_idle_for_the_processor_and_the_disk() {
        let own = thread_policy();
        let lowered = std::thread::spawn(|| {
            lower_priority();
            // A thread it starts inherits the policy (the decoder thread of a file).
            let child = std::thread::spawn(thread_policy).join().unwrap();
            (thread_policy(), child)
        })
        .join()
        .unwrap();
        assert_eq!(lowered.0 .0, 5, "scheduling policy {:?}", lowered.0);
        assert_eq!(lowered.1 .0, 5, "the thread it starts: {:?}", lowered.1);
        assert_eq!(lowered.0 .1, 3, "I/O class {:?}", lowered.0);
        assert_eq!(thread_policy(), own, "the calling thread changed");
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
                sr += x * libm::cos(a);
                si += x * libm::sin(a);
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
