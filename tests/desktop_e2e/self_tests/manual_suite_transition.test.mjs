import assert from "node:assert/strict";
import test from "node:test";
import { manualSuiteWorkspaceFailures, transitionManualSuiteWorkspace, restoreManualSuiteSession, manualSuiteRestoredSessionFailures } from "../drivers/manual_suite_transition.mjs";

const oldPath = "C:\\manual-suite\\case-one", newPath = "C:\\manual-suite\\case-two";
const root = "C:\\manual-suite";
const runtime = { desktop_owner_path: `${root}\\desktop-owner.json`, desktop_owner: { process_id: 90,
  process_start_time_utc_ticks: "1000", executable_path: "C:\\bin\\moyai-desktop.exe" } };
function surface(workspace = oldPath, { fresh = false } = {}) {
  const sessionId = fresh ? null : "01M4JZ789M94P1YM0A3GWH9EDD";
  return { projection: {
    workspace_path: workspace, overlay: "none", busy: false, navigation_loading: false, navigation_admission_open: true,
    background_mutation_pending: false, async_polling_required: false, post_run_refresh_pending: false, provider_loading: false,
    pending_async_operations: [], confirmation_visible: false, confirmation: null, stop_target: null,
    task_activity_state: "idle", agent_tree_active: false, draft_prompt: "", image_input: "", attached_images: [], pending_turn_inputs: [],
    draft_target: { workspacePath: workspace, sessionId }, run_target: { workspacePath: workspace, sessionId },
    thread_empty: fresh, transcript_rows: fresh ? [{ row_kind: "empty_placeholder" }] : [{ row_kind: "user", body: "Case one" }, { row_kind: "assistant", body: "Complete" }],
    status_message: `作業フォルダー: ${workspace}`,
  }, prompt: "", errors: [], visible_dialogs: [], palette: false, palette_actions: [] };
}

function exercise({ initial = surface(), selected = surface(newPath, { fresh: true }), extraCommand = null, commandArgs = null, installError = null,
  cleanupError = null, openError = null, pickerCount = 1, ownerDrift = false } = {}) {
  let current = structuredClone(initial), sequence = 0, pickerVisible = false;
  const records = [], clicks = [], calls = [], resources = [], nativeActions = [];
  const sink = { async record(name, value) { records.push({ name, value: structuredClone(value) }); } };
  const input = {
    async installProbe() { resources.push("input-install"); },
    async cleanup() { resources.push("input-cleanup"); if (cleanupError === "input") throw new Error("input cleanup failed"); return {}; },
  };
  const commands = {
    async install() { resources.push("commands-install"); if (installError) throw new Error(installError); },
    async snapshot(after = 0) { return { found: true, sequence, dropped_through: 0, calls: structuredClone(calls.filter(row => row.sequence > after)) }; },
    async remove() { resources.push("commands-remove"); if (cleanupError === "commands") throw new Error("commands cleanup failed"); return { removed: true }; },
  };
  const dependencies = {
    createInput: () => input, createCommands: () => commands,
    observe: async () => structuredClone(current),
    async wait(options) {
      assert.equal(options.timeoutMs, 15_000);
      assert.equal(options.retrySampleErrors, false);
      const value = await options.sample();
      if (!options.accept(value)) { const error = new Error(options.label); error.code = "observation-timeout"; error.evidence = { last_observation: value }; throw error; }
      return { value };
    },
    async replace(_input, locator, text) {
      if (locator.identity.id === "local-search") {
        assert.equal(text, "プロジェクトを追加");
        current.palette_actions = [{ id: "create-project-from-picker", disabled: false }];
      } else throw new Error("unexpected text target");
      return { test: "GUI text injection is independently covered by WebviewInput tests" };
    },
    async click(_input, locator) {
      clicks.push(structuredClone(locator));
      switch (locator.identity.action) {
        case "show-command-palette": current.projection.overlay = "command_palette"; current.palette = true; break;
        case "create-project-from-picker":
          calls.push({ sequence: ++sequence, command: "create_project_from_picker", args: commandArgs ?? {} });
          if (extraCommand) calls.push({ sequence: ++sequence, command: extraCommand, args: {} });
          pickerVisible = true; current.palette = false; current.projection.overlay = "none"; break;
        default: throw new Error(`unexpected action ${locator.identity.action}`);
      }
      return { test: "Native trust is independently covered by WebviewInput tests and the real suite qualification" };
    },
    native: {
      async snapshot(owner) {
        assert.equal(owner.executionRoot, root);
        assert.equal(owner.ownerPath, runtime.desktop_owner_path);
        assert.deepEqual(owner.expectedOwner, runtime.desktop_owner);
        const windows = [{ hwnd: "0x10", root_hwnd: "0x10", process_id: 90, thread_id: 1, class_name: "Tauri Window", is_root: true, visible: true, enabled: true }];
        if (pickerVisible) for (let index = 0; index < pickerCount; index++) windows.push({ hwnd: `0x${20 + index}`, root_hwnd: `0x${20 + index}`, process_id: 90,
          thread_id: 2 + index, class_name: "#32770", is_root: true, visible: true, enabled: true });
        return { owner: { ...runtime.desktop_owner, ...(ownerDrift && pickerVisible ? { process_start_time_utc_ticks: "2000" } : {}) }, windows };
      },
      async open(args) {
        nativeActions.push({ action: "open", args: structuredClone(args) });
        assert.equal(args.executionRoot, root); assert.equal(args.ownerPath, runtime.desktop_owner_path);
        assert.equal(args.intent, "directory"); assert.equal(args.selectedPath, newPath); assert.equal(args.candidate.hwnd, "0x20");
        if (openError) throw openError;
        pickerVisible = false; current = structuredClone(selected);
        return { selected_path: args.selectedPath, selected_path_intent: "directory", delivery_verified: true };
      },
      async probe(args) { nativeActions.push({ action: "probe", args: structuredClone(args) }); return { live: pickerVisible }; },
      async close(args) { nativeActions.push({ action: "close", args: structuredClone(args) }); if (cleanupError === "native") throw new Error("native cleanup failed"); pickerVisible = false; return { cleanup_only: true }; },
    },
  };
  const run = () => transitionManualSuiteWorkspace({ context: { root, paths: { workspace: newPath } }, previousContext: { paths: { workspace: oldPath } }, driver: {}, runtime, sink }, dependencies);
  return { run, records, clicks, calls, resources, nativeActions };
}

test("idle predecessor opens the exact native project folder once and receives a fresh case owner", async () => {
  const value = exercise(), result = await value.run();
  assert.equal(result.workspace, newPath);
  assert.equal(result.fresh_session, true);
  assert.equal(result.switched, true);
  assert.deepEqual(value.clicks.map(row => row.identity.action), ["show-command-palette", "create-project-from-picker"]);
  assert.deepEqual(value.calls.map(row => row.command), ["create_project_from_picker"]);
  assert.equal(value.nativeActions.filter(row => row.action === "open").length, 1);
  assert.equal(value.nativeActions.some(row => row.action === "close"), false);
  assert.deepEqual(value.resources, ["input-install", "commands-install", "commands-remove", "input-cleanup"]);
  assert.ok(value.records.some(row => row.name === "manual-suite-workspace-ready"));
});

test("an existing ancestor project with conversation A does not restore A when opening case B", async () => {
  const initial = surface();
  initial.projection.project_rows = [{ project_id: "5G6B7K88AAZKGG7CRK6N6PVYT5", path: root, label: "common ancestor" }];
  initial.projection.selected_project_index = 0;
  initial.projection.session_rows = [{ session_id: initial.projection.draft_target.sessionId }];
  initial.projection.selected_session_index = 0;
  const selected = surface(newPath, { fresh: true });
  selected.projection.project_rows = structuredClone(initial.projection.project_rows);
  selected.projection.selected_project_index = 0;
  selected.projection.session_rows = structuredClone(initial.projection.session_rows);
  selected.projection.selected_session_index = -1;
  const value = exercise({ initial, selected });
  const result = await value.run();
  assert.equal(result.surface.projection.workspace_path, newPath);
  assert.equal(result.surface.projection.draft_target.sessionId, null);
  assert.deepEqual(result.surface.projection.session_rows, initial.projection.session_rows);
  assert.deepEqual(value.calls, [{ sequence: 1, command: "create_project_from_picker", args: {} }]);
});

test("active, permission, async, prompt draft and stale workspace refuse input before probe installation", async () => {
  for (const change of [
    value => value.projection.busy = true,
    value => value.projection.confirmation = { id: 1 },
    value => value.projection.pending_async_operations = ["running"],
    value => value.projection.agent_tree_active = true,
    value => value.projection.task_activity_state = "running",
    value => value.prompt = "unsubmitted user draft",
    value => value.projection.draft_prompt = "uncommitted model draft",
    value => value.projection.draft_target.workspacePath = "C:\\wrong",
    value => value.projection.workspace_path = "C:\\wrong",
  ]) {
    const initial = surface(); change(initial);
    const value = exercise({ initial });
    await assert.rejects(value.run(), error => error.code === "manual-suite-workspace-predecessor");
    assert.equal(value.clicks.length, 0);
    assert.equal(value.resources.length, 0);
  }
});

test("fresh owner gate independently rejects prior history, session targets, overlays and wrong run workspace", () => {
  assert.deepEqual(manualSuiteWorkspaceFailures(surface(newPath, { fresh: true }), newPath), []);
  for (const [failure, change] of [
    ["canonical-history", value => value.projection.transcript_rows = [{ row_kind: "assistant", body: "Old result" }]],
    ["canonical-history", value => value.projection.thread_empty = false],
    ["session-owner", value => value.projection.run_target.sessionId = "old"],
    ["session-owner", value => value.projection.draft_target.sessionId = "old"],
    ["workspace-owner", value => value.projection.run_target.workspacePath = oldPath],
    ["overlay", value => value.visible_dialogs = [{ labelledby: "permission-title" }]],
    ["surface-errors", value => value.errors = ["error"]],
    ["pending-input", value => value.projection.attached_images = ["old-image.png"]],
    ["pending-input", value => value.projection.image_input = "old-image.png"],
    ["pending-input", value => value.projection.pending_turn_inputs = [{ text: "queued old case" }]],
  ]) { const value = surface(newPath, { fresh: true }); change(value); assert.ok(manualSuiteWorkspaceFailures(value, newPath).includes(failure)); }
});

test("a picker that restores the old session or a different cwd fails instead of accepting prior work", async () => {
  for (const selected of [surface(newPath), surface(oldPath, { fresh: true })]) {
    const value = exercise({ selected });
    await assert.rejects(value.run(), error => error.code === "observation-timeout");
    assert.deepEqual(value.clicks.map(row => row.identity.action), ["show-command-palette", "create-project-from-picker"]);
    assert.deepEqual(value.calls.map(row => row.command), ["create_project_from_picker"]);
    assert.ok(value.records.some(row => row.name === "manual-suite-workspace-error"));
    assert.deepEqual(value.resources.slice(-2), ["commands-remove", "input-cleanup"]);
  }
});

test("extra commands, duplicate project opening and unexpected picker arguments cannot satisfy the oracle", async () => {
  for (const variant of [{ extraCommand: "submit_prompt" }, { extraCommand: "create_project_from_picker" }, { extraCommand: "switch_workspace" }, { commandArgs: { unexpected: true } }]) {
    const value = exercise(variant);
    await assert.rejects(value.run(), error => error.code === "desktop-command-probe-cardinality" || error.code === "desktop-command-probe-call-mismatch");
    assert.equal(value.records.some(row => row.name === "manual-suite-workspace-ready"), false);
  }
});

test("a different process owner or ambiguous native picker never receives a path selection", async () => {
  for (const variant of [{ ownerDrift: true }, { pickerCount: 2 }]) {
    const value = exercise(variant);
    await assert.rejects(value.run(), error => error.code === "native-owner-identity-drift" || error.code === "native-window-cardinality");
    assert.equal(value.nativeActions.some(row => row.action === "open" || row.action === "close"), false);
    assert.deepEqual(value.resources.slice(-2), ["commands-remove", "input-cleanup"]);
  }
});

test("ambiguous path delivery is never repeated and closes only the acquired picker during cleanup", async () => {
  const openError = Object.assign(new Error("path delivery ambiguous"), { code: "native-dialog-path-open-invalid" });
  for (const cleanupError of [null, "native"]) {
    const value = exercise({ openError, cleanupError });
    await assert.rejects(value.run(), error => error === openError);
    assert.equal(value.nativeActions.filter(row => row.action === "open").length, 1);
    const closes = value.nativeActions.filter(row => row.action === "close");
    assert.equal(closes.length, 1); assert.equal(closes[0].args.candidate.hwnd, "0x20");
    const receipt = value.records.findLast(row => row.name === "manual-suite-workspace-cleanup");
    assert.equal(receipt.value.errors.length, cleanupError ? 1 : 0);
    assert.deepEqual(value.resources.slice(-2), ["commands-remove", "input-cleanup"]);
  }
});

test("failed command observer acquisition does not remove another owner's probe", async () => {
  const value = exercise({ installError: "existing observer owner" });
  await assert.rejects(value.run(), /existing observer owner/);
  assert.deepEqual(value.resources, ["input-install", "commands-install", "input-cleanup"]);
  assert.equal(value.clicks.length, 0);
});

test("cleanup failure cannot leave an otherwise successful transition accepted", async () => {
  for (const cleanupError of ["input", "commands"]) {
    const value = exercise({ cleanupError });
    await assert.rejects(value.run(), error => error.code === "manual-suite-workspace-cleanup");
    const receipt = value.records.findLast(row => row.name === "manual-suite-workspace-cleanup");
    assert.equal(receipt.value.errors.length, 1);
  }
});

test("an already fresh destination needs no GUI mutation", async () => {
  const value = exercise({ initial: surface(newPath, { fresh: true }) });
  const result = await transitionManualSuiteWorkspace({ context: { paths: { workspace: newPath } }, driver: {}, sink: { async record() {} } }, {
    createInput: () => ({ installProbe() { throw new Error("not expected"); } }),
    createCommands: () => ({ install() { throw new Error("not expected"); } }),
    observe: async () => surface(newPath, { fresh: true }),
  });
  assert.equal(result.switched, false);
  assert.equal(result.fresh_session, true);
  assert.equal(value.clicks.length, 0);
});

function restoration({ projectFirst = false, drift = null, active = false, missingProject = false, cleanupError = false } = {}) {
  const checkpoint = { workspace: newPath, session_id: "01M4JZ789M94P1YM0A3GWH9EDE", turn_id: "01M4JZ789M94P1YM0A3GWH9EDF",
    project_id: "01M4JZ789M94P1YM0A3GWH9EDG", admission_revision: "3", turn_page_total: 628, turn_page_limit: 80 };
  let current = surface(), resources = [], clicks = [], records = [];
  current.projection.busy = active;
  current.projection.session_rows = projectFirst ? [] : [{ session_id: checkpoint.session_id }];
  current.projection.project_rows = missingProject ? [] : [{ project_id: checkpoint.project_id }];
  const selected = surface(newPath);
  selected.projection.draft_target.sessionId = checkpoint.session_id;
  selected.projection.run_target.sessionId = checkpoint.session_id;
  selected.projection.run_target.expectedState = { kind: "idle", latestTurnId: checkpoint.turn_id, admissionRevision: "3" };
  selected.projection.session_rows = [{ session_id: checkpoint.session_id }];
  selected.projection.selected_session_index = 0;
  // Opening a saved row can load a bounded page. Full-history continuity belongs
  // to the existing Case52 restart/page owner, rather than this navigation gate.
  selected.projection.turn_page_limit = 10;
  if (drift) drift(selected);
  const dependencies = {
    observe: async () => structuredClone(current),
    createInput: () => ({ async installProbe() { resources.push("install"); }, async cleanup() { resources.push("cleanup"); if (cleanupError) throw new Error("cleanup failed"); } }),
    async click(_input, locator) {
      clicks.push(structuredClone(locator));
      if (locator.identity.action === "project") {
        current.projection.selected_project_index = 0;
        current.projection.session_rows = [{ session_id: checkpoint.session_id }];
      } else current = structuredClone(selected);
      return { trusted: true };
    },
    async wait(options) {
      const value = await options.sample();
      if (!options.accept(value)) throw Object.assign(new Error("saved owner did not settle"), { code: "observation-timeout" });
      return { value };
    },
  };
  const run = () => restoreManualSuiteSession({ context: { paths: { workspace: newPath } }, previousContext: { paths: { workspace: oldPath } },
    driver: {}, checkpoint, sink: { async record(name, payload) { records.push({ name, payload }); } } }, dependencies);
  return { run, clicks, resources, records, checkpoint, selected };
}

test("deferred restoration reselects the saved session without creating a conversation or sending a prompt", async () => {
  const value = restoration();
  const result = await value.run();
  assert.deepEqual(manualSuiteRestoredSessionFailures(result, value.checkpoint), []);
  assert.deepEqual(value.clicks.map(row => row.identity.action), ["session"]);
  assert.equal(value.clicks[0].identity.focusKey, `session:${value.checkpoint.session_id}:select`);
  assert.deepEqual(value.resources, ["install", "cleanup"]);
});

test("restoration navigates to the exact saved project before selecting its session when needed", async () => {
  const value = restoration({ projectFirst: true }); await value.run();
  assert.deepEqual(value.clicks.map(row => row.identity.action), ["project", "session"]);
  assert.equal(value.clicks[0].identity.focusKey, `project:${value.checkpoint.project_id}:select`);
  assert.deepEqual(value.resources, ["install", "cleanup"]);
});

test("wrong saved workspace, session, latest turn or admission cannot satisfy restoration", async () => {
  for (const drift of [v => v.projection.workspace_path = oldPath, v => v.projection.draft_target.sessionId = "old-session",
    v => v.projection.selected_session_index = -1, v => v.projection.run_target.expectedState.latestTurnId = "old-turn",
    v => v.projection.run_target.expectedState.admissionRevision = "2"]) {
    const value = restoration({ drift });
    await assert.rejects(value.run(), error => error.code === "observation-timeout");
    assert.deepEqual(value.clicks.map(row => row.identity.action), ["session"]);
    assert.deepEqual(value.resources, ["install", "cleanup"]);
  }
});

test("active predecessor, missing saved project and failed probe cleanup cannot be accepted", async () => {
  const active = restoration({ active: true });
  await assert.rejects(active.run(), error => error.code === "manual-suite-workspace-predecessor");
  assert.deepEqual(active.resources, []); assert.deepEqual(active.clicks, []);
  const missing = restoration({ projectFirst: true, missingProject: true });
  await assert.rejects(missing.run(), error => error.code === "manual-suite-workspace-saved-session");
  assert.deepEqual(missing.clicks, []); assert.deepEqual(missing.resources, ["install", "cleanup"]);
  const cleanup = restoration({ cleanupError: true }); await assert.rejects(cleanup.run(), /cleanup failed/);
  assert.deepEqual(cleanup.resources, ["install", "cleanup"]);
});
