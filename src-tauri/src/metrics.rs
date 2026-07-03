//! Live system-resource sampling for the bottom status bar.
//!
//! A single background thread polls CPU + memory (via `sysinfo`) and the GPU
//! on a fixed cadence and pushes each snapshot to the frontend on the
//! `system-metrics` event — the same fire-and-forget pattern the PTY layer
//! uses for output. The UI just listens; there's no command to call and
//! nothing to poll from JS.
//!
//! Everything here is architecture- and vendor-neutral where possible:
//! `sysinfo` handles CPU + memory on any CPU architecture (x86, ARM, …) and
//! OS, and the GPU sampler tries several backends in turn so AMD, Intel, and
//! NVIDIA adapters all report:
//!   1. `nvidia-smi`         — NVIDIA on any OS (richest data).
//!   2. Windows WDDM         — any adapter via PDH counters + DXGI.
//!   3. Linux DRM sysfs      — AMD/Intel via `/sys/class/drm`.
//! The working backend is remembered so we don't re-probe every tick.

use std::process::Command;
use std::thread;
use std::time::Duration;

use serde::Serialize;
use sysinfo::{System, MINIMUM_CPU_UPDATE_INTERVAL};
use tauri::{AppHandle, Emitter};

/// How often a fresh snapshot is emitted. Two seconds keeps the bar lively
/// without spawning `nvidia-smi` too aggressively.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(2000);

/// GPU slice of a snapshot; `None` when no NVIDIA GPU / `nvidia-smi` is present.
#[derive(Clone, Serialize)]
struct GpuMetrics {
    /// Adapter name, surfaced as a tooltip.
    name: String,
    /// Core utilization, 0–100.
    util: f32,
    /// Dedicated VRAM in use, mebibytes.
    vram_used_mb: u64,
    /// Total dedicated VRAM, mebibytes.
    vram_total_mb: u64,
}

/// One resource snapshot. Memory is in bytes; the frontend formats to GB.
#[derive(Clone, Serialize)]
struct SystemMetrics {
    /// Overall CPU utilization across all cores, 0–100.
    cpu: f32,
    mem_used: u64,
    mem_total: u64,
    gpu: Option<GpuMetrics>,
}

/// Spawn the sampler thread. Call once at startup; it runs for the app's life.
pub fn start(app: AppHandle) {
    thread::spawn(move || {
        let mut sys = System::new();

        // Prime the CPU counters: the first reading needs a prior sample to
        // diff against, so without this the initial emit would report 0%.
        sys.refresh_cpu_usage();
        thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);

        let mut gpu = GpuSampler::new();
        // Once we've seen enough consecutive GPU failures we assume there is
        // no usable GPU and stop probing on every tick.
        let mut gpu_enabled = true;
        let mut gpu_fails: u8 = 0;

        loop {
            sys.refresh_cpu_usage();
            sys.refresh_memory();

            let gpu_snapshot = if gpu_enabled {
                match gpu.sample() {
                    Some(g) => {
                        gpu_fails = 0;
                        Some(g)
                    }
                    None => {
                        gpu_fails += 1;
                        if gpu_fails >= 3 {
                            gpu_enabled = false;
                        }
                        None
                    }
                }
            } else {
                None
            };

            let snapshot = SystemMetrics {
                cpu: sys.global_cpu_usage(),
                mem_used: sys.used_memory(),
                mem_total: sys.total_memory(),
                gpu: gpu_snapshot,
            };
            let _ = app.emit("system-metrics", snapshot);

            thread::sleep(SAMPLE_INTERVAL);
        }
    });
}

/// Which GPU backend produced a reading. Cached so we don't re-probe backends
/// that don't apply on this machine every tick.
#[derive(Clone, Copy)]
enum GpuBackend {
    Nvidia,
    #[cfg(windows)]
    Wddm,
    #[cfg(target_os = "linux")]
    Drm,
}

/// Tries GPU backends in priority order and remembers the one that works.
struct GpuSampler {
    backend: Option<GpuBackend>,
}

impl GpuSampler {
    fn new() -> Self {
        Self { backend: None }
    }

    /// Backends to try, most-preferred first. Order is compile-time constant.
    #[allow(unused_mut)]
    fn candidates() -> Vec<GpuBackend> {
        let mut v = vec![GpuBackend::Nvidia];
        #[cfg(windows)]
        v.push(GpuBackend::Wddm);
        #[cfg(target_os = "linux")]
        v.push(GpuBackend::Drm);
        v
    }

    fn run(backend: GpuBackend) -> Option<GpuMetrics> {
        match backend {
            GpuBackend::Nvidia => sample_nvidia(),
            #[cfg(windows)]
            GpuBackend::Wddm => sample_windows_wddm(),
            #[cfg(target_os = "linux")]
            GpuBackend::Drm => sample_linux_drm(),
        }
    }

    fn sample(&mut self) -> Option<GpuMetrics> {
        // Fast path: reuse the backend that worked last time.
        if let Some(b) = self.backend {
            if let Some(g) = Self::run(b) {
                return Some(g);
            }
            self.backend = None; // it stopped working — fall back to probing.
        }
        for b in Self::candidates() {
            if let Some(g) = Self::run(b) {
                self.backend = Some(b);
                return Some(g);
            }
        }
        None
    }
}

/// Query the first GPU via `nvidia-smi`. Returns `None` if the tool is absent
/// or its output can't be parsed, so callers degrade to the next backend.
fn sample_nvidia() -> Option<GpuMetrics> {
    let mut cmd = Command::new("nvidia-smi");
    cmd.args([
        "--query-gpu=name,utilization.gpu,memory.used,memory.total",
        "--format=csv,noheader,nounits",
    ]);
    // Don't flash a console window on Windows for the child process.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    // First line = first GPU. Fields: name, util%, mem.used MiB, mem.total MiB.
    let line = text.lines().find(|l| !l.trim().is_empty())?;
    let mut parts = line.split(',').map(str::trim);

    let name = parts.next()?.to_string();
    // Some cards report "[N/A]" for utilization — treat that as 0 rather than
    // dropping an otherwise-good VRAM reading.
    let util = parts.next()?.parse().unwrap_or(0.0);
    let vram_used_mb = parts.next()?.parse().ok()?;
    let vram_total_mb = parts.next()?.parse().ok()?;

    Some(GpuMetrics {
        name,
        util,
        vram_used_mb,
        vram_total_mb,
    })
}

// ------------------------------ Windows WDDM ------------------------------
// Vendor-neutral: works for any adapter Windows exposes through WDDM.

/// Sample any GPU on Windows via performance counters (utilization + used
/// VRAM) and DXGI (adapter name + total VRAM).
#[cfg(windows)]
fn sample_windows_wddm() -> Option<GpuMetrics> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Performance::{
        PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhOpenQueryW,
    };

    // DXGI gives an accurate total VRAM (unlike WMI, which caps at ~4 GB) plus
    // the adapter name. Without at least this, treat the machine as GPU-less.
    let (name, vram_total_mb) = dxgi_primary_adapter()?;

    unsafe {
        // PDH query/counter handles are opaque `isize`s in this binding.
        let mut query: isize = 0;
        if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
            // No live counters available; still show the adapter + total VRAM.
            return Some(GpuMetrics {
                name,
                util: 0.0,
                vram_used_mb: 0,
                vram_total_mb,
            });
        }

        // English counter names work regardless of the OS display language.
        let util_path = wide(r"\GPU Engine(*)\Utilization Percentage");
        let mem_path = wide(r"\GPU Adapter Memory(*)\Dedicated Usage");
        let mut util_counter: isize = 0;
        let mut mem_counter: isize = 0;
        let has_util =
            PdhAddEnglishCounterW(query, PCWSTR(util_path.as_ptr()), 0, &mut util_counter) == 0;
        let has_mem =
            PdhAddEnglishCounterW(query, PCWSTR(mem_path.as_ptr()), 0, &mut mem_counter) == 0;

        // Utilization is a rate counter: two collections a moment apart let
        // PDH compute the delta between them.
        PdhCollectQueryData(query);
        thread::sleep(Duration::from_millis(150));
        PdhCollectQueryData(query);

        let util = if has_util {
            pdh_engine_utilization(util_counter).unwrap_or(0.0)
        } else {
            0.0
        };
        let used_bytes = if has_mem {
            pdh_dedicated_usage(mem_counter).unwrap_or(0)
        } else {
            0
        };

        PdhCloseQuery(query);

        Some(GpuMetrics {
            name,
            util: util as f32,
            vram_used_mb: used_bytes / (1024 * 1024),
            vram_total_mb,
        })
    }
}

/// NUL-terminated UTF-16, for the wide Win32 string APIs.
#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Pick the discrete/primary adapter (largest dedicated VRAM, skipping the
/// software renderer) and return its name plus total VRAM in mebibytes.
#[cfg(windows)]
fn dxgi_primary_adapter() -> Option<(String, u64)> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_DESC1, DXGI_ADAPTER_FLAG_SOFTWARE,
    };

    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut best: Option<(String, u64)> = None;
        let mut i = 0u32;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let mut desc = DXGI_ADAPTER_DESC1::default();
            if adapter.GetDesc1(&mut desc).is_err() {
                continue;
            }
            // Skip the Microsoft Basic Render Driver (software adapter).
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let total = desc.DedicatedVideoMemory as u64;
            let end = desc
                .Description
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(desc.Description.len());
            let name = String::from_utf16_lossy(&desc.Description[..end]);
            if best.as_ref().map_or(true, |(_, t)| total > *t) {
                best = Some((name, total));
            }
        }
        best.map(|(name, bytes)| (name, bytes / (1024 * 1024)))
    }
}

/// Sum utilization per GPU-engine type, then take the busiest type — this
/// mirrors how Task Manager reports an overall GPU load.
#[cfg(windows)]
unsafe fn pdh_engine_utilization(counter: isize) -> Option<f64> {
    use std::collections::HashMap;
    let mut per_type: HashMap<String, f64> = HashMap::new();
    for (name, value) in pdh_counter_items(counter)? {
        // Instance names look like `pid_1234_..._engtype_3D`; group by the
        // trailing engine type so multiple processes on one engine add up.
        let engtype = name.rsplit("engtype_").next().unwrap_or("").to_string();
        *per_type.entry(engtype).or_insert(0.0) += value;
    }
    Some(per_type.values().copied().fold(0.0_f64, f64::max).min(100.0))
}

/// Total dedicated VRAM in use, summed across all adapter-memory instances.
#[cfg(windows)]
unsafe fn pdh_dedicated_usage(counter: isize) -> Option<u64> {
    let bytes: f64 = pdh_counter_items(counter)?.iter().map(|(_, v)| *v).sum();
    Some(bytes as u64)
}

/// Read a wildcard PDH counter into `(instance name, value)` pairs.
#[cfg(windows)]
unsafe fn pdh_counter_items(counter: isize) -> Option<Vec<(String, f64)>> {
    use windows::Win32::System::Performance::{
        PdhGetFormattedCounterArrayW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
    };

    // First call sizes the buffer (returns PDH_MORE_DATA and fills the sizes).
    let mut buf_size = 0u32;
    let mut count = 0u32;
    PdhGetFormattedCounterArrayW(
        counter,
        PDH_FMT_DOUBLE,
        &mut buf_size,
        &mut count,
        None,
    );
    if buf_size == 0 {
        return Some(Vec::new());
    }

    // Allocate as items (not bytes) so the buffer is correctly aligned; PDH
    // also writes the instance-name strings into the buffer's tail.
    let item_bytes = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    let n = (buf_size as usize + item_bytes - 1) / item_bytes;
    let mut buf: Vec<PDH_FMT_COUNTERVALUE_ITEM_W> = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); n];

    let status = PdhGetFormattedCounterArrayW(
        counter,
        PDH_FMT_DOUBLE,
        &mut buf_size,
        &mut count,
        Some(buf.as_mut_ptr()),
    );
    if status != 0 {
        return None;
    }

    let mut out = Vec::with_capacity(count as usize);
    for item in &buf[..count as usize] {
        // A non-zero CStatus means this instance has no valid value.
        if item.FmtValue.CStatus != 0 {
            continue;
        }
        let name = pwstr_to_string(item.szName);
        out.push((name, item.FmtValue.Anonymous.doubleValue));
    }
    Some(out)
}

/// Copy a NUL-terminated wide string out of a Win32 `PWSTR`.
#[cfg(windows)]
unsafe fn pwstr_to_string(p: windows::core::PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *p.0.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p.0, len))
}

// ------------------------------ Linux DRM ---------------------------------
// Vendor-neutral for AMD (and newer Intel) via the kernel's DRM sysfs.

/// Sample the first DRM card that exposes amdgpu-style utilization + VRAM
/// counters under `/sys/class/drm/card*/device`.
#[cfg(target_os = "linux")]
fn sample_linux_drm() -> Option<GpuMetrics> {
    use std::fs;
    for entry in fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let card = entry.file_name();
        let card = card.to_string_lossy();
        // Want the card node itself (`card0`), not connector nodes (`card0-DP-1`).
        if !card.starts_with("card") || card.contains('-') {
            continue;
        }
        let dev = entry.path().join("device");
        let used = read_u64(&dev.join("mem_info_vram_used"));
        let total = read_u64(&dev.join("mem_info_vram_total"));
        if let (Some(used), Some(total)) = (used, total) {
            let util = read_u64(&dev.join("gpu_busy_percent")).unwrap_or(0);
            return Some(GpuMetrics {
                name: card.into_owned(),
                util: util as f32,
                vram_used_mb: used / (1024 * 1024),
                vram_total_mb: total / (1024 * 1024),
            });
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn read_u64(path: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    // Not an assertion — this exercises the vendor-neutral WDDM path (which
    // any WDDM adapter, including NVIDIA, supports) and prints what it read,
    // so the counter/DXGI plumbing can be eyeballed with `--nocapture`.
    #[test]
    fn wddm_backend_reports_something() {
        match sample_windows_wddm() {
            Some(g) => println!(
                "WDDM: {} — util {:.0}% — VRAM {}/{} MiB",
                g.name, g.util, g.vram_used_mb, g.vram_total_mb
            ),
            None => println!("WDDM: no adapter reported"),
        }
    }
}
