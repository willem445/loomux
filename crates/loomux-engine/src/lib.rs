//! The loomux orchestration engine — the part of loomux that has nothing to do
//! with being a desktop app.
//!
//! The crate landed empty (#888 slice A1 / #847 Phase 0) on purpose:
//! converting the repo to a Cargo workspace moves the lockfile, the `target/`
//! directory and the release profile, and that is a release-plumbing change
//! worth landing and proving on CI by itself, separately from thousands of
//! moved lines. Slice A2 fills it, in small batches.
//!
//! # Why it exists
//!
//! `src-tauri` links Tauri, which on Linux means webkit2gtk. A headless server
//! that must build a browser engine in order to run orchestration is not a
//! deployment shape, so the remote-engine work (#888) needs a core that builds
//! without it. That core is this crate.
//!
//! # The one rule
//!
//! **`src-tauri` depends on `loomux-engine`. The arrow never points back.**
//! No `tauri` in this crate's dependency tree, ever — not directly, not
//! transitively. Everything the engine needs from the host arrives as a trait
//! the host implements (`EventSink`, `PaneHost`; slice A3), never as an
//! `AppHandle`.
//!
//! # What lands here, in order
//!
//! Slice A2 moves the already-Tauri-free submodules; A3 introduces the host
//! traits; A4 moves `OrchRegistry` and the decision layer. Each is its own
//! reviewable change, and the bar for all of them is behavioural silence — the
//! existing suite green with no test edits. See
//! `doc/design/engine-extraction.md`.
//!
//! # What is here so far
//!
//! A2 batch 1 — [`report`] (the decision-grade report protocol's pure core,
//! #398) and [`termgrid`] (the dependency-free VT replay behind `get_output`,
//! #520). They went first because they are the two submodules with no outbound
//! dependency on anything else in `orchestration/` at all: both are `std`-only,
//! so they prove the move-and-re-export mechanism end to end without also
//! testing a dependency edge. `src-tauri/src/orchestration/mod.rs` re-exports
//! each under its old path, so every existing call site resolves unchanged.
//!
//! Their test coverage arrived in two different shapes, and the difference is
//! worth knowing before the next module moves. [`report`] brought nine inline
//! `#[cfg(test)]` unit tests with it. [`termgrid`] has **none** — it never had
//! any; it is covered entirely from `src-tauri/tests/orchestration.rs`, which
//! drives `render_screen`/`render_visible` from ~30 call sites and stayed
//! untouched by the move.
//!
//! Inline unit tests are *possible* on this side of the boundary, which is the
//! part that matters for what comes next: CLAUDE.md constraint 4 forces
//! `src-tauri`'s lib-linking tests to be integration tests because Windows test
//! executables need the comctl32-v6 manifest `build.rs` embeds, and nothing
//! here links Tauri or that manifest. So a module arriving with inline tests
//! keeps them, and a module whose coverage lives in the integration suite keeps
//! that instead — neither is converted on the way in.
//! `src-tauri/tests/smoke.rs` still has to exist for the app's own sake.
//!
//! A2 batch 2 — [`groupid`] (#904), the validated group identifier every
//! group-scoped path is built from. It brings the crate's first dependency,
//! `serde`: `GroupId`'s wire transparency is a hand-written
//! `Serialize`/`Deserialize` pair, and the `Deserialize` half is load-bearing
//! rather than convenience — it is what stops a hand-edited state file minting
//! an id the constructor would have refused. See the manifest for why that
//! dependency is safe under CLAUDE.md constraint 2.
//!
//! It also moved a **security tripwire's coverage**, which is the part worth
//! knowing before the next batch. `GroupId` deliberately has no
//! `AsRef<Path>`, and `src-tauri/tests/groupid.rs`'s
//! `the_orchestration_root_is_joined_with_a_group_in_exactly_one_place`
//! asserts that absence by scanning source. Once the type lives here, the
//! orphan rule leaves nowhere else that impl could be written — so the scan now
//! walks BOTH source roots, and a scan that only knew `src-tauri/src` would
//! have gone green forever while enforcing nothing. **Any future batch that
//! moves a type a source-scanning test watches owes the same check**: ask where
//! the violation can now be spelled, not where it used to be.
//!
//! A2 batch 3 — [`lessons`] (#268) and [`notify`] (#243), plus [`text`], which
//! is what made them movable. Both modules were otherwise leaves; each had a
//! single edge left pointing at `orchestration/mod.rs`, and both of those edges
//! were pure string helpers rather than registry state — so the helpers moved
//! ahead of their callers into [`text`] instead of a trait being invented to
//! reach back for them. Read that as the batch's actual finding: **an edge into
//! `mod.rs` is not automatically an edge into the registry.** Some of what
//! `mod.rs` holds is there because it is one large file, not because it is
//! coupled to `AppHandle`, and that kind of edge is cut by moving the callee,
//! not by abstracting the caller.
//!
//! This batch also widened two of [`notify`]'s functions from `pub(super)` to
//! `pub`. That is the crate boundary's doing rather than a policy change: their
//! callers (the `gh pr list` rollup, the merge queue's batch verdict) stayed in
//! `src-tauri`, and no visibility narrower than `pub` still reaches them. Worth
//! expecting again — a batch that leaves a caller behind converts that caller's
//! `pub(super)` into public API, so the question each time is whether the item
//! is one this crate is content to expose, not merely whether it compiles.
//!
//! It also brings `serde_json` ([`notify`] parses `gh --json` payloads) and
//! asks `serde` for `derive`. Both are argued in the manifest; the `derive`
//! half in particular was declared unnecessary there until this batch, and
//! is not any more.
//!
//! A2 batch 4 — [`model`], the shared data layer: [`model::Role`] (the closed
//! capability class), [`model::Containment`] (the deny tier it selects), the
//! per-CLI capability table and the pure functions over them. It is the first
//! batch that moved something *because of what comes next* rather than because
//! it had run out of edges: the `workflow` cluster is the batch after it, and
//! every one of these symbols is something `workflow` reads. Moving `workflow`
//! first would have left it reaching back into `src-tauri` for its own `kind:`
//! type — the arrow pointing the wrong way, in the module that most needed it
//! not to.
//!
//! Its finding is the mirror image of batch 2's, and worth having both. There,
//! a source-scanning tripwire had to FOLLOW the type, because the orphan rule
//! moved the only place its violation could be written. Here, `Role`'s
//! `template()`/`instructions_file()` had to STAY BEHIND: an inherent impl must
//! live in the crate defining its type, so keeping them as methods would have
//! dragged `src-tauri/src/orchestration/templates/*.md` and the byte-golden
//! fixture root that pins them into this crate — a *silent* relocation of
//! product content and its blessing procedure, done as a side effect of moving
//! an enum. They are free functions in `src-tauri` now, and the call-site
//! rewrite that cost is one the compiler checks exhaustively. **Ask of every
//! item on a moving type whether it is data or content**; the compiler will
//! tell you about the rewrite, and nothing will tell you about the relocation.
//!
//! Batch 5 then split that pair, and the amendment belongs here rather than
//! only in [`model`]: `role_template` stays (it loads the fixture-pinned bytes,
//! which is what the paragraph above is actually about) and
//! [`model::role_instructions_file`] came across (it loads nothing, and
//! `workflow::Block` calls it from inside this crate). Read the rule as **ask
//! it of each item, not of the pair** — batch 4 kept them together on the
//! ground that "the name and the bytes are one mapping", and that pairing was
//! the weaker half of its own argument.
//!
//! A2 batch 5 — the `workflow` CLUSTER: [`workflow`] (the
//! `.loomux/workflow.yml` parser, its types, the merge-gate spec file and the
//! capacity advice), [`profiles`] (the persona/profile loader and its
//! sanitizers) and [`locks`] (the named-resource state machine). The first
//! batch that had to move THREE modules at once, and the reason is a shape
//! worth recognising rather than a size: `profiles` calls
//! `workflow::{kind_from_str, resolve_profile_path}` while `parse_workflow`
//! calls `profiles::sanitize_allow`. That cycle is unremarkable inside one
//! crate and unrepresentable across two, so **a dependency cycle is a
//! partition, not an ordering** — no batch order exists that moves either
//! alone, and the only question was where to draw the line around it. `locks`
//! joins because `LockTable::sync` is typed on `workflow::ResourcePolicy`.
//!
//! The line was drawn TIGHT. `mergeq` looked like a fourth member and is not:
//! every mention of it in `workflow` is prose — a doc link and two references
//! in doc comments — and prose does not make an edge. What matters is that no
//! `mergeq` path appears in a body, which is the thing to check; counting the
//! mentions is neither necessary nor, as it turned out, easy to get right.
//! [`mqdriver`] is `workflow`'s heaviest consumer and stayed behind on purpose —
//! it reaches `capture_raw_with_timeout` (glossed here at the time as "the pane
//! host, which is slice A3"; batch 9 re-measured that call and found no host in
//! it — see below).
//! **An inbound edge never blocks a move**, because the re-export answers it:
//! for six batches `mqdriver` went on spelling `super::workflow::…` and never
//! learned anything had changed. Only outbound edges decide what a batch has to
//! contain. (It crossed in batch 12a; those imports are `crate::workflow::…`
//! now. The rule is what survives, not the example.)
//!
//! One outbound edge did not exist when this batch was planned, because batch 4
//! created it. `Block::instructions_file` calls `role_instructions_file`, which
//! batch 4 had deliberately left in `src-tauri` paired with `role_template`. So
//! batch 5 split that pair — see [`model`]'s header for the argument. The
//! finding generalises past this batch: **the batch that lifts a data layer
//! ahead of its caller can leave a new edge pointing the wrong way**, because
//! splitting a type's methods off it decides where those methods live, and the
//! caller that forces the question may not have moved yet. Re-derive the edge
//! set from the source at the start of every batch; a map drawn one batch ago
//! is describing a tree that has since changed.
//!
//! It brings `serde_norway` (the YAML parser `parse_workflow` is built on) and
//! `sha2` (`body_digest`, the hash the merge gate compares). Batch 3's rule
//! held: **a dependency a module uses has to be declared, not inherited** —
//! both are already in the shipped binary's graph via `src-tauri`, so no new
//! package joins the lock, but resolver-2's feature unification is not
//! crate-name unification and an undeclared crate does not compile at all. The
//! manifest carries the argument for each, and `src-tauri`'s carries the
//! getrandom audit both inherit.
//!
//! A2 batch 6 — [`mergeq`] (the merge queue's pure core, #581 slice C) and
//! [`mergeqview`] (the read-only projection the human's chrome renders, slice
//! F). Batch 5 named `mergeq` as the member it deliberately left out, and the
//! reason it could not go then is the reason it goes now: every symbol it
//! imports comes from `workflow`, which batch 5 brought across. Nothing in
//! either file changed but the prefix on an import.
//!
//! The two travel together on weaker ground than batch 5's cluster, and the
//! difference is this batch's finding. `workflow` and `profiles` were a
//! **cycle** — no batch order existed that moved either alone. `mergeq` and
//! `mergeqview` are a **chain**: `mergeqview` reads `mergeq` and nothing else,
//! and `mergeq` never reads back. So `mergeqview` *could* have stayed behind
//! and reached the engine through the re-export, exactly as `mqdriver` then did
//! for six batches; it
//! comes because it is a pure projection with no other edge and nothing in the
//! Tauri half to be near, not because the compiler insisted. **A cycle decides
//! a batch's contents; a chain only invites them** — and a batch that cannot
//! say which of the two it is has not drawn its own line.
//!
//! It is also where batch 5's inbound-edge rule meets real code rather than
//! prose. `mqdriver` and `mqloop` do not merely name `mergeq` in doc comments:
//! they import from it in their bodies (`use super::mergeq::{new_batch_id,
//! scratch_branch, …}`, `use super::mergeqview::MERGE_QUEUE_FILE`) and call
//! `mergeq::recheck_gate`. Both stayed in `src-tauri` — for the edges they had
//! at the time, which batch 9 re-measured and found were not host edges at all
//! (see its entry below) — both spelled `super::` exactly as before, and both
//! compiled against the re-export. (`mqdriver` crossed in batch 12a and
//! `mqloop` in 12b; what the pair demonstrated here is unaffected by their
//! having since moved.) Batch 5 established that prose is not an edge; the
//! other half belongs beside it, because it is the half that misleads: **a
//! body-level inbound edge is a genuine edge and still does not block a move.**
//! Only outbound edges decide what a batch has to contain.
//!
//! No dependency joins the crate. That is the result of reading the moved
//! files' own `use` lines — `serde` and `serde_json` in [`mergeq`],
//! `serde_json` and `std` in [`mergeqview`], all declared here since batch 3 —
//! rather than a check for the imports one expects: batch 5 failed CI on a
//! guessed list of crate names, and enumerating what the files actually import
//! is the cheap way not to repeat it.
//!
//! `mergeq::new_batch_id` seeds itself from std's `RandomState` rather than
//! from a random crate, which is CLAUDE.md constraint 2. It travels unchanged,
//! and it belongs here for the reason the manifest states: this crate is linked
//! into the shipped Windows binary, so the getrandom ban applies on this side
//! of the boundary exactly as it does in `src-tauri`.
//!
//! A3 batch 7 — [`obs`], the crash-observability core (#53): the panic hook and
//! its crash logs, the breadcrumb log, `data_root`/`logs_dir` and the
//! `LOOMUX_DATA_DIR` validation, the `running.lock` sentinel, and
//! [`obs::LockExt`] — the poison-tolerant `Mutex::lock` that most of `src-tauri`
//! locks through. A daemon needs the same crash trail a windowed app does, so
//! none of it is desktop-specific; it is here because the batches that follow
//! cannot move without `lock_safe` and `breadcrumb`, which every one of them
//! reaches for.
//!
//! **The first batch that split a FILE instead of moving one**, and the split
//! point was already written down: `obs.rs` fenced its two Tauri items off
//! behind its own `next-launch notice (Tauri surface)` section marker long
//! before this refactor existed. `StartupNotice` and the `take_startup_notice`
//! command stay in `src-tauri/src/obs.rs`, which re-exports this module
//! item-by-item so every `obs::…` call site over there resolves unchanged —
//! the move cost no call-site edits at all, and the single one that did change
//! belongs to the `env!` fix below, not to the move. Read the general form as
//! **a module's author may have already drawn the boundary** — worth looking
//! for a section marker before reaching for a trait, because the alternative
//! here was a bad one: `LockExt` is an inline extension trait on
//! `std::sync::Mutex` (`m.lock_safe()`), unreachable through a trait object as
//! called, so abstracting it would have meant a second implementation of the
//! one policy that must not have two.
//!
//! Its finding is a kind of edge the previous six batches never met, because
//! grep cannot see it. Every batch so far enumerated a module's outbound edges
//! by searching for `super::`/`crate::`; `obs.rs` has none — and it still had
//! one, in `record_crash`'s `env!("CARGO_PKG_VERSION")`. **`env!` is an edge to
//! the crate a file is compiled in**, and moving the file silently re-points it:
//! this crate's version is deliberately `0.0.0` (see the manifest), so a
//! verbatim move would have made every crash log read `version: 0.0.0` while
//! `doc/design/crash-observability.md` goes on promising the loomux release
//! version. Nothing fails to compile; nothing goes red. So
//! [`obs::install_panic_hook`] takes the app version as an argument and the host
//! passes `env!("CARGO_PKG_VERSION")` from `src-tauri/src/lib.rs`, where the
//! macro means what it says. **Sweep a moving file for `env!`, `file!`,
//! `include_str!` and `module_path!` alongside its `use` lines** — each is a
//! compile-time reference to the crate the file happens to be in, and each one
//! moves house without telling anybody.
//!
//! It brings `dirs` (`data_root` resolves the platform data dir) and the
//! crate's first `[dev-dependencies]`, `tempfile` with default features off —
//! the inline tests write into a temp tree. Both are argued in the manifest;
//! neither adds a package to the shipped binary's graph.
//!
//! A3 batch 9 — the two HOST PRIMITIVES `orchestration/mod.rs` was still
//! carrying: [`subproc`], the bounded child-process capture (#656, split out of
//! `OrchRegistry::capture_with_timeout` by #698), and [`fsatomic`], the durable
//! whole-file replace (#133). Both are `std` and nothing else —
//! `std::process`/`std::thread` for one, `std::fs` for the other — so no
//! dependency joins the crate and no manifest line changes.
//!
//! They arrive as **two modules, not one**, and that is the batch's finding.
//! The tempting shape was a single `hostio`: both are "the primitives that
//! touch the host", both were lifted in the same batch, and both are small. But
//! they share no symbol, no design note and no failure mode — a bounded
//! subprocess wait exists because a stalled child parks the single poll loop
//! and every `notify_when` notice with it, while a temp-fsync-rename exists
//! because a disk-full `fs::write` truncated tasks.json and destroyed a live
//! board. **A batch is a unit of moving, not a unit of grouping**; batches 5
//! and 6 argued which modules must travel together, and the mirror question is
//! this one — items that travel together do not thereby belong together, and a
//! module named for what its members have in common with the *batch* is a name
//! that will not survive the next reader.
//!
//! The other half is what "host primitive" turned out NOT to mean. Both were
//! called pane-host calls when A3 was planned — [`mqdriver`] stayed behind in
//! batch 5 on the stated ground that it "reaches the pane host
//! (`capture_raw_with_timeout`)" — and re-measuring the cluster at the start of
//! this batch found no host edge in either: no `tauri`, no `AppHandle`, no pty,
//! nothing that needs the trait work A3 is otherwise about. So they moved as
//! ordinary `std` leaves, months of prose notwithstanding. Batch 5's rule holds
//! and this is the sharpest instance of it yet: **re-derive the edge set from
//! the source at the start of every batch** — including from the notes this
//! crate's own header wrote down, which are describing a tree as it was.
//!
//! `subproc` has exactly one outward edge, [`obs::LockExt`] (`lock_safe` on the
//! abandoned-reader backlog), which batch 7 brought across for precisely this;
//! `fsatomic` has none. Both left every caller of the day in `src-tauri` —
//! `OrchRegistry::capture_with_timeout`, `mqdriver`'s `ProcessRunner`, and
//! every `atomic_write` call site — resolving through curated item-list
//! re-exports in `orchestration/mod.rs`, which is why the integration suite
//! needed no edit. (`mqdriver` is [`mqdriver`] here as of batch 12a and calls
//! [`subproc::capture_raw_with_timeout`] directly; `atomic_write`'s callers are
//! unaffected.)
//!
//! A3 batch 10 — the DELIVERY QUEUE: [`queue`], the pure core of the per-pane
//! FIFO (#445/#468/#467 — admission, coalescing, the flush plan, the
//! `queue.json` snapshot and its recovery split, the archive, the orphan
//! derivation), and [`queuestate`], the two mutable maps behind doors that
//! cannot be opened without paying what opening them costs (#562's
//! [`queuestate::QueueMap`], whose only `&mut` door writes the snapshot on the
//! way out, and #497's [`queuestate::DrainerRegistry`], whose only removal is
//! generation-checked).
//!
//! It is the largest module to cross so far and the cheapest to argue, which is
//! the point worth recording: `queue`'s whole outbound set is `GroupId`,
//! [`model::Delivery`] and [`text::LOOMUX_NOTICE_MARKER`], and `queuestate`'s is
//! `GroupId`, `Delivery`, `queue` itself and [`obs::LockExt`] — every one of
//! them landed here in batches 2, 7 and 8, so nothing had to be lifted ahead of
//! them. The
//! two are a **chain, not a cycle** in batch 6's sense (`queuestate` names
//! `queue`; `queue` never names back), so `queue` could have gone alone; they
//! travel together because `queuestate` has no other edge and nothing in the
//! Tauri half to be near, and because splitting them would have put the maps
//! one batch away from the type they hold.
//!
//! Its finding is about the RE-EXPORT rather than the move. Batch 9 re-exported
//! its two modules as curated item lists (#988), and that was right *there*
//! because every caller spelled the flat `orchestration::atomic_write`. Here
//! every caller — `mod.rs` and `src-tauri/tests/orchestration.rs` alike —
//! spells the MODULE path (`queue::QueuedDelivery`, `queuestate::QueueMap`), so
//! an item list would not preserve a single call site; the plain module
//! re-export batch 6 used is the shape that leaves the suite untouched.
//! #988's trap does not bite, and the reason is measurable rather than
//! stylistic: **neither file has one `pub(super)` or `pub(crate)` item**, so
//! the move force-widens nothing. `pub mod queue` already sat under
//! `pub mod orchestration`, which means `loomux_lib::orchestration::queue::…`
//! reached exactly this set of items before the move and reaches exactly it
//! after. The rule to carry: **pick the re-export shape from what the callers
//! spell, and take the item list when it buys a narrowing that is real** —
//! curating a list that widens nothing and preserves nothing is ceremony.
//!
//! No dependency joins: `serde` and `serde_json` (`queue`), `std` and
//! [`obs::LockExt`] (`queuestate`), all declared here since batch 3. Both files
//! are clean under batch 7's macro sweep — no `env!`, `option_env!`, `file!`,
//! `module_path!` or `include_str!` anywhere in either.
//!
//! A3 batch 11 — [`intake`], the pure core of the idle-tick intake gate
//! (#332/#429/#795/#864/#778): the host-side, zero-token diff of what changed
//! on GitHub since the last poll (label deltas, PR check transitions, PR
//! comment/review activity, and the full-autonomy eligible-unstarted set), the
//! bounded wake summary it composes, the poll-scheduling policy, and the pure
//! decision of whether an idle tick that has cleared its quiet window should
//! actually wake the orchestrator.
//!
//! Every outbound edge was already across, so the batch is one import prefix
//! deep: [`notify`] (batch 3) for the check-state vocabulary and the #189
//! `gh`-text sanitizer, [`model::DEFAULT_INTAKE_POLL_MINUTES`] (batch 8) for the
//! smart default, and [`groupid::GroupId`] (batch 2) for the due-poll selection.
//! No dependency joins — `serde`, `serde_json` and `std`, all declared since
//! batch 3 — and the file is clean under batch 7's macro sweep.
//!
//! Its finding is about a **source-scanning test**, which is batch 2's question
//! asked of a file rather than of a type. `intake.rs` is one of only two files
//! `src-tauri/tests/orchestration.rs` reads by literal path, to pin that the
//! `createdAt`/`submittedAt` serde renames survive (a rename degrades the #864
//! comment signal to permanent silence with every other test still green). A
//! verbatim move breaks that read outright — a loud failure, unlike batch 2's
//! silent one — so the batch does not claim zero test edits: it repoints the
//! path at `crates/loomux-engine/src`, exactly as `tests/groupid.rs` already
//! spells its second root. **A file a test names by path is an edge that no
//! grep for `super::` or `use` will find**, and it belongs in batch 7's macro
//! sweep as a sibling: sweep a moving file for who reads it *as a file*.
//!
//! It also removes a re-export rather than adding one. Batch 8 lifted
//! [`model::DEFAULT_INTAKE_POLL_MINUTES`] and gave it a flat
//! `orchestration::` spelling for its one caller, `intake.rs`, which was still
//! in `src-tauri`. That caller is here now, so the line has no consumer left and
//! comes off the list — `orchestration/mod.rs`'s re-exports are meant to read as
//! the live list, and a dead one makes the next reader re-derive it.
//!
//! A3 batch 12a — [`mqdriver`], the merge queue's **write primitives** (#581
//! slice D1): the [`mqdriver::MqRunner`] seam and its process implementation,
//! the live default-branch and PR lookups, the [`mqdriver::validate_target`]
//! refusal core all three §7 enforcement points funnel through, scratch minting
//! with its remote collision check, the create-only scratch push, the landing
//! push, the [`mqdriver::BatchVerification`] adapter over [`notify`]'s
//! classification, and namespace-exact cleanup.
//!
//! The module every batch since 5 named as the one staying behind, and it turned
//! out to owe nothing further: its whole outbound set — [`mergeq`] (batch 6),
//! [`notify`] (batch 3), [`workflow`] (batch 5) and
//! [`subproc::capture_raw_with_timeout`] (batch 9) — was already across, so the
//! move is import prefixes and a re-export. Batch 5 left it behind for a host
//! edge batch 9 then measured away; batches 6, 9 and 10 each restated it as the
//! standing example of an inbound edge answered by a re-export. **Six batches of
//! prose about why a module could not move, and the reason had expired three
//! batches earlier** — batch 9's rule, sharpened: re-derive the edge set from the
//! source, and treat this crate's own header as the *least* reliable place to
//! read it, because it is where a superseded reason survives best.
//!
//! No dependency joins (`serde`, `serde_json`, `std`, all declared since batch 3)
//! and the file is clean under batch 7's macro sweep. No test reads it by path,
//! so batch 11's finding does not bite either.
//!
//! Its own finding is the **re-export shape**, and it is the case batches 10 and
//! 11 had not met. Both established that the shape follows what the callers
//! spell, and every consumer here spells the module path — so the plain
//! `pub use loomux_engine::mqdriver;` is where that rule points. It is not what
//! `src-tauri` got, because the rule's second clause decides it: take the item
//! list **when it buys a narrowing that is real** (#988). Unlike `queue`,
//! `queuestate` and `intake`, this module had three `pub(super)` items —
//! `mqdriver::as_args`, `mqdriver::landable` and `mqdriver::declares_ci_green`
//! — whose only caller, `mqloop`, was still in `src-tauri` until batch 12b. So
//! `orchestration::mqdriver` was a curated
//! re-export **module** (batch 7's `obs.rs` shape): the module path every call
//! site spells is preserved, and the three kept their `pub(super)` reach under
//! it. The items themselves widened here and no re-export could stop that,
//! exactly as [`fsatomic::atomic_write`] did in batch 9. **The two shapes are
//! not alternatives ordered by taste** — a `pub use` line answers "what spelling
//! do callers use", a re-export module answers that *and* "what reach did each
//! item have", and only the second question was live before this batch. (Both
//! halves of that finding expired one batch later; batch 12b's entry says how,
//! and it is the reason this paragraph is in the past tense.)
//!
//! A3 batch 12b — [`mqloop`], the merge queue's **driver loop** (#581 slices D2
//! and D3) and the last of A3's module moves: §8's batch construction and its
//! temporary-worktree mechanism, the draft PR and its body builder, the bounded
//! check observation, §9's bisect and culprit attribution, §4's crash reconcile,
//! `merge_queue.json` persistence, and [`mqloop::drive`], the one-step-per-call
//! tick the unified `gh` poll loop calls.
//!
//! Batch 9's rule was applied to it rather than assumed, which is the only
//! reason this entry can be short: the outbound set was re-derived **from the
//! source**, not from the prose above, and every edge was already across —
//! [`mergeq`] and [`mergeqview`] (batch 6), [`mqdriver`] (12a), [`notify`]
//! (batch 3), [`workflow`] (batch 5), [`fsatomic::atomic_write`] (batch 9).
//! Nothing was lifted ahead of it and no dependency joins. Clean under batch 7's
//! macro sweep (the file contains no macro invocation at all) and under batch
//! 11's read-by-path sweep (nothing under `src-tauri/tests/` opens it as a
//! file); batch 2's `GroupId` tripwire is unaffected, since both source roots it
//! scans are unchanged and `mqloop` joins no group id onto a path — it is handed
//! an already-resolved `group_dir: &Path` and takes `group: &str` only as audit
//! and notice text.
//!
//! Its finding is what happens to 12a's finding. `mqloop` was the *only* caller
//! of the three items that made `orchestration::mqdriver` a curated re-export
//! module, so moving it here retired both: the three went back to `pub(crate)`
//! — the faithful translation of their old `pub(super)`, since the scope that
//! was "the `orchestration` module" is now "this crate" — and the curated file,
//! left with nothing to narrow, collapsed into the plain
//! `pub use loomux_engine::{mqdriver, mqloop};` batches 10 and 11 would have
//! written. **A forced widening is a debt with a due date, and the batch that
//! moves the last caller is when it comes due.** Nothing here is stylistic:
//! while the items were `pub`, [`mqdriver::validate_target`]'s refspec-shape
//! half was reachable from anywhere in `src-tauri` and batch 12a's header
//! recorded that it could do nothing about it. It is not reachable now, and the
//! compiler is what says so.
//!
//! `mqloop` itself force-widened nothing — it has no `pub(super)` or
//! `pub(crate)` item, so its `pub` set is identical before and after, and its
//! private members (`MAX_QUOTED`, `MAX_SIBLINGS_LISTED`, `worktree_dir_name`,
//! `remove_worktree`, `build_in_worktree`, `same_object`,
//! `rev_parse`, `quote`, `drained`, `release_if_drained`, `trim_terminal`,
//! `RawPrState`, `draft_pr_open`, `strand`, `advance_in_flight`, `land`,
//! `narrow_search`, `search_set`, `attribute`, `build_probe`, `start_batch`,
//! `refresh_and_select`, `stall`, `construct`, `kick_back_one`, `mv`, `requeue`,
//! `set_batch_tag`, `set_blocked`, `set_head`, `moves_json`, `teardown`,
//! `body_file_path`, `with_body_file`, `open_draft_pr`, `post_comment`,
//! `land_refusal_text`) stay private.

pub mod fsatomic;
pub mod groupid;
pub mod intake;
pub mod lessons;
pub mod locks;
pub mod mergeq;
pub mod mergeqview;
pub mod model;
pub mod mqdriver;
pub mod mqloop;
pub mod notify;
pub mod obs;
pub mod profiles;
pub mod queue;
pub mod queuestate;
pub mod report;
pub mod subproc;
pub mod termgrid;
pub mod text;
pub mod workflow;
