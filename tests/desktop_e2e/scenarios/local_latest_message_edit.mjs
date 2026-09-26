import { DesktopE2eError } from "../core/execution.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { startScriptedProvider } from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { providerRestartFixtureConfig, quiesceProviderResource } from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { wait } from "./hub_browser_enrollment.mjs";

const ID = "run.latest-message-edit", OWNER = `scenario:${ID}`;
const ORIGINAL = "desktop-local-edit-original: 最初の依頼です。";
const REVISED = "desktop-local-edit-revised: 修正後の依頼です。";
const PROMPT = { selector: "section.composer textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
const SEND = { selector: 'section.composer button[data-action="send"]', identity: { tag: "BUTTON", action: "send" } };
const fail = (message, evidence = {}) => new DesktopE2eError("product", "local-latest-edit-mismatch", message, evidence);

/** Actual Tauri window: edit the latest completed local message and send its fork. */
export function createLocalLatestMessageEditScenario() {
  const state = { provider: null, input: null, acceptedLedger: null, close: null };
  return Object.freeze({ id: ID, productOracle: "pass", manualGate: "not_required", databaseRequired: true,
    requestGracefulExit,
    async prepare(args) {
      state.provider = await startScriptedProvider({ turns: [
        { prompt: ORIGINAL, responseText: "LOCAL_ORIGINAL_OK" },
        { prompt: REVISED, responseText: "LOCAL_REVISED_OK" },
      ] });
      await prepareDesktopFixture({ ...args, owner: OWNER, configText: providerRestartFixtureConfig(state.provider.baseUrl) });
    },
    async execute({ context, driver: cdp, sink }) {
      const desktop = () => invokeDesktopCommand(cdp, "desktop_state");
      await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "local-edit-shell-ready" });
      await cdp.call("Runtime.enable"); await cdp.call("DOM.enable");
      state.input = new WebviewInput(cdp, { probeId: ID }); await state.input.installProbe();
      const click = target => state.input.click(target, { stableHitSamples: 3 });
      async function fill(target, value) {
        await click(target);
        await state.input.keyDown("Control"); await state.input.pressKey("a"); await state.input.keyUp("Control");
        await state.input.insertText(target, value);
      }
      try {
        await fill(PROMPT, ORIGINAL); await click(SEND);
        const original = await wait("The original local message finishes", desktop,
          p => p.run_status_key === "completed" && p.task_activity_state === "idle" && p.can_submit
            && p.transcript_rows?.some(row => row.row_kind === "assistant" && row.body.includes("LOCAL_ORIGINAL_OK")), 60000);
        const sourceSession = original.run_target.sessionId;
        const latestUser = [...original.transcript_rows].reverse().find(row => row.row_kind === "user");
        if (!sourceSession || latestUser?.body !== ORIGINAL || !latestUser.stable_history_identity) throw fail("The original message has no editable canonical owner");
        const editButton = { selector: `button[data-action="edit-local-message"][data-value=${JSON.stringify(latestUser.stable_history_identity)}]`,
          identity: { tag: "BUTTON", action: "edit-local-message" } };
        await wait("Only the latest completed user message offers edit", () => cdp.evaluate(`(() => {
          const actions = [...document.querySelectorAll('button[data-action="edit-local-message"]')];
          return { count: actions.length, value: actions[0]?.getAttribute('data-value'), disabled: actions[0]?.disabled };
        })()`), value => value.count === 1 && value.value === latestUser.stable_history_identity && value.disabled === false);
        await click(editButton);
        const fork = await wait("The edited message opens as a new local chat with its text", async () => ({
          state: await desktop(), prompt: await cdp.evaluate(`document.querySelector('textarea#prompt')?.value`),
        }), value => value.state.run_target.sessionId && value.state.run_target.sessionId !== sourceSession
          && value.state.draft_target.sessionId === value.state.run_target.sessionId && value.prompt === ORIGINAL, 30000);
        if (!fork.state.session_rows.some(row => row.session_id === sourceSession)) throw fail("The original chat disappeared after editing");
        await captureScenarioScreenshot({ cdp, sink, name: "local-latest-message-edit-draft", owner: OWNER });
        await fill(PROMPT, REVISED); await click(SEND);
        const revised = await wait("The revised local message completes in the new chat", desktop,
          p => p.run_target.sessionId === fork.state.run_target.sessionId && p.run_status_key === "completed"
            && p.transcript_rows?.some(row => row.row_kind === "assistant" && row.body.includes("LOCAL_REVISED_OK")), 60000);
        await wait("The revised answer is visible in the actual Tauri chat", () => cdp.evaluate(`({
          assistant: [...document.querySelectorAll('article.message.assistant')].some(row => row.innerText.includes('LOCAL_REVISED_OK')),
          prompt: document.querySelector('section.composer textarea#prompt')?.value,
          running: Boolean(document.querySelector('button[data-action="stop-main"]:not([hidden])'))
        })`), value => value.assistant && value.prompt === "", 30000);
        if (!revised.session_rows.some(row => row.session_id === sourceSession)) throw fail("The original chat was lost after resending");
        const accepted = state.provider.requestLedger.filter(row => row.route === "responses" && row.contract?.pass);
        if (accepted.length !== 2 || accepted.some(row => row.response_phase !== "completed"))
          throw fail("The provider did not receive exactly the original and revised requests", { accepted });
        state.acceptedLedger = structuredClone(state.provider.requestLedger);
        await captureScenarioScreenshot({ cdp, sink, name: "local-latest-message-edit-result", owner: OWNER });
        await sink.record("local-latest-message-edit-complete", { original_session_id: sourceSession,
          forked_session_id: fork.state.run_target.sessionId, edited_history_item_id: latestUser.stable_history_identity,
          provider_requests: accepted.length, scope: "Actual Tauri window and isolated provider on one Windows PC." },
          { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        await sink.record("local-latest-message-edit-failure", { desktop: await desktop().catch(() => null) },
          { phase: "executing", owner: OWNER }).catch(() => {});
        await captureScenarioScreenshot({ cdp, sink, name: "local-latest-message-edit-failure", owner: OWNER }).catch(() => {});
        throw error;
      } finally { await state.input.cleanup(); state.input = null; }
    },
    async quiesce({ inputs }) { return state.close ??= await quiesceProviderResource({ provider: state.provider,
      acceptedLedger: state.acceptedLedger, inputs }); },
    async cleanup() { return { input: state.close?.input ?? "fail", resources: [] }; },
  });
}
