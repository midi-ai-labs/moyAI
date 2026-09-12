import assert from "node:assert/strict";
import test from "node:test";
import { mcpPaginationPlan, mcpPaginationProviderOptions, mcpHistoryPageMatches, mcpPaginationJobSetMatches, mcpPaginationOutcome } from "../scenarios/mcp_history_pagination.mjs";
import { createMcpHistoryPaginationScenario } from "../scenarios/mcp_receiver_live.mjs";
import { ScriptedProvider } from "../drivers/scripted_provider.mjs";

const control = disabled => ({ count: 1, disabled, ariaDisabled: String(disabled) });
const page = (ids, number) => ({ dialogCount: 1, page: `execution:${number === 1 ? 0 : 20}`, label: `${number}ページ`, detailOwner: "execution:", error: "", selected: 0,
  previous: control(number === 1), next: control(number === 2), export: control(true), rows: ids.map(id => ({ id, state: "interrupted", pressed: "false" })) });

test("pagination is an independent scenario sharing the real receiver lifecycle", () => {
  const scenario = createMcpHistoryPaginationScenario();
  assert.equal(scenario.id, "mcp.history-pagination");
  assert.equal(scenario.manualGate, "pending");
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) assert.equal(typeof scenario[method], "function");
  assert.equal("launch" in scenario, false);
});
test("21 requests use distinct grant/request identities and an existing bounded ordinary provider", () => {
  const plan = mcpPaginationPlan();
  assert.equal(plan.length, 21);
  for (const key of ["rootTaskId", "requestKey", "prompt"]) assert.equal(new Set(plan.map(row => row[key])).size, 21);
  const options = mcpPaginationProviderOptions();
  assert.equal(options.turns.length, 21);
  assert.equal(options.responseBehavior, "hold_until_release");
  assert.equal("script" in options, false);
  assert.doesNotThrow(() => new ScriptedProvider(options));
});
test("page oracle rejects duplicate/wrong order, stale selection, incorrect disabled state or failed jobs", () => {
  const ids = Array.from({ length: 20 }, (_, index) => `job-${index}`);
  assert.equal(mcpHistoryPageMatches(page(ids, 1), ids, 1), true);
  for (const mutate of [p => { p.dialogCount = 2; }, p => { p.page = "execution:20"; }, p => { p.label = "2ページ"; },
    p => { p.detailOwner = "execution:old"; }, p => { p.selected = 1; }, p => { p.error = "failed"; },
    p => { p.previous.disabled = false; }, p => { p.previous.ariaDisabled = "false"; },
    p => { p.next.disabled = true; }, p => { p.export.disabled = false; }, p => { p.export.ariaDisabled = "false"; },
    p => { p.rows[0].id = p.rows[1].id; }, p => { p.rows.reverse(); }, p => { p.rows[0].state = "failed"; }, p => { p.rows[0].pressed = "true"; }]) {
    const value = page(ids, 1); mutate(value); assert.equal(mcpHistoryPageMatches(value, ids, 1), false);
  }
  assert.equal(mcpHistoryPageMatches(page(["last"], 2), ["last"], 2), true);
  assert.equal(mcpHistoryPageMatches(page(["last"], 2), ["wrong"], 2), false);
});
test("durable jobs must match every request identity and settle before pagination", () => {
  const jobs = mcpPaginationPlan().map((row, index) => ({ job_id: String(index).padStart(26, "0"), request_key: row.requestKey, root_task_id: row.rootTaskId, state: "interrupted" }));
  assert.equal(mcpPaginationJobSetMatches(jobs), true);
  for (const mutate of [j => j.pop(), j => { j[1].job_id = j[0].job_id; }, j => { j[0].state = "cancelling"; },
    j => { j[0].request_key = j[1].request_key; }, j => { j[0].root_task_id = "other"; }]) {
    const value = structuredClone(jobs); mutate(value); assert.equal(mcpPaginationJobSetMatches(value), false);
  }
});
test("automated pagination does not claim visual or model-generation success and rejects page errors", () => {
  assert.deepEqual(mcpPaginationOutcome([]), { acquisition: "pass", oracle: "pass", manual: "pending" });
  assert.throws(() => mcpPaginationOutcome(["browser exception"]), error => error.code === "mcp-history-pagination-mismatch");
});
