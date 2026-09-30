import { DesktopE2eError } from "../core/execution.mjs";
import { action, trustedClick, wait } from "./hub_browser_enrollment.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";

const CONFIG = '[role="dialog"][aria-labelledby="config-dialog-title"]';
const PROMPT = `${CONFIG} [data-ai-connection="main"] details.hub-model-prompt`;

export async function verifyHubModelPromptSettings({ pc, expectedPrompt, owner }) {
  const { driver: cdp, input, sink } = pc;
  const before = await invokeDesktopCommand(cdp, "hub_projection");
  await trustedClick(input, cdp, action("show-config", "aside.sidebar"), sink);
  const observe = () => cdp.evaluate(`(() => {
    const rows = [...document.querySelectorAll(${JSON.stringify(PROMPT)})];
    return rows.map(node => {
      const text = node.querySelector('pre');
      return { key: node.dataset.detailsKey, open: node.open,
        text: text?.textContent, visible: Boolean(text?.getClientRects().length),
        read_only: Boolean(text && !text.isContentEditable && !node.querySelector('input,textarea,[contenteditable=true]')) };
    });
  })()`);
  const rows = await wait("Selected Hub model instructions appear in Desktop settings", observe, value => value.length > 0);
  if (rows.length !== 1 || rows[0].text !== expectedPrompt || !rows[0].read_only) {
    throw new DesktopE2eError("product", "hub-model-prompt-settings", "Desktop does not show the exact selected model instructions read-only", { rows, expected_prompt: expectedPrompt });
  }
  if (!rows[0].open) {
    await trustedClick(input, cdp, {
      selector: `${PROMPT}[data-details-key=${JSON.stringify(rows[0].key)}] > summary`,
      identity: { tag: "DETAILS", detailsKey: rows[0].key },
    }, sink);
  }
  const displayed = await wait("Selected model instructions expand visibly", observe,
    value => value.length === 1 && value[0].open && value[0].visible && value[0].text === expectedPrompt && value[0].read_only);
  await captureScenarioScreenshot({ cdp, sink, name: `${pc.name}-hub-model-system-prompt`, owner });
  await trustedClick(input, cdp, action("close-overlay", CONFIG), sink);
  await wait("Viewing model instructions closes without a settings mutation", () => invokeDesktopCommand(cdp, "desktop_state"), value => value.overlay === "none");
  const after = await invokeDesktopCommand(cdp, "hub_projection");
  if (after.settings_revision !== before.settings_revision || JSON.stringify(after.main_review) !== JSON.stringify(before.main_review)) {
    throw new DesktopE2eError("product", "hub-model-prompt-settings", "Viewing Hub instructions changed the saved model selection", { before_revision: before.settings_revision, after_revision: after.settings_revision });
  }
  await sink.record("hub-model-prompt-settings", { pc: pc.label, displayed, settings_unchanged: true, closed: true }, { phase: "executing", owner });
}
