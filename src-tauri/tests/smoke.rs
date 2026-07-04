//! Minimal integration-test target. Its presence makes cargo accept the
//! `rustc-link-arg-tests` instructions from build.rs, which embed the
//! common-controls-v6 manifest test executables need to load on Windows
//! (see test.manifest). The real tests live as unit tests in the lib.

#[test]
fn lib_links_and_loads() {
    // Referencing the lib forces the full dependency graph (including the
    // UI stack) into this exe — it fails to LOAD with 0xc0000139 if the
    // manifest embedding regresses.
    assert_eq!(loomux_lib::orchestration::strip_ansi(b"\x1b[31mok\x1b[0m"), "ok");
}
