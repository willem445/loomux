# Resource guards: throttling CPU-bound commands across workers (#318)

Every worker in a group runs in its own worktree, on its own schedule. Nothing
stops two, five, or fifteen of them from calling `cargo build` in the same
minute — and on the human's own machine that is exactly what happened: enough
concurrent builds to make the box unusable while an orchestration run was in
flight. The fix has to be **generic** (the issue's own framing): not "special-
case `cargo build`", because the next repo's offender is `make`, or `npm run
build`, or a test suite that shells out to a real compiler. `resources:` in
`.loomux/workflow.yml` lets a repo name its own CPU-bound command patterns and
how many may run at once; a PATH shim serializes matching invocations through a
slot pool before the real binary runs.

This note is the architecture and the decisions behind it. The authoritative
source for the mechanics is the doc comments on
[`workflow::guard_class_for`](../../src-tauri/src/orchestration/workflow.rs)
and [`guard_shim_sh`](../../src-tauri/src/orchestration/mod.rs) — this note
explains *why* those shapes were chosen, not a duplicate of what they already
say precisely. User-facing configuration is in the README's Architecture map
entry for `workflow.rs`.

## What kind of thing this is

`resources:` sits outside the capability-closure rule `doc/design/workflows.md`
states for `blocks`/`edges`/`gates` (*"a workflow file can never grant a
capability"*) — because a resource guard doesn't grant or deny capabilities at
all, it only ever **delays**. A repo names a command pattern it wants
throttled; the worst a guard can do to that command is make it wait, and past
`timeout_minutes` it runs anyway. There is no `resources:` spelling that lets a
repo file *grant* a worker something it didn't already have. That asymmetry is
also why the failure direction runs backwards from every other spine in this
codebase — see *Degrade, not deny*, below.

## The what/how-many split, and why a repo can only lower N

A guard has two axes: **what** counts as one class (`cargo build` and
`cargo test` both contending for the same `heavy-build` pool, because they
compete for the same CPU, not because they're the same command), and **how
many** may run at once. The repo owns the first axis outright — it's the only
party that knows its own build graph. It does **not** own the second axis
outright:

```yaml
resources:
  concurrency:
    - class: heavy-build
      max: 2               # requested — a group/machine guardrail may only LOWER this
      timeout_minutes: 30  # after this a queued command runs anyway (fail-open)
      commands:
        - { program: cargo, args: [build] }   # leading positionals; flags skipped
        - { program: cargo, args: [test] }
```

`ResourceClass::max` is what the repo *requests*. The slot count a shim
actually enforces is `workflow::effective_slots(class, overrides)` —
`min(repo_max, override)` — where `overrides` is
`Guardrails::resource_guard_limits`, a machine/group-side
`BTreeMap<String, u32>` clamped to `workflow::MAX_RESOURCE_GUARD_SLOTS` (32,
the same ceiling both sides are held to; a machine override is not a loophole
around that sanity bound either). A class with no override just runs at its
own `max`.

The reason this can only go one direction is the same reason the capability
closure exists, aimed at a different resource: **a workflow file is untrusted
input.** It arrives with a `git clone`. If a repo's own `max:` could *raise*
the enforced slot count, a repo could ship `max: 999` — which is not a security
hole (nothing is granted), but it would make the guard a no-op on exactly the
machine it exists to protect, silently, from a file whoever opened a PR wrote.
The human who owns the machine is the only party who knows how many cores it
actually has and how many other things are competing for them; only they can
raise a ceiling, and a repo can always ask for less than that ceiling for its
own reasons (a class that's genuinely more contentious than the machine
default assumes). `Guardrails::resource_guard_limits` has no launcher UI yet —
config-file-only, a documented follow-up, the same posture the module doc
takes for every other guardrail before its knob existed.

## The enforcement point: extends the #83 shim, doesn't duplicate it

loomux already has exactly one mechanism for "an agent's invocation of a named
program is intercepted and can be redirected": the `gh`/`git` PATH shims from
#83 (`write_shim`/`ensure_shims`/`agent_pane_env`). Resource guards are the same
mechanism applied to a new, **dynamic** set of programs instead of the fixed
`gh`/`git` pair:

- `guarded_shim_programs(classes) -> Vec<String>` is a pure function (no I/O)
  that derives the distinct guarded program names from a compiled spec —
  `cargo`, `npm`, `make`, whatever a repo names. `OrchRegistry::ensure_shims`
  re-derives this set from `resource_guards(group)` fresh on every spawn, so
  editing `.loomux/workflow.yml` and relaunching picks up a changed set of
  guarded programs with no code change.
- `guard_shim_sh(program, real)` / `guard_shim_cmd` are the POSIX template and
  Windows `.cmd` delegator, same shape as `gh_shim_sh`/`git_shim_sh`. A
  non-matching invocation (`cargo check` when only `build`/`test` are guarded)
  `exec`s the real binary immediately — zero added latency, and no slot file is
  ever touched for it.

**`gh` and `git` are deliberately excluded from the dynamic set**, even if a
repo names them under `resources:`. They already carry the merge/release-gate
shim, which has many `exec "$REAL_GH" "$@"` exit points woven through
security-critical logic (the merge gate, the release gate, grant handling).
Folding resource-guard checks into all of those exit points would risk that
gate for a shape the schema was never meant to support — the schema's own
examples all name build tools (`cargo`/`npm`/`make`), never `gh`/`git`. A repo
that names `gh` or `git` under `resources:` simply gets no additional shim for
it, the same outcome as naming an uninstalled program. Worth stating plainly so
nobody proposes guarding `git push` as a "CPU-bound" class later: it isn't
supported, on purpose, and won't silently half-work.

The Rust-side matcher, `workflow::guard_class_for(classes, program, argv)`, and
the shim's own shell scanner have to agree on every case — program identity by
basename (a full path matches the same as a bare name, so `cargo` matches
`C:\...\cargo.EXE`), and a guard's `args` prefix-matching the invocation's
*positional* tokens with flags skipped. They're kept in lockstep by citing each
other in both doc comments rather than sharing code, because one side is Rust
and the other is a POSIX `while read` loop with no parser — there is no shared
function to extract.

## Slot mechanics: mkdir + a PID reaper, not `flock` — this is the primary mechanism, not a fallback

The obvious shell implementation of a counting semaphore is `flock`: open a
slot's fd, `flock` it, and the kernel releases it on any process death,
`SIGKILL` included, with no explicit release code needed. That was the first
design considered, and it doesn't work here — **the Windows 10 Git-Bash
baseline this project targets ships no `flock(1)`**, confirmed on the target
machine (`which flock` finds nothing in the loomux pane `PATH`, recorded on the
accepted #318 plan). So `mkdir`'s POSIX atomic-claim guarantee
(`mkdir "$slot_root/slot.$i"` succeeds for exactly one racing caller) is the
**primary** slot-acquisition mechanism on this project's baseline, not a
fallback path documented for completeness.

The cost of giving up `flock` is that a slot does **not** self-release on a
hard kill. Release is therefore two-layered:

1. `trap 'loomux_release' EXIT INT TERM HUP` removes the slot dir on every exit
   path a shell *can* trap — normal exit, and the graceful signals.
2. A **stale-PID reaper** (`loomux_pid_alive`) is the backstop for the one path
   it can't: `SIGKILL` / Windows `TerminateProcess`, which is exactly what this
   project's job-object pane teardown issues
   (`doc/design/job-object-teardown.md`). The *next* contender to try that slot
   number finds a dead holder's PID recorded in the slot's `pid` file and
   reclaims it — reaping is therefore lazy and only happens under contention,
   never as a background sweep.

A naive counter-or-trap-only scheme leaks the slot forever on a `-9`; this
reaper doesn't, and that was the property this PR's own CI went through the
most churn to actually prove (see *What the CI history says*, below).

**Reclaiming a stale slot renames it rather than delete-then-recreate.**
`mv "$cand" "$cand.stale.$$"` then `mkdir "$cand"` fresh, backgrounding the
actual `rm -rf` of the renamed-away directory since it's off the acquire
critical path. Delete-then-recreate-the-same-name can stay transiently refused
on some filesystems right after the removal; a rename never collides with its
own just-vacated name, and the rename itself is the atomic claim between
racing contenders — only one caller's `mv` of a given source can succeed, so
two reapers can't both think they won the same stale slot.

**PID liveness on Windows/Git-Bash is the sharp edge.** `$$` inside the shim is
the MSYS runtime's own pid, not necessarily what `tasklist` shows, and a
holder's death by external `TerminateProcess` (a job-object kill) bypasses
MSYS's own signal-delivery bookkeeping entirely — so trusting MSYS's notion of
"is this pid alive" is exactly the case that needs to work and is least likely
to. `loomux_pid_alive` instead maps the recorded MSYS pid to its real Windows
PID via `/proc/<pid>/winpid` (a standard MSYS pseudo-file) and asks `tasklist`
directly — the OS process table, not MSYS's. This was verified manually on the
target machine per standing policy (no `cargo test` locally): spawn a process
via Git Bash, kill it via `taskkill /F /PID <winpid>`, and confirm an unrelated
`sh` process's liveness check on the recorded pid correctly flips to "dead"
afterward. `/proc/<pid>` existence is the fallback where no winpid mapping
exists (Linux; MSYS2 also exposes a `/proc` view), then `kill -0` everywhere
else — notably macOS, which has no `/proc` at all.

## Re-entrancy: a process tree holds at most one slot per class

A guarded program can spawn *another* guarded program of the same class as a
child. This repo's own dogfood config does exactly that: `npm test`/`npm run
build` spawn `node` (via `node_modules/.bin` shims for `tsc`/`vite`, and
directly for `node --test`), and the child inherits the shimmed `PATH`
(`agent_pane_env` prepends it for the whole pane, and every descendant process
inherits its parent's environment). Without re-entrancy, the outer `npm`
holds a `node-build` slot and the inner `node` invocation then takes a
*second* slot from the same two-slot pool — effective concurrency for one
logical `npm test` run is 1, not 2, and at `max: 1` (a sanctioned machine
override — the exact case `Guardrails::resource_guard_limits` exists for) the
inner call waits on a slot its own parent already holds, self-deadlocking
every single run until `timeout_minutes` fails it open. A review of this PR
caught it: CI never exercises it (the shim only runs in agent panes, never in
GitHub Actions), so a config bug of this shape is invisible to the green
checkmark.

The fix is structural, in the shim, not a config workaround — a config fix
(splitting `npm` and `node` into separate classes, say) would only patch this
one repo's file and leave the same trap for the next repo that guards a
program which spawns another guarded program. Once a shim acquires a slot for
class `X` it exports `LOOMUX_RESGUARD_HELD_<X>=<slot path>` (the class name
folded through `tr` into a valid env-var suffix) before running the real
command; a guard shim invoked while its own class's marker is already present
passes straight through — no acquisition, audited `resource-guard-reentrant`
— instead of contending for a second slot. This is a property of the whole
subtree, not of one specific parent: any descendant, however many
guarded-program layers deep (`make` → `cc` → some guarded linker wrapper,
say), inherits the same exported marker and re-entrs for free.

**The honest caveat**: a child that explicitly clears or overrides the marker
before re-invoking a guarded program of the same class simply re-acquires
like a fresh, unrelated caller. That's accepted rather than defended against —
it degrades to ordinary queuing (or, worst case, briefly over-subscribes the
pool by one), never to a leaked slot or a widened capability, which is the
same "delay, never deny, never worse than restrict-only" posture this whole
feature already commits to everywhere else.

**The marker fold is many-to-one, so collisions are rejected at parse.**
`tr 'a-z-' 'A-Z_'` doesn't distinguish case or `-` vs `_`, so `node-build` and
`node_build` (or any case variant) fold to the identical
`LOOMUX_RESGUARD_HELD_NODE_BUILD` key. Two classes that share a folded key
would silently share one marker at runtime — a shim invocation of one class
wrongly read as re-entrant for the other, under-guarding it in a way no test
or CI run would surface (rev-8 N5). `workflow::parse_workflow` rejects this at
config time instead: alongside the existing exact-duplicate-class check, it
folds every class name through the same `tr`-equivalent
(`resource_guard_marker_key`) and reports a finding — naming both colliding
classes and the shared marker — the moment two distinct names collide. Fixed
at parse deliberately, not in the shim: a loud, reviewable finding when the
file is authored beats a shim that quietly does the wrong thing at runtime.

With re-entrancy in place, this repo's dogfood config keeps `npm`/`node` in
one combined `node-build` class rather than splitting them: the combined pool
is now safe at any `max:` including 1, and dogfooding a real, unmodified
`npm test` run is the one exercise of the re-entrancy path that isn't
synthetic — the integration suite pins the mechanism directly
(`a_nested_guarded_command_of_the_same_class_does_not_self_deadlock`), but
having the repo's own build actually walk through it on every CI run (via
`cargo test`/local dev, not GitHub Actions, which never touches the shim) is
free additional confidence.

### What the CI history actually says

The integration test asserting "a stale slot from a dead pid is reaped and
acquired" went through several revisions before it was trustworthy, and the
detour is worth recording because the failures were in the *test*, not the
mechanism. The original test held two slots, `kill -9`'d one holder, and
asserted a third caller reaped it. That failed consistently on Windows across
three mechanically different reap-hardening attempts (tasklist-based liveness,
a retried post-`rm` `mkdir`, then the `mv`-based atomic reap above) — an
unchanged symptom across unrelated fixes was the tell that the reap mechanism
wasn't the problem. Tracing pinned the real cause: Rust's `Child::kill()` on
Windows is `TerminateProcess` on the **direct child only**, and the shim
doesn't `exec` the guarded command (it has to run release logic afterward), so
the fake build ran as the shim's *own* child, one level removed from the
process Rust actually killed — killing the shim left that grandchild to exit
naturally on its own schedule, never producing the "dead pid still holding a
slot" precondition the test meant to create. A real pane hard-kill tears down
the whole process tree via a Windows job object (`doc/design/job-object-
teardown.md`), so this was a test-construction gap, not a production one. The
test now manufactures a guaranteed-dead PID directly (`mkdir` the slot, run a
trivial process to completion, record its now-defunct pid) instead of racing a
kill signal — deterministic on every platform, and it still exercises the same
`loomux_pid_alive` + rename-reap path a real crash would.

## Wait, don't error — and fail open past timeout

Slots exhausted means the shim loops rather than fails: a visible
`loomux: waiting for a '<class>' slot (n/N busy, waited Ns)...` line every
`heartbeat_secs` (30s default). That line does double duty — it's also
watchdog heartbeat output, so a worker that's legitimately queued behind a
build never trips `watchdog_stall_minutes` just for waiting its turn. Past
`timeout_minutes` (from the compiled spec), the shim **fails open**: it runs
the real command anyway and audits `resource-guard-timeout`.

This is the load-bearing design decision in the whole feature, so it's worth
stating as a rule rather than a detail: **a resource guard is a throughput
optimization, never a security boundary.** The merge gate fails *closed* — an
unsatisfiable condition refuses the merge forever, because letting unreviewed
code through would be the worse failure. A resource guard fails *open* — an
unsatisfiable wait (every slot wedged, a holder that will never release)
running the command anyway is strictly better than an orchestration run
deadlocking because a CPU throttle got in its own way. Progress beats deadlock.
The corollary is a documented, unsolved fairness gap: acquisition is a poll
loop, not a FIFO queue, so a caller can in principle be repeatedly out-raced by
later arrivals until its own timeout fires and it fails open anyway. Bounded by
`timeout_minutes`, and a fair queue is a reasonable follow-up — out of scope
here, because the backstop already keeps the failure mode "runs a bit early",
never "runs never".

`LOOMUX_TEST_TIMEOUT_SECS` / `_HEARTBEAT_SECS` / `_POLL_SECS` are test-only
seams, never set in production, so the integration suite can exercise a
multi-minute wait/timeout in real seconds.

## Degrade, not deny — the opposite failure direction from the merge gate

Every place this feature's data can be corrupted or partially unreadable —
`resource_guard_file_text`/`parse_resource_guard_file`'s round trip, an
unrecognized line or key inside the shim's own scan, a token that fails
re-sanitization — resolves the same way: skip the bad piece, keep going, guard
**less** than the repo declared. Never "block everything" (there is no
"everything" to block; this mechanism has no closed state) and never "over-
match" a corrupt entry into guarding something the repo didn't ask for. This is
the deliberate mirror image of `parse_gate_file`'s posture, which fails closed
on corruption because dropping a merge requirement would silently *widen* what
an agent may merge. A resource guard only ever restricts, so the equivalent
failure is a throughput regression, not a security one — see
`resource_guard_file_text`'s and `parse_resource_guard_file`'s doc comments,
which state the same rule at the point it's actually implemented.

The same direction governs the lifecycle: `sync_resource_guards` mirrors
`sync_merge_gate`'s create/resume call sites exactly (declared, cleared,
toggle-gated, pinned-on-resume, retained-on-a-broken-file) but inverts the one
place their safe directions differ. An empty `resources:` (or no workflow at
all) **removes** the spec file — no guards is always the safe state for a
restrict-only mechanism, so there's no "retain because dropping would widen
access" case to distinguish, unlike the merge gate.

## The honest bypass surface

Same limit the module doc already states for `gh`/`git`: **the shim raises the
floor, it is not a sandbox.** An agent with a shell can call the real program
by absolute path and skip the PATH shim entirely — the guard never even sees
that invocation. This is not a new hole this feature opens; it's the same one
every PATH-shim mechanism in this codebase has always had, restated here so
nobody reads "resource guard" as a stronger guarantee than "release gate" just
because the word "guard" is in both.

## This repo's own dogfood

`.loomux/workflow.yml` at the repo root guards its own real offenders from the
incident: `cargo build`, `cargo check`, `cargo test` (one `rust-build` class —
they all contend for the same CPU, the same reasoning `heavy-build` above
gives for `cargo build`/`cargo test`), and `npm run build`, `npm test`, and
bare `node` (a second `node-build` class, same logic on the frontend
toolchain). The third command is *every* `node` invocation, not specifically
`node --test`: the matcher only ever compares positional tokens with flags
skipped (`guard_class_for`'s doc comment), so a flag-only distinguisher like
`--test` can never appear in a matched prefix — the only expressible guard for
"the CPU cost `node --test` represents" is bare `node` with no args, an honest
schema limit rather than an oversight. That breadth is also exactly why this
class needs the re-entrancy mechanism above: `npm test`/`npm run build`
*themselves* spawn `node` as a child. Two classes, not one shared with Rust,
because the Rust and Node toolchains don't actually compete for the same
cache/incremental-build state the way `cargo build` and `cargo test` do with
each other — there's no reason to serialize a `cargo check` behind an
`npm test`.
