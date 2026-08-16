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

/// The worktree an agent spawn cuts is declared **in its own right**, and this
/// test is written so it cannot pass for the wrong reason (#1092 review,
/// finding 2 — the previous coverage proved the call site existed, not that it
/// worked).
///
/// The trap it is built to avoid: a worktree that happened to live *inside* the
/// checkout would resolve through the group's declaration and the descendant
/// rule, with the worktree's own declaration doing nothing — and the test would
/// still be green with that line deleted. loomux puts worktrees at
/// `<repo>-worktrees/<name>`, a SIBLING of the checkout, so the assertion below
/// first proves the sibling relationship and only then that it resolves. The two
/// together mean the resolution can have come from nowhere but the worktree's
/// own admit.
#[test]
fn spawning_an_agent_declares_the_worktree_it_cut() {
    let git = |dir: &std::path::Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "")
            .env("GIT_CONFIG_SYSTEM", "")
            .output()
            .expect("git must be installed for this test");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };

    // A bare remote with a real default branch: `git_worktree_add_sync` cuts
    // from `origin/<default>`, never the checkout's incidental HEAD (#204), so
    // the fixture needs an origin to resolve.
    let bare = tempfile::tempdir().unwrap();
    git(bare.path(), &["init", "-q", "--bare"]);
    git(bare.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]);

    let seed = tempfile::tempdir().unwrap();
    git(seed.path(), &["init", "-q"]);
    git(seed.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(seed.path(), &["config", "user.email", "t@t"]);
    git(seed.path(), &["config", "user.name", "t"]);
    std::fs::write(seed.path().join("base.txt"), "base").unwrap();
    git(seed.path(), &["add", "-A"]);
    git(seed.path(), &["commit", "-qm", "base on main"]);
    git(seed.path(), &["remote", "add", "origin", &bare.path().to_string_lossy()]);
    git(seed.path(), &["push", "-qu", "origin", "main"]);

    let cloneparent = tempfile::tempdir().unwrap();
    git(cloneparent.path(), &["clone", "-q", &bare.path().to_string_lossy(), "wc"]);
    let primary = cloneparent.path().join("wc");

    let repo_path = s(&primary);
    let (reg, _d) = test_registry();
    let roots = reg.roots();
    let g = reg.create_group(&repo_path, rails()).unwrap();

    let w = reg
        .spawn_agent(&g.id, Role::Worker, "w", "t", true, Some("agent-x".into()))
        .unwrap();

    // 1. The worktree is genuinely OUTSIDE the declared checkout. If this ever
    //    stops holding, the resolve below stops proving anything and this test
    //    must be rewritten rather than relaxed.
    assert!(
        !std::path::Path::new(&w.cwd).starts_with(&primary),
        "a cut worktree must be a SIBLING of the checkout (`<repo>-worktrees/<name>`), \
         not inside it — otherwise it would resolve through the group's own \
         declaration and this test could not tell the two apart. Got worktree {:?} \
         under repo {:?}",
        w.cwd,
        primary
    );

    // 2. And it resolves anyway — which, given 1, can only be its own admit.
    assert!(
        roots.resolve(&w.cwd).is_ok(),
        "the worktree an agent spawn cut must be declared in its own right — \
         without it the agent's pane cannot read its own workspace once slice C \
         enforces. Worktree: {:?}",
        w.cwd
    );

    // 3. The negative control on the same axis: the worktrees PARENT directory
    //    (`<repo>-worktrees`) is an ancestor of what was declared, and an
    //    ancestor grants strictly more than was declared, so it must be refused.
    //    This is what separates "the worktree was declared" from "something
    //    broad enough to contain it was declared".
    let worktrees_parent = std::path::Path::new(&w.cwd)
        .parent()
        .expect("a cut worktree has a parent directory");
    assert!(
        roots.resolve(&s(worktrees_parent)).is_err(),
        "the worktrees PARENT must not resolve — only the cut worktree itself was \
         declared, and an ancestor grants more than that. Parent: {worktrees_parent:?}"
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
/// CLAUDE.md). The needles are the four call syntaxes a root declaration can
/// have, and no rename of a *variable* steps over any of them:
///
///   `.admit(`               — the `RootRegistry` method, the primitive itself
///   `RootRegistry::admit(`  — the same method called as an associated fn
///   `admit_derived(`        — the best-effort engine-derived wrapper
///   `rootreg::admit(`       — the shared error mapping, qualified
///
/// A bare `admit(` is deliberately **not** a needle, and the reason is worth
/// recording because it is the kind of thing that makes a scan look thorough
/// while enforcing nothing: `queue::admit` is an unrelated function — the prompt
/// queue's admission control — with a dozen call sites across both crates. A
/// needle matching it would have to allowlist all of them, and an allowlist that
/// large stops being an argument and becomes a list nobody reads. The dot in
/// `.admit(` is what separates a method on a registry from a free function about
/// queues.
///
/// **Every needle is defeated by an import, so imports are default-denied.**
/// Each of the four spellings above can be dodged by naming the module or the
/// function something else on the way in, and all three shapes are real:
///
///   `use crate::rootreg::admit;`      then `admit(…)`
///   `use crate::rootreg as rr;`       then `rr::admit(…)`
///   `use crate::rootreg::{`⏎`admit,`⏎`};`  — the same, wrapped across lines
///
/// None of those calls matches a needle. So rather than chase the call sites,
/// the second assertion below denies the *doorway*: **a `use` statement that
/// mentions `rootreg` at all is an offender unless it is one of the two
/// allowlisted type-only imports**, matched on its whole normalized text. That
/// covers a function import, a module alias, an aliased function, and any of
/// them wrapped across lines, because none of those can name the module without
/// the word `rootreg` appearing in the statement.
///
/// Statements are joined to the terminating `;` before matching, which is what
/// makes the multi-line braced form reachable at all — matching line-by-line is
/// exactly how the first version of this guard missed it (#1092 review, N4).
///
/// **Residual limits, corrected.** An earlier version of this comment claimed an
/// aliasing import defeated the scan; it did not, and naming a closed hole while
/// two open ones sat beside it is worse than naming none. What genuinely remains:
/// a call reached through a function pointer or a trait object, and a call — or
/// an import — assembled by a macro; none exists today, and a macro-built `use`
/// is the one shape this cannot see, because there is no `use` statement in the
/// source to read. Renaming the FUNCTIONS themselves would dodge every needle,
/// which is why the allowlist counts are exact rather than a maximum: a rename
/// drops every count to zero and fails here instead of passing green. Nor is the
/// frontend scanned: `admitRoot` is a wrapper over the one command, and what
/// bounds it is that the command is the only door — which is what this scan pins
/// on the Rust side.
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

    const NEEDLES: &[&str] = &[
        ".admit(",
        "RootRegistry::admit(",
        "admit_derived(",
        "rootreg::admit(",
    ];

    /// The ONLY `use` statements permitted to mention `rootreg`, by file and by
    /// whole normalized text. Both import a TYPE and neither can produce a call
    /// spelling the needles miss.
    ///
    /// Whole-text rather than a prefix, so widening one — adding `admit` to the
    /// braces, or an `as` alias — fails here instead of matching a row it has
    /// outgrown.
    const PERMITTED_USES: &[(&str, &str)] = &[
        (
            "src-tauri/orchestration/mod.rs",
            "pub use loomux_engine::rootreg::{RootError, RootRegistry};",
        ),
        ("src-tauri/rootreg.rs", "use loomux_engine::rootreg::RootRegistry;"),
    ];

    let files = production_sources();
    let mut seen = vec![0usize; PERMITTED.len()];
    let mut offenders: Vec<String> = Vec::new();
    let mut bad_uses: Vec<String> = Vec::new();
    let mut permitted_uses_seen = vec![0usize; PERMITTED_USES.len()];
    let mut found_defining = false;

    for (name, path) in &files {
        if name.as_str() == DEFINING_FILE {
            found_defining = true;
            continue;
        }
        let src = std::fs::read_to_string(path).unwrap();
        let lines: Vec<&str> = src.lines().collect();
        // Pass 1 — the doorway. Every `use` STATEMENT, joined to its `;` so a
        // braced import wrapped across lines is one string here (matching
        // line-by-line is exactly how the first version missed it). Any
        // statement mentioning `rootreg` is denied unless it is allowlisted
        // whole: that covers a function import, a module alias
        // (`use crate::rootreg as rr;`), an aliased function, and any of them
        // wrapped — none can name the module without the word appearing.
        let mut i = 0;
        while i < lines.len() {
            let trimmed = lines[i].trim_start();
            let is_use = ["use ", "pub use ", "pub(crate) use "]
                .iter()
                .any(|kw| trimmed.starts_with(kw));
            if !is_use || trimmed.starts_with("//") {
                i += 1;
                continue;
            }
            let start = i;
            let mut stmt = String::new();
            while i < lines.len() {
                stmt.push(' ');
                stmt.push_str(lines[i].trim());
                if lines[i].contains(';') {
                    break;
                }
                i += 1;
            }
            i += 1;
            // Collapse runs of whitespace so a wrapped statement compares equal
            // to the single-line spelling an allowlist row is written in.
            let normalized = stmt.split_whitespace().collect::<Vec<_>>().join(" ");
            if normalized.contains("rootreg") {
                match PERMITTED_USES
                    .iter()
                    .position(|(f, s)| *f == name.as_str() && *s == normalized)
                {
                    Some(idx) => permitted_uses_seen[idx] += 1,
                    None => bad_uses.push(format!("{name}:{}: {normalized}", start + 1)),
                }
            }
        }
        // Pass 2 — the call sites.
        for (i, line) in lines.iter().enumerate() {
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
        bad_uses.is_empty(),
        "a `use` statement mentioning `rootreg` must be one of the allowlisted \
         TYPE-only imports (#1042; #1092 review N4). Every call-shape needle this \
         test uses can be dodged by renaming the module or the function on the way \
         in — `use crate::rootreg::admit;` then `admit(…)`, or \
         `use crate::rootreg as rr;` then `rr::admit(…)`, or either wrapped across \
         lines — so the doorway is default-denied rather than the call sites \
         chased. Spell the call `rootreg::admit_derived(…)` / \
         `RootRegistry::admit(…)` at its site instead. If a new import really is \
         type-only and safe, add its whole normalized text to `PERMITTED_USES` \
         with that argument. Found:\n{}",
        bad_uses.join("\n")
    );
    for (i, (file, stmt)) in PERMITTED_USES.iter().enumerate() {
        assert_eq!(
            permitted_uses_seen[i], 1,
            "`PERMITTED_USES` expects exactly one `{stmt}` in {file} — found {}. A \
             row matching nothing has stopped enforcing anything: the import it was \
             argued for moved, changed shape, or went away, and until the row is \
             updated a real replacement for it would be reported as an offender \
             rather than checked against it.",
            permitted_uses_seen[i]
        );
    }
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
