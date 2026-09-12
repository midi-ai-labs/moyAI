import { isDeepStrictEqual } from "node:util";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { DesktopStatePollBarrier } from "../drivers/desktop_state_poll_barrier.mjs";
import {
  SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
  SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE,
  createChatToolContinuationProviderScript, startScriptedProvider,
} from "../drivers/scripted_provider.mjs";
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from "../drivers/webview_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { classifyAcquiredObservationFailure, quiesceProviderResource } from "./provider_restart.mjs";
import { exactChatToolContinuationLedger, providerChatToolContinuationFixtureConfig } from "./provider_chat_tool_continuation.mjs";

const OWNER = "scenario:output.history-navigation";
const NODE_KEY = "moyai.desktop_e2e.output-history-nodes.v1";
export const OUTPUT_HISTORY_DRAFT = "未送信メモ：会話の詳細を確認中";
const PROMPT = { selector: "section.composer textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
const SEND = { selector: 'section.composer button[data-action="send"]', identity: { tag: "BUTTON", action: "send" } };
const DETAIL = { selector: '.output-activity-history-route button[data-action="jump-history-anchor"]', identity: { tag: "BUTTON", action: "jump-history-anchor" } };
const PANE = { selector: '.topbar button[data-action="toggle-artifact-pane"]', identity: { tag: "BUTTON", action: "toggle-artifact-pane" } };
const same = isDeepStrictEqual;
const fail = (message, evidence) => new DesktopE2eError("product", "output-history-navigation", message, evidence);

export function outputHistoryOwner(projection) {
  const target = projection?.run_target;
  const expected = target?.expectedState;
  if (!target?.sessionId || !target?.workspacePath || !["turn", "idle"].includes(expected?.kind)) return null;
  const turn = expected.kind === "turn" ? expected.turnId : expected.latestTurnId;
  if (!turn || !expected.admissionRevision) return null;
  return { workspace: target.workspacePath, session: target.sessionId, turn, admission: expected.admissionRevision };
}

export function outputHistoryDraftBaseline(projection) {
  if (!projection?.draft_target?.workspacePath || typeof projection.composer_commit_generation !== "string"
    || typeof projection.draft_prompt !== "string") return null;
  return { target: structuredClone(projection.draft_target), commitGeneration: projection.composer_commit_generation,
    committedText: projection.draft_prompt };
}

/** Typed text is frontend-owned until Send; the Rust committed draft must remain at its observed baseline. */
export function outputHistoryDraftFailures(surface, { draft, selection, baseline, focus = "none", sameNode = true } = {}) {
  const failures = [];
  if (!baseline || !same(outputHistoryDraftBaseline(surface?.projection), baseline)) failures.push("committed-draft-owner-changed");
  if (surface?.prompt?.count !== 1 || surface.prompt.visible !== true || surface.prompt.value !== draft
    || (sameNode && surface.prompt.same_node !== true) || surface.prompt.disabled !== false) failures.push("draft-or-editor-changed");
  if (focus === "prompt" && surface?.prompt?.focused !== true) failures.push("draft-focus-lost");
  if (selection && !same(surface?.prompt?.selection, selection)) failures.push("draft-selection-changed");
  return failures;
}

export function outputHistoryDestinationIdentity(surface) {
  const target = surface?.destination;
  if (!target?.anchor || !target.history_identity || !target.summary_focus_key) return null;
  return { anchor: target.anchor, historyIdentity: target.history_identity, focusKey: target.summary_focus_key };
}

/** Observable behavior only: the action must resolve the displayed canonical history destination. */
export function outputHistoryNavigationFailures(surface, { owner, phase, draft, selection, baseline, destinationIdentity,
  revealed = true, focus = "summary" } = {}) {
  const failures = [];
  const projection = surface?.projection;
  if (!owner || !same(outputHistoryOwner(projection), owner) || projection?.draft_target?.sessionId !== owner.session) failures.push("run-or-session-owner-changed");
  if (phase === "running" && (projection?.run_status_key !== "running" || projection?.busy !== true
    || projection?.task_activity_state !== "running")) failures.push("running-state-lost");
  if (phase === "completed" && (projection?.run_status_key !== "completed" || projection?.busy !== false
    || projection?.task_activity_state !== "idle" || projection?.post_run_refresh_pending !== false)) failures.push("completion-not-settled");
  if (surface?.errors !== 0 || projection?.overlay !== "none") failures.push("error-or-overlay-present");
  if (surface?.destination?.count !== 1 || typeof surface.destination.details_open !== "boolean"
    || !destinationIdentity || !same(outputHistoryDestinationIdentity(surface), destinationIdentity)
    || surface.destination.history_identity !== `turn:${owner?.turn}:work-summary`) failures.push("history-destination-mismatch");
  // The output activity section exists only while busy. Completed work is opened from its
  // canonical transcript disclosure, after the frontend has retired the running-only route.
  if (phase === "running" && (surface?.route?.count !== 1 || surface.route.enabled !== true
    || surface.route.target !== surface?.destination?.anchor)) failures.push("history-route-mismatch");
  if (phase === "completed" && surface?.route?.count !== 0) failures.push("completed-activity-route-not-retired");
  if (surface?.destination?.details_open !== revealed) failures.push("history-disclosure-state-mismatch");
  if (revealed && surface?.destination?.summary_visible !== true) failures.push("history-destination-not-visible");
  // Transcript cards may be recreated by passive progress renders; their canonical target and
  // live focused disclosure must stay the same. Only the actively editable Main textarea is retained.
  if (focus === "summary" && surface?.destination?.summary_focused !== true) failures.push("destination-focus-lost");
  if (draft !== undefined) failures.push(...outputHistoryDraftFailures(surface, { draft, selection, baseline, focus }));
  return failures;
}

async function observe(cdp, barrier) {
  const projection = barrier?.installed ? await barrier.sampleBackend() : await invokeDesktopCommand(cdp, "desktop_state");
  const owner = outputHistoryOwner(projection);
  const historyIdentity = owner ? `turn:${owner.turn}:work-summary` : null;
  const dom = await cdp.evaluate(`(() => {
    const visible = (element) => {
      if (!(element instanceof HTMLElement) || !element.isConnected) return false;
      const rect = element.getBoundingClientRect(), style = getComputedStyle(element);
      return rect.width > 0 && rect.height > 0 && style.display !== 'none'
        && style.visibility !== 'hidden' && Number(style.opacity) !== 0;
    };
    const inViewport = (element) => {
      if (!visible(element)) return false;
      const rect = element.getBoundingClientRect(), thread = document.querySelector('#thread')?.getBoundingClientRect();
      const composer = document.querySelector('.composer')?.getBoundingClientRect();
      if (!thread) return false;
      return rect.left >= thread.left - 1 && rect.right <= thread.right + 1
        && rect.top >= thread.top - 1 && rect.bottom <= Math.min(thread.bottom, composer?.top ?? innerHeight) + 1;
    };
    const routes = document.querySelectorAll(${JSON.stringify(DETAIL.selector)}), route = routes[0];
    const anchor = route?.getAttribute('data-history-target') ?? null;
    const destinations = Array.from(document.querySelectorAll('#thread [data-history-identity]'))
      .filter((node) => node.getAttribute('data-history-identity') === ${JSON.stringify(historyIdentity)});
    const target = destinations[0], details = target?.querySelector('.message-body > details');
    const summary = details?.querySelector(':scope > summary');
    const prompts = document.querySelectorAll('section.composer textarea#prompt'), prompt = prompts[0];
    const memory = globalThis[Symbol.for('${NODE_KEY}')];
    return {
      pane: { collapsed: document.querySelector('.app-frame')?.classList.contains('artifact-collapsed') ?? true,
        mode: document.querySelector('.artifact-pane')?.getAttribute('data-pane-mode') ?? null },
      route: { count: routes.length, target: anchor, visible: visible(route),
        enabled: route instanceof HTMLButtonElement && !route.disabled && !route.closest('[inert]'),
        focused: document.activeElement === route },
      destination: { count: destinations.length, anchor: target?.getAttribute('data-history-anchor') ?? null,
        history_identity: target?.getAttribute('data-history-identity') ?? null,
        details_open: details instanceof HTMLDetailsElement ? details.open : null,
        summary_focus_key: summary?.getAttribute('data-focus-key') ?? null,
        summary_focused: document.activeElement === summary, summary_visible: inViewport(summary),
        same_summary: Boolean(memory?.summary && memory.summary === summary) },
      prompt: { count: prompts.length, visible: visible(prompt), value: prompt?.value ?? null, focused: document.activeElement === prompt,
        selection: prompt ? [prompt.selectionStart, prompt.selectionEnd] : null,
        same_node: Boolean(memory?.prompt && memory.prompt === prompt), disabled: prompt?.disabled ?? null },
      errors: Array.from(document.querySelectorAll('.fatal, .ui-error-notice, .validation.error, #thread .message.error')).filter(visible).length,
    };
  })()`);
  return { projection, ...dom };
}

async function wait(cdp, barrier, label, accept, timeoutMs = 15_000) {
  try {
    return (await waitForObservation({ label, timeoutMs, pollMs: 60, retrySampleErrors: false,
      sample: () => observe(cdp, barrier), accept })).value;
  } catch (error) { throw classifyAcquiredObservationFailure(error, { code: "output-history-state", message: label }); }
}
async function recordClick(input, target, sink) {
  const afterSequence = (await input.snapshotProbe()).sequence;
  const acquired = await input.click(target);
  const proof = assertTrustedProbeSequence(await input.snapshotProbe(afterSequence), { afterSequence, expected: [
    { type: "pointerdown", identity: target.identity, button: 0, buttons: 1 },
    { type: "pointerup", identity: target.identity, button: 0, buttons: 0 },
    { type: "click", identity: target.identity, button: 0, buttons: 0 },
  ] });
  await sink.record("output-history-pointer", { target, acquired, proof }, { phase: "executing", owner: OWNER });
}
async function type(input, text, sink) {
  await recordClick(input, PROMPT, sink);
  const afterSequence = (await input.snapshotProbe()).sequence;
  await input.insertText(PROMPT, text);
  const proof = assertTrustedTextInsertion(await input.snapshotProbe(afterSequence), { afterSequence, identity: PROMPT.identity, text });
  await sink.record("output-history-text", { proof }, { phase: "executing", owner: OWNER });
}
function summaryTarget(surface) {
  const key = surface.destination.summary_focus_key;
  if (!key) throw fail("The history destination has no accessible disclosure", surface);
  return { selector: `#thread summary[data-focus-key=${JSON.stringify(key)}]`, identity: { tag: "SUMMARY", focusKey: key } };
}
async function remember(cdp, surface) {
  await cdp.evaluate(`(() => {
    const target = Array.from(document.querySelectorAll('#thread [data-history-identity]')).find((node) => node.getAttribute('data-history-identity') === ${JSON.stringify(surface.destination.history_identity)});
    globalThis[Symbol.for('${NODE_KEY}')] = { prompt: globalThis[Symbol.for('${NODE_KEY}')]?.prompt ?? document.querySelector('#prompt'), summary: target?.querySelector('.message-body > details > summary') };
  })()`);
}
async function stable(cdp, barrier, sink, label, options) {
  const started = Date.now();
  const sample = await waitForObservation({ label, timeoutMs: 5000, pollMs: 100, retrySampleErrors: false,
    sample: () => observe(cdp, barrier), accept: (surface) => {
      const failures = outputHistoryNavigationFailures(surface, options);
      if (failures.length) throw fail(label, { failures, surface });
      return Date.now() - started >= 1200;
    } });
  await sink.record(label, sample, { phase: "executing", owner: OWNER });
}
export function outputHistoryKeyboardProof(snapshot, afterSequence, target) {
  return assertTrustedProbeSequence(snapshot, { afterSequence, expected: [
    { type: "keydown", identity: target.identity, key: "Enter", code: "Enter" },
    { type: "click", identity: target.identity },
    { type: "keyup", identity: target.identity, key: "Enter", code: "Enter" },
  ] });
}

async function keyboardCompletedDisclosure(input, cdp, barrier, sink, target) {
  // A pointer collapse may already own this summary. Leave and return through native Tab so
  // this phase proves keyboard reachability as well as native Enter activation.
  if ((await observe(cdp, barrier)).destination.summary_focused) {
    await input.keyDown("Shift");
    try { await input.pressKey("Tab"); } finally { await input.keyUp("Shift"); }
    await wait(cdp, barrier, "Shift+Tab leaves the completed history disclosure", (surface) => !surface.destination.summary_focused);
  }
  let steps = 0;
  for (; steps < 40; steps += 1) {
    if ((await observe(cdp, barrier)).destination.summary_focused) break;
    await input.pressKey("Tab");
  }
  if (steps === 40) throw fail("Keyboard navigation did not reach the completed history disclosure", { steps });
  const before = await observe(cdp, barrier);
  const afterSequence = (await input.snapshotProbe()).sequence;
  await input.pressKey("Enter");
  const snapshot = await input.snapshotProbe(afterSequence);
  const after = await observe(cdp, barrier);
  await sink.record("output-history-keyboard-delivery", { target, steps, before, after, snapshot }, { phase: "executing", owner: OWNER });
  const proof = outputHistoryKeyboardProof(snapshot, afterSequence, target);
  await sink.record("output-history-keyboard", { target, steps, proof }, { phase: "executing", owner: OWNER });
}

export function createOutputHistoryNavigationScenario() {
  const state = { provider: null, acceptedLedger: null, quiesce: null, cleanupFailures: [] };
  return Object.freeze({
    id: "output.history-navigation", productOracle: "pass", manualGate: "pending", databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({ expectedPrompt: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
        responseBehavior: "hold_until_release", script: createChatToolContinuationProviderScript() });
      await prepareDesktopFixture({ context, sink, phase, owner: OWNER,
        configText: providerChatToolContinuationFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_OUTPUT_HISTORY.txt", sentinelText: "Output history GUI navigation fixture.\n" });
      await sink.record("output-history-provider", state.provider.resourceObservation(), { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink }) {
      await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "output-history-shell" });
      const input = new WebviewInput(cdp, { probeId: "output-history-navigation" });
      const commands = new DesktopCommandProbe(cdp, { probeId: "output-history-navigation-commands", commands: ["submit_prompt", "cancel_run", "submit_side_chat", "cancel_side_chat"] });
      const barrier = new DesktopStatePollBarrier(cdp, { barrierId: "output-history-navigation-poll" });
      let primary = null;
      try {
        await input.installProbe(); await commands.install();
        if (state.provider.requestLedger.length) throw fail("Unexpected provider request before GUI Send", state.provider.requestLedger);
        const initial = await observe(cdp, null);
        const initialDraft = outputHistoryDraftBaseline(initial.projection);
        await type(input, SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT, sink);
        const ready = await wait(cdp, null, "Output history prompt ready", (surface) => surface.projection.overlay === "none" && surface.errors === 0
          && same(surface.projection.run_target, initial.projection.run_target)
          && outputHistoryDraftFailures(surface, { draft: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
            baseline: initialDraft, focus: "prompt", sameNode: false }).length === 0);
        const expected = { command: "submit_prompt", args: { text: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
          expectedTarget: ready.projection.draft_target, expectedRunTarget: ready.projection.run_target } };
        const commandStart = (await commands.snapshot()).sequence;
        await recordClick(input, SEND, sink);
        let held = await wait(cdp, null, "One completed tool with the continuation held", (surface) =>
          exactChatToolContinuationLedger(state.provider.requestLedger, ["completed", "held"])
          && surface.projection.run_status_key === "running" && surface.projection.busy
          && outputHistoryOwner(surface.projection) !== null && surface.projection.transcript_rows.some((row) => row.row_kind === "work_summary_running"));
        const owner = outputHistoryOwner(held.projection);
        if (held.pane.collapsed) await recordClick(input, PANE, sink);
        held = await wait(cdp, null, "Output pane history route is available", (surface) => surface.pane.mode === "output"
          && !surface.pane.collapsed && surface.route.count === 1 && surface.route.enabled);
        await type(input, OUTPUT_HISTORY_DRAFT, sink);
        await input.keyDown("Shift");
        try { await input.pressKey("Home"); } finally { await input.keyUp("Shift"); }
        await remember(cdp, held);
        const selection = [0, OUTPUT_HISTORY_DRAFT.length];
        const draft = { owner, phase: "running", draft: OUTPUT_HISTORY_DRAFT, selection,
          baseline: outputHistoryDraftBaseline(held.projection), destinationIdentity: outputHistoryDestinationIdentity(held), focus: "prompt" };
        await wait(cdp, null, "Running editor retains the unsent draft", (surface) => outputHistoryNavigationFailures(surface, draft).length === 0);
        await stable(cdp, null, sink, "output-history-running-draft-focus", draft);
        await recordClick(input, summaryTarget(held), sink);
        await wait(cdp, null, "Running history collapsed through its summary", (surface) => surface.destination.details_open === false);
        await recordClick(input, DETAIL, sink);
        const running = { ...draft, focus: "summary" };
        await wait(cdp, null, "Pointer opens and focuses running history", (surface) => outputHistoryNavigationFailures(surface, running).length === 0);
        await barrier.install(); await barrier.arm();
        await waitForObservation({ label: "An actual frontend desktop_state poll is waiting", timeoutMs: 5000, pollMs: 30,
          retrySampleErrors: false, sample: () => barrier.snapshot(), accept: (value) => value.phase === "waiting" });
        const resumed = await barrier.resumeFresh();
        if (resumed.delivered !== true) throw new DesktopE2eError("harness", "output-history-poll-not-delivered", "No frontend poll was delivered", resumed);
        await stable(cdp, barrier, sink, "output-history-running-focus-after-poll", running);
        await sink.record("output-history-running-poll-delivery", resumed, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "output-history-running-pointer-detail", owner: OWNER });
        await barrier.remove();
        state.provider.releaseScriptRole("chat_continuation");
        const completed = { ...draft, phase: "completed", focus: "none" };
        let terminal = await wait(cdp, null, "One completed response with the draft retained", (surface) =>
          exactChatToolContinuationLedger(state.provider.requestLedger, ["completed", "completed"])
          && outputHistoryNavigationFailures(surface, { ...completed, revealed: surface.destination.details_open }).length === 0
          && surface.projection.transcript_rows.filter((row) => row.row_kind === "assistant").map((row) => row.body).join('') === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE);
        await sink.record("output-history-completion-settled", terminal, { phase: "executing", owner: OWNER });
        if (terminal.destination.details_open) {
          await recordClick(input, summaryTarget(terminal), sink);
          terminal = await wait(cdp, null, "Completed history collapsed by user", (surface) =>
            outputHistoryNavigationFailures(surface, { ...completed, revealed: false, focus: "summary" }).length === 0);
        }
        await remember(cdp, terminal);
        await keyboardCompletedDisclosure(input, cdp, null, sink, summaryTarget(terminal));
        const final = { ...completed, focus: "summary" };
        await wait(cdp, null, "Keyboard opens and focuses completed history", (surface) => outputHistoryNavigationFailures(surface, final).length === 0);
        await stable(cdp, null, sink, "output-history-completed-draft-and-focus", final);
        await captureScenarioScreenshot({ cdp, sink, name: "output-history-completed-keyboard-detail", owner: OWNER });
        const commandProof = assertExactDesktopCommandSequence(await commands.snapshot(commandStart), { afterSequence: commandStart, expected: [expected] });
        state.acceptedLedger = state.provider.requestLedger;
        if (!exactChatToolContinuationLedger(state.acceptedLedger, ["completed", "completed"])) throw fail("History navigation created another provider request", state.acceptedLedger);
        await sink.record("output-history-navigation-result", { owner, commandProof, ledger: state.acceptedLedger,
          unsent_draft: OUTPUT_HISTORY_DRAFT, committed_draft_baseline: draft.baseline,
          destination_identity: draft.destinationIdentity, selection, data_seed: "none; history created by GUI Send",
          navigation: { running: "output activity route via pointer", completed: "canonical transcript disclosure via Tab/Enter" },
          visual_review_required: ["running pointer disclosure and activity status visibility", "completed keyboard disclosure and focus visibility", "unsent draft remains readable without clipping"] }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "pending" };
      } catch (error) { primary = error; throw error; }
      finally {
        for (const [kind, operation] of [["poll", () => barrier.remove()], ["input", () => input.cleanup()],
          ["commands", () => commands.remove()], ["nodes", () => cdp.evaluate(`delete globalThis[Symbol.for('${NODE_KEY}')]`)]]) {
          try { await operation(); } catch (error) { state.cleanupFailures.push({ kind, error: error?.message ?? String(error) }); }
        }
        if (!primary && state.cleanupFailures.length) throw new DesktopE2eError("harness", "output-history-cleanup", "Scenario input resources did not settle", state.cleanupFailures);
      }
    },
    async quiesce({ inputs }) {
      state.quiesce ??= await quiesceProviderResource({ provider: state.provider, acceptedLedger: state.acceptedLedger, inputs });
      return structuredClone(state.quiesce);
    },
    async cleanup() {
      return { input: state.quiesce?.input === "pass" && state.cleanupFailures.length === 0 ? "pass" : "fail",
        resources: [{ kind: "output-history-navigation", failures: state.cleanupFailures }] };
    },
  });
}
