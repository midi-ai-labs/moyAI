import assert from "node:assert/strict";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { access, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { desktopFixtureRoot, desktopLaunchEnvironment, desktopOwnersMatch, normalizeDesktopIsolation, prepareDesktopFixtureEnvironment } from "../core/desktop_isolation.mjs";
import { createCompanionContext, createDesktopRunContext } from "../core/run_context.mjs";
import { EvidenceSink } from "../core/evidence_sink.mjs";
import { parseArguments } from "../run_scenario.mjs";
import { executeSuiteCases, parseSuiteArguments } from "../run_suite.mjs";
import { normalizeScenarioEnvironment, WindowsTauriHost } from "../drivers/windows_tauri_host.mjs";

function contextAt(root, mode = "fixture") {
  return { root, desktopIsolation: mode, paths: {
    config: path.join(root, "config"), config_file: path.join(root, "config/config.toml"),
    data: path.join(root, "data"), prefs: path.join(root, "prefs"), prefs_file: path.join(root, "prefs/desktop.toml"),
    webview: path.join(root, "webview"),
  } };
}

test("isolation is explicit; invalid values and scenario-owned roots are rejected", () => {
  assert.equal(normalizeDesktopIsolation(), "user-wide");
  assert.deepEqual(parseArguments([]), {});
  assert.equal(parseArguments(["--desktop-isolation", "fixture"])["desktop-isolation"], "fixture");
  assert.equal(parseSuiteArguments(["--desktop-isolation", "fixture"]).desktopIsolation, "fixture");
  for (const value of ["", "disabled", "Fixture", null]) assert.throws(() => normalizeDesktopIsolation(value));
  assert.throws(() => parseArguments(["--desktop-isolation", "shared"]), /isolation/);
  assert.throws(() => parseSuiteArguments(["--desktop-isolation", "shared"]), /isolation/);
  assert.throws(() => normalizeScenarioEnvironment({ MOYAI_DESKTOP_E2E_ROOT: "C:\\other" }), /not allowed/);
});

test("suite forwards explicit fixture mode to each case and leaves default arguments intact", async () => {
  const calls = [];
  const common = { ids: ["shell.baseline"], binary: "binary", artifactParent: "artifacts",
    execute: async args => { calls.push(args); return {}; }, readResult: async () => ({ scenario: "shell.baseline", automation: "pass" }) };
  await executeSuiteCases(common);
  await executeSuiteCases({ ...common, desktopIsolation: "fixture" });
  assert.deepEqual(calls[1], [...calls[0], "--desktop-isolation", "fixture"]);
  assert.equal(calls[0].includes("--desktop-isolation"), false);
});

test("preserved Desktop ownership includes start time and executable, not just PID", () => {
  const owner = (pid, stamp = "123", executable = "C:\\product\\moyai-desktop.exe") => ({ process_id: pid, process_start_time_utc_ticks: stamp, executable_path: executable });
  const existing = owner(10), a = owner(20), b = owner(30);
  assert.equal(desktopOwnersMatch([existing], [a, existing], [a]), true);
  assert.equal(desktopOwnersMatch([existing, a], [b, a, existing], [b]), true);
  assert.equal(desktopOwnersMatch([existing, a], [a, existing]), true);
  assert.equal(desktopOwnersMatch([existing], [owner(10, "456")] ), false);
  assert.equal(desktopOwnersMatch([existing], [owner(10, "123", "C:\\other\\moyai-desktop.exe")] ), false);
  assert.equal(desktopOwnersMatch([existing], []), false);
  assert.equal(desktopOwnersMatch([existing], [existing, b]), false);
  assert.equal(desktopOwnersMatch([existing], [existing, existing], [existing]), false);
});

test("launch environment discards inherited fixture identities and owns all virtual PC paths", () => {
  const context = contextAt(path.resolve("fixture-parent/desktop-b"));
  const inherited = { Path: "preserved", moyai_desktop_e2e_root: "old-root", MOYAI_DESKTOP_E2E_RUNNER: "old-runner", MoYaI_TeSt_ReSoUrCe_ReGiStRy: "old-registry", temp: "old-temp", TMP: "old-temp" };
  const temp = path.join(context.root, "temp");
  const env = desktopLaunchEnvironment({ context, inherited, processTemp: temp, scenarioEnvironment: { MOYAI_DESKTOP_E2E_RUNNER: "explicit-runner" } });
  assert.equal(env.MOYAI_DESKTOP_E2E_ROOT, context.root);
  assert.equal(env.MOYAI_TEST_RESOURCE_REGISTRY, path.join(context.root, "resource-admission"));
  assert.equal(env.MOYAI_CONFIG_PATH, context.paths.config_file);
  assert.equal(env.MOYAI_DESKTOP_PREFS_PATH, context.paths.prefs_file);
  assert.equal(env.MOYAI_DATA_DIR, context.paths.data);
  assert.equal(env.WEBVIEW2_USER_DATA_FOLDER, context.paths.webview);
  assert.equal(env.MOYAI_DESKTOP_E2E_RUNNER, "explicit-runner");
  assert.deepEqual([env.TEMP, env.TMP, env.TMPDIR], [temp, temp, temp]);
  assert.equal(env.temp, undefined);
  assert.equal(env.moyai_desktop_e2e_root, undefined);
  assert.equal(env.Path, "preserved");
  assert.throws(() => desktopLaunchEnvironment({ context, inherited, processTemp: temp, scenarioEnvironment: { MOYAI_TEST_RESOURCE_REGISTRY: "other" } }), /conflicts/);
  const normal = desktopLaunchEnvironment({ context: { ...context, desktopIsolation: "user-wide" }, inherited, processTemp: temp });
  assert.equal(normal.MOYAI_DESKTOP_E2E_ROOT, undefined);
  assert.equal(normal.MOYAI_TEST_RESOURCE_REGISTRY, undefined);
  assert.equal(normal.MOYAI_DESKTOP_E2E_RUNNER, undefined);
  const legacy = desktopLaunchEnvironment({ context: { ...context, desktopIsolation: "user-wide" }, inherited, processTemp: temp, scenarioEnvironment: { MOYAI_TEST_RESOURCE_REGISTRY: "explicit-legacy" } });
  assert.equal(legacy.MOYAI_TEST_RESOURCE_REGISTRY, "explicit-legacy");
});

test("virtual PC paths cannot mix roots or escape their execution", () => {
  const parent = path.resolve("fixture-parent"), context = contextAt(path.join(parent, "desktop-b"));
  context.root = parent;
  assert.equal(desktopFixtureRoot(context), path.join(parent, "desktop-b"));
  assert.throws(() => desktopFixtureRoot({ ...context, paths: { ...context.paths, data: path.join(parent, "data") } }), /data/);
  assert.throws(() => desktopFixtureRoot({ ...contextAt(path.resolve("outside")), root: parent }), /escaped/);
});

test("fixture preparation requires physical directories and reuses only the same registry and temp", async t => {
  const root = await mkdtemp(path.join(os.tmpdir(), "moyai-isolation-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const context = contextAt(root);
  for (const key of ["config", "data", "prefs", "webview"]) await mkdir(context.paths[key]);
  const first = await prepareDesktopFixtureEnvironment(context);
  await writeFile(path.join(first.registry, "retained"), "same-pc");
  assert.deepEqual(await prepareDesktopFixtureEnvironment(context), first);
  assert.equal(await readFile(path.join(first.registry, "retained"), "utf8"), "same-pc");
  const bad = path.join(root, "bad"), external = path.join(root, "external");
  await mkdir(bad); await mkdir(external);
  const badContext = { ...contextAt(bad), root };
  for (const key of ["data", "prefs", "webview"]) await mkdir(badContext.paths[key]);
  await symlink(external, badContext.paths.config, process.platform === "win32" ? "junction" : "dir");
  await assert.rejects(() => prepareDesktopFixtureEnvironment(badContext), /physical/);
  await assert.rejects(() => access(path.join(bad, "resource-admission")), /ENOENT/);
});

test("companion contexts stay in one manifest and scoped evidence seals in the parent inventory", async t => {
  const parent = await mkdtemp(path.join(os.tmpdir(), "moyai-companion-"));
  t.after(() => rm(parent, { recursive: true, force: true }));
  const binary = path.join(parent, "fixture.exe"), harness = path.join(parent, "harness");
  await writeFile(binary, "test"); await mkdir(harness); await writeFile(path.join(harness, "test.mjs"), "test");
  const { context, sink } = await createDesktopRunContext({ artifactParent: path.join(parent, "artifacts"), binary, harnessRoot: harness, executionId: "e2e-20260913-isolation", scenarioId: "shell.baseline", desktopIsolation: "fixture" });
  assert.equal(context.manifest.desktop_isolation, "fixture");
  const child = await createCompanionContext(context, "desktop-b");
  assert.equal(child.root, context.root);
  assert.equal(child.manifest, context.manifest);
  assert.equal(desktopFixtureRoot(child), path.join(context.root, "desktop-b"));
  assert.notEqual(child.paths.logs, context.paths.logs);
  await assert.rejects(() => createCompanionContext(context, "desktop-b"), /EEXIST/);
  await assert.rejects(() => createCompanionContext(context, "../outside"), /invalid/);
  await assert.rejects(() => createCompanionContext({ ...context, desktopIsolation: "user-wide" }, "desktop-c"), /requires fixture/);
  await sink.writeJson("owners/desktop.json", { pc: "a" });
  const scoped = sink.scope("companions/desktop-b");
  const stored = await scoped.writeJson("owners/desktop.json", { pc: "b" });
  assert.equal(JSON.parse(await readFile(path.join(scoped.root, stored.relative_path), "utf8")).pc, "b");
  await sink.record("parent-event", {}); await scoped.record("child-event", {});
  const seal = await sink.seal({ classification: "manual_pending" });
  assert.equal(seal.event_count, 2);
  assert.equal(seal.files.some(row => row.relative_path === "companions/desktop-b/owners/desktop.json"), true);
  await assert.rejects(() => scoped.writeJson("late.json", {}), /sealed/);
  assert.throws(() => sink.scope("../outside"), /unsafe/);
});

test("companion launch cannot acquire an independent host without an attached parent", async () => {
  const host = new WindowsTauriHost();
  await assert.rejects(() => host.openCompanion({ context: contextAt(path.resolve("fixture")), sink: {}, scenario: {} }), /attached fixture root host/);
});
