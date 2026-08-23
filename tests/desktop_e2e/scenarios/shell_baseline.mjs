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

export function shellReadinessFailures(value, { afterRevision = null, expectedWorkspace = null } = {}) {
  const failures = [];
  if (value?.title !== "moyAI") failures.push("page-title-not-moyai");
  if (value?.ready_state !== "complete") failures.push("document-not-complete");
  if (value?.body_connected !== true) failures.push("body-not-connected");
  if (value?.app_count !== 1 || value?.app_connected !== true) failures.push("app-root-not-exact");
  if (value?.splash_count !== 0) failures.push("startup-splash-still-visible");
  if (value?.app_frame_count !== 1 || value?.shell_count !== 1 || value?.conversation_count !== 1) failures.push("interactive-shell-not-exact");
  if (value?.composer_count !== 1) failures.push("composer-not-exact");
  if (value?.shell_inert !== false) failures.push("interactive-shell-inert");
  if (value?.visible_blocking_overlay_count !== 0 || value?.visible_modal_backdrop_count !== 0) failures.push("blocking-overlay-visible");
  if (value?.visible_fatal_count !== 0) failures.push("fatal-error-visible");
  if (value?.visible_recoverable_error_count !== 0) failures.push("recoverable-error-visible");
  if (value?.prompt_count !== 1 || value?.prompt_visible !== true || value?.prompt_enabled !== true) failures.push("main-prompt-not-interactable");
  if (value?.prompt_center_hit !== true) failures.push("main-prompt-hit-test-failed");
  if (value?.tauri_invoke_available !== true || value?.desktop_state_ok !== true) failures.push("desktop-state-command-unavailable");
  if (typeof value?.projection_revision !== "string" || !/^\d+$/.test(value.projection_revision)) failures.push("projection-revision-invalid");
  if (afterRevision !== null) {
    try {
      if (BigInt(value?.projection_revision ?? "-1") <= BigInt(afterRevision)) failures.push("ordinary-projection-not-fresh");
    } catch { failures.push("ordinary-projection-not-fresh"); }
  }
  if (value?.startup_status !== "ready" || value?.initial_setup_required !== false || value?.startup_action_overlay !== "none") failures.push("startup-not-ready");
  if (value?.startup_fail_check_count !== 0) failures.push("startup-check-failed");
  if (value?.projection_overlay !== "none" || value?.confirmation_visible !== false || value?.confirmation_id !== null || value?.confirmation_present !== false) failures.push("projection-overlay-open");
  if (value?.run_status_key !== "idle" || value?.task_activity_state !== "idle" || value?.agent_tree_active !== false) failures.push("task-owner-not-idle");
  if (value?.post_run_refresh_pending !== false || value?.provider_loading !== false) failures.push("projection-background-work-pending");
  if (value?.composer_submit_mode !== "new_request" || value?.can_submit !== true) failures.push("composer-admission-closed");
  if (value?.navigation_admission_open !== true) failures.push("navigation-admission-closed");
  if (expectedWorkspace !== null && value?.workspace_path !== expectedWorkspace) failures.push("workspace-owner-mismatch");
  if (value?.navigation_loading !== false
    || value?.busy !== false
    || value?.background_mutation_pending !== false
    || value?.async_polling_required !== false
    || value?.pending_async_operation_count !== 0) failures.push("projection-not-settled");
  if (value?.visibility_state !== "visible" || value?.document_hidden !== false) failures.push("document-not-visible");
  return failures;
}

export function shellReadinessAccepted(value, options = {}) {
  return shellReadinessFailures(value, options).length === 0;
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
  const readiness = await waitForObservation({
    label: "interactive Desktop shell readiness",
    timeoutMs: 30_000,
    pollMs: 100,
    sample: () => cdp.evaluate(`(async () => {
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
    })()`),
    accept: (value) => shellReadinessAccepted(value, { afterRevision: refreshRevision, expectedWorkspace: context.paths.workspace }),
  });
  const observation = readiness.value;
  const failures = shellReadinessFailures(observation, { afterRevision: refreshRevision, expectedWorkspace: context.paths.workspace });
  await sink.record("shell-observation", { observation, readiness: { attempts: readiness.attempts, elapsed_ms: readiness.elapsed_ms }, failures }, { phase: "executing", owner: evidenceOwner });
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
