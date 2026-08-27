import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import {
  SCRIPTED_PROVIDER_MODEL_ID,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";
import {
  WebviewInput,
  assertExactSemanticTarget,
  assertTrustedProbeSequence,
} from "../drivers/webview_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot, selectedNavigationIdentity } from "./observations.mjs";
import {
  classifyAcquiredObservationFailure,
  providerRestartFixtureConfig,
  quiesceProviderResource,
} from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:settings.session";
export const SESSION_ROOT_ALPHA_PROMPT = "create root alpha";
export const SESSION_ROOT_BETA_PROMPT = "create root beta";
export const SESSION_ROOT_ALPHA_RESPONSE = "ALPHA_OK";
export const SESSION_ROOT_BETA_RESPONSE = "BETA_OK";
export const SESSION_CONTEXT_BEFORE = "";
export const SESSION_CONTEXT_AFTER = "65537";
export const SESSION_RESTART_STABILITY_MS = 500;
export const SESSION_PROVIDER_PROFILE = "openai_responses";
export const SESSION_PROVIDER_API_KEY_ENV = "";
export const SESSION_PROVIDER_PROFILE_OPTIONS = Object.freeze([
  "lm_studio",
  "openai_compatible",
  "openai_responses",
  "lm_studio_chat_completions",
]);

const PROMPT = Object.freeze({ selector: "textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } });
const SEND = Object.freeze({
  selector: 'button[data-action="send"]',
  identity: { tag: "BUTTON", action: "send" },
});
export const SESSION_SETTINGS_MODEL_TRIGGER = Object.freeze({
  selector: 'button[data-action="show-session-settings"][data-session-settings-trigger="model"]',
  identity: { tag: "BUTTON", action: "show-session-settings", sessionSettingsTrigger: "model" },
});

const STABLE_NAVIGATION_ID = /^[0-9A-HJKMNP-TV-Z]{26}$/;

function stableNavigationId(value, label) {
  if (typeof value !== "string" || !STABLE_NAVIGATION_ID.test(value)) {
    throw new TypeError(`${label} is not a canonical navigation identity`);
  }
  return value;
}

export function projectNewSessionLocator(projectId) {
  const owner = stableNavigationId(projectId, "project id");
  const focusKey = `project:${owner}:new-session`;
  return Object.freeze({
    selector: `button[data-action="new-project-session"][data-focus-key="${focusKey}"]`,
    identity: { tag: "BUTTON", action: "new-project-session", focusKey },
  });
}

export function projectRowHoverLocator(projectId) {
  const owner = stableNavigationId(projectId, "project id");
  const focusKey = `project:${owner}:select`;
  return Object.freeze({
    selector: `button[data-action="project"][data-focus-key="${focusKey}"]`,
    identity: { tag: "BUTTON", action: "project", focusKey },
  });
}

export function projectSessionSelectionLocator(rootSessionId) {
  const owner = stableNavigationId(rootSessionId, "root session id");
  const focusKey = `session:${owner}:select`;
  return Object.freeze({
    selector: `button[data-action="session"][data-focus-key="${focusKey}"]`,
    identity: { tag: "BUTTON", action: "session", focusKey },
  });
}
const BASE_URL = Object.freeze({
  selector: '[data-modal="session-settings"] input[data-session-setting="base-url"]',
  identity: { tag: "INPUT", sessionSetting: "base-url" },
});
const CONTEXT_WINDOW = Object.freeze({
  selector: '[data-modal="session-settings"] input[data-session-setting="context-window"]',
  identity: { tag: "INPUT", sessionSetting: "context-window" },
});
const APPLY_SESSION_SETTINGS = Object.freeze({
  selector: '[data-modal="session-settings"] button[data-action="apply-session-settings"]',
  identity: { tag: "BUTTON", action: "apply-session-settings" },
});
const DISCARD_SESSION_SETTINGS = Object.freeze({
  selector: '[data-modal="session-settings"] button[data-action="discard-session-settings"]',
  identity: { tag: "BUTTON", action: "discard-session-settings" },
});
const CLOSE_SESSION_SETTINGS = Object.freeze({
  selector: '[data-modal="session-settings"] button[data-action="close-overlay"]',
  identity: { tag: "BUTTON", action: "close-overlay" },
});
const CANCEL_DIRTY_CLOSE = Object.freeze({
  selector: '[data-modal="session-settings-close-confirmation"] button[data-action="cancel-local-confirm"]',
  identity: { tag: "BUTTON", action: "cancel-local-confirm" },
});

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function productFailure(code, message, evidence) {
  return new DesktopE2eError("product", code, message, evidence);
}

function errorObservation(error) {
  return {
    name: error?.name ?? "Error",
    code: error?.code ?? null,
    message: error?.message ?? String(error),
    evidence: error?.evidence ?? null,
  };
}

function settingsGate(id, failure, pass, expected, actual) {
  return { id, failure, pass: pass === true, expected, actual };
}

function settingsDecision(stage, observation, gates, detail = {}) {
  const { terminalFailures = [], ...evidence } = detail;
  const failures = Array.from(new Set(
    gates.filter((gate) => !gate.pass).map((gate) => gate.failure),
  ));
  const terminal = new Set(terminalFailures);
  const terminalFailuresPresent = failures.filter((failure) => terminal.has(failure));
  const status = failures.length === 0
    ? "pass"
    : terminalFailuresPresent.length > 0 ? "fail" : "pending";
  return {
    stage,
    status,
    accepted: status === "pass",
    failures,
    terminal_failures: terminalFailuresPresent,
    gates,
    observation,
    ...evidence,
  };
}

function navigationSettled(projection) {
  return projection?.navigation_loading === false
    && projection?.background_mutation_pending === false
    && projection?.async_polling_required === false
    && Array.isArray(projection?.pending_async_operations)
    && projection.pending_async_operations.length === 0;
}

function projectionRevisionAdvanced(current, baseline) {
  if (baseline === null) return true;
  try {
    return BigInt(current ?? "-1") > BigInt(baseline);
  } catch {
    return false;
  }
}

function exactTargetOwner(target, { workspacePath, rootSessionId, settingsRevision }) {
  return targetShapeValid(target)
    && target.workspacePath === workspacePath
    && target.rootSessionId === rootSessionId
    && target.settingsRevision === settingsRevision;
}

export function hoveredProjectActionDecision(observation, locator) {
  try {
    return {
      decision: "pass",
      acquired: assertExactSemanticTarget(observation, locator),
      error: null,
    };
  } catch (error) {
    return {
      decision: error?.code === "semantic-target-hidden" ? "pending" : "fail",
      acquired: null,
      error: errorObservation(error),
    };
  }
}

function targetShapeValid(target) {
  return typeof target?.workspacePath === "string"
    && target.workspacePath.length > 0
    && typeof target?.rootSessionId === "string"
    && target.rootSessionId.length > 0
    && /^\d+$/.test(target?.settingsRevision ?? "")
    && /^\d+$/.test(target?.configGeneration ?? "")
    && typeof target?.runtimeOwnerToken === "string"
    && target.runtimeOwnerToken.length > 0;
}

function errorFree(surface) {
  return surface?.visible_fatal_count === 0
    && surface?.visible_recoverable_error_count === 0
    && surface?.visible_validation_error_count === 0;
}

function exactSessionSettingsTriggerReady(observation, trigger) {
  return observation?.count === 1
    && observation.visible === true
    && observation.enabled === true
    && observation.identity?.tag === "BUTTON"
    && observation.identity?.action === "show-session-settings"
    && observation.identity?.session_settings_trigger === trigger;
}

function exactSessionSettingsTriggersReady(surface) {
  return exactSessionSettingsTriggerReady(surface?.triggers?.model, "model")
    && exactSessionSettingsTriggerReady(surface?.triggers?.access, "access");
}

export function projectRootNavigationDecision(surface, {
  projectId,
  rootSessionIds,
  selectedRootSessionId = null,
}) {
  stableNavigationId(projectId, "project id");
  if (!Array.isArray(rootSessionIds) || rootSessionIds.length === 0) {
    throw new TypeError("project root navigation requires at least one root session id");
  }
  const requiredRoots = rootSessionIds.map((id) => stableNavigationId(id, "root session id"));
  if (new Set(requiredRoots).size !== requiredRoots.length) {
    throw new TypeError("project root navigation identities must be unique");
  }
  if (selectedRootSessionId !== null) {
    stableNavigationId(selectedRootSessionId, "selected root session id");
    if (!requiredRoots.includes(selectedRootSessionId)) {
      throw new TypeError("selected root session must be part of the required navigation roots");
    }
  }
  if (!errorFree(surface)) return "fail";
  const projection = surface?.projection;
  if (
    projection?.overlay !== "none"
    || projection?.navigation_loading !== false
    || surface?.visible_dialog_count !== 0
  ) return "pending";
  const projects = Array.isArray(projection?.project_rows) ? projection.project_rows : [];
  const sessions = Array.isArray(projection?.session_rows) ? projection.session_rows : [];
  const domRows = Array.isArray(surface?.project_session_navigation)
    ? surface.project_session_navigation
    : [];
  const selectedProject = projects[projection?.selected_project_index];
  if (selectedProject?.project_id !== projectId) return "pending";

  for (const rootSessionId of requiredRoots) {
    const projectionCount = sessions.filter((row) => row?.session_id === rootSessionId).length;
    const focusKey = `session:${rootSessionId}:select`;
    const matchingDom = domRows.filter((row) => row?.action === "session" && row?.focus_key === focusKey);
    if (projectionCount > 1 || matchingDom.length > 1) return "fail";
    if (
      projectionCount !== 1
      || matchingDom.length !== 1
      || matchingDom[0].visible !== true
      || matchingDom[0].enabled !== true
    ) return "pending";
  }

  const selectedProjection = sessions[projection?.selected_session_index];
  const selectedDom = domRows.filter((row) => row?.aria_current === "page");
  if (selectedDom.length > 1) return "fail";
  if (selectedRootSessionId === null) {
    return projection?.selected_session_index === -1 && selectedDom.length === 0
      ? "pass"
      : "pending";
  }
  const selectedFocusKey = `session:${selectedRootSessionId}:select`;
  return selectedProjection?.session_id === selectedRootSessionId
    && selectedDom.length === 1
    && selectedDom[0].action === "session"
    && selectedDom[0].focus_key === selectedFocusKey
    ? "pass"
    : "pending";
}

function restartNavigationFacts(surface, {
  projectId,
  rootSessionIds,
  expectedRootSessionId,
}) {
  const ownerProjectId = stableNavigationId(projectId, "project id");
  if (!Array.isArray(rootSessionIds) || rootSessionIds.length === 0) {
    throw new TypeError("restart navigation requires root session identities");
  }
  const roots = rootSessionIds.map((id) => stableNavigationId(id, "root session id"));
  if (new Set(roots).size !== roots.length) {
    throw new TypeError("restart navigation root session identities must be unique");
  }
  const expectedRoot = stableNavigationId(expectedRootSessionId, "expected root session id");
  if (!roots.includes(expectedRoot)) {
    throw new TypeError("restart expected root must be one of the required roots");
  }
  const projection = surface?.projection;
  const projects = Array.isArray(projection?.project_rows) ? projection.project_rows : [];
  const sessions = Array.isArray(projection?.session_rows) ? projection.session_rows : [];
  const domRows = Array.isArray(surface?.project_session_navigation)
    ? surface.project_session_navigation
    : [];
  const rootFacts = roots.map((rootSessionId) => {
    const focusKey = `session:${rootSessionId}:select`;
    const projectionRows = sessions.filter((row) => row?.session_id === rootSessionId);
    const renderedRows = domRows.filter(
      (row) => row?.action === "session" && row?.focus_key === focusKey,
    );
    return {
      root_session_id: rootSessionId,
      projection_count: projectionRows.length,
      rendered_count: renderedRows.length,
      visible: renderedRows.length === 1 ? renderedRows[0].visible === true : false,
      enabled: renderedRows.length === 1 ? renderedRows[0].enabled === true : false,
      aria_current: renderedRows.length === 1 ? renderedRows[0].aria_current : null,
    };
  });
  const selectedProjection = sessions[projection?.selected_session_index];
  const selectedDomRows = domRows.filter((row) => row?.aria_current === "page");
  const selectedDom = selectedDomRows.length === 1 ? selectedDomRows[0] : null;
  const selectedProject = projects[projection?.selected_project_index];
  const expectedFocusKey = `session:${expectedRoot}:select`;
  const expectedRootFact = rootFacts.find((row) => row.root_session_id === expectedRoot);
  const expectedSelected = selectedProjection?.session_id === expectedRoot
    && selectedDomRows.length === 1
    && selectedDom?.action === "session"
    && selectedDom?.focus_key === expectedFocusKey;
  return {
    ownerProjectId,
    roots,
    expectedRoot,
    projection,
    project: {
      count: projects.filter((row) => row?.project_id === ownerProjectId).length,
      selected_project_id: selectedProject?.project_id ?? null,
    },
    rootFacts,
    expectedRootFact,
    selected: {
      projection_root_session_id: selectedProjection?.session_id ?? null,
      rendered_count: selectedDomRows.length,
      action: selectedDom?.action ?? null,
      focus_key: selectedDom?.focus_key ?? null,
    },
    expectedSelected,
  };
}

export function restartedSessionSelectionDecision(surface, expected) {
  const facts = restartNavigationFacts(surface, expected);
  const settled = navigationSettled(facts.projection);
  const gates = [
    settingsGate("surface-errors", "restart-selection-surface-error", errorFree(surface), {
      fatal: 0,
      recoverable: 0,
      validation: 0,
    }, {
      fatal: surface?.visible_fatal_count ?? null,
      recoverable: surface?.visible_recoverable_error_count ?? null,
      validation: surface?.visible_validation_error_count ?? null,
    }),
    settingsGate("overlay-closed", "restart-selection-overlay-open", facts.projection?.overlay === "none"
      && surface?.visible_dialog_count === 0
      && surface?.panel?.count === 0
      && surface?.confirmation?.count === 0, {
      overlay: "none",
      dialog_count: 0,
      panel_count: 0,
      confirmation_count: 0,
    }, {
      overlay: facts.projection?.overlay ?? null,
      dialog_count: surface?.visible_dialog_count ?? null,
      panel_count: surface?.panel?.count ?? null,
      confirmation_count: surface?.confirmation?.count ?? null,
    }),
    settingsGate("navigation-settled", "restart-navigation-not-settled", settled, {
      navigation_loading: false,
      background_mutation_pending: false,
      async_polling_required: false,
      pending_async_operation_count: 0,
    }, {
      navigation_loading: facts.projection?.navigation_loading ?? null,
      background_mutation_pending: facts.projection?.background_mutation_pending ?? null,
      async_polling_required: facts.projection?.async_polling_required ?? null,
      pending_async_operation_count: Array.isArray(facts.projection?.pending_async_operations)
        ? facts.projection.pending_async_operations.length
        : null,
    }),
    settingsGate("projection-revision", "restart-selection-projection-revision-invalid", typeof facts.projection?.projection_revision === "string"
      && /^\d+$/.test(facts.projection.projection_revision), "unsigned decimal string", facts.projection?.projection_revision ?? null),
    settingsGate("project-owner", "restart-project-owner-mismatch", facts.project.count === 1
      && facts.project.selected_project_id === facts.ownerProjectId, {
      project_id: facts.ownerProjectId,
      count: 1,
      selected: true,
    }, facts.project),
    settingsGate("root-owners", "restart-root-owners-not-exact", facts.rootFacts.every(
      (row) => row.projection_count === 1 && row.rendered_count === 1,
    ), facts.roots.map((root_session_id) => ({ root_session_id, projection_count: 1, rendered_count: 1 })), facts.rootFacts),
    settingsGate("selection-action", "restart-root-selection-action-not-ready", facts.expectedRootFact?.projection_count === 1
      && facts.expectedRootFact?.rendered_count === 1
      && facts.expectedRootFact.visible === true
      && facts.expectedRootFact.enabled === true, {
      root_session_id: facts.expectedRoot,
      projection_count: 1,
      rendered_count: 1,
      visible: true,
      enabled: true,
    }, facts.expectedRootFact ?? null),
  ];
  const terminalFailures = [];
  if (!errorFree(surface)) terminalFailures.push("restart-selection-surface-error");
  if (typeof facts.projection?.projection_revision !== "string"
    || !/^\d+$/.test(facts.projection.projection_revision)) {
    terminalFailures.push("restart-selection-projection-revision-invalid");
  }
  if (facts.project.count > 1) terminalFailures.push("restart-project-owner-mismatch");
  if (facts.rootFacts.some((row) => row.projection_count > 1 || row.rendered_count > 1)) {
    terminalFailures.push("restart-root-owners-not-exact");
  }
  if ((facts.expectedRootFact?.rendered_count ?? 0) > 1) {
    terminalFailures.push("restart-root-selection-action-not-ready");
  }
  if (settled && facts.project.count === 1 && facts.project.selected_project_id !== facts.ownerProjectId) {
    terminalFailures.push("restart-project-owner-mismatch");
  }
  return settingsDecision("restart-root-selection", surface, gates, {
    terminalFailures,
    route: facts.expectedSelected ? "already-selected" : "select-exact-root",
    expected_root_session_id: facts.expectedRoot,
    selected: facts.selected,
  });
}

export function restartedSessionSettingsTriggerDecision(surface, expected) {
  const afterProjectionRevision = expected?.afterProjectionRevision ?? null;
  if (afterProjectionRevision !== null
    && (typeof afterProjectionRevision !== "string" || !/^\d+$/.test(afterProjectionRevision))) {
    throw new TypeError("restart trigger projection baseline must be decimal digits");
  }
  const selection = restartedSessionSelectionDecision(surface, expected);
  const facts = restartNavigationFacts(surface, expected);
  const settled = navigationSettled(facts.projection);
  const fresh = projectionRevisionAdvanced(
    facts.projection?.projection_revision,
    afterProjectionRevision,
  );
  const target = facts.projection?.session_settings?.target ?? null;
  const gates = [
    ...selection.gates,
    settingsGate("navigation-revision", "restart-navigation-not-fresh", fresh, afterProjectionRevision === null
      ? { required: false }
      : { projection_revision: `>${afterProjectionRevision}` }, {
      projection_revision: facts.projection?.projection_revision ?? null,
    }),
    settingsGate("selected-root", "restart-root-not-selected", facts.expectedSelected, {
      root_session_id: facts.expectedRoot,
      rendered_count: 1,
      aria_current: "page",
    }, facts.selected),
    settingsGate("session-settings-target", "restart-session-settings-target-mismatch", facts.projection?.session_settings?.available === true
      && targetShapeValid(target)
      && target.rootSessionId === facts.expectedRoot, {
      available: true,
      root_session_id: facts.expectedRoot,
    }, {
      available: facts.projection?.session_settings?.available ?? null,
      root_session_id: target?.rootSessionId ?? null,
      target,
    }),
    settingsGate("model-trigger", "restart-model-trigger-not-ready", exactSessionSettingsTriggerReady(surface?.triggers?.model, "model"), {
      count: 1,
      visible: true,
      enabled: true,
      action: "show-session-settings",
      trigger: "model",
    }, surface?.triggers?.model ?? null),
    settingsGate("access-trigger", "restart-access-trigger-not-ready", exactSessionSettingsTriggerReady(surface?.triggers?.access, "access"), {
      count: 1,
      visible: true,
      enabled: true,
      action: "show-session-settings",
      trigger: "access",
    }, surface?.triggers?.access ?? null),
  ];
  const terminalFailures = [...selection.terminal_failures];
  if (settled && fresh && !facts.expectedSelected) terminalFailures.push("restart-root-not-selected");
  if (settled && fresh && (
    facts.projection?.session_settings?.available !== true
    || !targetShapeValid(target)
    || target.rootSessionId !== facts.expectedRoot
  )) {
    terminalFailures.push("restart-session-settings-target-mismatch");
  }
  for (const [triggerName, failure] of [
    ["model", "restart-model-trigger-not-ready"],
    ["access", "restart-access-trigger-not-ready"],
  ]) {
    const trigger = surface?.triggers?.[triggerName];
    if ((trigger?.count ?? 0) > 1 || (
      trigger?.count === 1
      && trigger.identity !== null
      && (
        trigger.identity?.tag !== "BUTTON"
        || trigger.identity?.action !== "show-session-settings"
        || trigger.identity?.session_settings_trigger !== triggerName
      )
    )) terminalFailures.push(failure);
  }
  return settingsDecision("restart-session-settings-trigger", surface, gates, {
    terminalFailures,
    expected_root_session_id: facts.expectedRoot,
    selected: facts.selected,
  });
}

function exactCommandDecision(snapshot, afterSequence, expected) {
  try {
    return {
      status: "pass",
      accepted: true,
      acquired: assertExactDesktopCommandSequence(snapshot, { afterSequence, expected }),
      error: null,
    };
  } catch (error) {
    const pending = error?.code === "desktop-command-probe-cardinality"
      && Array.isArray(snapshot?.calls)
      && snapshot.calls.length < expected.length;
    return {
      status: pending ? "pending" : "fail",
      accepted: false,
      acquired: null,
      error: errorObservation(error),
    };
  }
}

export function restartedSessionSettingsOpenDecision(
  { surface, commandSnapshot },
  { expectedTargetRoot, afterCommandSequence },
) {
  const rootSessionId = stableNavigationId(expectedTargetRoot, "expected root session id");
  if (!Number.isInteger(afterCommandSequence) || afterCommandSequence < 0) {
    throw new TypeError("restart open command sequence must be non-negative");
  }
  const target = surface?.projection?.session_settings?.target ?? null;
  const command = exactCommandDecision(commandSnapshot, afterCommandSequence, [
    { command: "show_session_settings", args: {} },
  ]);
  const gates = [
    settingsGate("surface-errors", "restart-open-surface-error", errorFree(surface), {
      fatal: 0,
      recoverable: 0,
      validation: 0,
    }, {
      fatal: surface?.visible_fatal_count ?? null,
      recoverable: surface?.visible_recoverable_error_count ?? null,
      validation: surface?.visible_validation_error_count ?? null,
    }),
    settingsGate("overlay", "restart-session-settings-overlay-not-open", surface?.projection?.overlay === "session_settings", "session_settings", surface?.projection?.overlay ?? null),
    settingsGate("target", "restart-open-target-mismatch", targetShapeValid(target)
      && target.rootSessionId === rootSessionId, { root_session_id: rootSessionId }, target),
    settingsGate("panel", "restart-session-settings-panel-not-open", surface?.panel?.count === 1
      && surface.panel.visible === true
      && surface.panel.inert === false, {
      count: 1,
      visible: true,
      inert: false,
    }, {
      count: surface?.panel?.count ?? null,
      visible: surface?.panel?.visible ?? null,
      inert: surface?.panel?.inert ?? null,
    }),
    settingsGate("dialog-topology", "restart-open-dialog-topology-invalid", surface?.visible_dialog_count === 1
      && surface?.visible_backdrop_count === 1
      && surface?.confirmation?.count === 0, {
      dialog_count: 1,
      backdrop_count: 1,
      confirmation_count: 0,
    }, {
      dialog_count: surface?.visible_dialog_count ?? null,
      backdrop_count: surface?.visible_backdrop_count ?? null,
      confirmation_count: surface?.confirmation?.count ?? null,
    }),
    settingsGate("command", "restart-open-command-not-exact", command.accepted, {
      after_sequence: afterCommandSequence,
      calls: [{ command: "show_session_settings", args: {} }],
    }, command.accepted ? command.acquired : command.error),
  ];
  const terminalFailures = [];
  if (!errorFree(surface)) terminalFailures.push("restart-open-surface-error");
  if (surface?.projection?.overlay !== "none" && surface?.projection?.overlay !== "session_settings") {
    terminalFailures.push("restart-session-settings-overlay-not-open");
  }
  if (surface?.projection?.overlay === "session_settings" && (
    !targetShapeValid(target) || target.rootSessionId !== rootSessionId
  )) terminalFailures.push("restart-open-target-mismatch");
  if ((surface?.panel?.count ?? 0) > 1) terminalFailures.push("restart-session-settings-panel-not-open");
  if ((surface?.visible_dialog_count ?? 0) > 1
    || (surface?.visible_backdrop_count ?? 0) > 1
    || (surface?.confirmation?.count ?? 0) > 0) {
    terminalFailures.push("restart-open-dialog-topology-invalid");
  }
  if (command.status === "fail") terminalFailures.push("restart-open-command-not-exact");
  return settingsDecision("restart-session-settings-open", { surface, command_snapshot: commandSnapshot }, gates, {
    terminalFailures,
    expected_root_session_id: rootSessionId,
    command: command.acquired,
  });
}

function transcriptValues(projection, kind) {
  return Array.isArray(projection?.transcript_rows)
    ? projection.transcript_rows.filter((row) => row?.row_kind === kind).map((row) => row.body)
    : [];
}

export function exactSessionProviderLedger(ledger, responseCount) {
  return Array.isArray(ledger)
    && ledger.length === responseCount
    && ledger.every((row) => row?.route === "responses"
      && row.method === "POST"
      && row.pathname === "/v1/responses"
      && row.contract?.pass === true
      && row.response_phase === "completed"
      && row.response_status === 200);
}

function sessionProviderLedgerFacts(ledger, expectedResponseCount) {
  const rows = Array.isArray(ledger) ? ledger : null;
  const invalidRows = rows === null ? [{ index: null, row: ledger }] : rows.flatMap((row, index) => (
    row?.route === "responses"
      && row.method === "POST"
      && row.pathname === "/v1/responses"
      && row.contract?.pass === true
      && row.response_phase === "completed"
      && row.response_status === 200
      ? [] : [{ index, row }]
  ));
  const status = rows === null
    || invalidRows.length > 0
    || rows.length > expectedResponseCount
    ? "fail"
    : rows.length === expectedResponseCount ? "pass" : "pending";
  return {
    status,
    expected_response_count: expectedResponseCount,
    actual_response_count: rows?.length ?? null,
    invalid_rows: invalidRows,
    rows,
  };
}

function validateRestoredSessionSettingsExpected(expected) {
  const projectId = stableNavigationId(expected?.projectId, "restart project id");
  if (!Array.isArray(expected?.rootSessionIds) || expected.rootSessionIds.length === 0) {
    throw new TypeError("restart restored panel requires exact root session identities");
  }
  const rootSessionIds = expected.rootSessionIds.map((id) => stableNavigationId(id, "restart root session id"));
  if (new Set(rootSessionIds).size !== rootSessionIds.length) {
    throw new TypeError("restart restored panel root session identities must be unique");
  }
  const expectedTargetRoot = stableNavigationId(expected?.expectedTargetRoot, "restart expected target root");
  if (!rootSessionIds.includes(expectedTargetRoot)) {
    throw new TypeError("restart restored panel target must be one of the exact roots");
  }
  for (const [label, value] of [
    ["workspace path", expected?.expectedWorkspacePath],
    ["settings revision", expected?.expectedSettingsRevision],
    ["base URL", expected?.expectedBaseUrl],
    ["model", expected?.expectedModel],
    ["provider profile", expected?.expectedProviderProfile],
    ["access mode", expected?.expectedAccessMode],
    ["prompt", expected?.expectedPrompt],
    ["response", expected?.expectedResponse],
  ]) {
    if (typeof value !== "string" || value.length === 0) {
      throw new TypeError(`restart restored panel ${label} must be a non-empty string`);
    }
  }
  if (!/^\d+$/.test(expected.expectedSettingsRevision)) {
    throw new TypeError("restart restored panel settings revision must be decimal digits");
  }
  if (typeof expected?.expectedApiKeyEnv !== "string") {
    throw new TypeError("restart restored panel API-key env must be a string");
  }
  if (!Number.isInteger(expected?.expectedResponseCount) || expected.expectedResponseCount < 0) {
    throw new TypeError("restart restored panel response count must be non-negative");
  }
  return {
    ...expected,
    projectId,
    rootSessionIds,
    expectedTargetRoot,
  };
}

function panelFieldActual(field) {
  return {
    count: field?.count ?? null,
    visible: field?.visible ?? null,
    enabled: field?.enabled ?? null,
    value: field?.value ?? null,
    options: Array.isArray(field?.options) ? field.options : [],
  };
}

export function restoredSessionSettingsPanelDecision(sample, expected) {
  const required = validateRestoredSessionSettingsExpected(expected);
  const surface = sample?.surface;
  const projection = surface?.projection;
  const sessionSettings = projection?.session_settings;
  const target = sessionSettings?.target ?? null;
  const panel = surface?.panel;
  const navigation = restartNavigationFacts(surface, {
    projectId: required.projectId,
    rootSessionIds: required.rootSessionIds,
    expectedRootSessionId: required.expectedTargetRoot,
  });
  const settled = navigationSettled(projection);
  const ledger = sessionProviderLedgerFacts(sample?.ledger, required.expectedResponseCount);
  const rootInventoryExact = navigation.rootFacts.every((row) => row.projection_count === 1
    && row.rendered_count === 1
    && row.visible === true
    && row.enabled === false);
  const panelOwnerReady = projection?.overlay === "session_settings"
    && panel?.count === 1
    && panel.visible === true
    && panel.inert === false;
  const expectedFields = [
    ["base-url", "restart-panel-base-url-mismatch", panel?.base_url, required.expectedBaseUrl, sessionSettings?.base_url],
    ["model", "restart-panel-model-mismatch", panel?.model, required.expectedModel, sessionSettings?.model],
    ["provider-profile", "restart-panel-provider-profile-mismatch", panel?.provider_profile, required.expectedProviderProfile, sessionSettings?.provider_profile, SESSION_PROVIDER_PROFILE_OPTIONS],
    ["api-key-env", "restart-panel-api-key-env-mismatch", panel?.api_key_env, required.expectedApiKeyEnv, sessionSettings?.api_key_env],
    ["access-mode", "restart-panel-access-mode-mismatch", panel?.access_mode, required.expectedAccessMode, sessionSettings?.access_mode],
    ["context-window", "restart-panel-context-window-mismatch", panel?.context_window, SESSION_CONTEXT_AFTER, sessionSettings?.context_window],
  ];
  const gates = [
    settingsGate("surface-errors", "restart-restored-surface-error", errorFree(surface), {
      fatal: 0,
      recoverable: 0,
      validation: 0,
    }, {
      fatal: surface?.visible_fatal_count ?? null,
      recoverable: surface?.visible_recoverable_error_count ?? null,
      validation: surface?.visible_validation_error_count ?? null,
    }),
    settingsGate("provider-ledger", "restart-provider-ledger-not-exact", ledger.status === "pass", {
      status: "pass",
      response_count: required.expectedResponseCount,
    }, ledger),
    settingsGate("navigation-settled", "restart-restored-navigation-not-settled", settled, true, settled),
    settingsGate("workspace-owner", "restart-restored-workspace-owner-mismatch", projection?.workspace_path === required.expectedWorkspacePath, required.expectedWorkspacePath, projection?.workspace_path ?? null),
    settingsGate("project-owner", "restart-restored-project-owner-mismatch", navigation.project.count === 1
      && navigation.project.selected_project_id === required.projectId, {
      project_id: required.projectId,
      count: 1,
      selected: true,
    }, navigation.project),
    settingsGate("root-inventory", "restart-restored-root-inventory-not-exact", rootInventoryExact, required.rootSessionIds.map((root_session_id) => ({
      root_session_id,
      projection_count: 1,
      rendered_count: 1,
      visible: true,
      enabled: false,
    })), navigation.rootFacts),
    settingsGate("selected-root", "restart-restored-root-owner-drift", navigation.expectedSelected, {
      root_session_id: required.expectedTargetRoot,
      rendered_count: 1,
      aria_current: "page",
    }, navigation.selected),
    settingsGate("transcript", "restart-restored-transcript-mismatch", sameValue(transcriptValues(projection, "user"), [required.expectedPrompt])
      && sameValue(transcriptValues(projection, "assistant"), [required.expectedResponse]), {
      users: [required.expectedPrompt],
      assistants: [required.expectedResponse],
    }, {
      users: transcriptValues(projection, "user"),
      assistants: transcriptValues(projection, "assistant"),
    }),
    settingsGate("overlay", "restart-restored-overlay-not-open", projection?.overlay === "session_settings", "session_settings", projection?.overlay ?? null),
    settingsGate("panel-owner", "restart-restored-panel-owner-not-ready", panelOwnerReady, {
      count: 1,
      visible: true,
      inert: false,
    }, {
      count: panel?.count ?? null,
      visible: panel?.visible ?? null,
      inert: panel?.inert ?? null,
    }),
    settingsGate("target-owner", "restart-restored-target-owner-mismatch", sessionSettings?.available === true
      && exactTargetOwner(target, {
        workspacePath: required.expectedWorkspacePath,
        rootSessionId: required.expectedTargetRoot,
        settingsRevision: required.expectedSettingsRevision,
      }), {
      available: true,
      workspace_path: required.expectedWorkspacePath,
      root_session_id: required.expectedTargetRoot,
      settings_revision: required.expectedSettingsRevision,
      config_generation: "restart-local canonical decimal",
      runtime_owner_token: "restart-local non-empty",
    }, { available: sessionSettings?.available ?? null, target }),
    settingsGate("base-url-projection", "restart-restored-base-url-mismatch", sessionSettings?.base_url === required.expectedBaseUrl, required.expectedBaseUrl, sessionSettings?.base_url ?? null),
    settingsGate("model-projection", "restart-restored-model-mismatch", sessionSettings?.model === required.expectedModel, required.expectedModel, sessionSettings?.model ?? null),
    settingsGate("provider-profile-projection", "restart-restored-provider-profile-mismatch", sessionSettings?.provider_profile === required.expectedProviderProfile, required.expectedProviderProfile, sessionSettings?.provider_profile ?? null),
    settingsGate("api-key-env-projection", "restart-restored-api-key-env-mismatch", sessionSettings?.api_key_env === required.expectedApiKeyEnv, required.expectedApiKeyEnv, sessionSettings?.api_key_env ?? null),
    settingsGate("access-mode-projection", "restart-restored-access-mode-mismatch", sessionSettings?.access_mode === required.expectedAccessMode, required.expectedAccessMode, sessionSettings?.access_mode ?? null),
    settingsGate("context-window-projection", "restart-restored-context-window-mismatch", sessionSettings?.context_window === SESSION_CONTEXT_AFTER, SESSION_CONTEXT_AFTER, sessionSettings?.context_window ?? null),
    settingsGate("inheritance", "restart-restored-inheritance-mismatch", sessionSettings?.context_window_inherited === false, {
      context_window_inherited: false,
    }, {
      context_window_inherited: sessionSettings?.context_window_inherited ?? null,
    }),
    settingsGate("scope", "restart-panel-scope-mismatch", panel?.scope_count === 1
      && panel.scope_visible === true
      && panel.scope_text === "このセッションだけ", {
      count: 1,
      visible: true,
      text: "このセッションだけ",
    }, {
      count: panel?.scope_count ?? null,
      visible: panel?.scope_visible ?? null,
      text: panel?.scope_text ?? null,
    }),
    ...expectedFields.map(([id, failure, field, expectedValue, projectionValue, expectedOptions]) => settingsGate(
      `field-${id}`,
      failure,
      field?.count === 1
        && field.visible === true
        && field.enabled === true
        && field.value === expectedValue
        && (expectedOptions === undefined || sameValue(field.options, expectedOptions))
        && projectionValue === expectedValue,
      {
        count: 1,
        visible: true,
        enabled: true,
        value: expectedValue,
        ...(expectedOptions === undefined ? {} : { options: expectedOptions }),
      },
      { ...panelFieldActual(field), projection_value: projectionValue ?? null },
    )),
    settingsGate("apply-action", "restart-panel-apply-action-invalid", panel?.apply?.count === 1
      && panel.apply.visible === true
      && panel.apply.enabled === false, {
      count: 1,
      visible: true,
      enabled: false,
    }, panel?.apply ?? null),
    settingsGate("discard-action", "restart-panel-discard-action-invalid", panel?.discard?.count === 0, { count: 0 }, panel?.discard ?? null),
    settingsGate("preferences-action", "restart-panel-preferences-action-invalid", panel?.preferences?.count === 1
      && panel.preferences.visible === true
      && panel.preferences.enabled === true, {
      count: 1,
      visible: true,
      enabled: true,
    }, panel?.preferences ?? null),
    settingsGate("close-action", "restart-panel-close-action-invalid", panel?.close?.count === 1
      && panel.close.visible === true
      && panel.close.enabled === true, {
      count: 1,
      visible: true,
      enabled: true,
    }, panel?.close ?? null),
    settingsGate("global-actions", "restart-panel-global-action-leak", panel?.save_global_count === 0, 0, panel?.save_global_count ?? null),
    settingsGate("dialog-topology", "restart-restored-dialog-topology-invalid", surface?.visible_dialog_count === 1
      && surface?.visible_backdrop_count === 1
      && surface?.confirmation?.count === 0, {
      dialog_count: 1,
      backdrop_count: 1,
      confirmation_count: 0,
    }, {
      dialog_count: surface?.visible_dialog_count ?? null,
      backdrop_count: surface?.visible_backdrop_count ?? null,
      confirmation_count: surface?.confirmation?.count ?? null,
    }),
  ];
  const terminalFailures = [];
  if (!errorFree(surface)) terminalFailures.push("restart-restored-surface-error");
  if (ledger.status === "fail") terminalFailures.push("restart-provider-ledger-not-exact");
  if (settled) {
    for (const failure of [
      "restart-restored-workspace-owner-mismatch",
      "restart-restored-project-owner-mismatch",
      "restart-restored-root-inventory-not-exact",
      "restart-restored-root-owner-drift",
      "restart-restored-transcript-mismatch",
    ]) {
      if (gates.some((gate) => gate.failure === failure && !gate.pass)) terminalFailures.push(failure);
    }
  }
  if (projection?.overlay !== "none" && projection?.overlay !== "session_settings") {
    terminalFailures.push("restart-restored-overlay-not-open");
  }
  if ((panel?.count ?? 0) > 1) terminalFailures.push("restart-restored-panel-owner-not-ready");
  if ((surface?.visible_dialog_count ?? 0) > 1
    || (surface?.visible_backdrop_count ?? 0) > 1
    || (surface?.confirmation?.count ?? 0) > 0) {
    terminalFailures.push("restart-restored-dialog-topology-invalid");
  }
  if (panelOwnerReady) {
    for (const gate of gates) {
      if (!gate.pass && ![
        "restart-provider-ledger-not-exact",
        "restart-restored-navigation-not-settled",
      ].includes(gate.failure)) terminalFailures.push(gate.failure);
    }
  }
  return settingsDecision("restart-restored-session-settings-panel", sample, gates, {
    terminalFailures,
    expected_root_session_id: required.expectedTargetRoot,
    provider_ledger: ledger,
    selected: navigation.selected,
  });
}

export function completedSessionRootReady(surface, ledger, { prompt, response, responseCount }) {
  const projection = surface?.projection;
  const target = projection?.session_settings?.target;
  return errorFree(surface)
    && exactSessionProviderLedger(ledger, responseCount)
    && projection?.run_status_key === "completed"
    && projection?.task_activity_state === "idle"
    && projection?.busy === false
    && projection?.agent_tree_active === false
    && projection?.post_run_refresh_pending === false
    && projection?.background_mutation_pending === false
    && projection?.async_polling_required === false
    && projection?.navigation_loading === false
    && projection?.overlay === "none"
    && projection?.session_settings?.available === true
    && targetShapeValid(target)
    && exactSessionSettingsTriggersReady(surface)
    && projection.session_settings.base_url.length > 0
    && projection.session_settings.model.length > 0
    && projection.session_settings.provider_profile === SESSION_PROVIDER_PROFILE
    && projection.session_settings.api_key_env === SESSION_PROVIDER_API_KEY_ENV
    && sameValue(transcriptValues(projection, "user"), [prompt])
    && sameValue(transcriptValues(projection, "assistant"), [response])
    && surface?.visible_dialog_count === 0
    && surface?.prompt?.count === 1
    && surface.prompt.value === ""
    && surface.prompt.enabled === true;
}

export async function observeSessionSettingsSurface(cdp) {
  return cdp.evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function') throw new Error('tauri-invoke-unavailable');
    const projection = await invoke('desktop_state');
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const visible = (element) => {
      if (!(element instanceof HTMLElement) || !element.isConnected) return false;
      const style = getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity) !== 0
        && rect.width > 0 && rect.height > 0;
    };
    const rows = (selector) => Array.from(document.querySelectorAll(selector));
    const one = (selector) => {
      const matches = rows(selector);
      const node = matches.length === 1 ? matches[0] : null;
      return { count: matches.length, visible: visible(node), node };
    };
    const field = (name) => {
      const found = one('[data-modal="session-settings"] [data-session-setting="' + name + '"]');
      const node = found.node;
      return {
        count: found.count,
        visible: found.visible,
        enabled: (node instanceof HTMLInputElement || node instanceof HTMLSelectElement)
          && !node.disabled
          && !node.readOnly,
        value: node instanceof HTMLInputElement || node instanceof HTMLSelectElement ? node.value : null,
        options: node instanceof HTMLSelectElement
          ? Array.from(node.options).map((option) => option.value)
          : [],
      };
    };
    const button = (selector) => {
      const found = one(selector);
      return {
        count: found.count,
        visible: found.visible,
        enabled: found.node instanceof HTMLButtonElement
          && !found.node.disabled
          && found.node.getAttribute('aria-disabled') !== 'true',
      };
    };
    const sessionSettingsTrigger = (trigger) => {
      const found = one('button[data-action="show-session-settings"][data-session-settings-trigger="' + trigger + '"]');
      const node = found.node;
      return {
        count: found.count,
        visible: found.visible,
        enabled: node instanceof HTMLButtonElement
          && !node.disabled
          && node.getAttribute('aria-disabled') !== 'true',
        identity: node instanceof HTMLElement ? {
          tag: node.tagName.toUpperCase(),
          action: node.dataset.action ?? null,
          session_settings_trigger: node.dataset.sessionSettingsTrigger ?? null,
        } : null,
      };
    };
    const projectSessionNavigation = Array.from(document.querySelectorAll(
      'button[data-action="session"][data-focus-key]'
    )).map((node) => ({
      action: node instanceof HTMLElement ? (node.dataset.action ?? null) : null,
      focus_key: node instanceof HTMLElement ? (node.dataset.focusKey ?? null) : null,
      aria_current: node.getAttribute('aria-current'),
      visible: visible(node),
      enabled: node instanceof HTMLButtonElement
        && !node.disabled
        && node.getAttribute('aria-disabled') !== 'true'
        && node.closest('[inert]') === null,
    }));
    const panel = one('.session-settings-modal[data-modal="session-settings"][data-surface="session-settings"]');
    const confirmation = one('[data-modal="session-settings-close-confirmation"][role="alertdialog"]');
    const scope = one('[data-modal="session-settings"] [data-session-scope="root-only"]');
    const prompt = one('textarea#prompt');
    return {
      projection,
      panel: {
        count: panel.count,
        visible: panel.visible,
        inert: panel.node instanceof HTMLElement && panel.node.closest('[inert]') !== null,
        scope_count: scope.count,
        scope_visible: scope.visible,
        scope_text: scope.node instanceof HTMLElement ? scope.node.innerText.trim() : null,
        base_url: field('base-url'),
        model: field('model'),
        provider_profile: field('provider-profile'),
        api_key_env: field('api-key-env'),
        access_mode: field('access-mode'),
        context_window: field('context-window'),
        max_output_tokens: field('max-output-tokens'),
        apply: button('[data-modal="session-settings"] button[data-action="apply-session-settings"]'),
        discard: button('[data-modal="session-settings"] button[data-action="discard-session-settings"]:not([hidden])'),
        preferences: button('[data-modal="session-settings"] button[data-action="open-preferences-from-session-settings"]'),
        close: button('[data-modal="session-settings"] button[data-action="close-overlay"]'),
        save_global_count: rows('[data-modal="session-settings"] [data-action="save-global-config"], [data-modal="session-settings"] [data-action="save-provider-global"]').length,
      },
      confirmation: {
        count: confirmation.count,
        visible: confirmation.visible,
        cancel: button('[data-modal="session-settings-close-confirmation"] button[data-action="cancel-local-confirm"]'),
        discard_close: button('[data-modal="session-settings-close-confirmation"] button[data-action="confirm-session-settings-discard-close"]'),
      },
      triggers: {
        model: sessionSettingsTrigger('model'),
        access: sessionSettingsTrigger('access'),
      },
      project_session_navigation: projectSessionNavigation,
      prompt: {
        count: prompt.count,
        value: prompt.node instanceof HTMLTextAreaElement ? prompt.node.value : null,
        enabled: prompt.node instanceof HTMLTextAreaElement && !prompt.node.disabled && !prompt.node.readOnly,
      },
      active: (() => {
        const node = document.activeElement;
        return {
          tag: node instanceof Element ? node.tagName.toUpperCase() : '',
          action: node instanceof HTMLElement ? (node.dataset.action ?? null) : null,
          focus_key: node instanceof HTMLElement ? (node.dataset.focusKey ?? null) : null,
          session_setting: node instanceof HTMLElement ? (node.dataset.sessionSetting ?? null) : null,
        };
      })(),
      visible_dialog_count: rows('[role="dialog"], [role="alertdialog"]').filter(visible).length,
      visible_backdrop_count: rows('.modal-backdrop').filter(visible).length,
      visible_fatal_count: rows('.fatal').filter(visible).length,
      visible_recoverable_error_count: rows('.ui-error-notice').filter(visible).length,
      visible_validation_error_count: rows('.validation.error').filter(visible).length,
    };
  })()`);
}

export function sessionSettingsPanelReady(surface, {
  expectedTarget,
  contextWindow,
  dirty = false,
  inherited = null,
} = {}) {
  const projection = surface?.projection?.session_settings;
  return errorFree(surface)
    && surface?.projection?.overlay === "session_settings"
    && projection?.available === true
    && targetShapeValid(projection?.target)
    && (expectedTarget === undefined || sameValue(projection.target, expectedTarget))
    && surface?.panel?.count === 1
    && surface.panel.visible === true
    && surface.panel.inert === false
    && surface.panel.scope_count === 1
    && surface.panel.scope_visible === true
    && surface.panel.scope_text === "このセッションだけ"
    && surface.panel.base_url.count === 1
    && surface.panel.base_url.visible === true
    && surface.panel.base_url.value === projection.base_url
    && surface.panel.model.count === 1
    && surface.panel.model.visible === true
    && surface.panel.model.value === projection.model
    && projection.provider_profile === SESSION_PROVIDER_PROFILE
    && surface.panel.provider_profile.count === 1
    && surface.panel.provider_profile.visible === true
    && surface.panel.provider_profile.enabled === true
    && surface.panel.provider_profile.value === projection.provider_profile
    && sameValue(surface.panel.provider_profile.options, SESSION_PROVIDER_PROFILE_OPTIONS)
    && projection.api_key_env === SESSION_PROVIDER_API_KEY_ENV
    && surface.panel.api_key_env.count === 1
    && surface.panel.api_key_env.visible === true
    && surface.panel.api_key_env.enabled === true
    && surface.panel.api_key_env.value === projection.api_key_env
    && surface.panel.access_mode.count === 1
    && surface.panel.access_mode.visible === true
    && surface.panel.access_mode.value === projection.access_mode
    && surface.panel.context_window.count === 1
    && surface.panel.context_window.visible === true
    && surface.panel.context_window.value === contextWindow
    && surface.panel.max_output_tokens.count === 0
    && surface.panel.max_output_tokens.visible === false
    && surface.panel.apply.count === 1
    && surface.panel.apply.visible === true
    && surface.panel.apply.enabled === dirty
    && surface.panel.discard.count === (dirty ? 1 : 0)
    && (!dirty || (
      surface.panel.discard.visible === true
      && surface.panel.discard.enabled === true
    ))
    && surface.panel.preferences.count === 1
    && surface.panel.preferences.visible === true
    && surface.panel.close.count === 1
    && surface.panel.close.visible === true
    && surface.panel.save_global_count === 0
    && surface?.confirmation?.count === 0
    && surface?.visible_dialog_count === 1
    && (inherited === null || projection.context_window_inherited === inherited);
}

export function sessionSettingsDirtyGuardReady(surface, expectedTarget) {
  return errorFree(surface)
    && surface?.projection?.overlay === "session_settings"
    && sameValue(surface?.projection?.session_settings?.target, expectedTarget)
    && surface?.panel?.count === 1
    && surface.panel.visible === true
    && surface.panel.inert === true
    && surface.panel.provider_profile.value === SESSION_PROVIDER_PROFILE
    && sameValue(surface.panel.provider_profile.options, SESSION_PROVIDER_PROFILE_OPTIONS)
    && surface.panel.api_key_env.value === SESSION_PROVIDER_API_KEY_ENV
    && surface.panel.context_window.value === SESSION_CONTEXT_AFTER
    && surface.panel.max_output_tokens.count === 0
    && surface.panel.max_output_tokens.visible === false
    && surface.panel.apply.enabled === true
    && surface.panel.discard.count === 1
    && surface?.confirmation?.count === 1
    && surface.confirmation.visible === true
    && surface.confirmation.cancel.count === 1
    && surface.confirmation.cancel.enabled === true
    && surface.confirmation.discard_close.count === 1
    && surface.confirmation.discard_close.enabled === true
    && surface?.visible_dialog_count === 2;
}

export function expectedSessionSettingsApplyCommand(surface) {
  const target = surface?.projection?.session_settings?.target;
  if (!targetShapeValid(target)) {
    throw new TypeError("Session Settings apply expectation requires an exact target");
  }
  const panel = surface.panel;
  return {
    command: "apply_session_settings",
    args: {
      input: {
        baseUrl: panel.base_url.value,
        model: panel.model.value,
        providerProfile: panel.provider_profile.value,
        apiKeyEnv: panel.api_key_env.value,
        accessMode: panel.access_mode.value,
        contextWindow: panel.context_window.value,
      },
      expectedTarget: structuredClone(target),
    },
  };
}

export function advancedSessionSettingsTarget(current, baseline) {
  return targetShapeValid(current)
    && targetShapeValid(baseline)
    && current.workspacePath === baseline.workspacePath
    && current.rootSessionId === baseline.rootSessionId
    && BigInt(current.settingsRevision) > BigInt(baseline.settingsRevision)
    && current.runtimeOwnerToken === baseline.runtimeOwnerToken;
}

function keyCode(character) {
  if (/^[a-z]$/.test(character)) return `Key${character.toUpperCase()}`;
  if (/^[0-9]$/.test(character)) return `Digit${character}`;
  if (character === " ") return "Space";
  if (character === "-") return "Minus";
  throw new TypeError(`unsupported Session Settings scenario character: ${character}`);
}

function expectedTypedEvents(text, identity) {
  return Array.from(text).flatMap((character) => [
    { type: "keydown", identity, key: character, code: keyCode(character) },
    { type: "input", identity, inputType: "insertText", data: character },
    { type: "keyup", identity, key: character, code: keyCode(character) },
  ]);
}

async function trustedClick(input, locator) {
  const start = (await input.snapshotProbe()).sequence;
  const target = await input.click(locator);
  const snapshot = await input.snapshotProbe(start);
  const probe = assertTrustedProbeSequence(snapshot, {
    afterSequence: start,
    expected: [
      { type: "pointerdown", identity: locator.identity, button: 0, buttons: 1 },
      { type: "pointerup", identity: locator.identity, button: 0, buttons: 0 },
      { type: "click", identity: locator.identity, button: 0, buttons: 0 },
    ],
  });
  return { target, probe, sequence: snapshot.sequence };
}

async function trustedHover(input, locator) {
  const start = (await input.snapshotProbe()).sequence;
  const target = await input.hover(locator);
  const snapshot = await input.snapshotProbe(start);
  const probe = assertTrustedProbeSequence(snapshot, {
    afterSequence: start,
    expected: [{ type: "pointermove", identity: locator.identity, buttons: 0 }],
  });
  return { target, probe, sequence: snapshot.sequence };
}

async function trustedType(input, locator, text) {
  const click = await trustedClick(input, locator);
  const start = click.sequence;
  await input.typeText(text);
  const snapshot = await input.snapshotProbe(start);
  return assertTrustedProbeSequence(snapshot, {
    afterSequence: start,
    expected: expectedTypedEvents(text, locator.identity),
  });
}

async function trustedReplaceDigits(input, locator, text) {
  if (!/^\d+$/.test(text)) throw new TypeError("Session Settings replacement requires decimal digits");
  const click = await trustedClick(input, locator);
  const start = click.sequence;
  await input.keyDown("Control");
  await input.pressKey("a");
  await input.keyUp("Control");
  await input.typeText(text);
  const snapshot = await input.snapshotProbe(start);
  return assertTrustedProbeSequence(snapshot, {
    afterSequence: start,
    expected: [
      { type: "keydown", identity: locator.identity, key: "Control", code: "ControlLeft" },
      { type: "keydown", identity: locator.identity, key: "a", code: "KeyA" },
      { type: "keyup", identity: locator.identity, key: "a", code: "KeyA" },
      { type: "keyup", identity: locator.identity, key: "Control", code: "ControlLeft" },
      ...expectedTypedEvents(text, locator.identity),
    ],
  });
}

async function trustedEscape(input, identity) {
  const start = (await input.snapshotProbe()).sequence;
  await input.pressKey("Escape");
  return assertTrustedProbeSequence(await input.snapshotProbe(start), {
    afterSequence: start,
    expected: [{ type: "keydown", identity, key: "Escape", code: "Escape" }],
  });
}

async function waitForCommands(probe, afterSequence, expected, label) {
  const observed = await waitForObservation({
    label,
    timeoutMs: 10_000,
    pollMs: 50,
    retrySampleErrors: false,
    sample: () => probe.snapshot(afterSequence),
    accept: (snapshot) => snapshot.calls.length >= expected.length,
  });
  return assertExactDesktopCommandSequence(observed.value, { afterSequence, expected });
}

async function assertNoCommandsStable(probe, afterSequence, minimumStableMs = 250) {
  const deadline = Date.now() + minimumStableMs;
  let latest;
  do {
    latest = await probe.snapshot(afterSequence);
    assertExactDesktopCommandSequence(latest, { afterSequence, expected: [] });
    await delay(50);
  } while (Date.now() < deadline);
  return latest;
}

async function waitForPanel(cdp, options, label) {
  return waitForObservation({
    label,
    timeoutMs: 20_000,
    pollMs: 75,
    retrySampleErrors: false,
    sample: () => observeSessionSettingsSurface(cdp),
    accept: (surface) => sessionSettingsPanelReady(surface, options),
  });
}

async function cancelDirtySessionSettingsCloseGuard(input, cdp, options, label) {
  const activation = await trustedClick(input, CANCEL_DIRTY_CLOSE);
  const settled = await waitForPanel(cdp, options, label);
  return { activation, settled };
}

async function openSessionSettings(input, commands, cdp, options, label) {
  const start = (await commands.snapshot()).sequence;
  const activation = await trustedClick(input, SESSION_SETTINGS_MODEL_TRIGGER);
  const opened = await waitForPanel(cdp, options, label);
  const command = await waitForCommands(
    commands,
    start,
    [{ command: "show_session_settings", args: {} }],
    `${label} command`,
  );
  return { activation, opened, command };
}

async function closeCleanSessionSettings(input, commands, cdp, label) {
  const start = (await commands.snapshot()).sequence;
  const activation = await trustedClick(input, CLOSE_SESSION_SETTINGS);
  const closed = await waitForObservation({
    label,
    timeoutMs: 10_000,
    pollMs: 75,
    retrySampleErrors: false,
    sample: () => observeSessionSettingsSurface(cdp),
    accept: (surface) => errorFree(surface)
      && surface?.projection?.overlay === "none"
      && surface?.panel?.count === 0
      && surface?.confirmation?.count === 0
      && surface?.visible_dialog_count === 0,
  });
  const command = await waitForCommands(
    commands,
    start,
    [{ command: "close_overlay", args: {} }],
    `${label} command`,
  );
  return { activation, closed, command };
}

async function waitForHoveredProjectAction(input, locator, label) {
  let terminal = null;
  let observed;
  try {
    observed = await waitForObservation({
      label,
      timeoutMs: 2_000,
      pollMs: 16,
      retrySampleErrors: false,
      sample: () => input.observeExactTarget(locator),
      accept: (sample) => {
        terminal = hoveredProjectActionDecision(sample.observation, locator);
        return terminal.decision !== "pending";
      },
    });
  } catch (error) {
    throw new DesktopE2eError(
      "harness",
      "session-settings-project-action-hover-timeout",
      "the exact project action did not become visibly actionable after trusted hover",
      { locator, error: errorObservation(error) },
    );
  }
  if (terminal.decision === "fail") {
    throw new DesktopE2eError(
      "harness",
      "session-settings-project-action-hover-invalid",
      "the hovered project exposed an ambiguous, incorrect, disabled, or occluded action owner",
      { locator, observation: observed.value, error: terminal.error },
    );
  }
  return {
    ...observed,
    value: { ...observed.value, acquired: terminal.acquired },
  };
}

async function waitForProjectRootNavigation(
  cdp,
  expected,
  label,
  { freshRoot = false } = {},
) {
  let terminalDecision = "pending";
  let observed;
  try {
    observed = await waitForObservation({
      label,
      timeoutMs: 30_000,
      pollMs: 75,
      retrySampleErrors: false,
      sample: () => observeSessionSettingsSurface(cdp),
      accept: (surface) => {
        terminalDecision = projectRootNavigationDecision(surface, expected);
        if (terminalDecision !== "pass" || !freshRoot) return terminalDecision !== "pending";
        const projection = surface?.projection;
        terminalDecision = projection?.session_settings?.available === false
          && projection?.session_settings?.target === null
          && projection?.draft_target?.sessionId === null
          && projection?.thread_empty === true
          && projection?.can_submit === true
          ? "pass"
          : "pending";
        return terminalDecision !== "pending";
      },
    });
  } catch (error) {
    throw classifyAcquiredObservationFailure(error, {
      code: "session-settings-project-navigation-missing",
      message: "the expected root sessions did not settle to one exact project navigation owner",
    });
  }
  if (terminalDecision === "fail") {
    throw productFailure(
      "session-settings-project-navigation-ambiguous",
      "project navigation exposed an ambiguous or invalid root session owner",
      { observation: observed },
    );
  }
  return observed;
}

async function createCompletedRoot({ input, cdp, provider, prompt, response, responseCount }) {
  await trustedType(input, PROMPT, prompt);
  await trustedClick(input, SEND);
  return waitForObservation({
    label: `completed root ${responseCount}`,
    timeoutMs: 90_000,
    pollMs: 100,
    retrySampleErrors: false,
    sample: async () => ({ surface: await observeSessionSettingsSurface(cdp), ledger: provider.requestLedger }),
    accept: ({ surface, ledger }) => completedSessionRootReady(surface, ledger, {
      prompt,
      response,
      responseCount,
    }),
  });
}

async function settleGenerationResources(state, input, commandProbe, generation, primaryError) {
  const outcome = { generation, input: null, command_probe: null, failures: [] };
  try { outcome.input = await input.cleanup(); }
  catch (error) { outcome.failures.push({ owner: "webview-input", error: errorObservation(error) }); }
  try { outcome.command_probe = await commandProbe.remove(); }
  catch (error) { outcome.failures.push({ owner: "desktop-command-probe", error: errorObservation(error) }); }
  state.resources.push(outcome);
  if (outcome.failures.length > 0 && primaryError === null) {
    throw new DesktopE2eError(
      "harness",
      "session-settings-resource-cleanup-failed",
      "Session Settings generation resources did not settle",
      outcome,
    );
  }
}

export function restartedSessionSettingsCloseDecision(
  { surface, commandSnapshot, ledger },
  { afterCommandSequence, ...expected },
) {
  if (!Number.isInteger(afterCommandSequence) || afterCommandSequence < 0) {
    throw new TypeError("restart close command sequence must be non-negative");
  }
  const required = validateRestoredSessionSettingsExpected(expected);
  const projection = surface?.projection;
  const target = projection?.session_settings?.target ?? null;
  const navigation = restartNavigationFacts(surface, {
    projectId: required.projectId,
    rootSessionIds: required.rootSessionIds,
    expectedRootSessionId: required.expectedTargetRoot,
  });
  const settled = navigationSettled(projection);
  const expectedNavigationEnabled = projection?.overlay === "none";
  const rootInventoryExact = navigation.rootFacts.every((row) => row.projection_count === 1
    && row.rendered_count === 1
    && row.visible === true
    && row.enabled === expectedNavigationEnabled);
  const providerLedger = sessionProviderLedgerFacts(ledger, required.expectedResponseCount);
  const command = exactCommandDecision(commandSnapshot, afterCommandSequence, [
    { command: "close_overlay", args: {} },
  ]);
  const gates = [
    settingsGate("surface-errors", "restart-close-surface-error", errorFree(surface), {
      fatal: 0,
      recoverable: 0,
      validation: 0,
    }, {
      fatal: surface?.visible_fatal_count ?? null,
      recoverable: surface?.visible_recoverable_error_count ?? null,
      validation: surface?.visible_validation_error_count ?? null,
    }),
    settingsGate("provider-ledger", "restart-close-provider-ledger-not-exact", providerLedger.status === "pass", {
      status: "pass",
      response_count: required.expectedResponseCount,
    }, providerLedger),
    settingsGate("command", "restart-close-command-not-exact", command.accepted, {
      after_sequence: afterCommandSequence,
      calls: [{ command: "close_overlay", args: {} }],
    }, command.accepted ? command.acquired : command.error),
    settingsGate("navigation-settled", "restart-close-navigation-not-settled", settled, true, settled),
    settingsGate("workspace-owner", "restart-close-workspace-owner-drift", projection?.workspace_path === required.expectedWorkspacePath, required.expectedWorkspacePath, projection?.workspace_path ?? null),
    settingsGate("project-owner", "restart-close-project-owner-drift", navigation.project.count === 1
      && navigation.project.selected_project_id === required.projectId, {
      project_id: required.projectId,
      count: 1,
      selected: true,
    }, navigation.project),
    settingsGate("root-inventory", "restart-close-root-inventory-drift", rootInventoryExact, required.rootSessionIds.map((root_session_id) => ({
      root_session_id,
      projection_count: 1,
      rendered_count: 1,
      visible: true,
      enabled: expectedNavigationEnabled,
    })), navigation.rootFacts),
    settingsGate("selected-root", "restart-close-root-owner-drift", navigation.expectedSelected, {
      root_session_id: required.expectedTargetRoot,
      rendered_count: 1,
      aria_current: "page",
    }, navigation.selected),
    settingsGate("transcript", "restart-close-transcript-drift", sameValue(transcriptValues(projection, "user"), [required.expectedPrompt])
      && sameValue(transcriptValues(projection, "assistant"), [required.expectedResponse]), {
      users: [required.expectedPrompt],
      assistants: [required.expectedResponse],
    }, {
      users: transcriptValues(projection, "user"),
      assistants: transcriptValues(projection, "assistant"),
    }),
    settingsGate("overlay", "restart-close-overlay-not-closed", projection?.overlay === "none", "none", projection?.overlay ?? null),
    settingsGate("dialog-topology", "restart-close-dialog-topology-invalid", surface?.panel?.count === 0
      && surface?.confirmation?.count === 0
      && surface?.visible_dialog_count === 0
      && surface?.visible_backdrop_count === 0, {
      panel_count: 0,
      confirmation_count: 0,
      dialog_count: 0,
      backdrop_count: 0,
    }, {
      panel_count: surface?.panel?.count ?? null,
      confirmation_count: surface?.confirmation?.count ?? null,
      dialog_count: surface?.visible_dialog_count ?? null,
      backdrop_count: surface?.visible_backdrop_count ?? null,
    }),
    settingsGate("target-owner", "restart-close-target-owner-drift", projection?.session_settings?.available === true
      && exactTargetOwner(target, {
        workspacePath: required.expectedWorkspacePath,
        rootSessionId: required.expectedTargetRoot,
        settingsRevision: required.expectedSettingsRevision,
      }), {
      available: true,
      workspace_path: required.expectedWorkspacePath,
      root_session_id: required.expectedTargetRoot,
      settings_revision: required.expectedSettingsRevision,
    }, { available: projection?.session_settings?.available ?? null, target }),
    settingsGate("restored-values", "restart-close-restored-values-drift", projection?.session_settings?.base_url === required.expectedBaseUrl
      && projection?.session_settings?.model === required.expectedModel
      && projection?.session_settings?.provider_profile === required.expectedProviderProfile
      && projection?.session_settings?.api_key_env === required.expectedApiKeyEnv
      && projection?.session_settings?.access_mode === required.expectedAccessMode
      && projection?.session_settings?.context_window === SESSION_CONTEXT_AFTER
      && projection?.session_settings?.context_window_inherited === false, {
      base_url: required.expectedBaseUrl,
      model: required.expectedModel,
      provider_profile: required.expectedProviderProfile,
      api_key_env: required.expectedApiKeyEnv,
      access_mode: required.expectedAccessMode,
      context_window: SESSION_CONTEXT_AFTER,
      context_window_inherited: false,
    }, projection?.session_settings ?? null),
    settingsGate("triggers", "restart-close-trigger-owner-drift", exactSessionSettingsTriggersReady(surface), {
      model: "exact visible enabled",
      access: "exact visible enabled",
    }, surface?.triggers ?? null),
  ];
  const terminalFailures = [];
  if (!errorFree(surface)) terminalFailures.push("restart-close-surface-error");
  if (providerLedger.status === "fail") terminalFailures.push("restart-close-provider-ledger-not-exact");
  if (command.status === "fail") terminalFailures.push("restart-close-command-not-exact");
  if ((surface?.panel?.count ?? 0) > 1
    || (surface?.confirmation?.count ?? 0) > 0
    || (surface?.visible_dialog_count ?? 0) > 1
    || (surface?.visible_backdrop_count ?? 0) > 1) {
    terminalFailures.push("restart-close-dialog-topology-invalid");
  }
  if (settled) {
    for (const failure of [
      "restart-close-workspace-owner-drift",
      "restart-close-project-owner-drift",
      "restart-close-root-inventory-drift",
      "restart-close-root-owner-drift",
      "restart-close-transcript-drift",
    ]) {
      if (gates.some((gate) => gate.failure === failure && !gate.pass)) terminalFailures.push(failure);
    }
  }
  if (command.status === "pass" && projection?.overlay === "none") {
    for (const gate of gates) {
      if (!gate.pass && ![
        "restart-close-provider-ledger-not-exact",
        "restart-close-navigation-not-settled",
      ].includes(gate.failure)) terminalFailures.push(gate.failure);
    }
  }
  return settingsDecision("restart-session-settings-close", {
    surface,
    command_snapshot: commandSnapshot,
    ledger,
  }, gates, {
    terminalFailures,
    expected_root_session_id: required.expectedTargetRoot,
    provider_ledger: providerLedger,
    command: command.acquired,
    selected: navigation.selected,
  });
}

async function waitForSettingsDecision({
  label,
  timeoutMs = 30_000,
  sample,
  decide,
  timeoutCode,
  timeoutMessage,
  rejectedCode,
  rejectedMessage,
}) {
  let observed;
  try {
    observed = await waitForObservation({
      label,
      timeoutMs,
      pollMs: 75,
      retrySampleErrors: false,
      sample: async () => decide(await sample()),
      accept: (decision) => {
        if (!["pending", "pass", "fail"].includes(decision?.status)) {
          throw new DesktopE2eError(
            "harness",
            "session-settings-decision-invalid",
            `${label} returned an invalid structured decision`,
            decision,
          );
        }
        return decision.status !== "pending";
      },
    });
  } catch (error) {
    throw classifyAcquiredObservationFailure(error, {
      code: timeoutCode,
      message: timeoutMessage,
    });
  }
  if (observed.value.status === "fail") {
    throw productFailure(rejectedCode, rejectedMessage, observed.value);
  }
  return observed;
}

export function createStableRestoredSessionSettingsDecision({
  minimumStableMs = SESSION_RESTART_STABILITY_MS,
  now = () => Date.now(),
  ...expected
} = {}) {
  validateRestoredSessionSettingsExpected(expected);
  let acceptedSince = null;
  return (sample) => {
    const restored = restoredSessionSettingsPanelDecision(sample, expected);
    if (restored.status !== "pass") {
      acceptedSince = null;
      const stability = settingsGate(
        "continuous-stability",
        "restart-restored-panel-not-stable",
        false,
        { minimum_stable_ms: minimumStableMs },
        { accepted_since: null, observed_at: null, stable_for_ms: 0 },
      );
      return settingsDecision("restart-restored-session-settings-stability", sample, [
        ...restored.gates,
        stability,
      ], {
        terminalFailures: restored.terminal_failures,
        restored_panel: restored,
        minimum_stable_ms: minimumStableMs,
      });
    }
    const observedAt = now();
    if (acceptedSince === null) {
      acceptedSince = observedAt;
    }
    const stableForMs = observedAt - acceptedSince;
    const stability = settingsGate(
      "continuous-stability",
      "restart-restored-panel-not-stable",
      stableForMs >= minimumStableMs,
      { minimum_stable_ms: minimumStableMs },
      { accepted_since: acceptedSince, observed_at: observedAt, stable_for_ms: stableForMs },
    );
    return settingsDecision("restart-restored-session-settings-stability", sample, [
      ...restored.gates,
      stability,
    ], {
      terminalFailures: restored.terminal_failures,
      restored_panel: restored,
      minimum_stable_ms: minimumStableMs,
      stable_for_ms: stableForMs,
    });
  };
}

export function createSettingsSessionScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    resources: [],
  };
  return Object.freeze({
    id: "settings.session",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({
        turns: [
          { prompt: SESSION_ROOT_ALPHA_PROMPT, responseText: SESSION_ROOT_ALPHA_RESPONSE },
          { prompt: SESSION_ROOT_BETA_PROMPT, responseText: SESSION_ROOT_BETA_RESPONSE },
        ],
      });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: providerRestartFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_SESSION_SETTINGS.txt",
        sentinelText: "moyAI Desktop E2E durable root Session Settings fixture.\n",
      });
      await sink.record("session-settings-fixture", state.provider.resourceObservation(), { phase, owner: OWNER });
    },
    async execute({ context, driver: firstCdp, host, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("Session Settings scripted provider was not prepared");
      await acquireInteractiveShell({ context, driver: firstCdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "session-settings-shell-ready",
      });
      if (provider.requestLedger.length !== 0) {
        throw productFailure("session-settings-cold-start-network", "Desktop contacted the provider before a root request", provider.requestLedger);
      }
      const firstInput = new WebviewInput(firstCdp, { probeId: "settings-session-g1" });
      const firstCommands = new DesktopCommandProbe(firstCdp, {
        probeId: "settings-session-g1",
        commands: ["show_session_settings", "apply_session_settings", "close_overlay"],
      });
      let firstSettled = false;
      let secondSettled = false;
      let secondInput = null;
      let secondCommands = null;
      let primaryError = null;
      try {
        await firstInput.installProbe();
        await firstCommands.install();
        const alphaCompleted = await createCompletedRoot({
          input: firstInput,
          cdp: firstCdp,
          provider,
          prompt: SESSION_ROOT_ALPHA_PROMPT,
          response: SESSION_ROOT_ALPHA_RESPONSE,
          responseCount: 1,
        });
        const alphaProjection = alphaCompleted.value.surface.projection;
        const alphaTarget = structuredClone(alphaProjection.session_settings.target);
        const alphaIdentity = selectedNavigationIdentity(alphaProjection);
        if (
          typeof alphaIdentity.project_id !== "string"
          || alphaIdentity.session_id !== alphaTarget.rootSessionId
        ) {
          throw productFailure(
            "session-settings-alpha-project-owner-missing",
            "root alpha did not settle under one selected project navigation owner",
            { identity: alphaIdentity, target: alphaTarget },
          );
        }
        const alphaProjectId = alphaIdentity.project_id;

        const alphaOpen = await openSessionSettings(firstInput, firstCommands, firstCdp, {
          expectedTarget: alphaTarget,
          contextWindow: SESSION_CONTEXT_BEFORE,
          dirty: false,
          inherited: true,
        }, "root alpha Session Settings");
        await trustedReplaceDigits(firstInput, CONTEXT_WINDOW, SESSION_CONTEXT_AFTER);
        const dirtyAlpha = await waitForPanel(firstCdp, {
          expectedTarget: alphaTarget,
          contextWindow: SESSION_CONTEXT_AFTER,
          dirty: true,
          inherited: true,
        }, "dirty root alpha Session Settings");

        const guardCommandStart = (await firstCommands.snapshot()).sequence;
        await trustedClick(firstInput, CLOSE_SESSION_SETTINGS);
        const explicitGuard = await waitForObservation({
          label: "Session Settings explicit dirty close guard",
          timeoutMs: 10_000,
          pollMs: 75,
          retrySampleErrors: false,
          sample: () => observeSessionSettingsSurface(firstCdp),
          accept: (surface) => sessionSettingsDirtyGuardReady(surface, alphaTarget),
        });
        await assertNoCommandsStable(firstCommands, guardCommandStart);
        const guardScreenshot = await captureScenarioScreenshot({
          cdp: firstCdp,
          sink,
          name: "session-settings-dirty-close-guard",
          owner: OWNER,
        });
        const explicitGuardCancel = await cancelDirtySessionSettingsCloseGuard(firstInput, firstCdp, {
          expectedTarget: alphaTarget,
          contextWindow: SESSION_CONTEXT_AFTER,
          dirty: true,
          inherited: true,
        }, "cancelled Session Settings dirty close");
        await trustedEscape(firstInput, BASE_URL.identity);
        const escapeGuard = await waitForObservation({
          label: "Session Settings Escape dirty close guard",
          timeoutMs: 10_000,
          pollMs: 75,
          retrySampleErrors: false,
          sample: () => observeSessionSettingsSurface(firstCdp),
          accept: (surface) => sessionSettingsDirtyGuardReady(surface, alphaTarget),
        });
        await assertNoCommandsStable(firstCommands, guardCommandStart);
        const escapeGuardCancel = await cancelDirtySessionSettingsCloseGuard(firstInput, firstCdp, {
          expectedTarget: alphaTarget,
          contextWindow: SESSION_CONTEXT_AFTER,
          dirty: true,
          inherited: true,
        }, "cancelled Session Settings Escape dirty close");
        await trustedClick(firstInput, DISCARD_SESSION_SETTINGS);
        await waitForPanel(firstCdp, {
          expectedTarget: alphaTarget,
          contextWindow: SESSION_CONTEXT_BEFORE,
          dirty: false,
          inherited: true,
        }, "discarded root alpha Session Settings draft");
        await assertNoCommandsStable(firstCommands, guardCommandStart);

        await trustedReplaceDigits(firstInput, CONTEXT_WINDOW, SESSION_CONTEXT_AFTER);
        const applyableAlpha = await waitForPanel(firstCdp, {
          expectedTarget: alphaTarget,
          contextWindow: SESSION_CONTEXT_AFTER,
          dirty: true,
          inherited: true,
        }, "applyable root alpha Session Settings");
        const expectedApply = expectedSessionSettingsApplyCommand(applyableAlpha.value);
        const applyCommandStart = (await firstCommands.snapshot()).sequence;
        const applyActivation = await trustedClick(firstInput, APPLY_SESSION_SETTINGS);
        const appliedAlpha = await waitForObservation({
          label: "durable root alpha Session Settings apply",
          timeoutMs: 30_000,
          pollMs: 75,
          retrySampleErrors: false,
          sample: () => observeSessionSettingsSurface(firstCdp),
          accept: (surface) => advancedSessionSettingsTarget(surface?.projection?.session_settings?.target, alphaTarget)
            && sessionSettingsPanelReady(surface, {
              contextWindow: SESSION_CONTEXT_AFTER,
              dirty: false,
              inherited: false,
            }),
        });
        const alphaAppliedTarget = structuredClone(appliedAlpha.value.projection.session_settings.target);
        const applyCommand = await waitForCommands(
          firstCommands,
          applyCommandStart,
          [expectedApply],
          "root alpha exact Session Settings apply command",
        );
        const appliedScreenshot = await captureScenarioScreenshot({
          cdp: firstCdp,
          sink,
          name: "session-settings-alpha-applied",
          owner: OWNER,
        });
        const alphaClose = await closeCleanSessionSettings(firstInput, firstCommands, firstCdp, "root alpha Session Settings close");

        const betaNewSessionLocator = projectNewSessionLocator(alphaProjectId);
        const betaProjectHover = await trustedHover(
          firstInput,
          projectRowHoverLocator(alphaProjectId),
        );
        const betaNewSessionReady = await waitForHoveredProjectAction(
          firstInput,
          betaNewSessionLocator,
          "root beta exact new-session action revealed by project hover",
        );
        const betaNewSessionActivation = await trustedClick(firstInput, betaNewSessionLocator);
        const freshBetaOwner = await waitForProjectRootNavigation(
          firstCdp,
          {
            projectId: alphaProjectId,
            rootSessionIds: [alphaTarget.rootSessionId],
            selectedRootSessionId: null,
          },
          "fresh root beta under the alpha project",
          { freshRoot: true },
        );
        await sink.record("session-settings-project-new-session-route", {
          project_id: alphaProjectId,
          hover: betaProjectHover,
          action_ready: betaNewSessionReady,
          activation: betaNewSessionActivation,
          fresh_owner: freshBetaOwner.value,
        }, { phase: "executing", owner: OWNER });
        const betaCompleted = await createCompletedRoot({
          input: firstInput,
          cdp: firstCdp,
          provider,
          prompt: SESSION_ROOT_BETA_PROMPT,
          response: SESSION_ROOT_BETA_RESPONSE,
          responseCount: 2,
        });
        const betaProjection = betaCompleted.value.surface.projection;
        const betaTarget = structuredClone(betaProjection.session_settings.target);
        const betaIdentity = selectedNavigationIdentity(betaProjection);
        if (betaTarget.rootSessionId === alphaTarget.rootSessionId) {
          throw productFailure("session-settings-root-identity-reused", "new project session reused the root Session Settings owner", {
            alpha: alphaTarget,
            beta: betaTarget,
          });
        }
        if (
          betaIdentity.project_id !== alphaProjectId
          || betaIdentity.session_id !== betaTarget.rootSessionId
        ) {
          throw productFailure(
            "session-settings-beta-project-owner-drift",
            "root beta did not remain under the root alpha project owner",
            { alpha_project_id: alphaProjectId, identity: betaIdentity, target: betaTarget },
          );
        }
        const betaOpen = await openSessionSettings(firstInput, firstCommands, firstCdp, {
          expectedTarget: betaTarget,
          contextWindow: SESSION_CONTEXT_BEFORE,
          dirty: false,
          inherited: true,
        }, "root beta non-leaking Session Settings");
        const betaScreenshot = await captureScenarioScreenshot({
          cdp: firstCdp,
          sink,
          name: "session-settings-beta-global-defaults",
          owner: OWNER,
        });
        const betaClose = await closeCleanSessionSettings(firstInput, firstCommands, firstCdp, "root beta Session Settings close");

        const projectRoots = await waitForProjectRootNavigation(
          firstCdp,
          {
            projectId: alphaProjectId,
            rootSessionIds: [alphaTarget.rootSessionId, betaTarget.rootSessionId],
            selectedRootSessionId: betaTarget.rootSessionId,
          },
          "root alpha and beta exact project navigation",
        );
        const alphaLocator = projectSessionSelectionLocator(alphaTarget.rootSessionId);
        const alphaSelection = await trustedClick(firstInput, alphaLocator);
        const alphaNavigation = await waitForProjectRootNavigation(
          firstCdp,
          {
            projectId: alphaProjectId,
            rootSessionIds: [alphaTarget.rootSessionId, betaTarget.rootSessionId],
            selectedRootSessionId: alphaTarget.rootSessionId,
          },
          "root alpha exact project navigation reselected",
        );
        await waitForObservation({
          label: "root alpha reselected",
          timeoutMs: 30_000,
          pollMs: 75,
          retrySampleErrors: false,
          sample: () => observeSessionSettingsSurface(firstCdp),
          accept: (surface) => errorFree(surface)
            && surface?.projection?.overlay === "none"
            && surface?.projection?.session_settings?.target?.rootSessionId === alphaTarget.rootSessionId
            && exactSessionSettingsTriggersReady(surface),
        });
        const alphaReopened = await openSessionSettings(firstInput, firstCommands, firstCdp, {
          contextWindow: SESSION_CONTEXT_AFTER,
          dirty: false,
          inherited: false,
        }, "root alpha durable Session Settings after root switch");
        if (alphaReopened.opened.value.projection.session_settings.target.rootSessionId !== alphaTarget.rootSessionId) {
          throw productFailure("session-settings-root-reselect-mismatch", "reopened Session Settings did not own root alpha", alphaReopened.opened.value);
        }
        await closeCleanSessionSettings(firstInput, firstCommands, firstCdp, "reselected root alpha Session Settings close");
        await sink.record("session-settings-root-isolation", {
          alpha: {
            identity: alphaIdentity,
            initial_target: alphaTarget,
            applied_target: alphaAppliedTarget,
            open: alphaOpen,
            dirty: dirtyAlpha.value,
            explicit_guard: explicitGuard.value,
            explicit_guard_cancel: explicitGuardCancel,
            escape_guard: escapeGuard.value,
            escape_guard_cancel: escapeGuardCancel,
            apply_activation: applyActivation,
            apply_command: applyCommand,
            close: alphaClose,
            reselect: alphaSelection,
            reselected_navigation: alphaNavigation.value,
            reopened: alphaReopened.opened.value,
          },
          beta: {
            identity: betaIdentity,
            target: betaTarget,
            project_hover: betaProjectHover,
            new_session_ready: betaNewSessionReady,
            new_session_activation: betaNewSessionActivation,
            fresh_owner: freshBetaOwner.value,
            open: betaOpen.opened.value,
            close: betaClose,
          },
          project_navigation: projectRoots.value,
          provider_ledger: provider.requestLedger,
          screenshots: { guard: guardScreenshot, applied: appliedScreenshot, beta: betaScreenshot },
        }, { phase: "executing", owner: OWNER });

        firstSettled = true;
        await settleGenerationResources(state, firstInput, firstCommands, 1, null);
        const restarted = await host.restart({ context, scenario: this, sink, driver: firstCdp, phase: "executing" });
        await acquireInteractiveShell({ context, driver: restarted.driver, sink }, {
          evidenceOwner: OWNER,
          screenshotStem: "session-settings-restarted-shell",
        });
        secondInput = new WebviewInput(restarted.driver, { probeId: "settings-session-g2" });
        secondCommands = new DesktopCommandProbe(restarted.driver, {
          probeId: "settings-session-g2",
          commands: ["show_session_settings", "close_overlay"],
        });
        await secondInput.installProbe();
        await secondCommands.install();
        const restartExpected = {
          expectedWorkspacePath: alphaAppliedTarget.workspacePath,
          projectId: alphaProjectId,
          rootSessionIds: [alphaTarget.rootSessionId, betaTarget.rootSessionId],
          expectedTargetRoot: alphaTarget.rootSessionId,
          expectedSettingsRevision: alphaAppliedTarget.settingsRevision,
          expectedBaseUrl: provider.baseUrl,
          expectedModel: SCRIPTED_PROVIDER_MODEL_ID,
          expectedProviderProfile: SESSION_PROVIDER_PROFILE,
          expectedApiKeyEnv: SESSION_PROVIDER_API_KEY_ENV,
          expectedAccessMode: "default",
          expectedPrompt: SESSION_ROOT_ALPHA_PROMPT,
          expectedResponse: SESSION_ROOT_ALPHA_RESPONSE,
          expectedResponseCount: 2,
        };
        const restartSelectionReady = await waitForSettingsDecision({
          label: "restarted exact root inventory and alpha selection route",
          sample: () => observeSessionSettingsSurface(restarted.driver),
          decide: (surface) => restartedSessionSelectionDecision(surface, {
            projectId: restartExpected.projectId,
            rootSessionIds: restartExpected.rootSessionIds,
            expectedRootSessionId: restartExpected.expectedTargetRoot,
          }),
          timeoutCode: "session-settings-restart-selection-route-missing",
          timeoutMessage: "restart did not expose one exact project/root inventory for selecting root alpha",
          rejectedCode: "session-settings-restart-selection-route-invalid",
          rejectedMessage: "restart exposed an ambiguous or incorrect project/root selection owner",
        });
        let restartAlphaSelection = null;
        let restartSelectionBaselineRevision = null;
        if (restartSelectionReady.value.route === "select-exact-root") {
          restartSelectionBaselineRevision = restartSelectionReady.value.observation.projection.projection_revision;
          restartAlphaSelection = await trustedClick(
            secondInput,
            projectSessionSelectionLocator(restartExpected.expectedTargetRoot),
          );
        }
        const restartTriggerReady = await waitForSettingsDecision({
          label: "restarted root alpha Session Settings trigger",
          sample: () => observeSessionSettingsSurface(restarted.driver),
          decide: (surface) => restartedSessionSettingsTriggerDecision(surface, {
            projectId: restartExpected.projectId,
            rootSessionIds: restartExpected.rootSessionIds,
            expectedRootSessionId: restartExpected.expectedTargetRoot,
            afterProjectionRevision: restartSelectionBaselineRevision,
          }),
          timeoutCode: "session-settings-restart-alpha-trigger-missing",
          timeoutMessage: "trusted root alpha selection did not settle to one exact Session Settings trigger owner",
          rejectedCode: "session-settings-restart-alpha-trigger-invalid",
          rejectedMessage: "trusted root alpha selection settled to an incorrect or ambiguous Session Settings trigger owner",
        });
        await sink.record("session-settings-restart-selection-route", {
          readiness: restartSelectionReady.value,
          activation: restartAlphaSelection,
          settled_trigger: restartTriggerReady.value,
        }, { phase: "executing", owner: OWNER });
        const restartOpenStart = (await secondCommands.snapshot()).sequence;
        const restartOpenActivation = await trustedClick(secondInput, SESSION_SETTINGS_MODEL_TRIGGER);
        const restartOpen = await waitForSettingsDecision({
          label: "restarted root alpha Session Settings open",
          sample: async () => ({
            surface: await observeSessionSettingsSurface(restarted.driver),
            commandSnapshot: await secondCommands.snapshot(restartOpenStart),
          }),
          decide: (sample) => restartedSessionSettingsOpenDecision(sample, {
            expectedTargetRoot: restartExpected.expectedTargetRoot,
            afterCommandSequence: restartOpenStart,
          }),
          timeoutCode: "session-settings-restart-open-missing",
          timeoutMessage: "trusted Session Settings activation did not open one exact root alpha panel",
          rejectedCode: "session-settings-restart-open-invalid",
          rejectedMessage: "trusted Session Settings activation opened an incorrect or ambiguous panel owner",
        });
        const stableDecision = createStableRestoredSessionSettingsDecision(restartExpected);
        const restored = await waitForSettingsDecision({
          label: "root alpha durable Session Settings after exact restart",
          timeoutMs: 30_000,
          sample: async () => ({
            surface: await observeSessionSettingsSurface(restarted.driver),
            ledger: provider.requestLedger,
          }),
          decide: stableDecision,
          timeoutCode: "session-settings-restart-restored-panel-missing",
          timeoutMessage: "root alpha Session Settings did not remain exactly restored after restart",
          rejectedCode: "session-settings-restart-restored-panel-invalid",
          rejectedMessage: "root alpha Session Settings restoration produced irreversible owner, value, topology, error, or provider drift",
        });
        const restoredScreenshot = await captureScenarioScreenshot({
          cdp: restarted.driver,
          sink,
          name: "session-settings-alpha-restored-after-restart",
          owner: OWNER,
        });
        const restartCloseStart = (await secondCommands.snapshot()).sequence;
        const restartCloseActivation = await trustedClick(secondInput, CLOSE_SESSION_SETTINGS);
        const restartClose = await waitForSettingsDecision({
          label: "restarted root alpha Session Settings close",
          timeoutMs: 10_000,
          sample: async () => ({
            surface: await observeSessionSettingsSurface(restarted.driver),
            commandSnapshot: await secondCommands.snapshot(restartCloseStart),
            ledger: provider.requestLedger,
          }),
          decide: (sample) => restartedSessionSettingsCloseDecision(sample, {
            ...restartExpected,
            afterCommandSequence: restartCloseStart,
          }),
          timeoutCode: "session-settings-restart-close-missing",
          timeoutMessage: "root alpha Session Settings did not close while preserving its exact owner",
          rejectedCode: "session-settings-restart-close-invalid",
          rejectedMessage: "root alpha Session Settings close produced irreversible owner, topology, command, error, or provider drift",
        });
        state.acceptedLedger = structuredClone(provider.requestLedger);
        await sink.record("session-settings-restart-persistence", {
          restart: restarted.restart,
          selection_readiness: restartSelectionReady.value,
          selection_activation: restartAlphaSelection,
          trigger_readiness: restartTriggerReady.value,
          open_activation: restartOpenActivation,
          open: restartOpen.value,
          restored: restored.value,
          stable_for_ms: SESSION_RESTART_STABILITY_MS,
          close_activation: restartCloseActivation,
          close: restartClose.value,
          accepted_provider_ledger: state.acceptedLedger,
          screenshot: restoredScreenshot,
        }, { phase: "executing", owner: OWNER });
        secondSettled = true;
        await settleGenerationResources(state, secondInput, secondCommands, 2, null);
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        if (!firstSettled) {
          firstSettled = true;
          try { await settleGenerationResources(state, firstInput, firstCommands, 1, primaryError); }
          catch (error) { if (primaryError === null) throw error; }
        }
        if (secondInput !== null && secondCommands !== null && !secondSettled) {
          secondSettled = true;
          try { await settleGenerationResources(state, secondInput, secondCommands, 2, primaryError); }
          catch (error) { if (primaryError === null) throw error; }
        }
      }
    },
    async quiesce({ inputs }) {
      if (state.quiesceOutcome !== null) return structuredClone(state.quiesceOutcome);
      state.quiesceOutcome = await quiesceProviderResource({
        provider: state.provider,
        acceptedLedger: state.acceptedLedger,
        inputs,
      });
      return structuredClone(state.quiesceOutcome);
    },
    async cleanup() {
      const resourcesPass = state.resources.every((resource) => resource.failures.length === 0);
      const quiescePass = state.quiesceOutcome?.input === "pass";
      return {
        input: resourcesPass && quiescePass ? "pass" : "fail",
        resources: [{
          kind: "settings-session-verification",
          generation_resources: state.resources,
          quiesce_input: state.quiesceOutcome?.input ?? null,
        }],
      };
    },
  });
}
