//! Stage timers for Lossless? (BT_PROFILE=<file>): measurement only, nothing here changes a
//! result. Without the variable every call is one test of a flag.
//!
//! A file being analysed has a context in its worker thread (`file`). Spans opened in that thread
//! add up, by path of nested stages ("facts_doubt/detect_aac"), the number of calls, the wall time
//! and the CPU time of the thread (/proc/thread-self/schedstat, Linux). Work done by other threads
//! on the file's behalf (the decoder thread, rayon helpers) is not in the CPU time of the span that
//! waits for it: the decoder reports its own (`add`), and a run with BT_PROFILE_JOBS=1 has no helper
//! at all, so its CPU times are those of the stages. At the end of the operation `finish` writes one
//! JSON line per file (its stage totals, the decodes it made and why), the marks of the passes and
//! the process totals.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

fn target() -> &'static Option<std::path::PathBuf> {
    static TARGET: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    TARGET.get_or_init(|| std::env::var_os("BT_PROFILE").map(Into::into))
}

/// Whether BT_PROFILE is set.
#[inline]
pub fn enabled() -> bool {
    target().is_some()
}

/// BT_PROFILE_JOBS=<n>: the number of files read at a time, for a run that measures (only with BT_PROFILE).
pub fn jobs() -> Option<usize> {
    if !enabled() {
        return None;
    }
    std::env::var("BT_PROFILE_JOBS").ok()?.parse().ok()
}

fn start_time() -> &'static Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now)
}

fn lines() -> &'static Mutex<Vec<String>> {
    static LINES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    LINES.get_or_init(Default::default)
}

fn push(value: serde_json::Value) {
    if let Ok(mut l) = lines().lock() {
        l.push(value.to_string());
    }
}

fn micros_since_start() -> u64 {
    start_time().elapsed().as_micros() as u64
}

/// CPU time of the calling thread, in microseconds (0 where unknown).
fn thread_cpu_us() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/thread-self/schedstat").unwrap_or_default();
        text.split_whitespace().next().and_then(|n| n.parse::<u64>().ok()).map_or(0, |ns| ns / 1000)
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// CPU time of the calling thread so far, for a thread that reports it by hand (`add`).
pub fn cpu_now() -> u64 {
    if enabled() {
        thread_cpu_us()
    } else {
        0
    }
}

#[derive(Default)]
struct Acc {
    calls: u64,
    wall: u64,
    cpu: u64,
}

struct Context {
    file: String,
    phase: &'static str,
    t0: u64,
    stack: Vec<&'static str>,
    acc: BTreeMap<String, Acc>,
    decodes: Vec<&'static str>,
    info: serde_json::Value,
    /// Windows given to a grid detector (total) and distinct ones, per detector.
    windows: BTreeMap<&'static str, (u64, std::collections::HashSet<u64>)>,
}

thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

/// The analysis of `path` starts in this thread, in `phase` ("main": the file's first reading;
/// "extension": read again because another track of its show has an AAC grid).
pub struct FileGuard(bool);

pub fn file(path: &Path, phase: &'static str) -> FileGuard {
    if !enabled() {
        return FileGuard(false);
    }
    CONTEXT.with(|c| {
        *c.borrow_mut() = Some(Context {
            file: path.to_string_lossy().into_owned(),
            phase,
            t0: micros_since_start(),
            stack: Vec::new(),
            acc: BTreeMap::new(),
            decodes: Vec::new(),
            info: serde_json::Value::Null,
            windows: BTreeMap::new(),
        });
    });
    FileGuard(true)
}

/// What the file is (size, length, format), once read from its header.
pub fn describe(path: &Path, seconds: f64, rate: u32, bits: u32, channels: u32) {
    if !enabled() {
        return;
    }
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    CONTEXT.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.info = serde_json::json!({ "bytes": bytes, "seconds": seconds, "rate": rate, "bits": bits, "channels": channels });
        }
    });
}

/// One reading of the whole file by the decoder (`how`: in a thread of its own or in the caller's).
pub fn decoded(how: &'static str) {
    if !enabled() {
        return;
    }
    CONTEXT.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.decodes.push(how);
        }
    });
}

impl Drop for FileGuard {
    fn drop(&mut self) {
        if !self.0 {
            return;
        }
        let t1 = micros_since_start();
        let ctx = CONTEXT.with(|c| c.borrow_mut().take());
        let Some(ctx) = ctx else { return };
        let stages: serde_json::Map<String, serde_json::Value> = ctx
            .acc
            .iter()
            .map(|(k, a)| (k.clone(), serde_json::json!({ "n": a.calls, "wall": a.wall, "cpu": a.cpu })))
            .collect();
        push(serde_json::json!({
            "file": ctx.file,
            "phase": ctx.phase,
            "t0": ctx.t0,
            "t1": t1,
            "worker": rayon::current_thread_index(),
            "info": ctx.info,
            "decodes": ctx.decodes,
            "windows": ctx.windows.iter().map(|(k, (n, set))| (k.to_string(), serde_json::json!([n, set.len()]))).collect::<serde_json::Map<_, _>>(),
            "stages": stages,
        }));
    }
}

/// A stage of the file being analysed in this thread; its time is added up when it is dropped.
pub struct Span(Option<(Instant, Option<u64>)>);

/// A stage with its wall time and the thread's CPU time (two small reads of /proc).
pub fn span(stage: &'static str) -> Span {
    open(stage, true)
}

/// A stage called often (per slice, per probe): the wall time only.
pub fn span_hot(stage: &'static str) -> Span {
    open(stage, false)
}

fn open(stage: &'static str, cpu: bool) -> Span {
    if !enabled() {
        return Span(None);
    }
    CONTEXT.with(|c| match c.borrow_mut().as_mut() {
        Some(ctx) => {
            ctx.stack.push(stage);
            Span(Some((Instant::now(), cpu.then(thread_cpu_us))))
        }
        None => Span(None),
    })
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some((t0, cpu0)) = self.0.take() else { return };
        let wall = t0.elapsed().as_micros() as u64;
        let cpu = cpu0.map_or(0, |c0| thread_cpu_us().saturating_sub(c0));
        CONTEXT.with(|c| {
            if let Some(ctx) = c.borrow_mut().as_mut() {
                let a = ctx.acc.entry(ctx.stack.join("/")).or_default();
                (a.calls, a.wall, a.cpu) = (a.calls + 1, a.wall + wall, a.cpu + cpu);
                ctx.stack.pop();
            }
        });
    }
}

/// Time measured elsewhere (the decoder's own thread), added under `stage` of the current path.
pub fn add(stage: &'static str, wall_us: u64, cpu_us: u64) {
    if !enabled() {
        return;
    }
    CONTEXT.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            let path = if ctx.stack.is_empty() { stage.to_string() } else { format!("{}/{stage}", ctx.stack.join("/")) };
            let a = ctx.acc.entry(path).or_default();
            (a.calls, a.wall, a.cpu) = (a.calls + 1, a.wall + wall_us, a.cpu + cpu_us);
        }
    });
}

/// The windows a detector of grids is given (`kind`: "mp3", "aac"), to count how many are the same
/// ones as in an earlier call on this file (the whole file's, then each part's).
pub fn windows<'a>(kind: &'static str, windows: impl Iterator<Item = &'a [[f32; 2]]>) {
    if !enabled() {
        return;
    }
    use std::hash::{Hash, Hasher};
    let prints: Vec<u64> = windows
        .map(|w| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            w.iter().for_each(|f| (f[0].to_bits(), f[1].to_bits()).hash(&mut h));
            h.finish()
        })
        .collect();
    CONTEXT.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            let e = ctx.windows.entry(kind).or_default();
            e.0 += prints.len() as u64;
            e.1.extend(prints);
        }
    });
}

/// A moment of the operation (start and end of a pass over the files).
pub fn mark(name: &str) {
    if enabled() {
        push(serde_json::json!({ "mark": name, "t": micros_since_start() }));
    }
}

/// Starts a measured operation: earlier lines are dropped, the clock starts again.
pub fn start() {
    if enabled() {
        let _ = start_time();
        if let Ok(mut l) = lines().lock() {
            l.clear();
        }
        mark("start");
    }
}

/// Writes the lines to the BT_PROFILE file, with the totals of the process.
pub fn finish() {
    let Some(path) = target() else { return };
    mark("end");
    let mut proc = serde_json::json!({ "proc": true, "t": micros_since_start() });
    #[cfg(target_os = "linux")]
    {
        if let Ok(stat) = std::fs::read_to_string("/proc/self/stat") {
            // After the name in parentheses: state is field 3, utime 14, stime 15, delayacct_blkio_ticks 42.
            let rest: Vec<&str> = stat.rsplit_once(')').map_or("", |(_, r)| r).split_whitespace().collect();
            let field = |n: usize| rest.get(n - 3).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            proc["utime_ticks"] = field(14).into();
            proc["stime_ticks"] = field(15).into();
            proc["blkio_ticks"] = field(42).into();
        }
        if let Ok(io) = std::fs::read_to_string("/proc/self/io") {
            for line in io.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    if let Ok(n) = v.trim().parse::<u64>() {
                        proc[k] = n.into();
                    }
                }
            }
        }
    }
    push(proc);
    if let Ok(l) = lines().lock() {
        let _ = std::fs::write(path, l.join("\n") + "\n");
    }
}
