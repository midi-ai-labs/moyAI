import assert from "node:assert/strict";
import test from "node:test";
import { DESKTOP_COMMAND_OBSERVER_SYMBOL } from "../src/api.ts";
import { sharedWorkAction } from "../src/shared_work_actions.ts";
import { sharedWorkActionEnabled, acceptSharedWork, selectSharedLoginMode } from "../src/shared_work_state.ts";
import { renderPasswordSetup } from "../src/shared_work_password_setup.ts";
import { renderHubConversation } from "../src/shared_work_conversation.ts";
import { sharedProjection, sharedUiFixture } from "./shared_work_fixture.ts";
import type { ActionContext } from "../src/actions.ts";

function setupFixture() {
  const local = sharedUiFixture();
  local.projection = sharedProjection({ principal: null, projects: [], selected_project_id: null, status: null });
  local.username = "alice";
  local.password = "new-private-password";
  local.setupCode = "b".repeat(64);
  local.setupPasswordConfirm = local.password;
  return local;
}

test("first use shows code fields before the checklist and password login is an explicit secret-clearing switch", () => {
  const local = setupFixture();
  const html = renderHubConversation(local, "");
  assert.ok(html.indexOf('id="shared-setup-code"') < html.indexOf('data-shared-region="onboarding"'));
  const login = html.slice(html.indexOf('data-shared-region="login"'), html.indexOf('data-shared-region="onboarding"'));
  assert.doesNotMatch(login, /<details/);
  assert.match(login, /data-action="shared-setup-password"/);
  assert.doesNotMatch(login, /data-action="shared-login"/);
  assert.equal(selectSharedLoginMode(local, "password"), true);
  assert.equal(local.username, "alice");
  assert.equal(local.password + local.setupCode + local.setupPasswordConfirm, "");
  const password = renderHubConversation(local, "");
  assert.match(password, /data-action="shared-login"/);
  assert.doesNotMatch(password, /id="shared-setup-code"/);
  local.pending = "login";
  assert.equal(selectSharedLoginMode(local, "setup"), false);
  assert.equal(local.loginMode, "password");
  local.pending = null;
  local.projection = sharedProjection();
  assert.equal(selectSharedLoginMode(local, "setup"), false);
});

test("first password setup requires the connected current person, exact code and matching password", () => {
  const local = setupFixture();
  assert.equal(sharedWorkActionEnabled(local, "setup-password", ""), true);
  assert.match(renderPasswordSetup(local), /type="password" autocomplete="off"/);
  local.setupPasswordConfirm = "different";
  assert.equal(sharedWorkActionEnabled(local, "setup-password", ""), false);
  local.setupPasswordConfirm = local.password;
  local.setupCode = "not-a-code";
  assert.equal(sharedWorkActionEnabled(local, "setup-password", ""), false);
  local.setupCode = "b".repeat(64);
  local.projection!.connected = false;
  assert.equal(sharedWorkActionEnabled(local, "setup-password", ""), false);
  local.projection = sharedProjection();
  assert.equal(sharedWorkActionEnabled(local, "setup-password", ""), false);
});

test("password setup clears transient secrets before dispatch and redacts diagnostic observers", async () => {
  const local = setupFixture(), sent: Record<string, unknown>[] = [], observed: unknown[] = [];
  const inputs = new Map(["shared-password", "shared-setup-code", "shared-setup-confirm"].map(id => [id, { value: "private" }]));
  const originals = new Map<PropertyKey, PropertyDescriptor | undefined>();
  const observerKey = Symbol.for(DESKTOP_COMMAND_OBSERVER_SYMBOL);
  for (const key of ["window", "document", observerKey]) originals.set(key, Object.getOwnPropertyDescriptor(globalThis, key));
  Object.defineProperty(globalThis, observerKey, { configurable: true, value: (value: unknown) => observed.push(value) });
  Object.defineProperty(globalThis, "document", { configurable: true, value: { querySelector: (selector: string) => inputs.get(selector.slice(1)) ?? null } });
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (_name: string, args: Record<string, unknown>) => {
    sent.push(args);
    assert.equal(local.password, ""); assert.equal(local.setupCode, ""); assert.equal(local.setupPasswordConfirm, "");
    assert.ok([...inputs.values()].every(input => input.value === ""));
    return sharedProjection();
  } } } });
  try {
    const context = { uiState: { sharedWork: local }, getViewState: () => ({ hub_project_open: true, overlay: "none" }), rerender() {} } as unknown as ActionContext;
    const first = sharedWorkAction(context, "setup_password");
    await sharedWorkAction(context, "setup_password");
    await first;
    const setup = sent.filter(args => (args?.request as Record<string, unknown>)?.kind === "setup_password");
    assert.equal(setup.length, 1);
    assert.deepEqual(setup[0], { expectedGeneration: "1", request: { kind: "setup_password", username: "alice", password: "new-private-password", code: "b".repeat(64) } });
    assert.ok(local.projection?.principal);
    const diagnostic = JSON.stringify(observed);
    assert.ok(!diagnostic.includes("new-private-password"));
    assert.ok(!diagnostic.includes("b".repeat(64)));
    assert.match(diagnostic, /\[redacted\]/);
  } finally {
    for (const [key, descriptor] of originals) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else Reflect.deleteProperty(globalThis, key);
    }
  }
});

test("lost setup reply leaves secrets cleared and directs the user to ordinary login", async () => {
  const local = setupFixture();
  const windowBefore = Object.getOwnPropertyDescriptor(globalThis, "window");
  const documentBefore = Object.getOwnPropertyDescriptor(globalThis, "document");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async () => { throw new Error("reply lost"); } } } });
  Object.defineProperty(globalThis, "document", { configurable: true, value: { querySelector: () => null } });
  try {
    const context = { uiState: { sharedWork: local }, getViewState: () => ({ hub_project_open: true, overlay: "none" }), rerender() {} } as unknown as ActionContext;
    await sharedWorkAction(context, "setup_password");
    assert.equal(local.pending, null); assert.equal(local.password, ""); assert.equal(local.setupCode, ""); assert.equal(local.setupPasswordConfirm, "");
    assert.match(local.error, /通常ログイン/);
  } finally {
    if (windowBefore) Object.defineProperty(globalThis, "window", windowBefore); else Reflect.deleteProperty(globalThis, "window");
    if (documentBefore) Object.defineProperty(globalThis, "document", documentBefore); else Reflect.deleteProperty(globalThis, "document");
  }
});

test("changing the authenticated owner discards unfinished setup secrets", () => {
  const local = setupFixture();
  acceptSharedWork(local, sharedProjection({ generation: "2" }));
  assert.equal(local.setupCode, ""); assert.equal(local.password, ""); assert.equal(local.setupPasswordConfirm, "");
});

test("empty projects distinguish missing human membership from this PC's access", () => {
  const local = sharedUiFixture();
  local.projection = sharedProjection({ projects: [], selected_project_id: null, status: null, project_access: "no_membership" });
  assert.match(renderHubConversation(local, ""), /あなたをプロジェクトの参加者として登録/);
  assert.doesNotMatch(renderHubConversation(local, ""), /あなたの参加登録は済んでいます/);
  local.projection.project_access = "device_not_allowed";
  assert.match(renderHubConversation(local, ""), /あなたの参加登録は済んでいます/);
  assert.match(renderHubConversation(local, ""), /このPCをプロジェクトの操作PCへ追加/);
  local.projection.project_access = undefined;
  assert.doesNotMatch(renderHubConversation(local, ""), /あなたの参加登録は済んでいます/);
});
