import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import {
  canonicalOptionalUlid,
  canonicalU64,
  canonicalUlid,
  canonicalWorkspace,
} from "../core/canonical_identity.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import { startScriptedProvider } from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import {
  classifyAcquiredObservationFailure,
  exactProviderTurnLedger,
  providerRestartFixtureConfig,
  quiesceProviderResource,
} from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import {
  captureScenarioScreenshot,
  invokeDesktopCommand,
  invokeDesktopCommandOutcome,
  selectedNavigationIdentity,
} from "./observations.mjs";

const OWNER = "scenario:prompt-review.cancel";
export const PROMPT_REVIEW_RAW_TEXT = "improve exact target";
export const PROMPT_REVIEW_ENHANCED_TEXT = "enhanced exact target";

const PROMPT = Object.freeze({
  selector: "section.composer textarea#prompt",
  identity: { tag: "TEXTAREA", id: "prompt" },
});
const ENHANCE = Object.freeze({
  selector: 'section.composer button[data-action="enhance-prompt"]',
  identity: { tag: "BUTTON", action: "enhance-prompt" },
});
const REVIEW_DRAFT_IDENTITY = Object.freeze({ tag: "TEXTAREA", id: "review-draft" });

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function productFailure(code, message, evidence) {
  return new DesktopE2eError("product", code, message, evidence);
}

function errorObservation(error) {
  return {
    owner: error instanceof DesktopE2eError ? error.owner : "harness",
    code: error?.code ?? "unclassified-error",
    message: error?.message ?? String(error),
    evidence: error?.evidence ?? null,
  };
}

function canonicalPositiveDecimal(value) {
  return canonicalU64(value) && value !== "0";
}

function canonicalIdentityString(value) {
  return typeof value === "string" && value.length > 0 && value.trim() === value;
}

export function exactRunExpectedState(state, expected = undefined) {
  if (state === null || typeof state !== "object" || Array.isArray(state)) return false;
  const exact = state.kind === "idle"
    ? sameValue(Object.keys(state).sort(), ["admissionRevision", "kind", "latestTurnId"])
      && canonicalOptionalUlid(state.latestTurnId)
      && canonicalU64(state.admissionRevision)
    : state.kind === "turn"
      ? sameValue(Object.keys(state).sort(), ["admissionRevision", "kind", "turnId"])
        && canonicalUlid(state.turnId)
        && canonicalU64(state.admissionRevision)
      : false;
  if (!exact) return false;
  if (expected === undefined) return true;
  if (!exactRunExpectedState(expected) || state.kind !== expected.kind) return false;
  if (state.admissionRevision !== expected.admissionRevision) return false;
  return state.kind === "turn"
    ? expected.kind === "turn" && state.turnId === expected.turnId
    : expected.kind === "idle" && state.latestTurnId === expected.latestTurnId;
}

function exactDraftTarget(target, expected) {
  return target !== null
    && typeof target === "object"
    && !Array.isArray(target)
    && sameValue(Object.keys(target).sort(), ["ownerGeneration", "sessionId", "workspacePath"])
    && target.workspacePath === expected.workspacePath
    && canonicalWorkspace(target.workspacePath)
    && target.sessionId === expected.sessionId
    && canonicalOptionalUlid(target.sessionId)
    && target.ownerGeneration === expected.ownerGeneration
    && canonicalU64(target.ownerGeneration);
}

export function exactRunTarget(target, expected) {
  return target !== null
    && typeof target === "object"
    && !Array.isArray(target)
    && sameValue(Object.keys(target).sort(), [
      "expectedState",
      "permissionConfirmationId",
      "runtimeOwnerToken",
      "sessionId",
      "workspacePath",
    ])
    && target.workspacePath === expected.workspacePath
    && target.sessionId === expected.sessionId
    && canonicalWorkspace(target.workspacePath)
    && canonicalOptionalUlid(target.sessionId)
    && canonicalIdentityString(target.runtimeOwnerToken)
    && target.runtimeOwnerToken === expected.runtimeOwnerToken
    && (target.permissionConfirmationId === null
      || canonicalU64(target.permissionConfirmationId))
    && target.permissionConfirmationId === expected.permissionConfirmationId
    && exactRunExpectedState(target.expectedState, expected.expectedState);
}

function exactReviewTarget(target, expected) {
  return target !== null
    && typeof target === "object"
    && !Array.isArray(target)
    && sameValue(Object.keys(target).sort(), [
      "expectedState",
      "ownerGeneration",
      "requestId",
      "sessionId",
      "workspacePath",
    ])
    && exactDraftTarget({
      workspacePath: target.workspacePath,
      sessionId: target.sessionId,
      ownerGeneration: target.ownerGeneration,
    }, expected)
    && canonicalPositiveDecimal(target.requestId)
    && exactRunExpectedState(target.expectedState, expected.expectedState);
}

export function staleRunTarget(target) {
  if (!exactRunExpectedState(target?.expectedState)) {
    throw new TypeError("a canonical run target is required to construct a stale target");
  }
  const currentRevision = BigInt(target.expectedState.admissionRevision);
  const staleRevision = currentRevision === 18_446_744_073_709_551_615n
    ? (currentRevision - 1n).toString()
    : (currentRevision + 1n).toString();
  return {
    ...structuredClone(target),
    expectedState: target.expectedState.kind === "idle"
      ? {
        kind: "idle",
        latestTurnId: target.expectedState.latestTurnId,
        admissionRevision: staleRevision,
      }
      : {
        kind: "turn",
        turnId: target.expectedState.turnId,
        admissionRevision: staleRevision,
      },
  };
}

export function exactPromptReviewProviderLedger(ledger) {
  return exactProviderTurnLedger(ledger);
}

function commonOwnerFailures(surface, ledger, expected) {
  const failures = [];
  const projection = surface?.projection;
  if (!exactDraftTarget(projection?.draft_target, expected)) failures.push("draft-target-drift");
  if (!exactRunTarget(projection?.run_target, expected)) failures.push("run-target-drift");
  if (projection?.composer_commit_generation !== expected.composerCommitGeneration) {
    failures.push("composer-commit-generation-drift");
  }
  if (!sameValue(selectedNavigationIdentity(projection), expected.navigationIdentity)) {
    failures.push("navigation-owner-drift");
  }
  if (surface?.prompt?.value !== expected.rawText) {
    failures.push("composer-draft-drift");
  }
  if (projection?.run_status_key !== "idle" || projection?.busy !== false || projection?.agent_tree_active !== false) {
    failures.push("run-owner-not-idle");
  }
  if (projection?.background_mutation_pending !== false
    || projection?.async_polling_required !== false
    || !Array.isArray(projection?.pending_async_operations)
    || projection.pending_async_operations.length !== 0) {
    failures.push("async-owner-not-settled");
  }
  if (!exactPromptReviewProviderLedger(ledger)) failures.push("provider-ledger-not-exact");
  if (surface?.visible_fatal_count !== 0) failures.push("fatal-error-visible");
  if (surface?.visible_recoverable_error_count !== 0) failures.push("recoverable-error-visible");
  return failures;
}

export function staleEnhanceRejectedFailures(sample, expected) {
  const failures = [];
  const projection = sample?.surface?.projection;
  const conflictState = sample?.outcome?.error?.state;
  if (sample?.outcome?.ok !== false || sample?.outcome?.error?.kind !== "conflict") {
    failures.push("stale-enhance-not-conflict");
  }
  if (!exactDraftTarget(conflictState?.draft_target, expected)
    || !exactRunTarget(conflictState?.run_target, expected)) {
    failures.push("conflict-state-owner-drift");
  }
  if (!exactDraftTarget(projection?.draft_target, expected)) failures.push("stale-enhance-draft-target-mutated");
  if (!exactRunTarget(projection?.run_target, expected)) failures.push("stale-enhance-run-target-mutated");
  if (sample?.surface?.prompt?.value !== expected.rawText) {
    failures.push("stale-enhance-draft-mutated");
  }
  if (projection?.overlay !== "none"
    || projection?.review_target !== null
    || projection?.review_raw_text !== ""
    || projection?.review_draft_text !== "") {
    failures.push("stale-enhance-review-created");
  }
  if (projection?.run_status_key !== "idle"
    || projection?.busy !== false
    || projection?.agent_tree_active !== false
    || projection?.background_mutation_pending !== false) {
    failures.push("stale-enhance-runtime-mutated");
  }
  if (sample?.surface?.enhance_enabled !== true) failures.push("stale-enhance-admission-not-restored");
  if (!Array.isArray(sample?.ledger) || sample.ledger.length !== 0) failures.push("stale-enhance-provider-contacted");
  if (sample?.surface?.visible_fatal_count !== 0) failures.push("stale-enhance-fatal-visible");
  if (sample?.surface?.visible_recoverable_error_count !== 0) failures.push("stale-enhance-error-visible");
  return failures;
}

export function promptReviewOpenedFailures(surface, ledger, expected) {
  const failures = commonOwnerFailures(surface, ledger, expected);
  const projection = surface?.projection;
  if (projection?.overlay !== "prompt_review") failures.push("prompt-review-overlay-not-open");
  if (!exactReviewTarget(projection?.review_target, expected)) failures.push("review-target-not-canonical");
  if (projection?.review_raw_text !== expected.rawText) failures.push("review-raw-text-drift");
  if (projection?.review_draft_text !== expected.enhancedText) failures.push("review-draft-projection-drift");
  if (projection?.send_enhanced_enabled !== true || projection?.send_raw_enabled !== true) {
    failures.push("review-send-admission-closed");
  }
  if (surface?.dialog_count !== 1 || surface?.dialog_visible !== true) failures.push("review-dialog-not-exact");
  if (surface?.review_draft?.count !== 1
    || surface?.review_draft?.visible !== true
    || surface?.review_draft?.enabled !== true
    || surface?.review_draft?.value !== expected.enhancedText) {
    failures.push("review-draft-dom-drift");
  }
  if (surface?.review_raw?.count !== 1 || surface?.review_raw?.text !== expected.rawText) {
    failures.push("review-raw-dom-drift");
  }
  if (surface?.cancel_button?.count !== 1
    || surface?.cancel_button?.visible !== true
    || surface?.cancel_button?.enabled !== true) {
    failures.push("review-cancel-not-interactable");
  }
  if (surface?.shell_inert !== true) failures.push("review-background-not-inert");
  if (surface?.active?.id !== "review-draft") failures.push("review-draft-not-focused");
  return failures;
}

export function createStablePromptReviewOpenedPredicate(expected) {
  let acceptedReviewTarget = null;
  return (sample) => {
    if (promptReviewOpenedFailures(sample?.surface, sample?.ledger, expected).length !== 0) {
      return false;
    }
    const currentReviewTarget = sample.surface.projection.review_target;
    if (acceptedReviewTarget === null) {
      acceptedReviewTarget = structuredClone(currentReviewTarget);
      return false;
    }
    return sameValue(currentReviewTarget, acceptedReviewTarget);
  };
}

export function promptReviewCancelledFailures(surface, ledger, expected) {
  const failures = commonOwnerFailures(surface, ledger, expected);
  const projection = surface?.projection;
  if (projection?.overlay !== "none") failures.push("prompt-review-overlay-not-closed");
  if (projection?.review_target !== null) failures.push("review-target-not-cleared");
  if (projection?.review_raw_text !== "" || projection?.review_draft_text !== "") {
    failures.push("review-content-not-cleared");
  }
  if (projection?.send_enhanced_enabled !== false || projection?.send_raw_enabled !== false) {
    failures.push("review-send-admission-not-cleared");
  }
  if (surface?.dialog_count !== 0 || surface?.visible_modal_backdrop_count !== 0) {
    failures.push("review-dialog-not-removed");
  }
  if (surface?.shell_inert !== false) failures.push("composer-background-not-restored");
  if (surface?.prompt?.count !== 1 || surface?.prompt?.visible !== true || surface?.prompt?.enabled !== true) {
    failures.push("composer-not-interactable");
  }
  return failures;
}

async function observePromptReviewSurface(cdp) {
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
    const enabled = (element) => element instanceof HTMLElement
      && !element.matches(':disabled')
      && element.getAttribute('aria-disabled') !== 'true'
      && element.closest('[inert]') === null;
    const identity = (element) => ({
      tag: element instanceof Element ? element.tagName.toUpperCase() : '',
      id: element instanceof Element && element.id ? element.id : null,
      action: element instanceof HTMLElement ? (element.dataset.action ?? null) : null,
    });
    const shell = document.querySelector('.app-frame > .shell');
    const dialogs = Array.from(document.querySelectorAll('[role="dialog"][aria-labelledby="prompt-review-dialog-title"]'));
    const dialog = dialogs.length === 1 ? dialogs[0] : null;
    const draft = document.querySelector('textarea#review-draft');
    const raw = document.querySelector('[role="dialog"][aria-labelledby="prompt-review-dialog-title"] .review-grid > pre');
    const cancel = document.querySelector('[role="dialog"][aria-labelledby="prompt-review-dialog-title"] button[data-action="cancel-review"]');
    const prompt = document.querySelector('section.composer textarea#prompt');
    return {
      projection,
      shell_inert: shell === null ? null : shell.matches('[inert]') || shell.getAttribute('aria-hidden') === 'true',
      dialog_count: dialogs.length,
      dialog_visible: visible(dialog),
      review_draft: {
        count: document.querySelectorAll('textarea#review-draft').length,
        value: draft instanceof HTMLTextAreaElement ? draft.value : null,
        visible: visible(draft),
        enabled: enabled(draft),
      },
      review_raw: {
        count: document.querySelectorAll('[role="dialog"][aria-labelledby="prompt-review-dialog-title"] .review-grid > pre').length,
        text: raw?.textContent ?? null,
      },
      cancel_button: {
        count: document.querySelectorAll('[role="dialog"][aria-labelledby="prompt-review-dialog-title"] button[data-action="cancel-review"]').length,
        visible: visible(cancel),
        enabled: enabled(cancel),
      },
      prompt: {
        count: document.querySelectorAll('section.composer textarea#prompt').length,
        value: prompt instanceof HTMLTextAreaElement ? prompt.value : null,
        visible: visible(prompt),
        enabled: enabled(prompt),
      },
      enhance_enabled: enabled(document.querySelector('section.composer button[data-action="enhance-prompt"]')),
      active: identity(document.activeElement),
      visible_fatal_count: Array.from(document.querySelectorAll('.fatal')).filter(visible).length,
      visible_recoverable_error_count: Array.from(document.querySelectorAll('.ui-error-notice')).filter(visible).length,
      visible_modal_backdrop_count: Array.from(document.querySelectorAll('.modal-backdrop')).filter(visible).length,
    };
  })()`);
}

function expectedTypedEvents(text) {
  const keyCode = (character) => {
    if (/^[a-z]$/.test(character)) return `Key${character.toUpperCase()}`;
    if (character === " ") return "Space";
    throw new TypeError(`unsupported prompt review scenario character: ${character}`);
  };
  return Array.from(text).flatMap((character) => [
    { type: "keydown", identity: PROMPT.identity, key: character, code: keyCode(character) },
    { type: "input", identity: PROMPT.identity, inputType: "insertText", data: character },
    { type: "keyup", identity: PROMPT.identity, key: character, code: keyCode(character) },
  ]);
}

async function trustedClick(input, locator) {
  const start = (await input.snapshotProbe()).sequence;
  const target = await input.click(locator);
  const snapshot = await input.snapshotProbe(start);
  return {
    target,
    probe: assertTrustedProbeSequence(snapshot, {
      afterSequence: start,
      expected: [
        { type: "pointerdown", identity: locator.identity, button: 0, buttons: 1 },
        { type: "pointerup", identity: locator.identity, button: 0, buttons: 0 },
        { type: "click", identity: locator.identity, button: 0, buttons: 0 },
      ],
    }),
  };
}

async function waitForAcquiredProductStage({ label, timeoutMs, sample, accept, code, message }) {
  try {
    return await waitForObservation({ label, timeoutMs, pollMs: 100, sample, accept });
  } catch (error) {
    throw classifyAcquiredObservationFailure(error, { code, message });
  }
}

function expectedOwner(initial, workspacePath) {
  const target = initial?.draft_target;
  const runTarget = initial?.run_target;
  const expectedRunOwner = {
    workspacePath,
    sessionId: target?.sessionId,
    runtimeOwnerToken: runTarget?.runtimeOwnerToken,
    permissionConfirmationId: runTarget?.permissionConfirmationId,
    expectedState: runTarget?.expectedState,
  };
  if (initial?.workspace_path !== workspacePath
    || !exactDraftTarget(target, {
      workspacePath,
      sessionId: target?.sessionId,
      ownerGeneration: target?.ownerGeneration,
    })
    || !canonicalU64(target?.ownerGeneration)
    || !exactRunTarget(runTarget, expectedRunOwner)
    || runTarget.sessionId !== target.sessionId
    || (target.sessionId === null && !exactRunExpectedState(runTarget.expectedState, {
      kind: "idle",
      latestTurnId: null,
      admissionRevision: "0",
    }))
    || typeof initial?.composer_commit_generation !== "string"
    || !/^\d+$/.test(initial.composer_commit_generation)) {
    throw productFailure("initial-composer-owner-invalid", "the initial composer owner projection was not canonical", {
      expected_workspace: workspacePath,
      projection_workspace: initial?.workspace_path ?? null,
      draft_target: target ?? null,
      run_target: runTarget ?? null,
      composer_commit_generation: initial?.composer_commit_generation ?? null,
    });
  }
  return {
    workspacePath,
    sessionId: target.sessionId,
    ownerGeneration: target.ownerGeneration,
    runtimeOwnerToken: runTarget.runtimeOwnerToken,
    permissionConfirmationId: runTarget.permissionConfirmationId,
    expectedState: structuredClone(runTarget.expectedState),
    runTarget: structuredClone(runTarget),
    composerCommitGeneration: initial.composer_commit_generation,
    navigationIdentity: selectedNavigationIdentity(initial),
    rawText: PROMPT_REVIEW_RAW_TEXT,
    enhancedText: PROMPT_REVIEW_ENHANCED_TEXT,
  };
}

export function createPromptReviewCancelScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    inputCleanupFailure: null,
  };
  return Object.freeze({
    id: "prompt-review.cancel",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({
        expectedPrompt: PROMPT_REVIEW_RAW_TEXT,
        responseText: PROMPT_REVIEW_ENHANCED_TEXT,
      });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: providerRestartFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_PROMPT_REVIEW_CANCEL.txt",
        sentinelText: "moyAI Desktop E2E Prompt Review exact-target fixture.\n",
      });
      await sink.record("scripted-provider-started", state.provider.resourceObservation(), { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("scripted provider was not prepared");
      await acquireInteractiveShell({ context, driver: cdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "prompt-review-shell-ready",
      });
      if (provider.requestLedger.length !== 0) {
        throw productFailure("prompt-review-cold-start-request", "Desktop contacted the provider before Prompt Enhance", {
          ledger: provider.requestLedger,
        });
      }

      const initial = await invokeDesktopCommand(cdp, "desktop_state");
      const expected = expectedOwner(initial, context.paths.workspace);
      const input = new WebviewInput(cdp, { probeId: "prompt-review-cancel" });
      let primaryError = null;
      try {
        await input.installProbe();
        const promptClick = await trustedClick(input, PROMPT);
        const typeStart = (await input.snapshotProbe()).sequence;
        await input.typeText(PROMPT_REVIEW_RAW_TEXT);
        const typedSnapshot = await input.snapshotProbe(typeStart);
        const trustedTyping = assertTrustedProbeSequence(typedSnapshot, {
          afterSequence: typeStart,
          expected: expectedTypedEvents(PROMPT_REVIEW_RAW_TEXT),
        });
        const typed = await waitForAcquiredProductStage({
          label: "typed composer draft and Enhance admission",
          timeoutMs: 10_000,
          sample: async () => ({ surface: await observePromptReviewSurface(cdp), ledger: provider.requestLedger }),
          accept: (sample) => sample?.surface?.prompt?.value === PROMPT_REVIEW_RAW_TEXT
            && sample?.surface?.enhance_enabled === true
            && exactDraftTarget(sample?.surface?.projection?.draft_target, expected)
            && exactRunTarget(sample?.surface?.projection?.run_target, expected)
            && Array.isArray(sample?.ledger)
            && sample.ledger.length === 0,
          code: "prompt-review-composer-not-ready",
          message: "trusted typing did not settle to an Enhance-ready composer with the same owner",
        });
        await sink.record("trusted-prompt-review-input-acquired", {
          input_kind: "browser_trusted",
          prompt_click: promptClick,
          typing_probe: trustedTyping,
          projection_revision: typed.value.surface.projection.projection_revision,
          draft_target: typed.value.surface.projection.draft_target,
          run_target: typed.value.surface.projection.run_target,
        }, { phase: "executing", owner: OWNER });

        const staleExpectedRunTarget = staleRunTarget(typed.value.surface.projection.run_target);
        const staleOutcome = await invokeDesktopCommandOutcome(cdp, "enhance_prompt", {
          text: PROMPT_REVIEW_RAW_TEXT,
          expectedTarget: typed.value.surface.projection.draft_target,
          expectedRunTarget: staleExpectedRunTarget,
        });
        const staleRejected = await waitForAcquiredProductStage({
          label: "stale Prompt Enhance rejection before provider mutation",
          timeoutMs: 10_000,
          sample: async () => ({
            outcome: staleOutcome,
            surface: await observePromptReviewSurface(cdp),
            ledger: provider.requestLedger,
          }),
          accept: (sample) => staleEnhanceRejectedFailures(sample, expected).length === 0,
          code: "stale-prompt-enhance-not-rejected",
          message: "a Prompt Enhance with an obsolete expected run state was not rejected before mutation",
        });
        await sink.record("stale-prompt-enhance-rejected", {
          command: "enhance_prompt",
          stale_expected_run_target: staleExpectedRunTarget,
          conflict: staleOutcome.error,
          provider_ledger: staleRejected.value.ledger,
          current_draft_target: staleRejected.value.surface.projection.draft_target,
          current_run_target: staleRejected.value.surface.projection.run_target,
          current_review_target: staleRejected.value.surface.projection.review_target,
        }, { phase: "executing", owner: OWNER });

        const enhance = await trustedClick(input, ENHANCE);
        const openedPredicate = createStablePromptReviewOpenedPredicate(expected);
        const opened = await waitForAcquiredProductStage({
          label: "Prompt Review exact target and enhanced draft",
          timeoutMs: 45_000,
          sample: async () => ({ surface: await observePromptReviewSurface(cdp), ledger: provider.requestLedger }),
          accept: openedPredicate,
          code: "prompt-review-open-contract-mismatch",
          message: "Prompt Enhance did not settle to the exact review target and rendered draft",
        });
        const openedScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "prompt-review-opened",
          owner: OWNER,
        });
        await sink.record("prompt-review-opened", {
          input_kind: "browser_trusted",
          enhance,
          expected_owner: expected,
          review_target: opened.value.surface.projection.review_target,
          provider_ledger: opened.value.ledger,
          surface: opened.value.surface,
          screenshot: openedScreenshot,
        }, { phase: "executing", owner: OWNER });

        const escapeStart = (await input.snapshotProbe()).sequence;
        await input.pressKey("Escape");
        const escapeSnapshot = await input.snapshotProbe(escapeStart);
        const trustedEscape = assertTrustedProbeSequence(escapeSnapshot, {
          afterSequence: escapeStart,
          expected: [
            { type: "keydown", identity: REVIEW_DRAFT_IDENTITY, key: "Escape", code: "Escape" },
            { type: "keyup", identity: REVIEW_DRAFT_IDENTITY, key: "Escape", code: "Escape" },
          ],
        });
        const cancelled = await waitForAcquiredProductStage({
          label: "Prompt Review exact Escape cancellation",
          timeoutMs: 10_000,
          sample: async () => ({ surface: await observePromptReviewSurface(cdp), ledger: provider.requestLedger }),
          accept: (sample) => promptReviewCancelledFailures(sample?.surface, sample?.ledger, expected).length === 0,
          code: "prompt-review-cancel-contract-mismatch",
          message: "trusted Escape did not cancel only the current review while preserving the composer owner and draft",
        });
        const cancelledScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "prompt-review-cancelled",
          owner: OWNER,
        });
        state.acceptedLedger = structuredClone(cancelled.value.ledger);
        await sink.record("prompt-review-cancelled", {
          input_kind: "browser_trusted",
          escape_probe: trustedEscape,
          expected_owner: expected,
          final_projection_revision: cancelled.value.surface.projection.projection_revision,
          final_draft_target: cancelled.value.surface.projection.draft_target,
          accepted_provider_ledger: state.acceptedLedger,
          surface: cancelled.value.surface,
          screenshot: cancelledScreenshot,
        }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        try { await input.cleanup(); }
        catch (error) {
          state.inputCleanupFailure = errorObservation(error);
          if (primaryError === null) {
            throw new DesktopE2eError(
              "harness",
              "prompt-review-input-cleanup-failed",
              "Prompt Review WebView input did not settle exactly",
              state.inputCleanupFailure,
            );
          }
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
      const quiesced = state.quiesceOutcome !== null;
      const pass = quiesced
        && state.quiesceOutcome.input === "pass"
        && state.inputCleanupFailure === null;
      return {
        input: pass ? "pass" : "fail",
        resources: [{
          kind: "prompt-review-cancel-verification",
          quiesced,
          quiesce_input: state.quiesceOutcome?.input ?? null,
          input_cleanup_failure: state.inputCleanupFailure,
        }],
      };
    },
  });
}
