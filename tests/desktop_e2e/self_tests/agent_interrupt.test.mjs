import assert from "node:assert/strict";
import test from "node:test";

import {
  AGENT_INTERRUPT_PATH,
  AGENT_INTERRUPT_PROMPT,
  agentInterruptControlFailures,
  agentInterruptFixtureConfig,
  agentInterruptInFlightFailures,
  agentInterruptListFailures,
  agentInterruptTerminalFailures,
  createAgentInterruptScenario,
  exactAgentInterruptHeldLedger,
  exactAgentInterruptTarget,
  exactAgentInterruptTerminalLedger,
} from "../scenarios/agent_interrupt.mjs";
import {
  SCRIPTED_PROVIDER_AGENT_INTERRUPT_MESSAGE,
  SCRIPTED_PROVIDER_AGENT_INTERRUPT_ROOT_RESPONSE,
  SCRIPTED_PROVIDER_AGENT_INTERRUPT_TASK_NAME,
} from "../drivers/scripted_provider.mjs";

const WORKSPACE = "C:\\e2e\\agent-interrupt";
const ROOT_SESSION_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const ROOT_TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const CHILD_SESSION_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const CHILD_TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAY";

function interruptTarget(overrides = {}) {
  return {
    workspacePath: WORKSPACE,
    rootSessionId: ROOT_SESSION_ID,
    agentPath: AGENT_INTERRUPT_PATH,
    childSessionId: CHILD_SESSION_ID,
    expectedTurnId: CHILD_TURN_ID,
    admissionRevision: "3",
    ...overrides,
  };
}

function rootHistory() {
  return [
    {
      row_kind: "user",
      stable_history_identity: "root-user-history",
      title: "",
      body: AGENT_INTERRUPT_PROMPT,
    },
    {
      row_kind: "work_summary_completed",
      stable_history_identity: "root-summary-history",
      title: "実行結果",
      body: "completed",
    },
    {
      row_kind: "assistant",
      stable_history_identity: null,
      title: "",
      body: SCRIPTED_PROVIDER_AGENT_INTERRUPT_ROOT_RESPONSE,
    },
  ];
}

function providerRow(role, phase, status) {
  return {
    route: "responses",
    method: "POST",
    pathname: "/v1/responses",
    query_present: false,
    contract: { pass: true, role },
    response_phase: phase,
    response_status: status,
  };
}

function heldLedger() {
  return [
    providerRow("root_initial", "completed", 200),
    providerRow("child_held", "held", null),
    providerRow("root_continuation", "completed", 200),
  ];
}

function terminalLedger() {
  const rows = heldLedger();
  rows[1] = providerRow("child_held", "peer_closed", null);
  return rows;
}

function rootSessionRow() {
  return {
    session_id: ROOT_SESSION_ID,
    status: "completed",
    loaded_status: "idle",
    admission_revision: "9",
    interrupt_target: null,
  };
}

function runningProjection(overrides = {}) {
  return {
    projection_revision: "21",
    workspace_path: WORKSPACE,
    run_status_key: "completed",
    run_target: {
      sessionId: ROOT_SESSION_ID,
      expectedState: {
        kind: "idle",
        latestTurnId: ROOT_TURN_ID,
        admissionRevision: "9",
      },
    },
    session_rows: [],
    chat_session_rows: [rootSessionRow()],
    transcript_rows: rootHistory(),
    agent_activity_rows: [{
      agent_path: AGENT_INTERRUPT_PATH,
      session_id: CHILD_SESSION_ID,
      task_name: SCRIPTED_PROVIDER_AGENT_INTERRUPT_TASK_NAME,
      task_preview: SCRIPTED_PROVIDER_AGENT_INTERRUPT_MESSAGE,
      status: "running",
      current_activity: "Model request 1",
      result_preview: "",
      started_order: 1,
      updated: false,
      active_turn_id: CHILD_TURN_ID,
      interrupt_target: interruptTarget(),
    }],
    busy: false,
    agent_tree_active: true,
    task_activity_state: "running",
    async_polling_required: true,
    can_cancel_run: false,
    post_run_refresh_pending: false,
    background_mutation_pending: false,
    pending_async_operations: [],
    navigation_loading: false,
    overlay: "none",
    confirmation_visible: false,
    ...overrides,
  };
}

function inFlightSample(overrides = {}) {
  return {
    surface: {
      projection: runningProjection(),
      interrupt_button: { count: 0, visible: false, enabled: false },
      output_agent_trigger: { count: 1, visible: true, enabled: true },
      agent_list_card: { count: 0, visible: false, enabled: false, status_key: null },
      history_agent_card: { count: 1, visible: false, status_key: "running", status_text: "作業中" },
      agent_inspector: { count: 0, visible: false, agent_path: null, status_text: null },
      visible_fatal_count: 0,
      visible_recoverable_error_count: 0,
      visible_dialog_count: 0,
      visible_modal_backdrop_count: 0,
    },
    ledger: heldLedger(),
    provider: {
      active_request_count: 1,
      accepted_response_count: 3,
      successful_response_count: 2,
      scripted_responses_request_count: 3,
    },
    ...overrides,
  };
}

function listSample(overrides = {}) {
  const base = inFlightSample();
  return {
    ...base,
    surface: {
      ...base.surface,
      output_agent_trigger: { count: 0, visible: false, enabled: false },
      agent_list_card: { count: 1, visible: true, enabled: true, status_key: "running" },
      agent_inspector: { count: 1, visible: true, agent_path: null, status_text: null },
    },
    ...overrides,
  };
}

function controlSample(overrides = {}) {
  const base = listSample();
  return {
    ...base,
    surface: {
      ...base.surface,
      agent_list_card: { count: 0, visible: false, enabled: false, status_key: null },
      interrupt_button: { count: 1, visible: true, enabled: true },
      agent_inspector: {
        count: 1,
        visible: true,
        agent_path: AGENT_INTERRUPT_PATH,
        status_text: "作業中",
      },
    },
    ...overrides,
  };
}

function rootOwner() {
  return {
    workspacePath: WORKSPACE,
    sessionId: ROOT_SESSION_ID,
    expectedState: {
      kind: "idle",
      latestTurnId: ROOT_TURN_ID,
      admissionRevision: "9",
    },
    history: rootHistory(),
  };
}

function terminalProjection(overrides = {}) {
  return {
    ...runningProjection(),
    projection_revision: "22",
    agent_activity_rows: [{
      ...runningProjection().agent_activity_rows[0],
      status: "interrupted",
      current_activity: "",
      result_preview: "Interrupted",
      updated: true,
      active_turn_id: null,
      interrupt_target: null,
    }],
    agent_tree_active: false,
    task_activity_state: "idle",
    async_polling_required: false,
    ...overrides,
  };
}

function childExecution() {
  return {
    workspace_path: WORKSPACE,
    root_session_id: ROOT_SESSION_ID,
    agent_path: AGENT_INTERRUPT_PATH,
    session_id: CHILD_SESSION_ID,
    task_name: SCRIPTED_PROVIDER_AGENT_INTERRUPT_TASK_NAME,
    transcript_rows: [
      {
        row_kind: "system",
        body: `Message Type: NEW_TASK\nTask name: ${AGENT_INTERRUPT_PATH}\nSender: /root\nPayload:\n${SCRIPTED_PROVIDER_AGENT_INTERRUPT_MESSAGE}`,
      },
      { row_kind: "work_summary_cancelled", body: "" },
    ],
    turn_page_offset: 0,
    turn_page_end: 3,
    turn_page_total: 3,
    turn_page_has_previous: false,
  };
}

function terminalSample(overrides = {}) {
  return {
    surface: {
      projection: terminalProjection(),
      interrupt_button: { count: 0, visible: false, enabled: false },
      output_agent_trigger: { count: 0, visible: false, enabled: false },
      agent_list_card: { count: 0, visible: false, enabled: false, status_key: null },
      history_agent_card: { count: 1, visible: false, status_key: "interrupted", status_text: "中断しました" },
      agent_inspector: {
        count: 1,
        visible: true,
        agent_path: AGENT_INTERRUPT_PATH,
        status_text: "中断",
      },
      visible_fatal_count: 0,
      visible_recoverable_error_count: 0,
      visible_dialog_count: 0,
      visible_modal_backdrop_count: 0,
    },
    ledger: terminalLedger(),
    provider: {
      active_request_count: 0,
      accepted_response_count: 3,
      successful_response_count: 2,
      scripted_responses_request_count: 3,
    },
    child_execution_outcome: { ok: true, error: null, value: childExecution() },
    ...overrides,
  };
}

test("agent interrupt target requires exact child lineage with canonical ULIDs and u64 revision", () => {
  assert.equal(exactAgentInterruptTarget(interruptTarget()), true);
  assert.equal(exactAgentInterruptTarget(
    interruptTarget({ admissionRevision: "18446744073709551615" }),
  ), true);
  assert.equal(exactAgentInterruptTarget(interruptTarget(), interruptTarget()), true);
  for (const invalid of [
    interruptTarget({ rootSessionId: ROOT_SESSION_ID.toLowerCase() }),
    interruptTarget({ childSessionId: null }),
    interruptTarget({ expectedTurnId: "turn-1" }),
    interruptTarget({ admissionRevision: "03" }),
    interruptTarget({ admissionRevision: -1 }),
    interruptTarget({ admissionRevision: "18446744073709551616" }),
    interruptTarget({ agentPath: "/root/sibling" }),
    { ...interruptTarget(), excess: true },
    (() => { const value = interruptTarget(); delete value.admissionRevision; return value; })(),
  ]) {
    assert.equal(exactAgentInterruptTarget(invalid), false, JSON.stringify(invalid));
  }
});

test("in-flight oracle requires one coherent projection, completed root, held child, and canonical pane route", () => {
  assert.deepEqual(agentInterruptInFlightFailures(inFlightSample()), []);
  assert.equal(exactAgentInterruptHeldLedger(heldLedger()), true);
  assert.equal(exactAgentInterruptHeldLedger(terminalLedger()), false);

  const missingRevision = interruptTarget();
  delete missingRevision.admissionRevision;
  assert.ok(agentInterruptInFlightFailures(inFlightSample({
    surface: {
      ...inFlightSample().surface,
      projection: runningProjection({
        agent_activity_rows: [{
          ...runningProjection().agent_activity_rows[0],
          interrupt_target: missingRevision,
        }],
      }),
    },
  })).includes("child-interrupt-target-not-canonical"));
  assert.ok(agentInterruptInFlightFailures(inFlightSample({
    surface: {
      ...inFlightSample().surface,
      output_agent_trigger: { count: 2, visible: true, enabled: true },
    },
  })).includes("canonical-agent-pane-route-not-interactable"));
  assert.ok(agentInterruptInFlightFailures(inFlightSample({
    ledger: [...heldLedger(), providerRow("child_held", "held", null)],
  })).includes("provider-three-role-flow-not-held"));
});

test("canonical agent pane exposes one exact child list route", () => {
  const expectedTarget = interruptTarget();
  assert.deepEqual(agentInterruptListFailures(listSample(), expectedTarget), []);
  assert.ok(agentInterruptListFailures(listSample({
    surface: {
      ...listSample().surface,
      agent_list_card: { count: 1, visible: true, enabled: true, status_key: "completed" },
    },
  })).includes("exact-child-list-route-not-interactable"));
  for (const driftedTarget of [
    interruptTarget({ expectedTurnId: "01ARZ3NDEKTSV4RRFFQ69G5FAZ" }),
    interruptTarget({ admissionRevision: "4" }),
  ]) {
    assert.ok(agentInterruptListFailures(listSample({
      surface: {
        ...listSample().surface,
        projection: runningProjection({
          agent_activity_rows: [{
            ...runningProjection().agent_activity_rows[0],
            active_turn_id: driftedTarget.expectedTurnId,
            interrupt_target: driftedTarget,
          }],
        }),
      },
    }), expectedTarget).includes("child-interrupt-target-drifted"));
  }
});

test("opened child inspector owns one exact interrupt control", () => {
  assert.deepEqual(agentInterruptControlFailures(controlSample(), interruptTarget()), []);
  assert.ok(agentInterruptControlFailures(controlSample({
    surface: {
      ...controlSample().surface,
      interrupt_button: { count: 2, visible: true, enabled: true },
    },
  })).includes("exact-child-interrupt-control-not-interactable"));
  assert.ok(agentInterruptControlFailures(controlSample({
    surface: {
      ...controlSample().surface,
      agent_inspector: {
        count: 1,
        visible: true,
        agent_path: "/root/sibling",
        status_text: "作業中",
      },
    },
  })).includes("exact-child-interrupt-control-not-interactable"));
});

test("terminal oracle requires durable interrupted child, stable root, no newer turn, and no replay", () => {
  const target = interruptTarget();
  const owner = rootOwner();
  assert.deepEqual(agentInterruptTerminalFailures(terminalSample(), target, owner), []);
  assert.equal(exactAgentInterruptTerminalLedger(terminalLedger()), true);
  assert.equal(exactAgentInterruptTerminalLedger(heldLedger()), false);

  assert.ok(agentInterruptTerminalFailures(terminalSample({
    surface: {
      ...terminalSample().surface,
      projection: terminalProjection({
        run_target: {
          sessionId: ROOT_SESSION_ID,
          expectedState: {
            kind: "idle",
            latestTurnId: "01ARZ3NDEKTSV4RRFFQ69G5FAZ",
            admissionRevision: "10",
          },
        },
      }),
    },
  }), target, owner).includes("root-or-newer-turn-was-affected"));
  assert.ok(agentInterruptTerminalFailures(terminalSample({
    child_execution_outcome: { ok: true, error: null, value: { ...childExecution(), transcript_rows: [] } },
  }), target, owner).includes("durable-child-cancelled-history-missing"));
  assert.ok(agentInterruptTerminalFailures(terminalSample({
    child_execution_outcome: {
      ok: true,
      error: null,
      value: { ...childExecution(), turn_page_end: 2 },
    },
  }), target, owner).includes("durable-child-cancelled-history-missing"));
  assert.ok(agentInterruptTerminalFailures(terminalSample({
    ledger: [...terminalLedger(), providerRow("child_held", "peer_closed", null)],
  }), target, owner).includes("provider-request-replayed-or-child-completed"));
  assert.ok(agentInterruptTerminalFailures(terminalSample({
    surface: { ...terminalSample().surface, visible_recoverable_error_count: 1 },
  }), target, owner).includes("error-or-blocking-overlay-visible"));
});

test("agent interrupt scenario enables only the bounded multi-agent provider surface", () => {
  const config = agentInterruptFixtureConfig("http://127.0.0.1:43123");
  assert.match(config, /supports_tools = true/);
  assert.match(config, /parallel_tool_calls = false/);
  assert.match(config, /\[multi_agent\][\s\S]*enabled = true/);
  assert.match(config, /max_concurrent_agents = 2/);
  assert.match(config, /max_concurrent_model_requests = 2/);
  assert.match(config, /max_retries = 0/);
  assert.doesNotMatch(config, /max_(?:output_)?tokens|reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]/);
  assert.doesNotMatch(config, /run[-_ ]?\d+/i);

  const scenario = createAgentInterruptScenario();
  assert.equal(scenario.id, "agent.interrupt");
  assert.equal(scenario.productOracle, "pass");
  assert.equal(scenario.manualGate, "not_required");
  assert.equal(scenario.databaseRequired, true);
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) {
    assert.equal(typeof scenario[method], "function", method);
  }
});
