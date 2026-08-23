//! The one place the old product name is still spelled: the `loomux` →
//! `orrerix` **filesystem, environment and protocol** compatibility seam
//! (#1153 phases 3 and 4).
//!
//! Renaming a product is free right up to the point where the name is also an
//! *identity somebody else already holds a copy of* — on their disk, in their
//! shell profile, or in a transcript and a generated config this app itself
//! wrote months ago. Four of those exist, and they are not the same problem:
//!
//! 1. **The app's own data root** (`<platform data dir>/loomux`) — ours, written
//!    only by us, and the one place a one-time move is defensible. The policy,
//!    its hazard and its escape hatch are argued in
//!    `doc/design/rebrand-filesystem.md`; the decision itself is
//!    [`obs::plan_default_root`](crate::obs::plan_default_root), deliberately a
//!    pure function so the policy is one `match` arm to change — its
//!    `(false, true)` arm, `Migrate` → `UseLegacy`, and nothing else. That the
//!    edit really works is pinned by
//!    `obs::tests::the_documented_revert_really_stops_the_migration`, because
//!    the first cut of it did not (see [`obs::RootPlan`](crate::obs::RootPlan)).
//! 2. **A repo's committed config dir** (`.loomux/`) — *the user's*, in *their*
//!    git history, on *their* branches. Never moved, never renamed, not once:
//!    the preferred name is read first and the legacy name is read when the
//!    preferred one is absent, forever, and a repo that keeps `.loomux/`
//!    keeps working with no action and no nag. See [`pick_repo_path`].
//! 3. **Environment variables** — an operator's shell profiles, CI configs and
//!    scripts, which we cannot edit and must not break. `ORRERIX_X` is preferred
//!    and `LOOMUX_X` is the fallback. See [`pick_env`].
//! 4. **Protocol identities** — the notice marker, the MCP server name and
//!    token header, the audit actor: what *agents* match on. Not a file and
//!    not a variable, but the same shape of problem under a stricter rule,
//!    for the reason argued at [`NOTICE_MARKERS`] — one spelling emitted,
//!    every spelling accepted, and the accepted set written down exactly
//!    once (#1153 phase 3).
//!
//! Every decision here is a pure function over "what exists / what is set", so
//! the *policy* is testable without a disk or a mutated process environment
//! (`std::env::set_var` is both `unsafe` and cross-thread global — not something
//! to reach for to test a two-branch rule). The thin readers that supply the
//! real inputs sit beside each pure function and carry no logic of their own.
//!
//! **This module is meant to shrink and eventually die.** Every `LEGACY_`
//! constant below is a deprecation, and deleting one is a deliberate,
//! separately-argued break of somebody's working setup — not tidying.

use std::ffi::OsString;
use std::path::Path;

/// The product's directory/dir-prefix identity today.
pub const NAME: &str = "orrerix";

/// What it was called before #1153. Still read everywhere `NAME` is.
pub const LEGACY_NAME: &str = "loomux";

/// A repo's committed config dir — `<repo>/.orrerix/`, holding `workflow.yml`,
/// `lessons.md` and the canvas's `workflow.layout.json`.
pub const CONFIG_DIR: &str = ".orrerix";

/// The pre-#1153 spelling of [`CONFIG_DIR`]. Read when `.orrerix/` is absent —
/// permanently, and it is *never* renamed on the user's behalf: it is a
/// tracked directory in their repository, so "migrating" it would mean writing
/// a commit-shaped change into a working tree we do not own, on a branch we did
/// not pick, in the middle of whatever they were doing.
pub const LEGACY_CONFIG_DIR: &str = ".loomux";

/// Prefix of every environment variable this app reads.
pub const ENV_PREFIX: &str = "ORRERIX_";

/// The pre-#1153 environment prefix, still honoured as a fallback.
pub const LEGACY_ENV_PREFIX: &str = "LOOMUX_";

// ---------- environment ----------

/// Which name an environment value came from — so a caller that wants to say
/// "you are using the old name" can, without reading the environment twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvPick {
    /// The value, or `None` when neither name is set.
    pub value: Option<OsString>,
    /// True when the value came from the `LOOMUX_` name.
    pub from_legacy: bool,
}

/// Choose between the preferred and legacy readings of one variable.
///
/// **The rule is presence, on both sides alike**: if the `ORRERIX_` name is set
/// *at all* it wins, and the `LOOMUX_` name is consulted only when it is not.
/// Deliberately not "the first non-empty one" — that would make the two names
/// obey different rules depending on their contents, and an operator who sets
/// `ORRERIX_DATA_DIR=` to *deliberately* blank out an inherited `LOOMUX_DATA_DIR`
/// (a normal thing to do in a CI job or a wrapper script) would silently get
/// the inherited value back. Empty and malformed values are the *consumer's*
/// business — `obs::data_root_from` already rejects an empty or relative data
/// dir and says so — and this function must not pre-empt that by quietly
/// substituting a different variable's value for the one that was set.
pub fn pick_env(preferred: Option<OsString>, legacy: Option<OsString>) -> EnvPick {
    match preferred {
        Some(v) => EnvPick { value: Some(v), from_legacy: false },
        None => EnvPick { from_legacy: legacy.is_some(), value: legacy },
    }
}

/// [`pick_env`] against the real process environment, for the variable whose
/// name is `ORRERIX_{suffix}` / `LOOMUX_{suffix}`.
pub fn env_os(suffix: &str) -> EnvPick {
    pick_env(
        std::env::var_os(format!("{ENV_PREFIX}{suffix}")),
        std::env::var_os(format!("{LEGACY_ENV_PREFIX}{suffix}")),
    )
}

/// [`env_os`] as lossy UTF-8, for the variables that are read as text
/// (a flag, an argument string) rather than as a path.
pub fn env_string(suffix: &str) -> Option<String> {
    env_os(suffix).value.map(|v| v.to_string_lossy().into_owned())
}

/// Both spellings of one variable, `ORRERIX_` first — for error and help text
/// that has to name what the reader should actually set. Naming only the new
/// one would make a message about a *legacy* value name a variable the user
/// has not set; naming only the old one would advertise the deprecated name as
/// the answer.
pub fn env_names(suffix: &str) -> String {
    format!("{ENV_PREFIX}{suffix} (or {LEGACY_ENV_PREFIX}{suffix})")
}

// ---------- a repo's committed config dir ----------

/// Which spelling of a repo-relative config path a given repo actually uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoPick {
    /// `.orrerix/…` — present, or the name to create when neither is.
    Preferred,
    /// `.loomux/…` — present, and the preferred name is not.
    Legacy,
}

/// The dual-discovery rule, stated once: **the legacy path wins only when it is
/// the only one there.**
///
/// The tie-break matters more than it looks. A repo with *both* directories is
/// mid-migration (a human copied the files, or a branch merged), and there the
/// preferred name is the one to obey — otherwise adding `.orrerix/workflow.yml`
/// to a repo would have no effect until the author also deleted `.loomux/`,
/// which is precisely the "why is my edit being ignored" failure a fallback is
/// supposed to prevent. And when *neither* exists we still answer `Preferred`,
/// because the answer is then "the path a repo should create", and that is the
/// new name.
pub fn pick_repo_path(preferred_exists: bool, legacy_exists: bool) -> RepoPick {
    if legacy_exists && !preferred_exists {
        RepoPick::Legacy
    } else {
        RepoPick::Preferred
    }
}

/// [`pick_repo_path`] against a real repo, for a `preferred`/`legacy` pair of
/// repo-relative *file* paths — returning the one this repo uses.
///
/// Both candidates are `&'static str` because both are consts: the answer is a
/// path that can be stored, compared and printed as cheaply as the constants it
/// chooses between, which is what lets every display site ("this repo declares
/// a workflow (`…`)") name the file that was *actually read* instead of a
/// hard-coded guess that is wrong for half the world.
pub fn resolve_repo_file(
    repo: &str,
    preferred: &'static str,
    legacy: &'static str,
) -> &'static str {
    let root = Path::new(repo);
    match pick_repo_path(root.join(preferred).is_file(), root.join(legacy).is_file()) {
        RepoPick::Preferred => preferred,
        RepoPick::Legacy => legacy,
    }
}

// ---------- protocol identities (#1153 phase 3) ----------
//
// These are the spellings AGENTS see and match on: the marker every notice
// opens with, the MCP server they call tools on, the header their CLI presents
// to it, and the actor this app signs its own audit and queue records with.
// They differ from the two sections above in what "legacy" costs. A stale
// `.loomux/` is a file we can still find; a stale `[loomux]` is a *recorded
// transcript*, an *already-generated* CLI config on disk and a *pane already
// briefed* — none of which we can rewrite. So the rule here is stricter than
// dual-discovery: emit exactly one spelling, accept both on every reading
// surface, and never let the two sets be written down twice.

/// The marker every notice this app types into a pane opens with.
///
/// Protocol, not decoration. Three surfaces turn on it: the role templates
/// teach an agent to recognise it, `mask_loomux_notices` (`orchestration`)
/// finds notice rows in a pane tail by it, and [`notify::sanitize_pane_text`]
/// neutralizes it in untrusted text so nothing outside this app can forge a
/// notice-shaped row.
///
/// Lowercase, and it must stay that way: the detector lowercases the row
/// before comparing, so a capital letter here matches nothing — and fails
/// *open*, with no compile error.
///
/// **Unforgeable only in the delivery direction, and that asymmetry is the
/// whole design constraint** (carried here from this const's previous home in
/// `text`). Nothing an agent sends *through* this app can carry the marker:
/// `notify::sanitize_gh_text` rewrites `[`→`(` in every untrusted field
/// before it is formatted into a notice, and `intake`'s own test pins that a
/// third-party issue title can never produce this string. But an agent's pane
/// *output* is not sanitized at all, so an agent can print these bytes itself
/// — echoing a notice back, quoting one in a summary, or induced to by a
/// hostile prompt. A marker row is therefore evidence that *someone wrote a
/// notice-shaped row*, never proof that this app wrote this one, and
/// `mask_loomux_notices` is scoped to exactly what that weaker claim supports.
///
/// [`notify::sanitize_pane_text`]: crate::notify::sanitize_pane_text
pub const NOTICE_MARKER: &str = "[orrerix]";

/// The pre-#1153 marker. **Accepted forever, on every reading surface** —
/// and, unlike the other `LEGACY_` constants here, not merely for the user's
/// convenience. Two independent reasons, either sufficient:
///
/// - **Recorded transcripts are parsed for the rest of time.** Session restore
///   scrapes a pane's *already-written* output for an orchestration signature
///   ([`crate::sessions::detect_orch_signature`]). Every transcript recorded
///   before this rename carries the old marker, and a reader that stopped
///   accepting it would silently strip the orchestration identity off every
///   session a user already has.
/// - **Neutralizing it is a security control.** The forgery guard rewrites
///   `[`→`(` in untrusted text precisely so a hostile issue title cannot
///   produce a row an agent reads as a host notice. An agent briefed before
///   the rename still treats `[loomux] …` as one, so a sanitizer that stopped
///   neutralizing the old marker would reopen forgery against exactly the
///   agents with no way to know the name had changed.
pub const LEGACY_NOTICE_MARKER: &str = "[loomux]";

/// **Every** marker a reader must recognise and a sanitizer must neutralize —
/// one array, iterated by both sides.
///
/// The alternative is two lists that happen to agree today: a detector's
/// `starts_with` chain and a sanitizer's neutralize set. They only have to
/// disagree once, and the disagreement is not a compile error — it is a marker
/// one side treats as a host notice while the other leaves un-scrubbed, which
/// *is* the forgery hole. `every_accepted_marker_is_also_neutralized` asserts
/// the two sides against this array rather than against a list written down in
/// a test, so adding a third spelling to one side alone cannot pass.
pub const NOTICE_MARKERS: [&str; 2] = [NOTICE_MARKER, LEGACY_NOTICE_MARKER];

/// The marker `text` opens with, in any accepted spelling — `None` if it opens
/// with none. Case-insensitive, matching the detector's own contract.
///
/// The caller strips its own framing first (`orchestration::deframe`); this
/// function knows about markers and nothing else.
pub fn leading_notice_marker(text: &str) -> Option<&'static str> {
    let head = text.trim_start().to_lowercase();
    NOTICE_MARKERS.into_iter().find(|m| head.starts_with(m))
}

/// The MCP server name this app declares to every agent CLI — and so the
/// `mcp__orrerix__*` tool prefix an agent actually types.
///
/// Deliberately an alias of [`NAME`] rather than its own literal. The rename
/// is only atomic because one word drives the server-map key, the generated
/// CLI allowlists and the tool prefix together; spelling it a second time here
/// would let those drift, and a drifted allowlist is an agent whose every tool
/// call is denied. If they ever must differ, that is a deliberate change with
/// its own argument — not a typo this file quietly absorbed.
pub const MCP_SERVER: &str = NAME;

/// The pre-#1153 server name, kept so the deprecation has a name in Rust and
/// so the emit-side scan can ban what the app still accepts.
///
/// The reader that actually needs it is **not** in this language: tab restore
/// parses a launch command captured months ago, in `src/panerestore.ts`, which
/// carries its own copy of the accepted set because no Rust constant crosses
/// that boundary. `rebrand.rs`'s
/// `the_frontend_accepts_every_mcp_identity_the_backend_still_mints` is what
/// keeps the two from diverging — stated here rather than left as an
/// implication, because a doc claiming a coupling that does not exist is how
/// the next reader deletes one side believing the other follows.
pub const LEGACY_MCP_SERVER: &str = LEGACY_NAME;

/// Every server spelling a **repo-authored** `tools:` list may name, in the
/// order a reader should try them. Same one-array discipline as
/// [`NOTICE_MARKERS`], and it exists for the same reason: a persona file was
/// written before the flag day and nobody is going to rewrite it for its
/// author.
///
/// **This set answers "what did the author NAME", never "what may the agent
/// HAVE"** — and the distinction is the whole of rev-967 B1. Reading a stale
/// `loomux/*` as a live grant would leave a delegate holding a scope that
/// matches no server this app declares; reading a stale `loomux/report` as
/// *nothing* lets the repair path widen a deliberate one-tool scope into the
/// whole server. Callers use it for the first question only, and
/// `orchestration::ResolvedPersona` documents which of its two predicates
/// takes it and why the other must not.
pub const MCP_SERVERS: [&str; 2] = [MCP_SERVER, LEGACY_MCP_SERVER];

/// The tool-name prefix an agent CLI builds out of [`MCP_SERVER`] — what
/// claude's `--allowedTools` takes on argv, and what an agent actually types.
///
/// Spelled as a literal because it has to be: `concat!` takes literals, not
/// consts, so `"mcp__" + MCP_SERVER` cannot be written as a `const`. What
/// makes it a DERIVATION rather than a second driftable spelling is
/// `the_tool_prefixes_are_derived_from_the_server_names`, which computes it
/// and compares — and it matters here more than the usual amount, because a
/// prefix that disagrees with the server map is not a wrong string, it is an
/// allowlist that denies every tool call the agent makes.
pub const MCP_TOOL_PREFIX: &str = "mcp__orrerix";

/// The pre-#1153 tool prefix. Read, never written: it is what an
/// already-recorded launch command line says, and tab restore has to
/// recognise its own past output.
pub const LEGACY_MCP_TOOL_PREFIX: &str = "mcp__loomux";

/// The token header each agent CLI's generated MCP config presents to the
/// orchestration server, and the one this server issues going forward.
pub const AGENT_TOKEN_HEADER: &str = "X-Orrerix-Agent";

/// The pre-#1153 token header. **The server must keep accepting it**, and this
/// one is not optional in the way a filesystem fallback is: an agent's MCP
/// config is written once, at group create, and lives in that group's dir. A
/// group created before the rename presents the old header on every call it
/// will ever make, so a server that only read the new one would fail every
/// tool call in every live group the moment the app updated under it.
pub const LEGACY_AGENT_TOKEN_HEADER: &str = "X-Loomux-Agent";

/// Both header spellings, in the order a reader should try them. Same
/// one-array discipline as [`NOTICE_MARKERS`].
pub const AGENT_TOKEN_HEADERS: [&str; 2] = [AGENT_TOKEN_HEADER, LEGACY_AGENT_TOKEN_HEADER];

/// The `actor` this app signs its own audit records with, and the `from` on
/// every delivery it sends rather than relays.
///
/// An alias of [`NAME`] for the same reason [`MCP_SERVER`] is: it is the
/// product's name appearing in a record, not an independent identifier.
pub const AUDIT_ACTOR: &str = NAME;

/// The pre-#1153 actor. Every `audit.jsonl` and every queued delivery written
/// before the rename carries it, on disk, unrewritable — so a matcher that
/// asks "did *we* write this?" must accept it forever. See [`is_host_actor`].
pub const LEGACY_AUDIT_ACTOR: &str = LEGACY_NAME;

/// Did this app write the record signed `actor` — under either spelling?
///
/// One predicate, because the question is asked from both sides of the rename
/// and an `== AUDIT_ACTOR` written out by hand at one of the call sites is a
/// record from before the flag day silently reclassified as somebody else's.
pub fn is_host_actor(actor: &str) -> bool {
    actor == AUDIT_ACTOR || actor == LEGACY_AUDIT_ACTOR
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn the_preferred_env_name_wins_when_both_are_set() {
        let pick = pick_env(Some(os("new")), Some(os("old")));
        assert_eq!(pick.value, Some(os("new")));
        assert!(!pick.from_legacy);
    }

    #[test]
    fn the_legacy_env_name_is_read_when_the_preferred_one_is_absent() {
        let pick = pick_env(None, Some(os("old")));
        assert_eq!(pick.value, Some(os("old")));
        assert!(pick.from_legacy, "the caller must be able to say the old name was used");
    }

    #[test]
    fn neither_env_name_set_is_none_and_not_legacy() {
        let pick = pick_env(None, None);
        assert_eq!(pick.value, None);
        assert!(!pick.from_legacy);
    }

    /// The rule is PRESENCE, not non-emptiness: a deliberately-blanked
    /// `ORRERIX_X` must not fall through to an inherited `LOOMUX_X`. If this
    /// ever goes green with `Some(os("old"))` the two names have stopped
    /// obeying one rule and a wrapper script's blanking has become a no-op.
    #[test]
    fn an_empty_preferred_env_value_does_not_fall_back() {
        let pick = pick_env(Some(os("")), Some(os("old")));
        assert_eq!(pick.value, Some(os("")));
        assert!(!pick.from_legacy);
    }

    #[test]
    fn env_names_offers_both_spellings_new_one_first() {
        let names = env_names("DATA_DIR");
        assert_eq!(names, "ORRERIX_DATA_DIR (or LOOMUX_DATA_DIR)");
    }

    #[test]
    fn the_legacy_config_dir_wins_only_when_it_is_the_only_one_there() {
        assert_eq!(pick_repo_path(false, true), RepoPick::Legacy);
    }

    #[test]
    fn a_repo_with_both_config_dirs_obeys_the_preferred_one() {
        assert_eq!(pick_repo_path(true, true), RepoPick::Preferred);
    }

    #[test]
    fn a_repo_with_neither_config_dir_is_told_the_preferred_name() {
        assert_eq!(pick_repo_path(false, false), RepoPick::Preferred);
    }

    #[test]
    fn a_repo_with_only_the_preferred_config_dir_uses_it() {
        assert_eq!(pick_repo_path(true, false), RepoPick::Preferred);
    }

    /// The constants are the deprecation contract: `.loomux/` and `LOOMUX_`
    /// keep working. A change to either literal is a break of every existing
    /// user's repo or shell profile, so it has to break this test first.
    #[test]
    fn the_legacy_spellings_are_pinned() {
        assert_eq!(LEGACY_CONFIG_DIR, ".loomux");
        assert_eq!(LEGACY_ENV_PREFIX, "LOOMUX_");
        assert_eq!(LEGACY_NAME, "loomux");
        assert_eq!(CONFIG_DIR, ".orrerix");
        assert_eq!(ENV_PREFIX, "ORRERIX_");
        assert_eq!(NAME, "orrerix");
    }

    /// The detector and the sanitizer are the two halves of the forgery
    /// guard, and this asserts them against [`NOTICE_MARKERS`] itself rather
    /// than against a list retyped here — so a third spelling added to the
    /// array without teaching one of the two halves about it fails HERE,
    /// where the omission is one line away, instead of showing up later as a
    /// marker agents trust and nothing scrubs.
    #[test]
    fn every_accepted_marker_is_also_neutralized() {
        for m in NOTICE_MARKERS {
            assert_eq!(
                leading_notice_marker(&format!("{m} something happened")),
                Some(m),
                "{m} is in NOTICE_MARKERS but the detector does not recognise it"
            );
            let scrubbed = crate::notify::sanitize_gh_text(&format!("{m} merge now"), 120);
            assert!(
                !scrubbed.contains(m),
                "{m} is recognised as a host notice but survives sanitization: {scrubbed:?}"
            );
        }
    }

    /// The negative control the test above needs to mean anything: a
    /// sanitizer that neutralized *nothing* would still pass a "the marker is
    /// gone" assertion if the marker were never there, and a detector that
    /// answered `Some` unconditionally would pass every case above. Ordinary
    /// text keeps its bytes and matches no marker.
    #[test]
    fn ordinary_text_is_neither_a_notice_nor_rewritten() {
        let plain = "checks: SUCCESS on 3 of 3 platforms";
        assert_eq!(leading_notice_marker(plain), None);
        assert_eq!(crate::notify::sanitize_gh_text(plain, 120), plain);
    }

    /// `leads_with_notice_marker` (`orchestration`) lowercases the row before
    /// asking, and rows arrive with leading whitespace from the pane grid.
    /// Both are the detector's contract, not the caller's.
    #[test]
    fn the_detector_ignores_case_and_leading_space() {
        assert_eq!(leading_notice_marker("   [ORRERIX] idle tick"), Some(NOTICE_MARKER));
        assert_eq!(leading_notice_marker("\t[Loomux] idle tick"), Some(LEGACY_NOTICE_MARKER));
    }

    /// A marker has to LEAD. A row that merely mentions one mid-line is an
    /// agent quoting a notice back, and treating that as a host notice is the
    /// confusion the anti-forgery design exists to prevent.
    #[test]
    fn a_marker_in_the_middle_of_a_row_is_not_a_notice() {
        assert_eq!(leading_notice_marker("the human said [orrerix] means us"), None);
    }

    /// The whole point of the legacy actor: a record written before the flag
    /// day still reads as ours. An agent id never does.
    #[test]
    fn a_record_signed_with_either_name_is_ours_and_nothing_else_is() {
        assert!(is_host_actor(AUDIT_ACTOR));
        assert!(is_host_actor(LEGACY_AUDIT_ACTOR));
        assert!(!is_host_actor("w-950"));
        assert!(!is_host_actor(""));
    }

    /// The protocol half of the deprecation contract, pinned as literals for
    /// the same reason the filesystem half above is: every one of these is a
    /// string somebody else already holds a copy of — in a transcript, in a
    /// generated MCP config, in an `audit.jsonl` — and changing one is a break
    /// of that copy, not a rename.
    #[test]
    fn the_legacy_protocol_spellings_are_pinned() {
        assert_eq!(LEGACY_NOTICE_MARKER, "[loomux]");
        assert_eq!(LEGACY_AGENT_TOKEN_HEADER, "X-Loomux-Agent");
        assert_eq!(LEGACY_MCP_SERVER, "loomux");
        assert_eq!(LEGACY_AUDIT_ACTOR, "loomux");
        assert_eq!(NOTICE_MARKER, "[orrerix]");
        assert_eq!(AGENT_TOKEN_HEADER, "X-Orrerix-Agent");
        assert_eq!(MCP_SERVER, "orrerix");
        assert_eq!(AUDIT_ACTOR, "orrerix");
    }

    /// The two tool prefixes are DERIVED, and this is the derivation — the
    /// literals above cannot be written any other way, so this is the only
    /// thing standing between them and a silent disagreement with the server
    /// map that would deny every tool call an agent makes.
    #[test]
    fn the_tool_prefixes_are_derived_from_the_server_names() {
        assert_eq!(MCP_TOOL_PREFIX, format!("mcp__{MCP_SERVER}"));
        assert_eq!(LEGACY_MCP_TOOL_PREFIX, format!("mcp__{LEGACY_MCP_SERVER}"));
    }

    /// The marker is compared against a lowercased row, so an upper-case
    /// letter in the constant matches nothing and fails OPEN — no compile
    /// error, no red anywhere else, just a notice nobody masks.
    #[test]
    fn every_marker_is_lowercase() {
        for m in NOTICE_MARKERS {
            assert_eq!(m, &m.to_lowercase(), "a marker must be lowercase to be matchable");
        }
    }
}
