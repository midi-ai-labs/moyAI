import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { manualLiveExportTranscript } from "../drivers/manual_live_session.mjs";

const SESSION = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const OTHER = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const TURN = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const PROMPT = "A multiline request.\n\nKeep the exact body.";

function surface(workspace, { selected = SESSION, status = "実行完了" } = {}) {
  const target = { workspacePath: workspace, sessionId: SESSION, runtimeOwnerToken: "idle:3",
    expectedState: { kind: "idle", latestTurnId: TURN, admissionRevision: "3" } };
  return { projection: { workspace_path: workspace, run_target: target,
    draft_target: { workspacePath: workspace, sessionId: SESSION, ownerGeneration: "1" },
    selected_project_index: 0, project_rows: [{ project_id: OTHER, path: workspace }],
    selected_session_index: 0, session_rows: [{ session_id: selected, title: "private title" }],
    status_message: status, status_detail: "private status detail", run_status_key: "completed",
    history_export_enabled: true, busy: false, navigation_loading: false,
    post_run_refresh_pending: false, background_mutation_pending: false,
    async_polling_required: false, pending_async_operations: [],
    config_fields: [{ key: "secret", value: "private credential" }],
    transcript_rows: [{ body: "private conversation" }] },
    composer: { count: 1, visible: true, run_target: structuredClone(target) },
    selected_navigation: [{ action: "session", focus_key: `session:${selected}:select`, visible: true }],
    visible_fatal_count: 0, visible_recoverable_error_count: 0 };
}

async function fixture(t, { before = {}, after = {}, observeError = false, clickError = false, content = null } = {}) {
  const parent = fileURLToPath(new URL("../../../../project_sandbox/manual-st-harness-self-tests/", import.meta.url));
  await mkdir(parent, { recursive: true });
  const workspace = await mkdtemp(path.join(parent, "export-fix-"));
  t.after(async () => { assert.equal(path.dirname(workspace), path.resolve(parent)); await rm(workspace, { recursive: true }); });
  const events = [], records = [], bytes = [], waits = [];
  let clicked = false;
  const input = {
    snapshotProbe: async (sequence = 0) => ({ found: true, sequence: events.length,
      dropped_through: 0, events: events.filter(row => row.sequence > sequence) }),
    click: async locator => {
      if (clickError) { const error = new Error("driver click failed"); error.code = "semantic-target-hidden"; throw error; }
      clicked = true;
      events.push({ sequence: events.length + 1, type: "click", isTrusted: true, ...locator.identity });
      if (content !== null) {
        const directory = path.join(workspace, ".moyai", "transcript-exports");
        await mkdir(directory, { recursive: true });
        await writeFile(path.join(directory, "conversation.md"), content);
      }
      return { identity: locator.identity };
    },
  };
  const sink = { record: async (kind, payload) => records.push({ kind, payload }),
    writeBytes: async (name, value) => { bytes.push({ name, value }); return { relative_path: name }; } };
  const driver = { evaluate: async () => {
    if (observeError === true || observeError === "after" && clicked) { const error = new Error("private driver diagnostic"); error.code = "cdp-disconnected"; throw error; }
    return surface(workspace, clicked ? after : before);
  } };
  const wait = async options => {
    waits.push({ label: options.label, timeoutMs: options.timeoutMs });
    const value = await options.sample();
    if (await options.accept(value)) return { value, attempts: 1, elapsed_ms: 1 };
    const error = new Error(`Manual transcript export timed out after ${options.timeoutMs}ms`);
    error.code = "observation-timeout";
    error.evidence = { label: options.label, attempts: 1, elapsed_ms: options.timeoutMs, last_value: value, last_error: null };
    throw error;
  };
  const args = { context: { paths: { workspace } }, driver, input, sink, sessionId: SESSION,
    prompt: PROMPT, owner: "scenario:manual.case3", stem: "case3",
    observationBudget: { assertRemaining: (_label, ceiling) => ceiling === undefined ? 20 : Math.min(ceiling, 20) } };
  return { args, wait, events, records, bytes, waits };
}

function markdown(prompt = PROMPT, session = SESSION) {
  return `# Conversation\n\n> ${prompt.replaceAll("\n", "\n> ")}\n\nDone.\n\n<details><summary>実行情報</summary>\n\n- Session: \`${session}\`\n</details>\n`;
}

test("export owner mismatch after an acquired trusted click is a product failure with public evidence", async t => {
  const f = await fixture(t, { before: { selected: OTHER }, after: { selected: OTHER } });
  await assert.rejects(manualLiveExportTranscript(f.args, { wait: f.wait }), error => {
    assert.equal(error.owner, "product");
    assert.equal(error.code, "manual-export-owner-mismatch");
    assert.deepEqual(error.evidence.observation.last_value, []);
    assert.equal(error.evidence.export.before.state.selected_session_id, OTHER);
    assert.equal(error.evidence.export.after.state.run_target.sessionId, SESSION);
    assert.equal(error.evidence.export.trusted_click, true);
    return true;
  });
  assert.equal(f.events.length, 1);
  assert.equal(f.records.filter(row => row.kind === "case3-export-observation").length, 2);
});

test("a newly observed specific export refusal is product owned, while an unchanged stale refusal is not", async t => {
  const refusal = "transcript export target is no longer available";
  for (const stale of [false, true]) {
    const f = await fixture(t, { before: { status: stale ? refusal : "実行完了" }, after: { status: refusal } });
    await assert.rejects(manualLiveExportTranscript(f.args, { wait: f.wait }), error => {
      assert.equal(error.owner, stale ? "harness" : "product");
      assert.equal(error.code, stale ? "observation-timeout" : "manual-export-rejected");
      assert.equal(error.evidence.export.after.state.status.kind, "target-unavailable");
      return true;
    });
  }
});

test("unknown export timeout keeps its original diagnosis and remains a harness acquisition failure", async t => {
  const f = await fixture(t);
  await assert.rejects(manualLiveExportTranscript(f.args, { wait: f.wait }), error => {
    assert.equal(error.owner, "harness"); assert.equal(error.code, "observation-timeout");
    assert.equal(error.evidence.observation.elapsed_ms, 20);
    assert.equal(error.evidence.observation.last_error, null);
    assert.deepEqual(error.evidence.observation.last_value, []);
    return true;
  });
});

test("public observation failure cannot turn the original file timeout into a product failure", async t => {
  const f = await fixture(t, { observeError: true, after: { selected: OTHER } });
  await assert.rejects(manualLiveExportTranscript(f.args, { wait: f.wait }), error => {
    assert.equal(error.owner, "harness"); assert.equal(error.code, "observation-timeout");
    assert.equal(error.evidence.export.after.error.code, "cdp-disconnected");
    assert.equal(error.evidence.observation.last_error, null);
    return true;
  });
});

test("driver click failure is retained without pretending an export command was acquired", async t => {
  const f = await fixture(t, { clickError: true, after: { selected: OTHER } });
  await assert.rejects(manualLiveExportTranscript(f.args, { wait: f.wait }), error => {
    assert.equal(error.owner, "harness"); assert.equal(error.code, "semantic-target-hidden");
    assert.equal(error.message, "driver click failed");
    assert.equal(error.evidence.export.trusted_click, false);
    return true;
  });
  assert.equal(f.events.length, 0);
});

test("successful exact export preserves bytes and does not expose unrelated public fields", async t => {
  const f = await fixture(t, { content: markdown(), after: { status: "会話をMarkdownで保存しました: private path" } });
  const result = await manualLiveExportTranscript(f.args, { wait: f.wait });
  assert.equal(await readFile(result.source_path, "utf8"), markdown());
  assert.equal(f.bytes[0].value.toString("utf8"), markdown());
  const diagnostic = JSON.stringify(f.records.filter(row => row.kind === "case3-export-observation"));
  for (const privateValue of ["private credential", "private conversation", "private title", "private status detail", "private path"]) {
    assert.equal(diagnostic.includes(privateValue), false);
  }
  assert.equal(f.records.at(-1).payload.after.state.status.kind, "saved");
});

test("invalid exported session or prompt is still a product content failure", async t => {
  for (const content of [markdown(PROMPT, OTHER), markdown("Different request")]) {
    const f = await fixture(t, { content });
    await assert.rejects(manualLiveExportTranscript(f.args, { wait: f.wait }), error => error.owner === "product" && error.code === "case1-export-content");
  }
});

test("unbounded cases keep the separate ten second export acquisition ceiling", async t => {
  const f = await fixture(t, { content: markdown() });
  f.args.observationBudget = null;
  await manualLiveExportTranscript(f.args, { wait: f.wait });
  assert.equal(f.waits.find(row => row.label === "Manual transcript export").timeoutMs, 10_000);
});

test("an unavailable post-click observation or changed current turn stays a harness failure", async t => {
  for (const changed of ["observer", "turn", "workspace", "malformed"]) {
    const f = await fixture(t, { observeError: changed === "observer" ? "after" : false,
      before: { selected: OTHER }, after: { selected: OTHER, status: "transcript export target is no longer available" } });
    const evaluate = f.args.driver.evaluate;
    let calls = 0;
    f.args.driver.evaluate = async (...args) => {
      const value = await evaluate(...args);
      if (calls++ > 0) {
        if (changed === "turn") value.projection.run_target.expectedState.latestTurnId = OTHER;
        if (changed === "workspace") value.projection.run_target.workspacePath = path.join(f.args.context.paths.workspace, "different");
        if (changed === "malformed") value.projection.run_target.expectedState.admissionRevision = "bad";
      }
      return value;
    };
    await assert.rejects(manualLiveExportTranscript(f.args, { wait: f.wait }), error => error.owner === "harness" && error.code === "observation-timeout");
  }
});

test("diagnostic storage failure preserves the original timeout and the separate evidence failure", async t => {
  const f = await fixture(t, { before: { selected: OTHER }, after: { selected: OTHER } });
  const record = f.args.sink.record;
  f.args.sink.record = async (kind, payload) => {
    if (kind === "case3-export-observation" && payload.after) throw Object.assign(new Error("private evidence storage reason"), { code: "EACCES" });
    return record(kind, payload);
  };
  await assert.rejects(manualLiveExportTranscript(f.args, { wait: f.wait }), error => {
    assert.equal(error.owner, "harness"); assert.equal(error.code, "observation-timeout");
    assert.equal(error.evidence.observation.label, "Manual transcript export");
    assert.equal(error.evidence.evidence_capture_error.code, "EACCES");
    assert.equal(JSON.stringify(error.evidence).includes("private evidence storage reason"), false);
    return true;
  });
});

test("a failed file observation is not reclassified from an unrelated refusal status", async t => {
  const f = await fixture(t, { after: { status: "transcript export target is no longer available" } });
  const wait = async options => {
    if (options.label !== "Manual transcript export") return f.wait(options);
    const error = new Error("Manual transcript export timed out after 20ms");
    error.code = "observation-timeout";
    error.evidence = { label: options.label, attempts: 1, elapsed_ms: 20, last_value: [], last_error: "filesystem observation unavailable" };
    throw error;
  };
  await assert.rejects(manualLiveExportTranscript(f.args, { wait }), error => error.owner === "harness"
    && error.code === "observation-timeout" && error.evidence.observation.last_error === "filesystem observation unavailable");
});
