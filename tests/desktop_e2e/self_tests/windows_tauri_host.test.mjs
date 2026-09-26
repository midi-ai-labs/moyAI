import assert from "node:assert/strict";
import test from "node:test";

import {
  normalizeScenarioEnvironment,
  desktopLaunchArguments,
  releaseResourcesThenAuditClosedStore,
  selectActiveCleanupDriver,
  WindowsTauriHost,
} from "../drivers/windows_tauri_host.mjs";

test("restart fixture mutation is rejected before acquiring any process when its callback is invalid", async () => {
  let invoked = false;
  const host = new WindowsTauriHost();
  await assert.rejects(host.restart({ beforeRelaunch: true }), /beforeRelaunch must be a function/);
  const context = { root: "C:\\execution", binary: "C:\\desktop.exe", desktopIsolation: "fixture",
    paths: { logs: "C:\\execution\\logs", workspace: "C:\\execution\\workspace", config: "C:\\execution\\config",
      data: "C:\\execution\\data", prefs: "C:\\execution\\prefs", webview: "C:\\execution\\webview" } };
  await assert.rejects(host.restart({ context, driver: null, beforeRelaunch: async () => { invoked = true; } }), /attached live generation/);
  assert.equal(invoked, false);
});

test("cold and duplicate activation use the same bounded fixture argv without shell quoting", () => {
  const context = { paths: { workspace: "C:\\fixture\\workspace" } };
  const config = "C:\\fixture\\workspace\\team config 日本語.toml";
  assert.deepEqual(desktopLaunchArguments(context), ["--dir", context.paths.workspace]);
  assert.deepEqual(desktopLaunchArguments(context, config), ["--dir", context.paths.workspace, "--join-config", config]);
  for (const value of ["relative.toml", "C:\\fixture\\workspace", "C:\\fixture\\workspace2\\other.toml", "C:\\fixture\\workspace\\..\\other.toml", config + "\0"]) {
    assert.throws(() => desktopLaunchArguments(context, value), /joinConfigPath/);
  }
});

test("scenario environment permits only bounded product config overrides", () => {
  assert.deepEqual(normalizeScenarioEnvironment({
    MOYAI_BASE_URL: "http://127.0.0.1:43111",
    MOYAI_DOCLING_ENABLED: "true",
  }), {
    MOYAI_BASE_URL: "http://127.0.0.1:43111",
    MOYAI_DOCLING_ENABLED: "true",
  });
  assert.throws(() => normalizeScenarioEnvironment([]), /must be an object/);
  assert.throws(() => normalizeScenarioEnvironment({ PATH: "C:\\bin" }), /not allowed/);
  assert.throws(() => normalizeScenarioEnvironment({ MOYAI_CONFIG_PATH: "C:\\other.toml" }), /not allowed/);
  assert.throws(() => normalizeScenarioEnvironment({ MOYAI_BASE_URL: "bad\0value" }), /value is invalid/);
});

test("cleanup never falls back to the closed generation-one driver after restart begins", () => {
  const generationOne = { generation: 1 };
  const generationTwo = { generation: 2 };
  assert.equal(selectActiveCleanupDriver(generationTwo, generationOne, { restartBegan: true }), generationTwo);
  assert.equal(selectActiveCleanupDriver(null, generationOne, { restartBegan: true }), null);
  assert.equal(selectActiveCleanupDriver(null, generationOne, { restartBegan: false }), generationOne);
});

test("scenario resources close after settlement attempts and closed-store audit runs only at exact zero", async () => {
  const order = [];
  const common = {
    context: { root: "C:\\execution", paths: { database: "C:\\execution\\data\\moyai.sqlite3" } },
    scenario: { databaseRequired: true },
    inputs: { acquisition: "pass" },
    releaseScenarioResources: async () => {
      order.push("scenario-release");
      return { input: "pass", resources: [] };
    },
    auditSqlite: async () => {
      order.push("sqlite-audit");
      return { pass: true };
    },
  };
  const settled = await releaseResourcesThenAuditClosedStore({
    ...common,
    desktopExited: true,
    profileRows: [],
  });
  assert.deepEqual(order, ["scenario-release", "sqlite-audit"]);
  assert.equal(settled.sqlite.pass, true);

  order.length = 0;
  const unsettled = await releaseResourcesThenAuditClosedStore({
    ...common,
    desktopExited: false,
    profileRows: [{ process_id: 99 }],
  });
  assert.deepEqual(order, ["scenario-release"]);
  assert.equal(unsettled.scenarioQuiesce.input, "pass");
  assert.equal(unsettled.sqlite.pass, false);
  assert.equal(unsettled.sqlite.skipped_reason, "desktop-or-profile-owner-not-zero");

  order.length = 0;
  const unsampled = await releaseResourcesThenAuditClosedStore({
    ...common,
    desktopExited: true,
    profileRows: null,
  });
  assert.deepEqual(order, ["scenario-release"]);
  assert.equal(unsampled.sqlite.pass, false);
  assert.equal(unsampled.sqlite.skipped_reason, "profile-owner-not-sampled");

  order.length = 0;
  const resourceFailure = await releaseResourcesThenAuditClosedStore({
    ...common,
    desktopExited: true,
    profileRows: [],
    releaseScenarioResources: async () => {
      order.push("scenario-release-failed");
      return { input: "fail", resources: [] };
    },
  });
  assert.deepEqual(order, ["scenario-release-failed"]);
  assert.equal(resourceFailure.sqlite.pass, false);
  assert.equal(resourceFailure.sqlite.skipped_reason, "scenario-resources-not-quiesced");
});
