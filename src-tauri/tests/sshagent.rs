//! The hidden-ConPTY `ssh-add` driver, against a **fake** `ssh-add` (#2368
//! slice A).
//!
//! CLAUDE.md constraint 3: never spawn a real agent or `ssh` CLI to test
//! anything. Every scenario below is a `.bat` that speaks OpenSSH's own strings
//! — `Enter passphrase for %s%s: `, `Bad passphrase, try again for %s%s: `,
//! `Identity added: %s (%s)` — so what is under test is the *conversation*
//! (does the driver answer the right ask, give up the way ssh-add documents,
//! stay inside its bound, and keep a secret off `detail`) rather than OpenSSH.
//!
//! The prompts carry no trailing newline, exactly as the vendor's do, which is
//! the property `classify_ssh_add_line`'s whole-transcript reading exists for: a
//! line-oriented driver would never see either ask.
//!
//! Windows-only, like `direct_spawn.rs` and `job_object.rs`: the fixture is a
//! `.bat` and `cmd.exe`'s `set /p` is what gives us a program that blocks on a
//! console read. `drive_ssh_add` itself compiles and runs everywhere — it is the
//! *fixture* that is Windows-shaped, not the code under test.
#![cfg(windows)]

use loomux_lib::sshagent::{drive_ssh_add, SshAddOutcome};
use std::time::{Duration, Instant};

/// Write one fake `ssh-add` and return the argv that runs it.
///
/// CRLF because a `.bat` is read by `cmd.exe`, and argv rather than a command
/// string because that is how the driver spawns: `["cmd.exe", "/C", <bat>]`.
fn fake_ssh_add(dir: &std::path::Path, name: &str, body: &str) -> Vec<String> {
    let bat = dir.join(name);
    let crlf = body.replace('\n', "\r\n");
    std::fs::write(&bat, crlf).expect("write the fake ssh-add");
    let comspec = std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
    vec![comspec, "/C".to_string(), bat.to_string_lossy().into_owned()]
}

/// The happy path's fixture: ask once, accept `hunter2`, report success in the
/// vendor's own words.
const ACCEPTS: &str = concat!(
    "@echo off\n",
    "set /p p=Enter passphrase for C:\\keys\\id_ed25519: \n",
    "if not \"%p%\"==\"hunter2\" goto bad\n",
    "echo Identity added: C:\\keys\\id_ed25519 test-key\n",
    "exit /b 0\n",
    ":bad\n",
    "set /p q=Bad passphrase, try again for C:\\keys\\id_ed25519: \n",
    "exit /b 1\n",
);

#[test]
fn the_right_passphrase_is_answered_and_the_key_is_added() {
    let dir = tempfile::tempdir().unwrap();
    let argv = fake_ssh_add(dir.path(), "accepts.bat", ACCEPTS);

    let outcome = drive_ssh_add(&argv, b"hunter2", Duration::from_secs(20));

    assert_eq!(
        outcome,
        SshAddOutcome::Added,
        "the driver must recognise the prompt, answer it, and read `Identity added`"
    );
}

#[test]
fn a_wrong_passphrase_gives_up_the_way_ssh_add_documents() {
    // The retry ask is an ask: ssh-add re-prompts until it is handed an EMPTY
    // passphrase. Answering it with the same wrong value again is a spin that
    // ends at the timeout, so the property being pinned is BOTH the verdict and
    // that it arrived long before the bound — the empty-line give-up is what
    // ended the conversation, not the deadline.
    let dir = tempfile::tempdir().unwrap();
    let argv = fake_ssh_add(dir.path(), "rejects.bat", ACCEPTS);

    let bound = Duration::from_secs(20);
    let started = Instant::now();
    let outcome = drive_ssh_add(&argv, b"wrong-one", bound);
    let elapsed = started.elapsed();

    match &outcome {
        SshAddOutcome::BadPassphrase { detail } => {
            assert!(
                detail.contains("Bad passphrase"),
                "the refusal must quote ssh-add's own words, got: {detail}"
            );
        }
        other => panic!("expected BadPassphrase, got {other:?}"),
    }
    assert!(
        elapsed < bound / 2,
        "the give-up must end the run, not the timeout — took {elapsed:?} of {bound:?}"
    );
}

#[test]
fn a_program_that_never_prompts_is_bounded_rather_than_waited_on() {
    // `set /p` with no prompt text blocks on a console read forever. Nothing the
    // driver recognises is ever printed, so there is nothing to answer — the
    // only thing that can end this run is the bound, which is the point: this
    // runs on the blocking pool, and an unbounded wait there is a slot held for
    // the life of the app.
    let dir = tempfile::tempdir().unwrap();
    let argv = fake_ssh_add(
        dir.path(),
        "silent.bat",
        "@echo off\necho working\nset /p never=\n",
    );

    let bound = Duration::from_secs(2);
    let started = Instant::now();
    let outcome = drive_ssh_add(&argv, b"hunter2", bound);
    let elapsed = started.elapsed();

    assert_eq!(outcome, SshAddOutcome::Timeout);
    assert!(
        elapsed < bound * 5,
        "the bound must actually bound: took {elapsed:?} for a {bound:?} deadline"
    );
}

#[test]
fn a_program_that_echoes_the_passphrase_back_cannot_leak_it_through_detail() {
    // The vacuity control for `scrub_secret`, run in situ rather than on a
    // string literal. The real ssh-add reads with echo off and never prints a
    // passphrase back — so the scrub is defending against the case where the
    // program on the other end of the pty is NOT the real ssh-add. This fixture
    // is exactly that program.
    let dir = tempfile::tempdir().unwrap();
    let argv = fake_ssh_add(
        dir.path(),
        "leaks.bat",
        concat!(
            "@echo off\n",
            "set /p p=Enter passphrase for C:\\keys\\id_ed25519: \n",
            "echo Bad passphrase, try again for %p%\n",
            "exit /b 1\n",
        ),
    );

    let outcome = drive_ssh_add(&argv, b"correcthorsebatterystaple", Duration::from_secs(20));

    match &outcome {
        SshAddOutcome::BadPassphrase { detail } => {
            assert!(
                !detail.contains("correcthorsebatterystaple"),
                "an echoed passphrase must not ride out on `detail`, got: {detail}"
            );
            // Positive control: an assertion that a string does NOT contain a
            // secret passes just as well over an empty detail. The rest of what
            // the fake said must still be there, or this test would pass against
            // a driver that reported nothing at all.
            assert!(
                detail.contains("Bad passphrase"),
                "the rest of the line must survive the scrub, got: {detail}"
            );
        }
        other => panic!("expected BadPassphrase, got {other:?}"),
    }
}
