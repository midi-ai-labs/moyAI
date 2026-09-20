import assert from "node:assert/strict";
import test from "node:test";
import { renderSharedWork } from "../src/shared_work_render.ts";
import { sharedWorkPresentation } from "../src/shared_work_state.ts";
import { sharedUiFixture } from "./shared_work_fixture.ts";

const region = (html: string, name: string) => html.match(new RegExp(`<section data-shared-region="${name}"[\\s\\S]*?</section>`))?.[0] ?? "";

test("stale Runner contact remains distinct from job state in environment list and detail", () => {
  const local = sharedUiFixture();
  const contact = { last_contact_ms: 1_789_200_000_000, state: "stale" };
  const p = local.projection!;
  p.observed_at_ms = 1_789_200_100_000;
  Object.assign(p.status!.environments[0], { occupied: 0, other_occupants: 0, runner_contact: contact });
  Object.assign(p.status!.jobs[0], { state: "running", runner_contact: contact });
  p.detail = { id: "job-a", project_id: "project-a", root_id: "job-a", parent_id: null,
    environment_id: "env-a", title: "試験の仕事", input: {}, result: null, state: "running",
    awaiting_child_id: null, revision: 2, created_at_ms: 1, updated_at_ms: 2 };
  Object.assign(p.detail, { runner_contact: contact, wait_reason: "停止の確認を待っています。", uncertainty_reason: "外部処理を照合してください。" });
  const html = renderSharedWork(sharedWorkPresentation(local));
  for (const name of ["environments", "detail"]) {
    assert.match(region(html, name), /PCの応答なし（状態不明）/);
    assert.ok(region(html, name).includes(new Date(contact.last_contact_ms).toLocaleString("ja-JP")));
  }
  assert.doesNotMatch(region(html, "environments"), /空きがあります/);
  assert.match(region(html, "detail"), /実行中/);
  assert.match(region(html, "detail"), /停止の確認を待っています。/);
  assert.match(region(html, "detail"), /外部処理を照合してください。/);
});

test("recent contact does not claim new admission and unconfirmed environments do not claim availability", () => {
  const local = sharedUiFixture();
  const env = local.projection!.status!.environments[0];
  Object.assign(env, { occupied: 0, other_occupants: 0, enabled: false });
  for (const runner_contact of [undefined, { state: "unconfirmed", last_contact_ms: null }]) {
    Object.assign(env, { runner_contact });
    const html = region(renderSharedWork(sharedWorkPresentation(local)), "environments");
    assert.match(html, /PCの応答なし（状態不明）/);
    assert.match(html, /最終応答: 未確認/);
    assert.match(html, /受付停止/);
    assert.doesNotMatch(html, /空きがあります|受付中/);
  }
  // Hub owns recency, so even an old numeric timestamp must not be reclassified by the UI clock.
  Object.assign(env, { runner_contact: { state: "recent", last_contact_ms: 1 } });
  const html = region(renderSharedWork(sharedWorkPresentation(local)), "environments");
  assert.match(html, /PCからの応答あり/);
  assert.match(html, /受付停止/);
  assert.doesNotMatch(html, /状態不明|空きがあります|受付中/);
});

test("lost contact preserves occupied work and terminal answers and contact recovery clears only the communication warning", () => {
  const local = sharedUiFixture();
  const p = local.projection!;
  const stale = { state: "stale", last_contact_ms: 1 };
  Object.assign(p.status!.environments[0], { occupied: 1, runner_contact: stale });
  Object.assign(p.status!.jobs[0], { state: "succeeded", runner_contact: stale });
  p.detail = { id: "job-a", project_id: "project-a", root_id: "job-a", parent_id: null,
    environment_id: "env-a", title: "試験の仕事", input: {}, result: { text: "結果を保存しました。" }, state: "succeeded",
    awaiting_child_id: null, revision: 2, created_at_ms: 1, updated_at_ms: 2 };
  Object.assign(p.detail, { runner_contact: stale, uncertainty_reason: "照合先 <script> を確認" });
  let html = renderSharedWork(sharedWorkPresentation(local));
  assert.match(region(html, "environments"), /1 \/ 2 枠を使用中/);
  assert.match(region(html, "detail"), /完了/);
  assert.match(region(html, "detail"), /結果を保存しました。/);
  assert.match(region(html, "detail"), /照合先 &lt;script&gt;/);
  assert.doesNotMatch(html, /<script>/);
  const recent = { state: "recent", last_contact_ms: 2 };
  for (const row of [p.status!.environments[0], p.status!.jobs[0], p.detail]) Object.assign(row, { runner_contact: recent });
  html = renderSharedWork(sharedWorkPresentation(local));
  assert.doesNotMatch(html, /PCの応答なし（状態不明）/);
  assert.match(region(html, "environments"), /1 \/ 2 枠を使用中/);
  assert.match(region(html, "detail"), /結果を保存しました。/);
  assert.match(region(html, "detail"), /照合先 &lt;script&gt;/);
});
