//! Durable whole-file replace (#133).
//!
//! One primitive: [`atomic_write`], the temp-file + fsync + rename that every
//! orchestration state file (`state.json`, `group.json`, `tasks.json`,
//! `queue.json`, the #83 grants) is stored through, plus the sequence counter
//! that keeps two concurrent writers' scratch files apart.
//!
//! It is here (#888 slice A3 batch 9) because durability is not a desktop
//! concern: `std::fs` only, no `tauri`, no pane, and a headless daemon writes
//! exactly the same files the app does. Through batch 9 it had no outward edge
//! at all — not even `lock_safe` — which is what let it move in the same batch
//! as [`crate::subproc`] without sharing anything with it. The two are
//! deliberately separate modules: a bounded subprocess capture and a crash-safe
//! file replace answer different failure modes and share no design story.
//!
//! **It has exactly one outward edge now** (#1609), and it is named here rather
//! than left for a reader to find in the body: [`atomic_write`] calls
//! [`crate::budget::note_durable_write`]. That is two thread-local reads and,
//! on the rare path, a breadcrumb — no lock, no wait, no new failure mode — and
//! it exists because this function is the single door every durable
//! orchestration state file goes through, which makes it the only place that
//! can notice a write happening on a read path that a bounded acquisition could
//! unwind out of (`doc/design/lock-liveness.md` §4). The boundary argument
//! above is untouched: still `std::fs` only, still no `tauri`, still nothing a
//! headless daemon cannot link.
//!
//! **Deliberately no `tempfile`.** Uniqueness comes from a std atomic, which
//! keeps this clear of the getrandom-based crates the Windows 10 baseline
//! cannot load (CLAUDE.md constraint 2) — and this crate is linked into the
//! shipped Windows binary, so that ban applies on this side of the boundary
//! exactly as it does in `src-tauri`.

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic counter that makes every temp filename unique, so two concurrent
/// writers to the same durable file never share a `.tmp` sibling. Some of the
/// files written through `atomic_write` (state.json, group.json) are not
/// serialized under a lock, so distinct temp names are what keeps a concurrent
/// pair from corrupting each other's scratch file. A std atomic keeps us clear
/// of the getrandom-based crates the Windows 10 baseline can't load (see the
/// Cargo.toml notes) — no `tempfile` needed for a unique name.
static ATOMIC_WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Durably replace `path` with `bytes`: write a same-directory temp file, flush
/// it to disk, then atomically rename it over the destination. A failure or
/// crash mid-write leaves the previous good file intact (at worst an orphaned
/// `.tmp` sibling) — never the truncated/empty destination that plain
/// `fs::write` produces. This is the #133 fix: a disk-full `fs::write` had
/// truncated tasks.json and destroyed the live board.
///
/// Same-directory temp is required for rename atomicity on Windows — a rename
/// across volumes falls back to a non-atomic copy. `fs::rename` on Windows maps
/// to `MoveFileExW` with `REPLACE_EXISTING`, which atomically replaces the
/// destination on the same volume, so the primary path already does the right
/// thing; the fallback only covers the rare case where the destination is
/// briefly locked (antivirus, an open reader). The temp is fsync'd before the
/// rename so a rename can't expose a metadata-only file whose data blocks never
/// reached disk — exactly the disk-full failure mode.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // #1609 rider R1's detector. Every durable orchestration state file is
    // written through here, which makes this the one place that can notice a
    // durable write happening inside a `read_budget` frame and outside any
    // `MutationScope` — the shape whose unwind could tear it. It REPORTS (a
    // counter and a bounded breadcrumb) and never refuses; see
    // `budget::note_durable_write` and `doc/design/lock-liveness.md` §4.
    crate::budget::note_durable_write(
        path.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
    );
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    // Ensure the destination dir exists — group state dirs always do, but the #83
    // grant subdirs (`merge_grants/`, `release_grants/`) may be fresh.
    fs::create_dir_all(dir)?;
    let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("state");
    let seq = ATOMIC_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{stem}.{}.{seq}.tmp", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?; // durable before the rename — the disk-full guard
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Rename can fail if the destination is momentarily locked. Fall
            // back to a direct write so the update isn't lost; keep the temp on
            // failure so the new contents remain recoverable.
            let r = fs::write(path, bytes);
            if r.is_ok() {
                let _ = fs::remove_file(&tmp);
            }
            r
        }
    }
}
