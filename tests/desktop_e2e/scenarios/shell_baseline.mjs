import path from "node:path";
import { fileURLToPath } from "node:url";

import { waitForObservation } from "../core/deadline.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";

const scenarioDirectory = path.dirname(fileURLToPath(import.meta.url));

export const scenario = Object.freeze({
  id: "shell.baseline",
  productOracle: "not_required",
  manualGate: "not_required",
  databaseRequired: true,
  prepare: prepareShellBaseline,
  execute: executeShellBaseline,
  requestGracefulExit,
  quiesce: quiesceShellBaseline,
  cleanup: cleanupShellBaseline,
});

const TERMINAL_SHELL_RUN_STATUSES = Object.freeze(["idle", "completed", "cancelled", "failed"]);

function shellGate(id, failure, pass, expected, actual, required = true) {
  return {
    id,
    failure,
    required,
    pass: !required || pass === true,
    expected,
    actual,
  };
}

function ordinaryProjectionFresh(projectionRevision, afterRevision) {
  if (afterRevision === null) return true;
  try {
    return BigInt(projectionRevision ?? "-1") > BigInt(afterRevision);
  } catch {
    return false;
  }
}

export function shellReadinessDecision(
  value,
  { afterRevision = null, expectedWorkspace = null } = {},
) {
  const projectionRevisionValid = typeof value?.projection_revision === "string"
    && /^\d+$/.test(value.projection_revision);
  const gates = [
    shellGate("page-title", "page-title-not-moyai", value?.title === "moyAI", "moyAI", value?.title ?? null),
    shellGate("document-ready-state", "document-not-complete", value?.ready_state === "complete", "complete", value?.ready_state ?? null),
    shellGate("body-connected", "body-not-connected", value?.body_connected === true, true, value?.body_connected ?? null),
    shellGate(
      "app-root",
      "app-root-not-exact",
      value?.app_count === 1 && value?.app_connected === true,
      { count: 1, connected: true },
      { count: value?.app_count ?? null, connected: value?.app_connected ?? null },
    ),
    shellGate("startup-splash", "startup-splash-still-visible", value?.splash_count === 0, { count: 0 }, { count: value?.splash_count ?? null }),
    shellGate(
      "interactive-shell",
      "interactive-shell-not-exact",
      value?.app_frame_count === 1 && value?.shell_count === 1 && value?.conversation_count === 1,
      { app_frame_count: 1, shell_count: 1, conversation_count: 1 },
      {
        app_frame_count: value?.app_frame_count ?? null,
        shell_count: value?.shell_count ?? null,
        conversation_count: value?.conversation_count ?? null,
      },
    ),
    shellGate("composer", "composer-not-exact", value?.composer_count === 1, { count: 1 }, { count: value?.composer_count ?? null }),
    shellGate("shell-interaction", "interactive-shell-inert", value?.shell_inert === false, { inert: false }, { inert: value?.shell_inert ?? null }),
    shellGate(
      "blocking-overlays",
      "blocking-overlay-visible",
      value?.visible_blocking_overlay_count === 0 && value?.visible_modal_backdrop_count === 0,
      { blocking: 0, modal_backdrop: 0 },
      {
        blocking: value?.visible_blocking_overlay_count ?? null,
        modal_backdrop: value?.visible_modal_backdrop_count ?? null,
      },
    ),
    shellGate("fatal-errors", "fatal-error-visible", value?.visible_fatal_count === 0, { count: 0 }, { count: value?.visible_fatal_count ?? null }),
    shellGate("recoverable-errors", "recoverable-error-visible", value?.visible_recoverable_error_count === 0, { count: 0 }, { count: value?.visible_recoverable_error_count ?? null }),
    shellGate(
      "main-prompt-interaction",
      "main-prompt-not-interactable",
      value?.prompt_count === 1 && value?.prompt_visible === true && value?.prompt_enabled === true,
      { count: 1, visible: true, enabled: true },
      {
        count: value?.prompt_count ?? null,
        visible: value?.prompt_visible ?? null,
        enabled: value?.prompt_enabled ?? null,
      },
    ),
    shellGate("main-prompt-hit-test", "main-prompt-hit-test-failed", value?.prompt_center_hit === true, true, value?.prompt_center_hit ?? null),
    shellGate(
      "desktop-state-command",
      "desktop-state-command-unavailable",
      value?.tauri_invoke_available === true && value?.desktop_state_ok === true,
      { tauri_invoke_available: true, desktop_state_ok: true },
      {
        tauri_invoke_available: value?.tauri_invoke_available ?? null,
        desktop_state_ok: value?.desktop_state_ok ?? null,
        error: value?.desktop_state_error ?? null,
      },
    ),
    shellGate("projection-revision-format", "projection-revision-invalid", projectionRevisionValid, "unsigned decimal string", value?.projection_revision ?? null),
    shellGate(
      "projection-revision-freshness",
      "ordinary-projection-not-fresh",
      projectionRevisionValid && ordinaryProjectionFresh(value?.projection_revision, afterRevision),
      afterRevision === null ? null : `>${afterRevision}`,
      value?.projection_revision ?? null,
      afterRevision !== null,
    ),
    shellGate(
      "startup-state",
      "startup-not-ready",
      value?.startup_status === "ready"
        && value?.initial_setup_required === false
        && value?.startup_action_overlay === "none",
      { status: "ready", initial_setup_required: false, action_overlay: "none" },
      {
        status: value?.startup_status ?? null,
        initial_setup_required: value?.initial_setup_required ?? null,
        action_overlay: value?.startup_action_overlay ?? null,
      },
    ),
    shellGate("startup-checks", "startup-check-failed", value?.startup_fail_check_count === 0, { fail_count: 0 }, { fail_count: value?.startup_fail_check_count ?? null }),
    shellGate(
      "projection-overlay",
      "projection-overlay-open",
      value?.projection_overlay === "none"
        && value?.confirmation_visible === false
        && value?.confirmation_id === null
        && value?.confirmation_present === false,
      { overlay: "none", confirmation_visible: false, confirmation_id: null, confirmation_present: false },
      {
        overlay: value?.projection_overlay ?? null,
        confirmation_visible: value?.confirmation_visible ?? null,
        confirmation_id: value?.confirmation_id ?? null,
        confirmation_present: value?.confirmation_present ?? null,
      },
    ),
    shellGate(
      "run-status-terminal",
      "run-status-not-terminal",
      TERMINAL_SHELL_RUN_STATUSES.includes(value?.run_status_key),
      TERMINAL_SHELL_RUN_STATUSES,
      value?.run_status_key ?? null,
    ),
    shellGate("task-activity", "task-owner-not-idle", value?.task_activity_state === "idle", "idle", value?.task_activity_state ?? null),
    shellGate("agent-tree", "task-owner-not-idle", value?.agent_tree_active === false, { active: false }, { active: value?.agent_tree_active ?? null }),
    shellGate("post-run-refresh", "projection-background-work-pending", value?.post_run_refresh_pending === false, { pending: false }, { pending: value?.post_run_refresh_pending ?? null }),
    shellGate("provider-load", "projection-background-work-pending", value?.provider_loading === false, { loading: false }, { loading: value?.provider_loading ?? null }),
    shellGate("composer-submit-mode", "composer-admission-closed", value?.composer_submit_mode === "new_request", "new_request", value?.composer_submit_mode ?? null),
    shellGate("composer-can-submit", "composer-admission-closed", value?.can_submit === true, true, value?.can_submit ?? null),
    shellGate("navigation-admission", "navigation-admission-closed", value?.navigation_admission_open === true, { open: true }, { open: value?.navigation_admission_open ?? null }),
    shellGate(
      "workspace-owner",
      "workspace-owner-mismatch",
      value?.workspace_path === expectedWorkspace,
      expectedWorkspace,
      value?.workspace_path ?? null,
      expectedWorkspace !== null,
    ),
    shellGate("navigation-load", "projection-not-settled", value?.navigation_loading === false, { loading: false }, { loading: value?.navigation_loading ?? null }),
    shellGate("projection-busy", "projection-not-settled", value?.busy === false, false, value?.busy ?? null),
    shellGate("background-mutation", "projection-not-settled", value?.background_mutation_pending === false, { pending: false }, { pending: value?.background_mutation_pending ?? null }),
    shellGate("async-polling", "projection-not-settled", value?.async_polling_required === false, { required: false }, { required: value?.async_polling_required ?? null }),
    shellGate("pending-async-operations", "projection-not-settled", value?.pending_async_operation_count === 0, { count: 0 }, { count: value?.pending_async_operation_count ?? null }),
    shellGate(
      "document-visibility",
      "document-not-visible",
      value?.visibility_state === "visible" && value?.document_hidden === false,
      { visibility_state: "visible", hidden: false },
      { visibility_state: value?.visibility_state ?? null, hidden: value?.document_hidden ?? null },
    ),
  ];
  const failures = Array.from(new Set(
    gates.filter((gate) => !gate.pass).map((gate) => gate.failure),
  ));
  return {
    accepted: failures.length === 0,
    failures,
    criteria: {
      after_revision: afterRevision,
      expected_workspace: expectedWorkspace,
      terminal_run_statuses: [...TERMINAL_SHELL_RUN_STATUSES],
    },
    gates,
    observation: value ?? null,
  };
}

export function shellReadinessFailures(value, options = {}) {
  return shellReadinessDecision(value, options).failures;
}

export function shellReadinessAccepted(value, options = {}) {
  return shellReadinessDecision(value, options).accepted;
}

export async function prepareShellBaseline({ context, sink, phase }) {
  await prepareDesktopFixture({
    context,
    sink,
    phase,
    owner: "scenario:shell.baseline",
    configSourcePath: path.join(scenarioDirectory, "..", "fixtures", "shell-baseline.config.toml"),
    sentinelName: "E2E_SHELL_BASELINE.txt",
    sentinelText: "moyAI Desktop E2E shell baseline fixture.\n",
  });
}

export async function executeShellBaseline(args) {
  return acquireInteractiveShell(args, {
    evidenceOwner: "scenario:shell.baseline",
    screenshotStem: "shell-baseline",
  });
}

export async function acquireInteractiveShell(
  { context, driver: cdp, sink },
  {
    evidenceOwner = `scenario:${context.scenarioId}`,
    screenshotStem = `shell-${context.scenarioId.replace(/[^a-z0-9._-]+/g, "-")}`,
  } = {},
) {
  await cdp.call("Runtime.enable");
  await cdp.call("DOM.enable");
  await cdp.call("Accessibility.enable");
  const refresh = await cdp.evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function') return { ok: false, error: 'tauri-invoke-unavailable', projection: null };
    try { return { ok: true, error: null, projection: await invoke('refresh_desktop') }; }
    catch (error) { return { ok: false, error: error instanceof Error ? error.message : String(error), projection: null }; }
  })()`);
  if (refresh?.ok !== true || !/^\d+$/.test(refresh?.projection?.projection_revision ?? "")) {
    throw new Error(`refresh_desktop acquisition failed: ${refresh?.error ?? "invalid projection"}`);
  }
  const refreshRevision = refresh.projection.projection_revision;
  await sink.record("shell-refresh-trigger", {
    projection_revision: refreshRevision,
    async_polling_required: refresh.projection.async_polling_required,
    pending_async_operations: refresh.projection.pending_async_operations,
  }, { phase: "executing", owner: evidenceOwner });
  const readinessOptions = Object.freeze({
    afterRevision: refreshRevision,
    expectedWorkspace: context.paths.workspace,
  });
  const readiness = await waitForObservation({
    label: "interactive Desktop shell readiness",
    timeoutMs: 30_000,
    pollMs: 100,
    sample: async () => shellReadinessDecision(await cdp.evaluate(`(async () => {
      const visible = (element) => {
        if (!(element instanceof HTMLElement) || !element.isConnected) return false;
        const style = getComputedStyle(element);
        const rect = element.getBoundingClientRect();
        return style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity) !== 0
          && rect.width > 0 && rect.height > 0;
      };
      const invoke = window.__TAURI_INTERNALS__?.invoke;
      let projection = null;
      let invokeError = null;
      if (typeof invoke === 'function') {
        try { projection = await invoke('desktop_state'); }
        catch (error) { invokeError = error instanceof Error ? error.message : String(error); }
      }
      await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      const body = document.body;
      const app = document.querySelector('#app');
      const shell = document.querySelector('.app-frame > .shell');
      const prompt = document.querySelector('#app > .app-frame > .shell > main.conversation > section.composer > textarea#prompt');
      const promptRect = prompt?.getBoundingClientRect();
      const promptStyle = prompt instanceof HTMLElement ? getComputedStyle(prompt) : null;
      const centerTarget = promptRect && promptRect.width > 0 && promptRect.height > 0
        ? document.elementFromPoint(promptRect.left + promptRect.width / 2, promptRect.top + promptRect.height / 2)
        : null;
      return {
        title: document.title,
        ready_state: document.readyState,
        body_connected: Boolean(body?.isConnected),
        body_text_length: (body?.innerText ?? '').length,
        app_count: document.querySelectorAll('#app').length,
        app_connected: Boolean(app?.isConnected),
        splash_count: document.querySelectorAll('.splash-screen').length,
        app_frame_count: document.querySelectorAll('.app-frame').length,
        shell_count: document.querySelectorAll('.app-frame > .shell').length,
        conversation_count: document.querySelectorAll('.shell > main.conversation').length,
        composer_count: document.querySelectorAll('#app > .app-frame > .shell > main.conversation > section.composer').length,
        shell_inert: shell === null ? null : shell.matches('[inert]') || shell.closest('[inert]') !== null || shell.getAttribute('aria-hidden') === 'true',
        visible_blocking_overlay_count: Array.from(document.querySelectorAll('[data-modal], [role="dialog"], [role="alertdialog"]')).filter(visible).length,
        visible_modal_backdrop_count: Array.from(document.querySelectorAll('.modal-backdrop')).filter(visible).length,
        visible_fatal_count: Array.from(document.querySelectorAll('.fatal')).filter(visible).length,
        visible_recoverable_error_count: Array.from(document.querySelectorAll('.ui-error-notice')).filter(visible).length,
        prompt_count: document.querySelectorAll('#app > .app-frame > .shell > main.conversation > section.composer > textarea#prompt').length,
        prompt_visible: visible(prompt),
        prompt_enabled: prompt instanceof HTMLTextAreaElement && !prompt.disabled && !prompt.readOnly
          && prompt.tabIndex >= 0
          && prompt.getAttribute('aria-disabled') !== 'true'
          && prompt.closest('[hidden], [inert], [aria-hidden="true"]') === null
          && promptStyle?.pointerEvents !== 'none',
        prompt_center_hit: prompt !== null && centerTarget === prompt,
        tauri_invoke_available: typeof invoke === 'function',
        desktop_state_ok: projection !== null && invokeError === null,
        desktop_state_error: invokeError,
        projection_revision: projection?.projection_revision ?? null,
        workspace_path: projection?.workspace_path ?? null,
        startup_status: projection?.startup?.status ?? null,
        initial_setup_required: projection?.startup?.initial_setup_required ?? null,
        startup_action_overlay: projection?.startup?.action_overlay ?? null,
        startup_fail_check_count: Array.isArray(projection?.startup?.checks)
          ? projection.startup.checks.filter((row) => row?.status === 'fail').length
          : null,
        projection_overlay: projection?.overlay ?? null,
        confirmation_visible: projection?.confirmation_visible ?? null,
        confirmation_id: projection?.confirmation_id ?? null,
        confirmation_present: projection?.confirmation != null,
        run_status_key: projection?.run_status_key ?? null,
        task_activity_state: projection?.task_activity_state ?? null,
        agent_tree_active: projection?.agent_tree_active ?? null,
        post_run_refresh_pending: projection?.post_run_refresh_pending ?? null,
        provider_loading: projection?.provider_loading ?? null,
        composer_submit_mode: projection?.composer_submit_mode ?? null,
        can_submit: projection?.can_submit ?? null,
        navigation_admission_open: projection?.navigation_admission_open ?? null,
        navigation_loading: projection?.navigation_loading ?? null,
        busy: projection?.busy ?? null,
        background_mutation_pending: projection?.background_mutation_pending ?? null,
        async_polling_required: projection?.async_polling_required ?? null,
        pending_async_operation_count: Array.isArray(projection?.pending_async_operations) ? projection.pending_async_operations.length : null,
        visibility_state: document.visibilityState,
        document_hidden: document.hidden,
      };
    })()`), readinessOptions),
    accept: (decision) => decision.accepted,
  });
  const decision = readiness.value;
  const observation = decision.observation;
  const failures = decision.failures;
  await sink.record("shell-observation", {
    ...decision,
    readiness: { attempts: readiness.attempts, elapsed_ms: readiness.elapsed_ms },
  }, { phase: "executing", owner: evidenceOwner });
  if (failures.length > 0) {
    const error = new Error(`shell baseline acquisition failed: ${failures.join(",")}`);
    error.code = "shell-baseline-acquisition-failed";
    error.evidence = { observation, failures };
    throw error;
  }
  const screenshot = await cdp.screenshot();
  const screenshotIdentity = await sink.writeBytes(`screenshots/${screenshotStem}.png`, screenshot);
  await sink.record("shell-screenshot", screenshotIdentity, { phase: "executing", owner: evidenceOwner });
  return { acquisition: "pass", oracle: "not_required", manual: "not_required", observation, screenshot: screenshotIdentity };
}

export async function requestGracefulExit(cdp) {
  return cdp.evaluate(`(() => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function') return { requested: false, reason: 'tauri-invoke-unavailable' };
    setTimeout(() => invoke('exit_app'), 0);
    return { requested: true, reason: null };
  })()`);
}

export async function quiesceShellBaseline() {
  return { input: "pass", resources: [] };
}

export async function cleanupShellBaseline() {
  return { input: "pass", resources: [] };
}
