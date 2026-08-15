// The ONE `ModelCatalog` the app shares (#935).
//
// `modelcatalog.ts` takes its probe as an INJECTED function precisely so it stays
// importable by `node --test` (nothing on its import graph may reach
// `@tauri-apps`), which means the module cannot also own the instance wired to
// the real backend call. This file is that wiring, and nothing else.
//
// It is a module-level constant rather than a field on each surface because the
// memo is the point: `ModelCatalog.probe` collapses repeated asks into one call
// per program, and a per-form instance would restart that memo every time a form
// opens — including when the launcher pane BECOMES a workflow pane, which is the
// exact handover this slice adds a second consumer for.
//
// **What promoting a memo to app scope costs, and why it is safe to pay here.**
// A per-form memo forgets everything when the form closes, so a stale answer had
// a natural expiry: open a new pane and the machine is asked again. At app scope
// there is no such moment, and an answer kept here is kept until loomux exits. So
// the memo keeps only an answer worth keeping (`worthKeeping`), mirroring the
// backend's own rule — `probe_agent_cli` caches complete probes and deliberately
// does NOT cache failures or partial answers, "a CLI installed while loomux is
// running must become launchable on the next probe". A front-memo that kept those
// would not duplicate that cache, it would make it unreachable: install gemini
// mid-session and every surface would go on reporting it missing until a restart.
// Freshness is bounded from the other side by the callers, which ask per surface
// rather than per paint.

// The list-models detector (#993) is wired in here too, and its second half —
// reading the bytes the backend hands back — is `modelwire.ts`'s. The seam is
// split that way on purpose: `readCliModelReply` correlates the reply against
// the id the backend says it sent, so neither side has to spell that id twice,
// and the whole reader stays inside `node --test`'s reach.
//
// Since #1020 that detector arrives on TWO routes, and this file wires both to
// the same reader so they cannot disagree:
//
//   pull  `listCliModels` — a lookup against what the backend's startup sweep
//         already found. Fired by a picker opening. Cannot spawn anything.
//   push  `onModelsDetected` — the sweep's own result, as it lands, for the
//         forms that were already open when it did.
//
// Neither is redundant: the pull covers a form that opens after the event
// fired (or a webview that registered its listener too late to catch it), and
// the push covers a form that opened before the answer existed. `acceptReport`
// is what makes the second one safe to apply late.

import { listCliModels, onModelsDetected, probeAgentCli } from "./pty";
import { ModelCatalog } from "./modelcatalog";
import { readCliModelReply } from "./modelwire";

export const modelCatalog = new ModelCatalog(probeAgentCli, (program) =>
  listCliModels(program).then(readCliModelReply)
);

/** Start taking the startup sweep's pushed results (#1020). Called once, from
 *  `main.ts`'s boot — as early as possible, because the sweep is already
 *  running by the time the webview exists and an event that arrives before this
 *  listener does is one a picker has to pull for instead.
 *
 *  Never rejects onward: a subscription loomux could not register costs the
 *  push route, and the pull route still answers. */
export function startModelDetection(): void {
  void onModelsDetected(({ program, reply }) => {
    modelCatalog.acceptReport(program, readCliModelReply(reply));
  }).catch(() => {
    /* the pull route (`ModelCatalog.detect`) remains */
  });
}
