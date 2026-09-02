# The repo field's recents dropdown (#2010)

The launcher's new-pane **Repository** field offers the most recent directories.
This note records the shape of the fix and the one contract it widens; the
persuasive history is on the issue.

## The defect

The field was a plain `<input>` plus a `<datalist>` of recents. Browsers filter
a datalist's suggestions by the input's current text, and the launcher pre-fills
the input (`defaultFolder` — the split-from pane's cwd, or the newest recent),
so the dropdown showed only the entries that happened to start with that
prefix — in practice, one entry or none. The recents were recorded and fed in
correctly; the control could not display them. This is exactly the defect the
model field escaped, and `src/modelpicker.ts` documents why a datalist does not
work there (its header, lines 9-12).

## The fix: one dropdown mechanism, reused

The repo field is now the `ModelPicker` control — the same class the model
pickers use, not a second dropdown implementation. The module header's rule is
why: "the second copy of a control is the second place a fix has to be
remembered." The picker lists every recent directory regardless of the input's
text, newest first, and marks the current value by selecting its option; its
`custom…` escape keeps the field wider than the list, because an unknown path is
the normal case for a path field — free text and `Browse…` behave exactly as
they did.

Two consequences of the reuse, both deliberate:

- When the current value IS a recent, the picker's dropdown branch hides the
  free-text input (the value shows in the dropdown, as it does for a model id).
  Editing it freely costs one click on `custom…`, the same trade the model
  picker makes in both of its hosts. Typing into a field whose value is not a
  recent — the common case, since the pre-fill from `defaultFolder` (#214)
  usually is not — needs no click at all: the picker opens on its custom branch.
- The pane's initial-focus marker (`data-initial-focus`, rev-74 LOW-4/LOW-6)
  follows the VISIBLE half of the picker: the free-text input when it is
  showing, the recents select when the dropdown branch hides it, because
  `focus()` on a hidden element lands nowhere.

## What was widened on ModelPicker

Additive, and forced by the repo host's needs; the workflow pane's usage is
unchanged:

- `get select()` / `get input()` — the two elements, so a host can compose the
  picker into its own field layout (the repo row's per-kind placeholder and the
  initial-focus marker above). Read-mostly: the picker owns the structure.
- `set value` — the Browse… pick, whose dialog result is not a keystroke. It
  re-runs `pickerSelection` (so an unknown path opens the custom branch) and
  fires nothing, matching how a programmatic write has always behaved; the
  caller does its own follow-up work.
- `focus()` — focuses whichever half is showing, for the validation-error paths
  that bounce the human back to this field.

## Recording and persistence

- Recorded on every launch (as before: terminal cwd, content/git root,
  orchestrator/agent repo) **and now on every Browse… pick** — the pick IS the
  gesture, the same argument the #1042 `admitRoot` call beside it makes.
- Not recorded from elsewhere, on purpose: the side dock re-roots to the active
  pane's cwd and opens no directory itself, and the terminal pane's "Change
  folder" chip is a shell `cd`, not a launch — neither is a directory the human
  asked the launcher to work in.
- The ordering/dedup/cap decision lives in `src/recentdirs.ts`
  (`mergeRecentDir`, node-tested in `test/recentdirs.test.ts`): trimmed, then
  deduplicated to the front, capped at `MAX_RECENT_REPOS` (8 — the cap
  `addRecentRepo` has always enforced; the issue's "say 10" was an estimate).
  Storage stays in localStorage under the existing `loomux.recentRepos` key
  (`src/agents.ts`), read fresh on every write.
- A read that fails outright (`localStorage.getItem` throwing) **declines the
  write**: a list the store could not show us must not be overwritten with one
  built from nothing. A blob that parses to garbage is different — the data is
  gone, nothing to preserve — and rebuilds from empty, as before. (Single
  tenant, so the multi-tenant BoardPrefsStore rule does not apply; the
  declined-write half of it does.)
