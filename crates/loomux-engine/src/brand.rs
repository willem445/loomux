//! The one place the old product name is still spelled: the `loomux` →
//! `orrerix` **filesystem and environment** compatibility seam (#1153 phase 4).
//!
//! Renaming a product is free right up to the point where the name is also an
//! *identity on someone's disk*. Three of those exist, and they are not the same
//! problem:
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
}
