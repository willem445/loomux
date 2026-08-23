# Marketing research: loomux vs. the agent-multiplexer field

*First pass compiled 2026-07-05; second pass 2026-08-14 (see "Second pass"
below). Sources linked at the bottom; feature claims reflect each project's
public docs/README as of the pass that recorded them — re-verify before
quoting, this category churns fast (see: Crystal).*

Purpose: raw material for tuning the README sales pitch and positioning.
Not shipped to users; keep honest, including our weaknesses.

## Loomux's own frame: three levels of use

1. **Plain terminal multiplexer** — Windows Terminal–class GUI terminal with
   matrix splits, drag-rearrange, session restore. No agents required.
2. **Agent pane multiplexing** — parallel agent CLIs in visible panes, attention
   routing, session browser that resumes Claude Code / Copilot CLI sessions.
3. **Autonomous agentic feature development** — orchestrator/worker groups:
   planning agent files GitHub issues, delegates by typing prompts into worker
   panes, task board, human merge gate, audit log, host-enforced guardrails
   (never `--dangerously-skip-permissions`).

Plus cross-cutting extras: git overlay panel, open-in-IDE, CPU/GPU/mem monitor,
sessions tab. Origin story: the gaps zellij/tmux-lineage tools left, on
Windows, aiming to stay in **one tool for ~95% of agentic coding**.

## Competitor profiles

### herdr — the closest pitch collision

- **What:** Rust TUI multiplexer (~10MB single binary) that runs *inside* your
  terminal, tmux-style. Sidebar shows each agent's state
  (blocked/working/done/idle), auto-detected from process names + output
  heuristics, zero config, 15+ agent CLIs supported.
- **Strengths:** detach/reattach persistence (sessions survive terminal close),
  SSH/remote attach (even from a phone), Unix socket API so agents/scripts can
  drive it, ~12k stars, active (v0.7.1 June 2026), dual-licensed AGPL +
  commercial.
- **Limits:** monitoring + multiplexing **only** — no built-in orchestration
  (ecosystem plugins bolt it onto the socket API), no git integration, no
  resource monitoring, **Windows is beta**.
- **Overlap with loomux:** heavy at levels 1–2 in *pitch* ("agent panes, see
  which one needs you"); light in *substance* because of the GUI/TUI split.
  Their state sidebar ≈ our attention routing; their socket API ≈ our MCP
  server.
- **They own / we can't:** headless servers, detach/reattach, SSH-from-anywhere,
  PTYs surviving app close.
- **We own / they can't:** WebGL git overlay with commit graph + staging, task
  board with clickable issue/PR chips, audit timeline, launcher dialogs,
  resource monitor, open-in-IDE, first-class Windows ConPTY, and the entire
  level-3 orchestration layer.

### ORCA (stablyai/orca, onorca.dev) — the most dangerous competitor

- **What:** YC-backed, MIT, **Electron** "Agent Development Environment" for
  **macOS/Windows/Linux** + iOS/Android companion apps. ~12.4k stars, 745
  releases, daily shipping cadence.
- **Features:** parallel agents in git worktrees across **25–30+ agent CLIs**
  (any CLI that runs in a terminal); "Ghostty-class" WebGL terminals with
  infinite splits and scrollback persisting across restarts; VS Code-compatible
  editor; inline diff annotation with comments routed back to agents; native
  GitHub **and Linear** in-app; SSH remote worktrees with auto-reconnect;
  steer agents from your phone; account/usage/rate-limit tracking; Design Mode
  (click elements in embedded Chromium → HTML/CSS/screenshot into the prompt);
  MCP/hooks/skills.
- **Orchestration model:** human-driven **fan-out**: spawn N agents on a task,
  compare outputs, merge the winner. No autonomous planning agent, no task
  decomposition, no delegated worker queue.
- **Honest read:** the only competitor whose README makes loomux look like a
  subset — it spans our levels 1 and 2 completely, on Windows, open source,
  funded, fast-moving. It punctures the "only Windows-native GUI terminal
  with agent orchestration" combination claim.
- **What survives against it:**
  1. **Level 3** — ORCA has no planning agent; our orchestrator/worker +
     merge gate + audit + guardrails is a layer they don't do.
  2. **Anti-bloat** — ORCA wants to *replace the IDE* (embedded editor,
     embedded Chromium, Linear, mobile, Electron). Loomux is deliberately a
     terminal (Tauri + vanilla TS, open-in-IDE instead of embedding one).
     "Orca is an IDE that contains terminals; loomux is a terminal."
  3. **Trust model** — their pitch is 10–100 agents steered from a phone;
     ours is a small audited fleet, every prompt visible, no agent merges.
- **Action item:** install ORCA on Windows and test its terminal quality
  before making any Windows-superiority claim — and to study the best-funded
  UX in the category.

### Conductor (conductor.build)

- **What:** free, closed-source, **macOS-only** native GUI. Parallel Claude
  Code / Codex / Cursor agents, each in an isolated git worktree with its own
  branch, terminal, diff, and review→PR→merge→archive path. Uses your existing
  agent login; no subscription.
- **Read:** the most polished product in the category and the UX quality bar
  for our level 2 + merge gate. But human-driven (you dispatch tasks; no
  planning agent) and literally unavailable to our Windows audience.

### Crystal (stravu/crystal) — deprecated

- **What was it:** open-source Electron desktop app; parallel Claude
  Code/Codex sessions in worktrees, diff visualization, rebase/squash tooling,
  MIT license.
- **Status:** deprecated **February 2026**, succeeded by commercial Nimbalyst.
- **Read as market signal:** a solo open-source GUI in this space got enough
  traction to justify a commercial successor — and still died inside a year.
  The category rewards momentum and punishes stalling.

### vibe-kanban (BloopAI)

- **What:** open-source web UI (localhost). Kanban board (To Do → In Progress
  → Review → Done) where tasks are executed by 10+ agent CLIs in per-task
  worktrees. Opens PRs with generated descriptions; review comments are fed
  back to the agent; built-in app preview with DevTools.
- **Read:** strongest open-source competitor on level 3's *surface*, but the
  philosophy is inverted: **task-centric, not terminal-centric** — agents run
  behind the board, you interact with cards and diffs, not CLIs. Human is
  still the planner; no orchestrator agent decomposes work. Our task board
  visually resembles theirs, so expect direct comparisons.

### claude-squad (smtg-ai)

- **What:** open-source TUI over tmux + git worktrees. Instance list,
  background auto-accept mode, diff review before merge. Requires tmux + gh.
- **Read:** the minimalist "tmux with agent awareness" option. Competes with
  herdr more than with us. No Windows without WSL. Human-driven.

### amux (mixpeek) — closest in ambition

- **What:** open-source, single-file Python server over tmux. Web/PWA
  dashboard (drive agents from a phone, offline-capable), SQLite task board
  with atomic claiming + iCal sync, inter-agent 1:1 channels with @mentions,
  self-healing watchdog (crash recovery, auto-compaction on context
  exhaustion, stuck prompts **auto-answered in "YOLO mode"**).
- **Read:** the only other tool where agents coordinate agents — but with the
  **opposite trust model**. Amux maximizes *unattended throughput* (headless,
  auto-answering, self-healing, phone-first). Loomux maximizes *trustworthy
  autonomy* (every prompt visible in a pane, audit log, host-enforced
  guardrails, no permission bypass ever, human merge gate). This philosophical
  fork is our clearest level-3 differentiator. No native Windows (tmux).

## Comparison table

| Tool | Form factor | Platform | Isolation | Who plans? | Trust model | Status |
| --- | --- | --- | --- | --- | --- | --- |
| **loomux** | Native GUI terminal (Tauri) | **Windows-first**, cross-platform | worktree-or-branch per task | You **or orchestrator agent** | Visible prompts, audit log, human merge gate, never bypass | Active (ours) |
| herdr | TUI in your terminal | Linux/macOS; Windows beta | panes/workspaces | You (API for scripts) | n/a (monitoring only) | Active, ~12k★, AGPL+commercial |
| ORCA | Electron GUI "ADE" + mobile | macOS/Windows/Linux | git worktrees | You (fan-out, pick winner) | Human diff review→merge; phone steering | Active, ~12.4k★, MIT, YC-backed |
| Conductor | Native GUI app | macOS only | git worktrees | You | Human review→merge | Active, free, closed |
| Crystal | Electron GUI | Cross-platform | git worktrees | You | Human review | **Deprecated** → Nimbalyst |
| vibe-kanban | Web UI (localhost) | Cross-platform | git worktrees | You, via board | PR review, comments→agent | Active, OSS, popular |
| claude-squad | TUI over tmux | macOS/Linux | git worktrees | You | Auto-accept optional | Active, OSS |
| amux | Web/PWA over tmux | macOS/Linux | tmux sessions | You **or agents** | Unattended / YOLO auto-answer | Active, OSS |

## Where each of our three levels stands

- **Level 1 (plain multiplexer):** most of the field doesn't compete — they
  wrap agents, they aren't terminals you'd live in. Exceptions: herdr (TUI)
  and **ORCA**, which claims Ghostty-class WebGL terminals with persisted
  scrollback inside its ADE. Other competitors: Windows Terminal, WezTerm,
  zellij. "One tool for 95% of the day" still holds, but ORCA claims it too —
  from the opposite direction (IDE-that-contains-terminals vs. terminal).
- **Level 2 (agent panes):** the **commoditized axis** — everyone does
  "parallel agents in isolated workspaces with review," and ORCA does it on
  Windows with 25+ agents. Our edges: terminal-first UX and native ConPTY
  polish (verify vs. ORCA before claiming). Don't market on this layer alone.
- **Level 3 (autonomous orchestration):** still the open ground. amux is the
  only agent-coordinates-agents comparable (opposite trust bet); ORCA is
  fan-out-and-pick-a-winner; vibe-kanban/Conductor keep the human as planner.
  A planning agent that decomposes work into GitHub issues, delegates to
  workers, and routes review feedback — behind a human merge gate with an
  audit log — is occupied by nobody else. This is now the **primary**
  differentiator, not one of three.

## Positioning recommendations

1. **Lead with level 3 + anti-bloat, not the combination.** The old claim
   ("only Windows-native GUI terminal with agent orchestration") died when
   ORCA entered the picture — it's Windows-capable, terminal-containing, and
   agent-orchestrating. What's defensible now: (a) the only tool with an
   **autonomous planning agent** behind a human merge gate and audit log, and
   (b) deliberately a *terminal*, not an ADE — lightweight Tauri vs. Electron
   kitchen sink. "Orca is an IDE that contains terminals; loomux is a
   terminal."
2. **Reference competitors, don't frame against them.** Add a README section:
   *"How loomux compares to tmux / zellij / herdr / Conductor / vibe-kanban"*.
   Captures search traffic and answers the inevitable question on our terms.
   Avoid "herdr alternative" as the headline — it anchors us to the layer
   where they're more mature (multiplexing basics, community, 15+ agent
   detection vs. our 2).
3. **Candidate one-liners:**
   - "herdr multiplexes your agents; loomux manages your agents' work."
   - "The agent multiplexer that's actually first-class on Windows."
   - "From GitHub issue to human-gated merge — every prompt visible, every
     action audited."
   - "One tool for 95% of agentic coding: terminal, agent fleet, git, review."
4. **Against amux specifically:** contrast trust models, not features.
   "Unattended fleets that auto-answer their own prompts" vs. "a fleet you can
   watch, steer, and audit — where no agent ever merges."
5. **Distribution beats framing:** get listed in
   [awesome-agent-orchestrators](https://github.com/andyrewlee/awesome-agent-orchestrators),
   the amux "12 tools ranked" style comparison posts, and Nimbalyst's
   "best multi-agent desktop apps" roundups. That's where category shoppers
   actually compare.

## Honest weaknesses (know them before someone else says them)

- No detach/reattach: close loomux and PTYs die. Session browser resumes
  *conversations* (and whole orchestrations), not live shells.
- No remote/SSH story at all.
- Agent-source support: 2 (Claude Code, Copilot CLI) vs. herdr's 15+
  zero-config detections and ORCA's 25–30+ / any-CLI support.
- Early-stage community vs. herdr's ~12k stars, ORCA's ~12.4k (YC-backed,
  daily releases), vibe-kanban's traction.
- No mobile companion, no usage/rate-limit tracking, no Linear integration —
  all table stakes in ORCA's pitch.
- Category risk: fast churn (Crystal died in <1 year); listicle-driven
  discovery favors tools with marketing sites.

## Rebrand: name candidates (researched 2026-07-05)

Motivation: "loomux" ends in **-mux**, anchoring the brand to terminal
multiplexing — the commoditized layer — instead of the agent-orchestration
layer that's actually differentiated.

**Metaphor families already claimed by competitors (avoid):** herding (herdr),
orchestra/conductor (Conductor, Maestro), ocean/pods (ORCA), squads/crews
(claude-squad, CrewAI), -mux suffix (amux, workmux, tmux lineage).

### Shortlist (ranked)

1. **Overloom** — coined: *oversee* + *loom*. "The loom you stand over" —
   agents weave, you watch the work happen and gate the merge. Keeps lineage
   with loomux (rename is legible to existing users).
   Availability: npm **free**, `overloom.dev` **unregistered**, no GitHub
   project or product found. `overloom.com` resolves to an AWS parking-style
   IP — check registrar manually.
2. **Retinue** — real word: the staff of attendants serving a person of rank.
   Inverts the field's herding metaphor — you don't chase a herd, you command
   a staff that reports to you. Fits the merge-gate/audit trust model.
   Availability: npm **free**, `retinue.dev` **unregistered**. Minor non-dev
   collisions: Retinue Systems (dev agency), a Shopify app, a timesheet app.
3. **Castellan** — runs the castle for the lord who keeps the keys (= human
   merge gate). Availability: npm **free**, but `castellan.dev` is taken
   (Vercel), Castellan Solutions is a legacy risk-SaaS brand (absorbed into
   Riskonnect 2022), and OpenStack has a Python key-manager lib `castellan`.
4. **Handloom** — real word; human-driven loom = human-in-the-loop automation
   in one word. npm **free**, `handloom.dev` unregistered. Risk: search
   results dominated by the (large) Indian handloom textile sector.
5. **Treadle** — the pedal the human presses to drive the loom. npm **free**;
   `treadle.dev` taken; word is obscure.

### Killed candidates (and why)

- **reeve** — "Reeve Code" already exists: a CLI-first AI coding agent with
  multi-agent orchestration. Direct competitor collision.
- **jacquard** — perfect story (the programmable loom that inspired punch
  cards), but Phrasee rebranded to Jacquard (jacquard.com, enterprise AI,
  2024) using the *same* loom→computing narrative. npm was free; brand isn't.
- **muster** — npm taken + Virtual Vertex "Muster" is a render-farm
  orchestrator (adjacent category).
- npm-taken: weft, heddle, drover, seneschal, gaffer, flotilla, adjutant,
  garrison, portcullis, gatehouse, quarterdeck, atelier, bailey.
- Established-product collisions: foreman (Ruby), wrangler (Cloudflare),
  shepherd (GNU), helm/tiller (K8s), warden (Ruby auth + Bitwarden), butler
  (itch.io), valet (Laravel), fleet (JetBrains), weave (W&B), warp (terminal!),
  tower (Git Tower), watchtower (Docker), crew/squad/herd/orca families.

### Round 2: non-AI-space names (researched 2026-07-06)

Goal: escape the polluted agent/swarm/orchestra namespace entirely — pick a
word from an unrelated domain whose tie to the tool is *discoverable*, not
advertised (the Ghostty/Zed pattern). Domains mined: weaving deep-cuts,
watermill/canal engineering, falconry, horology, sailing/climbing.

Shortlist (ranked):

1. **Creance** — falconry: the long light line a falconer holds while a
   young hawk flies free. *Supervised autonomy in one word* — the bird
   really flies; the human really holds the line. Availability: npm
   **free**, `creance.dev` **unregistered**, no software product found
   (validating precedent: industrial-AI co "Falkonry" uses the same
   metaphor). Risks: pronunciation (KREE-ənss), French finance homonym
   ("créance" = receivable) adds search noise, obscure — but obscurity is
   what makes it ownable.
2. **Escapement** — horology: the mechanism that takes a spring's raw power
   and releases it one measured, audible tick at a time. Raw power made
   observable and gated = the trust model; each tick = an audit line.
   npm **free**; `escapement.dev`/`.com` registered (check what's there);
   no dev-tool collision found. 10 chars is long for a CLI command.
3. **Sluice** — the gate that controls the flow; a mining sluice also sifts
   gold from slurry (= reviewing agent output). Vivid, 6 letters, verb-able.
   npm squatted (v0.0.2 stub), `sluice.dev`/`.com` registered. Moderate.
4. **Sley** — weaving: to draw warp threads through the reed in order.
   Keeps the loom heritage as a hidden layer. 4 chars, npm **free**,
   homophone of "slay" (fun or confusing, pick one). Very obscure.
5. **Pawl** — the ratchet catch: work advances one way only, never slips
   back, holds until you release it. Sounds like "Paul" — friendly mascot
   potential. npm squatted stub; `pawl.dev` registered.

Killed in round 2: **orrery** (perfect metaphor — a desktop machine making
many moving bodies comprehensible at a glance — but CaseyHaralson/orrery is
already an *AI-agent workflow orchestration CLI* on GitHub, and the domains
are taken), **skein** (threads + a flight of geese; but Skein hash function,
jcrist/skein Hadoop-YARN tool, skein.dev taken), **belay** (you hold the
climber's rope + naval "belay that order" = countermand; but npm taken and
BELAY is a virtual-assistant staffing company — adjacent space), **dovecote**
(homing pigeons report home; Dovecot is the IMAP server), plus npm-taken:
bowline, capstan, millrace, raddle.

### Round 3: pure coinages (researched 2026-07-06)

Real words — even obscure ones — kept colliding, so these are invented
compounds built from the tool's own roots (loom, thread, ward, keep, tend)
so the tie-back survives the invention. Vetted against npm, **GitHub repo
search**, and DNS.

Fully clean (npm free, zero GitHub repos, both `.dev` and `.com`
unregistered):

- **Loomwick** — loom + -wick (the place-name suffix: a village of looms) —
  and a wick is itself a woven thread that carries the flame. Cozy,
  Ghostty-energy, doubly on-theme.

Clean on npm + GitHub, `.dev` free, `.com` parked/registered:

- **Loomward** — loom + ward: the guardian of the looms (wards =
  guardrails), and "-ward" as direction — attention turned toward the loom.
- **Loomkeep** — the castle *keep* of looms: the human holds the gate.
  (One unrelated `loomkeeper.github.io` pages repo exists.)
- **Foreloom** — foreman + loom; you stand *before* the loom. Faint echo of
  "heirloom" (craft, permanence).
- **Overweave** — you oversee the weaving; verb-able. `.dev` free.

Also clean npm+GitHub, weaker: **Loomstead**, **Loomhall**, **Loomsman**
(gendered -man suffix). Near-miss coinages: **Threddle** (thread + treadle —
playful, one tiny personal repo, threddle.com taken), **Tendle**/**Reedle**/
**Vantle** (existing repos/orgs).

Killed on request: **AgentLoom** (`agentloom` npm at v0.1.13 + multiple
GitHub repos), **AgentWeave** (npm v0.1.2 + 3 repos; adjacent to W&B Weave),
**AgentThread** (`agent-thread` npm v0.1.8 + repos; "thread" reads as
conversation-thread in AI tooling). General rule adopted: no "Agent-"
prefix — it's the most squatted pattern of 2026 and files the product into
the pile instead of differentiating it. The category goes in the tagline
("command a fleet of coding agents from one terminal"), not the name — the
pattern every serious competitor (herdr, Conductor, ORCA, Crystal) follows.

### Round 4: pane/window-architecture names (researched 2026-07-06)

Strategic note: "pane" words share -mux's weakness (they name the container,
not the capability) — *except* through the glass lens: the panes are why the
trust model works. Work happens behind glass, visible, and unlike a
dashboard's fixed glazing you can open the window and reach in.

- **Mullion** — the frame member that divides a window into panes; loomux
  *is* the mullion, and it echoes the original brand story (the loom as "the
  frame that holds every thread"). npm **free**, but: `mullionlabs/mullion-ts`
  is an active *LLM context-management* library (20★, pushed Feb 2026) — an
  AI-space collision on the same word — and mullion.dev/.com/.app are all
  registered. Best metaphor of the family, half-taken. (Validating: a dead
  0-star Electron project `zoopdoop/mullion` was literally a "multi-pane
  browser" — the metaphor is natural.)
- **Oriel** — a bay window that projects outward; the seat where you sit and
  watch. Cleanest of the family: npm **free**, `oriel.com` has **no DNS
  record**, GitHub matches are noise. Oxford-college association; homophone
  risk (oriole/Ariel).
- **Casement** — a window that *opens*: you can reach into every pane and
  type, vs. competitors' look-only dashboards. Conceptually the best
  differentiator; npm taken (v4.0.0 lib — survivable via `casement-desktop`,
  but not clean).
- **Transom** — reports arrive "over the transom." npm free; both domains
  registered; transom.org is a known radio site.
- **Muntin** — the strip subdividing panes; npm free, muntin.dev
  unregistered, but the word sounds like "mutton."
- Killed: **glazier** (google/glazier + linebender/glazier — known in the
  Rust GUI community we live in), **fenestra** (npm + WerWolv/Fenestra),
  **spandrel** (chaiNNer's AI upscaling lib!), **vitrine/vitrail** (npm
  stubs; "vitrine" is everyday Portuguese, heavy search noise), **lattice**
  (Lattice HR), **panoply** (Panoply data warehouse), **quarrel** (the
  diamond leaded-glass pane — but reads as "argument"), **panopticon**
  (accurate and dystopian).

### Rebrand caveats

- **The loom prefix is no longer clean (found 2026-08-14).**
  [`ghuntley/loom`](https://github.com/ghuntley/loom) is a Rust AI coding agent
  (1.4k★, proprietary, "if your name is not Geoffrey Huntley then do not use",
  last pushed 2026-04-10) from a well-known figure in exactly our audience.
  It is not a competitor — it's a personal research REPL nobody else may use —
  but it means a loom-prefixed rebrand (Loomwick, Loomward, Loomkeep,
  Foreloom, Loomstead/Loomhall) now shares a name-stem with a visible Rust
  agent project. Doesn't kill the round-3 shortlist; does downgrade "keeps
  lineage with loomux" from an asset to a wash. The non-loom candidates
  (Creance, Oriel, Retinue) are unaffected.
- Checks done: npm registry, DNS resolution, web search. **Not done:**
  trademark search (USPTO/EUIPO class 9/42), GitHub org availability,
  registrar WHOIS (no-DNS ≠ unregistered), social handles. Do these before
  committing.
- Cost of renaming is lowest **now** (pre-1.0, small install base, npm
  package is already `loomux-desktop` not `loomux`). It only gets more
  expensive.
- Whatever the name, keep the loom brand story — it's genuinely good; the
  problem was only the `-mux` suffix pointing at the wrong layer.

### Decision (2026-08-16): **Orrerix**

The name is **Orrerix**. It is a coinage, and it is not on any shortlist above
— it is derived from the one metaphor the research liked best and had to throw
away for availability rather than for fit.

**Why it is a coinage rather than the word itself.** Round 2 killed the bare
word: see *"Killed in round 2: **orrery** (perfect metaphor … but
CaseyHaralson/orrery is already an AI-agent workflow orchestration CLI on
GitHub, and the domains are taken)"* above. Nothing about that verdict was
about the metaphor — the entry says "perfect metaphor" in the same breath — so
the fix is to keep the image and shed the collision. `Orrerix` does that: it is
one token, it is not `orrery`, and it does not carry the `loom` stem the
**Rebrand caveats** section downgraded after `ghuntley/loom` turned up. (Cited
by the passage's own words rather than by line number: a line cite is valid only
at the commit it was derived on, and this file is still being appended to.)

**The brand story it keeps.** An orrery is a desk-sized machine of many bodies
in independent motion, geared so one mechanism keeps them all in phase — which
is the product, not a metaphor for it: independent agents in their own panes,
one orchestrator holding the phase, and a human who can see the whole mechanism
at once. That satisfies the caveat above ("keep the loom brand story") on its
actual terms — the thing worth keeping was the *mechanism* story; the `-mux`
suffix, and now the `loom` stem, were only ever the parts pointing at the wrong
layer.

**The object it points at.** The provenance the branding cites is the Whipple
Museum's [Grand Orrery](https://www.whipplemuseum.cam.ac.uk/explore-whipple-collections/astronomy/grand-orrery) — George Adams, London, c. 1750: the Sun at the
centre, the six planets then known and their moons, each running its own track
at its own period, the whole thing held in phase by one mechanism. Naming a
specific object in a named collection is what makes the story checkable instead
of a vague appeal to astronomy, so the README pitch and the docs landing page
each carry one short paragraph of it and link out; neither retells the history.

**Availability evidence — recorded here as OWED, not as done.** The name was
checked and reported **CLEAR ON ALL 8 SURFACES**, but that check exists as a
screenshot held by the human and is **not yet committed to this repo**. Until
it lands here it is not evidence this document can be read as carrying, so it
is written down as an open item rather than a result:

| # | Surface | Recorded here? |
|---|---|---|
| 1 | npm registry | pending commit |
| 2 | GitHub repo search | pending commit |
| 3 | GitHub org availability | pending commit |
| 4 | `.dev` domain (DNS **and** registrar WHOIS) | pending commit |
| 5 | `.com` domain (DNS **and** registrar WHOIS) | pending commit |
| 6 | web/product search | pending commit |
| 7 | trademark (USPTO/EUIPO class 9/42) | pending commit |
| 8 | social handles | pending commit |

Surfaces 3, 4/5 (the WHOIS half), 7 and 8 are exactly the ones the **Rebrand
caveats** section lists as *"Not done"* and says to do "before committing", so
they are the ones whose recorded result matters most. The rename is phased
(#1153) and phase 1 moves **only** the user-facing brand — window title, in-app
strings, README and `docs/` prose. Every public identity that a clearance
failure would actually cost something to unwind — the GitHub repo name, the npm
package, `productName`/`identifier`, release-asset names — is held back to the
human-gated final phase, and the gate on that phase is this table being filled
in from the committed evidence.

## Second pass (2026-08-14): specs, meta-harnesses and deterministic orchestrators

The first pass surveyed *products shaped like loomux* (GUI/TUI multiplexers with
agent awareness). This pass surveyed eight projects from the adjacent
"agent framework / meta-harness" side of the category, several of them far
larger than anything in the first pass. Repo metadata below is from the GitHub
API on 2026-08-14, not from page scrapes.

| Repo | ★ | Created | What it actually is | Competes? |
| --- | --- | --- | --- | --- |
| bytedance/deer-flow | 80.0k | 2025-05 | LangGraph "SuperAgent" harness — research/code/create, sandboxes, IM gateways | No — different category |
| ruvnet/ruflo | 67.9k | 2025-06 | Swarm meta-harness *inside* Claude Code (MCP hooks, "Queen-led" swarms) | No — prompt/MCP layer |
| openai/symphony | 26.7k | 2026-02 | **Specification** for issue→agent orchestration + thin Elixir reference impl | **Yes — strategically** |
| omnigent-ai/omnigent | 8.9k | 2026-06 | Meta-harness; three-level policy engine, cross-device sessions | Partly |
| agentlas-ai/Agentlas-OS | 1.2k | 2026-06 | Agent packaging/portability + hub marketplace | No |
| ghuntley/loom | 1.4k | 2025-12 | Personal Rust agent REPL, proprietary, stale since 2026-04 | No (naming only) |
| sipyourdrink-ltd/bernstein | 882 | 2026-03 | Deterministic parallel-agent orchestrator; worktrees, signed audit | **Yes — directly** |
| aeonfun/aeon | 661 | 2026-03 | Unattended agent fleet on GitHub Actions cron | No — opposite bet |

### openai/symphony — the strategic threat is the spec, not the product

The Elixir reference implementation is 44 commits and self-described as "a
low-key engineering preview for testing in trusted environments." The asset is
`SPEC.md`: ~2,300 normative lines describing *our orchestration loop* — poll an
issue tracker, create a per-issue isolated workspace, run a coding agent with
bounded concurrency, retry with backoff, reconcile against tracker state — with
policy held in an in-repo `WORKFLOW.md` that is a near-exact analogue of our
`.loomux/workflow.yml`. Apache-2.0, from OpenAI, explicitly inviting ports:
"Implement Symphony according to the following spec."

The risk is **standardization, not displacement**. If Symphony becomes the
interop contract for issue→agent orchestration, loomux is a non-conforming
implementation with a proprietary config format.

Two things in the spec cut the other way, and they are the most useful finding
of this pass:

- **§2.2 Non-Goals** excludes "Rich web UI or multi-tenant control plane" and
  "Prescribing a specific dashboard or terminal UI implementation." They
  deliberately leave our entire surface empty.
- **§15.1** states "This specification does not require a single approval,
  sandbox, or operator-confirmation policy," and the example posture is
  auto-approve every command and file change, hard-fail on user-input-required.
  §15.5 offers hardening *guidance*, not enforcement.

So: **our host-side merge gate is a concrete answer to the exact question
Symphony refuses to answer.** That is a sharper line than anything in the first
pass, and it suggests a strategy other than competing — loomux could position as
*a* conformant implementation, the supervised one, rather than an alternative
to the spec.

Also worth knowing: the agent-runner protocol is the Codex app-server protocol
specifically (§10). Symphony is single-vendor at the execution layer where we
are not.

### sipyourdrink-ltd/bernstein — the closest architectural competitor we have

Closer to us than anything in the first pass. Per-task git worktrees with atomic
task claiming, a TUI dashboard (`bernstein live`) plus a browser GUI, cost
metrics, **49 wired agent adapters**, Apache-2.0, active, Windows via Docker.

Its distinguishing bet is determinism: one LLM call decomposes the goal into
tasks (each with a role, *owned files*, and completion signals), then a plain
Python scheduler takes over — "no model in the coordination loop" — so runs
replay byte-identically. It ships an always-on lineage spine, optional
HMAC-chained audit logs, and Ed25519-signed run receipts a reviewer verifies
**offline** with the receipt file alone.

- **Beats us on:** audit rigour (signed offline-verifiable receipts vs. our
  filterable rows), adapter breadth (49 vs. 3), deterministic replay,
  air-gapped install.
- **We beat it on:** the human merge gate (bernstein has none — a "janitor"
  auto-merges once verification signals pass), pane-level visibility and
  mid-task steering (its dashboards are read-only, and polling-lagged by its
  own admission), GitHub issue/PR-native flow (bernstein is
  goal→decompose, not an issue queue), native Windows, and maturity
  (solo-maintained beta).

### omnigent-ai/omnigent — partial overlap, and a policy engine worth studying

Policies stack across three levels — "server-wide (admin), per-agent
(developer), and per-session (you), with the stricter session rules checked
first" — with built-ins like `ask_on_os_tools`, `max_tool_calls_per_session`,
and a `cost_budget` that supports a soft warning tier before a hard cap. Sessions
follow you across terminal, browser, phone and a macOS native app; teammates can
attach to a running session and co-drive, with their messages executing on *your*
machine. Sub-agents run in parallel git worktrees with cross-vendor reviewers.
Roughly ten sandbox providers (Modal, E2B, Daytona, Kubernetes, …).

**Windows is explicitly degraded**: no native PTY/tmux terminal wrappers, and
no filesystem or network isolation — a Job Object contains the process tree and
enforces resource limits, nothing more. Their own docs recommend Linux/macOS
or WSL. That's our opening, stated by them.

### aeonfun/aeon — the amux trust-fork, now on GitHub Actions

"No approval loops. No babysitting." Skills are Markdown files with frontmatter,
scheduled by cron in GitHub Actions; a self-healing loop
(`heartbeat` → `skill-health` → `skill-repair` → `self-improve`) repairs its own
broken skills, a model scores every run 1–5, and `spawn-instance` forks the whole
thing into a fleet of specialized instances. Guardrails exist but are opt-in or
coarse: read-only capability tiers, irreversible actions fail closed, an optional
"Fleet Watcher" authorization layer.

Not a serious competitor (661★, heavy marketing — "2M GitHub stars secured", a
token on Bankr), but it sharpens the structural point: **loomux stops when you
close the laptop; aeon starts there.** No machine required at all.

### Not competition

- **deer-flow** (80k★, ByteDance) — general long-horizon agent harness on
  LangGraph: sandboxes, skills, sub-agents, long-term memory, observability via
  LangSmith/Langfuse, and IM gateways for Telegram, Slack, Feishu, WeChat,
  DingTalk and Discord. Has a "Terminal Workbench" TUI, but it's a chat surface,
  not a multiplexer. Different category entirely; mined below for ideas.
- **ruflo** (67.9k★, ruvnet) — swarm meta-harness that runs *inside* Claude Code
  via MCP hooks. Exactly the prompt-layer class our README already frames as
  complementary (superpowers, gstack, oh-my-claudecode). Its star count is a
  distribution lesson, not a product threat.
- **Agentlas-OS** — agent portability and a hub marketplace ("your agent is an
  asset, not a setting trapped in one chat"). Different problem. Note its
  distribution tactic: the installer optionally writes a routing block into the
  host's global `~/.claude/CLAUDE.md`.
- **ghuntley/loom** — see the rebrand caveat above; naming collision only.

### Feature gaps this pass surfaced

Ranked by fit with what loomux already claims. The first three are filed as
#998, #999 and #1000; the rest are recorded here.

1. **Strip tracker credentials from the agent child process** (Symphony §15.3,
   filed as #998).
   Symphony executes tracker writes host-side with the host credential and
   requires adapters to declare secret env names for *removal* from the child
   environment. Our README already concedes the `gh` shim can be routed around
   by a determined agent with shell access — this is the layer that can't be,
   short of a machine account. Tension with #434's non-goals; needs a decision,
   not a patch.
2. **Reconciliation: a tracker state change stops a live run** (Symphony §7,
   §14.4, filed as #999). Remove `agent-ready` or close the issue → the running
   worker is stopped and its workspace cleaned. A supervision primitive we don't
   have: the label handshake is currently inbound-only.
3. **Per-task owned-file declarations at decompose time** (bernstein, filed as
   #1000). The
   orchestrator assigns each task the files it owns, so parallel workers
   structurally cannot collide — stronger than worktree isolation, which only
   defers the collision to merge.
4. **Retry onto a different model on verification failure** (bernstein). We
   retry; they re-route.
5. **Signed, offline-verifiable audit receipts** (bernstein). Natural upgrade to
   the audit log, and it would make the "82% unsupervised" claim independently
   checkable instead of self-reported.
6. **Workspace lifecycle hooks** (Symphony §9.4) — post-worktree-create setup
   commands, with required timeouts so a hook can't wedge the orchestrator.
7. **Soft-warn budget tier and per-session tool-call caps** (omnigent — warn at
   $3, hard stop at $5; `max_tool_calls_per_session`). Our token budget is a
   single cliff.
8. **Any remote channel at all.** deer-flow ships six IM gateways, omnigent does
   phone, aeon does notification channels. Our "no remote story" weakness is now
   table stakes, and a notify-and-steer bot is far cheaper than the SSH/detach
   work in #122/#887/#888.
9. **Agent CLI breadth: 3 vs. bernstein's 49.** The first pass logged this as
   2-vs-15; it got worse, and it's the most quotable number against us.
10. Minor: model-scored run quality 1–5 (aeon); a linter for
    `.loomux/workflow.yml` (aeon ships `aeon-doctor` for its own config).

### Updated positioning read

Differentiation **survives this pass and is sharper than the first pass
concluded**. Nothing in these eight is a GUI terminal where orchestration is
visible and steerable pane by pane: Symphony's non-goals exclude it by design,
bernstein's dashboards are read-only, and everything else is headless or chat.
Windows-first survives intact across all eight — omnigent degrades, bernstein
needs Docker, aeon needs Actions.

Two honest caveats:

- The ground we own is uncontested partly because **the market is betting the
  other way**. Symphony sells "manage work instead of supervising coding
  agents"; aeon sells "the most autonomous agent is the one that never asks."
  Our bet is that supervision *is* the product. That's a real fork, not a moat —
  it wins only if unsupervised fleets keep producing work humans won't merge.
- Star counts in this pass (80k, 68k, 27k) are an order of magnitude above the
  first pass's leaders. Distribution, not features, is the gap that compounds.

## Sources

Second pass (2026-08-14): <https://github.com/bytedance/deer-flow>,
<https://github.com/ruvnet/ruflo>,
<https://github.com/sipyourdrink-ltd/bernstein> (+ `docs/reference/KNOWN_LIMITATIONS.md`),
<https://github.com/openai/symphony> (+ `SPEC.md`),
<https://github.com/omnigent-ai/omnigent>, <https://github.com/ghuntley/loom>,
<https://github.com/agentlas-ai/Agentlas-OS>, <https://github.com/aeonfun/aeon>.

First pass (2026-07-05):

- herdr: <https://herdr.dev/>, <https://github.com/ogulcancelik/herdr>,
  <https://betterstack.com/community/guides/ai/herdr-ai-agent/>,
  <https://github.com/yigitkonur/awesome-herdr>,
  <https://aiweekly.co/alerts/herdr-adds-agent-state-awareness-to-terminal-multiplexing>
- ORCA: <https://www.onorca.dev/>, <https://github.com/stablyai/orca>,
  <https://www.ycombinator.com/companies/stably-ai-orca>,
  <https://dev.to/andrew-ooo/orca-review-the-ide-built-for-parallel-coding-agents-15df>
- Conductor: <https://www.conductor.build/>, <https://docs.conductor.build/>,
  <https://news.ycombinator.com/item?id=44594584>
- Crystal: <https://github.com/stravu/crystal>, <https://nimbalyst.com/crystal/>
- vibe-kanban: <https://github.com/BloopAI/vibe-kanban>, <https://vibekanban.com/>
- claude-squad: <https://github.com/smtg-ai/claude-squad>,
  <https://smtg-ai.github.io/claude-squad/>
- amux: <https://github.com/mixpeek/amux>, <https://amux.io/>,
  <https://news.ycombinator.com/item?id=47104424>,
  <https://amux.io/guides/best-ai-agent-multiplexers-2026/>
- Ecosystem lists: <https://github.com/andyrewlee/awesome-agent-orchestrators>,
  <https://nimbalyst.com/blog/best-multi-agent-desktop-apps-claude-code-codex-2026/>
