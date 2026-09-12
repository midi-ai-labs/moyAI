import path from "node:path";
import { mkdir } from "node:fs/promises";
import { DesktopE2eError } from "../core/execution.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { startScriptedProvider } from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { providerRestartFixtureConfig } from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { byId, action, wait, trustedClick } from "./hub_browser_enrollment.mjs";

const OWNER = "scenario:navigation.running-controls", PROMPT = "Keep this isolated GUI navigation run open.";
const fail = (message, evidence) => new DesktopE2eError("product", "running-navigation-mismatch", message, evidence);
export function createRunningNavigationScenario() {
  const state = { provider: null, input: null, failures: [], close: null };
  return Object.freeze({ id: "navigation.running-controls", productOracle: "pass", manualGate: "pending", databaseRequired: true,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({ expectedPrompt: PROMPT, responseBehavior: "hold_until_peer_close" });
      await prepareDesktopFixture({ context, sink, phase, owner: OWNER, configText: providerRestartFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_RUNNING_NAV.txt", sentinelText: "Running GUI navigation fixture.\n" });
      await mkdir(path.join(context.paths.workspace, ".git"));
    },
    async execute({ context, driver: cdp, sink }) {
      await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "running-navigation-shell" });
      const input = state.input = new WebviewInput(cdp, { probeId: "running-navigation" });
      await input.installProbe();
      await trustedClick(input, cdp, byId("prompt", "TEXTAREA"), sink);
      await input.insertText(byId("prompt", "TEXTAREA"), PROMPT);
      await trustedClick(input, cdp, action("send", "section.composer"), sink);
      const running = await wait("Real Main turn is running with one held provider response", async () => ({
        p: await invokeDesktopCommand(cdp, "desktop_state"), ledger: state.provider.requestLedger,
      }), value => value.p.run_status_key === "running" && value.ledger.filter(row => row.route === "responses").length === 1
        && value.ledger.some(row => row.contract?.pass && row.response_phase === "held"));
      await trustedClick(input, cdp, action("refresh", "aside.sidebar"), sink);
      const refreshed = await wait("Manual refresh preserves the same running owner", () => invokeDesktopCommand(cdp, "desktop_state"),
        value => value.run_status_key === "running" && JSON.stringify(value.run_target) === JSON.stringify(running.p.run_target));
      const rowIndex = refreshed.session_rows.findIndex(row => row.loaded_status === "active");
      if (rowIndex < 0) throw fail("No active session row for the running fixture", refreshed.session_rows);
      const rowButton = name => ({ selector: `button[data-action="${name}"][data-index="${rowIndex}"]`, identity: { tag: "BUTTON", action: name } });
      const guarded = await wait("Selected running session keeps navigation controls disabled", () => cdp.evaluate(`(() => {
        return ${JSON.stringify([rowButton("rejoin-session").selector,rowButton("interrupt-session").selector])}.map(selector=>{
          const e=document.querySelector(selector),r=e?.getBoundingClientRect();
          return {selector,visible:Boolean(r?.width&&r?.height),disabled:e?.disabled,aria:e?.getAttribute('aria-disabled')};
        });
      })()`), value => value.every(row=>row.visible&&row.disabled&&row.aria==='true'));
      if (state.provider.requestLedger.filter(row => row.route === "responses").length !== 1) throw fail("Refresh sent a second model request");
      await captureScenarioScreenshot({ cdp, sink, name: "running-session-navigation-disabled", owner: OWNER });
      await trustedClick(input, cdp, action("cancel-run", "section.run-strip"), sink);
      const terminal = await wait("Main Stop ends the exact running session", async () => ({
        p: await invokeDesktopCommand(cdp, "desktop_state"), ledger: state.provider.requestLedger,
      }), value => value.p.run_status_key === "cancelled" && !value.p.busy && value.p.can_submit
        && value.ledger.some(row => row.response_phase === "peer_closed"));
      await captureScenarioScreenshot({ cdp, sink, name: "running-session-interrupted", owner: OWNER });
      await sink.record("running-navigation-controls", { running, refreshed, guarded, terminal,
        residual:"Rejoin and sidebar Interrupt require an active session outside this Desktop's current busy run; only their disabled state is verified here." }, { phase: "executing", owner: OWNER });
      return { acquisition: "pass", oracle: "pass", manual: "pending" };
    },
    async requestGracefulExit(cdp) {
      if (state.input) { try { await state.input.cleanup(); } catch { state.failures.push("input-cleanup"); } state.input = null; }
      return requestGracefulExit(cdp);
    },
    async quiesce() {
      if (state.input) { try { await state.input.cleanup(); } catch { state.failures.push("input-cleanup"); } state.input = null; }
      const provider = state.provider ? await state.provider.close() : { pass: true };
      state.close = { pass: provider.pass && !state.failures.length, provider, failures: [...state.failures] };
      return { input: state.close.pass ? "pass" : "fail", resources: [{ kind: "running-navigation", ...state.close }] };
    },
    async cleanup() { return { input: state.close?.pass ? "pass" : "fail", resources: [] }; },
  });
}
