import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, readFile, realpath, rename, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { DesktopE2eError } from "../core/execution.mjs";
import { createManagedExecutionRunner } from "../drivers/managed_execution_runner.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { action, byId, trustedFocus, wait } from "./hub_browser_enrollment.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";

const ID = "settings.project-folder-recovery", OWNER = `scenario:${ID}`;
const execute = promisify(execFile);
const fail = (message, evidence = {}) => new DesktopE2eError("product", "project-folder-recovery-mismatch", message, evidence);
const sameFolder = (actual, expected) => typeof actual === "string"
  && path.toNamespacedPath(path.resolve(actual)).toLowerCase() === path.toNamespacedPath(path.resolve(expected)).toLowerCase();
const absent = candidate => stat(candidate).then(() => false, error => { if (error.code === "ENOENT") return true; throw error; });

export function legacyFolderReceipt(installed, environmentId) {
  const copy = structuredClone(installed);
  const mappings = copy.settings?.environments?.filter(row => row.environment_id === environmentId);
  const receipts = copy.provisions?.filter(row => row.environment_id === environmentId);
  if (mappings?.length !== 1 || receipts?.length !== 1 || !receipts[0].success
    || !copy.templates?.some(row => row.id === "desktop-default") || !copy.desktop_binding)
    throw new TypeError("Legacy fixture requires one successfully bound Desktop environment");
  receipts[0].template_id = "desktop-default";
  return copy;
}

export function recoveredFolderReady(projection, projectId, directory) {
  const row = projection?.projects?.find(project => project.id === projectId);
  return row?.can_execute === true && row.preparation_state === "ready" && !row.error
    && sameFolder(row.directory, directory) && projection.can_pause === true;
}

export function folderReselectionVisible(observation, project) {
  return observation?.enabled === true && observation.project_id === project.id
    && observation.project_label === project.label && Boolean(observation.error_text?.trim())
    && observation.button_label === "作業フォルダーを選び直す"
    && observation.row_text?.includes("このPCではこのプロジェクトの新しい仕事を実行できません")
    && !observation.section_text?.includes("os error 109");
}

async function ownedExistingPath(root, candidate) {
  const actualRoot = await realpath(root), actual = await realpath(candidate);
  const relative = path.relative(actualRoot, actual);
  if (!relative || path.isAbsolute(relative) || relative === ".." || relative.startsWith(`..${path.sep}`))
    throw new TypeError("Recovery fixture path escaped its execution root");
  return actual;
}

/** Continue the common GUI onboarding with a migrated, subsequently moved folder. */
export async function exerciseProjectFolderRecovery(args) {
  const { context, host, sink, scenario, state, projectId, environmentId, approvedRoot, projectFolder,
    runnerBinary, runnerTestBinary, chooseFolder, click, settleInput, execution, updateRuntime } = args;
  let cdp = args.cdp, runtime = args.runtime;
  const operationsPath = path.join(context.paths.data, "runner-operations.json");
  const movedRoot = path.join(context.root, "moved-execution");
  const movedProject = path.join(movedRoot, path.basename(projectFolder));
  const markerName = "existing-user-work.txt", markerText = "Existing project work must survive folder recovery. 日本語\n";
  await writeFile(path.join(projectFolder, markerName), markerText, { flag: "wx" });
  const started = state.runner.identity;

  async function restart(beforeRelaunch) {
    await settleInput();
    const result = await host.restart({ context, scenario, sink, driver: cdp, beforeRelaunch: async () => {
      const stopped = await state.runner.quiesce();
      if (!stopped.pass) throw fail("The exact independent Runner did not stop before editing the fixture", stopped);
      if (beforeRelaunch) await beforeRelaunch();
      state.runner = createManagedExecutionRunner({ context, sink, runnerBinary, runnerTestBinary });
    } });
    cdp = result.driver; runtime = result.runtime; updateRuntime(result);
    await cdp.call("Runtime.enable"); await cdp.call("DOM.enable");
    state.input = new WebviewInput(cdp, { probeId: `${ID}-${runtime.generation}` }); await state.input.installProbe();
    if ((await invokeDesktopCommand(cdp, "desktop_state")).overlay !== "hub") await click(action("show-hub", "aside.sidebar"));
    await click(byId("hub-tab-devices"));
    return result;
  }

  const firstRestart = await restart(async () => {
    await ownedExistingPath(context.root, operationsPath);
    const installed = legacyFolderReceipt(JSON.parse(await readFile(operationsPath, "utf8")), environmentId);
    const receipt = installed.provisions.find(row => row.environment_id === environmentId);
    if (!/^[A-Za-z0-9_-]+$/.test(environmentId) || !Number.isSafeInteger(receipt.generation)) throw new TypeError("Invalid fixture identity");
    const database = await ownedExistingPath(context.root, path.join(context.root, "hub-browser", "data", "shared-work.sqlite3"));
    // Seed the matching released Hub representation, not only the local receipt.
    // The enrolled fixture has no jobs and its exact Runner is now stopped.
    const sql = `BEGIN IMMEDIATE; UPDATE shared_provisioning SET template_id='desktop-default'
      WHERE environment_id='${environmentId}' AND template_id='local-folder' AND generation=${receipt.generation};
      SELECT changes(); COMMIT;`;
    const result = await execute("sqlite3.exe", [database, sql], { windowsHide: true, timeout: 10000 });
    if (result.stdout.trim() !== "1") throw fail("The legacy Hub fixture must update exactly the matching provision generation", { output: result.stdout });
    await writeFile(operationsPath, JSON.stringify(installed, null, 2));
    const source = await ownedExistingPath(context.root, approvedRoot);
    if (!await absent(movedRoot) || path.dirname(path.resolve(movedRoot)) !== path.resolve(context.root)) throw new TypeError("Moved fixture destination must be new and inside the execution root");
    await rename(source, movedRoot);
    await sink.record("project-folder-legacy-fixture", { environment_id: environmentId, generation: receipt.generation,
      hub_template: "desktop-default", runner_template: "desktop-default", missing_creation_root: approvedRoot,
      missing_project_folder: projectFolder, moved_project_folder: movedProject }, { phase: "executing", owner: OWNER });
  });
  const unavailable = await wait("Missing legacy folder remains visible without terminating the Runner", execution,
    p => p.projects.some(row => row.id === projectId && row.preparation_state === "failed" && row.error), 60000);
  const running = await state.runner.capture(runtime.desktop_process_id);
  if (running.identity.runner_id === started.runner_id || !await absent(approvedRoot)) throw fail("The recovery startup reused its old process or silently recreated the missing folder");
  const target = { selector: `button[data-action="bind-project-folder"][data-value=${JSON.stringify(projectId)}]`, identity: { tag: "BUTTON", action: "bind-project-folder" } };
  const missingProject = unavailable.projects.find(row => row.id === projectId);
  const recoveryUi = await wait("Desktop identifies the unavailable project and offers folder reselection", () => cdp.evaluate(`(() => {
    const button = document.querySelector(${JSON.stringify(target.selector)});
    const row = button?.closest('.device-project-folder');
    return { enabled: Boolean(button && !button.disabled), project_id: button?.dataset.value,
      project_label: row?.querySelector('strong')?.textContent?.trim() || '',
      button_label: button?.textContent?.trim() || '', error_text: row?.querySelector('[data-error="true"]')?.textContent || '',
      row_text: row?.innerText || '', section_text: document.querySelector('#device-execution')?.innerText || '' };
  })()`), value => folderReselectionVisible(value, missingProject));
  await trustedFocus(state.input, cdp, target);
  await captureScenarioScreenshot({ cdp, sink, name: "project-folder-missing-recoverable", owner: OWNER });
  await chooseFolder(target, movedProject);
  await wait("Choosing an existing folder repairs the same legacy project", execution,
    p => recoveredFolderReady(p, projectId, movedProject), 60000);
  const saved = JSON.parse(await readFile(operationsPath, "utf8"));
  if (!sameFolder(saved.settings.environments.find(row => row.environment_id === environmentId)?.directory, movedProject)
    || !await absent(approvedRoot) || await readFile(path.join(movedProject, markerName), "utf8") !== markerText)
    throw fail("Reselection did not persist the chosen folder or altered existing work");
  await captureScenarioScreenshot({ cdp, sink, name: "project-folder-reselected", owner: OWNER });

  // Creation location is a separate setting and must not move the selected project.
  const newCreationRoot = path.join(context.root, "new-creation-root"); await mkdir(newCreationRoot);
  const setup = { selector: '#device-execution details[data-details-key="device-execution-setup"] > summary', identity: { tag: "DETAILS", detailsKey: "device-execution-setup" } };
  if (!await cdp.evaluate(`document.querySelector('#device-execution details[data-details-key="device-execution-setup"]')?.open`)) await click(setup);
  await chooseFolder(action("device-execution-prepare"), newCreationRoot);
  await wait("Creation-location review contains the chosen folder", execution, p => sameFolder(p.review?.directory, newCreationRoot));
  await click(action("device-execution-enable"));
  await wait("Saving the creation location preserves the project's independently selected folder", execution,
    p => p.review === null && sameFolder(p.directory, newCreationRoot) && recoveredFolderReady(p, projectId, movedProject), 60000);
  const secondRestart = await restart();
  const restored = await wait("Both folder settings survive a full Desktop and Runner restart", execution,
    p => sameFolder(p.directory, newCreationRoot) && recoveredFolderReady(p, projectId, movedProject), 60000);
  const finalRunner = await state.runner.capture(runtime.desktop_process_id);
  if (finalRunner.identity.runner_id === running.identity.runner_id || !await absent(approvedRoot)
    || await readFile(path.join(movedProject, markerName), "utf8") !== markerText) throw fail("Restart did not preserve recovered work without recreating the old folder");
  const finalOperations = (await state.runner.command(["operations", "--runner", finalRunner.identity.runner_id])).projection;
  if (!sameFolder(finalOperations.environments.find(row => row.environment_id === environmentId)?.directory, movedProject))
    throw fail("The restarted Runner did not adopt the GUI-saved project folder");
  await trustedFocus(state.input, cdp, target);
  await captureScenarioScreenshot({ cdp, sink, name: "project-folder-recovery-after-restart", owner: OWNER });
  await sink.record("project-folder-recovery-complete", { project_id: projectId, environment_id: environmentId,
    unavailable, recovery_ui: recoveryUi, restored, missing_folder_not_recreated: true, existing_files_preserved: true,
    legacy_hub_and_runner_receipts: true, saved_project_directory: movedProject, saved_creation_root: newCreationRoot,
    initial_runner: started, recovery_runner: running.identity, final_runner: finalRunner.identity,
    first_restart: firstRestart.restart, final_restart: secondRestart.restart,
    scope: "Actual Tauri/native picker, independent Runner and isolated Hub on this PC. Physical WinB and UNC reachability are not exercised." }, { phase: "executing", owner: OWNER });
  return { acquisition: "pass", oracle: "pass", manual: "pending" };
}
