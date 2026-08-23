import assert from "node:assert/strict";
import test from "node:test";

import { validFixtureSentinelName } from "../scenarios/fixture.mjs";

test("fixture sentinel accepts scenario names with ordinary lowercase extensions", () => {
  assert.equal(validFixtureSentinelName("E2E_SHELL_BASELINE.txt"), true);
  assert.equal(validFixtureSentinelName("E2E_PROVIDER_RESTART.txt"), true);
});

test("fixture sentinel remains one safe non-reserved file name", () => {
  for (const value of ["../escape.txt", "nested/file.txt", "nested\\file.txt", "CON.txt", "LPT1.log", "trailing.", "x"]) {
    assert.equal(validFixtureSentinelName(value), false, value);
  }
});
