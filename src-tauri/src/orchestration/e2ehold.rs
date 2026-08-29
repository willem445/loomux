//! Test-only injector for a long registry-lock hold — the mechanism the E2E
//! soak/liveness lane (#1603, plan #1600 §3 Phase 4.1) has to reproduce in
//! order to assert anything about it.
//!
//! ## Why this exists at all
//!
//! Plan #1600 §2.2 is the finding this module answers: every guard in this
//! repo is a *shape* scan, and every one of the beta4/5/6 hangs is a
//! *liveness* bug. A shape scan cannot ask "does the app still accept input
//! after running for a while under load", and the honest reason it never got
//! asked is that nothing in the tree can make the app hold a lock for a long
//! time on purpose. The soak lane's first four assertions need no help; the
//! fifth — *a long hold is in progress and the app still answers* — needs the
//! hold to exist.
//!
//! ## Why it cannot ship enabled
//!
//! Two independent gates, one of them a compile-time one:
//!
//! 1. **`#[cfg(debug_assertions)]`.** The watcher below is compiled only into
//!    a dev-profile build. `Cargo.toml`'s `[profile.release]` does not set
//!    `debug-assertions`, so a release build keeps cargo's default (`false`)
//!    and the only `start` it contains is the empty arm at the bottom of this
//!    file. There is no flag, no environment variable and no file that can
//!    reach the removed code, because it is not in the binary.
//!    `tests/e2ehold_guard.rs` pins that shape.
//! 2. **An explicit opt-in.** Even in a dev build the watcher thread is not
//!    started unless `ORRERIX_E2E_LOCK_HOLD` (or the legacy
//!    `LOOMUX_E2E_LOCK_HOLD`) is exactly `1`. `npm run tauri dev` therefore
//!    behaves as it always has.
//!
//! ## Why a file, not a `#[tauri::command]`
//!
//! `doc/design/e2e-testing.md` states the harness's own goal as "zero new
//! Tauri commands or ACL surface", and the queue-badge spec turned down a
//! test hook for the same reason. A command would have been permanent
//! product surface: a name in `generate_handler!`, an entry in
//! `command_manifest::APP_COMMANDS`, and an ACL grant in
//! `permissions/sets/` — all of them present in a *release* build even with
//! the body cfg'd away. A file under the app-data root costs none of that,
//! and it is strictly better for the test besides: the Playwright process
//! *owns* that directory (`e2e/fixtures.ts` creates it and points
//! `ORRERIX_DATA_DIR` at it), so it can trigger a hold and read back when the
//! lock was actually acquired without going through the very IPC path whose
//! liveness is the thing under test. A probe that has to be answered by the
//! app to tell you the app is stuck is not a probe.
//!
//! ## Protocol
//!
//! - The test writes `<data root>/e2e-lock-hold.request`:
//!   `{"target":"groups","hold_ms":30000}`.
//! - The watcher consumes (deletes) it, acquires that registry mutex, and
//!   writes `<data root>/e2e-lock-hold.state` with `acquired_ms` set, then
//!   sleeps, then rewrites it with `released_ms` set.
//! - The test reads the state file directly off disk. `acquired_ms` present
//!   is the positive control that the hold really happened; `released_ms`
//!   still absent when the liveness assertions finish is the control that the
//!   hold spanned them, rather than having expired before they ran.

use std::sync::Arc;

// Only the dev-profile arms build a state document.
#[cfg(debug_assertions)]
use serde_json::json;

use super::OrchRegistry;

/// Environment-variable suffix (`ORRERIX_` / legacy `LOOMUX_`) that must be
/// exactly `1` before the watcher thread is started at all.
pub const ENV_SUFFIX: &str = "E2E_LOCK_HOLD";

/// Request file, relative to the app-data root. Written by the test,
/// consumed (deleted) by the watcher before the hold begins — so a request is
/// honoured exactly once and a crashed run cannot leave one armed.
pub const REQUEST_FILE: &str = "e2e-lock-hold.request";

/// State file, relative to the app-data root. Rewritten at each transition.
pub const STATE_FILE: &str = "e2e-lock-hold.state";

/// Hard ceiling on a requested hold. A soak spec asks for tens of seconds;
/// this bound exists so a typo in a fixture cannot wedge a dev build for the
/// rest of its life, since nothing can interrupt the hold once it starts.
pub const MAX_HOLD_MS: u64 = 300_000;

/// How often the watcher looks for a request. Small enough that a spec's
/// trigger is prompt, and it only ever runs in a build that was explicitly
/// asked for it.
pub const POLL_MS: u64 = 100;

/// Which registry mutex to hold.
///
/// All three are on `resolve_token`'s path — it locks `by_token`, then
/// `agents`, then (for the caller's `role_hint`) `groups` — and
/// `resolve_token` is what *every* MCP request resolves through, `ping`
/// included. They are also what the polled `orch_group_summary` /
/// `orch_group_usage` reads acquire. So each of them reproduces the beta6
/// mechanism (plan #1600 §1.2) from a different starting point, and the
/// spec can pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Groups,
    Agents,
    ByToken,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::Groups => "groups",
            Target::Agents => "agents",
            Target::ByToken => "by_token",
        }
    }

    pub fn parse(s: &str) -> Option<Target> {
        match s {
            "groups" => Some(Target::Groups),
            "agents" => Some(Target::Agents),
            "by_token" => Some(Target::ByToken),
            _ => None,
        }
    }
}

/// Parses a request document into `(target, hold_ms)`.
///
/// Split out from the watcher because it is the only part of this module with
/// a decision in it, and a fixture that misspells a target should fail the
/// spec with a stated reason rather than by a hold that silently never
/// happens (which reads exactly like a passing liveness assertion — the
/// vacuity CLAUDE.md's positive-control convention is about).
pub fn parse_request(text: &str) -> Result<(Target, u64), String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("request is not JSON: {e}"))?;
    let target_str = v["target"]
        .as_str()
        .ok_or_else(|| "request has no string `target`".to_string())?;
    let target = Target::parse(target_str)
        .ok_or_else(|| format!("unknown target `{target_str}` (groups|agents|by_token)"))?;
    let hold_ms = v["hold_ms"]
        .as_u64()
        .ok_or_else(|| "request has no integer `hold_ms`".to_string())?;
    if hold_ms == 0 {
        return Err("hold_ms must be greater than zero".to_string());
    }
    if hold_ms > MAX_HOLD_MS {
        return Err(format!("hold_ms {hold_ms} exceeds the {MAX_HOLD_MS} ms ceiling"));
    }
    Ok((target, hold_ms))
}

/// Whether the opt-in environment variable arms the injector.
///
/// A pure function rather than an inline comparison so the gate is testable:
/// `#[cfg(debug_assertions)]` is a shape a source scan can check, but
/// "nothing arms it except the exact string `1`" is a behaviour, and a
/// documented gate nobody ever performs the edit against is only a claim
/// (CLAUDE.md, the escape-hatch convention).
pub fn armed(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Starts the request watcher — dev builds only, and only on an explicit
/// opt-in. See the module doc for why both gates are here.
#[cfg(debug_assertions)]
pub fn start(reg: Arc<OrchRegistry>) {
    if !armed(super::brand::env_string(ENV_SUFFIX).as_deref()) {
        return;
    }
    eprintln!("[e2ehold] armed via {}=1; idle until a request file appears", super::brand::env_names(ENV_SUFFIX));
    std::thread::spawn(move || watch(&reg));
}

/// Release arm: the injector does not exist in a shipped build. Keeping the
/// symbol (rather than cfg-ing the call site in `lib.rs`) is the shape
/// `voice.rs` already uses for its non-Windows arms, and it is what keeps
/// `lib.rs` free of a conditional that could drift.
#[cfg(not(debug_assertions))]
pub fn start(_reg: Arc<OrchRegistry>) {}

#[cfg(debug_assertions)]
fn watch(reg: &OrchRegistry) {
    let root = crate::obs::data_root();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        run_once(reg, &root);
    }
}

/// Honours at most one pending request under `root`, BLOCKING for the hold's
/// duration if it finds one. Returns whether there was one.
///
/// Split out of the watcher, and public, for one reason: the E2E spec that
/// uses this injector is an expected failure (`test.fail()`), and that marker
/// absorbs every assertion inside its own test — including the ones checking
/// that the hold really happened. So the proof that a request actually takes
/// the named mutex has to live where no such marker can reach it, which is
/// `tests/e2ehold_guard.rs` calling this directly against a temp root.
#[cfg(debug_assertions)]
pub fn run_once(reg: &OrchRegistry, root: &std::path::Path) -> bool {
    let request = root.join(REQUEST_FILE);
    let state = root.join(STATE_FILE);
    let Ok(text) = std::fs::read_to_string(&request) else { return false };
    // Consume before acting: a request is honoured exactly once, and a hold
    // that panics cannot leave the file behind to be re-honoured on the next
    // tick.
    let _ = std::fs::remove_file(&request);
    match parse_request(&text) {
        Ok((target, hold_ms)) => hold(reg, &state, target, hold_ms),
        Err(e) => {
            write_state(&state, &json!({ "error": e, "requested_ms": super::now_ms() }));
        }
    }
    true
}

#[cfg(debug_assertions)]
fn hold(reg: &OrchRegistry, state: &std::path::Path, target: Target, hold_ms: u64) {
    use crate::obs::LockExt;

    let requested_ms = super::now_ms();
    write_state(
        state,
        &json!({
            "target": target.as_str(),
            "hold_ms": hold_ms,
            "requested_ms": requested_ms,
            "acquired_ms": serde_json::Value::Null,
            "released_ms": serde_json::Value::Null,
        }),
    );

    // One arm per target rather than a trait object: the three guards have
    // three different types, and the guard has to stay alive across the sleep
    // — which is the whole point.
    let acquired_ms = match target {
        Target::Groups => {
            let _g = reg.groups.lock_safe();
            sleep_holding(state, target, hold_ms, requested_ms)
        }
        Target::Agents => {
            let _g = reg.agents.lock_safe();
            sleep_holding(state, target, hold_ms, requested_ms)
        }
        Target::ByToken => {
            let _g = reg.by_token.lock_safe();
            sleep_holding(state, target, hold_ms, requested_ms)
        }
    };

    write_state(
        state,
        &json!({
            "target": target.as_str(),
            "hold_ms": hold_ms,
            "requested_ms": requested_ms,
            "acquired_ms": acquired_ms,
            "released_ms": super::now_ms(),
        }),
    );
}

/// Stamps `acquired_ms` (the test's positive control that the lock really was
/// taken) and then sleeps out the hold. Called with the guard alive on the
/// caller's stack.
#[cfg(debug_assertions)]
fn sleep_holding(
    state: &std::path::Path,
    target: Target,
    hold_ms: u64,
    requested_ms: u64,
) -> u64 {
    let acquired_ms = super::now_ms();
    write_state(
        state,
        &json!({
            "target": target.as_str(),
            "hold_ms": hold_ms,
            "requested_ms": requested_ms,
            "acquired_ms": acquired_ms,
            "released_ms": serde_json::Value::Null,
        }),
    );
    std::thread::sleep(std::time::Duration::from_millis(hold_ms));
    acquired_ms
}

/// Write-then-rename, because the reader is a separate OS process polling the
/// same path: a plain truncating write is observable half-finished, and the
/// spec would read it as malformed JSON at exactly the moment it matters.
#[cfg(debug_assertions)]
fn write_state(state: &std::path::Path, value: &serde_json::Value) {
    let tmp = state.with_extension("state.tmp");
    if std::fs::write(&tmp, value.to_string().as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, state);
    }
}
