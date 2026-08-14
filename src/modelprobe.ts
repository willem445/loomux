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
// exact handover this slice adds a second consumer for. The backend caches too;
// this keeps the IPC and its worst-case timeout off the second surface as well.

import { probeAgentCli } from "./pty";
import { ModelCatalog } from "./modelcatalog";

export const modelCatalog = new ModelCatalog(probeAgentCli);
