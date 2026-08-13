// The workflow pane's INTERACTION STRUCTURE (#880 slice B), driven through the real app.
//
// Regression class: a gesture that changes state the human cannot see. Before this, the
// property form lived behind a "Blocks" tab and the canvas behind a "Graph" tab, so clicking a
// block on the canvas selected it and re-rendered its editor OFF SCREEN — from the human's side
// the click did nothing at all. Every unit test in the repo passed throughout: the model was
// right, the selection was right, and the only thing wrong was which of three stacked panes was
// on top. That is precisely the class a DOM-free test cannot reach and this harness can, so it
// is the one E2E assertion this slice owes.
//
// It spawns no agent CLI: a workflow pane is a content pane over a YAML file (CLAUDE.md
// constraint 3 is satisfied by construction, not by care). The workflow it opens is written to a
// fresh temp dir per test, so the pane's own layout write (`.loomux/workflow.layout.json`, which
// a node click produces) lands there and never in the checkout.
import { test, expect } from "../fixtures";
import { createWorkflowPane, paneByName } from "../helpers";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

/** A small, valid, COMMENTED workflow — comments included because the pane's whole contract is
 *  that it preserves them, and a fixture without any could not notice if it stopped. */
const WORKFLOW = `# The roster this repo's runs may use.
version: 1
name: e2e demo workflow
blocks:
  - id: planner
    name: Planner
    kind: planner
    cli: claude
  - id: worker-deep
    name: Deep worker
    kind: worker
    cli: claude
  - id: rev-lead
    name: Review lead
    kind: reviewer
    cli: copilot
edges:
  - from: planner
    to: worker-deep
  - from: worker-deep
    to: rev-lead
gates:
  merge:
    require: all-pass
    reviewers: [rev-lead]
`;

/** The same workflow with the LAST block gone — and, with it, the edge and the merge gate that
 *  referenced it (removing a block without its references would be a different test: a file
 *  full of dangling-reference findings). Written over the fixture to simulate the thing this
 *  pane's conflict machinery exists for: something else editing the workflow under the pane. */
const WORKFLOW_WITHOUT_REV_LEAD = `# The roster this repo's runs may use.
version: 1
name: e2e demo workflow
blocks:
  - id: planner
    name: Planner
    kind: planner
    cli: claude
  - id: worker-deep
    name: Deep worker
    kind: worker
    cli: claude
edges:
  - from: planner
    to: worker-deep
`;

/** A repo-shaped temp dir holding exactly one workflow file. */
function makeRepo(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "loomux-e2e-wf-"));
  fs.mkdirSync(path.join(root, ".loomux"), { recursive: true });
  fs.writeFileSync(path.join(root, ".loomux", "workflow.yml"), WORKFLOW, "utf8");
  return root;
}

test("clicking a block on the canvas shows that block's editor, docked beside it", async ({
  appPage: page,
}, testInfo) => {
  const repo = makeRepo();
  try {
    await createWorkflowPane(page, { name: "Workflow", repo });
    const pane = paneByName(page, "Workflow");

    const body = pane.locator(".wf-body");
    const canvas = pane.locator(".wf-graph");
    const yaml = pane.locator(".wf-yaml");
    const inspector = pane.locator(".wf-inspector");

    // The pane opens on its BODY surface (the file is there and readable), with the canvas as
    // the primary surface and the inspector docked beside it. The raw YAML is the toggle, so it
    // starts off.
    await expect(body).toBeVisible({ timeout: 15_000 });
    await expect(canvas).toBeVisible();
    await expect(inspector).toBeVisible();
    await expect(yaml).toBeHidden();

    // Nothing selected yet = the workflow's own settings. Stated so the assertion after the
    // click is a CHANGE and not something that was already true — the trap the git-overlay spec
    // records for its own first row.
    const title = inspector.locator(".wf-insp-title");
    const sub = inspector.locator(".wf-insp-sub");
    await expect(title).toHaveText("Workflow settings");
    await expect(sub).not.toContainText("worker-deep");

    // Docked, not squeezed to nothing: the whole point is that the editor occupies real space
    // beside the canvas rather than replacing it.
    const canvasBox = await canvas.boundingBox();
    const inspBox = await inspector.boundingBox();
    expect(canvasBox).not.toBeNull();
    expect(inspBox).not.toBeNull();
    expect(inspBox!.width).toBeGreaterThan(120);
    expect(canvasBox!.width).toBeGreaterThan(120);
    // Beside, not on top: the canvas ends where the inspector begins (±1px for borders).
    expect(inspBox!.x).toBeGreaterThanOrEqual(canvasBox!.x + canvasBox!.width - 1);

    // THE GESTURE. Pick the node by the label it draws rather than by render order — an
    // order-indexed click would still pass if the canvas silently drew a different block.
    //
    // The press lands at an explicit point inside the node's RECT rather than its centre, and
    // both halves of that are deliberate (the node box is 168×52 — `NODE_W`/`NODE_H` in
    // workflowlayout.ts):
    //   * the rect, because the two `<text>` labels paint OVER it and Playwright refuses a
    //     click whose target would be intercepted — the centre is in the gap between the two
    //     lines, but only just, and a longer block name would close that gap;
    //   * (140, 44), because the out-port sits at the right edge's midpoint (168, 26) and a
    //     press within 12px of it (`PORT_HIT`) means "draw an edge", not "select this node".
    //     From here that distance is ~33px, which is not a margin a font change can eat.
    const nodes = pane.locator(".wf-graph-svg .wf-node-g");
    await expect(nodes).toHaveCount(3);
    await nodes
      .filter({ hasText: "Deep worker" })
      .locator("rect.wf-node")
      .click({ position: { x: 140, y: 44 } });

    // …and the editor for the thing just clicked is THERE, named by the id that edges and the
    // merge gate reference. This is the assertion that was false before #880 — the selection
    // happened, and the editor showing it was behind a tab.
    await expect(title).toHaveText("Deep worker");
    await expect(sub).toContainText("worker-deep");
    // Not just a header: the block's own editor is rendered, with its immutable id in the field.
    const idField = inspector
      .locator(".wf-field")
      .filter({ has: page.locator(".wf-label", { hasText: /^Id$/ }) })
      .locator("input");
    await expect(idField).toHaveValue("worker-deep");
    // The canvas did not go anywhere — that is what "docked" means.
    await expect(canvas).toBeVisible();
    // …and the roster agrees with the inspector rather than lighting a stale row.
    await expect(pane.locator(".wf-row.active .wf-row-main")).toHaveText("Deep worker");
    // The inspector names the selection ONCE. Each form used to open with its own <h3> saying
    // the same words the docked header says one line above it (review finding 1 — visible in
    // this spec's own first screenshot before the fix: "Deep worker" at 13px, then "Deep
    // worker" again at 14px, 8px below). The header won; the four <h3>s went.
    await expect(inspector.getByText("Deep worker", { exact: true })).toHaveCount(1);

    await testInfo.attach("workflow-pane-canvas-and-inspector.png", {
      body: await pane.screenshot(),
      contentType: "image/png",
    });

    // The raw YAML is still first-class, as a TOGGLE over the canvas — and the inspector stays
    // docked through it, because it is beside both surfaces rather than a peer of either.
    await pane.locator('.wf-btn:text-is("YAML")').click();
    await expect(yaml).toBeVisible();
    await expect(canvas).toBeHidden();
    await expect(inspector).toBeVisible();
    await expect(sub).toContainText("worker-deep");
    await expect(pane.locator(".wf-yaml-area")).toHaveValue(/name: e2e demo workflow/);

    await testInfo.attach("workflow-pane-yaml-toggle.png", {
      body: await pane.screenshot(),
      contentType: "image/png",
    });

    // Toggling back returns the canvas, with the same selection still showing.
    await pane.locator('.wf-btn:text-is("YAML")').click();
    await expect(canvas).toBeVisible();
    await expect(yaml).toBeHidden();
    await expect(sub).toContainText("worker-deep");

    // Nothing was saved: the pane edits a buffer, and this test only ever selected things.
    expect(fs.readFileSync(path.join(repo, ".loomux", "workflow.yml"), "utf8")).toBe(WORKFLOW);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
});

test("a selection whose block disappears leaves the roster and the inspector agreeing", async ({
  appPage: page,
}) => {
  // Review finding 2, as the exact scenario it named. The inspector is the render that
  // NORMALIZES the selection (`inspectorTarget` falls back when the selected block is gone);
  // the roster only highlights whatever `this.selection` says. Render the roster first and a
  // stale selection lights NOTHING, while the inspector quietly shows the workflow's own
  // settings — two surfaces disagreeing about what is selected, until something else happens to
  // re-render the roster.
  //
  // The trigger is real rather than contrived: an agent rewriting the workflow it is running
  // under is the scenario this pane's conflict machinery exists for, and Reload is how you take
  // its version.
  const repo = makeRepo();
  try {
    await createWorkflowPane(page, { name: "Reload", repo });
    const pane = paneByName(page, "Reload");
    const inspector = pane.locator(".wf-inspector");

    await expect(pane.locator(".wf-body")).toBeVisible({ timeout: 15_000 });
    // Select the LAST block — the index that goes out of range when it is removed.
    await pane
      .locator(".wf-row")
      .filter({ has: page.locator(".wf-row-main", { hasText: /^Review lead$/ }) })
      .click();
    await expect(inspector.locator(".wf-insp-title")).toHaveText("Review lead");

    // Something else rewrites the file without that block, and the human takes its version.
    // Spelled out rather than derived from WORKFLOW by regex: this file's own line endings
    // depend on how git checked it out, and a `\n`-anchored substitution that silently matched
    // nothing under CRLF would leave the file untouched and fail this test for the wrong reason.
    fs.writeFileSync(path.join(repo, ".loomux", "workflow.yml"), WORKFLOW_WITHOUT_REV_LEAD, "utf8");
    await pane.locator('.wf-btn:text-is("Reload")').click();

    // Exactly one row is lit, and it is the one the inspector is showing. Before the fix this
    // was ZERO lit rows beside an inspector reading "Workflow settings".
    await expect(inspector.locator(".wf-insp-title")).toHaveText("Workflow settings");
    await expect(pane.locator(".wf-row.active")).toHaveCount(1);
    await expect(pane.locator(".wf-row.active .wf-row-main")).toHaveText("e2e demo workflow");
  } finally {
    fs.rmSync(repo, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
});

test("the roster is still a way in — clicking a row shows the same editor", async ({
  appPage: page,
}) => {
  // The roster is the pane's keyboard/accessibility path to every selection, so #880 keeps it
  // as the left column rather than folding it into the canvas. It has to reach the same
  // inspector the canvas does, or it is a second, quieter way to make a dead click.
  const repo = makeRepo();
  try {
    await createWorkflowPane(page, { name: "Roster", repo });
    const pane = paneByName(page, "Roster");
    const inspector = pane.locator(".wf-inspector");

    const row = (name: RegExp) =>
      pane.locator(".wf-row").filter({ has: page.locator(".wf-row-main", { hasText: name }) });
    const idField = inspector
      .locator(".wf-field")
      .filter({ has: page.locator(".wf-label", { hasText: /^Id$/ }) });

    await expect(pane.locator(".wf-body")).toBeVisible({ timeout: 15_000 });
    await row(/^Review lead$/).click();

    await expect(inspector.locator(".wf-insp-title")).toHaveText("Review lead");
    await expect(inspector.locator(".wf-insp-sub")).toContainText("rev-lead");
    await expect(idField).toHaveCount(1);

    // The gate is a selection in the same list, and it is not a block — the inspector has to
    // switch EDITORS, not just re-title itself, so the block's fields have to be gone.
    await row(/^Merge$/).click();
    await expect(inspector.locator(".wf-insp-title")).toHaveText("Merge gate");
    await expect(idField).toHaveCount(0);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
});
