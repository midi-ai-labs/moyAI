import assert from "node:assert/strict";
import test from "node:test";
import { case52TranscriptExportResult } from "../scenarios/case5_2.mjs";

const sessionId = "01M4N29EW9F8WV6X3A11H6CYET";
const prompt = "取消を検証してください。\n\n- gate 到達後に cancel\n- worker 終了後に比較\n> この行も原文";
const quote = body => `> ${body.trim().replaceAll("\r\n", "\n").replaceAll("\n", "\n> ")}`;
const metadata = id => `<details><summary>実行情報</summary>\n\n- Session: \`${id}\`\n</details>\n`;
const answer = "取消後の状態と保存を確認しました。";
const projection = (user = prompt, assistant = answer) => ({
  run_target: { sessionId },
  transcript_rows: [{ row_kind: "user", body: user }, { row_kind: "assistant", body: assistant }],
});
const markdown = (user = prompt, assistant = answer, details = "") =>
  `# 取消の検証\n\n${quote(user)}\n\n${details}${assistant}\n\n${metadata(sessionId)}`;
const check = (text, terminalProjection = projection(), prompts = { stage4: { text: prompt } }) =>
  case52TranscriptExportResult(text, { sessionId, prompts, terminalProjection });

test("Case5_2 export accepts the renderer's multiline User quote, blank lines and literal quote characters", () => {
  assert.equal(check(markdown()).pass, true);
  assert.deepEqual(check(markdown()).visible_prompt_stages, ["stage4"]);
  assert.equal(check(markdown()).visible_prompt_scope, "terminal-final-user");
});

test("Case5_2 export normalizes CRLF and outer trim without collapsing interior whitespace", () => {
  assert.equal(check(markdown().replaceAll("\n", "\r\n"), projection(`\r\n${prompt}\r\n`)).pass, true);
  assert.equal(check(markdown(prompt.replace("gate 到達後", "gate  到達後"))).pass, false);
  assert.equal(check(markdown(prompt.replace("\n\n", "\n"))).pass, false);
});

test("Case5_2 export requires only Stage4, retaining the existing gate", () => {
  assert.equal(check(markdown(), projection(), { stage1: { text: "以前の調査" }, stage4: { text: prompt } }).pass, true);
});

test("Case5_2 export rejects a missing or changed User body even when the final Assistant quotes the prompt", () => {
  const assistant = `依頼の引用:\n\n${quote(prompt)}`;
  assert.equal(check(markdown("変更された依頼", assistant), projection(prompt, assistant)).pass, false);
  assert.equal(check(`# 取消の検証\n\n${assistant}\n\n${metadata(sessionId)}`, projection(prompt, assistant)).pass, false);
});

test("Case5_2 export rejects Assistant quotes inside previous-message details as User evidence", () => {
  const details = `<details><summary>1 previous messages</summary>\n\n${quote(prompt)}\n\n</details>\n\n`;
  assert.equal(check(markdown("変更された依頼", answer, details)).pass, false);
});

test("Case5_2 export binds the quoted original input to the terminal User and session", () => {
  assert.equal(check(markdown(), projection("別の最終依頼")).pass, false);
  assert.equal(check(markdown(), { ...projection(), run_target: { sessionId: "01M4N29EW9F8WV6X3A11H6CYES" } }).pass, false);
  assert.equal(check(markdown(), { run_target: { sessionId }, transcript_rows: [{ row_kind: "assistant", body: answer }] }).pass, false);
});

test("Case5_2 export requires a unique exact Session in trailing execution metadata", () => {
  assert.equal(check(markdown().replace(metadata(sessionId), metadata(`${sessionId}X`))).pass, false);
  assert.equal(check(markdown().replace(`- Session: \`${sessionId}\``, `- Session: \`${sessionId}\`\n- Session: \`${sessionId}\``)).pass, false);
  assert.equal(check(markdown().replace(metadata(sessionId), `Session ${sessionId}\n`)).pass, false);
});

test("Case5_2 export rejects a changed final Assistant boundary instead of treating its quote as User input", () => {
  assert.equal(check(markdown(prompt, `${answer} 改変`)).pass, false);
});

test("Case5_2 export rejects a one-line prompt present only in the Assistant or an unrelated Session mention", () => {
  const oneLine = "cancel 後の保存を確認";
  const assistant = `> ${oneLine}\n\nSession ${sessionId}`;
  const inputs = { stage4: { text: oneLine } };
  assert.equal(check(markdown("別の依頼", assistant), projection(oneLine, assistant), inputs).pass, false);
  assert.equal(check(markdown(oneLine, assistant).replace(metadata(sessionId), metadata("other-session")), projection(oneLine, assistant), inputs).pass, false);
});

test("Case5_2 export handles nested renderer details and an earlier turn before its final User", () => {
  const details = "<details><summary>2 previous messages</summary>\n\n> 作業中です。\n\n<details><summary>tool</summary>\n\n結果\n\n</details>\n\n</details>\n\n";
  const text = markdown(prompt, answer, details).replace("# 取消の検証\n\n", "# 取消の検証\n\n> 最初の依頼\n\n最初の応答\n\n");
  assert.equal(check(text).pass, true);
});
