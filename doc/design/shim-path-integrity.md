# Shim path integrity (#509)

The `gh`/`git` interceptor shims (#83) enforce loomux's merge and release gates.
This note records what broke about them when they were invoked from a
PowerShell/cmd pane, how far each failure reached, and the two separate fixes —
because the two symptoms reported in #509 look like one bug and are not.

## The shared precondition

A `.cmd` delegator (`gh.cmd`) launches `sh.exe` by an **absolute** path baked in
at shim-write time (#335), precisely so the gate does not depend on the invoking
shell's PATH containing `sh`. But the launched `sh` still **inherits the
caller's PATH** — and a PowerShell/cmd pane's PATH carries neither

* Git for Windows' `usr\bin` (where `tr`, `head`, `tail`, `date`, `cat`, `rm`,
  `mv` live), nor
* a native `git.exe` ahead of loomux's own `git.cmd`.

Under Git Bash both happen to be present, which is why every existing shim test
passed and the hole stayed invisible until a worker hit it from PowerShell.

## Failure 1 — a silently fail-OPEN gate (the headline)

The shim normalizes case with `tr` before matching. With `tr` missing, the
command substitution `x=$(printf '%s' "$v" | tr …)` writes
`tr: command not found` to stderr and sets **`x` to the empty string** — and
carries on. Empty is not a safe default here. Audited line by line:

| shim line | what it normalizes | effect when `tr` is missing |
| --- | --- | --- |
| `a_method=$(… \| tr '[:lower:]' '[:upper:]')` (×3, `-X`/`--method`) | the HTTP method | **FAIL-OPEN** — empty method falls back to `GET`, so `is_write=0` and a `-X DELETE` never reaches the write arms |
| `path_low=$(… \| tr '[:upper:]' '[:lower:]')` | the URL path | **FAIL-OPEN** — matches none of `graphql`, `git/refs`, `git/tags`, `releases`; the whole `gh api` release arm is dead |
| `ref_low=$(…)` | the parsed `ref` field | fail-open in the same arm (heads/tags classification) |
| `ql=$(…)`, `rest=$(… \| tr -d ' "')` | the graphql query, the grant tag | unreachable once `path_low` is empty |
| `low=$(printf '%s' "$*" \| tr …)` (merge arm) | the whole argv | **FAIL-OPEN** — `*mergepullrequest*` never matches, so a graphql merge is not gated |
| `_safe=$(… \| tr -c 'A-Za-z0-9._-' '_')` | the release-grant filename | fail-CLOSED — empty ⇒ no grant path ⇒ blocked |
| `cur_head=$(… \| tr …)` | the PR head oid (workflow gate) | fail-CLOSED — empty ⇒ `unresolved-head` ⇒ blocked |

So the two `tr` uses that key *grant lookup* failed closed (a legitimate grant is
refused — annoying, safe), and every `tr` use that keys *gate matching* failed
**open**. Measured against the shipped shim with a stripped PATH, with a fake gh
standing in for the real one:

```
$ sh ./gh api -X DELETE repos/o/r/git/refs/tags/v1.0.0        # normal PATH
orrerix: publishing a release/tag (v1.0.0) requires an explicit human grant …
rc=1
$ env -i PATH=/c/Windows/system32 sh.exe ./gh api -X DELETE repos/o/r/git/refs/tags/v1.0.0
./gh: line 177: tr: command not found
./gh: line 228: tr: command not found
FAKE-GH-RAN
rc=0
```

`gh pr merge` and the REST `pulls…/merge` shape stayed gated (neither needs
`tr` before its decision), which is why the hole was not obvious: the ergonomic
paths still refused, and only the raw-`gh api` route — the one the shim exists to
intercept (#196) — fell open.

### Fix

Two parts, both in `shim_deps_preamble`, emitted **byte-identically** into both
shims:

1. **Repair PATH.** Prepend the coreutils directory resolved at shim-write time
   from this machine's own `sh` install layout (`winpath::resolve_utils_dir`,
   probing for `tr` the way `resolve_sh` probes for `sh`). Derived, never
   hardcoded — CLAUDE.md constraint 8, the rule #263 was reverted for. The entry
   is written in MSYS form (`/c/Program Files/Git/usr/bin`): a drive-letter
   entry in an MSYS `$PATH` reads as a *relative* directory named `C:` and
   resolves nothing, which was confirmed by measurement before the form was
   chosen.
2. **Prove it worked.** Before one line of gate logic runs, assert every
   dependency resolves (`command -v`, a builtin, so the check cannot itself be
   defeated by the PATH problem it tests for). A missing one refuses the command
   with an actionable message and a `gate-degraded-missing-dep` audit line.
   Fail CLOSED and loud — the pre-#509 behavior was fail-open and silent, which
   is the one thing a gate must never be.

3. **Make it impossible, not merely improbable** (`loomux_norm_guard`, rev-21 N2).
   The two above reduce the *probability* of a failed normalizer; neither can
   eliminate it, because `command -v` proves a tool **resolves**, not that it
   **runs**. A `tr` that resolves and then dies — corrupt install, arch
   mismatch, fork failure — reproduces #509 exactly, and the startup probe
   reports everything fine. So the gate never consumes a normalization at all:
   a normalizer that returns EMPTY for a NON-empty input has failed by
   construction, and the shim refuses (`gate-degraded-normalize-failed`) rather
   than matching gate patterns against an empty string.

   That is also *why* the resolution-only gap is closed here rather than by a
   functional canary at startup: the canary would spend a subprocess on every
   gated `gh` and `git` invocation, and the `git` shim sits on a hot path. The
   guard costs nothing per call, and it covers strictly more — a tool that
   breaks *between* the startup check and the call site is still caught.

The invariant is the durable half. The PATH repair fixes the environment we know
about, the self-check names a missing tool loudly, and the guard means no
failure of any of these tools — missing or broken, now or later — can put an
empty string in front of a gate pattern again.

### Guard completeness — what each pin proves, and what it cannot (#564)

The guard above is only worth its claim if *every* normalizer has one, so two
different pins ask that question, and they fail in different directions. Stating
the limits here is the point: the pin's original "cannot rot into a blanket
permission" wording had to be corrected in review for overclaiming (#552), and an
artifact that asserts a correctness it does not have is worse than one that
asserts nothing.

**The text scan** (`every_shim_normalizer_is_guarded_or_explicitly_exempted`)
enumerates every `VAR=$(… | tr …)` in both generated shims and requires each to be
guarded, or listed in `UNGUARDED_TR_SITES` with a reason. It asserts the site
count exactly, requires each exemption to match exactly one site, and pins each
exempted line verbatim. What it proves is "**every site I can see is guarded**" —
never "every site is one I can see". Three residuals, from #564:

| | |
| --- | --- |
| **O1** | A normalizer written in a shape the scan does not match — `sed`, backticks, an assignment split across two lines — is invisible **and** leaves the exact count correct, so both halves pass. This is the residual risk of pinning a text pattern rather than a behaviour, and no amount of pattern-widening removes it. |
| **O2** | *Closed.* An exempted line rewritten IN PLACE, keeping its variable and its context, still matched exactly one site while the exemption's stated reason quietly stopped being true of it. Each exempted line is now pinned verbatim, so any edit reddens and the reason has to be re-decided. |
| **O3** | *Closed.* `… \|\| true` squashes to `\|\|true`, which contains `\|tr`, so the scan read a shell OR as a `tr` pipeline. Fail-safe (it over-counted), and it never reached the site tally because those lines carry no `=$(` — fixed anyway, because a scan with known false positives is one somebody later loosens to make quiet, and the loosening is where the risk enters. |

**The behavioural sweep**
(`no_single_broken_text_tool_can_let_a_gated_command_through`) is the answer to
O1, which is the only one a text scan cannot close from the inside. It breaks
**one text tool at a time** — resolvable, so `command -v` is satisfied, and
silently producing nothing, which is what a corrupt install or a fork failure
looks like at the call site — and requires every gated shape to still be refused
and never to reach the real binary. Shape is irrelevant to it: a normalizer added
in any syntax, consuming any of the listed tools in front of a gate decision
without a guard, reddens it.

Its own limit, equally plainly: it enumerates **command shapes** and **tools**. A
normalizer on a code path no listed shape reaches, or built from a tool not in
`CANDIDATE_NORMALIZER_TOOLS` (which deliberately includes tools nothing uses
today), is outside it. It is a **different** net from the text scan — neither one
contains the other, and neither is complete; both are kept because they are blind
to different things.

The counter-examples run both ways, and are worth stating so nobody later reads
one pin as subsuming the other and deletes it (#564 rev-1 B1):

* **The scan catches what the sweep cannot see.** An unguarded site added inside
  a code path no swept shape reaches reddens the scan and nothing else. The
  workflow-verdict gate used to be exactly that region — the sweep now enters it
  (a `merge_gate` fixture and a `gh pr merge` shape), which shrinks the region
  but does not remove it: `also: ci-green`, the malformed-gate arms and the
  `--input`-body parse are all still unswept.
* **The sweep catches what the scan cannot see.** A normalizer in an unrecognised
  shape — `sed`, backticks, split across lines — is invisible to the scan *and*
  leaves its exact count correct. That is O1, and it is observed, not argued:
  see the evidence runs cited in PR #612.

## Failure 2 — `gh pr create` dies (a different cause)

#509 assumed the same PATH cause. It is not. `gh pr create|status|…` shells out
to git with `git config --get-regexp ^branch\.<b>\.(remote|merge)$`. With the
shim dir first on PATH that resolves to loomux's `git.cmd`, and Windows can only
run a `.cmd` through a `cmd.exe /c` layer — which **re-parses the command line
after expanding it**, so the unquoted `|` splits it and gh reports
`failed to run git: 'merge' is not recognized`.

No batch quoting fixes this. Measured against that exact argument:

| delegator form | result |
| --- | --- |
| `"%ORRERIX_SH%" "%~dp0git" %*` (shipped) | `'merge)$' is not recognized` |
| `set "ARGS=%*"` + `setlocal EnableDelayedExpansion` + `!ARGS!` | `'merge)$' is not recognized` |
| per-argument `%~1` re-quoting loop | `'merge)$' is not recognized` |
| native `git.exe`, same argument | `arg3=[^branch\.(remote\|merge)$]` — intact |

The split happens in the `cmd.exe` the *caller* spawns, before the batch file's
first line executes; nothing inside the `.cmd` can prevent it. Only being a
native `.exe` avoids it.

### Fix

For the gh **built-in** subcommands that shell out to git, the POSIX gh shim
prepends the real git's directory to the PATH it hands the real gh, so gh's
internal git calls reach `git.exe` and arrive intact.

**What that costs, stated plainly.** For those subcommands the real gh's own git
calls are not tag-gated. The `.cmd` gate for everything the *agent* types is
untouched; what changes is the git that gh itself runs. gh has no command that
pushes a tag (`gh release create` creates the tag through the API — and is gated
by this same shim), so the residual is "a program gh runs on the agent's
behalf". That is exactly why the adjustment is restricted to a list of gh
built-ins: gh refuses to let a user alias or an extension shadow a built-in, so
`gh <alias>` (a `!`-alias runs a shell) and `gh extension …` keep the gated git.
A gh version that grows a new git-using command degrades to the old
broken-argument behavior rather than opening anything.

**The residual is slightly wider than "built-ins don't run agent code" suggests,
and should be read as it is** (rev-21 N3). A built-in *can* reach agent-authored
code: `gh pr create` / `gh issue create` in a TTY — which agent panes have —
spawn an editor resolved from `GH_EDITOR`, `git config core.editor` or `EDITOR`,
and `core.editor` is repo-local, agent-writable and persistent. That child
inherits gh's PATH, i.e. the ungated git. This grants no *new* capability — the
cheaper bypass of calling the real binary by absolute path is already conceded
below — so it does not change the trade and is not a reason to widen or narrow
the built-in list. It is a reason to state the boundary as "built-ins, which can
still spawn an editor the agent named" rather than as "nothing here runs agent
code", which would be false.

Against the alternative — leaving `gh pr create` broken — this is the safer
trade. A shim that breaks the sanctioned path pushes agents onto raw `gh api`,
which is the precise route #196 closed. And it sits well inside the surface
workflows.md already concedes under "The bypass surface, honestly": an agent
that wants to evade has the cheaper route of calling the real binary by absolute
path.

## What is still not fixed

**O1 is narrowed, not eliminated** (see *Guard completeness* above). Nothing
enumerates normalizer sites from behaviour alone: the text scan is blind to
shapes, the behavioural sweep is blind to command paths and tools it does not
list, and a new normalizer that both scans miss would be unguarded. Closing it
outright means the shims stopping being generated shell — the gate logic in a
native binary, where a normalizer is a function call rather than a line of text
— which is a packaging change, not a test change.

An argument an **agent** types with an unquoted shell metacharacter is still
mangled by the same `cmd.exe /c` layer when it runs `gh`/`git` from a
PowerShell/cmd pane. That fails loudly and visibly, and it is not a gate hole:
a mangled command line cannot turn a refusal into an allow. Closing it would
mean replacing the `.cmd` delegators with a native executable — a packaging
change (a second build target, bundled and located at runtime), out of scope
here and worth its own issue if the mangling is ever hit in practice.
