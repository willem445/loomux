PR #{{PR}} is back with you at head {{HEAD}} (base {{BASE}}).

{{WHAT}}

Address it, then call report(outcome=done, ref=#{{PR}}) — that report is what advances this drive and nothing else does. If your fix moved the head, push first and report once the checks are green. If it did not move the head — a PR-body or comment edit, or a finding you are answering rather than changing code for — report done anyway and say so in the note; the driver reads an unchanged head plus your report(done) as a body-only fix and sends the PR back for re-review. A report(progress) advances nothing. This is attempt {{ATTEMPT}} of {{MAX_ATTEMPTS}}.
