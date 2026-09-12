import assert from "node:assert/strict";
import test from "node:test";
import { createMcpReceiverLiveScenario, createMcpReceiverStopScenario, receiverStoppedWhileProviderHeld, receiverPublication, receiverActivityMatches, receiverCompletionOutcome, receiverHistoryReloadReady } from "../scenarios/mcp_receiver_live.mjs";

test("MCP receiver reuses the normal Desktop lifecycle and retains the unconfirmed visual/export gate", () => {
  const scenario = createMcpReceiverLiveScenario();
  assert.equal(scenario.id, "mcp.receiver-live");
  assert.equal(scenario.manualGate, "pending");
  assert.equal(scenario.databaseRequired, true);
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) assert.equal(typeof scenario[method], "function");
  assert.equal("launch" in scenario, false);
  assert.throws(() => createMcpReceiverLiveScenario({ ignoreHTTPSErrors: true }));
});

test("MCP stop variant requires a real interrupted job before releasing held provider",()=>{
  const scenario=createMcpReceiverStopScenario();assert.equal(scenario.id,"mcp.receiver-stop");assert.equal(scenario.manualGate,"pending");
  const job={state:"interrupted"},ledger=[{route:"chat_completions",contract:{role:"chat_continuation",pass:true},response_phase:"held"}];
  assert.equal(receiverStoppedWhileProviderHeld(job,ledger),true);
  for(const state of ["running","cancelling","completed","failed"])assert.equal(receiverStoppedWhileProviderHeld({state},ledger),false);
  for(const rows of [[],[...ledger,...ledger],[{...ledger[0],response_phase:"completed"}],[{...ledger[0],contract:{role:"chat_continuation",pass:false}}]])assert.equal(receiverStoppedWhileProviderHeld(job,rows),false);
});

test("receiver completion rejects uncaught Hub page errors even after the GUI workflow succeeds", () => {
  assert.deepEqual(receiverCompletionOutcome([]), { acquisition: "pass", oracle: "pass", manual: "pending" });
  const pageErrors = ["TypeError: failed while refreshing history"];
  assert.throws(() => receiverCompletionOutcome(pageErrors), error => {
    assert.equal(error.owner, "product");
    assert.equal(error.code, "mcp-receiver-live-mismatch");
    assert.deepEqual(error.evidence.page_errors, pageErrors);
    return true;
  });
});

test("receiver publication requires exactly the approved online device and enabled agent profile", () => {
  const publication = { profile_id: "receiver", enabled: true, mode: "agent" };
  const device = { device_id: "approved-device", online: true, publications: [publication] };
  const accepts = devices => receiverPublication({ devices }, "approved-device", "receiver");
  assert.deepEqual(accepts([device]), publication);
  for (const devices of [[], [device, device], [{ ...device, online: false }], [{ ...device, device_id: "other" }],
    [{ ...device, publications: [publication, publication] }], [{ ...device, publications: [{ ...publication, enabled: false }] }],
    [{ ...device, publications: [{ ...publication, mode: "tools" }] }], [{ ...device, publications: [{ ...publication, profile_id: "other" }] }]]) {
    assert.equal(accepts(devices), null);
  }
});

test("receiver activity acceptance requires Rust state and the matching visible strip, including disappearance", () => {
  const idle = { activity: { running: 0, waiting: 0, awaiting_approval: 0, cancelling: 0, unavailable: false }, stripCount: 0, runningBadge: 0, label: "" };
  const running = { activity: { ...idle.activity, running: 1 }, stripCount: 1, runningBadge: 1, label: "MCP実行中 実行中 1件" };
  assert.equal(receiverActivityMatches(running, true), true);
  assert.equal(receiverActivityMatches(idle, false), true);
  assert.equal(receiverActivityMatches(idle, true), false);
  assert.equal(receiverActivityMatches(running, false), false);
  for (const value of [{ ...running, stripCount: 0 }, { ...running, runningBadge: 0 }, { ...running, label: "MCP待機中" },
    { ...running, activity: { ...running.activity, unavailable: true } }, { ...running, activity: { ...running.activity, running: 2 } }]) {
    assert.equal(receiverActivityMatches(value, true), false);
  }
  for (const value of [{ ...idle, stripCount: 1 }, { ...idle, activity: { ...idle.activity, unavailable: true } },
    { ...idle, activity: { ...idle.activity, awaiting_approval: 1 } }]) assert.equal(receiverActivityMatches(value, false), false);
});

test("history reselection waits for refresh settlement and cannot accept the previous selected row", () => {
  const row = { id: "job-a", state: "completed", pressed: "false" };
  const ready = { dialogCount: 1, page: "execution:0", detailOwner: "execution:",
    refresh: { count: 1, disabled: false, ariaDisabled: "false" }, selectedRows: 0, listError: "", rows: [row] };
  assert.equal(receiverHistoryReloadReady(ready, "job-a"), true);
  const interrupted={...ready,rows:[{...row,state:"interrupted"}]};
  assert.equal(receiverHistoryReloadReady(interrupted,"job-a","interrupted"),true);
  assert.equal(receiverHistoryReloadReady(ready,"job-a","interrupted"),false);
  for (const observation of [
    { ...ready, rows: [] }, // normal loading clears the previous list
    { ...ready, detailOwner: "execution:job-a", selectedRows: 1, rows: [{ ...row, pressed: "true" }] },
    { ...ready, refresh: { ...ready.refresh, disabled: true } },
    { ...ready, refresh: { ...ready.refresh, ariaDisabled: "true" } },
    { ...ready, listError: "履歴を取得できませんでした。" },
    { ...ready, rows: [{ ...row, id: "job-b" }] },
    { ...ready, rows: [{ ...row, state: "running" }] },
    { ...ready, rows: [row, row] },
    { ...ready, page: "instruction:0" },
    { ...ready, dialogCount: 0 },
  ]) assert.equal(receiverHistoryReloadReady(observation, "job-a"), false);
});
