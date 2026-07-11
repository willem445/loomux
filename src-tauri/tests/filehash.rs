//! Integration tests for file hashing (issue #214).
//!
//! Must be an integration test, not a unit test (CLAUDE.md constraint #4 — the Windows
//! test exe needs build.rs's comctl32-v6 manifest). Drives the public `filehash::*`
//! helpers the Tauri command wraps, so no Tauri runtime is needed.
//!
//! THE DIGESTS ARE CHECKED AGAINST PUBLISHED VECTORS, NOT AGAINST OURSELVES. A test
//! that hashes a file and asserts the answer equals what the code produced proves only
//! that the code is deterministic. These use the standard `abc` / empty-string vectors
//! from FIPS 180-4 (SHA-1/256/512) and the "check" values from the CRC catalogue
//! (`123456789`), so a wrong algorithm, a wrong CRC variant, or a byte-order slip is
//! caught rather than blessed.

use loomux_lib::filehash::{hash_file, hash_path, HashAlgo};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn never() -> bool {
    false
}

/// Hash a file containing exactly `content`, with no cancellation.
fn digest_of(dir: &Path, content: &[u8], algo: HashAlgo) -> String {
    let path = dir.join("f.bin");
    fs::write(&path, content).unwrap();
    hash_path(&path, algo, &never).unwrap().unwrap()
}

fn err_code(msg: &str) -> &str {
    msg.split(':').next().unwrap_or("").trim()
}

// ---------- known vectors ----------

#[test]
fn sha_digests_match_the_published_fips_vectors() {
    let d = tempfile::tempdir().unwrap();

    // FIPS 180-4 / RFC 3174 — the canonical "abc" vectors.
    assert_eq!(
        digest_of(d.path(), b"abc", HashAlgo::Sha256),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        digest_of(d.path(), b"abc", HashAlgo::Sha512),
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
         2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
    );
    assert_eq!(
        digest_of(d.path(), b"abc", HashAlgo::Sha1),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );

    // The empty file — a real case (a freshly created new file has zero bytes, so this
    // is what its column shows the moment you make one).
    assert_eq!(
        digest_of(d.path(), b"", HashAlgo::Sha256),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn crc_digests_match_the_catalogue_check_values_and_pin_the_variant() {
    // "CRC-16" and "CRC-8" are genuinely ambiguous — there are dozens of each. These are
    // the catalogue "check" values for the exact variants we picked, over "123456789",
    // so a silent switch to a different polynomial/init/reflection fails here rather
    // than quietly producing a checksum that matches nothing the user can compare with.
    let d = tempfile::tempdir().unwrap();
    let msg = b"123456789";

    // CRC-32/ISO-HDLC — what zlib, PNG, zip and gzip all mean by "CRC32".
    assert_eq!(digest_of(d.path(), msg, HashAlgo::Crc32), "cbf43926");
    // CRC-16/ARC — the classic IBM/ANSI "CRC-16".
    assert_eq!(digest_of(d.path(), msg, HashAlgo::Crc16), "bb3d");
    // CRC-8/SMBUS — the plain polynomial-0x07 CRC-8.
    assert_eq!(digest_of(d.path(), msg, HashAlgo::Crc8), "f4");
}

#[test]
fn digests_are_zero_padded_to_the_algorithm_width() {
    // A CRC-8 of 5 must render "05", not "5" — otherwise digests of the same algorithm
    // don't compare as strings and a user eyeballing two checksums is misled.
    let d = tempfile::tempdir().unwrap();
    for algo in [HashAlgo::Crc8, HashAlgo::Crc16, HashAlgo::Crc32] {
        let width = match algo {
            HashAlgo::Crc8 => 2,
            HashAlgo::Crc16 => 4,
            _ => 8,
        };
        // Search a few inputs for one whose digest has a leading zero; assert the width
        // holds for every input regardless.
        for i in 0u8..32 {
            let hex = digest_of(d.path(), &[i], algo);
            assert_eq!(hex.len(), width, "{algo:?} must always be {width} hex chars");
            assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        }
    }
}

#[test]
fn a_file_larger_than_the_streaming_chunk_hashes_correctly() {
    // The chunk is 64 KiB; a file that spans several chunks exercises the streaming loop
    // rather than a single read. If the loop dropped or double-fed a chunk, this diverges
    // from the one-shot digest of the same bytes.
    let d = tempfile::tempdir().unwrap();
    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();

    let path = d.path().join("big.bin");
    fs::write(&path, &big).unwrap();
    let streamed = hash_path(&path, HashAlgo::Sha256, &never).unwrap().unwrap();

    // Independent one-shot digest of the same bytes.
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(&big);
    let expected = h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();

    assert_eq!(streamed, expected, "the streaming loop must not alter the digest");
}

// ---------- containment, kinds, and refusals ----------

#[test]
fn hashing_refuses_to_escape_the_root() {
    // Same choke point as every other file-manager op — a `rel` that climbs out is
    // refused, so "hash this" can never be turned into "read that".
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(parent.path().join("secret.txt"), "x").unwrap();

    let e = hash_file(root.to_str().unwrap(), "../secret.txt", HashAlgo::Sha256, &never).unwrap_err();
    assert!(
        matches!(err_code(&e), "outside-root" | "not-found" | "invalid-path"),
        "got: {e}"
    );
}

#[test]
fn a_directory_has_no_hash_and_says_so() {
    // The listing column skips directories entirely; this is the backend refusing to be
    // asked, so a stray rel can't produce a nonsense digest of a directory handle.
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(d.path().join("sub")).unwrap();
    let e = hash_file(d.path().to_str().unwrap(), "sub", HashAlgo::Sha256, &never).unwrap_err();
    assert_eq!(err_code(&e), "is-dir", "got: {e}");
}

#[test]
fn a_symlink_is_not_followed_for_hashing() {
    // Hashing a link would silently hash its TARGET — which may sit outside the root.
    // Consistent with every other op in this pane: a link is shown and otherwise inert.
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "precious").unwrap();
    let link = root.path().join("link.txt");
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(outside.path().join("secret.txt"), &link).is_ok();
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(outside.path().join("secret.txt"), &link).is_ok();
    if !made {
        eprintln!("skipping: symlinks not permitted here");
        return;
    }
    let e = hash_file(root.path().to_str().unwrap(), "link.txt", HashAlgo::Sha256, &never).unwrap_err();
    assert_eq!(err_code(&e), "symlink", "got: {e}");
}

#[test]
fn an_unknown_algorithm_name_is_refused_rather_than_defaulted() {
    // Defaulting a typo'd algo to SHA-256 would hand the user a digest labelled as
    // something it isn't — worse than an error.
    assert!(HashAlgo::from_wire("sha256").is_ok());
    assert!(HashAlgo::from_wire("crc8").is_ok());
    let e = HashAlgo::from_wire("md5").unwrap_err();
    assert_eq!(err_code(&e), "invalid-algo", "got: {e}");
}

// ---------- cancellation ----------

#[test]
fn a_pre_cancelled_hash_returns_no_digest_at_all() {
    // Not a partial digest — there is no such thing. `None` means "we stopped", and the
    // caller emits nothing for that file rather than a value that means nothing.
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("f.bin");
    fs::write(&path, vec![b'x'; 500_000]).unwrap();

    let cancel = AtomicBool::new(true);
    let out = hash_path(&path, HashAlgo::Sha256, &|| cancel.load(Ordering::Relaxed)).unwrap();
    assert!(out.is_none(), "a cancelled hash yields no digest");
}

#[test]
fn cancellation_is_polled_between_chunks_so_a_big_file_stops_promptly() {
    // The point of chunk-level polling: navigating away from a directory must abandon a
    // multi-gigabyte hash in flight, not "when it finishes". The flag flips after a few
    // chunks; the hash must stop rather than run to completion.
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("big.bin");
    // Several chunks' worth (chunk = 64 KiB).
    fs::write(&path, vec![b'x'; 64 * 1024 * 10]).unwrap();

    let polls = AtomicUsize::new(0);
    let out = hash_path(&path, HashAlgo::Sha256, &|| {
        polls.fetch_add(1, Ordering::Relaxed) >= 3
    })
    .unwrap();
    assert!(out.is_none(), "it must stop, not finish");
    assert!(
        polls.load(Ordering::Relaxed) < 10,
        "it must stop soon after the flag flips, not read the whole file"
    );
}

// ---------- cache invalidation (the property the frontend key relies on) ----------

#[test]
fn changing_a_files_content_changes_its_digest() {
    // The frontend caches digests keyed by (path, size, mtime). That is only sound if a
    // content change actually moves the digest — pinned here so the cache's premise is
    // tested, not assumed.
    let d = tempfile::tempdir().unwrap();
    let a = digest_of(d.path(), b"before", HashAlgo::Sha256);
    let b = digest_of(d.path(), b"after!", HashAlgo::Sha256);
    assert_ne!(a, b, "same length, different content — the digest must differ");
}
