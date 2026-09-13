import assert from "node:assert/strict";
import test from "node:test";

import { duplicateLaunchAccepted, restoredSingleInstanceAccepted } from "../scenarios/shell_single_instance.mjs";

const owner = { process_id: 4100, process_start_time_utc_ticks: "638914752000000000", executable_path: "C:\\moyai\\moyai-desktop.exe" };
const candidate = { hwnd: "0x100", root_hwnd: "0x100", process_id: owner.process_id, thread_id: 812,
  class_name: "Tauri Window", visible: true, minimized: false, enabled: true, is_root: true };

test("duplicate launch must exit clearly while exactly the original Desktop remains", () => {
  const good = { process: { outcome: { root_exit_code: 0, timed_out: false }, job: { descendant_zero: true } },
    stdout: "moyAI Desktop is already running; showing the existing window.\n", desktops: [owner] };
  assert.equal(duplicateLaunchAccepted(good, owner), true);
  for (const mutate of [
    v => { v.stdout = ""; }, v => { v.process.outcome.root_exit_code = 1; },
    v => { v.process.outcome.timed_out = true; }, v => { v.process.job.descendant_zero = false; },
    v => { v.desktops.push(owner); }, v => { v.desktops[0].process_start_time_utc_ticks = "123"; },
    v => { v.desktops[0].executable_path = "C:\\other\\moyai-desktop.exe"; },
  ]) {
    const invalid = structuredClone(good); mutate(invalid);
    assert.equal(duplicateLaunchAccepted(invalid, owner), false);
  }
});

test("restoration preserves the selected session and draft, and requires visible notice and same restored HWND", () => {
  const projection = { selected_project_index: 0, selected_session_index: 0, project_rows: [{ project_id: "project", path: "C:\\work" }],
    session_rows: [{ session_id: "root" }], workspace_path: "C:\\work", draft_target: { sessionId: "root" },
    transcript_rows: [], busy: false, status_message: "moyAI は既に起動しています。既存のウィンドウを表示しました。" };
  const before = { prompt: "unsent 日本語", projection };
  const good = { before, candidate, window: { live: true, exact_identity: true, window: candidate },
    surface: { prompt: before.prompt, projection, notice: projection.status_message, notice_visible: true, errors: [] } };
  assert.equal(restoredSingleInstanceAccepted(good), true);
  for (const mutate of [
    v => { v.window.window = { ...candidate, hwnd: "0x101" }; },
    v => { v.window.window = { ...candidate, visible: false }; },
    v => { v.window.window = { ...candidate, minimized: true }; },
    v => { v.surface.notice = ""; }, v => { v.surface.notice_visible = false; },
    v => { v.surface.prompt = ""; },
    v => { v.surface.projection = { ...projection, draft_target: { sessionId: "other" } }; },
    v => { v.surface.projection = { ...projection, session_rows: [{ session_id: "other" }] }; },
  ]) {
    const invalid = structuredClone(good); mutate(invalid);
    assert.equal(restoredSingleInstanceAccepted(invalid), false);
  }
});
