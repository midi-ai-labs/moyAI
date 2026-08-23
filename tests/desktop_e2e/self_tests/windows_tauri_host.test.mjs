import assert from "node:assert/strict";
import test from "node:test";

import {
  normalizeScenarioEnvironment,
  releaseResourcesThenAuditClosedStore,
  selectActiveCleanupDriver,
} from "../drivers/windows_tauri_host.mjs";

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
