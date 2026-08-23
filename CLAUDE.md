# Orrerix — instructions for Claude Code

Tauri 2 desktop terminal multiplexer for AI agent management. Rust backend
(`src-tauri/`), vanilla-TypeScript frontend (`src/` — no UI framework), xterm.js
terminals, Vite. `doc/design/architecture.md` maps every module; deeper designs
live in `doc/design/`.

The repo root is a **Cargo workspace**: `src-tauri` (the desktop app, links
Tauri), `crates/loomux-engine` (the Tauri-free orchestration core, filling up
one batch at a time as #888 moves modules into it) and `crates/loomux-server`
(the remote-engine daemon that will host that core — a binary, and a leaf
nothing else depends on). One `Cargo.lock` and one `target/`, both at the repo
root. See `doc/design/engine-extraction.md` and
`doc/design/remote-engine-daemon.md`.

## Commands

| What | Command |
| --- | --- |
| Typecheck + bundle frontend | `npm run build` (runs `tsc --noEmit` first — this is the typecheck) |
| Frontend unit tests | `npm test` (Node 22 built-in runner, runs `test/**/*.test.ts` directly) |
| One frontend test file | `node --test test/layout.test.ts` |
| Backend check (what CI gates on) | `cargo check --locked --workspace` at the repo root |
| Backend tests | `cargo test --locked --workspace` at the repo root |
| One backend test | `cargo test --locked -p loomux --test orchestration <name_filter>` at the repo root |
| Run the app | `npm run tauri dev` — opens a GUI window and never exits; don't run it unattended |

There is no lint/format gate (no eslint/prettier; rustfmt is not enforced in
CI) — match the surrounding style instead of reformatting. (Agents may still
run `rustfmt --check` as a *syntax* check, discarding its formatting
opinions — see the `ci-validate` skill.)

### Running these in an agent worktree

- **`npm ci` before any `npm`/`node` command.** `node_modules/` is gitignored
  and not shared between worktrees, so a freshly-cut one has none. A
  missing-package error is *you never installed*, never a red suite — the
  `ci-validate` skill has the trap in full.
- **Every text file is CRLF on disk and LF in the blob** — `core.autocrlf=true`
  is this project's Windows baseline (see `.gitattributes`, which overrides it
  for exactly the files a build rewrites). So a `node -e` anchor built from an
  LF string, or from `git show <ref>:<file>`, never matches the worktree copy,
  and writing one back with bare LF silently flips that region's endings.
  Read the file's own EOL and rewrite your anchor to match; run a byte-identity
  or prefix proof blob-vs-blob (`git show` both sides), never blob-vs-worktree.
  Signature: `anchor not found` on a string you can see in the file (#1196).
- **Anchor every `cd` at an absolute path.** The Bash tool's cwd persists
  between calls, so a second relative `cd src-tauri/src/...` resolves against
  the previous `cd` and fails with `No such file or directory`.
- **There is no `python3`** — the `WindowsApps` alias stub exits 126
  (`Permission denied`). Use `node -e` for ad-hoc scripting. A scratch script
  **must be `.cjs`**: the root `package.json` sets `"type": "module"`, so a `.js`
  file in its scope — `./.scratch/` included — is ESM and `require` is a
  `ReferenceError: require is not defined in ES module scope`, not a broken
  script. Node resolves `"type"` from the NEAREST `package.json`, so `npm/` (no
  `"type"`) is CJS and `npm/bin/orrerix.js` uses `require` correctly (#1181).
- **A multi-line shell script is a file, not a `-c` argument.** Inline Bash dies
  on Git Bash quoting (`unexpected EOF while looking for matching '`, reported
  far from the real line). Write it under `./.scratch/` and run the file; pipe
  prose to `gh` with `--body-file -` (#1181).

### Agent workers: NO local Rust builds — CI is the only build/test path

The Commands table above is for humans. For agent workers, **local
`cargo` builds and tests of ANY size are banned entirely** — no `cargo
build`, no `cargo check`, no single-test `-j 4` iteration, nothing that
invokes `rustc`. This is a human hard directive: a first compile costs
5-8 GB of `target/` per worktree, and a worker fleet building locally
exhausts the disk (#488).

How workers validate instead: push early, open a draft PR immediately,
and read CI (`ci-validate` skill's draft-PR-early flow). Iterate by
reasoning + pushing; CI is both the proof and the compiler.
Frontend-only commands that never invoke `rustc` (`npm run build`/`tsc`,
`npm test`/`node --test`) remain fine locally once `npm ci` has run in the
worktree (see above), as does `rustfmt --check --edition 2021 <changed .rs>`
— a parser, not a build, and the one pre-push
syntax check for Rust (#558; see the skill for the read-stderr recipe and why
`cargo check` is not covered). The one `cargo` exception: `cargo update
--workspace` for release lockfile bumps — dependency resolution only, never
compiles.

## Hard constraints — check before coding

1. **Never resize the PTY for a UI feature.** Git view, task board, audit
   viewer, badges, compose strip — all are overlays or header/board chrome
   floating over the terminal. Resizing ConPTY triggers full repaints that
   pollute scrollback. Visual padding belongs on the `.xterm` element, not on
   the layout.
   **Two panels are in the layout, and both are deliberate**: `#sessions` and
   `.sidedock` (#1150, at the human's direction) are flex siblings of
   `#grid-area`, so opening either autosizes the open panes. What makes them
   permissible is not that they are old or asked-for: it is that each width
   change is a DISCRETE human click whose purpose is to change how much room
   the terminals get, and that `src/resizeburst.ts` collapses the whole
   animated burst into one fit per pane at the settled geometry. One known
   exception is open and watched rather than claimed away: the dock also
   re-widths itself when the room around it changes, so one panel's slide can
   chain onto the other's ease and outrun that coalescer (#1203). A PASSIVE or
   continuous trigger (a focus change, a follow, a timer, an attention flip)
   may still never reach a PTY resize — the side dock's own pane-following
   costs zero, and that is the line, not the panel count. Adding a third
   in-flow panel, or animating one of these two for longer than
   `FIT_MAX_WAIT_MS` minus a window, needs the argument in
   `doc/design/side-dock.md` and `doc/design/xterm-resize-reflow.md` first.
2. **No getrandom-based crates in `src-tauri`** (uuid v4, rand, tempfile with
   default features). They import `bcryptprimitives.dll!ProcessPrng`, which
   this project's Windows 10 baseline doesn't export — the binary then fails
   to load with 0xc0000139. Ids/tokens use std's OS-seeded `RandomState`. See
   the notes in `src-tauri/Cargo.toml` before adding any dependency.
3. **Never spawn real agent CLIs** (`claude`, `copilot`) to test or validate
   anything — it burns the user's paid credits. Tests fake the agent side
   (see `src-tauri/tests/`); the user does live agent validation themselves.
4. **Backend tests that link the lib must be integration tests**
   (`src-tauri/tests/*.rs`), not unit tests: Windows test executables need the
   comctl32-v6 manifest that `build.rs` embeds via `-tests`-scoped link args.
   Those args require at least one integration-test target to exist — never
   delete `tests/smoke.rs`.
5. **Frontend never touches Tauri IPC directly.** `src/transport.ts` is the
   ONLY module that may import `@tauri-apps/*` — it owns the `EngineTransport`
   seam (`invoke`/`listen` plus the host capabilities). Every backend
   capability is a `#[tauri::command]` plus a typed wrapper in `src/pty.ts` or
   a per-feature bridge (`git.ts`, `fileapi.ts`, `orchestration.ts`), and those
   wrappers call the seam. `test/transport.test.ts` enforces this — a direct
   `@tauri-apps` import anywhere else in `src/` fails the suite. See
   doc/design/engine-transport.md.
6. **A group id becomes a path in exactly one place.** `GroupId`
   (`crates/loomux-engine/src/groupid.rs`) has one validating constructor;
   `group_dir_at` (`src-tauri`) is the only function that joins one onto a
   root, and it takes a `GroupId`, not a string. `#[tauri::command]`s parse
   their raw `group_id` at the boundary (`command_group`) and thread the type
   from there. Never add a second join, never implement `AsRef<Path>` for
   `GroupId`, and never reintroduce a `&str` group parameter on anything that
   builds a path. Two source-scanning tests in `src-tauri/tests/groupid.rs`
   enforce this: one that every group-taking command parses at the boundary,
   one that `.join` is fed a group in exactly one place — the latter also
   asserting no `AsRef<Path>` impl exists, since nothing else can. That one
   scans **both** source roots (`src-tauri/src` and `crates/loomux-engine/src`)
   because the orphan rule puts the only writable `AsRef<Path>` impl in
   whichever crate owns the type; keep every root it must watch in its `ROOTS`
   list. It is a textual scan and enumerates its own limits: qualified and bare
   spellings of `AsRef`/`Path` are matched, but an aliased `Path` import, a
   macro-generated impl, a multi-line impl header, and `PathBuf::push`
   (indistinguishable from `Vec::push`) are not. None appears today — don't be
   the first. The compiler, not the scan, is what makes a `GroupId` unable to
   reach a `join` as a value.
   Membership ("may this caller touch this group?") is a **separate** check and
   is not implied by holding a valid id.
   **Four identifier families share one validating constructor** (#925):
   `loomux_engine::pathseg::PathSegment`. The group id (via `GroupId`, which
   keeps its own type and delegates only its *checks*), the agent-session id,
   the agent id, and merge-queue batch ids (via `mergeq::valid_id_component`)
   all run the same `check_segment`. Express a **new** family through
   `PathSegment` rather than writing another private "is this a safe id"
   predicate — the reason the consolidation happened is that four had drifted
   apart and the weakest was the one guarding a live `Path::join`.
   **One family is deliberately outside it**, and it is named here so nobody
   concludes the rule is decorative on finding it: workflow **block ids** are
   validated by `workflow::sanitize_id`, which is weaker than `check_segment`
   on exactly the two rules the alphabet does not give you — it permits a
   leading `-` and a Windows reserved device name — and it **rewrites rather
   than refuses** (`sanitize_id("../x")` yields `x`), which is the
   two-strings-name-one-directory hazard `pathseg` exists to avoid; bounded
   only because `parse_workflow` rejects an id `sanitize_id` had to change. A
   block id becomes `<id>.md` in the group dir. That is operator-authored
   config rather than caller input, so it is not a containment breach and #925
   left it alone; the filename scan below carries it as an argued allowlist row
   rather than being blind to it.
   The join scan's permitted-assembly-point list is one row **per family**
   (each required exactly once, so a renamed one fails loudly rather than
   watching nothing), and a sibling scan in `src-tauri/tests/pathseg.rs` covers
   the shape it structurally cannot see: a value interpolated into a **file
   name** (`format!("{x}.json")`). Its trigger is the *shape* — an
   interpolation plus a file-extension literal, matched inside the `format!`
   template — never a binding's name, per the source-scanning-guard convention
   below; default-deny, with an argued allowlist whose rows each name a proof
   that is re-checked, and which fails when a row goes stale.
   Still open, and **not** closed by any of this: the `ft_*`/`fm_*`/git `repo`
   **roots** are arbitrary caller-supplied absolute paths checked only by
   `is_dir()`. That is a root-admission problem, not a segment one — no
   predicate separates a repo from `~/.ssh` — and it is tracked on #1042.
7. **No agent ever merges a PR to the default branch.** Open the PR and stop;
   the human reviews and merges. This is the rule for *every* agent —
   workers, reviewers and planners have no merge authority at all, anywhere,
   and must never merge, tag, or publish a release.
   **One narrow carve-out, for the orchestrator only:** an orchestrator may
   merge a sub-PR into a **non-default branch** it owns — typically an
   integration branch collecting a batch of sub-PRs for a single human
   review — and only once that sub-PR has a reviewer's approval, green CI,
   and every review finding fixed or explicitly deferred. Merging to the
   default branch is **never** covered: it always needs a per-PR grant from
   the human, as does creating a release or pushing a `v*` tag. The carve-out
   exists so a human reviews one combined PR instead of five, not to reduce
   how much a human reviews. When in doubt, open the PR and ask. (#469)
8. **Orrerix is a generic agentic-dev tool — never bake this repo's or this
   machine's quirks into product code.** No toolchain special-casing (nothing
   cargo-/npm-specific in `src-tauri`; express "what's expensive/guarded/built
   here" as repo config, the way the resource guard's `resources:` block does)
   and no operator-setup assumptions (paths, core counts, installed tools). A
   behavior that only makes sense for developing orrerix itself belongs in
   `.orrerix/` config or the dev docs, not the product (precedent: #263).
9. **Never self-approve a security/install gate** (npm's `allow-scripts`
   review, a `gh` shim confirmation, anything else that exists to make a
   human or the orchestrator decide). If one fires, stop and
   `message_orchestrator`/`report("blocked", …)` instead of running the
   approve command yourself — even a narrowly-scoped approval is a security
   decision, and it isn't yours to make unprompted (precedent: #357). The
   repo pre-declares the one approval the build genuinely needs
   (`package.json`'s `allowScripts` field, committed); if `allow-scripts` or
   any other gate fires for something new, the answer is still to ask, not
   to decide.
   **If you're staring at an `allow-scripts` warning right now:** the
   `package.json` entry pins esbuild to an exact version
   (`"esbuild@0.25.12": true`) on purpose, not by oversight — it's the safer
   of npm's two forms (a name-only entry would silently cover every future
   version too). That means a routine `esbuild` version bump (pulled in via
   `vite` or a direct upgrade) makes the gate fire again for the new version
   — that is the pin working as designed, not a fix that broke. The right
   response is the same as for a brand-new package: stop and get a fresh
   human approval for the new version. Do not bump the pinned version
   yourself, do not switch it to a name-only entry to make the warning stop
   recurring, and do not add any other package's entry alongside it —
   widening this the "convenient" way is exactly the self-approval this
   constraint exists to prevent.

## Code conventions

- Frontend logic that needs tests is extracted into DOM-free pure modules
  (`layout.ts`, `steer.ts`, `spawnexpiry.ts`, …) and tested in
  `test/*.test.ts` with `node:test` + `node:assert/strict`. DOM wiring is
  validated by hand — don't simulate a DOM in tests.
- Backend: unit tests inline under `#[cfg(test)]` only if they don't link the
  full lib; otherwise integration tests (constraint 4). Orchestration logic is
  covered in `src-tauri/tests/orchestration.rs`.
- **An edit to a role template (`orchestrator|worker|reviewer|planner.md` under
  `src-tauri/src/orchestration/templates/`) re-blesses
  `src-tauri/tests/fixtures/pre222/` in the same commit** — the fixtures pin those
  four templates byte-for-byte modulo registered placeholders
  (`block.md`/`workflow.md` are deliberately not fixture-pinned; an edit whose
  only change is a `{{...}}` placeholder registered in LIVE needs no re-bless —
  the pin strips registered keys before comparing, precedent #859). Signature: `a_workflow_placeholder_must_sit_at_the_end_of_a_line_it_shares`
  and `the_toggle_off_leaves_every_instruction_file_byte_for_byte_what_it_was` go red
  alone, on a round where nothing else moved. Procedure and re-bless log:
  `src-tauri/tests/fixtures/pre222/README.md` (#867, #868, #874).
- **A source-scanning guard must not decide from a binding's *name*** — a rename
  steps over it, so it enforces nothing. Decide on name-independent axes and
  default-deny: the receiver (anything building a path off a declared root is
  denied unless it is on an allow-list carrying a reason per entry) plus a shape
  that cannot compile any other way (`.join(x.as_str())`); a name heuristic is a
  labelled supplement at best, and the residual blind spot is stated where the
  scan is implemented. Precedents: `tests/groupid.rs`, `tests/perf_dispatch.rs`,
  `test/perfpolicy.test.ts`. Signature: the guard's own doc quotes the line it
  was written for, and that line still passes (#922).
- `src-tauri/src/orchestration/mod.rs` is tens of thousands of lines — grep for
  the function/struct, don't read it top to bottom. Anchor an INSERT above the
  item's `///` block, never above its `#[tauri::command]`/`fn` — those sit BELOW
  the doc that owns them, so splicing there hands your item the neighbour's
  preamble and leaves the neighbour undocumented, with nothing red to say so.
  Signature: a `+///` line in the diff sitting directly under a context `///`
  line (#1229).
- Comments in this codebase explain *why* (design constraints, Windows quirks,
  issue numbers) — keep that density and style.
- Write tests that test intent, not implementation echoes.
- **A coverage claim is a claim.** When a PR body or comment says a test or mechanism
  polices a property, run the one mutation that removes it and watch WHICH tests
  redden — a match is evidence, a mismatch is a correction; disclose it (#664, #673,
  #682). A red evidences only the assertion it REACHED and MOVED: a panic before it,
  a split test's already-green half, or a companion that also passed broken prove
  nothing — split the test, or say which half moved (#710, #712, #727). A mutation a
  *reviewer* names is still unrun; run it before quoting it into the body, which
  becomes the squash message (#868). A suite green ACROSS a redesign is a
  control, not coverage — its fixtures were written against the old shape's
  failure modes. List what the NEW predicate reads and check some fixture varies
  each; a value every fixture happens to share is an unpinned axis. Signature:
  the fixtures all carry one incidental constant (four WIP caps, all `review` or
  `in-progress`, none on the status rows are born into) and the axis the
  redesign made load-bearing has no witness (#1182).
  A mutation is evidence only if it LANDED: a `sed`/`node -e` edit whose anchor
  matches nothing exits 0 and the suite then passes for the wrong reason, which reads
  exactly like coverage. Assert the mutation is present — anchor count, or a diff
  against the pre-mutation blob — and abort rather than record a run you did not
  produce; the CRLF trap under *Running these in an agent worktree* is one way the
  anchor silently misses. Signature: the pre- and post-mutation runs report identical
  pass counts (#1297).
- **An absence-only assertion needs a positive control, and the vacuity is a SHAPE.**
  `is_empty()`, `!contains(…)`, "renders nothing" — each passes just as well when the
  mechanism never ran at all. Pin first that it DID (`fired.len() == 1`, `scanned > 0`
  — a loose floor, not a second brittle pin on the thing's shape), then grep the suite
  for the same shape: the site a review names is rarely the only one. Signature: fixing
  a vacuous-test finding uncovers its twin one test over, green against an empty scan
  (#1209).
  A positive control proves the mechanism RAN, never that it SAW every subject: where it
  guards a pattern over a population, put the raw-count cross-check under *Every number in
  a PR body* in the assertion itself, where it fires on a blind instrument without anyone
  thinking to mutate the one subject it cannot see. Signature: one field renamable alone
  while a guard already carrying a vacuity control stays green (#1297, `test/reposlug.test.ts`).
- **A non-interference pin is fail-able only when its two operands COLLIDE.** A test
  asserting that operation X leaves Y alone must construct the fixture so X's key IS Y's
  subject; disjoint literals hold under every implementation, the symmetric one the pin
  exists to forbid included. Where a guard refuses the colliding fixture at write time,
  build it before the collision exists — write the link at an id that does not exist yet,
  then mint and delete that row — rather than weakening the assertion to fit. And count
  mutation rounds against the properties CLAIMED, not the tests reddened: a round that
  neutered the neighbouring property in the same test covers that one only. Signature: the
  assertion's two literals never meet (links `"#7"`, deletes `"t-2"`) while body, design
  note and doc comment all say the non-interference is pinned (#1300 B1,
  `doc/design/board-sprints-and-links.md` §3).
- **A test's specimen must stay a member of the class it witnesses.** When a directive
  moves a real specimen out of that class (a declared value converging with the
  default, a file gaining its "absent" block, a concrete list going stale), relocate
  the property onto a witness that still distinguishes — never relax the assertion to
  fit today's specimen. If the converged case still deserves coverage, give it its own
  strictly-weaker, explicitly-labelled assertion (#689). A **mechanical sweep** is the
  commonest way a specimen leaves its class: a rename that rewrites a test's string
  literal to the new spelling deletes the witness in the same commit that changes the
  behaviour, so CI stays green over it (#1225). The same drift bites outside tests: a
  hand-derived value a claim rests on (a line cite, a count) is valid only at the
  commit it was derived on, and your own next commit invalidates it as silently as a
  rebase. Cite a SYMBOL (#763); a position that must be recorded is swept in the LAST
  commit touching its source (#752).
- **A rename of an identity string classifies every site as EMIT or ACCEPT before
  rewriting it.** An emit site takes the new spelling alone; a reader keeps every
  accepted spelling — and a reader whose question is *what did the author DECIDE*
  fails by WIDENING a capability grant when it stops recognising the old one, which is
  the app granting itself capability that #222's closure forbids. Accept-both is not
  the blanket answer either, so split per question: `doc/design/rebrand-protocol.md`,
  "The one reader that must NOT accept every spelling". Pin the pre-rename specimen
  BESIDE the current one (#1225).
- **A documented escape hatch is a counterfactual — only a test that performs the
  edit pins it.** When a comment, design note or PR body says a policy can be undone
  by changing one arm or flag, the dispatch below it must give that variant its own
  arm, and a test must feed the *reverted* arm's return value to the real dispatch —
  plus a set assertion that exactly the intended variants take the dangerous branch,
  so the count fails when one is folded back in. Signature: an arm folding the
  escape-hatch variant in with the live one, excused by a comment saying that variant
  is unreachable — which the documented edit is precisely what makes false. Worked
  example: `obs::root_action`, `the_documented_revert_really_stops_the_migration` and
  `exactly_one_plan_variant_moves_anything` (#1205 B1).
- **A per-CLI identity string is read off the source, never branched on it.**
  `source === "claude" ? "claude" : "copilot"` is right only while there are
  exactly two CLIs; a third silently inherits the else-branch and the pane
  name, badge or resume command asserts the wrong CLI. Gating *behavior* a CLI
  genuinely has (`cli == "claude"` for hook settings) is fine — producing a
  *name* that way is a defect. Adding a CLI means grepping `"claude" ?`,
  `== "claude"`, `"claude" =>` (match-arm dispatch the first two patterns
  miss) and the `!= "claude"` polarity across `src/` and `src-tauri/src/`,
  and classifying every hit as behavior or mistype (#722, #841).
- **A guard reads every one of its inputs by one rule.** Taking one signal from
  "the options OR the existing state" and the next from the options alone is a
  bypass exactly the width of that asymmetry; so is a check present at one call
  site and absent from its sibling. Union every field on one side *inside* the
  pure guard — never at the DOM call sites, which drift — and pin all four
  crossings of {which side says X} × {which side says Y} plus the negative
  control, so "refuse everything" cannot pass either. Worked example:
  `sshOrchestrationRefusal` and `doc/design/ssh-panes.md` (#859, #906, #921).
  The two signals can also be ONE state read at two POINTS: a "before" snapshot
  taken below any part of the write — the row insertion included — is the same
  asymmetry, and it leaves the guard inert exactly where before and after
  coincide. Hoist the snapshot above every mutation rather than subtracting the
  written row back out in your head, and when a review names one asymmetry
  re-derive EVERY input — the redesign is where the next one lands. Signature:
  the fix for a one-rule finding ships a second of the same class (#1182).
- **A `Mutex` that serialises tests is locked with `lock_safe`, never
  `.lock().unwrap()`.** One failing test panics under the guard and poisons it,
  so every later test on that lock dies of `PoisonError` — one genuine failure
  reported as N, and a mutation round's reds stop being attributable to the
  behaviour they were cut for. Restore any global the harness overrode from a
  `Drop` guard, for the same reason. Signature: extra tests reddening with
  `Result::unwrap() on an Err value: PoisonError` beside the one you
  expected (`SERIAL` in `crates/loomux-engine/src/obs.rs`, #1236).

## Refinements & scope increases from the user

Default: when the user asks for a refinement or feature addition on work already in
progress (an open PR, an active branch), **fold it into the active PR** rather than
deferring it to a follow-up issue. This is different from an agent inventing extra scope
mid-diff — that's still a review ground to bounce ("scope drift... split it"). Here the
user is the one increasing scope, deliberately, because they thought of the right shape
while watching the work land — that's a refinement, not drift. Only defer to a separate
issue when the user explicitly says to ("later", "follow-up issue", "separate PR"). Don't
narrow their ask back down to the original ticket on your own judgment.

## When the brief can't be followed as written

- **A self-contradicting instruction is not implementable — say which half you dropped.**
  Where a plan states a rule twice and the two readings differ, never silently pick one:
  name the contradiction, implement the reading it states first and argues for at length,
  get the deviation approved, and record it where the plan's NEXT implementer looks — the
  design-note section and, for a role-template edit, the `pre222` re-bless log — not only in
  a PR body nobody re-reads. Signature: a "take the first that decides it" ladder given a
  rung BELOW one that always decides (board order over an array never ties), so the new rung
  is unreachable text (`doc/design/board-sprints-and-links.md` §7,
  `src-tauri/tests/fixtures/pre222/README.md`, #1300).
- **A slice told to create a shared seam yields to an in-flight branch that already built it
  richer — and ships none of it.** A third, weaker shape defeats the seam it was meant to
  honour, and extending an unmerged branch breaks "this slice waits on nothing"; leave the
  seam where it is and let the later slice add the one key it owes. Confirm with the
  orchestrator before proceeding. Signature: the plan's composition contract says "whichever
  PR lands first creates it" while the richer implementation is unmerged, its own doc comment
  already reserving your key (#1300, `BoardFilter`).

## Git & GitHub workflow

- Commits: `type(scope): imperative subject (#issue)` — e.g.
  `fix(orchestration): expire timed-out spawn requests (#106)`. Common scopes:
  `orchestration`, `pty`, `gitview`, `launcher`, `tasks`, `clipboard`,
  `metrics`, `ui`, `build`, `release`.
- Branch from `main`; PR to `main`.
- **Delete a PR's branch once it merges.** `gh pr merge --delete-branch`
  handles it, but skips the remote delete when a local worktree still holds
  the branch — after cleaning the worktree, verify with
  `git ls-remote --heads origin <branch>` and `git push origin --delete
  <branch>` if it survived. Whoever performs the merge owns this step (#662).
- **Git Bash mangles a `<ref>:<path>` argument when the path starts with a
  dot.** `git rev-parse origin/main:.github/x` is rewritten to
  `origin\main;.github\x` and errors, while `origin/main:src/x` works — so a
  blob-by-blob sweep silently reports exactly the dot-directory files
  (`.claude/`, `.github/`, `.orrerix/`) as mismatched, and an error string
  compared as a blob reads as a real difference. Prefix `MSYS_NO_PATHCONV=1`
  on any `git`/`gh` invocation whose argument carries a ref-colon-path (#841).
- **An end-of-file append conflicts on its shared trailing tokens, not on its
  content.** Test blocks in `src-tauri/tests/orchestration.rs` all end `);` + `}`,
  so two branches appending there get that tail matched as common context and
  each side arrives ending mid-assertion; concatenating splices one block into
  the middle of the other's final `assert!`. Prove the resolution rather than
  parse-checking it: the base blob must be a verbatim **prefix** of the resolved
  one (`startsWith` over `git show <ref>:<file>` for both), with a single append
  hunk in `git diff -U0`. Signature: a conflict whose two sides are each
  syntactically incomplete (#1196).
- GitHub issues are the work queue. Labels the orchestration workflow uses:
  `agent-managed` (an orchestrator owns it), `agent-ready` (groomed — go),
  `agent-investigation` (research only — post findings as an issue comment,
  no code), `agent-prototype` (build for demo/feedback).
- User-visible behavior changes must update the matching user-docs page under
  `docs/` (the README is a pitch, not a manual — only touch it when the pitch
  itself changes); substantial designs get a `doc/design/*.md` note.
- **Every number in a PR body is measured at the base AND at the head** — never
  derived by arithmetic, remembered, or carried from a mid-branch run. Counts,
  deltas, diffstats and run ids all go stale on the next commit. Read both
  totals out of the two runs' own logs, and check that the per-file deltas sum
  to the total you are claiming (#859, #862, #889, #907, #914, #921).
  A number is also only as good as the instrument that produced it: a regex character
  class is a GUESS about the alphabet of its own subjects, so census by walking the real
  delimiters and cross-check the total against a raw count of the container. Signature:
  two totals stated confidently and both light by exactly one, from a `([a-z-]+)` that
  stops dead at the digit in a real value (`no-sha256`) — a census that cannot see one
  of its own subjects is not a census (#1209). Build the pattern from what a token may
  CONTAIN (a fact), never from what may FOLLOW it (unbounded prose): the second instance
  was a follow-class omitting `#`, blinding a guard to `…/loomux#readme` (#1297).
- **A sweep is dated to the base it was run on.** A rename or purge is complete only
  against the tree it was grepped on: a rebase replays your patches but not your grep,
  and work merged meanwhile authors fresh instances of the string you removed — a live
  defect, not a stale measurement. An all-`=` `git range-diff` (`ci-validate`'s recipe)
  does not cover it and cannot: it says your patches replayed unchanged, never that the
  base they replayed onto is clean of what you swept. So re-grep the entity across EVERY
  root (`crates/`, `src-tauri/`, `src/`, `docs/`, `test/`, `e2e/`) after each rebase and
  before the final green — scoping it to the directory you last edited is the miss —
  and `git log -S` names the commit that authored each survivor. Signature: review names
  N stale sites and the whole-tree grep finds N+1 (#1205, whose range-diff was `=` on
  every commit with five fresh instances sitting in the new base; #1191).
  A rebase also widens a SET, and no grep finds that one because nothing YOU wrote
  went stale: a sibling refactor routing a second list through one shared refusal
  leaves every test enumerating that rule covering only the list that existed when
  it was written. Re-read each shared helper the new base put your tests' rules
  behind, and perform the edit on every site it serves. Signature: the shared helper's
  own doc names the divergence it prevents — `gate_reviewer_error`, "static list ends
  up refusing a manager while a routing rule quietly accepts one" — and no test builds
  the second list (#1229).
  A rebase imports RULES as well as code, and those apply retroactively to your OPEN diff
  with nothing mechanical pointing at what you now violate — no red, no stale grep hit, no
  range-diff row. Diff the convention surfaces across the rebase span (`git diff
  <old-base>..<new-base> -- CLAUDE.md .claude/skills/ .orrerix/lessons.md`) and re-check
  your own diff against each bullet the base gained. Signature: an all-`=` range-diff and a
  green suite over a diff that matches a convention younger than your branch point; #1300
  found its own set-widening defect that way, unprompted by review.
- **Correcting a false claim is a multi-surface edit.** A design rationale here
  lives on several permanent surfaces at once — the code comment, the
  `doc/design/*.md` note, the PR body (which becomes the squash message), and
  the `docs/` page when the claim is user-visible (the bullet above mandates
  it) — so a claim deleted from one survives on the others. Verify the purge by
  grepping the *entity* the claim names, never the phrasing you rewrote.
  Signature: a re-review that clears a claim on two surfaces and finds it alive
  on the third (#878). **Correct the twin, not just the named site**: one entity
  grep clears the sites you thought of, while each line you rewrite still has a
  paraphrase elsewhere (a comment and its design-note gloss), so grep each
  corrected line's own distinctive noun (`by provenance`, `strictly narrower`)
  across the tree in the same pass. Signature of the miss: the same finding
  reopens a third round, on twins of lines an earlier round corrected (#922).
  An earlier **commit subject** superseded later in the same PR is a surface
  too — the squash aggregates it and it cannot be edited in place, so flag it
  in the body for whoever squashes (#909). Run the sweep whenever YOU narrow a
  guarantee, not only when a review names a false claim: a mid-branch fix
  falsifies your own earlier prose and nothing MECHANICAL points at it — no
  number to re-derive, no test to redden — and a reviewer's list of sites is a
  sample, not the set (#1189: a module header falsified by that PR's own later
  commit, and 6 sites named against 11 found; #1215).
  Where the claim is a **quotation** rather than a paraphrase it is checkable,
  so check it instead of sweeping: every passage a PR body quotes out of a file
  in its own diff must still hold in that file at head — modulo whitespace, and
  by hand for an inline quotation the mechanical harvest does not reach.
  Signature: the body's *What changed* quotes the exact phrasing a later commit
  on the same branch removed, and the squash republishes it on the one surface
  nobody can edit afterwards (#1271). Recipe, and the blind spots it is scoped
  to: `.claude/skills/ci-validate/SKILL.md`.
- **A doc naming a file or test that hasn't landed must say so in its tense
  and name the slice** — `` `tests/perf_dispatch.rs` *will* enforce … (#743
  S2/S3) `` — or the pointer waits for that slice. Present tense beside a
  shipped guarantee in the same construction reads as shipped, and the reader
  who acts on it gets silent green (#750).
- **A claim about how markdown RENDERS is measured, never read off the
  source.** Put the text through GitHub's own GFM endpoint before claiming a PR
  body, issue comment or `docs/` page renders a certain way — `gh api -X POST
  markdown -f mode=gfm -f text="$(cat file.md)"`. A blank line silently ends a
  table, and the row you claimed becomes a paragraph of literal pipes (#926).
- **A claim about the PR body is measured on the POSTED body, never on your
  draft.** Writing it is not posting it: a body rebuilt from sources destroys any
  edit made to the assembled file, so edit the sources, assemble, then re-read the
  result with `gh pr view <n> --json body`. On the READING side the body is unpinned
  by the head SHA and drifts under a recorded verdict, so re-read it immediately
  BEFORE recording one — `body-unchanged` refuses a post-pass edit at the merge, but
  cannot give back a round already spent on stale text (#565). Signature: a re-review
  quoting a body line as verbatim what it was, on a finding your own response section
  says was narrowed (#1225).
- **Historical context lives in design notes, ADRs, and issue/PR history —
  never in user docs, this repo's own agent instruction files
  (`.github/agents/`, `.claude/skills/`, `.orrerix/workflow.yml`), or this
  file.** Incident stories, superseded rules, dates, and "how we got here"
  narratives pollute every future reader's context. Reader-facing text
  carries the current rule and its operational why, with at most a bare
  issue/PR ref as provenance; strip any such narrative you find when
  editing these surfaces — including `.orrerix/lessons.md`, which carries
  the rule and fix only, with refs as provenance. Out of scope: code
  comments (the "comments explain *why*" convention), the shipped
  agent-role templates (`src-tauri/src/orchestration/templates/`, governed
  by their design notes), and **vendored files** (any skill directory with
  a "Vendored skill — do not edit in place" README, e.g.
  `.claude/skills/frontend-design/`):
  editing those silently forks the vendor — re-vendor from upstream instead,
  per `THIRD_PARTY_NOTICES.md`.
