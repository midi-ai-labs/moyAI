import assert from "node:assert/strict";
import test from "node:test";
import { createScenario } from "../scenario_registry.mjs";
import { cancelledActivationAccepted, joinedActivationAccepted, movedActivationAccepted, directRouteIdentity, publicCertificatePem } from "../scenarios/hub_join_config.mjs";

test("public Hub TOML export accepts escaped Windows CRLF as well as LF PEM", () => {
  const pem = "-----BEGIN CERTIFICATE-----\ncertificate-body\n-----END CERTIFICATE-----";
  for (const encoded of [pem, pem.replaceAll("\n", "\\n"), pem.replaceAll("\n", "\\r\\n")]) {
    assert.equal(publicCertificatePem(`ca_certificate_pem = "${encoded}"`), pem);
  }
});

const desktop = {
  hub: { main_mode: "direct", side_chat_mode: "direct" },
  provider_effective_base_url: "http://127.0.0.1:9", provider_effective_model_id: "existing-model",
  provider_effective_profile: "openai_compatible", access_mode: "default", draft_target: { workspace: "fixture", session: null },
};

test("activation scenarios share Hub options and do not add warm argv to ordinary startup", () => {
  for (const mode of ["cold", "warm"]) {
    const scenario = createScenario(`hub.join-config-${mode}`, { hubBinary: "C:/fixture/moyai-hub.exe", headed: true });
    assert.equal(scenario.id, `hub.join-config-${mode}`);
    assert.equal(scenario.joinConfigPath, null);
    for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) assert.equal(typeof scenario[method], "function");
  }
});

test("cancel oracle rejects persisted mutation, admission, lost draft or changed Direct settings", () => {
  const value = {
    before: { desktop }, after: { desktop: structuredClone(desktop), prompt: "Keep this unsent draft when cancelling team participation. 日本語",
      network: { enrollment: "unconfigured", device_id: null, request_id: null } },
    configBefore: "original config", configAfter: "original config", prefsBefore: "original prefs", prefsAfter: "original prefs",
  };
  assert.equal(cancelledActivationAccepted(value), true);
  for (const mutate of [
    x => { x.configAfter = "changed"; }, x => { x.prefsAfter = "changed"; }, x => { x.after.prompt = ""; },
    x => { x.after.network.request_id = "unexpected-join"; }, x => { x.after.desktop.hub.main_mode = "hub"; },
    x => { x.after.desktop.draft_target.session = "other"; },
  ]) { const changed = structuredClone(value); mutate(changed); assert.equal(cancelledActivationAccepted(changed), false); }
});

test("OK requires actual device registration and a visible unauthenticated shared entry, keeping both Direct routes", () => {
  const url = "https://127.0.0.1:9471", expected = directRouteIdentity(desktop);
  const value = { desktop: { ...desktop, startup: { onboarding_intent: "team" } },
    network: { enrollment: "active", hub_url: url, device_id: "device-1" },
    shared: { connected: true, principal: null, projects: [] }, loginVisible: true };
  assert.equal(joinedActivationAccepted(value, expected, url), true);
  for (const mutate of [
    x => { x.network.enrollment = "pending"; }, x => { x.network.hub_url = "https://other:9471"; },
    x => { x.shared.principal = { user_id: "unexpected-auto-login" }; }, x => { x.shared.projects = [{}]; },
    x => { x.loginVisible = false; }, x => { x.desktop.hub.side_chat_mode = "hub"; },
    x => { x.desktop.provider_effective_model_id = "overwritten"; },
  ]) { const changed = structuredClone(value); mutate(changed); assert.equal(joinedActivationAccepted(changed, expected, url), false); }
});

test("endpoint migration preserves the same ordinary person and PC, visible account and Direct routes without a new login", () => {
  const expected = { url: "https://127.0.0.1:9555", device_id: "same-pc", user_id: "alice", display_name: "Endpoint Alice", direct: directRouteIdentity(desktop) };
  const value = { desktop, network: { enrollment: "active", hub_url: expected.url, device_id: expected.device_id },
    shared: { connected: true, principal: { user_id: "alice", administrator: false } },
    surface: { count: 1, login_visible: false, account_text: "Endpoint Alice · Hub projects" },
    calls: [{ args: { request: { kind: "refresh" } } }] };
  assert.equal(movedActivationAccepted(value, expected), true);
  for (const mutate of [
    x => { x.network.hub_url = "https://old.example:9471"; }, x => { x.network.device_id = "new-pc"; },
    x => { x.network.enrollment = "pending"; }, x => { x.shared.principal.user_id = "other"; },
    x => { x.shared.principal.administrator = true; }, x => { x.surface.login_visible = true; },
    x => { x.surface.account_text = "old person"; }, x => { x.surface.count = 0; },
    x => { x.calls.push({ args: { request: { kind: "login" } } }); },
    x => { x.calls.push({ args: { request: { kind: "setup_password" } } }); },
    x => { x.desktop.hub.main_mode = "hub"; },
  ]) { const changed = structuredClone(value); mutate(changed); assert.equal(movedActivationAccepted(changed, expected), false); }
});
