import path from "node:path";
import { lstat, mkdir } from "node:fs/promises";

export function normalizeDesktopIsolation(value = "user-wide") {
  if (value !== "user-wide" && value !== "fixture") throw new TypeError("desktop isolation must be user-wide or fixture");
  return value;
}

export function desktopFixtureRoot(context) {
  const root = path.dirname(path.resolve(context.paths.config));
  const relative = path.relative(path.resolve(context.root), root);
  if (relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) throw new TypeError("Desktop fixture root escaped execution");
  for (const [key, suffix] of Object.entries({ config: "config", config_file: "config/config.toml", data: "data", prefs: "prefs", prefs_file: "prefs/desktop.toml", webview: "webview" })) {
    if (path.resolve(context.paths[key]).toLowerCase() !== path.resolve(root, suffix).toLowerCase()) throw new TypeError(`Desktop fixture ${key} does not match its root`);
  }
  return root;
}

function sameOwner(left, right) {
  return left.process_id === right.process_id && left.process_start_time_utc_ticks === right.process_start_time_utc_ticks
    && path.resolve(left.executable_path).toLowerCase() === path.resolve(right.executable_path).toLowerCase();
}

export function desktopOwnersMatch(preserved, observed, owned = []) {
  const expected = [...preserved, ...owned];
  return expected.length === observed.length && new Set(expected.map(row => row.process_id)).size === expected.length
    && expected.every(owner => observed.filter(row => sameOwner(owner, row)).length === 1);
}

export async function prepareDesktopFixtureEnvironment(context) {
  const root = desktopFixtureRoot(context);
  const physical = async candidate => {
    const item = await lstat(candidate);
    if (!item.isDirectory() || item.isSymbolicLink()) throw new TypeError("Desktop fixture requires physical directories");
  };
  let current = path.resolve(context.root);
  await physical(current);
  for (const component of path.relative(current, root).split(path.sep).filter(Boolean)) {
    current = path.join(current, component);
    await physical(current);
  }
  for (const key of ["config", "data", "prefs", "webview"]) await physical(context.paths[key]);
  for (const name of ["temp", "resource-admission"]) {
    const candidate = path.join(root, name);
    try { await mkdir(candidate); } catch (error) { if (error.code !== "EEXIST") throw error; }
    await physical(candidate);
  }
  return { root, temp: path.join(root, "temp"), registry: path.join(root, "resource-admission") };
}

export function desktopLaunchEnvironment({ context, scenarioEnvironment = {}, processTemp, inherited = process.env }) {
  const mode = normalizeDesktopIsolation(context.desktopIsolation);
  const env = { ...inherited };
  // Windows environment names are case-insensitive. A parent fixture must not
  // supply an alternative spelling of any launch-owned identity or temp path.
  const owned = new Set(["MOYAI_CONFIG_PATH", "MOYAI_DATA_DIR", "MOYAI_DESKTOP_PREFS_PATH", "MOYAI_DESKTOP_E2E_ROOT", "MOYAI_DESKTOP_E2E_RUNNER", "MOYAI_TEST_RESOURCE_REGISTRY", "WEBVIEW2_USER_DATA_FOLDER", "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "TEMP", "TMP", "TMPDIR"]);
  for (const key of Object.keys(env)) if (owned.has(key.toUpperCase())) delete env[key];
  Object.assign(env, scenarioEnvironment, {
    MOYAI_CONFIG_PATH: context.paths.config_file, MOYAI_DATA_DIR: context.paths.data,
    MOYAI_DESKTOP_PREFS_PATH: context.paths.prefs_file, WEBVIEW2_USER_DATA_FOLDER: context.paths.webview,
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=0", TEMP: processTemp, TMP: processTemp, TMPDIR: processTemp, RUST_BACKTRACE: "1",
  });
  if (mode === "fixture") {
    const root = desktopFixtureRoot(context), registry = path.join(root, "resource-admission");
    if (scenarioEnvironment.MOYAI_TEST_RESOURCE_REGISTRY !== undefined && path.resolve(scenarioEnvironment.MOYAI_TEST_RESOURCE_REGISTRY).toLowerCase() !== registry.toLowerCase()) throw new TypeError("scenario resource registry conflicts with Desktop fixture root");
    env.MOYAI_DESKTOP_E2E_ROOT = root;
    env.MOYAI_TEST_RESOURCE_REGISTRY = registry;
  }
  return env;
}
