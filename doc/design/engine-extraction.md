# Extracting the orchestration engine into its own crate

Issue #847 (the investigation), #888 track A (the reason it is happening now).
This note is the boundary argument: what `loomux-engine` is, what may cross into
it, what must not, and the order the moves happen in. Slice A1 — the workspace
scaffold — landed first and landed empty on purpose; A2 is now moving modules
into it in batches (§6).

Related: `doc/design/architecture.md` (the module map), `doc/design/engine-transport.md`
(#905 — the same cut line, on the frontend side), `doc/design/remote-engine-protocol.md`
(#888 slice B1 — the wire contract the daemon will speak),
`doc/design/groupid-and-path-roots.md` (#904).

## 1. Why a crate, and why now

`src-tauri` is one package whose library links Tauri. On Linux that means
webkit2gtk. The remote-engine work needs a headless daemon that owns the PTYs,
the registry, the board and the timers, and **a server that has to build a
browser engine in order to exist is not a deployment shape** — it drags GUI
system libraries onto a box that will never draw a pixel, and it makes the
build's failure modes the desktop's failure modes.

So the engine has to become something that compiles with no `tauri` in its
dependency tree. That is the whole requirement, and it is the only one this
extraction is trying to satisfy. It is worth saying what it is *not*: this is
not a bid to publish a library, not a plugin API, and not an abstraction layer
added because layering is tidy. #847 investigated it as a good idea in its own
right and recommended holding at Phase 2 unless something needed Phase 3; #888
is the something.

The seams are already there, which is why this is a sequence of moves rather
than a rewrite: the backend integration suite drives orchestration headless
today, with `AppHandle` absent. The extraction makes that supported rather than
incidental.

## 2. The boundary

**One rule, and everything else follows from it: `src-tauri` depends on
`loomux-engine`; the arrow never points back.**

In the engine:

- the orchestration core — registry, task board, gates and verdicts, the
  delivery/queue machinery, audit, the merge queue, workflow parsing, digests,
  lessons, profiles, termgrid, notifications, the MCP server;
- everything it needs from the host, expressed as a **trait the host
  implements**.

Staying in `src-tauri`:

- every `#[tauri::command]`, the capability/ACL manifests, the webview;
- the desktop implementations of those host traits;
- the Windows-specific desktop surface (Job Objects, shell integration, PDH/DXGI
  metrics, WASAPI voice).

Two traits carry the whole relationship, and they arrive in slice A3 rather than
here, because designing them against real call sites beats designing them
against an empty crate:

- **`EventSink`** — the engine emits typed events; the desktop implementation
  forwards to `AppHandle::emit`, the daemon's serialises them onto the wire, and
  the test one records them. This replaces ~27 direct `emit` sites.
- **`PaneHost`** — the engine asks for a pane and writes to it; the desktop
  implementation reaches the `PtyManager` in Tauri state, the daemon's owns the
  PTYs directly. `NullPaneHost` replaces today's "if the app handle is `None`"
  test branch, which is the same idea already, just spelled as an `Option`.

The bar for both is **behavioural silence**: the existing integration suite
green with no test edits. A trait swap that needs the tests rewritten to pass is
a trait swap that changed behaviour.

## 3. `GroupId` moves with the engine, and membership does not follow it

#904 already did the hard part. `GroupId` is a validated newtype with one
constructor, deserialisation routed through the same gate, and no
`AsRef<Path>`; ids become paths in exactly one function. The reason that
mattered was stated in that note and is worth restating here because the
extraction is what makes it load-bearing: the old trust in `group_id` was a fact
about the **transport** (only our own webview can invoke a command), and a crate
consumer — let alone a network client — does not inherit that fact.

So `groupid.rs` moves into the engine (slice A2 batch 2) with its logic
untouched, and the engine's public API takes `GroupId`, never `&str`, wherever a
group is named. A caller that has not parsed cannot call. That is the property,
and it survives the move because it lives in the type rather than in the caller.
Every edit inside the module is a doc comment: three intra-doc links pointed at
`src-tauri` items (`group_dir_at`, `mergeq::valid_id_component`, `SOLO_GROUP`)
and would have dangled once `super::` named the engine crate root, so they are
re-spelled as plain code text, and the module doc gains the tripwire argument §6
spells out. No logic line moved. It is also what gives the
engine its first dependency: `serde`, for the hand-written `Deserialize` that
routes a persisted id back through `GroupId::parse` rather than trusting the
file.

What does **not** move with it: **membership is a separate check.** Holding a
well-formed id says the string is safe to join onto a root; it says nothing
about whether this caller may touch that group. Today the desktop answers that
question by being the only caller. A daemon cannot, and #888's design note owns
that answer — it is not smuggled in here as a side effect of the crate boundary.

The remaining caller-supplied path identifiers (`ft_*`/`fm_*` roots, `repo`,
`session_id`) are **#925**, not this work. They are a stated merge blocker for
the listener slices. The extraction neither fixes nor worsens them; it just must
not quietly move an unvalidated identifier into a place that looks more trusted
than it is.

## 4. Publish stance

`publish = false`, version `0.0.0`, and deliberately **not** one of the seven
version fields `scripts/check-versions.js` keeps in lockstep.

The argument: a workspace-internal crate buys the entire architectural benefit —
a compiler-enforced boundary, a dependency tree that can be audited, a daemon
that can link it — with none of the crates.io stability tax. A published crate's
API is a public contract with strangers and a semver obligation on every rename;
this one's consumers are both in this repo. A version number on it would be a
number nobody reads and one more thing a release bump could silently forget.

If it is ever published, it joins the version check in the same commit that
flips the flag. (Repo rules already make the crate's API a public contract for
*this* repo's purposes, which is why this note exists at all.)

Lint stances — `forbid(unsafe_code)` and friends — are deliberately **not** set
yet. A blanket stance invented against an empty crate is a stance the module
moves would have to fight or silently weaken; it belongs in the slice that can
see the code it applies to.

## 5. What slice A1 actually changed, and why the plumbing was the risk

The code change is a virtual workspace manifest and an empty crate. The
*interesting* change is that converting to a workspace moves three things that
nothing in the Rust source mentions, each of which fails quietly:

1. **The release profile.** Cargo reads `[profile.*]` from the workspace root
   manifest and **ignores it in a member, with a warning rather than an error**.
   Left in `src-tauri/Cargo.toml`, `lto`, `codegen-units` and the
   `debug`/`strip` settings that make a crash backtrace name loomux's own
   functions (#53) would have stopped applying to every release build while CI
   stayed green. It moved to the root manifest.
2. **The lockfile.** One `Cargo.lock` per workspace, at the root. A leftover
   `src-tauri/Cargo.lock` would be a file cargo never updates again but which
   still parses — and `check-versions.js` would have gone on reading a version
   out of it. It moved, and the check follows it.
3. **The build directory.** `target/` is at the workspace root now, which the
   `.gitignore`, the rust-cache config in both workflows, ci.yml's
   `LOOMUX_E2E_EXE` and `e2e/fixtures.ts`'s `DEFAULT_EXE` all had to learn. The
   E2E pair is the nastiest of these: it is a `continue-on-error` job, so drift
   surfaces as "exe not found" long after the edit that caused it.

CI also gained `--workspace` on its cargo invocations. Without it cargo builds
only the package in the invocation directory, so the engine crate would never
have been compiled by CI at all and a broken manifest could merge green.

`test/workspacelayout.test.ts` pins all of the above. It is a repo-file pin in
the style of `releasepromote.test.ts`: none of these invariants is reachable
from a unit test of product code, agents are banned from running cargo locally
(#488), and every one of them fails silently rather than loudly.

## 6. Order of the moves, and why it is strictly serial

- **A1 — the scaffold** (this slice): workspace, empty crate, this note, the
  plumbing above.
- **A2 — the Tauri-free submodules.** Move the modules that already have no
  Tauri in them; `mod.rs` re-exports so no caller changes. Small batches, one
  PR each. Per-PR bar: CI green and `git diff --stat` showing moves plus import
  lines only — except where a batch states otherwise and argues it. Two have:
  batch 2, whose move changed what a source-scanning test could see and so had
  to edit that test, and batch 4, which found a wire contract the suite did not
  pin per variant and added the test rather than claiming the exemption over it
  (both below).

  **The batch order is a dependency order, not a size order**, and "Tauri-free"
  turned out to be the weaker of the two tests. Every module in the A2 set is
  free of `tauri::` — that was never the constraint that bit. What decides
  whether a module can move *alone* is its outbound edges, because an edge that
  still points into `src-tauri` would make the arrow point back:

  - **Batch 1 — `report`, `termgrid`.** The only two with no outbound edge at
    all: both are `std`-only. They go first precisely because they test the
    move-and-re-export mechanism and nothing else.
  - **Batch 2 — `groupid`. Code-clean but not free**, and its cost was a *test*
    one worth stating here because nothing in the module itself showed it. The
    single-assembly-point guard in `src-tauri/tests/groupid.rs` scanned
    `CARGO_MANIFEST_DIR/src` and asserts, among other things, that no
    `impl AsRef<Path> for GroupId` exists. After the move that impl can only
    ever be written in `loomux-engine` (the orphan rule leaves nowhere else), so
    a scan confined to `src-tauri/src` would have been watching a directory the
    violation can no longer reach — green forever, enforcing nothing, while
    CLAUDE.md constraint 6 cites it as the enforcement. So the scan now walks
    **both** source roots, asserts per root that it found files, and asserts
    that the file *defining* `GroupId` was in scope — the last one being what
    survives the next move, since a file count cannot tell two roots from one
    root counted twice.

    Because that changes a tripwire's coverage, the batch did **not** take the
    pure-relocation exemption the others take: it owed, and produced, real
    red-before-green — a planted `impl AsRef<Path> for GroupId` and a planted
    second assembly point, each in `crates/loomux-engine/src`, each reddening
    the extended scan on CI. The generalizable rule, and the reason this
    paragraph outlives the batch: **when a type moves, ask where the violation
    can be spelled now, not where it used to live.** For a trait impl the orphan
    rule answers that exactly.

    `groupid` also brings the crate's first dependency, `serde` — `GroupId`'s
    hand-written `Deserialize` is the gate a persisted or hand-edited id is
    routed back through, so it travels with the type. It took `serde` without
    the `derive` feature, both impls being hand-written; batch 3 turned that on
    (below). `serde` is already in the shipped binary's linked graph, so the
    getrandom audit's ground is unchanged.
  - **Batch 3 — `lessons`, `notify`, and the helper lift that made them
    leaves.** Each had exactly one code edge left into `mod.rs`, and the useful
    part is what those edges turned out to be: `lessons` reached back for
    `tail_snippet` (a char-boundary-safe byte-suffix cut) and `notify` for
    `pr_number` (a PR-ref parse). Neither is registry state, neither touches
    `AppHandle`, and neither is a candidate for a host trait — they were
    stranded in `mod.rs` because `mod.rs` is one very large file, not because
    they are coupled to the desktop. So the two helpers moved into the engine
    **ahead of their callers**, as `loomux-engine`'s `text` module, and the two
    modules followed behind them.

    The rule worth carrying forward: **an edge into `mod.rs` is not
    automatically an edge into the registry.** Before a module gets held back
    for A3, read what it actually reaches for — a pure callee is cut by moving
    the callee, which costs a re-export, not by abstracting the caller, which
    costs a trait. The coupling map on #968 applied that test to what remains,
    and it is what makes the `workflow` cluster a "model batch" (shared data
    types and const tables lifted out of `mod.rs`) rather than trait work.

    `text` is deliberately its own module rather than filed beside either
    caller: both helpers have consumers past the module that forced the lift
    (`tail_snippet` also backs the pty exit notice; `pr_number` is reached from
    the merge-grant path, the board's PR lookup and the MCP argument parsers),
    and filing a shared helper inside one consumer makes every other consumer
    depend on that consumer for a reason the code does not have.

    Two costs this batch paid that the next one should expect. **A `pub(super)`
    whose caller stays behind becomes public API**: `notify`'s
    `check_is_pending`/`check_is_failing` are used by the `gh pr list` rollup
    and the merge queue's batch verdict, both of which stayed in `src-tauri`, so
    there is no visibility narrower than `pub` that still reaches them — worth
    asking each time whether the item is one the engine is content to expose,
    rather than only whether it compiles. And **a dependency a module derives on
    has to be declared, not inherited**: `notify` derives `Deserialize`, so the
    manifest now asks `serde` for `derive` (and adds `serde_json` for the `gh
    --json` walk). Resolver-2 unifies features across the packages in one build,
    so an undeclared `derive` would have compiled under CI's `--workspace` and
    failed only for someone building the engine alone.

    Pure relocation otherwise: no tripwire watches either helper, the moved
    modules' inline tests moved with them, and the integration suite stayed
    green with no test-logic edits — which is what the re-exports are for.
  - **Batch 4 — the shared data model** (`Role`, `Containment`, `CliCaps`,
    `SUPPORTED_CLIS`, `CLI_CAPS`, `EFFORT_LEVELS`, `CONTEXT_VARIANTS`,
    `cli_caps`, `cli_can_host`, `default_model`, `sanitize_model_opt`), lifted
    out of `mod.rs` into `loomux-engine`'s `model` module.

    **The first batch that moved something for the sake of what comes next**
    rather than because it had run out of edges. Batches 1–3 asked "what can
    move alone?"; this one asks "what has to be on the far side before
    `workflow` can move at all?" — and the answer is every symbol above.
    `Role` is a block's `kind:`; `CLI_CAPS` backs the knob remedies
    `parse_workflow` puts in its error messages; `EFFORT_LEVELS` and
    `CONTEXT_VARIANTS` are the closed vocabularies it rejects against;
    `sanitize_model_opt` normalises a block's `model:`. Move `workflow` first
    and all of that reaches back into `src-tauri` — the arrow pointing the
    wrong way in the module that most needs it not to. Data before the code
    that reads it is the ordering rule the rest of A2 should expect.

    ### `Role::template()` stays behind, and that is the design decision

    `Role` had two inherent methods welded to `include_str!("templates/*.md")`,
    and those four files are byte-pinned by `src-tauri/tests/fixtures/pre222/`
    with its own human re-bless procedure (see that directory's README).

    An inherent impl must live in the crate that defines its type. So carrying
    `template()`/`instructions_file()` along would have dragged `templates/*.md`
    — **and the fixture root that blesses them** — into `loomux-engine` as a
    side effect of moving an enum. Nothing would have failed. `cargo check` has
    no opinion about where a fixture root lives, the fixtures would have gone on
    passing from their new home, and the two surfaces that name the path (the
    README's procedure, CLAUDE.md's role-template convention) would have become
    quietly wrong. Product content and its blessing procedure are not something
    a refactor gets to relocate silently.

    So they are **free functions that stay in `src-tauri`**, next to the content
    they load: `role_template(Role) -> &'static str` and
    `role_instructions_file(Role) -> &'static str`. The price is a call-site
    rewrite, and the price is the argument: every missed site is a **build
    error**, on CI, before a human reads the diff. Given a choice between a
    mechanical failure the compiler enumerates and a silent one no test watches,
    take the loud one.

    Read this against batch 2 rather than instead of it — they are the same
    question answered in opposite directions. There, a source-scanning tripwire
    had to **follow** `GroupId` across the boundary, because the orphan rule
    moved the only place its violation could be written. Here, content had to
    **stay**, because an inherent impl would have moved the only place its
    mapping could be written. Neither "move everything with the type" nor "move
    nothing but data" is the rule. The rule is: **for each item on a moving
    type, ask whether it is the type's data or somebody else's content** — and
    where the answer is content, ask what stops noticing if it travels.

    Two smaller costs, both instances of things earlier batches already named.
    `Role::prefix` and `Role::as_str` were `pub(crate)`; a method's visibility
    belongs to the crate defining the type, so no re-export narrows them back
    and they are public API now. `default_model` and `sanitize_model_opt` were
    `pub(crate)` too, but they are free functions, so the re-export keeps them
    `pub(crate)` on this side exactly as batch 3 did for `tail_snippet`.

    ### What it owed in evidence, and what it did not

    The batch is a relocation, but it does not take the pure-move exemption
    whole, and the split above is why: the exemption is for a move *the existing
    suite already pins*, so the honest move is to establish that separately for
    each thing the move could break.

    - The **`Role` → template/file mapping** is genuinely pinned.
      `the_toggle_off_leaves_every_instruction_file_byte_for_byte_what_it_was`
      writes a default group's instruction files and byte-compares all four
      against `tests/fixtures/pre222/`, which covers the file *name* and the
      template *bytes* for every class that has them. The free-function rewrite
      cannot silently mis-map a role without reddening it.
    - `Role`'s **serde form** was pinned for four of five variants, and the way
      that got established is the part worth keeping. The first draft of this
      batch asserted that `Orchestrator` had no wire coverage; a planted
      `#[serde(rename)]` on it **disproved that** by reddening
      `sessions_backfill_from_audit_when_roster_predates_it`, which deletes
      `agents.json` and reads the class back out of the audit log — written
      through the same `Serialize`. `list_agents` covers the other three.
      **`Solo` is the only variant nothing reached**, and it is the one a
      rename would hurt most quietly, since a solo pane never traverses a spawn.
    - Nothing enforced `as_str`'s own claim to "match the `Serialize` rename",
      though `session_roles` records a class through one producer while
      `list_agents` emits it through the other.

    So the gap got the test and the rest did not: `model` pins all five variants
    against a literal wire table (a third party to both producers, since
    deriving either from the other pins nothing) and pins `as_str` against the
    same table. A containment-tier table test was written and then **deleted** —
    `every_capability_class_pins_its_deny_tier` already asserts that mapping,
    and the plant that should have evidenced a new copy reddened it plus five
    spawn-path tests instead.

    Two process findings this batch paid for, both worth having:

    1. **`cargo test` stops at the first failing target, and this crate's
       targets run after `src-tauri`'s.** So a plant that reddens anything in
       `src-tauri` prevents the engine's unit tests from running at all — the
       red says nothing about them. Every plant meant to evidence an engine test
       has to be one the rest of the suite does *not* catch, and the proof that
       it was is the `src-tauri` targets passing in the same run.
    2. **CI runs `npm test` before `cargo test`.** A batch that breaks a
       frontend test therefore cannot produce Rust red evidence at all until
       that is fixed, because the job never reaches the compiler.
  - **`digest` is not a leaf** despite reading like one. It calls
    `crate::sessions::yaml_field` and takes a `crate::opencodedb::TranscriptRow`
    — two modules staying in `src-tauri` — so it cannot move until those edges
    are cut or followed.
  - **`locks` must travel with `workflow`**, on which it depends for
    `ResourcePolicy` in both its body and its tests.
- **A3 — the traits.** `EventSink` + `PaneHost`, `OrchRegistry` drops
  `AppHandle`, `NullPaneHost` replaces the app-is-`None` branch. Per-PR bar: the
  integration suite green with **zero test edits**, plus one new crate-level
  test that drives a group headless end to end with nothing Tauri linked.
- **A4 — the registry.** `OrchRegistry`, the decision layer, and a PTY
  output-sink seam, so the crate compiles with no `tauri` anywhere in its tree.
  A CI step proves that from `cargo tree` rather than from this paragraph. This
  is the scoped subset of #847's Phase 3 the daemon needs, **not** the full
  `mod.rs` split.

They are serial because each rides the previous one's re-exports, and because
`src-tauri/src/orchestration/mod.rs` is the highest-conflict file in the repo:
two of these in flight at once is a merge-conflict machine, not parallelism.
A2 and A4 in particular want a quiet board.

Tests move with their modules. Note that inside the engine crate the
comctl32-v6 manifest machinery does not apply — that constraint (CLAUDE.md #4)
is about the Tauri app's Windows test executables, and
`src-tauri/tests/smoke.rs` must keep existing for it regardless.

## 7. Not in scope here

No listener, no socket, no wire format, no authentication — those are #888
tracks C and D, gated on the protocol note. The extraction is a pure refactor
that makes them *possible*; it grants no new capability and opens no new surface
by itself. If a slice of this work ever appears to, that is the bug.
