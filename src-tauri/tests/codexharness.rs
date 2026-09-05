//! The Codex **harness** (#2515 C1): the generated profile file, the launch
//! line, the store watcher and the solo MCP arm.
//!
//! Its sibling `codexsessions.rs` is about reading a store codex wrote; this
//! file is about the things loomux writes and the line it launches. The two
//! failure modes are independent — a lookup can be right while the profile
//! grants nothing, and vice versa.
//!
//! **What is worth pinning here, and why each is its own test.** Almost
//! everything in this slice fails SILENTLY. A profile whose top-level keys land
//! inside a table still parses; a contract that closes its own string
//! delimiter early still leaves a file on disk; a launch line missing `-p`
//! still starts codex. In every one of those cases the pane boots, looks
//! healthy, and simply has no trust, no tools or no contract — so the
//! assertions below are about the DOCUMENT and the LINE rather than about
//! whether the call returned `Ok`.
//!
//! An integration test, not inline `#[cfg(test)]`, per repo constraint 4.
//!
//! **No codex is ever run** (constraint 3), and not even `--help`: every vendor
//! fact these assertions encode is read blob-by-blob out of `openai/codex` at
//! tag `rust-v0.153.4` and quoted in `doc/design/codex.md`.

use loomux_lib::orchestration::{
    codex_profile_file_name, codex_profile_name, codex_profile_name_of_path,
    codex_profile_toml, codex_user_mcp_exposure, single_pane_autopilot_flags, CodexMcpAuth,
    PathSegment,
};
use std::fs;
use std::path::Path;

const CWD: &str = "C:\\Projects\\loomux-worktrees\\feat\\x";

fn seg(s: &str) -> PathSegment {
    PathSegment::parse(s).unwrap()
}

/// The profile a GROUP pane gets, at the posture most tests want.
fn group_profile(unattended: bool) -> String {
    codex_profile_toml(
        7777,
        CodexMcpAuth::EnvVar("ORRERIX_AGENT_TOKEN"),
        Path::new(CWD),
        unattended,
        "",
        None,
    )
}

// ---------------------------------------------------------------------------
// 1. The document's SHAPE — the failures that parse
// ---------------------------------------------------------------------------

/// **The one that fails silently and expensively.** TOML gives every key after
/// a table header to that table, so a top-level scalar emitted below
/// `[sandbox_workspace_write]` becomes `sandbox_workspace_write.approval_policy`
/// — a key codex's strict-config check reports at best and ignores at worst,
/// leaving the pane with none of its posture and nothing red to say so.
///
/// Asserted on POSITION rather than on the rendered text, because the text is
/// what a later edit changes: any new top-level key added below the first `[`
/// reddens this without anyone having to remember the rule.
#[test]
fn a_codex_profiles_top_level_keys_all_precede_the_first_table_header() {
    // Every top-level key the generator can emit, including the two that are
    // conditional — so this is a claim about the generator, not about one
    // configuration of it.
    let body = codex_profile_toml(
        7777,
        CodexMcpAuth::EnvVar("ORRERIX_AGENT_TOKEN"),
        Path::new(CWD),
        true,
        "high",
        Some("be excellent"),
    );
    let lines: Vec<&str> = body.lines().collect();
    let first_table = lines
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .expect("the document must have at least one table");
    for key in [
        "approval_policy",
        "sandbox_mode",
        "model_reasoning_effort",
        "developer_instructions",
    ] {
        let at = lines
            .iter()
            .position(|l| l.starts_with(&format!("{key} =")))
            .unwrap_or_else(|| panic!("{key} is not emitted at all:\n{body}"));
        assert!(
            at < first_table,
            "{key} is emitted at line {at}, BELOW the first table header at line {first_table} — \
             TOML would make it a key of that table, and nothing would fail loudly:\n{body}"
        );
    }
}

/// The trust line, which is the difference between a pane that reads its
/// kickoff and one that sits on "Do you trust the contents of this directory?"
/// eating it.
///
/// The Windows path is the specimen on purpose: it is the only one of the four
/// values in this document that needs escaping at all, and a quoted TOML key
/// with raw backslashes is a parse error rather than a wrong value — so getting
/// it wrong loses the WHOLE profile, not just the trust.
#[test]
fn a_codex_profile_trusts_the_panes_own_directory_with_the_backslashes_escaped() {
    let body = group_profile(true);
    assert!(
        body.contains("[projects.\"C:\\\\Projects\\\\loomux-worktrees\\\\feat\\\\x\"]"),
        "the trust key must be the pane's cwd as a TOML basic string, every backslash \
         doubled:\n{body}"
    );
    assert!(body.contains("trust_level = \"trusted\""), "{body}");
    // The negative half: a RAW backslash run inside the key would be a parse
    // error, so its absence is the actual property. Asserted separately
    // because the positive above would also pass on a document that carried
    // both forms.
    assert!(
        !body.contains("[projects.\"C:\\Projects"),
        "an unescaped backslash in the quoted key makes the whole profile unparseable:\n{body}"
    );
}

/// The posture is REAL on codex, unlike pi's, and it is the profile that
/// carries it — so the two postures must produce two different documents while
/// producing the same launch line (pinned in `codex_launch_flags_per_posture`).
///
/// Both directions are asserted. Only checking that unattended says `never`
/// would pass on a generator that hardcoded it, which is precisely the mistake
/// worth catching: an attended codex worker that never prompts is an
/// autonomous agent the human did not ask for.
#[test]
fn the_codex_approval_policy_flips_with_the_panes_posture() {
    assert!(group_profile(true).contains("approval_policy = \"never\""), "{}", group_profile(true));
    assert!(
        group_profile(false).contains("approval_policy = \"on-request\""),
        "{}",
        group_profile(false)
    );
    assert_ne!(
        group_profile(true),
        group_profile(false),
        "the two postures must differ SOMEWHERE in the profile — this is the document that \
         carries them, since the launch line deliberately does not"
    );
}

/// `workspace-write` plus network, and never the bypass rung.
///
/// The network line is not decoration: `workspace-write` turns network access
/// OFF by default, and a worker that cannot reach GitHub cannot open a PR — the
/// failure would arrive many minutes into a task, as a `gh` error, a long way
/// from this file.
#[test]
fn a_codex_pane_gets_workspace_write_with_network_and_never_the_bypass_rung() {
    let body = group_profile(true);
    assert!(body.contains("sandbox_mode = \"workspace-write\""), "{body}");
    assert!(body.contains("network_access = true"), "{body}");
    for forbidden in ["danger-full-access", "dangerously-bypass", "yolo"] {
        assert!(
            !body.contains(forbidden),
            "loomux never puts {forbidden:?} in a codex profile:\n{body}"
        );
    }
}

/// #687 on codex: the effort knob is a profile key, and an EMPTY effort emits
/// no key at all rather than an empty one.
///
/// The empty case is the one that matters and it is not a tidiness point:
/// `ReasoningEffort`'s own `FromStr` answers `Err("reasoning_effort must not be
/// empty")` for `""`, so `model_reasoning_effort = ""` would fail the whole
/// config — taking the trust level and the MCP server down with it — rather
/// than being ignored.
#[test]
fn a_codex_effort_knob_rides_the_profile_and_an_empty_one_emits_no_key() {
    let with = codex_profile_toml(
        7777,
        CodexMcpAuth::EnvVar("V"),
        Path::new(CWD),
        true,
        "xhigh",
        None,
    );
    assert!(with.contains("model_reasoning_effort = \"xhigh\""), "{with}");
    let without = group_profile(true);
    assert!(
        !without.contains("model_reasoning_effort"),
        "an unset effort must emit no key — an empty one is refused by codex outright, which \
         would lose the whole profile:\n{without}"
    );
}

// ---------------------------------------------------------------------------
// 2. The token — two shapes, and each must NOT be the other
// ---------------------------------------------------------------------------

/// A GROUP pane's profile names the environment variable and contains no token
/// byte (plan D2), and a SOLO pane's carries the token because it has no
/// environment to name (#2515 C1's amendment to D2).
///
/// Both halves assert the ABSENCE of the other shape as well as the presence of
/// their own. Presence alone would pass on a generator that emitted both maps,
/// which is the one outcome that is worse than either: it would put the secret
/// in the file AND depend on the variable.
#[test]
fn a_group_codex_profile_names_the_token_variable_and_a_solo_one_carries_the_token() {
    const TOKEN: &str = "tok-abc123-not-a-real-one";

    let group = codex_profile_toml(
        7777,
        CodexMcpAuth::EnvVar("ORRERIX_AGENT_TOKEN"),
        Path::new(CWD),
        true,
        "",
        None,
    );
    assert!(
        group.contains("env_http_headers = { \"X-Orrerix-Agent\" = \"ORRERIX_AGENT_TOKEN\" }"),
        "{group}"
    );
    assert!(
        !group.contains("http_headers = {"),
        "a group profile must not carry a literal header map — that is the solo shape:\n{group}"
    );

    let solo = codex_profile_toml(
        7777,
        CodexMcpAuth::Literal(TOKEN),
        Path::new(CWD),
        false,
        "",
        None,
    );
    assert!(solo.contains(&format!("http_headers = {{ \"X-Orrerix-Agent\" = \"{TOKEN}\" }}")), "{solo}");
    assert!(
        !solo.contains("env_http_headers"),
        "a solo profile must not name a variable: `solo_prepare` sets no pane environment, so \
         nothing would ever set it and the pane would connect unauthenticated while still being \
         advertised as a full channel member:\n{solo}"
    );
}

/// The group half of the pair above, stated as the property it actually is:
/// the token must not appear ANYWHERE in a group profile, not merely outside
/// the header map.
///
/// Separate from the test above because it is a different question. That one
/// asks which map is emitted; this one asks whether a token can reach the file
/// by some other route a later edit might add — a comment, a URL parameter, an
/// audit breadcrumb folded in.
#[test]
fn no_byte_of_a_group_codex_panes_token_reaches_its_profile() {
    const TOKEN: &str = "tok-must-not-appear-anywhere";
    let body = codex_profile_toml(
        7777,
        CodexMcpAuth::EnvVar("ORRERIX_AGENT_TOKEN"),
        Path::new(CWD),
        true,
        "high",
        Some("contract text"),
    );
    assert!(!body.contains(TOKEN), "{body}");
    // The control: this assertion is only meaningful because the generator
    // COULD have been handed the token — `CodexMcpAuth::Literal` is the same
    // type in the same position, and it does put it in.
    let solo = codex_profile_toml(
        7777,
        CodexMcpAuth::Literal(TOKEN),
        Path::new(CWD),
        true,
        "high",
        Some("contract text"),
    );
    assert!(
        solo.contains(TOKEN),
        "the assertion above is vacuous unless this generator can emit a token at all:\n{solo}"
    );
}

// ---------------------------------------------------------------------------
// 3. The contract, and the escaping that keeps it whole
// ---------------------------------------------------------------------------

/// A contract containing the delimiter must be ESCAPED, not truncated and not
/// rewritten (T1.5).
///
/// The failure this guards is the quiet one: a role contract that closes its
/// own string early leaves a file that either fails to parse (the pane loses
/// its trust and its MCP server, not just its persona) or — worse — parses with
/// the contract cut at the delimiter and the rest of it read as TOML.
///
/// Three specimens, each of which breaks a different naive encoder: a run of
/// three apostrophes (which is what the slice plan proposed to escape), a run
/// of three double quotes (which closes the form actually used), and a trailing
/// backslash (which would become a line continuation and swallow the closing
/// delimiter).
#[test]
fn a_codex_contract_with_triple_quotes_is_escaped_not_truncated() {
    let contract = "line one\n'''\nline two\n\"\"\"\nline three\\";
    let body = codex_profile_toml(
        7777,
        CodexMcpAuth::EnvVar("V"),
        Path::new(CWD),
        true,
        "",
        Some(contract),
    );
    // Every line of the contract survives — nothing was cut at a delimiter.
    for fragment in ["line one", "line two", "line three"] {
        assert!(body.contains(fragment), "{fragment:?} was lost:\n{body}");
    }
    // No run of three unescaped double quotes anywhere except the two
    // delimiters the generator itself wrote. Counting is how "escaped" is
    // asserted without asserting the exact encoding, which is an
    // implementation detail a later edit may improve.
    assert_eq!(
        body.matches("\"\"\"").count(),
        2,
        "exactly two `\"\"\"` runs may appear — the opening and closing delimiters. A third is \
         the contract closing the string early:\n{body}"
    );
    // The apostrophe run is content, so it must still be there VERBATIM: this
    // is the half that says "escaped, not rewritten". A generator that
    // sanitized the contract to make it fit would pass every assertion above.
    assert!(
        body.contains("'''"),
        "the apostrophe run is part of the contract and must reach the agent unchanged — \
         rewriting it would alter the role contract invisibly:\n{body}"
    );
    // And the trailing backslash must not be able to eat the delimiter.
    assert!(
        body.contains("line three\\\\"),
        "a trailing backslash must be escaped, or it becomes a TOML line continuation and \
         swallows the closing delimiter:\n{body}"
    );
}

// ---------------------------------------------------------------------------
// 4. The profile NAME — the vendor's alphabet, refused rather than sanitized
// ---------------------------------------------------------------------------

/// The writer's literal and the readers' const are ONE fact (#2515 C1).
///
/// `codex_profile_file_name` spells `.config.toml` literally in its `format!`
/// template — it has to, or `no_raw_identifier_is_interpolated_into_a_file_name`
/// would not see the site at all, since that scan's trigger is an extension
/// literal inside the template. The orphan sweep and
/// `codex_profile_name_of_path` cannot use a literal: `strip_suffix` needs the
/// value. So the suffix is spelled twice, and this is what stops that being
/// #502's "a delete path that re-derives a write path's shape either misses
/// files or matches too widely".
///
/// Asserted as a ROUND TRIP rather than as `assert_eq!` on two strings, because
/// the round trip is the property that actually matters: whatever the writer
/// produces, the reader must recover the same name from it. A test comparing
/// the two spellings would pass on a pair that agreed with each other and with
/// nothing the filesystem holds.
#[test]
fn the_codex_profile_suffix_and_its_file_name_builder_agree() {
    for id in ["w-3", "orch-1", "solo-27"] {
        let file = codex_profile_file_name(&seg(id)).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(file, format!("orrerix-{id}.config.toml"));
        assert_eq!(
            codex_profile_name_of_path(Path::new(&file)),
            Some(format!("orrerix-{id}").as_str()),
            "the reader must recover exactly the name `-p` is spelled with, from the file name \
             the writer produced — the two spellings of the suffix are one fact"
        );
    }
    // `file_stem` is the obvious wrong reader and would answer
    // `orrerix-w-3.config` — a name `-p` would look for under a file that is
    // not there. Pinned so the cheaper-looking implementation cannot be
    // adopted silently.
    let file = codex_profile_file_name(&seg("w-3")).unwrap();
    assert_ne!(
        codex_profile_name_of_path(Path::new(&file)),
        Path::new(&file).file_stem().and_then(|s| s.to_str()),
        "the extension has two dots, so a file_stem reader is wrong rather than merely different"
    );
}
/// A profile name is valid **by construction**, and the check inside
/// `codex_profile_name` is a backstop against somebody else's alphabet moving.
///
/// This test pins the RELATIONSHIP rather than pretending to exercise the `Err`
/// arm, because that arm cannot be reached from a valid `PathSegment` today:
/// `check_segment` accepts ASCII alphanumerics, `_` and `-` and then refuses
/// two things codex would have accepted (a leading `-`, a reserved device
/// name), so it is strictly NARROWER than `ProfileV2Name`'s `FromStr`. Writing
/// a fixture for the `Err` arm would mean constructing a `PathSegment` that
/// cannot exist, and a test that cannot fail is a decoration.
///
/// What CAN change is either alphabet — `check_segment` serves four identifier
/// families and could widen for one of them. When it does, the assertion below
/// reddens and says that the backstop just stopped being decorative.
#[test]
fn a_codex_profile_name_is_valid_by_construction_and_the_check_is_the_backstop() {
    // Every id loomux actually mints, spelled the way `Block::prefix()` plus a
    // sequence number spells it.
    for id in ["w-3", "rev-4", "orch-1", "solo-27", "plan-2212"] {
        let name = codex_profile_name(&seg(id)).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(name, format!("orrerix-{id}"));
    }

    // The subset claim itself, over the whole printable-ASCII range: anything
    // `PathSegment` admits must be something codex's `--profile` admits too.
    // This is what makes the `Err` arm unreachable, so it is the thing to pin.
    for byte in 0x20u8..0x7f {
        let candidate = format!("w{}3", byte as char);
        if PathSegment::parse(&candidate).is_err() {
            continue;
        }
        assert!(
            candidate.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "{candidate:?} is a legal PathSegment but carries a character codex's ProfileV2Name \
             refuses — `codex_profile_name`'s Err arm is now REACHABLE, which is fine, but this \
             test's premise (and its doc) must be rewritten to exercise it rather than to \
             assert it cannot happen"
        );
        assert!(codex_profile_name(&seg(&candidate)).is_ok(), "{candidate}");
    }

    // The non-vacuity control: the sweep above is only meaningful if it
    // actually rejected some characters — an empty loop body would pass.
    assert!(
        PathSegment::parse("w.3").is_err() && PathSegment::parse("w@3").is_err(),
        "this sweep proves nothing unless PathSegment really does refuse some printable ASCII"
    );
}

// ---------------------------------------------------------------------------
// 5. The exposure row
// ---------------------------------------------------------------------------

/// The human's own `[mcp_servers.*]` entries merge into a codex pane's tool
/// surface, so they are reported — and a human who declares none costs no audit
/// row at all (T1.8).
#[test]
fn the_codex_user_config_exposure_row_names_merged_servers_and_is_absent_for_none() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // No file at all: the overwhelmingly common case.
    assert!(codex_user_mcp_exposure(home, "orrerix").is_none());

    // A file with no servers is the SECOND absence, and a different one: the
    // file was read and said nothing. Both must be `None`, or every codex
    // spawn on a machine with an ordinary config.toml writes a noise row.
    fs::write(home.join("config.toml"), "model = \"gpt-5.5\"\n[tui]\nalternate_screen = \"never\"\n")
        .unwrap();
    assert!(
        codex_user_mcp_exposure(home, "orrerix").is_none(),
        "a config with no MCP servers must produce no row"
    );

    fs::write(
        home.join("config.toml"),
        "[mcp_servers.playwright]\ncommand = \"npx\"\n\n[mcp_servers.orrerix]\nurl = \"http://x\"\n",
    )
    .unwrap();
    let row = codex_user_mcp_exposure(home, "orrerix").expect("two servers must produce a row");
    assert_eq!(row["servers"], serde_json::json!(["orrerix", "playwright"]));
    // The DIRECTION, which is the opposite of pi's and is why it has a field:
    // loomux's profile is the later layer, so the user's same-named entry is
    // the one that loses.
    assert_eq!(row["this_pane_displaces_a_user_server_of_the_same_name"], serde_json::json!(true));
}

/// The blind spot, pinned rather than only disclosed: the scan is line-oriented,
/// so an INLINE `mcp_servers` table is invisible to it.
///
/// Without this, the disclosure in `codex_user_mcp_exposure`'s doc could go
/// false — someone widening the scan to a real TOML parse would leave the
/// "absence is not proof" sentence standing while it had stopped being true,
/// and nothing would say so. The pin fails in BOTH directions.
#[test]
fn the_codex_exposure_scan_cannot_see_an_inline_mcp_servers_table_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("config.toml"),
        "mcp_servers = { playwright = { command = \"npx\" } }\n",
    )
    .unwrap();
    assert!(
        codex_user_mcp_exposure(tmp.path(), "orrerix").is_none(),
        "this is the DISCLOSED blind spot, not a bug: the scan matches [mcp_servers.<name>] \
         headers. If this test starts failing because the scan was widened, delete the \
         'absence is not proof' paragraph from its doc in the same commit"
    );
    // The non-vacuity control: the same server declared in the shape the scan
    // DOES read is found, so the assertion above is about the shape rather
    // than about the function never returning anything.
    fs::write(tmp.path().join("config.toml"), "[mcp_servers.playwright]\ncommand = \"npx\"\n")
        .unwrap();
    assert!(codex_user_mcp_exposure(tmp.path(), "orrerix").is_some());
}

// ---------------------------------------------------------------------------
// 6. The posture toggle that is deliberately empty
// ---------------------------------------------------------------------------

/// `single_pane_autopilot_flags("codex")` is empty, and the test says WHY it is
/// empty so the row cannot be "tidied" into the `_` arm.
///
/// The claim is not "codex has no posture" — it has one, and loomux sets it in
/// the profile. The claim is that no part of it goes on the LINE, which is what
/// makes the launch line identical across postures.
#[test]
fn a_solo_codex_panes_posture_is_not_on_its_command_line() {
    assert_eq!(single_pane_autopilot_flags("codex"), "");
    // The control: this function is capable of returning something, so an
    // empty answer for codex is a decision rather than a property of the
    // function.
    assert!(!single_pane_autopilot_flags("claude").is_empty());
    // And the two spellings codex WOULD have used are never emitted, which is
    // the thing a future edit might reach for.
    for cli in ["codex", "claude", "copilot", "gemini", "opencode", "pi"] {
        let flags = single_pane_autopilot_flags(cli);
        assert!(
            !flags.contains("--ask-for-approval") && !flags.contains("--approve-for-me"),
            "{cli} must not carry codex's approval flags: {flags}"
        );
    }
}
