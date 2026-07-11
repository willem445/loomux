//! File hashing for the file-manager pane (issue #214).
//!
//! Two callers, one engine:
//!
//!   * the listing's **short SHA-256 column** — a whole directory's worth of files,
//!     filled in as they compute;
//!   * the right-click **Hash →** submenu — one file, one algorithm, on demand.
//!
//! ## Why it is a worker thread and not a command that returns a value
//!
//! Tauri runs a synchronous command on the **main (webview) thread**. Hashing reads
//! the file — every byte of it — so a sync `fm_hash(rel)` would freeze the entire
//! window on the first multi-megabyte row, and a directory of them would freeze it
//! for as long as the whole directory took. That is the same trap `ft_search` fell
//! into in #207, and this takes the same way out: `fm_hash_start` spawns a worker
//! thread, streams results back as `fm-hash` events tagged with the caller's id, and
//! polls a per-id cancel flag between files **and between chunks**, so navigating away
//! from a directory mid-hash stops the work rather than letting it grind on for a view
//! nobody is looking at.
//!
//! It reuses `fileedit`'s `SearchRegistry` and `ft_search_cancel` outright: ids come
//! from one monotonic frontend counter, so they are unique across the search, the
//! file-name enumeration, and this — one registry and one cancel command serve all
//! three.
//!
//! ## The file is STREAMED, never read into memory
//!
//! A 4 GiB ISO must cost 64 KiB of RAM, not 4 GiB. Everything below feeds a fixed
//! buffer through the hasher in a loop; nothing here ever calls `fs::read`.

use serde::Serialize;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::fileedit::{err, safe_resolve, SearchRegistry};
use sha1::Digest as _;
use tauri::{AppHandle, Emitter, State};

/// Streaming buffer. Big enough that the syscall overhead disappears, small enough
/// that a hundred concurrent hashes could never be a memory story.
const CHUNK: usize = 64 * 1024;

/// Results are streamed to the UI in batches of this size, so a directory's column
/// fills in progressively instead of arriving as one lump at the end. The cancel flag
/// is polled between files, so a batch also bounds how long a superseded run keeps
/// working before it notices.
const HASH_BATCH: usize = 8;

// CRC variants, named explicitly. "CRC-16" on its own is genuinely ambiguous (there
// are dozens), so the algorithm is pinned here and the UI shows which one it used
// rather than leaving the user to guess why their checksum doesn't match.
//
//   CRC-32/ISO-HDLC — the one everything means by "CRC32": zlib, PNG, zip, gzip.
//   CRC-16/ARC      — the classic "CRC-16" (IBM/ANSI), the most common bare CRC-16.
//   CRC-8/SMBUS     — the plain polynomial-0x07 CRC-8, likewise the unmarked default.
static CRC32: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);
static CRC16: crc::Crc<u16> = crc::Crc::<u16>::new(&crc::CRC_16_ARC);
static CRC8: crc::Crc<u8> = crc::Crc::<u8>::new(&crc::CRC_8_SMBUS);

/// The algorithms the Hash submenu offers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HashAlgo {
    Sha256,
    Sha512,
    Sha1,
    Crc32,
    Crc16,
    Crc8,
}

impl HashAlgo {
    /// The wire name the frontend sends. Kept as an explicit map (not a derive) so the
    /// two sides can't drift silently on a rename.
    pub fn from_wire(s: &str) -> Result<Self, String> {
        match s {
            "sha256" => Ok(Self::Sha256),
            "sha512" => Ok(Self::Sha512),
            "sha1" => Ok(Self::Sha1),
            "crc32" => Ok(Self::Crc32),
            "crc16" => Ok(Self::Crc16),
            "crc8" => Ok(Self::Crc8),
            _ => Err(err("invalid-algo", format!("unknown hash algorithm: {s}"))),
        }
    }
}

/// One running hash, whichever algorithm it is. An enum rather than a `Box<dyn>`: the
/// set is closed, and this keeps the hot loop monomorphic.
enum Running {
    Sha256(sha2::Sha256),
    Sha512(sha2::Sha512),
    Sha1(sha1::Sha1),
    Crc32(crc::Digest<'static, u32>),
    Crc16(crc::Digest<'static, u16>),
    Crc8(crc::Digest<'static, u8>),
}

impl Running {
    fn new(algo: HashAlgo) -> Self {
        match algo {
            HashAlgo::Sha256 => Self::Sha256(sha2::Sha256::new()),
            HashAlgo::Sha512 => Self::Sha512(sha2::Sha512::new()),
            HashAlgo::Sha1 => Self::Sha1(sha1::Sha1::new()),
            HashAlgo::Crc32 => Self::Crc32(CRC32.digest()),
            HashAlgo::Crc16 => Self::Crc16(CRC16.digest()),
            HashAlgo::Crc8 => Self::Crc8(CRC8.digest()),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha256(h) => h.update(bytes),
            Self::Sha512(h) => h.update(bytes),
            Self::Sha1(h) => h.update(bytes),
            Self::Crc32(d) => d.update(bytes),
            Self::Crc16(d) => d.update(bytes),
            Self::Crc8(d) => d.update(bytes),
        }
    }

    /// Lowercase hex, zero-padded to the algorithm's full width — a CRC-8 of 5 is
    /// "05", not "5", so digests of the same algorithm always compare as strings.
    fn finish(self) -> String {
        match self {
            Self::Sha256(h) => hex(&h.finalize()),
            Self::Sha512(h) => hex(&h.finalize()),
            Self::Sha1(h) => hex(&h.finalize()),
            Self::Crc32(d) => format!("{:08x}", d.finalize()),
            Self::Crc16(d) => format!("{:04x}", d.finalize()),
            Self::Crc8(d) => format!("{:02x}", d.finalize()),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Hash one already-resolved path, streaming it. `cancelled` is polled between chunks,
/// so abandoning a 4 GiB file is near-instant rather than "when it finishes".
/// Returns `None` when cancelled — a partial digest would be a lie, so there isn't one.
pub fn hash_path(
    path: &Path,
    algo: HashAlgo,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<String>, String> {
    let md = std::fs::symlink_metadata(path).map_err(|e| err("not-found", e.to_string()))?;
    if md.is_dir() {
        return Err(err("is-dir", "a directory has no file hash"));
    }
    if md.file_type().is_symlink() {
        // Consistent with every other op in this pane: a link is shown, never followed.
        // Hashing one would silently hash its TARGET, which may be outside the root.
        return Err(err("symlink", "symlinks are not followed"));
    }
    let mut file = std::fs::File::open(path).map_err(|e| err("io", e.to_string()))?;
    let mut running = Running::new(algo);
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancelled() {
            return Ok(None);
        }
        let n = file.read(&mut buf).map_err(|e| err("io", e.to_string()))?;
        if n == 0 {
            break;
        }
        running.update(&buf[..n]);
    }
    Ok(Some(running.finish()))
}

/// Hash `rel` under `root`, resolving it through the same containment choke point as
/// every other file-manager op. Testable without a Tauri runtime.
pub fn hash_file(
    root: &str,
    rel: &str,
    algo: HashAlgo,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<String>, String> {
    let path = safe_resolve(root, rel)?;
    hash_path(&path, algo, cancelled)
}

// ---------- wire types ----------

/// One file's outcome. Exactly one of `digest`/`error` is set; a cancelled file appears
/// in neither (it is simply never emitted — see `hash_path`).
#[derive(Clone, Serialize)]
pub struct HashResult {
    pub rel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One streamed batch, tagged with the caller's id so batches from a superseded run
/// (the user navigated away) are dropped by the frontend. Same discipline as
/// `ft-search` and `ft-files`.
#[derive(Clone, Serialize)]
struct HashEvent {
    id: u64,
    algo: String,
    results: Vec<HashResult>,
    done: bool,
}

/// Kick off hashing `rels` under `root` on a worker thread. Returns immediately;
/// results arrive as `fm-hash` events tagged with `id`, and `ft_search_cancel(id)`
/// stops it (one registry serves the search, the name index, and this).
///
/// Used for BOTH the listing column (many rels) and the Hash submenu (one rel) — same
/// path, so there is one place where hashing can be wrong, and it is the tested one.
#[tauri::command]
pub fn fm_hash_start(
    app: AppHandle,
    registry: State<'_, Arc<SearchRegistry>>,
    id: u64,
    root: String,
    rels: Vec<String>,
    algo: String,
) {
    let flag = registry.begin(id);
    let reg = registry.inner().clone();
    std::thread::spawn(move || {
        let cancelled = || flag.load(Ordering::Relaxed);
        let parsed = HashAlgo::from_wire(&algo);
        let mut batch: Vec<HashResult> = Vec::new();
        let emit = |results: Vec<HashResult>, done: bool| {
            let _ = app.emit(
                "fm-hash",
                HashEvent {
                    id,
                    algo: algo.clone(),
                    results,
                    done,
                },
            );
        };

        match parsed {
            Err(e) => {
                // A bad algorithm name is not a per-file failure — report it once,
                // against every rel, rather than silently hashing nothing.
                let results = rels
                    .into_iter()
                    .map(|rel| HashResult {
                        rel,
                        digest: None,
                        error: Some(e.clone()),
                    })
                    .collect();
                emit(results, true);
            }
            Ok(algo) => {
                for rel in rels {
                    if cancelled() {
                        break;
                    }
                    let result = match hash_file(&root, &rel, algo, &cancelled) {
                        Ok(Some(digest)) => HashResult {
                            rel,
                            digest: Some(digest),
                            error: None,
                        },
                        // Cancelled mid-file: emit nothing for it. A partial digest
                        // would be worse than no digest.
                        Ok(None) => break,
                        Err(e) => HashResult {
                            rel,
                            digest: None,
                            error: Some(e),
                        },
                    };
                    batch.push(result);
                    if batch.len() >= HASH_BATCH {
                        emit(std::mem::take(&mut batch), false);
                    }
                }
                // Always a terminal event, even when cancelled: the frontend keys off
                // `id`, so a `done` for a superseded run is simply ignored.
                emit(batch, true);
            }
        }
        reg.end(id);
    });
}
