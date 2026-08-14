# Architecture map

The source tree and its seams. This note is the map every other design note
assumes: what each module owns, and where a new capability is supposed to go.
It lived in the README until #609 moved it here — the README is a pitch, and a
file-by-file map of an 11k-line orchestration module is not one.

Deeper *why* for any individual subsystem lives in its own note in this
directory; the hard constraints (never resize the PTY, no getrandom crates on
the Windows baseline, no live agent testing) live in [`CLAUDE.md`](../../CLAUDE.md).

## The Cargo workspace

The repo root is a Cargo workspace with two members, one `Cargo.lock` and one
`target/`, both at the root:

```
Cargo.toml               workspace root: members, resolver, and [profile.release] — cargo reads profiles from the ROOT manifest only and merely warns about one in a member, so this is where lto/codegen-units/debug/strip have to live (#888 slice A1)
src-tauri/               the desktop app. Links Tauri; owns every #[tauri::command], the capability/ACL manifests, and the Windows desktop surface
crates/loomux-engine/    the orchestration core as a crate that does not link Tauri. Filling up in stages (#888 slice A2): src/report.rs + src/termgrid.rs (batch 1) and src/groupid.rs (batch 2) so far, re-exported under their old `orchestration::` paths so no call site moved with them. `groupid` is why the crate depends on `serde` — see the manifest
```

`loomux-engine` exists because #888 needs a headless Linux daemon to drive
orchestration, and a server that must build webkit2gtk in order to run is not a
deployment shape. One rule governs it: **`src-tauri` depends on the engine and
the arrow never points back**, with everything the engine needs from its host
arriving as a trait (`EventSink`, `PaneHost`) rather than an `AppHandle`. The
modules below move into it in stages — see
[`engine-extraction.md`](engine-extraction.md) for the boundary, the order, and
the publish stance.

## The source tree

```
src-tauri/src/
  pty.rs            PTY lifecycle (spawn/write/resize/kill) + output streaming; per-kind Terminal shells (PowerShell/cmd/Git Bash, #194) + Git Bash discovery. The input path never blocks under the global `ptys` map lock (#719): the per-pane writer/master are shared handles cloned out under it, `write_pty`/`change_dir` run their whole body on `spawn_blocking`, and there is deliberately no backend write queue — back pressure is what bounds memory. See doc/design/pty-input-path.md
  ptyout.rs         per-pane coalescing of PTY output before it crosses IPC (#712): a `pty-output` event costs one one-shot script compilation on the GUI thread, so the emit rate is bounded to one per 60 Hz frame per pane (leading-edge, so a quiet pane's chunk still crosses on arrival) instead of one per `read()` return. Bytes, order and the orchestration output ring are untouched. See doc/design/pty-output-coalescing.md
  sessions.rs       agent session discovery (one collect_*_candidates fn per file-backed source, plus scan_opencode for the one that is a SQLite store); metadata-first + a persisted head-parse index (session-index.json) so a long history costs a stat, not a parse (#493). See doc/design/session-index.md
  orchestration/    agent groups: registry, guardrails, MCP server, audit. Four agent-CLI
    adapters (claude, copilot, gemini, opencode) whose per-CLI differences live as DATA in
    `CLI_CAPS` rather than as `if cli == …` at a call site; opencode's whole seam — an
    env-delivered config document, its permission-key containment and its per-group session
    store — is `doc/design/opencode.md`. Compact-survival is
    layered (#329, #416, #417): a durable role CONTRACT riding the CLI's own system-prompt layer
    (a generated `~/.claude/agents`/`~/.copilot/agents` custom-agent file on both CLIs — Claude's
    own inline `--agents` JSON flag was replaced with a file in a later correction round after a
    live demo hit Windows CreateProcessW's 32,767-character command-line limit) is the primary
    defense against compaction diluting it; `end_group` reclaims a group's own generated files when
    it ends cleanly, and a one-shot startup sweep (`sweep_orphaned_agent_files`, #464) reclaims any
    left behind by a group that never reaches `end_group` at all (a crash, or — the case that
    actually filled a real `~/.claude/agents` with 1,111 stray files — a test run, whose ability to
    write there at all is itself fixed at the source in #502); both Claude's `PreCompact`/`SessionStart` hooks AND Copilot's own
    `preCompact` hook are TRUSTED evidence sources for detecting a compaction (no banner/
    token-drop guessing needed once configured) — Claude additionally gets a native
    `SessionStart(compact)` re-grounding signal Copilot's docs confirm has no equivalent, so a
    Copilot compaction always closes via loomux's own reinjection instead. Both CLIs have a real
    `/compact` command loomux can nudge at a natural lull or on request (Copilot's confirmed via
    its own CLI command reference, one tick later than Claude's since it lacks that native
    re-grounding signal); banner/token-drop/marker inference remains the fallback tier for
    hook-less setups. Since the CONTRACT itself now durably rides the system-prompt layer for
    almost every agent, the mandatory post-compact re-grounding notice is SLIM by default (a
    re-sync pointer to the task board/durable state/roster, plus the directive ledger tail
    inlined) rather than re-embedding instructions the agent's system prompt already holds
    permanently — full verbatim re-embedding is now reserved for the one documented case where
    a Copilot block's contract does NOT ride `--agent` (a user-authored native
    `.github/agents/*.md` persona). That mandatory notice is considered delivered when the AGENT
    acknowledges it — its own next MCP call — not when loomux's submit sampler happens to see the
    Enter (#535): the sampler misses routinely on a busy/repainting pane, and keying on it re-sent
    a re-grounding that had already landed, interrupting a working agent. A retry is also never
    spent into a live turn (deferred while output is flowing, bounded so a permanently noisy pane
    can't suppress it); the 3-attempt bound and the visible give-up for a genuinely lost
    re-grounding are unchanged. Every surface that reports a finished re-grounding says WHICH
    evidence closed it and claims only what that evidence proved (#546) — `ReinjectAck` owns the
    vocabulary once, and the badge (`re-grounding delivered` vs `re-grounding unproven (agent
    alive)`) and the audit (`compact-reinjection-confirmed` vs
    `compact-reinjection-liveness-only`) both derive from it. The liveness arm proves the agent
    is alive and working, never that it read the re-grounding and not even that the paste
    arrived; neither arm proves the read, because nothing loomux can observe from outside a
    session does. The same activity signal now also
    feeds the unconfirmed-delivery detector (#539): a pane whose box no longer holds our paste
    AND whose agent has called a loomux tool since is recorded as busy rather than announced as
    stranded, and the alarms that remain coalesce per pane into one notice naming every
    delivery. See doc/design/orchestration.md's Compact-nudge section and "The
    unconfirmed-delivery detector gets evidence about the AGENT".
    workflow.rs     the block model (#222): a repo's agent roster as data — `<repo>/.loomux/workflow.yml` parse + validation. A block's id is the agent's identity; `kind` is its CAPABILITY CLASS, and stays a closed 4-variant enum, so a repo file can declare five reviewers with five prompts but can never grant one write access. An optional `role_hint` (#250/#324: `advisor` | `process`) rides alongside `kind` for persona/template/badge selection only — inert w.r.t. capability, which keeps keying exclusively off `kind`. Also the ENFORCED merge gate (#222/#197): reviewer-attributed verdicts (pass | fail | escalate) as durable state, and the pure gate decision the `gh` shim mirrors — `gh pr merge` is refused until every reviewer the gate names has recorded a pass, and no human grant or autonomous marker can open it. See doc/design/workflows.md and doc/design/supervisor-skills.md
    lessons.rs      durable per-repo lessons (#268): `<repo>/.loomux/lessons.md`, a plain-Markdown convention file (no schema, no MCP write tool — edited and reviewed like any other file) read-and-capped into the orchestrator's kickoff only. Hard byte cap with oldest-drop truncation, wrapped in a data-not-instructions provenance framing (#189) — never grounds to bypass the merge gate. See doc/design/lessons.md
    profiles.rs     repo-authored personas from `.github/agents/*.md` (#51, harvested from PR #105): append/replace modes with a non-overridable loomux mechanics core. Compiled to each CLI's native custom-agent flag — `claude --agent`/`copilot --agent` (both resolve a NAME against a user/project agent-file directory; a user-authored file resolves unwrapped) — and (#416) the built-in role contract itself now rides the SAME native flag for every block, persona or not: a block with no user-authored file gets a loomux-generated file in the CLI's own user-level agent directory (`~/.claude/agents` or `~/.copilot/agents`, never the repo's `.github/agents/`). Claude's inline `--agents '<json>'` flag was the original #416 mechanism; it was replaced with the same file-based approach Copilot always used after a live demo hit Windows CreateProcessW's 32,767-character command-line limit once the full contract, not just a short persona, rode it
    groupid.rs      MOVED to crates/loomux-engine/src/groupid.rs (#888 slice A2 batch 2) and re-exported here, so `orchestration::groupid` / `orchestration::GroupId` still resolve. The validated group identifier (#904, #888 §0 layer 2). CLAUDE.md constraint 6 USED to trust `group_id` as a path segment because the caller is our own in-process webview — a fact about the TRANSPORT, not a credential, and one a network client would not supply; #904 rewrote it. `GroupId` moved that trust onto a type: one constructor (`parse`), a strict ASCII alphabet that makes `..`, separators, drive letters, control bytes and non-ASCII unspellable, and no `From<String>`/`new_unchecked` — deserialization goes through the same gate, so a hand-edited state file cannot mint one either. Deliberately not `AsRef<Path>`: an id becomes a path only inside `group_dir_at`, the one declared root helper, which `OrchRegistry::group_dir` is the `&self` convenience over — the `~50 orch_*` commands parse their raw string once at the boundary (`command_group`) and thread the type from there, and a source-scanning test pins that no second join grows back. That test walks BOTH source roots, not just `src-tauri/src`: the orphan rule puts the only legal `impl AsRef<Path> for GroupId` in whichever crate owns the type, so once the type moved, a single-root scan would have been green forever while enforcing nothing. Refuses, never rewrites. Two shapes the type ruled out on contact and that are worth knowing before you reach for them: `AttentionItem.group` stays a `String` (it is legitimately EMPTY for a plain pane, which a `GroupId` cannot be), and `QueuedDelivery.group` is an `Option<GroupId>` (there is no default group, and a pre-#468 snapshot entry without one must still PARSE so its payload surfaces as an orphan — `None` says "no recorded identity" in the type instead of via an empty string). See doc/design/groupid-and-path-roots.md
    humanq.rs       the human-question registry (#946): `questions.json` in the group dir, plus `ask_human`/`list_questions`/`withdraw_question`. Exists because a blocking orchestrator question is a fleet outage — a pane showing its CLI's own modal cannot take ANY delivery, so every worker report queues behind it — and `ask_human` returns an id immediately instead. The pending record is ENGINE state, not a liaison agent's: an agent pane compacts, dies and gets idle-killed, which would re-create that outage one level up; a presenter (webview inbox, liaison pane, #947 chat bridge) is a client of the record, never the record. The trust boundary is the point: every agent may ASK, no agent may ever ANSWER — `call_tool` is a closed match and no arm reaches `answer_question`, `AnswerSource` is a closed enum the ENTRY POINT supplies (so `orch_question_answer` hard-codes `Webview` rather than taking a `source` string), and both halves are pinned by a behavioural sweep plus a source scan. Reads are loud about a malformed file, deliberately unlike `tasks()`: every mutation is a whole-file read-modify-write, so "unparseable reads as empty" would let the next ask destroy a human's outstanding questions. See doc/design/human-questions.md
    digest.rs       session-digest friction extraction (#250/#324): one normalizer per agent CLI (Claude `.jsonl`, Copilot session-state, OpenCode `message`/`part` rows) turning a transcript into one event stream, then reduces it, deterministically and LLM-free, into "friction windows" (a tool error, a near-duplicate rerun, a test that went red-then-green, a reverted edit) — the `session_digest` MCP tool a `role_hint: process` block calls to mine a finished session cold. See doc/design/supervisor-skills.md
  obs.rs            crash observability: panic hook, breadcrumb log, unclean-exit notice; `data_root()` — the `<data dir>/loomux` root, overridable via `LOOMUX_DATA_DIR` (#394) for an isolated profile
  voice.rs          voice prompts (#58): mic capture (cpal) -> local whisper.cpp subprocess
  uistate.rs        durable UI state: atomic tabs.json store (project tabs #63) + settings.json store (app settings #370) + sshprofiles.json store (SSH connection profiles #887, hostnames/ports/identity-file PATHS only -- never a credential) -- same atomic-write/corrupt-quarantine primitives, opaque JSON owned by the frontend schema
  fileedit.rs       file-editor overlay (#174): lazy tree, read/write (atomic + hash conflict), streaming gitignore-aware search/replace (#207) + path-only name enumeration (#214); server-side path safety
  filemgr.rs        file-MANAGER pane (#214): list, new file/folder, rename, delete-to-Recycle-Bin, open-with-default-app, open-with chooser, reveal-in-OS-file-manager; reuses fileedit's path choke point. Shell APIs come from the `windows` dep we already have (ShellExecuteW + SHFileOperationW)
  filehash.rs       file hashing (#214): SHA-256/512, SHA-1, CRC-32/16/8 — streamed off-thread on a worker (never the main thread), cancellable via the #207 registry
  winpath.rs        fresh PATH/PATHEXT resolution (registry-merged, so a CLI installed mid-session is findable) + the `which`-style resolver shared by "open in editor" and direct-CLI spawn. Also resolves what the gh/git gate shims bake in: the absolute `sh.exe` (#335) and, from the same install layout, the POSIX coreutils dir the gate normalizes with (#509) — derived, never hardcoded. See doc/design/shim-path-integrity.md
  command_manifest.rs  single source of truth for the ACL manifest's 143 app-command names (#363) — shared by build.rs (include!, feeds the app manifest) and lib.rs (feeds tests/acl_manifest.rs, the coherence guard). Don't hand-count it: `app_commands_len_is_143` pins the total and carries the per-delta provenance, so that test is what this number tracks. See doc/design/acl-manifest.md
  lib.rs            Tauri wiring
src-tauri/
  capabilities/     ACL grants (#363): default.json grants `main` every command via the `main-ui` permission set; plugin-zero-template.json is the zero-grant template a future #360 plugin webview binds to
  permissions/      hand-authored module/tier permission sets (permissions/sets/*.toml) + Tauri-generated per-command allow-/deny- pairs (permissions/autogenerated/*.toml, DO NOT EDIT)
src/
  transport.ts      the ONE module that imports @tauri-apps: the `EngineTransport` seam (invoke/listen + host version, folder picker, close gate). Installable (`setEngineTransport`), so the frontend is mockable in `node:test` and a future remote engine (#888) is a second implementation, not a second frontend. Enforced by test/transport.test.ts -- see doc/design/engine-transport.md, and doc/design/remote-engine-protocol.md for the wire contract and threat model that second implementation would speak (H1-H9 resolved; no listener code yet).
  pty.ts            typed bridge to the backend (invoke + event bus), over transport.ts
  theme.ts          the one palette (#879): the slate/mist ground, the eight named hues, the THREE colour channels they serve (state / interaction / identity -- what separates the channels is POSITION, since the eight-hue set collapses under colour-vision deficiency and the four state dyes must not), the elevation ladder, the type roles, and the xterm ITheme built from them. Colour lives in three languages that cannot see each other -- styles.css's `:root`, index.html's pre-paint block (which runs before the bundle), and the ITheme (terminals are a WebGL canvas that custom properties never reach) -- so this module is the source and test/theme.test.ts pins the other two to it. Rationale and the rules every surface obeys: doc/design/ui-redesign.md (DOM-free, unit-tested)
  pane.ts           one pane: xterm instance + header UI -- or, for a CONTENT pane, a PTY-less surface: file manager (#214), file editor or git view (#217)
  heldbadge.ts      pure delivery-held badge presentation mapping (#246): reason -> header-chip label, for the moment loomux is withholding a prompt because it believes the pane's box is human-occupied, or (#420) because an interactive question/permission TUI is on screen and a blind Enter would silently answer it (DOM-free, unit-tested)
  humanorigin.ts    pure short-lived "this PTY write came from a human" latch (#518): marked by `term.onKey` and the two `term.paste()` sites, read synchronously in `onData`, closed at the end of that turn. Terminal-manufactured data (OSC/DCS query replies, device attributes, focus reports) is emitted in a different turn and reads false — a structural origin test, not a byte-shape guess at an open set of reply shapes (#440 B2-R's argument, applied to the backend's keystroke clock). A second, DEFERRED mark covers the two human paths `onKey` never sees — an IME commit (which xterm sends from a `setTimeout(0)`, a later task than the `compositionend`) and the `insertText` path (dead keys, soft keyboards) — marked from capture-phase textarea events on an ancestor and closed over two timer hops, so no registration order can let the close beat xterm's send (#528 B1: a one-hop close did exactly that, classifying every IME commit non-human). Rides through `ptywrite.ts` to `write_pty`'s `human` flag (DOM-free, unit-tested with real timers for the ordering)
  grid.ts           split-tree layout, dividers, focus, drag/maximize/minimize
  layout.ts         pure drag-reorder geometry (unit-tested, DOM-free)
  tabs.ts           project tabs (#63): TabManager -- tab list, active tab, routing (DOM-free)
  workspace.ts      one tab = a Grid + its own dock; hide/show, GL policy, preview composite
  tabbar.ts         the tab strip: switch/close/new, rename, color, alert chips, deterministic agent counter + orchestration markers (#194), preview
  tabroute.ts       pure tab routing + preview scale/sanitizer (unit-tested, DOM-free)
  tabstore.ts       pure encode/decode + schema validation of the persisted tab set (tabs + per-tab pane layout + restore pref, #194; a tab's group BINDINGS are a set and each orch pane records its own group, #485)
  settings.ts       durable app-wide settings (#370): pure encode/decode (DOM-free, unit-tested) + an in-memory singleton (getSettings/setSettings) for synchronous reads from pane.ts's keydown handler. Config-file-only -- no Settings UI exists yet
  sshprofile.ts     SSH connection profiles (#887): pure encode/decode + per-entry validation of sshprofiles.json (DOM-free, unit-tested). Allowlist in BOTH directions and an identity-file path guard, so the no-secrets invariant is structural -- a credential cannot survive a load/save cycle. OWNS the `RemoteShell` value set (type + list + default). See doc/design/ssh-panes.md
  sshcommand.ts     pure ssh argv builder for SSH panes (#887): option flags, the end-of-options separator, and ONE quoted remote-command string per declared remote shell (posix single-quoting / cmd.exe double-quote doubling, both adversarially unit-tested), plus the fresh->resume rewrite a reconnect uses. Takes flat primitives, never the profile type, and its `program` parameter is the fake-ssh test seam. DOM-free, unit-tested. See doc/design/ssh-panes.md
  restoredecision.ts pure restore-vs-fresh-vs-ask decision for the boot splash (DOM-free, unit-tested, #194)
  panerestore.ts    pure per-pane restore policy + layout-tree -> ordered rebuild plan + agent resume-command builder (DOM-free, unit-tested, #194)
  restoresplash.ts  cold-boot "restore last session?" overlay (thin DOM over restoredecision.ts, #194)
  tabcounts.ts      pure per-tab live-agent counter + live/dormant orchestration markers (DOM-free, unit-tested, #194)
  groupresume.ts    pure whole-group resume plan: orchestrator first, delegates rejoin-or-skip, and ONE plan is ONE group -- members are partitioned by their own recorded group, a member naming another group is refused, an unattributable set fails loudly (DOM-free, unit-tested, #194/#485)
  sessionreconcile.ts pure post-start session-id adoption matcher (refuses on any ambiguity) + the dormant card's "resume last session" candidate lookup (DOM-free, unit-tested, #440). See doc/design/session-id-learning.md
  panefit.ts        pure "hidden => no PTY resize" decision (the no-resize invariant)
  panethrottle.ts   pure flush policy for a VISIBLE but unfocused pane's PTY output (#720): batch into one write per window so xterm renders it ~6x less often (renderRows coalesces within an animation frame, never across one); leading-edge, so a quiet pane is never delayed, and byte-exact -- only the moment of the write moves. DOM-free, unit-tested. See doc/design/pane-render-throttle.md
  webglretry.ts     pure BOUNDED backoff for re-acquiring a lost WebGL context (#720): a context is a capped resource, so one pane re-acquiring evicts another's and an unbounded retry live-locks between panes. 2s/10s/60s then stop; a hide/show or a 5-minute healthy lifetime opens a fresh streak. DOM-free, unit-tested. See doc/design/pane-render-throttle.md
  sessions.ts       session browser sidebar: source/role chips, and (#1) each session's recorded task/goal, repo, branch, and PR (when the board has one) — absent rather than guessed for a session predating the field
  sessionstore.ts   the app's ONE session list: single-flight scan sharing so no two consumers can scan the disk at once (#493) (DOM-free, unit-tested). See doc/design/session-index.md
  sessionmeta.ts    pure session-browser task/repo-branch/PR formatting + truncation (#1) (DOM-free, unit-tested)
  resumeerror.ts    pure classification of a resume failure's structured backend tag into a UI affordance -- start-fresh vs a plain error (#412) (DOM-free, unit-tested)
  restorecard.ts    pure dormant-card lifecycle state machine -- idle/pending/error, click acknowledged immediately, failure always lands on a persistent error state (#479) (DOM-free, unit-tested)
  launcher.ts       in-pane welcome / pane-setup form (Agent / Orchestrator / Terminal / File-explorer / File-editor / Git / Workflow / SSH kind picker; the SSH section is also the inline profile editor -- what it launches is what it saves)
  modelcatalog.ts   pure "which models may this CLI be pinned to?" (#935): the curated-suggestions/backend-probe merge (probe leads, curated backs it, inherit row pinned first), the picker's selection rule, and the injected-probe memo both surfaces share. States no vendor fact of its own. DOM-free, unit-tested. See doc/design/model-catalog.md
  modelpicker.ts    the model dropdown + `custom…` escape, shared by the launcher's per-role selector and the workflow pane's block editor (#935) -- DOM wiring only, every decision it makes is modelcatalog.ts's
  panesetup.ts      pure kind-selection + validation core for the welcome screen (DOM-free, unit-tested). Also the SSH launch seam (#887): the S1->S2 compose, the refusals for values the profile store would silently discard, and `sshOrchestrationRefusal` -- the #887/#888 boundary, which reads the spawn options and the pane's own state by ONE union rule. See doc/design/ssh-panes.md
  orchestration.ts  frontend half of agent groups (panes, badges, focus); also the human-only cross-workspace channel commands (connect/disconnect/set-sender, standalone-pane prepare/bind/adopt) + `orch-channel` event routing (#271)
  shortcuts.ts      app-level keybindings (single source of truth)
  fileapi.ts        typed bridge to fileedit.rs (per-feature wrapper, like git.ts)
  fileedit.ts       the file editor (#174): tree + code editor + "Go to file" name search + content search/replace. Two hosts: the Alt+F overlay, and an editor PANE (#217, `embedded`) (DOM wiring)
  fileexplorer.ts   the file MANAGER a files pane hosts (#214): browse, open-with-default-app, new file/folder, rename, delete, context menu, SHA-256 column, Go to file (DOM wiring)
  fileexplorermodel.ts pure file-manager core: listing order, rooted navigation, breadcrumb, formatting, inline-edit validation, op-target binding (DOM-free, unit-tested)
  filemenu.ts       pure context-menu model: what appears, what it acts on (target bound at menu-open) (DOM-free, unit-tested)
  contextmenu.ts    generic context-menu renderer, `MenuItem<A>`: placement, submenus, Esc/click-away (DOM wiring)
  panemenu.ts       pure pane-header connect-menu model (#271): Connect/directional-completion/Join-as-receiver/Cancel/Disconnect/Make-sender per pane + pending-arm state, standalone panes included (DOM-free, unit-tested)
  pasteflow.ts      pure terminal copy/paste keydown gesture decisions (#370): Ctrl+V/Ctrl+Shift+V + Ctrl+C (selection-gated)/Ctrl+Shift+C key matching, the copy/paste/pass disposition enum that drives pane.ts's preventDefault() calls -- same for every pane kind, no pane-kind branch (DOM-free, unit-tested)
  channel.ts        pure connect-gesture reducer (arm/complete/cancel/set-sender) + per-channel color/number/direction chip derivation (#271) (DOM-free, unit-tested)
  filehashmodel.ts  pure hashing policy: auto-hash threshold, digest cache keying (path+size+mtime), formatting (DOM-free, unit-tested)
  filemgr.ts        typed bridge to filemgr.rs + filehash.rs (per-feature wrapper, like fileapi.ts)
  filematch.ts      pure file-NAME matching + ranking for "Go to file" (#214, DOM-free, unit-tested)
  modal.ts          the shared confirm/choice dialog (used by the editor and the file manager)
  filetreemodel.ts  pure lazy-tree model: sort/merge/flatten (DOM-free, unit-tested)
  fileicons.ts      pure filename -> inline-SVG icon mapping (DOM-free, unit-tested)
  searchresults.ts  pure search grouping + tree-hit + replace-selection model (DOM-free, unit-tested)
  searchsession.ts  pure streaming-search state machine: batch/cancel + result cap + enumeration-source pick (#207, DOM-free, unit-tested)
  dirtystate.ts     pure conflict/close-guard decisions -- shared by the editor's Esc/close and the editor PANE's close (#217) (DOM-free, unit-tested)
  eol.ts            pure line-ending detect/normalize/re-apply for EOL-safe dirty tracking (unit-tested)
  findwidget.ts     pure in-file-find logic: regex build + "n of m" match count (DOM-free, unit-tested)
  editorwidget.ts   swappable editor widget: lazy CodeMirror 6 (One Dark) + custom find panel + textarea fallback
  voice.ts          pure voice logic: target decision + push-to-talk state machine
  voicecontrol.ts   global single-capture controller; routes transcripts to focus
  main.ts           composition root (owns the TabManager + OrchWiring router)
e2e/                Playwright E2E PoC (experimental, `e2e-windows` CI job) — see doc/design/e2e-testing.md
npm/bin/loomux.js   the whole `loomux-desktop` npm package: a dependency-free Node launcher that fetches/installs/launches the desktop app. Command-based (#845) — plain `loomux` never installs over an existing install, only `loomux update` does; `update` is channel-aware, orders releases by semver itself (never GitHub's mutable `latest` pointer) and refuses a downgrade outright (#815/#816/#846). Pure logic unit-tested in test/launcher.test.ts. See doc/design/npm-launcher.md
```

## Extension seams

New agent sources add a `scan_*` in `sessions.rs`; new backend
capabilities add a `#[tauri::command]` plus a typed wrapper in `pty.ts` — or, for
a self-contained feature, a dedicated wrapper module (`git.ts`, `gh.ts`,
`fileapi.ts`). Either way the wrapper reaches the backend through `transport.ts`,
which is the only module that imports `@tauri-apps` (constraint 5, enforced by
`test/transport.test.ts`); a NEW Tauri API — not a new command, a new *API* —
is added to the `EngineTransport` interface there and nowhere else. **A new
command also needs an ACL grant** (#363): add its bare name to
`command_manifest::APP_COMMANDS`, then grant it to `main` — directly in
`capabilities/default.json` or via one of the sets under `permissions/sets/`
aggregated into `main-ui`. `tests/acl_manifest.rs` fails loudly if either step
is missed; see doc/design/acl-manifest.md. A new command, event listener or
timer must also satisfy the webview-thread responsiveness invariants, which
`tests/perf_dispatch.rs` and `test/perfpolicy.test.ts` will enforce against a
declared manifest (#743 S2/S3) — see doc/design/performance.md.
