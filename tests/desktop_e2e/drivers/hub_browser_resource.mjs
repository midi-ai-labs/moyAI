import { createRequire } from "node:module";
import { mkdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const defaultHubRepository = fileURLToPath(new URL("../../../../moyAI-Hub/", import.meta.url));
export function normalizeHubBrowserOptions(options = {}) {
  const keys = ["hubRepository", "hubBinary", "browserChannel", "headed"];
  if (!options || typeof options !== "object" || Array.isArray(options) || Object.keys(options).some(key => !keys.includes(key))) {
    throw new TypeError("Hub browser options must contain only hubRepository, hubBinary, browserChannel, headed");
  }
  const hubRepository = options.hubRepository ?? defaultHubRepository;
  const hubBinary = options.hubBinary ?? process.env.MOYAI_HUB_TEST_BINARY;
  const browserChannel = options.browserChannel ?? process.env.MOYAI_HUB_BROWSER_CHANNEL ?? (process.platform === "win32" ? "msedge" : "chromium");
  const headed = options.headed ?? true;
  if (!path.isAbsolute(hubRepository) || hubBinary && !path.isAbsolute(hubBinary)) throw new TypeError("Hub repository and executable paths must be absolute");
  if (!["msedge", "chrome", "chromium"].includes(browserChannel) || typeof headed !== "boolean") throw new TypeError("Invalid Hub browser channel or headed option");
  return { hubRepository: path.resolve(hubRepository), hubBinary, browserChannel, headed };
}

/** Shares Hub's server owner. The Desktop host continues to own only the actual Tauri app. */
export async function startHubBrowserResource({ context, sink, phase = "prepared", options = {} }) {
  const settings = normalizeHubBrowserOptions(options);
  let evidencePhase = phase;
  const record = (name, value) => sink.record(name, value, { phase: evidencePhase, owner: "hub-browser-resource" });
  const requireHub = createRequire(path.join(settings.hubRepository, "package.json"));
  const { chromium } = requireHub("playwright");
  const { startHubServer } = await import(pathToFileURL(path.join(settings.hubRepository, "tests/browser/hub_server.mjs")));
  const { createMetadataProvider } = await import(pathToFileURL(path.join(settings.hubRepository, "tests/browser/provider-fixture.mjs")));
  const root = path.join(context.root, "hub-browser");
  await mkdir(root, { recursive: false });
  const downloads = path.join(root, "downloads"), temporary = path.join(root, "temp");
  await mkdir(downloads); await mkdir(temporary);
  let hub = null, provider = null, browserContext = null, closePromise;
  const pageErrors = [];
  const close = () => closePromise ??= (async () => {
    evidencePhase = "cleaning";
    const failures = [];
    let hubResult = null, browserClosed = browserContext === null, providerClosed = provider === null;
    if (browserContext) try { await browserContext.close(); browserClosed = true; } catch { failures.push("browser-context-close"); }
    // Revoke test credentials and stop every Hub listener before closing its metadata upstream.
    if (hub) try { hubResult = await hub.close(); if (!hubResult.pass) failures.push("hub-lifecycle"); } catch { failures.push("hub-close"); }
    if (provider) try { await provider.close(); providerClosed = true; } catch { failures.push("metadata-provider-close"); }
    const result = { pass: failures.length === 0 && browserClosed && providerClosed, browser_closed: browserClosed,
      provider_closed: providerClosed, hub: hubResult, page_errors: [...pageErrors], failures };
    await record("hub-browser-resource-closed", result);
    return result;
  })();
  try {
    provider = await createMetadataProvider();
    hub = await startHubServer({ dataDirectory: path.join(root, "data"), binary: settings.hubBinary, record });
    browserContext = await chromium.launchPersistentContext(path.join(root, "browser-profile"), {
      headless: !settings.headed, ...(settings.browserChannel === "chromium" ? {} : { channel: settings.browserChannel }),
      acceptDownloads: true, downloadsPath: downloads, ignoreHTTPSErrors: false, viewport: { width: 1440, height: 1000 },
      env: { ...process.env, TEMP: temporary, TMP: temporary, TMPDIR: temporary },
    });
    browserContext.setDefaultTimeout(15_000);
    const page = browserContext.pages()[0] ?? await browserContext.newPage();
    page.on("pageerror", error => pageErrors.push(error.message));
    await record("hub-browser-started", { root, channel: settings.browserChannel, headed: settings.headed, url: hub.url,
      browser_version: browserContext.browser()?.version() ?? null, playwright_version: requireHub("playwright/package.json").version });
    const readOnlyHub = Object.freeze({ url: hub.url, networkPort: hub.networkPort, observeNetwork: hub.observeNetwork,
      command: (name, args = {}) => {
        if (!["hub_snapshot", "hub_network_snapshot", "hub_web_status"].includes(name) || Object.keys(args).length) {
          throw new TypeError("Combined GUI scenario private Hub commands are read-only");
        }
        return hub.command(name);
      },
    });
    return Object.freeze({ hub: readOnlyHub, page, provider, downloads, close,
      pageErrors: () => [...pageErrors],
      screenshot: async name => {
        evidencePhase = "executing";
        const bytes = await page.screenshot();
        const artifact = await sink.writeBytes(`screenshots/${name}.png`, bytes);
        await record("hub-browser-screenshot", { name, artifact });
      },
    });
  } catch (error) {
    const cleanup = await close();
    if (!cleanup.pass) throw new AggregateError([error], "Hub browser setup failed and cleanup did not pass");
    throw error;
  }
}
