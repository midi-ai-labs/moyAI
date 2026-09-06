import path from "node:path";
import { mkdir, readFile, stat, writeFile } from "node:fs/promises";
import { HubTauriResource } from "../drivers/hub_tauri_resource.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";

const OWNER = "scenario:manual.hub-device-network";

export function normalizeDeviceNetworkOptions(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).sort().join() !== "hub_binary,model,provider_base_url") {
    throw new TypeError("Hub device network requires hub_binary, model and provider_base_url");
  }
  if (typeof value.hub_binary !== "string" || !path.isAbsolute(value.hub_binary)) {
    throw new TypeError("Hub binary must be absolute");
  }
  const url = new URL(value.provider_base_url);
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    throw new TypeError("Provider endpoint must be credential-free HTTP(S)");
  }
  if (typeof value.model !== "string" || !value.model.trim() || value.model.length > 256 || /[\r\n\x00]/.test(value.model)) {
    throw new TypeError("Model must be a bounded single-line identifier");
  }
  return { hubBinary: value.hub_binary, endpoint: url.href.replace(/\/$/, ""), model: value.model };
}

export function validateDeviceNetworkVerdict(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || !["pass", "fail"].includes(value.oracle)
    || !["pass", "fail", "pending"].includes(value.manual)
    || !Array.isArray(value.observations) || value.observations.length < 1 || value.observations.length > 64
    || value.observations.some(item => typeof item !== "string" || !item.trim() || item.length > 2048)
    || value.scope !== "same-host-hub-desktop") {
    throw new TypeError("Manual verdict requires explicit observations and same-host scope");
  }
  return { oracle: value.oracle, manual: value.manual, scope: value.scope, observations: value.observations };
}

/** Native Computer Use owns input and visual judgement; this scenario owns only intent. */
export function createHubDeviceNetworkScenario(rawOptions = {}) {
  const options = normalizeDeviceNetworkOptions(rawOptions);
  let hub = null;
  return {
    id: "manual.hub-device-network", productOracle: "not_run", manualGate: "pending", databaseRequired: true,
    async prepare({ context, sink, phase }) {
      await prepareDesktopFixture({ context, sink, phase, owner: OWNER, configMode: "absent",
        sentinelName: "HUB_DEVICE_NETWORK.txt", sentinelText: "Isolated Hub and Desktop device-network GUI acceptance.\n" });
      await mkdir(path.join(context.paths.workspace, ".git"));
    },
    async execute({ context, sink }) {
      hub = new HubTauriResource();
      await hub.start({ context, sink, binary: options.hubBinary, timeoutMs: 3_600_000 });
      await sink.record("native-manual-ready", { input: "Windows Computer Use", scope: "same-host-hub-desktop",
        product_mutation_via_cdp: false, verdict_path: path.join(context.root, "manual-verdict.json") }, { phase: "executing", owner: OWNER });
      await writeFile(path.join(context.root, "native-ready.json"), JSON.stringify({ root: context.root, paths: context.paths,
        hubProvider: { base_url: options.endpoint, model: options.model }, desktopStartsWithoutUserConfig: true }), { flag: "wx" });
      const observed = await waitForObservation({ label: "Native Hub and Desktop GUI operation", timeoutMs: 3_300_000, pollMs: 500,
        sample: async () => {
          const file = path.join(context.root, "manual-verdict.json");
          try {
            if ((await stat(file)).size > 128 * 1024) throw new TypeError("Manual verdict exceeds its bound");
            return validateDeviceNetworkVerdict(JSON.parse(await readFile(file, "utf8")));
          } catch (error) { if (error.code === "ENOENT") return null; throw error; }
        }, accept: value => value !== null, retrySampleErrors: false });
      await sink.record("native-manual-verdict", observed.value, { phase: "executing", owner: OWNER });
      return { acquisition: "pass", oracle: observed.value.oracle, manual: observed.value.manual };
    },
    async requestGracefulExit() { return { requested: true, reason: "Native Computer Use File menu Exit" }; },
    async quiesce({ sink }) {
      if (!hub) return { input: "pass", resources: [] };
      const outcome = await hub.close();
      await sink.record("device-network-hub-cleanup", outcome, { phase: "cleaning", owner: OWNER });
      return { input: outcome.input, resources: [outcome] };
    },
    async cleanup() { return { input: "pass", resources: [] }; },
  };
}
