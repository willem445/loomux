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
loomux: publishing a release/tag (v1.0.0) requires an explicit human grant …
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

The self-check is the durable half. The PATH repair fixes the failure we know
about; the self-check means the *next* environment that strips a tool loudly
refuses instead of quietly ungating.

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
| `"%LOOMUX_SH%" "%~dp0git" %*` (shipped) | `'merge)$' is not recognized` |
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

Against the alternative — leaving `gh pr create` broken — this is the safer
trade. A shim that breaks the sanctioned path pushes agents onto raw `gh api`,
which is the precise route #196 closed. And it sits well inside the surface
workflows.md already concedes under "The bypass surface, honestly": an agent
that wants to evade has the cheaper route of calling the real binary by absolute
path.

## What is still not fixed

An argument an **agent** types with an unquoted shell metacharacter is still
mangled by the same `cmd.exe /c` layer when it runs `gh`/`git` from a
PowerShell/cmd pane. That fails loudly and visibly, and it is not a gate hole:
a mangled command line cannot turn a refusal into an allow. Closing it would
mean replacing the `.cmd` delegators with a native executable — a packaging
change (a second build target, bundled and located at runtime), out of scope
here and worth its own issue if the mangling is ever hit in practice.
