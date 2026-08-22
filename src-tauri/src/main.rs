// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Crash observability for the class the panic hook structurally cannot see
/// (#1219, after #1218's round-3 diagnosis). A refused allocation goes
/// `handle_alloc_error` → `abort()` and never enters `std::panicking`, so no
/// panic hook runs — all three of #1218's production crashes were that, and all
/// three left nothing. `std::alloc::set_alloc_error_hook` is the matching seam
/// and is nightly-only; a `#[global_allocator]` wrapper that reports on a null
/// return is the stable one. `loomux_lib::run` arms it via
/// `obs::install_alloc_error_reporting`; until then it is a pure delegation to
/// `System`.
///
/// **It belongs in the binary, not in `loomux_lib`, and that is load-bearing
/// rather than stylistic.** A `#[global_allocator]` may be declared once per
/// linked artifact, and it is inherited by everything that links the crate
/// declaring it. Declared in the lib, it is inherited by every
/// `src-tauri/tests/*` binary — including `tests/usage_memory.rs`, whose whole
/// method is to declare a *counting* allocator of its own and assert a peak
/// (#1218). Two declarations in one artifact is a hard compile error
/// ("the `#[global_allocator]` in this crate conflicts with global allocator
/// in: loomux_lib"), so the lib placement broke that test outright. Here it
/// covers exactly what it should: the shipped `loomux.exe`, and the E2E build
/// made from this same entry point. A test that needs a different allocator
/// keeps the right to say so.
///
/// Being in the binary loses nothing at runtime: the choice is made at link
/// time for the whole process, so allocations made by code inside `loomux_lib`
/// — which is all of them — go through this wrapper just the same.
#[global_allocator]
static ALLOC: loomux_engine::obs::CrashReportingAlloc = loomux_engine::obs::CrashReportingAlloc;

fn main() {
    loomux_lib::run()
}
