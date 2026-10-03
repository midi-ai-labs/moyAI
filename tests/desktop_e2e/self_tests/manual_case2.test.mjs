import assert from "node:assert/strict";
import crypto from "node:crypto";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { createManualCase2Scenario, normalizeManualCase2Options, manualCase2ImageDiagnostics,
  manualCase2FixtureFailures, manualCase2CaptureAclValid } from "../scenarios/manual_case2.mjs";
import { manualLivePrompt, manualLiveTranscriptResult } from "../drivers/manual_live_session.mjs";

const RAW = { provider_base_url: "http://provider.invalid/v1", model: "exact-model", python_executable: process.execPath,
  image_source: process.execPath.replace(/\.[^.]+$/, "") + ".jpg" };
const bytes = Buffer.from("fixture image bytes");
const data = `data:image/jpeg;base64,${bytes.toString("base64")}`;
const expected = { valid_data_url: true, mime_type: "image/jpeg", size_bytes: bytes.length,
  sha256: crypto.createHash("sha256").update(bytes).digest("hex") };

test("Case2 uses the canonical image prompt, explicit source and common manual lifecycle", async () => {
  const spec = await readFile(new URL("../../manual_ST/case2/spec.md", import.meta.url), "utf8");
  assert.match(manualLivePrompt(spec), /^添付画像 \[Image #1\]/);
  assert.match(manualLivePrompt(spec), /scenario_contract\.md/);
  const scenario = createManualCase2Scenario(RAW);
  assert.equal(scenario.id, "manual.case2"); assert.equal(scenario.manualGate, "pending");
  assert.equal(scenario.databaseRequired, true); assert.deepEqual(scenario.environment, {});
  for (const change of [{ image_source: undefined }, { image_source: "relative.jpg" }, { image_source: process.execPath },
    { supports_images: true }, { vision_capable: true }]) assert.throws(() => normalizeManualCase2Options({ ...RAW, ...change }));
});

test("Case2 prepared request evidence decodes actual user images in both current API modes", () => {
  for (const body of [{ model: "exact-model", messages: [{ role: "user", content: [{ type: "image_url", image_url: { url: data } }] }] },
    { model: "exact-model", input: [{ role: "user", content: [{ type: "input_image", image_url: data }] }] }]) {
    assert.deepEqual(manualCase2ImageDiagnostics(body), { model: "exact-model", image_count: 1, images: [expected] });
  }
  assert.deepEqual(manualCase2ImageDiagnostics({ model: "m", messages: [{ role: "assistant", content: [{ type: "image_url", image_url: { url: data } }] },
    { role: "user", content: [{ type: "text", text: data }] }], tools: [{ type: "image_url", image_url: { url: data } }] }), { model: "m", image_count: 0, images: [] });
  for (const url of ["https://example.invalid/image.jpg", "data:image/jpeg;base64,%%%", "data:image/jpeg;base64,YQ", "data:image/jpeg;base64,"]) {
    assert.deepEqual(manualCase2ImageDiagnostics({ messages: [{ role: "user", content: [{ type: "image_url", image_url: { url } }] }] }).images, [{ valid_data_url: false }]);
  }
});

test("Case2 keeps public contract and attached source immutable without treating generated outputs as fixture edits", () => {
  const before = ["scenario_contract.md", "reference-image.jpg"].map(name => ({ path: name, sha256: "same", size_bytes: 4 }));
  assert.deepEqual(manualCase2FixtureFailures(before, [...before, { path: "space_invader.py", sha256: "new" }], "reference-image.jpg"), []);
  assert.deepEqual(manualCase2FixtureFailures(before, [before[0], { ...before[1], sha256: "changed" }], "reference-image.jpg"), ["input-fixture-changed:reference-image.jpg"]);
  assert.deepEqual(manualCase2FixtureFailures(before, [{ ...before[0], symbolic_link: true }], "reference-image.jpg"), ["input-fixture-changed:scenario_contract.md", "input-fixture-changed:reference-image.jpg"]);
});

test("Case2 prepare copies exact inputs and activates task-local capture without touching the image source", async () => {
  const parent = fileURLToPath(new URL("../../../../project_sandbox/manual-case2-adapter-self-tests/", import.meta.url));
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(path.join(parent, "fixture-"));
  const paths = Object.fromEntries(["workspace", "config", "data", "prefs", "webview", "logs"].map(name => [name, path.join(root, name)]));
  await Promise.all(Object.values(paths).map(directory => mkdir(directory)));
  paths.config_file = path.join(paths.config, "config.toml"); paths.prefs_file = path.join(paths.prefs, "desktop.toml");
  const image = path.join(root, "operator image.jpg");
  const records = [];
  const sink = { record: async (...args) => records.push(args), writeBytes: async (name, content) => ({ path: name, size_bytes: content.length }) };
  try {
    await writeFile(image, bytes);
    const captureCalls = [];
    const scenario = createManualCase2Scenario({ ...RAW, image_source: image }, { prepareCapture: async input => captureCalls.push(input.captureDirectory) });
    await scenario.prepare({ context: { root, paths }, sink, phase: "prepared" });
    assert.deepEqual((await readdir(paths.workspace)).sort(), ["reference-image.jpg", "scenario_contract.md"]);
    assert.deepEqual(await readFile(path.join(paths.workspace, "scenario_contract.md")), await readFile(new URL("../../manual_ST/case2/scenario_contract.md", import.meta.url)));
    assert.deepEqual(await readFile(path.join(paths.workspace, "reference-image.jpg")), bytes);
    assert.deepEqual(await readFile(image), bytes);
    assert.deepEqual(await readdir(paths.data), []);
    assert.deepEqual(scenario.environment, { MOYAI_HTTP_REQUEST_CAPTURE_DIR: path.join(paths.logs, "case2-provider-requests") });
    assert.deepEqual(captureCalls, [path.join(paths.logs, "case2-provider-requests")]);
    const evidence = records.find(row => row[0] === "case2-input")[1];
    assert.equal(evidence.observation_timeout_ms, 30 * 60 * 1000);
    assert.equal(evidence.image.sha256, expected.sha256); assert.equal(evidence.image.source_filename, "operator image.jpg");
    assert.equal(evidence.model_capability, "unknown_unless_separate_provider_metadata_proves_support");
    assert.equal(evidence.provider_owned, false);
    await assert.rejects(createManualCase2Scenario({ ...RAW, image_source: image }).prepare({ context: { root, paths }, sink, phase: "prepared" }), error => error.code === "case2-not-fresh");
  } finally {
    assert.equal(path.dirname(path.resolve(root)).toLowerCase(), path.resolve(parent).toLowerCase());
    await rm(root, { recursive: true });
  }
});

test("Case2 private ACL verdict requires the current account, exact fresh capture path and inherited file protection", () => {
  const root = path.resolve("execution-root");
  const capture = path.join(root, "logs", "case2-provider-requests");
  const sid = "S-1-5-21-1-2-3-1001";
  const rules = [sid, "S-1-5-18", "S-1-5-32-544"].map(value => ({ sid: value, type: "Allow", full_control: true,
    inherited: false, inheritance: "ContainerInherit, ObjectInherit", propagation: "None" }));
  const value = { schema_version: "desktop-e2e.manual-case2-capture-acl.v1", execution_root: root, capture_directory: capture,
    current_account: "HOST\\user", current_sid: sid, allowed_sids: rules.map(row => row.sid),
    directory_acl: { owner_sid: sid, protected: true, rules }, inherited_file_acl: { owner_sid: sid, rules: rules.map(row => ({ ...row, inherited: true })) },
    parent_acls_unchanged: true, probe_removed: true };
  assert.equal(manualCase2CaptureAclValid(value, root, capture), true);
  for (const changed of [null, { ...value, current_sid: "S-1-5-18" }, { ...value, parent_acls_unchanged: false },
    { ...value, allowed_sids: [...value.allowed_sids, "S-1-1-0"] }, { ...value, directory_acl: { ...value.directory_acl, protected: false } },
    { ...value, inherited_file_acl: { ...value.inherited_file_acl, owner_sid: "S-1-5-21-4-5-6-1002" } },
    { ...value, directory_acl: { ...value.directory_acl, rules: rules.map((row, index) => index === 0 ? { ...row, sid: "S-1-1-0" } : row) } }]) {
    assert.equal(manualCase2CaptureAclValid(changed, root, capture), false);
  }
  for (const outside of [root, path.join(root, "workspace", "case2-provider-requests"), path.resolve(root, "..", "case2-provider-requests")]) {
    assert.equal(manualCase2CaptureAclValid({ ...value, capture_directory: outside }, root, outside), false);
  }
});

test("Case2 Desktop transcript quotation preserves its projected image path and bytes", () => {
  const session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
  const body = "canonical prompt\nC:\\workspace\\reference-image.jpg (19 bytes)";
  const markdown = `# Space Invader\n\n> ${body.replaceAll("\n", "\n> ")}\n\nDone.\n\n<details><summary>実行情報</summary>\n\n- Session: \`${session}\`\n\n</details>\n`;
  assert.equal(manualLiveTranscriptResult(markdown, { sessionId: session, prompt: body }).pass, true);
  assert.equal(manualLiveTranscriptResult(markdown, { sessionId: session, prompt: "canonical prompt" }).pass, false);
});
