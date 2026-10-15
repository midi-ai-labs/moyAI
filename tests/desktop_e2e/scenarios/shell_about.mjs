import { readFile } from "node:fs/promises";

import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import {
  acquireInteractiveShell,
  prepareShellBaseline,
  quiesceShellBaseline,
  requestGracefulExit,
} from "./shell_baseline.mjs";

const OWNER = "scenario:shell.about";
const HELP = { selector: 'button[data-action="show-help-menu"]', identity: { tag: "BUTTON", action: "show-help-menu" } };
const ABOUT = { selector: 'button[data-action="show-about"]', identity: { tag: "BUTTON", action: "show-about" } };
const CLOSE = {
  selector: '[role="dialog"][aria-labelledby="about-dialog-title"] .modal-actions button[data-action="close-overlay"]',
  identity: { tag: "BUTTON", action: "close-overlay" },
};

async function observeSurface(cdp) {
  return cdp.evaluate(`(async () => {
    const projection = await window.__TAURI_INTERNALS__.invoke('desktop_state');
    const visible = (element) => {
      if (!(element instanceof HTMLElement) || !element.isConnected) return false;
      const style = getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity) !== 0
        && rect.width > 0 && rect.height > 0;
    };
    const dialogs = Array.from(document.querySelectorAll('[role="dialog"][aria-labelledby="about-dialog-title"]'));
    const dialog = dialogs.length === 1 ? dialogs[0] : null;
    return {
      overlay: projection.overlay,
      about: projection.about,
      about_action_visible: visible(document.querySelector('button[data-action="show-about"]')),
      dialog_count: dialogs.length,
      dialog_visible: visible(dialog),
      aria_modal: dialog?.getAttribute('aria-modal') ?? null,
      title: dialog?.querySelector('#about-dialog-title')?.textContent?.trim() ?? null,
      rows: Array.from(dialog?.querySelectorAll('dl > div') ?? []).map((row) => ({
        label: row.querySelector('dt')?.textContent?.trim() ?? null,
        value: row.querySelector('dd')?.textContent?.trim() ?? null,
        visible: visible(row),
      })),
      focus_inside_dialog: dialog !== null && dialog.contains(document.activeElement),
      fatal_count: Array.from(document.querySelectorAll('.fatal, .ui-error-notice')).filter(visible).length,
    };
  })()`);
}

export function aboutMetadataReady(surface, expected) {
  const exactRow = (label, value) => {
    const rows = surface?.rows?.filter((row) => row.label === label) ?? [];
    return rows.length === 1 && rows[0].value === value && rows[0].visible === true;
  };
  return surface?.overlay === "about"
    && surface.dialog_count === 1
    && surface.dialog_visible === true
    && surface.aria_modal === "true"
    && surface.focus_inside_dialog === true
    && surface.title === "moyAIについて"
    && surface.about?.product_name === "moyAI"
    && surface.about?.version === expected.version
    && surface.about?.codename === expected.codename
    && exactRow("バージョン", expected.version)
    && exactRow("コードネーム", expected.codename)
    && surface.fatal_count === 0;
}

async function clickOnce(input, locator, sink) {
  const start = (await input.snapshotProbe()).sequence;
  const target = await input.click(locator);
  const probe = assertTrustedProbeSequence(await input.snapshotProbe(start), {
    afterSequence: start,
    expected: [
      { type: "pointerdown", identity: locator.identity, button: 0, buttons: 1 },
      { type: "pointerup", identity: locator.identity, button: 0, buttons: 0 },
      { type: "click", identity: locator.identity, button: 0, buttons: 0 },
    ],
  });
  await sink.record("about-trusted-action", { target, probe }, { phase: "executing", owner: OWNER });
}

async function waitForSurface(cdp, label, accept) {
  try {
    return await waitForObservation({
      label,
      timeoutMs: 10_000,
      pollMs: 50,
      sample: () => observeSurface(cdp),
      accept,
      retrySampleErrors: false,
    });
  } catch (error) {
    if (error?.code !== "observation-timeout" || error?.evidence?.last_error) throw error;
    throw new DesktopE2eError("product", "about-surface-mismatch", label, error.evidence);
  }
}

export function createShellAboutScenario() {
  let expected = null;
  let inputCleanup = { input: "pass", resources: [] };
  return Object.freeze({
    id: "shell.about",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    async prepare(args) {
      await prepareShellBaseline(args);
      const manifest = JSON.parse(await readFile(new URL("../../../package.json", import.meta.url), "utf8"));
      expected = { version: manifest.version, codename: "LYNX" };
    },
    requestGracefulExit,
    quiesce: quiesceShellBaseline,
    async cleanup() { return inputCleanup; },
    async execute({ context, driver: cdp, sink }) {
      await acquireInteractiveShell({ context, driver: cdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "about-shell-ready",
      });
      const input = new WebviewInput(cdp, { probeId: "shell-about" });
      let primaryError = null;
      try {
        await input.installProbe();
        await clickOnce(input, HELP, sink);
        await waitForSurface(cdp, "Help menu exposes About", (value) => value.overlay === "help_menu" && value.about_action_visible);
        await clickOnce(input, ABOUT, sink);
        const opened = await waitForSurface(cdp, "About shows current version and codename", (value) => aboutMetadataReady(value, expected));
        await sink.record("about-metadata-verified", { expected, observation: opened.value }, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "about-version-codename", owner: OWNER });
        await clickOnce(input, CLOSE, sink);
        await waitForSurface(cdp, "About closes through OK", (value) => value.overlay === "none" && value.dialog_count === 0);
        await acquireInteractiveShell({ context, driver: cdp, sink }, {
          evidenceOwner: OWNER,
          screenshotStem: "about-shell-restored",
        });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        try {
          const result = await input.cleanup();
          inputCleanup = { input: "pass", resources: [{ owner: "webview-input", pass: true, result }] };
        } catch (error) {
          inputCleanup = { input: "fail", resources: [{ owner: "webview-input", pass: false, message: error?.message ?? String(error) }] };
          if (primaryError === null) throw error;
        }
      }
    },
  });
}
