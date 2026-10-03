import { isDeepStrictEqual } from "node:util";
import { waitForObservation } from "../core/deadline.mjs";
import { canonicalUlid } from "../core/canonical_identity.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { manualLiveClick } from "../drivers/manual_live_session.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { acquireInteractiveShell } from "./shell_baseline.mjs";
import { createProviderChatToolContinuationScenario, exactChatToolContinuationLedger,
  exactTurnOwner, observeChatToolContinuationSurface, terminalSettled } from "./provider_chat_tool_continuation.mjs";

const ID = "history.tool-operation-failure";
const OWNER = `scenario:${ID}`;
export const TOOL_OPERATION_FAILURE_PROMPT = "Run the one deliberately failing unittest once, then report its expected failure.";
export const TOOL_OPERATION_FAILURE_RESPONSE = "EXPECTED_UNITTEST_FAILURE_RECORDED";
export const TOOL_OPERATION_FAILURE_PROGRESS = "ツール: 1件開始 / 0件完了 / 0件拒否 / 0件キャンセル / 1件失敗";
export const TOOL_OPERATION_FAILURE_FIXTURE = `import unittest

class ExpectedFailure(unittest.TestCase):
    def test_expected_failure(self):
        self.fail("EXPECTED_UNITTEST_FAILURE")
`;
const OUTPUT_MAX_BYTES = 2048; // Host non-success projection is bounded to 2 KiB, including its audit line.
const PANE = { selector: '.topbar button[data-action="toggle-artifact-pane"]', identity: { tag: "BUTTON", action: "toggle-artifact-pane" } };
const rows = (p, kind) => (p?.transcript_rows ?? []).filter(row => row.row_kind === kind);
const failedRows = text => [...(text ?? "").matchAll(/^- \[失敗\] /gmu)].length;

export function toolOperationFailureCall(context) {
  return { prompt: TOOL_OPERATION_FAILURE_PROMPT, name: "shell", arguments: {
    command: "python -m unittest", workdir: context.paths.workspace,
  }, outputMarker: "lifecycle_status: completed\nkind: process_exit_nonzero\nautomatic_retry: false\nexit_code: 1",
  outputMaxBytes: OUTPUT_MAX_BYTES, responseText: TOOL_OPERATION_FAILURE_RESPONSE };
}

function blockingFailure(surface) {
  return surface?.visible_fatal_count > 0 || surface?.visible_recoverable_error_count > 0
    || surface?.visible_validation_error_count > 0 || surface?.projection?.startup?.status === "failed"
    || ["failed", "cancelled", "incomplete"].includes(surface?.projection?.run_status_key)
    || surface?.projection?.confirmation_visible === true;
}

export function toolOperationFailureFailures(sample, { phase = "held", previousProjection = null, heldProjection } = {}) {
  const surface = sample?.surface, p = surface?.projection, held = phase === "held";
  const errors = rows(p, "error"), users = rows(p, "user"), assistants = rows(p, "assistant");
  const summaries = rows(p, held ? "work_summary_running" : "work_summary_completed");
  const owner = exactTurnOwner(p, held ? "turn" : "idle");
  const failures = [];
  if (!exactChatToolContinuationLedger(sample?.ledger, ["completed", held ? "held" : "completed"], OUTPUT_MAX_BYTES)) failures.push("provider-ledger-not-exact");
  if (blockingFailure(surface) || owner === null || (held
    ? p?.busy !== true || p.run_status_key !== "running" || p.task_activity_state !== "running"
    : !terminalSettled(surface))) failures.push("run-owner-or-terminal-mismatch");
  if (!held && heldProjection !== undefined
    && !isDeepStrictEqual(exactTurnOwner(heldProjection, "turn"), owner)) failures.push("held-terminal-owner-changed");
  if (users.length !== 1 || users[0].body !== TOOL_OPERATION_FAILURE_PROMPT || !canonicalUlid(users[0].stable_history_identity)) failures.push("user-not-exact");
  if (held) {
    // Running canonical rows embed tool evidence in the work summary; the
    // independent full error row appears after the terminal history refresh.
    if (errors.length !== 0 || surface?.errors?.length !== 0) failures.push("held-independent-error-row-unexpected");
    if (!/Exit code: 1(?:\s|$)/u.test(summaries[0]?.body ?? "")) failures.push("held-summary-failure-result-missing");
    const visibleSummary = surface?.running_summaries;
    if (visibleSummary?.length !== 1 || visibleSummary[0].visible !== true
      || visibleSummary[0].history_identity !== summaries[0]?.stable_history_identity
      || !/Exit code: 1(?:\s|$)/u.test(visibleSummary[0].text ?? "")) failures.push("held-summary-dom-owner-mismatch");
  } else {
    const sourceIdentity = errors[0]?.stable_history_identity ?? null;
    const body = errors[0]?.body ?? "";
    if (errors.length !== 1 || (sourceIdentity !== null && !canonicalUlid(sourceIdentity))
      || !/Exit code: 1(?:\r?\n|$)/u.test(body)
      || !/Ran 1 test in /u.test(body) || !body.includes("FAILED (failures=1)")) failures.push("unittest-failure-result-not-exact");
    // The fixture's unittest dash separators render as text-free <hr> elements.
    // Retain the raw result above and compare all remaining rendered text exactly.
    const expectedText = body.split(/\r?\n/u).filter(line => !/^-{3,}$/u.test(line.trim()))
      .join("\n").replace(/\s+/gu, " ").trim();
    const visibleError = surface?.errors?.[0];
    const actualText = typeof visibleError?.text === "string" ? visibleError.text.replace(/\s+/gu, " ").trim() : null;
    if (surface?.errors?.length !== 1 || visibleError.visible !== true
      || visibleError.history_identity !== sourceIdentity || actualText !== expectedText) failures.push("failure-dom-owner-mismatch");
  }
  if (summaries.length !== 1 || summaries[0].stable_history_identity !== `turn:${owner?.turnId}:work-summary`
    || rows(p, held ? "work_summary_completed" : "work_summary_running").length !== 0) failures.push("work-summary-owner-mismatch");
  if (!(p?.progress_text ?? "").includes(TOOL_OPERATION_FAILURE_PROGRESS)) failures.push("progress-count-mismatch");
  if (failedRows(p?.tool_status_text) !== 1 || /^- \[(完了|拒否|キャンセル)\] /mu.test(p?.tool_status_text ?? "")) failures.push("tool-list-count-mismatch");
  if (failedRows(summaries[0]?.body) !== 1) failures.push(held ? "held-summary-failure-missing" : "completed-summary-failure-missing");
  if (held ? assistants.length !== 0 || surface?.assistants?.length !== 0
    : assistants.length !== 1 || assistants[0].body !== TOOL_OPERATION_FAILURE_RESPONSE
      || surface?.assistants?.length !== 1 || surface.assistants[0].text !== TOOL_OPERATION_FAILURE_RESPONSE
      || surface.assistants[0].history_identity !== assistants[0].stable_history_identity) failures.push("assistant-not-exact");
  if (previousProjection !== null && (!isDeepStrictEqual(exactTurnOwner(previousProjection, "idle"), owner)
    || !isDeepStrictEqual(previousProjection.transcript_rows.map(row => [row.row_kind, row.stable_history_identity, row.body]),
      p?.transcript_rows?.map(row => [row.row_kind, row.stable_history_identity, row.body]))
    || previousProjection.tool_status_text !== p?.tool_status_text)) failures.push("restart-canonical-history-changed");
  return failures;
}

export function toolOperationFailureVisibleFailures(surface, activity) {
  const p = surface?.projection;
  const failures = [];
  if (activity?.progress?.count !== 1 || activity.progress.visible !== true || activity.progress.text !== p?.progress_text) failures.push("visible-progress-not-exact");
  if (activity?.tools?.count !== 1 || activity.tools.visible !== true || activity.tools.text !== p?.tool_status_text) failures.push("visible-tool-list-not-exact");
  return failures;
}

async function observeActivity(cdp) {
  return cdp.evaluate(`(() => {
    const visible = node => { const r = node.getBoundingClientRect(), s = getComputedStyle(node);
      return r.width > 0 && r.height > 0 && s.display !== 'none' && s.visibility !== 'hidden' && Number(s.opacity) !== 0; };
    const group = label => { const nodes = [...document.querySelectorAll('.output-activity-section .output-activity-group')]
      .filter(node => node.querySelector('h4')?.textContent.trim() === label);
      return { count: nodes.length, visible: nodes.length === 1 && visible(nodes[0]), text: nodes[0]?.querySelector('pre')?.innerText ?? null }; };
    return { collapsed: document.querySelector('.app-frame')?.classList.contains('artifact-collapsed') ?? true,
      progress: group('進捗'), tools: group('ツール') };
  })()`);
}

export function createToolOperationFailureScenario() {
  return createProviderChatToolContinuationScenario({ profile: {
    id: ID, owner: OWNER, prompt: TOOL_OPERATION_FAILURE_PROMPT, call: toolOperationFailureCall,
    sentinelName: "test_tool_operation_failure.py", sentinelText: TOOL_OPERATION_FAILURE_FIXTURE,
    blockingFailure, heldFailures: sample => toolOperationFailureFailures(sample),
    terminalFailures: (sample, _heldTime, held) => toolOperationFailureFailures(sample,
      { phase: "terminal", heldProjection: held?.surface?.projection ?? null }),
    async observeHeld({ cdp, input, sink, held }) {
      if ((await observeActivity(cdp)).collapsed) await manualLiveClick(input, PANE, sink, "show-held-tool-failure", { owner: OWNER, stem: "tool-failure" });
      const observed = await waitForObservation({ label: "held failure activity DOM", timeoutMs: 10000, retrySampleErrors: false,
        sample: () => observeActivity(cdp), accept: activity => toolOperationFailureVisibleFailures(held.surface, activity).length === 0 });
      const activity = observed.value;
      const failures = toolOperationFailureVisibleFailures(held.surface, activity);
      await sink.writeJson("tool-failure/held.json", { ...held, activity, failures });
      if (failures.length) throw new DesktopE2eError("product", "tool-failure-visible-count-mismatch", "Held tool failure DOM and projection differ", { activity, failures });
    },
    async afterTerminal({ context, cdp, sink, host, scenario, provider, terminal, waitForProductStage }) {
      await sink.writeJson("tool-failure/terminal.json", terminal);
      const restarted = await host.restart({ context, scenario, sink, driver: cdp, phase: "executing" });
      await acquireInteractiveShell({ context, driver: restarted.driver, sink }, { evidenceOwner: OWNER, screenshotStem: "tool-failure-restarted-shell" });
      const restored = await waitForProductStage({ label: "tool failure display after restart",
        sample: async () => ({ surface: await observeChatToolContinuationSurface(restarted.driver), ledger: provider.requestLedger }),
        decide: sample => blockingFailure(sample?.surface) ? "fail" : toolOperationFailureFailures(sample,
          { phase: "restart", previousProjection: terminal.surface.projection }).length === 0 ? "pass" : "pending",
        code: "tool-failure-restart-count-mismatch", message: "Restart changed tool failure counts or canonical evidence" });
      await sink.writeJson("tool-failure/restart.json", restored.value);
      await captureScenarioScreenshot({ cdp: restarted.driver, sink, name: "tool-failure-restarted-terminal", owner: OWNER });
    },
  } });
}
