// Classification of a resume failure's structured tag (#412). The backend's
// `resolve_resume_cwd`/`resolve_worker_resume_cwd` (orchestration/mod.rs) and
// `resolve_session_ref` always prefix a resume-time error with one of these
// tags — this is the frontend half of that contract: pure, DOM-free, so the
// mapping from "raw error string" to "what the UI should offer" is
// unit-tested (test/resumeerror.test.ts) rather than eyeballed in main.ts.

/** The four resume-failure shapes the backend can report. `null` when the
 *  message carries no recognized tag at all (an unrelated error — e.g. "group
 *  already has a live orchestrator"). */
export type ResumeFailureKind =
  | "not-found"
  | "workspace-missing"
  | "ambiguous"
  | "store-unreadable"
  | null;

const TAG_KIND: Record<string, ResumeFailureKind> = {
  "resume-not-found": "not-found",
  "resume-workspace-missing": "workspace-missing",
  "resume-ambiguous": "ambiguous",
  "resume-store-unreadable": "store-unreadable",
};

/** Extract the leading `resume-<tag>:` prefix from a thrown error's message,
 *  if it has one. Backend errors carry the tag at the very start of the
 *  string (see `resolve_resume_cwd` et al.); anything else — a plain string,
 *  or a tag this frontend doesn't recognize yet — is `null`. */
export function resumeFailureKind(message: string): ResumeFailureKind {
  const m = /^(resume-[a-z-]+):/.exec(message.trim());
  if (!m) return null;
  return TAG_KIND[m[1]] ?? null;
}

/** Whether this failure kind is one a "start fresh instead" affordance can
 *  actually fix: the session is provably unresolvable (never existed, or its
 *  workspace is gone), as opposed to "ambiguous" (needs a longer id/prefix,
 *  which a fresh spawn doesn't address) or "store-unreadable" (a real I/O
 *  problem retrying under a new session id wouldn't fix either). */
export function offersStartFresh(kind: ResumeFailureKind): boolean {
  return kind === "not-found" || kind === "workspace-missing";
}

/** A short, human-readable reason for the two `offersStartFresh` kinds, for
 *  the confirm dialog's body. Falls back to a generic phrasing for any other
 *  kind (never called for `null`/other kinds by the caller, but total rather
 *  than partial so a future kind added here doesn't need a matching frontend
 *  edit to stay safe). */
export function resumeFailureReason(kind: ResumeFailureKind): string {
  switch (kind) {
    case "workspace-missing":
      return "Its recorded workspace no longer exists on disk — the worktree may have been removed.";
    case "not-found":
      return "It was not found in the session history on this machine — it may have been cleared.";
    default:
      return "It could not be resumed.";
  }
}
