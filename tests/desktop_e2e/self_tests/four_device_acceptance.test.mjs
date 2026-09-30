import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { ANALYSIS_INPUT, ANALYSIS_EXPECTED, analysisJobId, sameAnalysisJobId, inputHash, resultAccepted, validateManifest, loopbackUrl } from "../fixtures/three_node_acceptance.mjs";
import { controlReply, KEEPALIVE_COMMAND } from "../fixtures/four_device_control.mjs";
import { approvalReviewRoot, foreignInspectionName, foreignOriginViewAccepted, foreignRootArrived, normalizeFourDeviceOptions, projectOverview, sameWindowsPath, fourDeviceLayout } from "../scenarios/four_device_acceptance.mjs";
import { applicationProcessOwners, captureComponent, commandIdentifiesEntrypoint, listenerMatchesUrl, submitAnalysisThroughUi } from "../scenarios/three_node_app_oracle.mjs";
import { captureScenarioScreenshot } from "../scenarios/observations.mjs";

const environments = Object.fromEntries(["api", "worker", "database"].map(role => [role, { environment_id: `env-${role}`, directory: path.resolve("acceptance-fixture", role) }]));
test("GUI-selected Windows folders match ordinary and verbatim forms without accepting siblings", () => {
  assert.equal(sameWindowsPath(String.raw`C:\fixture\api`, String.raw`\\?\C:\fixture\api`), true);
  assert.equal(sameWindowsPath("c:/fixture/api", String.raw`\\?\C:\FIXTURE\api`), true);
  assert.equal(sameWindowsPath(String.raw`\\server\share\api`, String.raw`\\?\UNC\server\share\api`), true);
  assert.equal(sameWindowsPath(String.raw`C:\fixture\api`, String.raw`C:\fixture\api-other`), false);
  assert.equal(sameWindowsPath(String.raw`\\server\share\api`, String.raw`\\?\UNC\server\share\api-other`), false);
  assert.equal(sameWindowsPath(undefined, String.raw`C:\fixture\api`), false);
});
function manifest() {
  return { version: 1, components: Object.fromEntries(Object.entries(environments).map(([role, env], index) => [role, {
    environment_id: env.environment_id, entrypoint: path.join(env.directory, "app.js"), base_url: `http://127.0.0.1:${8100 + index}/`,
    ...(role === "database" ? { database_file: path.join(env.directory, "jobs.db") } : {}),
  }])), ui_url: "http://127.0.0.1:8100/", control_url: "http://127.0.0.1:8100/control" };
}

test("foreign approval inspection opens the normal root turn and verifies the exact child approval read-only", () => {
  const expected = { id: "child-approval", context: { job_id: "child-job", root_id: "origin-root" } };
  const rootId = approvalReviewRoot(expected, "selected-root");
  assert.equal(rootId, "origin-root");
  assert.notEqual(rootId, expected.context.job_id);
  const controls = { handover: false, normal: [] };
  const view = { detail: { id: rootId, can_continue: false, can_revise: false }, status: { jobs: [{ id: rootId, can_cancel: false }] },
    approval: { ...structuredClone(expected), can_decide: false, can_reconfirm: false } };
  assert.equal(foreignOriginViewAccepted(view, rootId, controls, expected), true);
  for (const mutate of [v => { v.approval.id = "other-approval"; }, v => { v.approval.context.job_id = "other-child"; },
    v => { v.approval.context.root_id = "other-root"; }, v => { v.approval.can_decide = true; },
    v => { v.approval.can_reconfirm = true; }, v => { v.detail.can_continue = true; }, v => { v.detail.can_revise = true; },
    v => { v.status.jobs[0].can_cancel = true; }, v => { v.detail.id = "child-job"; }, v => { v.approval = null; }]) {
    const wrong = structuredClone(view); mutate(wrong);
    assert.equal(foreignOriginViewAccepted(wrong, rootId, controls, expected), false);
  }
  assert.equal(foreignOriginViewAccepted(view, rootId, { handover: true, normal: [] }, expected), false);
  assert.equal(foreignOriginViewAccepted(view, rootId, { handover: false, normal: ["shared-approve"] }, expected), false);
});

test("foreign navigation waits for both newly submitted root and conversation to propagate", () => {
  const root = { id: "new-d-root", root_id: "new-d-root", parent_id: null, conversation_id: "new-d-conversation", state: "queued" };
  const old = { selected_job_id: "old-a-root", status: { jobs: [] }, conversations: [] };
  assert.equal(foreignRootArrived(old, root.id), false);
  assert.equal(foreignRootArrived({ ...old, status: { jobs: [root] } }, root.id), false);
  assert.equal(foreignRootArrived({ ...old, conversations: [{ id: root.conversation_id }] }, root.id), false);
  assert.equal(foreignRootArrived({ ...old, status: { jobs: [root] }, conversations: [{ id: root.conversation_id }] }, root.id), true);
  assert.equal(foreignRootArrived({ ...old, status: { jobs: [{ ...root, parent_id: "parent" }] }, conversations: [{ id: root.conversation_id }] }, root.id), false);
});

test("repeated foreign inspection screenshots pass the real name validator and remain distinct", async () => {
  const saved = new Set(), root = "01M3J3DPR1GMZ1N3T7R0M1KEPE";
  const approvals = [{ id: "01M3J3DS68X5EYZ335N5Y1K17C" }, { id: "01M3J3DXF07M4Q7C9W3R82ZCPJ" }, null];
  const sink = { async writeBytes(relative_path) {
    assert.equal(saved.has(relative_path), false, "Evidence is append-only");
    saved.add(relative_path); return { relative_path };
  }, async record() {} };
  for (const approval of approvals) {
    await captureScenarioScreenshot({ cdp: { async screenshot() { return Buffer.from("fixture"); } }, sink,
      name: `d-${foreignInspectionName("a", root, approval)}`, owner: "test" });
  }
  assert.equal(saved.size, 3);
  assert.equal(approvals[0].id, "01M3J3DS68X5EYZ335N5Y1K17C", "Authority IDs retain their exact original case");
});
test("application result oracle fixes exact input bytes, sorting and digest", () => {
  assert.equal(Buffer.byteLength(ANALYSIS_INPUT), 49);
  assert.equal(inputHash(), ANALYSIS_EXPECTED.sha256);
  assert.equal(resultAccepted({ status: "COMPLETED", result: structuredClone(ANALYSIS_EXPECTED) }), true);
  for (const patch of [{ sha256: "wrong" }, { word_count: 7 }, { line_count: 4 }, { top5: [...ANALYSIS_EXPECTED.top5].reverse() }]) {
    assert.equal(resultAccepted({ status: "COMPLETED", result: { ...ANALYSIS_EXPECTED, ...patch } }), false);
  }
  assert.equal(resultAccepted({ status: "PENDING", result: ANALYSIS_EXPECTED }), false);
});

test("analysis result accepts JSON key order and hex case without coercing incorrect values", () => {
  const result = { ...ANALYSIS_EXPECTED, sha256: ANALYSIS_EXPECTED.sha256.toUpperCase(),
    top5: ANALYSIS_EXPECTED.top5.map(({ word, count }) => ({ count, word })) };
  assert.equal(resultAccepted({ status: "COMPLETED", result }), true);
  for (const patch of [{ word_count: "8" }, { line_count: "3" }, { sha256: `0x${ANALYSIS_EXPECTED.sha256}` },
    { sha256: ANALYSIS_EXPECTED.sha256.slice(1) }, { top5: result.top5.slice(0, 4) }, { top5: [...result.top5, result.top5[0]] },
    { top5: result.top5.map(entry => ({ ...entry, count: String(entry.count) })) }]) {
    assert.equal(resultAccepted({ status: "COMPLETED", result: { ...result, ...patch } }), false);
  }
});

test("job identity accepts safe integer IDs from API and their UI text without merging distinct IDs", () => {
  assert.equal(analysisJobId(0), "0");
  assert.equal(sameAnalysisJobId(7, "7"), true);
  assert.equal(sameAnalysisJobId("job-A", "job-A"), true);
  for (const [actual, expected] of [[7, "8"], [7, "07"], ["job-A", "job-a"], [null, null], [undefined, undefined],
    ["", ""], [NaN, "NaN"], [Infinity, "Infinity"], [1.5, "1.5"], [Number.MAX_SAFE_INTEGER + 1, "9007199254740992"], [{ id: 7 }, "7"]]) {
    assert.equal(sameAnalysisJobId(actual, expected), false);
  }
});

test("UI submission waits for the new displayed ID and retains the API's raw integer identity", async () => {
  let submitted = false, submittedReads = 0;
  const page = { getByTestId(id) {
    if (id === "job-id") return { async count() { return 1; }, async textContent() {
      if (!submitted) return "previous-job";
      return ++submittedReads === 1 ? "previous-job" : "0";
    } };
    if (id === "text-input") return { async fill(value) { assert.equal(value, ANALYSIS_INPUT); } };
    if (id === "submit-job") return { async click() { submitted = true; } };
    throw new Error(`Unexpected UI control: ${id}`);
  } };
  const result = await submitAnalysisThroughUi(page, "http://127.0.0.1:8181/", { readJob: async url => {
    assert.equal(url, "http://127.0.0.1:8181/jobs/0");
    assert.equal(submittedReads, 2, "The old displayed job must not be reused");
    return { id: 0, status: "PENDING" };
  } });
  assert.equal(result.id, 0);
  submitted = false;
  await assert.rejects(submitAnalysisThroughUi({ getByTestId(id) {
    if (id === "job-id") return { async count() { return submitted ? 1 : 0; }, async textContent() { return "new-job"; } };
    if (id === "text-input") return { async fill() { submitted = false; } };
    return { async click() { submitted = true; } };
  } }, "http://127.0.0.1:8181/", { readJob: async () => ({ id: "different-job", status: "PENDING" }) }), /different job IDs/);
});
test("manifest requires actual role folders and distinct declared environments", () => {
  assert.equal(validateManifest(manifest(), environments).version, 1);
  for (const mutate of [m => { m.components.worker.environment_id = environments.api.environment_id; },
    m => { m.components.worker.entrypoint = path.join(environments.api.directory, "worker.js"); },
    m => { m.components.database.database_file = path.resolve("outside.db"); },
    m => { m.components.database.base_url = "https://example.com/"; },
    m => { m.version = 0; }]) {
    const value = manifest(); mutate(value); assert.throws(() => validateManifest(value, environments));
  }
});
test("acceptance network oracle stays on unauthenticated loopback HTTP", () => {
  for (const value of ["https://127.0.0.1/", "http://example.com/", "http://user:pass@localhost/", "file:///tmp/db", "http://localhost/#fragment"]) assert.throws(() => loopbackUrl(value));
  assert.equal(loopbackUrl("http://127.0.0.1:8080/jobs"), "http://127.0.0.1:8080/jobs");
});
test("actual process command must identify the generated component entrypoint", () => {
  const entry = path.resolve("role folder", "worker.py");
  assert.equal(commandIdentifiesEntrypoint(`python "${entry}"`, entry), true);
  assert.equal(commandIdentifiesEntrypoint("python worker.py --port 8181", entry), true);
  assert.equal(commandIdentifiesEntrypoint("python fake-worker.py", entry), false);
  assert.equal(commandIdentifiesEntrypoint("python worker.py.bak", entry), false);
  assert.equal(commandIdentifiesEntrypoint("python server.py", entry), false);
});
test("role listener may belong to a supervisor while the actual Worker PID is separately owned", async () => {
  const root = path.resolve("acceptance-fixture"), runner = { process_id: 10 };
  const pc = { context: { root }, runner: { identity: runner }, runnerOwner: { owner: runner } };
  const sink = { root, async writeJson(relative_path) { return { relative_path }; } };
  const rows = new Map([
    [30, { process_id: 30, parent_process_id: 40, command_line: "python worker.py" }],
    [40, { process_id: 40, parent_process_id: 10, command_line: "python supervisor.py" }],
  ]);
  let listenerPid = 40;
  const invoke = async (action, parameters) => {
    if (action === "ObserveTreeListeners") return [{ process_id: listenerPid, address: "127.0.0.1", port: 8181 }];
    assert.equal(action, "Capture");
    if (!rows.has(parameters.ProcessId)) throw new Error("Outside assigned Runner tree");
    return rows.get(parameters.ProcessId);
  };
  const component = { entrypoint: path.join(root, "worker.py"), base_url: "http://127.0.0.1:8181/" };
  const observed = await captureComponent("worker", 30, pc, sink, "initial", component, { processCommand: invoke });
  assert.equal(observed.pid, 30);
  assert.equal(observed.listener_processes[0].pid, 40);
  assert.match(observed.listener_processes[0].ownerPath, /listener-40\.json$/);
  assert.deepEqual(applicationProcessOwners({ worker: observed }).map(row => row.pid), [30, 40]);
  listenerPid = 99;
  rows.set(99, { process_id: 99, parent_process_id: 98, command_line: "python foreign-supervisor.py" });
  await assert.rejects(captureComponent("worker", 30, pc, sink, "outside", component, { processCommand: invoke }), /Outside assigned Runner tree/);
});
function supervisedComponentFixture() {
  const root = path.resolve("acceptance-fixture"), runner = { process_id: 10 };
  const pc = { context: { root }, runner: { identity: runner }, runnerOwner: { owner: runner } };
  const artifacts = new Map();
  const sink = { root, async writeJson(relative_path, value) { artifacts.set(relative_path, value); return { relative_path }; } };
  const rows = new Map([
    [30, { process_id: 30, parent_process_id: 40, process_start_time_utc_ticks: "30", command_line: "python worker.py" }],
    [31, { process_id: 31, parent_process_id: 40, process_start_time_utc_ticks: "31", command_line: "python worker.py" }],
    [40, { process_id: 40, parent_process_id: 10, process_start_time_utc_ticks: "40", command_line: "python supervisor.py" }],
  ]);
  let listenerPid = 30;
  const invoke = async (action, parameters) => {
    if (action === "ObserveTreeListeners") return [{ process_id: listenerPid, address: "127.0.0.1", port: 8181 }];
    assert.equal(action, "Capture");
    if (!rows.has(parameters.ProcessId)) throw new Error("Outside assigned Runner tree");
    return rows.get(parameters.ProcessId);
  };
  const component = { entrypoint: path.join(root, "supervisor.py"), base_url: "http://127.0.0.1:8181/" };
  return { rows, artifacts, setListener(pid) { listenerPid = pid; },
    capture(pid, generation = "initial") { return captureComponent("worker", pid, pc, sink, generation, component, { processCommand: invoke }); } };
}
test("declared startup ancestor keeps actual Worker identity across restart and joins whole-stop owners", async () => {
  const fixture = supervisedComponentFixture();
  const initial = await fixture.capture(30);
  assert.equal(initial.pid, 30);
  assert.equal(initial.chain[0].process_id, 30);
  assert.equal(initial.startup_process.pid, 40);
  assert.equal(initial.startup_process.chain[0].process_id, 40);
  assert.match(initial.ownerPath, /app-worker-initial-30\.json$/);
  assert.equal([...fixture.artifacts.values()].some(row => row.process_id === 40), true);
  // A DB-style server can listen in the leaf: its non-listening startup owner
  // must still be checked when the whole conversation is stopped.
  assert.deepEqual(applicationProcessOwners({ worker: initial }).map(row => row.pid), [30, 40]);
  fixture.setListener(40);
  const restarted = await fixture.capture(31, "restart");
  assert.equal(restarted.pid, 31);
  assert.equal(restarted.startup_process.pid, 40);
  assert.deepEqual(applicationProcessOwners({ worker: restarted }).map(row => row.pid), [31, 40]);
});
test("an entrypoint on a sibling process cannot authorize the declared component", async () => {
  const fixture = supervisedComponentFixture();
  fixture.rows.get(40).command_line = "python unrelated-parent.py";
  fixture.rows.set(50, { process_id: 50, parent_process_id: 10, command_line: "python supervisor.py" });
  fixture.setListener(50);
  await assert.rejects(fixture.capture(30), /does not identify its entrypoint/);
});
test("matching startup command does not bypass assigned Runner ancestry", async () => {
  const fixture = supervisedComponentFixture();
  fixture.rows.get(40).parent_process_id = 99;
  await assert.rejects(fixture.capture(30), /Outside assigned Runner tree/);
});
test("startup ancestors with only entrypoint prefix or suffix matches are rejected", async () => {
  for (const command of ["python fake-supervisor.py", "python supervisor.py.bak"]) {
    const fixture = supervisedComponentFixture();
    fixture.rows.get(40).command_line = command;
    await assert.rejects(fixture.capture(30), /does not identify its entrypoint/);
  }
});
test("whole-stop verification includes distinct listener owners and deduplicates identical actual owners", () => {
  const owner = (pid, start = "1") => ({ pid, role: "api", chain: [{ process_start_time_utc_ticks: start }], ownerPath: `owner-${pid}-${start}.json` });
  const api = { ...owner(10), listener_processes: [owner(10), owner(20)] };
  const worker = { ...owner(30), listener_processes: [owner(40)] };
  const observed = applicationProcessOwners({ api, worker });
  assert.deepEqual(observed.map(row => row.pid), [10, 20, 30, 40]);
  assert.deepEqual(applicationProcessOwners({ a: owner(10), b: owner(10, "2") }).map(row => row.ownerPath), ["owner-10-1.json", "owner-10-2.json"]);
});
test("listener match includes the requested interface, not just a coincident port", () => {
  assert.equal(listenerMatchesUrl({ port: 8181, address: "0.0.0.0" }, "http://127.0.0.1:8181/"), true);
  assert.equal(listenerMatchesUrl({ port: 8181, address: "192.0.2.1" }, "http://127.0.0.1:8181/"), false);
  assert.equal(listenerMatchesUrl({ port: 8182, address: "127.0.0.1" }, "http://127.0.0.1:8181/"), false);
});
test("scenario options distinguish live provider credentials from scripted qualification", () => {
  assert.equal(normalizeFourDeviceOptions().live, null);
  assert.throws(() => normalizeFourDeviceOptions({ upstreamCredentialFile: path.resolve("credential.json") }));
  assert.throws(() => normalizeFourDeviceOptions({ runnerBinary: "relative.exe" }));
  assert.throws(() => normalizeFourDeviceOptions({ unknown: true }));
  const value = normalizeFourDeviceOptions({ liveProvider: { provider_base_url: "http://127.0.0.1:8119", model: "example" }, upstreamCredentialFile: path.resolve("credential.json") });
  assert.equal(value.live.model, "example");
});
test("comparison options retain ordinary defaults and bound only the test observation budget", () => {
  const defaults = normalizeFourDeviceOptions();
  assert.equal(defaults.contextComparison, null);
  assert.equal(defaults.generationObservationMs, 3600000);
  const options = { liveProvider: { provider_base_url: "http://127.0.0.1:8119", model: "example" }, upstreamCredentialFile: path.resolve("credential.json") };
  for (const contextComparison of ["remove", "preserve"]) {
    const value = normalizeFourDeviceOptions({ ...options, contextComparison, generationObservationMs: 7200000 });
    assert.equal(value.contextComparison, contextComparison); assert.equal(value.generationObservationMs, 7200000);
    assert.deepEqual(value.live, normalizeFourDeviceOptions(options).live);
  }
  for (const generationObservationMs of [0, -1, 1.5, "7200000", Infinity, 2147483648])
    assert.throws(() => normalizeFourDeviceOptions({ generationObservationMs }));
  for (const input of [{ contextComparison: "remove" }, { ...options, contextComparison: "unknown" },
    { liveProvider: options.liveProvider, contextComparison: "preserve" }]) assert.throws(() => normalizeFourDeviceOptions(input));
});
test("one role mapping changes overview and oracle owners while physical folders and permissions stay fixed", () => {
  const normal = fourDeviceLayout(), mapping = { api: "a", worker: "c", database: "b" }, swapped = fourDeviceLayout(mapping);
  assert.deepEqual(swapped.map(({ role, ...pc }) => pc), normal.map(({ role, ...pc }) => pc));
  assert.deepEqual(swapped.map(pc => [pc.name, pc.role]), [["a", "api"], ["b", "database"], ["c", "worker"], ["d", null]]);
  const physical = Object.fromEntries(normal.filter(pc => pc.folder).map(pc => [pc.name, {
    environment_id: `stable-${pc.name}`, directory: path.resolve("profiles", pc.name, pc.folder),
  }]));
  const roleOwners = Object.fromEntries(swapped.filter(pc => pc.role).map(pc => [pc.role, pc]));
  const assigned = Object.fromEntries(Object.entries(roleOwners).map(([role, pc]) => [role, physical[pc.name]]));
  const overview = projectOverview(assigned, mapping);
  assert.match(overview, /Worker環境: stable-c \(WinC\)/);
  assert.match(overview, /DB環境: stable-b \(WinB\)/);
  assert.equal(assigned.worker.directory, physical.c.directory); assert.equal(roleOwners.worker.name, "c");
  for (const roleMapping of [{ api: "b", worker: "a", database: "c" }, { api: "a", worker: "b", database: "b" },
    { api: "a", worker: "d", database: "c" }, { api: "a", worker: "b" }, { ...mapping, extra: "a" }])
    assert.throws(() => normalizeFourDeviceOptions({ roleMapping }));
});
test("overview carries stable role IDs while scripted controls never generate an app", () => {
  for (const env of Object.values(environments)) assert.ok(projectOverview(environments).includes(env.environment_id));
  const first = controlReply({ messages: [{ role: "user", content: "control qualification" }] }, environments);
  const firstCall = first.delta.tool_calls[0].function;
  assert.equal(firstCall.name, "shared_delegate");
  assert.equal(JSON.parse(firstCall.arguments).environment_id, environments.worker.environment_id);
  const child = controlReply({ messages: [{ role: "user", content: "four-device-child:worker" }] }, environments);
  assert.equal(JSON.parse(child.delta.tool_calls[0].function.arguments).command, KEEPALIVE_COMMAND);
  assert.equal(JSON.parse(child.delta.tool_calls[0].function.arguments).retain_after_turn, true);
  assert.equal(child.delta.tool_calls[0].function.name, "shell_start");
});
