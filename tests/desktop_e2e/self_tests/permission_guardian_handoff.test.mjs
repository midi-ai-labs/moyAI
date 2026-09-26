import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { createGuardianHandoffScenario, guardianHandoffPlan, guardianHandoffPendingFailures, guardianHandoffSeedReady,
  guardianHandoffTerminalFailures, guardianHandoffFooterFailures } from "../scenarios/permission_guardian_handoff.mjs";

const ID = "01M00000000000000000000000";
const roles = ["guardian_seed", "guardian_tool_initial", "guardian_review", "guardian_continuation"];
const ledger = length => roles.slice(0, length).map(role => ({ route: "responses", method: "POST", pathname: "/v1/responses",
  query_present: false, contract: { pass: true, role }, response_phase: "completed", response_status: 200 }));
const plan = decision => guardianHandoffPlan(path.resolve("fixture workspace"), decision);

test("seed readiness waits for the rendered idle owner and completed post-run refresh before a second Send", () => {
  const idle = { sessionId: ID, runtimeOwnerToken: "idle:1", expectedState: { kind: "idle", latestTurnId: ID, admissionRevision: "1" } };
  const surface = { projection: { run_target: idle, draft_target: { sessionId: ID }, run_status_key: "completed",
    task_activity_state: "idle", busy: false, post_run_refresh_pending: false, background_mutation_pending: false,
    async_polling_required: false, pending_async_operations: [], composer_submit_mode: "new_request", can_submit: true,
    transcript_rows: [{ row_kind: "assistant", body: "HANDOFF_AUTHORITY_READY" }] },
    composer: { count: 1, visible: true, run_target: structuredClone(idle) },
    prompt: { count: 1, visible: true, enabled: true, value: "" }, send: { count: 1, visible: true, enabled: false },
    visible_fatal_count: 0, visible_recoverable_error_count: 0 };
  assert.equal(guardianHandoffSeedReady({ surface, ledger: ledger(1) }), true);
  surface.composer.run_target = { ...idle, runtimeOwnerToken: "root:1", expectedState: { kind: "turn", turnId: ID, admissionRevision: "1" } };
  assert.equal(guardianHandoffSeedReady({ surface, ledger: ledger(1) }), false);
  surface.composer.run_target = structuredClone(idle);
  surface.projection.post_run_refresh_pending = true;
  assert.equal(guardianHandoffSeedReady({ surface, ledger: ledger(1) }), false);
  surface.projection.post_run_refresh_pending = false;
  surface.projection.pending_async_operations = ["history"];
  assert.equal(guardianHandoffSeedReady({ surface, ledger: ledger(1) }), false);
});

test("handoff uses isolated receipt effects and fresh common-lifecycle scenarios", () => {
  const value = plan("approve");
  assert.equal(value.command.split("\n").length, 66);
  assert.match(value.command, /AppendAllText/);
  assert.ok(value.receiptPath.startsWith(path.resolve("fixture workspace")));
  assert.notEqual(createGuardianHandoffScenario(), createGuardianHandoffScenario());
  for (const decision of ["approve", "abort"]) {
    const scenario = createGuardianHandoffScenario(decision);
    assert.equal(scenario.id, `permission.guardian-handoff-${decision}`);
    for (const method of ["prepare", "execute", "quiesce", "cleanup", "requestGracefulExit"]) assert.equal(typeof scenario[method], "function");
  }
  assert.throws(() => guardianHandoffPlan("relative", "approve"));
  assert.throws(() => createGuardianHandoffScenario("denied"));
});

test("handoff pending oracle requires actual user controls, exact request chain, and no premature effect", () => {
  const value = plan("approve");
  const rawReason = "代理承認からの確認: この操作の対象を確認してください。";
  const sample = { ledger: ledger(3), receipt: null, surface: { errors: 0,
    projection: { confirmation_visible: true, confirmation_id: "1", confirmation: { remote: null, details: [rawReason] } },
    dialog: { count: 1, id: "1", visible: true, busy: false, command: value.command, text: "確認が必要な理由\nこの操作の対象を確認してください。",
      buttons: ["approve-permission", "abort-permission"].map(action => ({ action, enabled: true, visible: true })) } } };
  assert.deepEqual(guardianHandoffPendingFailures(sample, value.command), []);
  assert.ok(guardianHandoffPendingFailures({ ...sample, receipt: value.receiptText }, value.command).includes("effect-before-human-approval"));
  assert.ok(guardianHandoffPendingFailures({ ...sample, ledger: ledger(4) }, value.command).includes("provider-not-awaiting-human"));
  sample.surface.projection.confirmation_id = ID;
  assert.ok(guardianHandoffPendingFailures(sample, value.command).includes("local-confirmation-owner"));
  sample.surface.projection.confirmation_id = "1";
  sample.surface.projection.confirmation.details = [];
  assert.ok(guardianHandoffPendingFailures(sample, value.command).includes("confirmation-reason"));
  sample.surface.projection.confirmation.details = [rawReason];
  sample.surface.dialog.text = "別の説明";
  assert.ok(guardianHandoffPendingFailures(sample, value.command).includes("confirmation-reason"));
  sample.surface.dialog.text = "確認が必要な理由\nこの操作の対象を確認してください。";
  sample.surface.dialog.command = "a different command";
  assert.ok(guardianHandoffPendingFailures(sample, value.command).includes("confirmation-content"));
});

test("terminal oracle rejects duplicate effects, cancellation execution, and model bypass after abort", () => {
  for (const decision of ["approve", "abort"]) {
    const value = plan(decision), approved = decision === "approve";
    const sample = { receipt: approved ? value.receiptText : null, ledger: ledger(approved ? 4 : 3),
      surface: { errors: 0, dialog: { count: 0 }, projection: { confirmation_visible: false, confirmation_id: null,
        busy: false, task_activity_state: "idle", run_status_key: approved ? "completed" : "cancelled",
        transcript_rows: approved ? [{ row_kind: "assistant", body: value.responseText }] : [] } } };
    assert.deepEqual(guardianHandoffTerminalFailures(sample, value), []);
    assert.ok(guardianHandoffTerminalFailures({ ...sample, receipt: value.receiptText.repeat(2) }, value).includes("effect-count"));
    if (!approved) assert.ok(guardianHandoffTerminalFailures({ ...sample, ledger: ledger(4) }, value).includes("provider-replay-or-continuation-mismatch"));
  }
});

test("long approval footer oracle requires both actual buttons inside the viewport and hit-testable", () => {
  const value = { viewport: { height: 800, width: 1200 }, footer: { top: 650, bottom: 760 },
    buttons: ["approve-permission", "abort-permission"].map(action => ({ action, rect: { top: 700, bottom: 740 }, reachable: true, enabled: true })) };
  assert.deepEqual(guardianHandoffFooterFailures(value), []);
  value.buttons[0].reachable = false;
  assert.ok(guardianHandoffFooterFailures(value).includes("button-not-reachable:approve-permission"));
  value.footer.bottom = 900;
  assert.ok(guardianHandoffFooterFailures(value).includes("footer-outside-viewport"));
});
