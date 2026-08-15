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

The remaining caller-supplied path identifiers are not this work. They split in
two: `session_id` and the agent id are **#925** (landed — they are validated
segments now, sharing `pathseg::PathSegment` with `GroupId`), while the
`ft_*`/`fm_*` roots and git `repo` are **#1042**, a root-admission registry
rather than a segment check, and that one is the stated merge blocker for the
listener slices. The extraction neither fixes nor worsens either; it just must
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

    **Amended by batch 5** (below): `role_instructions_file` did not stay. It
    loads no bytes, and `workflow::Block` calls it, so it followed `Role` into
    the engine; `role_template` stays, and it is the one this section's argument
    was ever about. The rule is asked of each item, not of the pair.

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
    `pub(crate)` too, but they are free functions, so the re-export keeps the
    flat `orchestration::…` spelling `pub(crate)` on this side exactly as batch
    3 did for `tail_snippet`.

    **Corrected in batch 5, because the original wording here was wrong in a way
    that would have compounded.** A `pub(crate) use` governs the *spelling it
    re-exports*, not the item: `mod.rs` also re-exports the whole module
    publicly (`pub use loomux_engine::model::{self, …}`), so every `pub` item in
    the engine's `model` is reachable as `orchestration::model::…` regardless.
    The claim that survives is **"no existing spelling widened"** — never "the
    public surface is unchanged". The reachability change is forced (an item
    must be `pub` in the engine to be callable from `src-tauri` at all) and
    harmless (`loomux-engine` is `publish = false`, an internal workspace
    boundary rather than a shipped API), and the right response to a forced,
    harmless change is to state it, not to contort the re-export chasing a
    literal "unchanged". Every batch after this one inherits the same shape, so
    it is stated once, here.

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
  - **Batch 5 — the `workflow` cluster** (`workflow`, `profiles`, `locks`),
    the batch every earlier one was clearing the way for. It is the first that
    could not be a single module, and the reason is a shape rather than a size.

    ### A cycle is a partition, not an ordering

    `profiles` calls `workflow::{kind_from_str, resolve_profile_path}`;
    `parse_workflow` calls `profiles::sanitize_allow`. That is a cycle —
    unremarkable inside one crate, unrepresentable across two. So no batch
    order exists that moves either alone: **once two modules are mutually
    dependent, the only remaining question is where to draw the line around
    them, not which goes first.** `locks` is in because `LockTable::sync` is
    typed on `workflow::ResourcePolicy` in its body and its tests.

    The line was drawn tight, and the two near-misses are the useful part.
    `mergeq` reads like a fourth member; every mention of it in `workflow` is
    prose — a doc link, and references inside doc comments — and **prose is not
    an edge**. The check that decides the question is "does a `mergeq` path
    appear in a body?", not "how many times is it named"; the first draft of
    this paragraph asserted a count and got it wrong. `mqdriver` was `workflow`'s
    heaviest consumer, and **batch 5 left it behind** deliberately — it reached
    `capture_raw_with_timeout`, glossed here at the time as "i.e. the pane host,
    which is slice A3" and retracted in the amendment below. That one
    is worth stating as a rule because it is the intuition that misleads:
    **an inbound edge never blocks a move.** `mqdriver` went on spelling
    `super::workflow::…` and never learned anything had changed, because that is
    what the re-export is. Only *outbound* edges decide what a batch contains.

    (Amended by batch 9: "`capture_raw_with_timeout`, i.e. the pane host" was
    wrong on the second half. Re-measuring the cluster found no host edge in it
    at all — it is `std::process` + `std::thread`, and it moved as an ordinary
    leaf. The batch-5 conclusion is unaffected, since `mqdriver` had other
    reasons to stay and an inbound edge was never the question; what the
    correction costs is the *reason*, which is why batch 9's entry restates it
    rather than only fixing the phrase.)

    ### The edge batch 4 created, and why the map was stale

    Batch 4 split `Role::template()`/`Role::instructions_file()` into free
    functions and left **both** in `src-tauri`. But `Block::instructions_file`
    is a `workflow` method and calls `role_instructions_file` — so batch 4
    handed batch 5 a fresh outbound edge into `mod.rs`, one that did not exist
    when the cluster's edge map was drawn on #888. The generalizable finding:
    **the batch that lifts a data layer ahead of its caller can leave a new
    edge pointing the wrong way**, because splitting a type's methods off it
    decides where those methods live, and the caller that forces the question
    may not have moved yet. Re-derive the edge set from the source at the start
    of every batch; a map drawn one batch ago describes a tree that has since
    changed.

    So batch 5 split batch 4's pair, and the split is the sharper reading of
    batch 4's own rule rather than a reversal of it. `role_template` **stays**:
    it loads `templates/*.md`, which `tests/fixtures/pre222/` byte-pins under a
    human re-bless procedure, and that content plus its blessing procedure is
    exactly what must not relocate as a side effect of a refactor.
    `role_instructions_file` **travels**: it loads nothing and maps a class onto
    the file name the *group directory* carries. Batch 4 kept the two together
    on the ground that "the name and the bytes are one mapping", and that
    pairing turned out to be the weaker half of its own argument — the argument
    was about content, and a bare `"worker.md"` is not content. Its behaviour is
    pinned identically either way; see below.

    ### What it owed in evidence

    This one **does** take the pure-relocation exemption, and unlike batch 2 and
    batch 4 it is entitled to: nothing here changes a tripwire's coverage, and
    every behaviour the move could break is already pinned by tests that did not
    move and were not edited. Established per behaviour rather than asserted
    wholesale, since that is the standard batch 4 set:

    - the block parser's **closed vocabularies** and `kind_from_str`'s
      reject-never-coerce shape — `src-tauri/tests/workflow.rs`, which drives
      `parse_workflow` through `orchestration::workflow` (i.e. through the new
      re-export) across its whole accept/reject surface;
    - **`resolve_profile_path`'s traversal refusal** — the same file's escape
      table (`..`, absolute, drive-letter, both separators), which is security
      behaviour and the one this batch most wanted pinned by a third party;
    - **`sanitize_allow`** — the same file's hostile-input table;
    - **`ResourcePolicy`** and the lock table — `tests/workflow.rs`'s default
      pin plus the wired multi-slot path in `tests/orchestration.rs`;
    - the **`Role` → instructions-file name** mapping, the one item that
      actually changed crates —
      `the_toggle_off_leaves_every_instruction_file_byte_for_byte_what_it_was`
      writes a default group's four instruction files and byte-compares them
      against `tests/fixtures/pre222/`, which pins the name as well as the
      bytes. A mis-mapped name reddens there wherever the function is defined,
      which is what makes the move safe rather than merely compiling.

    The moved modules' inline `#[cfg(test)]` tests moved with them and are
    engine unit tests now; nothing linking the lib changed target.

    Two dependencies travel with `workflow`: `serde_norway` (the YAML parser
    `parse_workflow` is built on) and `sha2` (`body_digest`, the hash the merge
    gate compares). Both are already in the shipped binary's graph via
    `src-tauri`, whose manifest carries the getrandom audit for each, so no new
    package joins the lock and no new getrandom edge appears — but batch 3's
    rule is why they are declared rather than assumed: feature unification is
    not crate-name unification, and an undeclared crate does not compile at all.
    Worth naming as the batch's one real process miss, since it cost a CI round:
    the pre-move edge sweep grepped a *guessed list* of crate names and found
    neither. Enumerating what the files actually import — rather than checking
    for the imports you expect — is the version of that step that works.
  - **Batch 6 — `mergeq`, `mergeqview`.** The batch 5 declined by name, taken
    now for the reason it was declined then: `mergeq`'s every import comes from
    `workflow`, so while `workflow` sat in `src-tauri` the edge pointed back,
    and once it landed in the engine the edge pointed forward. Nothing in either
    file changed but the prefix on an import — `super::workflow::…` and
    `super::mergeq::…` became `crate::…`, and the two `#[cfg(test)]` modules'
    `crate::orchestration::…` imports became `crate::…`. The re-export is the
    plain module form — the two `pub mod` lines in `orchestration/mod.rs` become
    `pub use loomux_engine::{mergeq, mergeqview};` — with no flat item
    re-export beside it, because every consumer reaches these items through the
    module (`super::mergeq::{GateSpec, …}`, `orchestration::mergeqview::project`)
    rather than as a bare `orchestration::…` name. Batches 3 and 4 needed the
    extra lines; this one does not, and that is a fact about the call sites
    rather than a difference in policy.

    **A cycle decides a batch's contents; a chain only invites them.** That is
    the finding, and it is the counterweight to batch 5's. There, `workflow` and
    `profiles` were mutually dependent, so no order existed that moved either
    alone and the only question was where to draw the line. Here `mergeqview`
    reads `mergeq` and nothing else, and `mergeq` never reads back — a chain, so
    `mergeqview` *could* have stayed in `src-tauri` and reached the engine
    through the re-export exactly as `mqdriver` then did, for the six batches
    before 12a took it. It comes because it is a
    pure projection with no other edge and nothing left in the Tauri half to be
    near, which is a judgement rather than a constraint. A batch that cannot say
    which of the two shapes it is has not drawn its own line.

    It is also where the inbound-edge rule meets real code. Batch 5 established
    that **prose is not an edge**, having found only doc mentions of `mergeq` in
    `workflow`; the half that misleads is the other one, and this batch supplies
    it. `mqdriver` and `mqloop` imported from both moved modules in their
    *bodies* (`use super::mergeq::{new_batch_id, scratch_branch, …}`,
    `use super::mergeqview::MERGE_QUEUE_FILE`, a `super::mergeq::recheck_gate`
    call), and **batch 6 left both behind**, for the edges they had at the
    time — batch 9 re-measured those and none of them was a host edge; see its
    entry below. Both went on spelling `super::` unchanged and compiled against
    the re-export: **a body-level inbound edge is a genuine edge and still does
    not block a move.** The same
    goes for the `#[tauri::command]` `orch_merge_queue`, which stays and calls
    `mergeqview::merge_queue_view` through the same line.

    Two costs earlier batches taught, both nil this time and both stated because
    the *check* is the point rather than the answer. No visibility widened —
    neither module has a `pub(crate)` or `pub(super)` item, so batch 3's
    "a batch that leaves a caller behind converts that caller's `pub(super)`
    into public API" had nothing to convert. No dependency joins: reading the
    moved files' own `use` lines gives `serde` and `serde_json` (`mergeq`),
    `serde_json` and `std` (`mergeqview`), all declared here since batch 3. That
    is batch 5's process miss applied rather than repeated — enumerate what the
    files import, never check for the imports you expect.

    Batch 2's question ("where can the violation now be spelled?") is asked and
    answered nil too: `tests/groupid.rs`'s scan already walks both source roots
    and is line-content-based, so a file moving *between* two scanned roots
    changes nothing it can see. `mergeqview`'s one `.join` takes a `&Path` and a
    file-name constant, and it was in scope before the move as it is after.

    The batch takes the pure-relocation exemption. Every behaviour it could
    break is pinned by `src-tauri/tests/mergequeue.rs`, which reaches both
    modules through `loomux_lib::orchestration::…` — the re-export — and was not
    edited, plus the modules' own inline tests, which travel with them and are
    engine unit tests now.
  - **A3 batch 8 — four small pure items, item-lifts into `model` and `text`
    rather than a whole-file move** (plan-558). `Delivery` (the
    `deliver_prompt`-lifecycle enum) joins `model`; `LOOMUX_NOTICE_MARKER`
    joins `text` beside `pr_number`; `DEFAULT_IDLE_TICK_MINUTES` and
    `DEFAULT_INTAKE_POLL_MINUTES` join `model` too, because the latter is
    defined *in terms of* the former and the two have to travel together.
    Batch 7 (`obs`, run in parallel by another worker) is the other half of
    this tranche; this entry covers only batch 8's four items.

    Takes the pure-relocation exemption: `Delivery`'s kebab-case
    `#[serde(rename…)]` attrs move verbatim, so its wire/persisted shape
    (`queue.json`'s `delivery_kind`) is byte-identical, and the queue snapshot
    round-trip tests that pin that shape do NOT move with it. The integration
    suite needed zero edits — every `Delivery::…` call site across `mod.rs`,
    `queue.rs`, `queuestate.rs`, `mcp.rs` and the integration suite reaches the
    moved enum through the flat `orchestration::…` re-export unchanged, and the
    same is true of every `LOOMUX_NOTICE_MARKER` use — `mod.rs`'s own (both
    code and doc-comment), `queue.rs`'s live check at `queue.rs:1034`, and the
    integration suite's — and of the two consts' `mod.rs`/`intake.rs` uses.
    (Per #973: state that fact, not a count of it — a grep-derived number rots
    the moment either file gains or loses a use, and is only worth stating
    where the point is the number itself, e.g. counting toward a cap.)
    **Amended by batch 11:** `intake.rs` is in the engine now and reaches both
    consts as `crate::model::…`, so it is no longer one of the callers this
    paragraph counts on the `src-tauri` side.

    **Visibility widened, batch-3 precedent — three items, one item unchanged.
    State the reachability precisely rather than reach for "unchanged" or
    "narrowed" — model.rs:61-73 is the standing correction for this exact
    phrasing, and it applies here too:**
    - `Delivery::wait_ready` was bare module-private in `src-tauri`. It is
      `pub` in the engine now, forced by the crate boundary (`mod.rs`'s own
      callers are a different crate), with no re-export able to narrow it back
      — a method's visibility is the defining crate's to set, the same fact
      batch 4 states for `Role::prefix`/`Role::as_str`.
    - `DEFAULT_IDLE_TICK_MINUTES` and `DEFAULT_INTAKE_POLL_MINUTES` were both
      bare module-private consts. They are `pub` in the engine now (forced,
      same reason). `mod.rs`'s `pub(crate) use` narrows only the FLAT spelling
      (`orchestration::DEFAULT_IDLE_TICK_MINUTES`, the one `mod.rs`/`intake.rs`
      actually call) back to "this crate". It does NOT narrow the item overall:
      `mod.rs` already re-exports the whole `model` module publicly (`pub use
      loomux_engine::model::{self, …}`), so both consts are also reachable as
      `orchestration::model::DEFAULT_IDLE_TICK_MINUTES` — and, since that path
      crosses no crate-private boundary, as
      `loomux_lib::orchestration::model::DEFAULT_IDLE_TICK_MINUTES` from
      outside the crate too.
      **Amended by batch 11**, which is where this sentence stopped being
      true and is worth flagging as a pattern rather than a typo: the flat
      re-export of `DEFAULT_INTAKE_POLL_MINUTES` existed for exactly one
      caller, `intake.rs`, and that caller moved into the engine — so the
      line has no consumer left and comes off, leaving `mod.rs`'s
      `pub(crate) use` covering `DEFAULT_IDLE_TICK_MINUTES` alone (which
      `intake.rs` never called in the first place). The identical claim was
      written on three surfaces — here, `model.rs`'s two const docs, and
      `mod.rs`'s batch-8 comment — and batch 11 initially corrected two of
      them and missed this one. That is the #878 signature exactly: grep the
      **entity** a claim names, never the phrasing you just rewrote.
      Forced and harmless otherwise, on the same terms `model.rs`
      states for `default_model`/`sanitize_model_opt`: an item must be `pub`
      here to cross the boundary at all, and `loomux-engine` is
      `publish = false` — "public" means reachable by a sibling crate in this
      workspace, not a shipped API promise.
    - `LOOMUX_NOTICE_MARKER` was already `pub` in `src-tauri`, so nothing
      widens there.
  - **`digest` is not a leaf** despite reading like one. It calls
    `crate::sessions::yaml_field` and takes a `crate::opencodedb::TranscriptRow`
    — two modules staying in `src-tauri` — so it cannot move until those edges
    are cut or followed.
- **A3 — the host-edge tier.** Planned in detail on #888 (plan-558): the
  measured edge map found that five of the six remaining orchestration modules
  are blocked not by `AppHandle` but by six pure `mod.rs` items plus
  `crate::obs`, so A3 opens with batches that close those edges and continues
  the A2 numbering. `EventSink` + `PaneHost`, `OrchRegistry` dropping
  `AppHandle` and `NullPaneHost` replacing the app-is-`None` branch land at the
  end of it, against the real Tauri edges rather than ahead of them. Per-PR bar
  is unchanged: the integration suite green with **zero test edits**, plus —
  for the trait work — one new crate-level test that drives a group headless end
  to end with nothing Tauri linked.
  - **Batch 7 — `obs`, split at its own section marker.** The first batch that
    moved *part* of a file, and the cut was not invented for the occasion:
    `obs.rs` had fenced its Tauri items off behind a
    `// ---------- next-launch notice (Tauri surface) ----------` comment since
    #53. Everything above it (the panic hook, breadcrumbs and rotation,
    `stamp`/`civil_from_days`, `data_root`/`logs_dir` and the `LOOMUX_DATA_DIR`
    validation, the `running.lock` sentinel, `LockExt`) is `std` + `dirs` and is
    now `loomux_engine::obs`; `StartupNotice` and the `#[tauri::command]`
    `take_startup_notice` stay, in a `src-tauri/src/obs.rs` that is otherwise a
    `pub use` of the engine module. The move itself cost no call-site edits at
    all: every `obs::…` path in `src-tauri` spells what it always did and
    resolves through the shim. One call site did change, and it belongs to the
    `env!` fix below rather than to the move — `lib.rs` now passes the app
    version to `install_panic_hook`. The re-export is written out item
    by item rather than as a glob — what `src-tauri` re-exports should be a list
    somebody chose, not whatever the engine module makes public next.

    It goes first in A3 because the rest of the tier waits on it: batch 9's
    capture cluster locks through `lock_safe`, and without that batch there is
    no `mqdriver` and no `mqloop`. The two alternatives were measured on #888
    and both lose to the cut. Moving the file **wholesale** is dead on arrival —
    `take_startup_notice` takes a `tauri::State`, so it means the engine links
    Tauri. Leaving `obs` in `src-tauri` **behind a host trait** costs the
    largest trait surface of the three options for two helpers, and cannot
    actually be paid: `LockExt` is an inline extension trait on
    `std::sync::Mutex` (`m.lock_safe()`), not reachable through a trait object
    as called, so every engine consumer would need a threaded `&dyn` through
    signatures that take none today, a global registration hook, or its own copy
    of the poison-recovery policy — a second implementation of the one thing
    that must not have two. **Look for the boundary the module's author already
    drew before reaching for a trait.**

    ### `env!` is an edge, and no grep for `super::` finds it

    This is the batch's real finding and it generalises past `obs`. Every batch
    so far enumerated a module's outbound edges by searching for
    `super::`/`crate::`. `obs.rs` has none — and it still had one:
    `record_crash` builds the crash log's `version:` line from
    `env!("CARGO_PKG_VERSION")`. That macro names *the crate the file is
    compiled in*, so the move re-points it, and this crate's version is
    deliberately `0.0.0` (a placeholder, not the release number — see its
    manifest). A verbatim move would have made every crash log read
    `version: 0.0.0` while `doc/design/crash-observability.md` goes on promising
    the loomux version. Nothing fails to compile. Nothing goes red.

    So `install_panic_hook` takes `app_version: &'static str` and
    `src-tauri/src/lib.rs` passes `env!("CARGO_PKG_VERSION")` from the crate
    where the macro means what it says — the identity is injected at the one
    startup entry point rather than read from the ambient crate, and every call
    site is compiler-checked. The rule to carry into batches 8–12: **sweep a
    moving file for `env!`, `option_env!`, `file!`, `module_path!` and
    `include_str!` alongside its `use` lines.** Each is a compile-time reference
    to the crate the file happens to live in, and each moves house silently.
    (`obs.rs` had exactly one, checked by grep over the whole moved region.)

    That one item is a **behaviour change and does not take the pure-relocation
    exemption** the rest of the batch takes. It was evidenced red-before-green on
    CI rather than asserted: the first commit is the naive move carrying an
    assertion that the crash log does not name the engine's placeholder version,
    red on all three platforms; the second threads the version and turns it
    green, with the assertion restated against an explicitly passed version so
    it stops depending on the release number. Local `cargo` is banned (#488), so
    CI is where red-before-green happens — which works, and is worth knowing for
    any later batch that finds a real defect on the way through.

    Two dependencies. `dirs` (what `data_root` calls) is already in the shipped
    binary's linked graph via `src-tauri`, so no package joins the lock — but it
    is the first engine dependency that appears in a getrandom query at all, and
    the engine manifest now says why that is fine in the one place an auditor
    will look: the edge is `getrandom → redox_users → dirs-sys → dirs`, gated to
    `cfg(target_os = "redox")`, compiled on none of the three platforms loomux
    ships or tests on. `tempfile` (default features off, so no `getrandom`
    feature) is the crate's first `[dev-dependencies]` entry, for the inline
    tests' temp trees; dev-dependencies are never built for a downstream crate,
    so they cannot reach the shipped binary regardless. `src-tauri`'s own note
    on the engine dependency said "the engine crate has no dependencies at all",
    which stopped being true at batch 2 and would have been actively misleading
    here; it now states the invariant that is actually load-bearing — every
    engine dependency so far is one `src-tauri` already depends on directly, so
    the linked graph is unchanged by the extraction.

    Nothing else moves. No tripwire relocates (the `tests/groupid.rs` scan
    already lists both source roots), no role template is touched so no pre222
    re-bless is owed, no visibility narrows or widens beyond `pub` items staying
    `pub`, and `src-tauri/tests/` is untouched — which is the proof that the
    re-export surface is complete rather than a claim about it.
  - **Batch 9 — `subproc`, `fsatomic`: the two "host primitives" that were not
    host calls.** Two clusters lifted out of `orchestration/mod.rs` into two new
    engine modules:

    - **`subproc`** — bounded child-process capture (#656, split out of
      `OrchRegistry::capture_with_timeout` by #698): `GH_CAPTURE_TIMEOUT` and
      its three sibling constants, `wait_bounded`, `capture_raw_with_timeout`
      and the injected-wait `capture_raw_inner` behind it,
      `abandon_child_and_readers`, and the process-wide
      `GH_CAPTURE_LEAKED_READERS` backlog with its sweep, its ceiling and its
      `#[doc(hidden)]` test seams.
    - **`fsatomic`** — durable whole-file replace (#133): `atomic_write` and the
      `ATOMIC_WRITE_SEQ` counter that keeps two concurrent writers' `.tmp`
      siblings apart. (The identically-shaped `atomic_write`s in
      `src-tauri/src/fileedit.rs` and `src-tauri/src/uistate.rs` are separate
      copies serving the editor and the UI-state file; consolidating them is a
      question of its own and is **not** this batch.)

    ### Why two modules and not one `hostio`

    They were lifted in one batch and belong in none. They share no symbol, no
    design note and no failure mode: `subproc` exists because a child parked on
    a stalled connection stops the single poll loop and every `notify_when`
    notice with it; `fsatomic` exists because a disk-full `fs::write` truncated
    `tasks.json` and destroyed a live board. The only thing they have in common
    is the batch that moved them. **A batch is a unit of moving, not a unit of
    grouping** — batches 5 and 6 argued which modules *must* travel together,
    and this is the mirror question: items that travel together do not thereby
    belong together, and a module named for what its members share with the
    batch is a name the next reader cannot use.

    ### "Host primitive" was a label, not a measurement

    Both were called pane-host calls before anyone re-read them. Batch 5 left
    `mqdriver` behind on the stated ground that it "reaches the pane host
    (`capture_raw_with_timeout`)", and §6's batch-5 entry said so in this file.
    Re-deriving the edge set at the start of this batch found no host edge in
    either cluster: no `tauri`, no `AppHandle`, no pty, no pane — `std::process`
    + `std::thread` for one, `std::fs` for the other. They moved as ordinary
    `std` leaves, and the A3 trait work they were supposedly waiting on was
    never theirs to wait for. Batch 5's rule survives its own counter-example
    and this is the sharpest instance of it: **re-derive the edge set from the
    source at the start of every batch** — including from the notes this repo
    wrote down, which describe a tree as it was and are not evidence about the
    tree as it is.

    A consequence worth carrying into the next batch, stated as what it is (a
    grep over the source, not a compiler's verdict), and stated carefully
    because two earlier drafts of this paragraph got it wrong in opposite
    directions: **batch 9 left `mqdriver.rs` and `mqloop.rs` keeping their
    `super::` call sites into the moved items — `super::capture_raw_with_timeout`
    (`mqdriver.rs:173`) and `super::atomic_write` (`mqloop.rs:135`) — and those
    resolved through the re-export into the engine, with no source edit on either
    side.** That is
    what a completed move looks like from the caller's seat, not a remaining
    problem: the call site was unchanged *because* the re-export was doing its
    job.
    The other modules they name were already across — `notify` since batch 3,
    `workflow` since batch 5, `mergeq`/`mergeqview` since batch 6. (`mqdriver`
    crossed in batch 12a and spells `crate::subproc::capture_raw_with_timeout`
    now; `mqloop` crossed in batch 12b and spells `crate::fsatomic::atomic_write`
    now. The sentence is left standing, in the past tense that describes what
    batch 9 did, because what it is about — a caller not noticing — is the thing
    worth keeping, and rewriting it to describe today's tree would delete the
    evidence for it *and* go stale again at the next batch.)

    What had NOT gone away **as of batch 9**, and was expected: `mqloop` reached
    `super::mqdriver::` throughout its body, and `mqdriver` was still in
    `src-tauri`, so those resolved to `src-tauri`'s `mqdriver` and not to the
    engine. That was a same-tier reference between two files at the same stage
    of the extraction, not an unresolved dependency on the Tauri half — the pair
    was expected to move in A3's later batches, which is what a chain looks like
    while neither end has gone yet. (Both ends have gone now: `mqdriver` in
    batch 12a, `mqloop` in 12b, and every one of those call sites is a
    `crate::mqdriver::` path inside the engine. The paragraph is kept in the
    past tense rather than deleted because the *distinction* it draws — a
    same-tier reference is not a Tauri-half dependency — is what a future batch
    facing a half-moved pair needs, and that outlives the pair it was written
    about.)

    Both wrong drafts are worth keeping visible, because they are the two ways
    this particular sentence fails. The first said "everything resolves into the
    engine", which ignored the `super::mqdriver::` references entirely. The
    second said "no remaining edge into the moved capture cluster", which is
    refuted by the two call sites named above — and by its own next clause,
    which listed them. **A sentence whose following clause contradicts it is not
    a wording problem**; it means the claim was written to sound narrow rather
    than derived from what the grep returned.

    ### Edges and visibility

    `subproc`'s single outward edge is `lock_safe` (the backlog `Mutex`), which
    is `crate::obs::LockExt` here since batch 7 — the dependency batch 7 named
    as its reason for going first, now discharged. `fsatomic` has no outward
    edge at all. Neither pulls a dependency: both are `std`, so no manifest and
    no lockfile line changes, and CLAUDE.md constraint 2 is satisfied the way
    `fsatomic`'s own header states — a std atomic for unique temp names,
    deliberately no `tempfile`.

    `mod.rs` re-exports both as **curated item lists** (#988), never
    `pub use module::{self}`, so no `orchestration::subproc::…` or
    `orchestration::fsatomic::…` path exists and the private members of each
    cluster stayed private in the engine rather than being widened to make a
    move compile. One item's visibility is forced wider: `atomic_write` was
    `pub(super)` in `mod.rs` and must be `pub` in the engine to be callable
    across the crate boundary, so `loomux_engine::fsatomic::atomic_write` is
    that crate's public API now. The `pub(super) use` in `mod.rs` fixes the
    reach of the flat `orchestration::atomic_write` spelling and does **not**
    narrow the item — the same correction `model.rs` carries, restated rather
    than assumed. Harmless because `loomux-engine` is `publish = false`: public
    means reachable by a sibling crate in this workspace.

    ### What it owed in evidence

    A **pure relocation**, exemption taken whole: no behaviour is added or
    changed, and every behaviour the move could break is pinned by tests that
    neither moved nor were edited. `src-tauri/tests/orchestration.rs` drives the
    capture cluster through the flat re-export across every arm of it — the
    ceiling (`gh_capture_admitted` against `GH_CAPTURE_MAX_LEAKED_READERS`), the
    bounded wait's both verdicts, the non-zero-exit-as-data contract, the forced
    wait-failure arm and the parked-reader accounting — and `atomic_write` is
    exercised by every state-file round-trip in the same suite. (No call-site
    count is given: it rots the moment either file gains or loses a use, and the
    arms are what the coverage claim is about — #973.)
    **`src-tauri/tests/` is untouched**, which is the proof the re-export surface is complete
    rather than a claim about it. The `#[doc(hidden)]` seams travel with their
    cluster; no test moved crate, since the cluster's coverage was never inline.
  - **Batch 10 — the delivery queue: `queue`, `queuestate`.** The pure core of
    the per-pane FIFO (#445/#468/#467 — admission and coalescing, the flush
    plan, the `queue.json` snapshot and its recovery split, the archive, the
    audit-derived orphan view) and the two mutable maps `orchestration/mod.rs`
    used to hold as plain fields (#562's `QueueMap`, whose only `&mut` door
    writes the snapshot on the way out; #497's `DrainerRegistry`, whose only
    removal is generation-checked).

    `queuestate`'s module doc argues that the **file boundary is the
    mechanism** — Rust's privacy is per-module, and `mod.rs` is one 30k-line
    module, so "the only way to mutate `queues` is through the sanctioned path"
    became a claim `rustc` checks only once the maps lived in a module of their
    own. That argument survives this move untouched, because it was never about
    which *crate* the module sits in. Worth stating rather than assuming: a
    refactor that relocates the thing an invariant is spelled in should say
    whether the invariant still holds, and here it does, for the same reason it
    did before.

    A **chain, not a cycle**, in batch 6's sense: `queuestate` names `queue` and
    `queue` never names back, so `queue` could have moved alone. They travel
    together because `queuestate`'s only other edges are `GroupId` and
    `obs::LockExt`, and because the maps have nothing left in the Tauri half to
    be near — a judgement, not a constraint, and batch 6's rule is that a batch
    which cannot say which of the two shapes it is has not drawn its own line.

    ### The re-export shape is a call-site measurement, not a house style

    Batch 9 re-exported as **curated item lists** (#988) and this batch uses the
    plain **module** form batch 6 used — `pub use loomux_engine::{queue,
    queuestate};` — and the difference between them is worth having, because
    "always curate" reads like the safer rule and is not.

    A curated item list buys exactly one thing: it stops a module's newly-`pub`
    items from becoming reachable under an `orchestration::…` path they never
    had. That was real in batch 9, whose `atomic_write` was `pub(super)` and had
    to widen. Here it buys nothing and costs everything. **Neither `queue.rs`
    nor `queuestate.rs` contains a single `pub(super)` or `pub(crate)` item**,
    so the crate boundary force-widens nothing at all; each file's private
    members — `lenient_group_id`, `flush_cause_clause`, `constituent_banner`,
    `age_clause`, `archive_line_version`, `FLUSH_ITEM_OVERHEAD`,
    `QueueDirty::write_needed`, and both maps' `inner` fields — stay private in
    the engine exactly as they were. And `pub mod queue` already sat under
    `pub mod orchestration`, so `loomux_lib::orchestration::queue::…` reached
    precisely this set of items before the move and reaches precisely it after.

    That last sentence is deliberately scoped, because the unscoped version of
    it is the error `model.rs`'s standing correction is about and this batch is
    the one most tempted by it. The two claims worth separating: **no item
    widened** — not one visibility keyword in either file differs from what it
    was, which is the strong claim earlier batches could not make and is true
    here only because there was nothing to widen — and **the `orchestration::`
    spelling reaches the identical set**. What is *not* claimed is that nothing
    became reachable anywhere: `loomux_engine::queue::…` is a new spelling, as
    `loomux_engine::model::…` and every predecessor was, because an item must be
    `pub` in the engine to cross the boundary at all. That is inherent to the
    move rather than to the re-export shape — a curated item list would not have
    prevented it either — and it is harmless on the standing terms:
    `loomux-engine` is `publish = false`, so "public" means reachable by a
    sibling crate in this workspace, not a shipped API. **A batch that says
    "reachability unchanged" without naming which spelling it means has
    over-claimed**, and the item-list batches are not exempt from that either.

    The cost side is what settles it. Every consumer — `mod.rs` and
    `src-tauri/tests/orchestration.rs` alike — spells the MODULE path
    (`queue::QueuedDelivery`, `queuestate::QueueMap`), never a flat
    `orchestration::…` name, because these modules were always `pub mod`
    rather than items lifted out of `mod.rs`. A flat item list would therefore
    preserve **no** call site, and rewriting the integration suite to suit the
    re-export style would forfeit the per-PR bar (zero test edits) that is the
    evidence the move is behaviourally silent. The rule to carry into batches
    11–12: **pick the re-export shape from what the callers spell, and take the
    item list when it buys a narrowing that is real.** A curated list that
    widens nothing and preserves nothing is ceremony, and ceremony that edits
    the test suite is worse than none.

    ### Edges, dependencies, macros

    Every outbound edge was already across, which is why this batch is large in
    lines and small in argument. `queue` reaches `GroupId` (batch 2),
    `model::Delivery` (batch 8) and `text::LOOMUX_NOTICE_MARKER` (batch 8);
    `queuestate` reaches `GroupId`, `queue` itself and `obs::LockExt` (batch 7),
    with `model::Delivery` appearing only inside its inline tests. Nothing had to
    be lifted ahead of them. (Corrected in batch 11, and the shape of the error
    is the reusable part: this paragraph originally wrote `queuestate`'s set as
    "**those**, `queue` itself, and `obs::LockExt`" — folding in `queue`'s three
    by reference and thereby claiming a `LOOMUX_NOTICE_MARKER` edge `queuestate`
    does not have. Only `queue.rs` references that constant. **An edge list
    written as a delta against the previous module's is not an edge list**; it
    reads as one, it is shorter, and it is how a module acquires an edge in the
    prose that it never had in the source. Enumerate per module, from the
    module's own `use` lines and bodies. `lib.rs`'s header and
    `orchestration/mod.rs`'s batch-10 comment both stated the set correctly, so
    this was the one surface carrying it — checked by grepping the *entity*
    across the tree rather than assumed from the phrasing, which is the repo's
    standing rule for a corrected claim.) Every edit inside the two files is
    the prefix on one of those paths — `super::GroupId` → `crate::groupid::
    GroupId`, `super::Delivery` → `crate::model::Delivery`,
    `super::LOOMUX_NOTICE_MARKER` → `crate::text::LOOMUX_NOTICE_MARKER` (the
    live check and its intra-doc link both), `super::queue::…` → `crate::queue::
    …` — plus the same rewrite inside the moved `#[cfg(test)]` modules, whose
    `super::super::` reached `orchestration` and now would reach the crate root.

    No dependency joins: reading the files' own `use` lines gives `serde` and
    `serde_json` for `queue`, `std` plus `obs::LockExt` for `queuestate`, all
    declared since batch 3 — batch 5's process rule applied rather than
    repeated. Batch 7's macro sweep is clean: no `env!`, `option_env!`, `file!`,
    `module_path!` or `include_str!` appears anywhere in either file, which
    matters because `queue`'s `SNAPSHOT_VERSION`/`ARCHIVE_LINE_VERSION` are
    hand-written constants rather than anything derived from the crate's own
    version — had either been an `env!`, the engine's placeholder `0.0.0` would
    have silently re-versioned every snapshot on disk.

    Batch 2's question — where can the violation be spelled now? — is asked and
    answered nil. `tests/groupid.rs`'s scan already walks both source roots, so
    a file moving between two scanned roots changes nothing it can see, and
    neither file joins a group id onto a path: `queue`'s only `GroupId` uses are
    a struct field and the lenient deserializer that routes a persisted string
    back through `GroupId::parse`, which is the gate working as designed.

    ### What it owed in evidence

    A **pure relocation**, exemption taken whole. Nothing is added or changed;
    every behaviour the move could break is pinned by tests that neither moved
    nor were edited. `src-tauri/tests/orchestration.rs` drives the queue's whole
    policy surface through the re-export — admission and the coalesce, the
    capacity/pressure notices, the flush plan and its stranded-marker arm, the
    `queue.json` snapshot round-trip and `split_recovered`'s marker/entry split,
    the archive round-trip, and the orphan derivation — and the two files' own
    inline `#[cfg(test)]` modules (including `queue`'s four property suites and
    `queuestate`'s real-removal tests) travel with them and are engine unit
    tests now. **`src-tauri/tests/` is untouched**, which is the proof the
    re-export surface is complete rather than a claim about it.

    ### Remaining same-tier edges

    What stays in `src-tauri` and still reaches these modules, so the next batch
    does not have to re-derive it: `mod.rs`'s impure half — `enqueue_text`,
    `deliver_now`, `run_queue_drainer`, `persist_queues`,
    `recover_persisted_queue`/`readmit_recovered`, the depth/pressure emitters
    and the orphan command — spells `queue::…` and `queuestate::…` unchanged and
    compiles against the re-export. That is a completed move seen from the
    caller's seat, not a remaining problem (batch 9's correction, restated
    because it is the sentence that keeps being written wrong).
    `queuestate::QueueSnapshotWriter` is implemented for `OrchRegistry` in
    `mod.rs` and for the module's own `SpyWriter` in the engine, and the
    `src-tauri` half staying behind is fine — the trait *is* the seam, the impl
    is a local type impling a foreign trait so the orphan rule is satisfied, and
    an inbound edge never blocks a move. What is genuinely still same-tier is
    `mqloop` → `mqdriver`, unchanged since batch 9 — two files at the same stage
    of the extraction, which move together in a later batch. (They did not, in
    the end, move together: batch 12a took `mqdriver` alone and 12b took
    `mqloop`. Splitting them cost the re-export module batch 12a's entry argues
    for, and bought two reviewable diffs instead of one across the two largest
    files in the feature.)
  - **Batch 11 — `intake`, the idle-tick intake gate's pure core**
    (#332/#429/#795/#864/#778): the host-side, zero-token diff of what changed
    on GitHub since the last poll — label deltas, PR check-state transitions, PR
    comment/review activity, and the full-autonomy eligible-unstarted set — plus
    the bounded wake summary it composes, the poll-scheduling policy
    (`due_intake_polls`), and the pure decision of whether a tick that has
    cleared its quiet window should actually wake the orchestrator.

    Small in argument, because batches 2, 3 and 8 had already cleared it: the
    whole outbound set is `notify` (the check-state vocabulary and the #189
    `gh`-text sanitizer), `model::DEFAULT_INTAKE_POLL_MINUTES` and
    `groupid::GroupId`. Every edit inside the file is the prefix on one of those
    three, in the body and in the moved `#[cfg(test)]` module alike. Batch 7's
    macro sweep is clean, and no dependency joins (`serde`, `serde_json`, `std`,
    declared since batch 3). The impure half stays behind and is unaffected:
    `poll_intake` — the two `gh` calls, the allow-list in `src-tauri/src/gh.rs`
    they go through, the audit records — and `idle_tick_tick` still spell
    `intake::…` through the re-export.

    The re-export is the plain **module** form, and batch 10's rule is applied
    rather than restated: every consumer spells the module path
    (`intake::due_intake_polls` and `intake::PendingIntake` in `mod.rs`,
    `intake::eligible_deltas` in `tests/workflow.rs`,
    `loomux_lib::orchestration::intake::MAX_INTAKE_POLLS_PER_TICK` in
    `tests/orchestration.rs`), no flat `orchestration::<item>` spelling exists,
    and #988's trap has nothing to catch — **not one `pub(super)` or
    `pub(crate)` item in the file**, so the boundary force-widens nothing and
    the private members (`RawLabel`, `RawIssueJson`, `RawRollupEntry`,
    `RawCommentJson`, `RawReviewJson`, `RawPrJson`, `rollup_entry_state`,
    `parse_task_issue_ref`, `PendingIntake::blocks`/`dropped`) stay private.
    Scoped as batch 10 scopes it: no item widened, the `orchestration::intake::`
    spelling reaches the identical set, and the new `loomux_engine::intake::`
    spelling is inherent to crossing the boundary rather than to the shape.

    ### A file a test names by path is an edge, and no `use` line shows it

    This is the batch's finding, and it is batch 2's question — *where can the
    violation be spelled now?* — asked of a **file** instead of a type. Eleven
    batches have enumerated a module's edges from `super::`/`crate::` paths;
    batch 7 added `env!` and its macro siblings, on the ground that they name
    the crate a file is compiled in and no grep for `use` finds them. Here is a
    third kind: `src-tauri/tests/orchestration.rs`'s
    `poll_intake_still_asks_gh_for_comment_and_review_activity` opens
    `intake.rs` **by literal path** and asserts the `createdAt`/`submittedAt`
    serde renames are still there — because losing them degrades the #864
    comment signal to permanent silence, with a positive test blessing the
    degraded shape and every other test green.

    So the batch does **not** claim zero test edits. It repoints that read at
    `crates/loomux-engine/src`, spelled the way `tests/groupid.rs` already
    spells its second source root, and changes nothing else about the test. The
    failure direction is the mild one — a moved file makes the read fail loudly
    on the first CI round, unlike batch 2's tripwire, which would have gone
    green forever while watching a directory the violation could no longer
    reach. That asymmetry is worth carrying: **a path-scanning test either
    breaks loudly or goes silently blind when its subject moves, and which one
    you get depends on whether it reads a file or walks a root.** Sweep a moving
    file for who reads it *as a file*, alongside the `use` lines and the macros.

    One consequence, easy to reintroduce and cheap to prevent: the same test's
    other half scans `orchestration/mod.rs` for the *call* to `intake`'s `gh pr
    list` argv builder, and that scan is `contains`-based. Spelling that call in
    a **comment** — which the first draft of the batch-11 re-export block did,
    twice — satisfies the pin from prose and leaves it green over a poller that
    had stopped calling the builder at all. The comment now states the
    prohibition where the next editor will read it.

    ### The re-export it removed

    Batch 8 lifted `DEFAULT_INTAKE_POLL_MINUTES` into `model` and gave it a flat
    `pub(crate)` re-export in `orchestration/mod.rs` for its one caller,
    `intake.rs`, which was still in `src-tauri`. That caller is in the engine
    now and spells `crate::model::…`, so nothing in `src-tauri` names the const
    in code at all. The line comes off. `mod.rs`'s own comment says those
    `pub use` lines "are the list, so it cannot go stale", and a list is only
    that if entries leave it when their reason does — a dead re-export is a
    reader's re-derivation, deferred. The item is untouched: still `pub` in the
    engine, still reachable as
    `orchestration::model::DEFAULT_INTAKE_POLL_MINUTES` through the existing
    module re-export. Two doc claims in `model.rs` that named `intake.rs` as a
    `src-tauri` caller reaching the flat spelling through `super::` are
    corrected in the same commit, since the move is what falsified them.

    ### What it owed in evidence

    A **pure relocation**, exemption taken whole — a `git mv`, five import
    prefixes, the re-export lines, and prose. Every behaviour the move could
    break is pinned by tests that neither moved nor changed an assertion:
    `tests/orchestration.rs` drives the gate through the re-export across its
    surface (the wake summary's four signal kinds and both PARTIAL caveats, the
    fetch-bound argv pins, the smart default, `MAX_INTAKE_POLLS_PER_TICK`, the
    fallback backoff and the idle-tick wiring), `tests/workflow.rs` drives
    `eligible_deltas`/`OpenIssueList` (#778) through the same re-export, and the
    file's own inline `#[cfg(test)]` module travels with it as engine unit tests.
    Both suites reach the module by `use loomux_lib::orchestration::intake;`,
    unchanged — the only edit under `src-tauri/tests/` is the literal path
    above, which is why the exemption survives the test edit rather than being
    voided by it: the edit repoints a read, and pins nothing new.

    Batch 2's tripwire question is answered nil for `tests/groupid.rs` — its
    scan already walks both source roots, and `intake.rs` joins no group id onto
    a path; its one `GroupId` use is a `HashMap` key in `due_intake_polls`.
  - **Batch 12a — `mqdriver`, the merge queue's write primitives** (#581 slice
    D1): the `MqRunner` seam and its `ProcessRunner`, the live default-branch and
    PR lookups §7 requires, the `validate_target` refusal core all three
    enforcement points funnel through, scratch minting with §4's remote collision
    check, the create-only scratch push, `land_batch`, the `BatchVerification`
    adapter over `notify`'s classification, and namespace-exact cleanup.

    ### The module every batch since 5 named as the one staying behind

    Its whole outbound set was already across before this batch began — `mergeq`
    (batch 6), `notify` (batch 3), `workflow` (batch 5),
    `subproc::capture_raw_with_timeout` (batch 9) — so the moved file's edits are
    four import prefixes, three visibility keywords, and five doc references that
    said `mod.rs` where they now have to say `orchestration/mod.rs`. Nothing was
    lifted ahead of it and nothing was left behind.

    That is worth more than the diff, because of what the prose said. Batch 5
    left `mqdriver` on the stated ground that it "reaches the pane host"; batch 9
    measured that call and found no host in it; batches 6, 9 and 10 each then
    restated `mqdriver` as the standing example of a module held back — batch 10
    as recently as "what is genuinely still same-tier is `mqloop` → `mqdriver`".
    **Six batches of prose about why a module could not move, three of them
    written after the reason had expired.** Batch 9's rule is the right one and
    this sharpens it: re-derive the edge set from the source, and treat these
    notes as the *least* reliable place to read it — a superseded reason survives
    best exactly where it was best argued. The reusable check is cheap and is
    what this batch actually did first: grep the moving file's `use` lines and
    bodies for `super::`/`crate::`, resolve each against the crate list, and stop
    reading the paragraph that explains why you cannot.

    No dependency joins (`serde`, `serde_json`, `std` — declared since batch 3),
    batch 7's macro sweep is clean (no `env!`, `option_env!`, `file!`,
    `module_path!`, `include_str!` anywhere in the file), batch 11's is clean too
    (nothing under `src-tauri/tests/` reads `mqdriver.rs` by path), and batch 2's
    tripwire question is nil — `tests/groupid.rs` already walks both roots, and
    the file joins no group id onto a path at all: its `group` parameters go to
    `mergeq::scratch_branch`, which builds a *ref name*, never a path.

    ### The re-export is a module, and this is the case batches 10 and 11 had not met

    Batches 10 and 11 both took the plain module form and both stated the rule as
    two clauses: **pick the shape from what the callers spell**, and **take the
    item list when it buys a narrowing that is real** (#988). Every batch since 9
    could satisfy the first clause and had nothing for the second to do, which is
    why the rule has been quoted as if the first clause were all of it.

    Here the clauses pointed different ways. Every consumer spelled the module
    path — `mqdriver::runner_for` and `mqdriver::audit_action::…` in `mod.rs`,
    `super::mqdriver::landable` in `mqloop.rs`,
    `loomux_lib::orchestration::mqdriver::{…}` in both
    `src-tauri/tests/mergequeue.rs` and `tests/orchestration.rs` — so a flat item
    list would have preserved **no** call site and would have rewritten two
    integration test files to suit a re-export style, which is the forfeit batch
    10 calls worse than no ceremony at all. And yet, unlike `queue`, `queuestate`
    and `intake`, this module **did** have `pub(super)` items: `as_args`,
    `landable` and `declares_ci_green`, whose only caller was `mqloop` — still in
    `src-tauri` at the time, so no visibility narrower than `pub` reached it.
    (Batch 12b moved it; everything from here to the end of this entry is a
    record of a shape that no longer exists, and its expiry is stated at the
    close.)

    So `src-tauri/src/orchestration/mqdriver.rs` stayed as a **curated re-export
    module**, batch 7's `obs.rs` shape: `pub use` for every item that was `pub`,
    and `pub(super) use` for the three that were `pub(super)` — which, in
    a module whose `super` is `orchestration`, is the reach those three had as
    `pub(super) fn` in the same file before the move. The visibility table,
    copied. The two shapes are therefore **not alternatives ordered by taste**: a
    `pub use` line answers "what spelling do the callers use", a re-export module
    answers that *and* "what reach did each item have", and only the second
    question was live before this batch.

    Stated with batch 10's precision, because the loose version would be wrong in
    both directions here. **The `orchestration::mqdriver::…` spelling reached
    exactly the set it had reached before**, item for item and reach for reach.
    **Three items did widen**, and no re-export could undo it:
    `loomux_engine::mqdriver::{as_args, landable, declares_ci_green}` became that
    crate's public API, forced by the boundary exactly as
    `fsatomic::atomic_write` was in batch 9, and harmless on the standing terms
    (`publish = false`; "public" means reachable by a sibling crate in this
    workspace). What is *not* claimed is that nothing became reachable anywhere.

    The narrowing is worth its lines for one of the three in particular.
    `landable` is **half** of the constraint-7 refusal — the predicate that
    decides whether a name may become a refspec component — and `validate_target`
    is the whole of it, ordering the unverifiable, default-branch, target and
    assertion refusals so that an unreadable answer can never fail to match the
    default and read as safe. §7's argument is that the three enforcement points
    must not drift into three slightly different opinions, so the habitual path
    should point at the whole check. `as_args` and `declares_ci_green` ride along
    because the defensible form of that file is the table copied, not a judgement
    made item by item.

    **What a re-export narrows is a spelling, never a reachability — and the
    first draft of this entry got that wrong in the direction that flatters the
    work.** It claimed a "publicly reachable half is an invitation to build the
    next guard on the wrong one", which reads as though the re-export module put
    `landable` out of reach. It did not, and could not: `src-tauri` depends on
    `loomux-engine` directly and already spelled `loomux_engine::…` in `gh.rs`,
    `obs.rs` and `orchestration/mod.rs`, so `loomux_engine::mqdriver::landable`
    compiled from any module in `src-tauri` and no shape of the re-export changed
    that — an item must be `pub` in the engine to be re-exported at all. The
    honest account of the benefit was **legibility, not access control**: the
    habitual `orchestration::mqdriver::…` path reached only the whole check, and
    someone reaching the half had to type a cross-crate path that said so.
    (Batch 12b took the three back to `pub(crate)`, which is the access control
    this paragraph correctly says a re-export could not buy — the gap named here
    is closed, by the compiler rather than by a spelling.)

    That is worth carrying past this batch, because every batch from here on has
    the same sentence available to write. **A curated re-export answers "what can
    a caller spell without thinking", not "what can a caller spell."** The second
    question has one answer for every item any batch has moved — `pub` in the
    engine, reachable by any sibling crate in this workspace — and it is harmless
    for the standing reason (`publish = false`), which is exactly why the
    temptation is to describe the first question as if it were the second. Batch
    10's rule already says to name the spelling a claim is about; this is what it
    costs when you do not, and rev-lead caught it in the PR body where it would
    have become the squash message.

    Two things about the re-export module, stated so the next batch does not have
    to re-derive them. It is **not automatic** — an item added to the engine
    module is unreachable through it until somebody adds it, which is batch 7's
    stance restated: what `src-tauri` re-exports should be a list somebody chose.
    And it **collapses in batch 12b**: once `mqloop` is in the engine and spells
    `crate::mqdriver::…`, the three narrow lines have no consumer left and the
    file should become the one `pub use loomux_engine::mqdriver;` that batches 10
    and 11 would have written. The shape is transitional on purpose, which is the
    honest reading of a pair split across two batches. (That is what batch 12b
    did — and it went one step further than this paragraph predicted, by taking
    the three items themselves back down to `pub(crate)`. See that entry.)

    ### Why the pair split at all

    Batch 10 predicted `mqloop` and `mqdriver` would "move together in a later
    batch", on the ground that they are a same-tier pair. They are a **chain** in
    batch 6's sense, not a cycle — `mqloop` imports from `mqdriver` throughout
    its body and `mqdriver` names `mqloop` only in prose — so no compiler binds
    them, and batch 6's rule is that a chain only invites. Against the invitation:
    the two files are the largest in the feature, `mqloop` carries the batch
    construction, the bisect and the persistence, and `orchestration/mod.rs` is
    the highest-conflict file in the repo. Two reviewable diffs beat one, and the
    cost of splitting was precisely the re-export module above — which is a cost
    this batch would have paid anyway, since `mqloop` was not the only consumer
    of those three items' narrow reach so much as the only one that existed yet.

    ### What it owed in evidence

    A **pure relocation**, exemption taken whole. Nothing is added or changed;
    every behaviour the move could break is pinned by tests that neither moved nor
    were edited. `src-tauri/tests/mergequeue.rs` drives the module's whole surface
    through the re-export — the argv pins that §4 says are the only honest test of
    a create-only push (the lease's trailing colon, the landing refspec, the
    exact-name delete, the `--exit-code` collision probe), `validate_target`'s
    four refusal arms including the renamed-default case and both qualified
    spellings `same_branch` normalizes, `mint_scratch`'s bounded re-roll and its
    loud exhaustion, `classify_checks`'s `Met`-is-not-green correction, and
    `land_batch`'s per-PR re-check refusing before any push — and
    `tests/orchestration.rs` drives the wiring above it through the same
    re-export. **`src-tauri/tests/` is untouched**, which is the proof the
    re-export surface is complete rather than a claim about it. The file has no
    inline `#[cfg(test)]` module, so no test changed crate.

    ### Remaining same-tier edges

    `mqloop` was the only one **as of this batch**, and 12a left it an ordinary
    re-export edge rather than a same-tier reference: `use
    super::mqdriver::{as_args, classify_checks, …}` and the
    `super::mqdriver::landable` / `declares_ci_green` / `pr_ci_green_detailed` /
    `ResolveFailure` / `audit_action::…` call sites all resolved through
    `orchestration/mqdriver.rs` into the engine, with no source edit in
    `mqloop.rs`. Its own outbound set for batch 12b was therefore predicted as
    `mqdriver` (across), `mergeq`/`mergeqview` (batch 6), `notify` (batch 3),
    `workflow` (batch 5) and `atomic_write` (batch 9) — all of them already on the
    engine side, so 12b should be import prefixes too. Re-derive it from the
    source anyway; that is this batch's whole finding. (12b did, and it held —
    see that entry. Every `super::mqdriver::` spelling named above is a
    `crate::mqdriver::` path inside the engine now, and the file this paragraph
    says they resolve through no longer exists.)

  - **Batch 12b — `mqloop`, the driver loop (#581 slices D2/D3). The tail of
    A3's module moves.** §8's batch construction and its temporary-worktree
    mechanism, the draft PR and its body builder, the bounded check observation,
    §9's bisect and culprit attribution, §4's crash reconcile,
    `merge_queue.json` persistence, and `drive` — the one-step-per-call tick the
    unified `gh` poll loop calls (#698). Moves to
    `crates/loomux-engine/src/mqloop.rs`.

    The prediction above was taken as a hypothesis, not as a finding, which is
    what batch 12a's own lesson demands: the outbound set was **re-derived from
    the source** — a grep of every `super::`/`crate::` occurrence in the file,
    resolved against the crate list — rather than read out of the paragraph that
    predicted it. It came back identical: `mergeq`/`mergeqview` (batch 6),
    `mqdriver` (12a), `notify` (batch 3), `workflow` (batch 5),
    `fsatomic::atomic_write` (batch 9). All already across, nothing lifted ahead
    of it, and the diff inside the moved file is import prefixes and nothing
    else. **That the prediction held is not evidence the prediction was
    trustworthy** — batch 12a's finding was about a prediction that had been
    wrong for six batches while reading exactly as confidently as this one.

    No dependency joins (`serde`, `serde_json`, `std`, declared since batch 3;
    `Cargo.toml` and `Cargo.lock` are untouched). Batch 7's macro sweep is clean
    in the strongest sense available — the file contains **no macro invocation at
    all**, so there is nothing for `env!`/`include_str!`/`file!`/`module_path!`
    to have hidden in. Batch 11's sweep is clean: nothing under
    `src-tauri/tests/` opens `mqloop.rs` as a file. Batch 2's tripwire is nil —
    `tests/groupid.rs` already walks both source roots, and `mqloop` never joins
    a group id onto a path: it is handed an already-resolved `group_dir: &Path`
    and takes `group: &str` only as audit and notice text.

    ### The batch that pays batch 12a's debt

    12a's re-export module existed for one reason and one caller. `mqloop` was
    the only thing in `src-tauri` that reached `as_args`, `landable` or
    `declares_ci_green` — verified for this batch by grepping the **entity**
    across `src-tauri/src`, `src-tauri/tests` and `crates/`, not by trusting the
    12a entry that says so: outside `mqdriver.rs` itself, every code reference
    was in `mqloop.rs`, and `tests/mergequeue.rs` names `landable` in three
    comments and calls it nowhere. So moving `mqloop` retired the whole
    construction at once:

    - the three items go back to **`pub(crate)`** in the engine, and
    - `src-tauri/src/orchestration/mqdriver.rs` is deleted, replaced by the
      single `pub use loomux_engine::{mqdriver, mqloop};` in `mod.rs` that
      batches 10 and 11 would have written.

    `pub(crate)` is the **faithful translation** of the old `pub(super)`, not a
    new narrowing invented here: the scope those items had was "the
    `orchestration` module", and the module's contents are now this crate. Batch
    12a could not write it, because a `pub(super)` does not reach across a crate
    boundary and its only caller was on the other side.

    Why it was worth doing rather than leaving as harmless surplus `pub`: 12a's
    entry above spends several paragraphs establishing, correctly, that **a
    re-export narrows a spelling and never a reachability** — while `landable`
    was `pub`, `loomux_engine::mqdriver::landable` compiled from anywhere in
    `src-tauri` and no shape of the re-export could stop it. That mattered
    because `landable` is only *half* of the constraint-7 refusal and
    `validate_target` is the whole of it. 12a recorded the gap and could do
    nothing about it. 12b closes it, and the compiler — not a header comment —
    is what holds it closed now.

    The rule to carry forward: **a forced widening is a debt with a due date.**
    A batch that widens an item to `pub` because its last caller has not crossed
    yet should name the batch that will move that caller, and the batch that
    moves it owes the reversion in the same diff. Left undone, the widening
    outlives its reason silently and the next reader has no way to tell a forced
    `pub` from a chosen one — the same failure mode as the superseded edge-set
    prose 12a caught, in the visibility table instead of the notes.

    Note also what did **not** happen: `mqloop` has no `pub(super)` or
    `pub(crate)` item of its own, so its own move force-widened **nothing** — its
    `pub` set is identical before and after, and its private members — the ones
    enumerated in `crates/loomux-engine/src/lib.rs`'s batch-12b entry — stay
    private. The only visibility keywords this batch moved, moved down.

    Deliberately no count here, and the omission is the point. A hand-maintained
    tally of a set that grows is a claim with a built-in expiry: it is correct on
    the day it is written and silently wrong the first time somebody adds a
    private helper, with nothing in the diff to prompt the update — the same
    shape as the superseded edge-set prose this batch's sibling caught, and the
    drift #973 is about. The enumeration is the artifact; adding a member means
    adding a name to it, which a reader can check against the file. A number is a
    second, weaker copy of the same fact that no reader can check and no test
    pins, so this note does not keep one.

    ### The re-export shape, one batch later

    12a's finding was that the two clauses of the shape rule ("follow what the
    callers spell"; "take the item list when it buys a narrowing that is real")
    could point in different directions. Here they point the same way again, and
    it is worth saying *why* rather than just recording the plain module form:
    every consumer of both modules spells the module path (`mqdriver::MqRunner`,
    `mqloop::drive`, `mqloop::refusal::…` in `mod.rs`;
    `loomux_lib::orchestration::{mqdriver,mqloop}::…` in
    `src-tauri/tests/mergequeue.rs` and `tests/orchestration.rs`), so the first
    clause points at the module form as always — and after the `pub(crate)`
    reversion the second clause has nothing left to buy, because there is no item
    whose reach a curated list could still narrow. The curated module was never a
    style preference; it was an answer to a question that has stopped being live.

    ### What it owed in evidence

    A **pure relocation**, exemption taken whole: no behaviour added or changed,
    so no new test, and the existing suite is the pin.
    `src-tauri/tests/mergequeue.rs` drives the entire moved surface — the batch
    plan and its worktree build, the bisect walk and culprit attribution, the
    enqueue/cancel refusals, the reconcile paths, the `drive` tick — and
    `tests/orchestration.rs` drives `refusal::is_loomux_fault` and the registry
    wiring above it. **Both are untouched by this batch**, which is the proof the
    re-export surface is complete rather than a claim about it: had any item or
    spelling failed to survive the move, they would not compile. The file has no
    inline `#[cfg(test)]` module, so no test changed crate.

    ### Remaining same-tier edges

    None for the merge queue: all four of its files (`mergeq`, `mergeqview`,
    `mqdriver`, `mqloop`) are in the engine, and no module in `src-tauri`
    reaches into another that is mid-move. `src-tauri/src/orchestration/` is now
    `mod.rs`, `templates/`, and three modules that are not merge-queue work and
    are not this batch's to place: `digest`, `humanq` (#946/#959 trust-boundary
    code, whose relocation batch 3 already recorded as a decision of its own)
    and `mcp`. What remains of A3 is the trait work its own header names —
    `EventSink` + `PaneHost`, and `NullPaneHost` replacing the app-is-`None`
    branch — followed by A4.
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
