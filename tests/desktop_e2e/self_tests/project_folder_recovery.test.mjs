import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { createScenario } from "../scenario_registry.mjs";
import { legacyFolderReceipt, recoveredFolderReady, folderReselectionVisible } from "../scenarios/project_folder_recovery.mjs";

test("folder recovery uses the common actual Desktop, native picker and Runner lifecycle", () => {
  const scenario = createScenario("settings.project-folder-recovery", {
    runnerBinary: path.resolve("runner.exe"), runnerTestBinary: path.resolve("runner-test.exe"),
  });
  assert.equal(scenario.id, "settings.project-folder-recovery");
  assert.equal(scenario.manualGate, "pending");
  assert.equal(scenario.databaseRequired, true);
  for (const method of ["prepare", "execute", "quiesce", "cleanup", "requestGracefulExit"]) assert.equal(typeof scenario[method], "function");
});

test("legacy fixture retains exact saved authority and only changes the selected receipt's released template kind", () => {
  const source = { settings: { environments: [{ environment_id: "chosen", directory: path.resolve("old-project"), access_mode: "default" }] },
    provisions: [{ environment_id: "chosen", template_id: "local-folder", generation: 7, success: true, error: null },
      { environment_id: "unrelated", template_id: "local-folder", generation: 3, success: true }],
    templates: [{ id: "desktop-default", base_root: path.resolve("old-root") }], desktop_binding: "existing-consent" };
  const original = structuredClone(source), seeded = legacyFolderReceipt(source, "chosen");
  assert.deepEqual(source, original);
  assert.deepEqual(seeded.settings, source.settings);
  assert.deepEqual(seeded.templates, source.templates);
  assert.deepEqual(seeded.provisions[1], source.provisions[1]);
  assert.deepEqual(seeded.provisions[0], { ...source.provisions[0], template_id: "desktop-default" });
  assert.throws(() => legacyFolderReceipt(source, "unrelated"), /one successfully bound/);
  assert.throws(() => legacyFolderReceipt({ ...source, provisions: [...source.provisions, source.provisions[0]] }, "chosen"), /one successfully bound/);
  assert.throws(() => legacyFolderReceipt({ ...source, desktop_binding: null }, "chosen"), /one successfully bound/);
});

test("recovery oracle requires the requested project, persisted new directory and usable execution state", () => {
  const folder = path.resolve("reselected-project");
  const project = { id: "p", can_execute: true, preparation_state: "ready", error: null, directory: path.toNamespacedPath(folder) };
  const projection = { can_pause: true, projects: [project] };
  assert.equal(recoveredFolderReady(projection, "p", folder), true);
  assert.equal(recoveredFolderReady(projection, "other", folder), false);
  for (const changed of [{ directory: path.resolve("old-project") }, { preparation_state: "failed" }, { error: "missing folder" }, { can_execute: false }])
    assert.equal(recoveredFolderReady({ ...projection, projects: [{ ...project, ...changed }] }, "p", folder), false);
  assert.equal(recoveredFolderReady({ ...projection, can_pause: false }, "p", folder), false);
});

test("missing-folder UI is identified by its project and recovery controls without relying on a pruned path", () => {
  const project = { id: "project-a", label: "既存アプリ", directory: null };
  const visible = { enabled: true, project_id: project.id, project_label: project.label,
    button_label: "作業フォルダーを選び直す", error_text: "作業フォルダーを利用できません。",
    row_text: "選ぶまで、このPCではこのプロジェクトの新しい仕事を実行できません。", section_text: "このPCで仕事を実行" };
  assert.equal(folderReselectionVisible(visible, project), true);
  for (const changed of [{ enabled: false }, { project_id: "other" }, { project_label: "別プロジェクト" },
    { error_text: "" }, { button_label: "作業フォルダーを選ぶ" }, { row_text: "準備完了" }, { section_text: "パイプは終了しました (os error 109)" }])
    assert.equal(folderReselectionVisible({ ...visible, ...changed }, project), false);
});
