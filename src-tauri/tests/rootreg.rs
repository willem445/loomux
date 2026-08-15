//! The declared-root registry's **admit** side — who may mint a declared root
//! (#1042 slice B; layer 2 of #888 §0, the other half of `tests/groupid.rs`).
//!
//! Integration tests, not unit tests: they link the full lib, which on Windows
//! needs the comctl32-v6 manifest `build.rs` embeds via `-tests`-scoped link
//! args (CLAUDE.md constraint 4). The registry MECHANISM has its own unit tests
//! next to it in `crates/loomux-engine/src/rootreg.rs`, where no lib link is
//! needed; nothing here re-tests containment, the descendant rule or
//! canonicalization. What is tested here is the half slice A could not reach:
//! **what actually puts a root in the registry.**
//!
//! Three parts, and the third is the one that matters most:
//!
//! 1. **The host wrapper** — the error mapping every admit site shares, so a
//!    refusal reaches a caller in the `code: message` shape the file commands
//!    already answer in.
//! 2. **Engine-derived declaration** — a group's checkout is declared as the
//!    group is created AND as it is resumed, and an agent's worktree as it is
//!    cut. With a negative control: a directory nobody declared does not
//!    resolve, without which every assertion above would also pass against a
//!    registry that simply said yes.
//! 3. **The source scans** — `DeclaredRoot` has no `AsRef<Path>` (the carry
//!    from #1055's review), and *every* admit site in the workspace is on an
//!    argued allowlist. `src-tauri/src/rootreg.rs`'s module doc makes a claim
//!    in one sentence — "these are all the ways a root gets declared" — and
//!    part 3 is the thing that keeps that sentence true after the next feature
//!    lands. Default-deny with one row per permitted site, each carrying its
//!    reason, and a stale row fails as loudly as a new site does.
//!
//! What is **not** testable in this slice, stated rather than glossed: that a
//! wire caller cannot admit. The enforcement for that is the listener's
//! default-deny dispatcher (C2), which does not exist yet — `admit_root`'s
//! `disabled` classification is written down (its own doc comment, and
//! `doc/design/groupid-and-path-roots.md`) and C2's roster test is what will
//! pin it. Claiming a test for it here would be claiming coverage this slice
//! does not have.

use loomux_lib::orchestration::{workflow, Guardrails, OrchRegistry, Role};
use loomux_lib::rootreg::admit;

/// A registry rooted in a disposable tree, with every agent-file override
/// pointed at it (#464 — a bare `OrchRegistry::new` writes generated
/// custom-agent files into the developer's REAL `~/.claude/agents` on its first
/// spawn). This file's ONE sanctioned construction site, pinned by
/// `no_registry_construction_bypasses_the_test_agent_dir_overrides` in
/// `tests/orchestration.rs`.
fn registry_at(root: &std::path::Path) -> OrchRegistry {
    let reg = OrchRegistry::new(root.to_path_buf());
    reg.set_port(45997);
    reg.set_claude_agents_dir_override(root.join("claude-agents"));
    reg.set_copilot_agents_dir_override(root.join("copilot-agents"));
    reg.set_compact_hook_dir_override(root.join("compacthook"));
    reg.set_copilot_hooks_dir_override(root.join("copilot-hooks"));
    reg
}

/// A registry over a fresh disposable state tree. `registry_at` is separate
/// because the resume path is only reachable by pointing a SECOND registry at an
/// existing tree — a relaunch — and both must apply the same overrides.
fn test_registry() -> (OrchRegistry, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry_at(dir.path());
    (reg, dir)
}

fn rails() -> Guardrails {
    Guardrails {
        max_agents: 2,
        agent_cli: "claude".into(),
        blocks: workflow::default_roster(&[
            (Role::Orchestrator, "", "opus"),
            (Role::Worker, "", "sonnet"),
            (Role::Reviewer, "", "sonnet"),
            (Role::Planner, "", "opus"),
        ]),
        auto_ops: false,
        idle_kill_minutes: 0,
        max_spawns_per_hour: 0,
        watchdog_stall_minutes: 0,
        ..Guardrails::default()
    }
}

fn s(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

// ---------- 1. the host wrapper's error mapping ----------

/// The refusals a caller can actually hit at an admit site, in the shape the
/// callers' existing probes already answer in.
///
/// This is what lets `admitRoot` REPLACE nothing and sit BEFORE everything on
/// the frontend: a typo'd or deleted folder keeps failing as `not-found`, which
/// is what `ft_list_dir` answers for the same path today, so no admit site had
/// to invent a new error path for slice B.
#[test]
fn the_admit_wrapper_answers_in_the_file_commands_error_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir(&dir).unwrap();
    let file = tmp.path().join("notadir.txt");
    std::fs::write(&file, b"x").unwrap();

    let reg = loomux_lib::orchestration::RootRegistry::new();

    assert_eq!(Ok(()), admit(&reg, &s(&dir)));
    // Idempotent, and the second call is not a different answer.
    assert_eq!(Ok(()), admit(&reg, &s(&dir)));
    assert_eq!(1, reg.declared_count());

    let not_a_dir = admit(&reg, &s(&file)).expect_err("a file is not a root");
    assert!(
        not_a_dir.starts_with("not-found: "),
        "a non-directory must keep answering `not-found`, the code `ft_list_dir` \
         already gives for the same path — got {not_a_dir:?}"
    );

    let relative = admit(&reg, "repo").expect_err("a relative path is not a root");
    assert!(
        relative.starts_with("invalid-path: "),
        "a relative root resolves against a process cwd nobody declared — got {relative:?}"
    );

    // Nothing above declared anything new.
    assert_eq!(1, reg.declared_count());
}

// ---------- 2. engine-derived declaration ----------

/// Creating a group declares its checkout, and the registry the commands see is
/// the one the group creation populated.
///
/// The negative control is the load-bearing half: `an_unrelated_dir` is a real,
/// existing directory that no source declared, and it must NOT resolve. Without
/// it, a `resolve` that answered `Ok` for everything would satisfy every other
/// assertion in this file.
#[test]
fn create_group_declares_the_group_checkout_and_nothing_else() {
    let (reg, _d) = test_registry();
    let repo = tempfile::tempdir().unwrap();
    let deep = repo.path().join("src").join("nested");
    std::fs::create_dir_all(&deep).unwrap();
    let unrelated = tempfile::tempdir().unwrap();

    let roots = reg.roots();
    assert!(
        roots.resolve(&s(repo.path())).is_err(),
        "nothing may be declared before the group exists — otherwise the assertion \
         below proves nothing about `create_group`"
    );

    reg.create_group(&s(repo.path()), rails()).unwrap();

    assert!(
        roots.resolve(&s(repo.path())).is_ok(),
        "creating a group must declare its checkout — an agent pane opened in it \
         can otherwise read nothing once slice C enforces"
    );
    assert!(
        roots.resolve(&s(&deep)).is_ok(),
        "a subdirectory of the declared checkout must resolve (the descendant rule) \
         — this is what keeps a pane that `cd`s around inside its repo alive"
    );
    assert!(
        roots.resolve(&s(unrelated.path())).is_err(),
        "an existing directory nobody declared must NOT resolve — this is the \
         negative control, and without it every other assertion here would also \
         pass against a registry that said yes to everything"
    );
}

/// The RESUME path declares too, and it is a genuinely separate crossing: the
/// registry is never persisted (by design — a persisted one would be
/// replay-poisonable), so a relaunch starts with an empty registry and the
/// group's checkout has to be re-declared from the state on disk rather than
/// read back from a file.
///
/// Reached the way a relaunch reaches it — a SECOND registry over the same state
/// root — so what is exercised is the resume branch of `create_group_ex`, not a
/// second create.
#[test]
fn resuming_a_group_redeclares_its_checkout_into_a_fresh_registry() {
    let (reg, state) = test_registry();
    let repo = tempfile::tempdir().unwrap();
    let created = reg.create_group(&s(repo.path()), rails()).unwrap();

    // The relaunch: new process, new registry, nothing inherited from disk.
    let relaunched = registry_at(state.path());
    let roots = relaunched.roots();
    assert!(
        roots.resolve(&s(repo.path())).is_err(),
        "a fresh registry must start empty — the registry is deliberately never \
         persisted, and a relaunch that inherited declarations would be exactly \
         the replay-poisonable artifact that design avoids"
    );

    let resumed = relaunched.create_group(&s(repo.path()), rails()).unwrap();
    assert_eq!(
        created.id, resumed.id,
        "the second call must RESUME the same group, not create a second one — \
         otherwise this test is a duplicate of the create-path one above"
    );
    assert!(
        roots.resolve(&s(repo.path())).is_ok(),
        "resuming a group must re-declare its checkout"
    );
}

// ---------- 3. the source scans ----------

fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Every PRODUCTION source root in the workspace, labelled by crate — two crates
/// can hold a `mod.rs`, and a bare file name would no longer say which one.
///
/// The engine root is not symmetry. It is the crate that DEFINES `DeclaredRoot`,
/// so by the orphan rule it is the only crate an `impl AsRef<Path> for
/// DeclaredRoot` can be written in at all; leaving it out would make the
/// `AsRef<Path>` assertion unfalsifiable.
const ROOTS: &[(&str, &str)] = &[
    ("src-tauri", concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
    (
        "loomux-engine",
        concat!(env!("CARGO_MANIFEST_DIR"), "/../crates/loomux-engine/src"),
    ),
];

/// Every production `.rs` in the workspace as `(name, path)`, where `name` is
/// `<crate>/<path relative to that crate's source root>` — a full relative path
/// rather than a bare file name, so two crates' (or two modules') `mod.rs`
/// cannot silently share one allowlist row.
fn production_sources() -> Vec<(String, std::path::PathBuf)> {
    let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
    for (label, root) in ROOTS {
        let mut found = Vec::new();
        collect_rs_files(std::path::Path::new(root), &mut found);
        // Asserted PER ROOT, not on the total: a mistyped or stale root
        // contributes nothing and would hide behind the other root's file count,
        // and `collect_rs_files` returns silently on an unreadable directory.
        assert!(
            !found.is_empty(),
            "no `.rs` found under the {label} source root ({root}) — a root that \
             scans nothing is a tripwire that cannot fire"
        );
        files.extend(found.into_iter().map(|p| {
            let rel = p
                .strip_prefix(std::path::Path::new(root))
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            (format!("{label}/{rel}"), p)
        }));
    }
    assert!(files.len() > 5, "the source scan found almost nothing — check the paths");
    files
}

/// `DeclaredRoot` must never gain an `AsRef<Path>` — the carry from #1055's
/// review (finding 2), and the exact sibling of the same refusal `GroupId`
/// already carries in `tests/groupid.rs`.
///
/// The property is not decoration. `as_path()` being a *named* method is what
/// makes every place a declared root becomes a working path greppable, and it is
/// the reason slice C can state where a root reaches the filesystem at all. A
/// blanket `AsRef<Path>` makes a `DeclaredRoot` silently usable as a path
/// everywhere `impl AsRef<Path>` is accepted — `Path::join`, `File::open`,
/// `Command::current_dir` — and the grep stops finding anything.
///
/// **Matched on shape, not on one spelling**, of either the trait or the type,
/// because the defining file imports neither by a qualified path:
///
///   impl AsRef<Path>                            for DeclaredRoot
///   impl AsRef<std::path::Path>                 for DeclaredRoot
///   impl std::convert::AsRef<Path>              for DeclaredRoot
///   impl core::convert::AsRef<std::path::Path>  for DeclaredRoot   (and mixed)
///
/// **Residual limits, stated rather than claimed away** (the same four
/// `tests/groupid.rs` enumerates, for the same reason — they are unbounded for a
/// textual scan and more regex buys less than it costs): an aliased import
/// (`use std::path::Path as P`), a macro-generated impl, an impl header split
/// across lines, and a `Deref<Target = Path>`, which would grant the same reach
/// by another road. None of the four is present today. What actually holds the
/// property is the compiler: with no such impl in the tree, a `DeclaredRoot`
/// cannot reach a path-taking API as a value at all.
#[test]
fn declared_root_never_gains_an_asref_path() {
    let files = production_sources();

    // The anchor, matched on CONTENT rather than on a path so #888's next batch
    // may relocate the module and this fails loudly instead of scanning past it.
    assert!(
        files.iter().any(|(_, p)| {
            std::fs::read_to_string(p).is_ok_and(|s| s.contains("pub struct DeclaredRoot {"))
        }),
        "the scan never reached the file that DEFINES `DeclaredRoot`. Wherever that \
         file lives is the one crate an `impl AsRef<Path> for DeclaredRoot` can \
         legally be written in (orphan rule), so a scan that misses it enforces \
         nothing — add its source root to `ROOTS`."
    );

    let mut offenders: Vec<String> = Vec::new();
    for (name, path) in &files {
        let src = std::fs::read_to_string(path).unwrap();
        for (i, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            // A comment may spell the construct literally — this module's docs
            // do, to explain this very rule.
            if trimmed.starts_with("//") {
                continue;
            }
            let n = trimmed.replace(' ', "");
            let Some((head, _)) = n.split_once(">forDeclaredRoot") else { continue };
            let Some((before_trait, ty)) = head.rsplit_once("AsRef<") else { continue };
            // `impl`, or `impl` + a module path ending in `convert::`.
            // Suffix-matched so an attribute or `pub` before it is fine.
            let in_trait_position =
                before_trait.ends_with("impl") || (before_trait.contains("impl") && before_trait.ends_with("convert::"));
            if in_trait_position && (ty == "Path" || ty.ends_with("::Path")) {
                offenders.push(format!("{name}:{}: {trimmed}", i + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "`DeclaredRoot` must have exactly one accessor, the named `as_path()`, and \
         never a blanket `AsRef<Path>` — that impl is what would make every place a \
         declared root reaches the filesystem invisible to a grep again (#1042, \
         #1055 review). Found:\n{}",
        offenders.join("\n")
    );
}

/// **The claim this whole slice rests on**: these are all the ways a root gets
/// declared.
///
/// `src-tauri/src/rootreg.rs`'s module doc says exactly that in a sentence, and
/// a sentence is not enforcement. This is default-deny over both production
/// source roots: any line that CALLS an admit is an offender unless its file is
/// on the allowlist below with an exact count and a reason. A new admit site
/// anywhere fails; so does a stale row, so the allowlist cannot quietly come to
/// describe code that no longer exists.
///
/// **Decided on shape, never on a binding's name** (the convention in
/// CLAUDE.md). The needles are the three call syntaxes a root declaration can
/// have, and no rename of a *variable* steps over any of them:
///
///   `.admit(`            — the `RootRegistry` method, the primitive itself
///   `admit_derived(`     — the best-effort engine-derived wrapper
///   `rootreg::admit(`    — the shared error mapping, qualified
///
/// A bare `admit(` was deliberately **rejected** as a needle, and the reason is
/// worth recording because it is the kind of thing that makes a scan look
/// thorough while enforcing nothing: `queue::admit` is an unrelated function —
/// the prompt queue's admission control — with a dozen call sites across both
/// crates. A needle that matched it would have to allowlist all of them, and an
/// allowlist that large stops being an argument and becomes a list nobody reads.
/// The dot in `.admit(` is what separates a method on a registry from a free
/// function about queues.
///
/// **Residual limits.** A call reached through a function pointer or a trait
/// object, and a call assembled by a macro, are not matched — none exists today.
/// Renaming the FUNCTIONS themselves would dodge every needle, which is why the
/// allowlist counts are exact rather than a maximum: a rename drops every count
/// to zero and fails here instead of passing green. Nor is the frontend scanned:
/// `admitRoot` is a wrapper over the one command, and what bounds it is that the
/// command is the only door — which is what this scan pins on the Rust side.
#[test]
fn every_admit_site_in_the_workspace_is_an_argued_one() {
    /// One row per file permitted to declare a root, with the exact number of
    /// call sites in it and the argument for each.
    ///
    /// Keyed by `<crate>/<path under that crate's src>` — the same label the
    /// offender report uses.
    const PERMITTED: &[(&str, usize, &str)] = &[
        (
            "src-tauri/rootreg.rs",
            1,
            "the admit surface itself — the ONE `RootRegistry::admit` call the \
             whole host side funnels through, wrapped by the `admit_root` \
             command and by the engine-derived helper beside it",
        ),
        (
            "src-tauri/orchestration/mod.rs",
            2,
            "orchestration's two engine-derived declarations: a group's checkout \
             (create AND resume — `create_group_ex` is both, so one site) and \
             the worktree `spawn_agent_ex` cuts for an agent",
        ),
        (
            "src-tauri/git.rs",
            1,
            "the worktree `git_worktree_add` cuts — a SIBLING of the repo \
             (`<repo>-worktrees/<name>`), so no descendant rule reaches it",
        ),
    ];

    /// The defining module, exempt from the CALL scan and required to exist.
    ///
    /// It holds `RootRegistry::admit` itself and that module's own unit tests,
    /// which call it dozens of times — and neither is a *consumer* declaring a
    /// root, which is the property this scan is about. Required rather than
    /// merely skipped, so a rename or a move fails here instead of silently
    /// exempting a file that no longer holds what the exemption was argued for.
    const DEFINING_FILE: &str = "loomux-engine/rootreg.rs";

    const NEEDLES: &[&str] = &[".admit(", "admit_derived(", "rootreg::admit("];

    let files = production_sources();
    let mut seen = vec![0usize; PERMITTED.len()];
    let mut offenders: Vec<String> = Vec::new();
    let mut found_defining = false;

    for (name, path) in &files {
        if name.as_str() == DEFINING_FILE {
            found_defining = true;
            continue;
        }
        let src = std::fs::read_to_string(path).unwrap();
        for (i, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            // A declaration is not a call: `pub(crate) fn admit_derived(`.
            if trimmed.contains("fn admit") {
                continue;
            }
            if !NEEDLES.iter().any(|n| trimmed.contains(n)) {
                continue;
            }
            match PERMITTED.iter().position(|(f, _, _)| *f == name.as_str()) {
                Some(idx) => seen[idx] += 1,
                None => offenders.push(format!("{name}:{}: {trimmed}", i + 1)),
            }
        }
    }

    assert!(
        found_defining,
        "the scan never reached {DEFINING_FILE}, the module that DEFINES the admit \
         — its exemption is argued for a file that is no longer where it was, so \
         update `DEFINING_FILE` (and check the exemption still holds)"
    );
    assert!(
        offenders.is_empty(),
        "a root may be declared ONLY where `PERMITTED` says (#1042). Found {} \
         unsanctioned admit site(s):\n{}\n\nIf one of these is legitimate, add its \
         file to `PERMITTED` with a count and the reason it is a TRUSTED source — \
         writing that argument down is the point of this test, because an admit \
         path nobody argued for is exactly how a wire caller comes to mint a root.",
        offenders.len(),
        offenders.join("\n")
    );
    for (i, (file, expected, why)) in PERMITTED.iter().enumerate() {
        assert_eq!(
            seen[i], *expected,
            "{file} was allowed {expected} admit site(s) ({why}) — found {}. A row \
             that no longer matches its file is a row that has stopped enforcing \
             anything; update the count and re-argue it, or remove the row.",
            seen[i]
        );
    }
}
