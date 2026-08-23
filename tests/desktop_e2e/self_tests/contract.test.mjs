import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import { EXECUTION_CLASSIFICATIONS, classifyExecution } from "../core/execution.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));

test("versioned result schema and classifier expose the same closed classification domain", async () => {
  const schema = JSON.parse(await readFile(path.join(here, "..", "contracts", "execution-result.v1.schema.json"), "utf8"));
  assert.deepEqual(schema.properties.classification.enum, EXECUTION_CLASSIFICATIONS);
  assert.equal(schema.additionalProperties, false);
  for (const required of ["schema_version", "execution_id", "scenario_id", "started_at", "finished_at", "elapsed_ms", "classification", "inputs", "reasons"]) {
    assert.equal(schema.required.includes(required), true, `${required} must remain required`);
  }

  const result = classifyExecution({ preflight: "pass", acquisition: "pass", oracle: "pass", manual: "not_required", cleanup: "pass" });
  assert.equal(result.schema_version, schema.properties.schema_version.const);
});
