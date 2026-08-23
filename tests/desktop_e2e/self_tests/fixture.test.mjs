import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";

import { prepareDesktopFixture, validFixtureSentinelName } from "../scenarios/fixture.mjs";

test("fixture sentinel accepts scenario names with ordinary lowercase extensions", () => {
  assert.equal(validFixtureSentinelName("E2E_SHELL_BASELINE.txt"), true);
  assert.equal(validFixtureSentinelName("E2E_PROVIDER_RESTART.txt"), true);
});

test("fixture sentinel remains one safe non-reserved file name", () => {
  for (const value of ["../escape.txt", "nested/file.txt", "nested\\file.txt", "CON.txt", "LPT1.log", "trailing.", "x"]) {
    assert.equal(validFixtureSentinelName(value), false, value);
  }
});

test("absent config fixture writes workspace and preferences but preserves a missing config path", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "moyai-desktop-e2e-fixture-"));
  const context = {
    paths: {
      workspace: path.join(root, "workspace"),
      config_file: path.join(root, "config", "config.toml"),
      data: path.join(root, "data"),
      prefs_file: path.join(root, "prefs", "desktop.toml"),
      webview: path.join(root, "webview"),
    },
  };
  const events = [];
  const sink = { record: async (...args) => events.push(args) };
  const { mkdir } = await import("node:fs/promises");
  await Promise.all(Object.values(context.paths)
    .filter((candidate) => candidate !== context.paths.config_file && candidate !== context.paths.prefs_file)
    .map((candidate) => mkdir(candidate, { recursive: true })));
  await mkdir(path.dirname(context.paths.prefs_file), { recursive: true });
  try {
    await prepareDesktopFixture({
      context,
      sink,
      phase: "prepared",
      owner: "self-test:fixture",
      configMode: "absent",
      sentinelName: "E2E_INITIAL_SETUP.txt",
    });
    await assert.rejects(stat(context.paths.config_file), (error) => error?.code === "ENOENT");
    assert.match(await readFile(context.paths.prefs_file, "utf8"), /last_workspace/);
    assert.equal(events.length, 1);
    assert.equal(events[0][1].config_mode, "absent");
    assert.equal(events[0][1].identities.config, null);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("fixture config mode rejects ambiguous present and absent ownership", async () => {
  const base = {
    context: { paths: { workspace: "unused", config_file: "unused", data: "unused", prefs_file: "unused", webview: "unused" } },
    sink: { record: async () => undefined },
    phase: "prepared",
    owner: "self-test:fixture",
  };
  await assert.rejects(prepareDesktopFixture(base), /present fixture requires exactly one config source/);
  await assert.rejects(
    prepareDesktopFixture({ ...base, configMode: "absent", configText: "[model]" }),
    /absent fixture cannot provide a config source/,
  );
  await assert.rejects(prepareDesktopFixture({ ...base, configMode: "legacy" }), /unsupported fixture config mode/);
});
