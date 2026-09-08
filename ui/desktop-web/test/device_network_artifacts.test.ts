import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { deviceCanStopJob, editDeviceNetworkField } from "../src/device_network_state.ts";
import { deviceCanInspectArtifacts, deviceCanExportArtifacts, inspectDeviceArtifacts, exportDeviceArtifacts,
  renderDeviceArtifacts, type RemoteArtifactManifest } from "../src/device_network_artifacts.ts";
import { deviceUiFixture } from "./device_network_fixture.ts";

const manifest = (): RemoteArtifactManifest => ({ job_id: "job-20", version: "a".repeat(64), files: [
  { path: "更新後.txt", kind: "move", from_path: "<更新前>.txt", base_sha256: "b".repeat(64), sha256: "c".repeat(64), byte_length: 12 },
] });
async function withContext(invoke: (name: string, args: Record<string, unknown>) => Promise<unknown>,
  run: (value: { context: ActionContext; local: ReturnType<typeof deviceUiFixture>; view: { overlay: string } }) => Promise<void>) {
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  const local = deviceUiFixture();
  local.jobs.outgoing[0].state = "completed";
  local.jobs.outgoing[0].can_stop = false;
  const view = { overlay: "hub" };
  const context = { uiState: { deviceNetwork: local }, getViewState: () => view, rerender() {} } as unknown as ActionContext;
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke } } });
  try { await run({ context, local, view }); }
  finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
}

test("artifact inspection and export use the captured remote reference and reviewed version without sending content or destination", async () => {
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  await withContext(async (name, args) => {
    calls.push({ name, args });
    return name === "device_network_artifacts" ? { reference_id: "reference-a", manifest: manifest() }
      : { reference_id: "reference-a", job_id: "job-20", version: manifest().version, directory: "C:/selected/new-artifacts" };
  }, async ({ context, local }) => {
    editDeviceNetworkField(local, "port", "7444", false);
    await inspectDeviceArtifacts(context, "reference-a");
    assert.equal(deviceCanExportArtifacts(local, "reference-a"), true);
    let html = renderDeviceArtifacts(local, "reference-a");
    assert.match(html, /移動元: &lt;更新前&gt;\.txt/);
    assert.match(html, /選択した場所に新しいフォルダ/);
    assert.match(html, new RegExp(manifest().version));
    await exportDeviceArtifacts(context, "reference-a");
    assert.deepEqual(calls, [
      { name: "device_network_artifacts", args: { referenceId: "reference-a" } },
      { name: "device_network_export_artifacts", args: { referenceId: "reference-a", version: manifest().version } },
    ]);
    html = renderDeviceArtifacts(local, "reference-a");
    assert.match(html, /C:\/selected\/new-artifacts/);
    assert.equal(local.port, "7444");
    assert.equal(local.dirty, true);
  });
});

test("running jobs have no artifact action and an empty terminal manifest is a valid non-exportable result", async () => {
  await withContext(async () => ({ reference_id: "reference-a", manifest: { ...manifest(), files: [] } }), async ({ context, local }) => {
    local.jobs.outgoing[0].state = "awaiting_approval";
    assert.equal(deviceCanInspectArtifacts(local, "reference-a"), false);
    assert.equal(renderDeviceArtifacts(local, "reference-a"), "");
    local.jobs.outgoing[0].state = "interrupted";
    await inspectDeviceArtifacts(context, "reference-a");
    assert.equal(deviceCanExportArtifacts(local, "reference-a"), false);
    assert.match(renderDeviceArtifacts(local, "reference-a"), /書き出せるファイル変更はありません/);
  });
});

test("a late manifest cannot be adopted by a replaced job and pending inspection leaves incoming Stop available", async () => {
  let release!: (value: unknown) => void;
  const deferred = new Promise(resolve => { release = resolve; });
  let calls = 0;
  await withContext(async () => { ++calls; return deferred; }, async ({ context, local }) => {
    const first = inspectDeviceArtifacts(context, "reference-a");
    await inspectDeviceArtifacts(context, "reference-a");
    assert.equal(calls, 1);
    assert.equal(deviceCanStopJob(local, "incoming:job-00"), true);
    local.jobs.outgoing[0].job_id = "different-job";
    release({ reference_id: "reference-a", manifest: manifest() });
    await first;
    assert.deepEqual(local.artifacts, {});
    assert.equal(local.artifactPending, null);
  });
});

test("cancelled destination selection keeps the reviewed version and never displays a saved receipt", async () => {
  await withContext(async name => name === "device_network_artifacts" ? { reference_id: "reference-a", manifest: manifest() } : null,
    async ({ context, local }) => {
      await inspectDeviceArtifacts(context, "reference-a");
      await exportDeviceArtifacts(context, "reference-a");
      assert.equal(local.artifacts["reference-a"].manifest.version, manifest().version);
      assert.equal(local.artifacts["reference-a"].receipt, null);
      assert.match(local.artifactNotices["reference-a"], /キャンセル/);
      assert.equal(deviceCanExportArtifacts(local, "reference-a"), true);
    });
});

test("a receipt for another reference or version is not reported as a successful export", async () => {
  await withContext(async name => name === "device_network_artifacts" ? { reference_id: "reference-a", manifest: manifest() }
    : { reference_id: "another-reference", job_id: "job-20", version: "f".repeat(64), directory: "C:/wrong" },
  async ({ context, local }) => {
    await inspectDeviceArtifacts(context, "reference-a");
    await exportDeviceArtifacts(context, "reference-a");
    assert.equal(local.artifacts["reference-a"].receipt, null);
    assert.ok(local.artifactErrors["reference-a"]);
    assert.doesNotMatch(renderDeviceArtifacts(local, "reference-a"), /C:\/wrong|作成して書き出しました/);
  });
});

test("retired remote authority leaves the reviewed manifest visible and explains that saved results remain", async () => {
  await withContext(async name => {
    if (name === "device_network_artifacts") return { reference_id: "reference-a", manifest: manifest() };
    throw "authority_retired";
  }, async ({ context, local }) => {
    await inspectDeviceArtifacts(context, "reference-a");
    await exportDeviceArtifacts(context, "reference-a");
    assert.equal(local.artifacts["reference-a"].manifest.version, manifest().version);
    assert.match(local.artifactErrors["reference-a"], /保持期間が終了/);
    assert.match(local.artifactErrors["reference-a"], /保存済みの結果/);
    assert.equal(local.artifactPending, null);
  });
});
