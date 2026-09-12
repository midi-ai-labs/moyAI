import http from "node:http";
import { isDeepStrictEqual } from "node:util";
import path from "node:path";
import { randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";

import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { scriptedProviderPortIsFetchSafe } from "../drivers/scripted_provider.mjs";
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from "../drivers/webview_input.mjs";
import { acquireInteractiveShell, prepareShellBaseline, requestGracefulExit } from "./shell_baseline.mjs";
import { quiesceProviderResource } from "./provider_restart.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";

const OWNER = "scenario:hub.connection-settings";
const DIALOG = '[role="dialog"][data-modal="hub"]';
const NODE_KEY = "moyai.desktop_e2e.hub.draft-node.v1";
export const HUB_FIXTURE_ID = "e2e-hub-control-plane";
export const HUB_FIXTURE_MODELS = Object.freeze([
  Object.freeze({ id: "e2e-main", label: "Main fixture model", capabilities: ["tools"] }),
  Object.freeze({ id: "e2e-side", label: "Side fixture model", capabilities: ["tools"] }),
]);
const exactKeys = (value, keys) => value !== null && typeof value === "object" && !Array.isArray(value)
  && JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...keys].sort());
const same = isDeepStrictEqual;
const selectionKeys = ["allowed_model_ids", "preferred_model_id", "required_capabilities", "wait_policy", "affinity_turns"];

function validSelection(value) {
  return exactKeys(value, selectionKeys)
    && Array.isArray(value.allowed_model_ids) && value.allowed_model_ids.length > 0
    && value.allowed_model_ids.length <= 128 && new Set(value.allowed_model_ids).size === value.allowed_model_ids.length
    && value.allowed_model_ids.includes(value.preferred_model_id)
    && Array.isArray(value.required_capabilities) && value.required_capabilities.length <= 32
    && new Set(value.required_capabilities).size === value.required_capabilities.length
    && value.required_capabilities.every((item) => typeof item === "string" && /^[A-Za-z0-9_.-]{1,64}$/.test(item))
    && ["wait_for_preferred", "allow_selected_fallback"].includes(value.wait_policy)
    && Number.isInteger(value.affinity_turns) && value.affinity_turns >= 1 && value.affinity_turns <= 100;
}

/** Deterministic control-plane wire only. It never accepts an inference request or logs bearer values. */
export class HubSettingsFixture {
  #server;
  #sockets = new Set();
  #ledger = [];
  #clients = new Map();
  #active = 0;
  #revision = "7";
  #models = structuredClone(HUB_FIXTURE_MODELS);
  #address = null;
  #closePromise = null;
  #closed = false;
  #clientSequence = 0;
  #bootstrap = `e2e-hub-bootstrap-${randomBytes(16).toString("hex")}`;
  #issuedTokens = [];

  constructor() {
    this.#server = http.createServer((request, response) => {
      this.#active += 1;
      void this.#handle(request, response).catch(() => {
        if (!response.headersSent) this.#reply(response, 500, { error: "fixture_internal_error" });
        else response.destroy();
      }).finally(() => { this.#active -= 1; });
    });
    this.#server.maxHeadersCount = 64;
    this.#server.requestTimeout = 5000;
    this.#server.headersTimeout = 5000;
    this.#server.keepAliveTimeout = 1;
    this.#server.on("connection", (socket) => {
      this.#sockets.add(socket);
      socket.once("close", () => this.#sockets.delete(socket));
    });
  }
  get bootstrapToken() { return this.#bootstrap; }
  get baseUrl() { if (!this.#address) throw new Error("Hub fixture is not listening"); return `http://127.0.0.1:${this.#address.port}`; }
  get requestLedger() { return structuredClone(this.#ledger); }
  get revision() { return this.#revision; }
  get clientCount() { return this.#clients.size; }
  containsCredential(text) { return [this.#bootstrap, ...this.#issuedTokens].some((secret) => String(text).includes(secret)); }
  get catalogModels() { return structuredClone(this.#models); }
  advanceRevision({ changeModelLabels = false } = {}) {
    this.#revision = (BigInt(this.#revision) + 1n).toString();
    if(changeModelLabels) this.#models=this.#models.map(model=>({...model,label:model.label+' revised'}));
  }
  reviewFor(context) { return structuredClone([...this.#clients.values()][0]?.reviews[context] ?? null); }

  async start() {
    if (this.#address || this.#closed) throw new Error("Hub fixture can only start once");
    // Reuse the canonical Fetch port policy; this fixture owns only its HTTP listener, never an app launcher.
    for (let attempt = 0; attempt < 16; attempt += 1) {
      await new Promise((resolve, reject) => {
        const failed = (error) => { this.#server.off("listening", ready); reject(error); };
        const ready = () => { this.#server.off("error", failed); resolve(); };
        this.#server.once("error", failed);
        this.#server.once("listening", ready);
        this.#server.listen({ host: "127.0.0.1", port: 0, exclusive: true });
      });
      const address = this.#server.address();
      if (address && typeof address !== "string" && address.address === "127.0.0.1" && scriptedProviderPortIsFetchSafe(address.port)) {
        this.#address = { host: address.address, port: address.port, family: address.family };
        return this;
      }
      await new Promise((resolve, reject) => this.#server.close((error) => error ? reject(error) : resolve()));
    }
    throw new Error("Hub fixture could not acquire a Fetch-safe loopback listener");
  }
  resourceObservation() {
    return { kind: "hub-settings-http", address: this.#address, listening: this.#server.listening,
      closed: this.#closed, active_request_count: this.#active, open_connection_count: this.#sockets.size,
      request_count: this.#ledger.length, registered_client_count: this.#clients.size };
  }
  async close() {
    this.#closePromise ??= (async () => {
      const before = this.resourceObservation();
      let forced = 0;
      await new Promise((resolve, reject) => {
        const deadline = setTimeout(() => { forced = this.#sockets.size; for (const socket of this.#sockets) socket.destroy(); }, 2000);
        this.#server.close((error) => { clearTimeout(deadline); if (error) reject(error); else resolve(); });
        this.#server.closeIdleConnections();
      });
      await waitForObservation({ label: "Hub fixture socket drain", timeoutMs: 2000, pollMs: 10,
        sample: () => this.#sockets.size, accept: (count) => count === 0 });
      this.#closed = true;
      const after = this.resourceObservation();
      return { before, after, forced_connection_count: forced,
        pass: before.listening && !after.listening && after.closed && after.active_request_count === 0 && after.open_connection_count === 0 };
    })();
    return structuredClone(await this.#closePromise);
  }
  #reply(response, status, value) {
    response.writeHead(status, { "content-type": "application/json", connection: "close" });
    response.end(JSON.stringify(value));
  }
  async #handle(request, response) {
    const target = new URL(request.url, "http://127.0.0.1");
    const header = request.headers.authorization;
    const client = [...this.#clients.values()].find((value) => header === `Bearer ${value.token}`);
    const bootstrap = header === `Bearer ${this.#bootstrap}`;
    const row = { sequence: this.#ledger.length + 1, method: request.method, pathname: target.pathname,
      authorization: client ? "client" : bootstrap ? "bootstrap" : "invalid", status: null, context: null, revision: null, selection: null };
    this.#ledger.push(row);
    const reply = (status, value) => { row.status = status; this.#reply(response, status, value); };
    if (this.#ledger.length > 256) return reply(429, { error: "fixture_request_bound" });
    if (request.headers.origin || target.search) return reply(400, { error: "invalid_request" });
    if (!client && !bootstrap) return reply(401, { error: "unauthorized" });
    if (request.method === "GET" && target.pathname === "/v1/catalog") {
      row.revision = this.#revision;
      return reply(200, { hub_id: HUB_FIXTURE_ID, software_version: "0.1.0", revision: this.#revision,
        models: this.#models, changes: [{ revision: this.#revision, at_ms: 1000, summary: "Fixture catalog revision" }] });
    }
    if (request.method !== "POST") return reply(404, { error: "not_found" });
    const parts = [];
    let bytes = 0;
    for await (const part of request) {
      bytes += part.byteLength;
      if (bytes > 16 * 1024) return reply(413, { error: "body_too_large" });
      parts.push(part);
    }
    let body;
    try { body = JSON.parse(Buffer.concat(parts).toString("utf8")); } catch { return reply(400, { error: "invalid_request" }); }
    if (target.pathname === "/v1/clients/register") {
      if (!bootstrap) return reply(401, { error: "unauthorized" });
      if (!exactKeys(body, ["label"]) || typeof body.label !== "string" || !body.label.trim()
        || Buffer.byteLength(body.label) > 256 || /[\u0000-\u001f\u007f]/.test(body.label)) return reply(400, { error: "invalid_request" });
      const id = `e2e-client-${++this.#clientSequence}`;
      const token = `e2e-client-token-${randomBytes(16).toString("hex")}`;
      this.#issuedTokens.push(token);
      this.#clients.set(id, { id, token, reviews: {} });
      return reply(200, { id, client_token: token, hub_id: HUB_FIXTURE_ID, revision: this.#revision,
        heartbeat_interval_ms: 1000, identity_scope: "server_session" });
    }
    if (!client || body.id !== client.id) return reply(401, { error: "unauthorized" });
    if (target.pathname === "/v1/clients/heartbeat") {
      if (!exactKeys(body, ["id", "activity"]) || !["idle", "waiting", "running"].includes(body.activity)) return reply(400, { error: "invalid_request" });
      row.revision = this.#revision;
      return reply(200, { id: client.id, revision: this.#revision });
    }
    if (target.pathname === "/v1/clients/disconnect") {
      if (!exactKeys(body, ["id"])) return reply(400, { error: "invalid_request" });
      this.#clients.delete(client.id);
      return reply(200, { id: client.id, disconnected: true });
    }
    if (target.pathname !== "/v1/clients/review") return reply(404, { error: "not_found" });
    if (!exactKeys(body, ["id", "context", "expected_hub_id", "reviewed_revision", "selection"])
      || !["main", "side_chat"].includes(body.context) || typeof body.reviewed_revision !== "string"
      || !/^[1-9][0-9]*$/.test(body.reviewed_revision)) return reply(400, { error: "invalid_request" });
    if (body.expected_hub_id !== HUB_FIXTURE_ID) return reply(409, { error: "different_hub", current_revision: this.#revision });
    if (body.reviewed_revision !== this.#revision) return reply(409, { error: "review_required", current_revision: this.#revision });
    if (!validSelection(body.selection)) return reply(400, { error: "invalid_selection" });
    if (body.selection.allowed_model_ids.some((id) => !this.#models.some((model) => model.id === id))) return reply(400, { error: "model_removed" });
    const compatible = this.#models.filter((model) => body.selection.allowed_model_ids.includes(model.id)
      && body.selection.required_capabilities.every((capability) => model.capabilities.includes(capability)));
    if (!compatible.length || (body.selection.wait_policy === "wait_for_preferred"
      && !compatible.some((model) => model.id === body.selection.preferred_model_id))) return reply(400, { error: "capability_mismatch" });
    row.context = body.context;
    row.revision = body.reviewed_revision;
    row.selection = structuredClone(body.selection);
    client.reviews[body.context] = { reviewed_revision: body.reviewed_revision, selection: structuredClone(body.selection) };
    return reply(200, { id: client.id, context: body.context, reviewed_revision: body.reviewed_revision });
  }
}

const inputTarget = (id) => ({ selector: `#${id}`, identity: { tag: "INPUT", id } });
const actionTarget = (name, scope = DIALOG) => ({ selector: `${scope} button[data-action="${name}"]`, identity: { tag: "BUTTON", action: name } });
function productFailure(label, evidence) { return new DesktopE2eError("product", "hub-settings-mismatch", label, evidence); }

async function observeSurface(cdp) {
  return cdp.evaluate(`(async () => {
    const hub = await window.__TAURI_INTERNALS__.invoke('hub_projection');
    const projection = await window.__TAURI_INTERNALS__.invoke('desktop_state');
    const one = (selector) => { const nodes = document.querySelectorAll(selector); return nodes.length === 1 ? nodes[0] : null; };
    const measure = (node) => {
      if (!(node instanceof HTMLElement)) return null;
      const rect = node.getBoundingClientRect(), style = getComputedStyle(node);
      const hit = document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2);
      return { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom, width: rect.width, height: rect.height,
        visible: node.isConnected && !node.hidden && style.display !== 'none' && style.visibility !== 'hidden'
          && Number(style.opacity) !== 0 && rect.width > 0 && rect.height > 0,
        center_hit: hit !== null && (hit === node || node.contains(hit)) };
    };
    const dialog = one('${DIALOG}');
    const token = one('#hub-token');
    const channel = (name) => ({
      selected: Array.from(document.querySelectorAll('input[data-hub-field^="' + name + ':model:"]:checked')).map((node) => node.dataset.hubField.split(':').at(-1)),
      preferred: (() => { const n=one('#hub-'+name+'-preferred'); return n ? { value:n.value,enabled:!n.disabled,options:Array.from(n.options).map(o=>({value:o.value,label:o.textContent,disabled:o.disabled})) } : null; })(),
      wait: (() => { const n=one('#hub-'+name+'-wait'); return n ? { value:n.value,enabled:!n.disabled,options:Array.from(n.options).map(o=>({value:o.value,label:o.textContent,disabled:o.disabled})) } : null; })(),
      affinity: one('#hub-' + name + '-affinity')?.value ?? null,
      capabilities: one('#hub-'+name+'-capabilities')?.value ?? null,
      routes: Object.fromEntries(['direct','hub'].map(mode=>{const n=one('[data-action="hub-'+(name==='main'?'main':'side')+'-'+mode+'"]');return [mode,n?{pressed:n.getAttribute('aria-pressed'),enabled:!n.disabled}:null];})),
      save_enabled: one('[data-action="hub-save-' + (name === 'main' ? 'main' : 'side') + '"]')?.disabled === false,
      confirmation: one('[data-settings-passive="hub-' + name + '-confirmation"]')?.textContent ?? '',
      feedback: one('[data-settings-passive="hub-' + name + '-feedback"]')?.textContent ?? '',
      models: Array.from(document.querySelectorAll('input[data-hub-field^="' + name + ':model:"]')).map((node) => {
        const label = node.labels?.length === 1 ? node.labels[0] : null;
        return { id: node.dataset.hubField.split(':').at(-1), type: node.type, enabled: !node.disabled,
          checkbox: measure(node), label_present: label !== null && label.contains(node),
          marked_missing: label?.classList.contains('is-missing') ?? false,
          model_label: label?.querySelector('strong')?.textContent?.trim() ?? '',
          model_id: label?.querySelector('small')?.textContent?.trim() ?? '' };
      }),
    });
    const draft = one('#hub-main-affinity');
    return { hub, overlay: projection.overlay, dialog_count: document.querySelectorAll('${DIALOG}').length,
      focused_inside: dialog !== null && dialog.contains(document.activeElement),
      token_empty: token?.value === '', token_password: token?.type === 'password', token_value_attribute: token?.getAttribute('value') ?? null,
      endpoint_value: one('#hub-endpoint')?.value ?? null, label_value: one('#hub-label')?.value ?? null,
      identity: one('[data-settings-passive="hub-identity"]')?.textContent ?? '',
      main: channel('main'), side_chat: channel('side_chat'),
      viewport: { width: innerWidth, height: innerHeight },
      footer: measure(one('${DIALOG} .hub-modal-footer')),
      footer_close: measure(one('${DIALOG} .hub-modal-footer button[data-action="close-overlay"]')),
      draft_active: draft !== null && document.activeElement === draft,
      draft_same_node: globalThis[Symbol.for('${NODE_KEY}')] === draft,
      fatal_count: Array.from(document.querySelectorAll('.fatal, .ui-error-notice')).filter((node) => {
        const style = getComputedStyle(node), rect = node.getBoundingClientRect();
        return !node.hidden && style.display !== 'none' && style.visibility !== 'hidden'
          && Number(style.opacity) !== 0 && rect.width > 0 && rect.height > 0;
      }).length };
  })()`);
}
/** Check usable controls against the live viewport, independently of the stylesheet implementation. */
export function hubConnectedLayoutFailures(surface) {
  const failures = [];
  const viewport = surface?.viewport;
  const inViewport = (rect) => viewport && Number.isFinite(viewport.width) && Number.isFinite(viewport.height)
    && rect?.visible === true
    && [rect.left, rect.top, rect.right, rect.bottom, rect.width, rect.height].every(Number.isFinite)
    && rect.left >= -1 && rect.top >= -1 && rect.right <= viewport.width + 1 && rect.bottom <= viewport.height + 1;
  if (!inViewport(surface?.footer)) failures.push("footer-outside-viewport");
  if (!inViewport(surface?.footer_close) || surface.footer_close.center_hit !== true) failures.push("footer-close-unreachable");
  const expectedModels = surface?.hub?.catalog?.models;
  if (!Array.isArray(expectedModels) || expectedModels.length === 0) return [...failures, "catalog-models-missing"];
  for (const context of ["main", "side_chat"]) {
    const rows = surface?.[context]?.models;
    if (!Array.isArray(rows) || rows.length !== expectedModels.length) {
      failures.push(`${context}-model-row-count`);
      continue;
    }
    for (const model of expectedModels) {
      const matches = rows.filter((row) => row.id === model.id);
      if (matches.length !== 1) { failures.push(`${context}-${model.id}-model-row-identity`); continue; }
      const row = matches[0], checkbox = row.checkbox;
      if (row.type !== "checkbox" || row.enabled !== true || checkbox?.visible !== true
        || !Number.isFinite(checkbox.width) || !Number.isFinite(checkbox.height)
        || checkbox.width <= 0 || checkbox.height <= 0 || checkbox.width > 32 || checkbox.height > 32) failures.push(`${context}-${model.id}-checkbox-size-or-state`);
      if (row.label_present !== true || row.model_label !== model.label || row.model_id !== model.id) failures.push(`${context}-${model.id}-model-label-missing`);
    }
  }
  return failures;
}
export function hubContextConfirmed(surface, context, revision, modelId, affinity) {
  const review = surface?.hub?.[`${context}_review`];
  return surface?.hub?.status === "connected" && surface.hub[`${context}_confirmation`] === "confirmed"
    && review?.reviewed_revision === revision && same(review?.selection.allowed_model_ids, [modelId])
    && review.selection.preferred_model_id === modelId && review.selection.affinity_turns === affinity
    && surface[context]?.confirmation.includes("Hubで確認済み") && surface.fatal_count === 0;
}
export function hubSelectionControlsFailures(surface, context, selection, { confirmed = false } = {}) {
  const local=surface?.[context], failures=[];
  if(!local || surface?.fatal_count!==0) return ["selection-surface"];
  const sameSet=(a,b)=>Array.isArray(a)&&Array.isArray(b)&&a.length===new Set(a).size&&b.length===new Set(b).size&&same([...a].sort(),[...b].sort());
  if(!sameSet(local.selected,selection.allowed_model_ids)) failures.push("allowed-checkboxes");
  if(!sameSet(local.preferred?.options?.map(o=>o.value),selection.allowed_model_ids)
    || local.preferred?.options?.some(o=>o.disabled || !o.label.trim())
    || local.preferred?.enabled!==true || local.preferred?.value!==selection.preferred_model_id) failures.push("preferred-options-or-value");
  if(!same(local.wait?.options?.map(o=>o.value),["wait_for_preferred","allow_selected_fallback"])
    || local.wait?.enabled!==true || local.wait?.value!==selection.wait_policy) failures.push("wait-options-or-value");
  const reviewed=surface.hub?.[`${context}_review`]?.selection;
  if(confirmed && (surface.hub?.[`${context}_confirmation`]!=="confirmed"
    || !reviewed || !sameSet(reviewed.allowed_model_ids,selection.allowed_model_ids)
    || !same({...reviewed,allowed_model_ids:[]},{...selection,allowed_model_ids:[]})
    || !local.confirmation.includes("Hubで確認済み"))) failures.push("selection-not-confirmed");
  return failures;
}
export function hubEmptySelectionControlsReady(surface, context) {
  const local=surface?.[context];
  return surface?.fatal_count===0 && same(local?.selected,[]) && local?.preferred?.enabled===false
    && same(local.preferred.options.map(o=>o.value),[""]) && local.preferred.value==="" && local.save_enabled===false;
}
export function hubDraftRetained(surface, revision, affinity) {
  return surface?.hub?.catalog?.revision === revision && surface.main?.affinity === affinity
    && surface.draft_active === true && surface.draft_same_node === true && surface.main.save_enabled === false
    && surface.main.feedback.includes("最新情報を取得") && surface.hub.main_confirmation === "review_required"
    && surface.hub.side_chat_confirmation === "review_required" && surface.fatal_count === 0;
}
export function hubPersistenceFailures(value, { endpoint, mainRevision = "8", sideRevision = "7", mainModels = HUB_FIXTURE_MODELS }) {
  const failures = [];
  if (!exactKeys(value, ["schema_version", "revision", "endpoint", "label", "hub_id", "main_review", "side_chat_review", "main_mode", "side_chat_mode", "main_catalog_baseline", "side_chat_catalog_baseline"])) return ["unexpected-settings-shape"];
  if (value.schema_version !== 3 || value.endpoint !== `${endpoint}/` || value.hub_id !== HUB_FIXTURE_ID) failures.push("saved-hub-identity");
  if (value.main_mode !== "direct" || value.side_chat_mode !== "direct") failures.push("review-changed-route-mode");
  for (const [context, revision, model, affinity] of [["main", mainRevision, "e2e-main", 9], ["side_chat", sideRevision, "e2e-side", 2]]) {
    const review = value[`${context}_review`];
    if (!exactKeys(review, ["hub_id", "reviewed_revision", "selection"]) || review.hub_id !== HUB_FIXTURE_ID
      || review.reviewed_revision !== revision || !exactKeys(review.selection, selectionKeys)
      || !same(review.selection.allowed_model_ids, [model]) || review.selection.preferred_model_id !== model
      || review.selection.affinity_turns !== affinity || !same(review.selection.required_capabilities, [])
      || review.selection.wait_policy !== "wait_for_preferred") failures.push(`${context}-logical-selection`);
    const baseline = value[`${context}_catalog_baseline`];
    if (!exactKeys(baseline, ["hub_id", "revision", "software_version", "models"])
      || baseline.hub_id !== HUB_FIXTURE_ID || baseline.revision !== revision
      || baseline.software_version !== "0.1.0" || !same(baseline.models, context === "main" ? mainModels : HUB_FIXTURE_MODELS)) failures.push(`${context}-catalog-baseline`);
  }
  return failures;
}
export function hubRestartPreferencesReady(surface, durable) {
  const hub = surface?.hub;
  return surface?.overlay === "hub" && surface.dialog_count === 1 && surface.focused_inside === true
    && hub?.status === "disconnected" && hub.catalog === null && hub.error === null
    && hub.settings_revision === durable.revision && hub.endpoint === durable.endpoint && hub.label === durable.label
    && hub.hub_id === durable.hub_id && surface.endpoint_value === durable.endpoint && surface.label_value === durable.label
    && surface.token_empty === true && surface.token_password === true && surface.token_value_attribute === null
    && ["main", "side_chat"].every((context) => hub[`${context}_confirmation`] === "unconfirmed"
      && hub[`${context}_mode`] === durable[`${context}_mode`]
      && same(hub[`${context}_review`], durable[`${context}_review`])
      && same(surface[context]?.selected, durable[`${context}_review`].selection.allowed_model_ids)
      && surface[context]?.affinity === String(durable[`${context}_review`].selection.affinity_turns)
      && surface[context]?.models?.length === durable[`${context}_review`].selection.allowed_model_ids.length
      && surface[context].models.every((row) => row.model_label === "保存済みのモデル" && row.marked_missing === false)
      && surface[context]?.save_enabled === false && surface[context]?.confirmation.includes("保存済み・Hubで未確認"))
    && surface.fatal_count === 0;
}

async function waitSurface(cdp, label, accept) {
  try { return (await waitForObservation({ label, timeoutMs: 12_000, pollMs: 75,
    sample: () => observeSurface(cdp), accept, retrySampleErrors: false })).value; }
  catch (error) { if (error?.code === "observation-timeout" && !error.evidence?.last_error) throw productFailure(label, error.evidence); throw error; }
}
export function hubControlReady(selector, observation) {
  if (observation.count > 1) throw productFailure("Hub control cardinality", { selector, ...observation });
  return observation.count === 1;
}
async function tabTo(input, cdp, selector) {
  const observe = () => cdp.evaluate(`(() => { const nodes = document.querySelectorAll(${JSON.stringify(selector)}); return { count: nodes.length, active: nodes.length === 1 && document.activeElement === nodes[0] }; })()`);
  // Trusted input delivery can finish before the async action mounts its next surface.
  await waitForObservation({ label: "Hub control appears after navigation", timeoutMs: 12_000, pollMs: 75,
    sample: observe, accept: value => hubControlReady(selector, value), retrySampleErrors: false });
  for (let attempt = 0; attempt < 60; attempt += 1) {
    const state = await observe();
    if (state.count !== 1) throw productFailure("Hub control cardinality", { selector, ...state });
    if (state.active) return;
    await input.pressKey("Tab");
  }
  throw new DesktopE2eError("harness", "hub-focus-target-unreachable", "Hub control was not reached through keyboard navigation", { selector });
}
async function click(input, cdp, target, sink, options) {
  await tabTo(input, cdp, target.selector);
  const start = (await input.snapshotProbe()).sequence;
  const acquired = await input.click(target, options);
  const probe = assertTrustedProbeSequence(await input.snapshotProbe(start), { afterSequence: start,
    expected: [
      { type: "pointerdown", identity: target.identity, button: 0, buttons: 1 },
      { type: "pointerup", identity: target.identity, button: 0, buttons: 0 },
      { type: "click", identity: target.identity, button: 0, buttons: 0 },
    ] });
  await sink.record("hub-trusted-action", { target, acquired, probe }, { phase: "executing", owner: OWNER });
}
export function assertHubTrustedClear(snapshot, afterSequence, target, observation) {
  const proof = assertTrustedProbeSequence(snapshot, { afterSequence, expected: [
    { type: "keydown", identity: target.identity, key: "Backspace" },
    { type: "input", identity: target.identity, inputType: "deleteContentBackward", data: null },
    { type: "keyup", identity: target.identity, key: "Backspace" },
  ] });
  if (observation?.count !== 1 || observation.value !== "" || observation.focused !== true) {
    throw productFailure("Hub clear did not retain the empty focused field", { target, observation });
  }
  return proof;
}
async function edit(input, cdp, id, value, sink, { secret = false } = {}) {
  const target = inputTarget(id);
  await tabTo(input, cdp, target.selector);
  await input.keyDown("Control");
  try { await input.pressKey("a"); } finally { await input.keyUp("Control"); }
  const clearStart = (await input.snapshotProbe()).sequence;
  await input.pressKey("Backspace");
  if (value === "") {
    const observation = await cdp.evaluate(`(() => {
      const nodes = document.querySelectorAll(${JSON.stringify(target.selector)});
      return { count: nodes.length, value: nodes[0]?.value, focused: document.activeElement === nodes[0] };
    })()`);
    const clearing = assertHubTrustedClear(await input.snapshotProbe(clearStart), clearStart, target, observation);
    await sink.record("hub-trusted-clear", { target, clearing, observation }, { phase: "executing", owner: OWNER });
    return;
  }
  const start = (await input.snapshotProbe()).sequence;
  let insertion;
  try {
    await input.insertText(target, value);
    insertion = assertTrustedTextInsertion(await input.snapshotProbe(start), { afterSequence: start, identity: target.identity, text: value });
  } catch (error) { if (secret) throw new DesktopE2eError("harness", "hub-secret-input-failed", "Hub token input could not be verified; evidence redacted"); throw error; }
  await sink.record("hub-trusted-edit", secret ? { target, trusted_insertion: true, value: "[redacted]" } : { target, insertion }, { phase: "executing", owner: OWNER });
}
async function openManualConnection(input, cdp, sink) {
  const opened = () => cdp.evaluate(`(() => {
    const nodes = document.querySelectorAll('#hub-manual-connection');
    return nodes.length === 1 && nodes[0].open === true;
  })()`);
  if (!await opened()) await click(input, cdp, {
    selector: "#hub-manual-connection > summary", identity: { tag: "DETAILS", detailsKey: "hub-manual-connection" },
  }, sink);
  await waitForObservation({ label: "Manual Hub connection controls opened", timeoutMs: 5000, pollMs: 75,
    sample: opened, accept: value => value === true, retrySampleErrors: false });
}
export function hubAdvancedControlsReady(observation) {
  return observation?.details_count === 1 && observation.summary_count === 1 && observation.open === true
    && observation.affinity_count === 1 && observation.affinity_visible === true && observation.affinity_enabled === true;
}
async function openAdvanced(input, cdp, context, sink) {
  const detailsId = `hub-${context}-advanced`;
  const target = { selector: `#${detailsId} > summary`, identity: { tag: "DETAILS", detailsKey: detailsId } };
  const observe = () => cdp.evaluate(`(() => {
    const details = document.querySelectorAll('#${detailsId}');
    const summaries = document.querySelectorAll('#${detailsId} > summary');
    const fields = document.querySelectorAll('#hub-${context}-affinity');
    const field = fields.length === 1 ? fields[0] : null;
    const style = field ? getComputedStyle(field) : null, rect = field?.getBoundingClientRect();
    return { details_count: details.length, summary_count: summaries.length,
      open: details.length === 1 ? details[0].open : null,
      summary_active: summaries.length === 1 && document.activeElement === summaries[0],
      active_tag: document.activeElement?.tagName ?? null, active_id: document.activeElement?.id ?? null,
      affinity_count: fields.length, affinity_enabled: field !== null && !field.disabled,
      affinity_visible: field !== null && field.isConnected && !field.hidden && style.display !== 'none'
        && style.visibility !== 'hidden' && Number(style.opacity) !== 0 && rect.width > 0 && rect.height > 0
        && field.closest('details:not([open])') === null };
  })()`);
  const before = await observe();
  const start = (await input.snapshotProbe()).sequence;
  try {
    if (!hubAdvancedControlsReady(before)) await click(input, cdp, target, sink, { stableHitSamples: 3 });
    let consecutive = 0;
    const settled = await waitForObservation({ label: `Hub ${context} advanced controls`, timeoutMs: 5000, pollMs: 75,
      sample: observe, retrySampleErrors: false,
      accept: (value) => { consecutive = hubAdvancedControlsReady(value) ? consecutive + 1 : 0; return consecutive >= 3; } });
    await sink.record("hub-advanced-controls-opened", { context, target, before, settled, probe: await input.snapshotProbe(start) }, { phase: "executing", owner: OWNER });
  } catch (error) {
    const evidence = { context, target, before, final: await observe(), probe: await input.snapshotProbe(start), cause: error?.evidence ?? null };
    await sink.record("hub-advanced-controls-failure", evidence, { phase: "executing", owner: OWNER });
    if (error?.code === "observation-timeout" && !error.evidence?.last_error) throw productFailure("Hub advanced controls did not settle after a trusted action", evidence);
    throw error;
  }
}

async function chooseDropdown(input, cdp, id, value, sink) {
  const target={selector:`#${id}`,identity:{tag:"SELECT",id}};
  await tabTo(input,cdp,target.selector);
  const before=await cdp.evaluate(`(() => { const n=document.querySelector(${JSON.stringify(target.selector)});return {value:n?.value,active:n===document.activeElement,disabled:n?.disabled,options:n?Array.from(n.options).map(o=>o.value):[]}; })()`);
  const index=before.options.indexOf(value);
  if(!before.active || before.disabled || index<0 || index>8) throw productFailure("Hub dropdown expected option unavailable",{target,value,before});
  await click(input,cdp,target,sink);
  const start=(await input.snapshotProbe()).sequence;
  await input.pressKey("Home");
  for(let count=0;count<index;count++)await input.pressKey("ArrowDown");
  await input.pressKey("Enter");
  // The native select popup owns some navigation keys. Require the trusted
  // committed input/change at the exact SELECT, rather than imaginary DOM key events.
  const probe=assertTrustedProbeSequence(await input.snapshotProbe(start),{afterSequence:start,expected:[
    {type:"input",identity:target.identity},{type:"change",identity:target.identity},
  ]});
  await waitForObservation({label:`Hub ${id} keyboard selection`,timeoutMs:5000,pollMs:60,retrySampleErrors:false,
    sample:()=>cdp.evaluate(`(() => {const n=document.querySelector(${JSON.stringify(target.selector)});return {value:n?.value,active:n===document.activeElement};})()`),accept:state=>state.value===value&&state.active});
  await sink.record("hub-trusted-dropdown-choice",{target,before,value,probe},{phase:"executing",owner:OWNER});
}


export function hubRouteModeFailures(surface, modes, reviews) {
  const failures=[];
  if(surface?.fatal_count!==0 || surface?.hub?.status!=="connected")return ["route-surface"];
  for(const context of ["main","side_chat"]){
    const mode=modes[context],buttons=surface[context]?.routes;
    if(surface.hub[context+"_mode"]!==mode || buttons?.[mode]?.pressed!=="true" || buttons[mode].enabled!==false
      || buttons?.[mode==="hub"?"direct":"hub"]?.pressed!=="false" || buttons[mode==="hub"?"direct":"hub"].enabled!==true)failures.push(context+"-route-controls");
    if(!same(surface.hub[context+"_review"],reviews[context]))failures.push(context+"-review-changed");
  }
  return failures;
}
export function hubCapabilitiesReady(surface,context,text,{valid,feedback=""}){
  return surface?.fatal_count===0 && surface?.[context]?.capabilities===text
    && surface[context].save_enabled===valid && (!feedback || surface[context].feedback.includes(feedback));
}
async function exerciseHubRoutes(input,cdp,sink){
  const before=await observeSurface(cdp),reviews={main:before.hub.main_review,side_chat:before.hub.side_chat_review};
  const modes={main:"direct",side_chat:"direct"};
  for(const [context,mode] of [["main","hub"],["side_chat","hub"],["main","direct"],["side_chat","direct"]]){
    await click(input,cdp,actionTarget('hub-'+(context==='main'?'main':'side')+'-'+mode),sink);
    modes[context]=mode;
    const observation=await waitSurface(cdp,"Independent Hub route mode",value=>hubRouteModeFailures(value,modes,reviews).length===0);
    await sink.record("hub-independent-route-mode",{context,mode,observation},{phase:"executing",owner:OWNER});
    await captureScenarioScreenshot({cdp,sink,name:'hub-route-'+context+'-'+mode,owner:OWNER});
  }
}
async function exerciseHubCapabilities(input,cdp,fixture,sink,context){
  const requests=fixture.requestLedger.filter(row=>row.pathname==="/v1/clients/review").length;
  for(const [text,feedback] of [["vision","必要な機能を満たすものがありません"],["tools!","英数字"],["Tools","必要な機能を満たすものがありません"]]){
    await edit(input,cdp,'hub-'+context+'-capabilities',text,sink);
    const observation=await waitSurface(cdp,"Hub invalid capability stays unsaved",surface=>hubCapabilitiesReady(surface,context,text,{valid:false,feedback}));
    await sink.record("hub-invalid-capability-unsaved",{context,text,observation},{phase:"executing",owner:OWNER});
  }
  if(fixture.requestLedger.filter(row=>row.pathname==="/v1/clients/review").length!==requests)throw productFailure("Invalid capabilities sent a review",fixture.requestLedger);
  await edit(input,cdp,'hub-'+context+'-capabilities','tools, tools',sink);
  await waitSurface(cdp,"Hub duplicate capabilities normalize into a valid selection",surface=>hubCapabilitiesReady(surface,context,'tools, tools',{valid:true}));
}
async function exerciseHubCatalogDetails(input,cdp,fixture,sink){
  for(const context of ["main","side_chat"]){
    const selector='#hub-'+context+'-catalog-diff',target={selector:selector+' > summary',identity:{tag:"DETAILS",detailsKey:'hub-'+context+'-catalog-diff'}};
    const observe=()=>cdp.evaluate('(()=>{const n=document.querySelector('+JSON.stringify(selector)+');return {count:document.querySelectorAll('+JSON.stringify(selector)+').length,open:n?.open,text:n?.textContent,region:n?.querySelector("[role=region]")?.getAttribute("aria-label")};})()');
    await waitForObservation({label:"Actual catalog change has a disclosure",timeoutMs:12000,pollMs:75,sample:observe,accept:v=>v.count===1&&v.open&&v.text.includes('revised')&&v.region===(context==='main'?'Main':'Side')+'のカタログ変更内容'});
    await click(input,cdp,target,sink);
    await waitForObservation({label:"Catalog details close",timeoutMs:5000,pollMs:75,sample:observe,accept:v=>v.count===1&&v.open===false});
    await click(input,cdp,target,sink);
    const reopened=await waitForObservation({label:"Catalog details reopen",timeoutMs:5000,pollMs:75,sample:observe,accept:v=>v.count===1&&v.open===true&&fixture.catalogModels.every(model=>v.text.includes(model.label))});
    await sink.record("hub-catalog-disclosure-roundtrip",{context,reopened:reopened.value},{phase:"executing",owner:OWNER});
    await captureScenarioScreenshot({cdp,sink,name:'hub-'+context+'-catalog-diff-reopened',owner:OWNER});
  }
  const target={selector:'#hub-change-history > summary',identity:{tag:"DETAILS",detailsKey:'hub-change-history'}};
  const observe=()=>cdp.evaluate('(()=>{const n=document.querySelector("#hub-change-history");return {open:n?.open,text:n?.textContent};})()');
  await click(input,cdp,target,sink);
  const opened=await waitForObservation({label:"Catalog revision history opens",timeoutMs:5000,pollMs:75,sample:observe,accept:v=>v.open===true&&v.text.includes('更新 8')&&v.text.includes('Fixture catalog revision')});
  await captureScenarioScreenshot({cdp,sink,name:'hub-catalog-change-history-open',owner:OWNER});
  await click(input,cdp,target,sink);
  await waitForObservation({label:"Catalog revision history closes",timeoutMs:5000,pollMs:75,sample:observe,accept:v=>v.open===false});
  await sink.record("hub-catalog-history-roundtrip",opened.value,{phase:"executing",owner:OWNER});
}

async function exerciseSelectionDropdowns(input, cdp, fixture, sink) {
  for(const context of ["main","side_chat"]){
    const opposite=context==="main"?"side_chat":"main",added=context==="main"?"e2e-side":"e2e-main";
    const before=await observeSurface(cdp),otherReview=before.hub[`${opposite}_review`];
    await click(input,cdp,inputTarget(`hub-${context}-model-${added}`),sink);
    const selection={allowed_model_ids:["e2e-main","e2e-side"],preferred_model_id:added,required_capabilities:["tools"],wait_policy:"allow_selected_fallback",affinity_turns:context==="main"?4:2};
    await exerciseHubCapabilities(input,cdp,fixture,sink,context);
    await chooseDropdown(input,cdp,`hub-${context}-preferred`,added,sink);
    await chooseDropdown(input,cdp,`hub-${context}-wait`,selection.wait_policy,sink);
    await waitSurface(cdp,"Hub independent dropdown draft",surface=>hubSelectionControlsFailures(surface,context,selection).length===0&&same(surface.hub[`${opposite}_review`],otherReview));
    await click(input,cdp,actionTarget(context==="main"?"hub-save-main":"hub-save-side"),sink);
    const saved=await waitSurface(cdp,"Hub independent dropdown save",surface=>hubSelectionControlsFailures(surface,context,selection,{confirmed:true}).length===0&&same(surface.hub[`${opposite}_review`],otherReview));
    const received=fixture.reviewFor(context)?.selection;
    if(!received || !same({...received,allowed_model_ids:[...received.allowed_model_ids].sort()},selection))throw productFailure("Hub dropdown wire selection mismatch",{context,selection,received});
    await sink.record("hub-dropdown-selection-confirmed",{context,selection,saved,other_review_unchanged:true},{phase:"executing",owner:OWNER});
    await captureScenarioScreenshot({cdp,sink,name:`hub-${context}-multiple-preferred-fallback`,owner:OWNER});
  }
  const reviewsBefore=fixture.requestLedger.filter(row=>row.pathname==="/v1/clients/review").length;
  await click(input,cdp,inputTarget("hub-main-model-e2e-main"),sink);
  await click(input,cdp,inputTarget("hub-main-model-e2e-side"),sink);
  const empty=await waitSurface(cdp,"Hub empty model selection cannot be saved",surface=>hubEmptySelectionControlsReady(surface,"main"));
  if(fixture.requestLedger.filter(row=>row.pathname==="/v1/clients/review").length!==reviewsBefore)throw productFailure("Empty Hub selection auto-saved",fixture.requestLedger);
  await sink.record("hub-empty-selection-disabled",empty,{phase:"executing",owner:OWNER});
  await captureScenarioScreenshot({cdp,sink,name:"hub-main-empty-choice-disabled",owner:OWNER});
  // Restore the original single-model preferences through the same visible controls.
  await click(input,cdp,inputTarget("hub-main-model-e2e-main"),sink);
  await edit(input,cdp,"hub-main-capabilities","",sink);
  await chooseDropdown(input,cdp,"hub-main-wait","wait_for_preferred",sink);
  await click(input,cdp,actionTarget("hub-save-main"),sink);
  await waitSurface(cdp,"Hub Main single model restored",surface=>hubContextConfirmed(surface,"main","7","e2e-main",4));
  await click(input,cdp,inputTarget("hub-side_chat-model-e2e-main"),sink);
  await edit(input,cdp,"hub-side_chat-capabilities","",sink);
  await chooseDropdown(input,cdp,"hub-side_chat-wait","wait_for_preferred",sink);
  await click(input,cdp,actionTarget("hub-save-side"),sink);
  await waitSurface(cdp,"Hub Side single model restored",surface=>hubContextConfirmed(surface,"main","7","e2e-main",4)&&hubContextConfirmed(surface,"side_chat","7","e2e-side",2));
}

export function createHubConnectionSettingsScenario() {
  const state = { fixture: null, acceptedLedger: null, quiesce: null, cleanupFailures: [] };
  return Object.freeze({
    id: "hub.connection-settings", productOracle: "pass", manualGate: "pending", databaseRequired: true,
    requestGracefulExit,
    async prepare(args) {
      state.fixture = await new HubSettingsFixture().start();
      await prepareShellBaseline(args);
      await args.sink.record("hub-fixture-started", state.fixture.resourceObservation(), { phase: args.phase, owner: OWNER });
    },
    async execute({ context, driver: firstCdp, host, sink }) {
      const fixture = state.fixture;
      let cdp = firstCdp;
      await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "hub-shell-ready" });
      let input = new WebviewInput(cdp, { probeId: "hub-connection-settings" });
      const releaseInput = async () => {
        if (input === null) return;
        for (const cleanup of [() => input.cleanup(), () => cdp.evaluate(`delete globalThis[Symbol.for('${NODE_KEY}')]`)]) {
          try { await cleanup(); } catch (error) { state.cleanupFailures.push(error?.message ?? String(error)); }
        }
        input = null;
      };
      let primaryError = null;
      try {
        await input.installProbe();
        await click(input, cdp, actionTarget("show-hub", "aside.sidebar"), sink);
        await click(input, cdp, { selector: "#hub-tab-models", identity: { tag: "BUTTON", id: "hub-tab-models" } }, sink);
        await waitSurface(cdp, "Hub preparation opens disconnected", (value) => value.overlay === "hub" && value.dialog_count === 1 && value.hub.status === "disconnected");
        await openManualConnection(input, cdp, sink);
        await edit(input, cdp, "hub-endpoint", fixture.baseUrl, sink);
        await edit(input, cdp, "hub-label", "Desktop E2E device", sink);
        await edit(input, cdp, "hub-token", fixture.bootstrapToken, sink, { secret: true });
        if (fixture.requestLedger.length !== 0) throw productFailure("Hub was contacted before explicit connect", fixture.requestLedger);
        await click(input, cdp, actionTarget("hub-connect"), sink);
        const connected = await waitSurface(cdp, "Connect fetches the catalog and clears the token", (value) => value.hub.status === "connected"
          && value.hub.catalog?.revision === "7" && value.token_empty && value.token_password && value.token_value_attribute === null
          && value.hub.main_confirmation === "unconfirmed" && value.hub.side_chat_confirmation === "unconfirmed");
        if (fixture.containsCredential(JSON.stringify(connected))) throw productFailure("Hub projection exposed a runtime credential", { credential_leak: true });
        await sink.record("hub-connected-secret-free", connected, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "hub-connected-catalog", owner: OWNER });
        const layoutFailures = hubConnectedLayoutFailures(connected);
        if (layoutFailures.length) throw productFailure("Connected Hub controls have an unusable layout", { failures: layoutFailures, observation: connected });

        await click(input, cdp, inputTarget("hub-main-model-e2e-main"), sink);
        await openAdvanced(input, cdp, "main", sink);
        await edit(input, cdp, "hub-main-affinity", "4", sink);
        await click(input, cdp, actionTarget("hub-save-main"), sink);
        const main = await waitSurface(cdp, "Main confirmation leaves Side unset", (value) => hubContextConfirmed(value, "main", "7", "e2e-main", 4)
          && value.hub.side_chat_review === null && value.hub.side_chat_confirmation === "unconfirmed");
        await sink.record("hub-main-independent-review", main, { phase: "executing", owner: OWNER });

        await click(input, cdp, inputTarget("hub-side_chat-model-e2e-side"), sink);
        await openAdvanced(input, cdp, "side_chat", sink);
        await edit(input, cdp, "hub-side_chat-affinity", "2", sink);
        await click(input, cdp, actionTarget("hub-save-side"), sink);
        const both = await waitSurface(cdp, "Both contexts retain separate selections", (value) => hubContextConfirmed(value, "main", "7", "e2e-main", 4)
          && hubContextConfirmed(value, "side_chat", "7", "e2e-side", 2));
        await sink.record("hub-both-independent-reviews", both, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "hub-independent-selections", owner: OWNER });

        await exerciseSelectionDropdowns(input,cdp,fixture,sink);
        await exerciseHubRoutes(input,cdp,sink);

        await edit(input, cdp, "hub-main-affinity", "9", sink);
        await cdp.evaluate(`globalThis[Symbol.for('${NODE_KEY}')] = document.querySelector('#hub-main-affinity'); void 0`);
        fixture.advanceRevision({changeModelLabels:true});
        const changed = await waitSurface(cdp, "Catalog drift keeps the focused unsaved draft and requires review", (value) => hubDraftRetained(value, "8", "9"));
        await sink.record("hub-revision-keeps-focused-draft", changed, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "hub-revision-focused-draft", owner: OWNER });
        await exerciseHubCatalogDetails(input,cdp,fixture,sink);
        await click(input, cdp, actionTarget("hub-refresh"), sink);
        await waitSurface(cdp, "Explicit refresh rebases the retained draft", (value) => value.hub.catalog?.revision === "8" && value.main.affinity === "9" && value.main.save_enabled);
        await click(input, cdp, actionTarget("hub-save-main"), sink);
        const refreshed = await waitSurface(cdp, "Main re-review does not confirm old Side selection", (value) => hubContextConfirmed(value, "main", "8", "e2e-main", 9)
          && value.hub.side_chat_review?.reviewed_revision === "7" && value.hub.side_chat_confirmation === "review_required");
        await sink.record("hub-independent-reconfirmation", refreshed, { phase: "executing", owner: OWNER });

        await click(input, cdp, actionTarget("hub-disconnect"), sink);
        const disconnected = await waitSurface(cdp, "Disconnect revokes device registration and retains logical selections", (value) => value.hub.status === "disconnected"
          && value.hub.main_confirmation !== "confirmed" && value.hub.side_chat_confirmation !== "confirmed" && value.token_empty && fixture.clientCount === 0);
        const durableText = await readFile(path.join(context.paths.config, "hub-settings.json"), "utf8");
        const durable = JSON.parse(durableText);
        const failures = hubPersistenceFailures(durable, { endpoint: fixture.baseUrl, mainModels: fixture.catalogModels });
        if (fixture.containsCredential(durableText) || failures.length) throw productFailure("Hub durable preferences mismatch", { failures, credential_leak: fixture.containsCredential(durableText) });
        const ledger = fixture.requestLedger;
        if (ledger.some((row) => row.status !== 200 || (row.pathname !== "/v1/clients/register" && row.authorization !== "client"))
          || !same(ledger.filter((row) => row.pathname === "/v1/clients/review").map((row) => [row.context, row.revision]), [["main", "7"], ["side_chat", "7"], ["main", "7"], ["side_chat", "7"], ["main", "7"], ["side_chat", "7"], ["main", "8"]])
          || ledger.filter((row) => row.pathname === "/v1/clients/register").length !== 1
          || ledger.filter((row) => row.pathname === "/v1/clients/disconnect").length !== 1) throw productFailure("Hub authenticated wire flow mismatch", ledger);
        await sink.record("hub-disconnected-durable-logical-settings", { observation: disconnected, durable, ledger, credentials_absent: true }, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "hub-disconnected-preferences", owner: OWNER });
        await click(input, cdp, actionTarget("close-overlay", `${DIALOG} .hub-modal-footer`), sink);
        await waitSurface(cdp, "Hub settings close", (value) => value.overlay === "none" && value.dialog_count === 0);
        await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "hub-shell-restored" });

        const ledgerBeforeRestart = fixture.requestLedger;
        await releaseInput();
        if (state.cleanupFailures.length) throw new DesktopE2eError("harness", "hub-input-cleanup-before-restart", "Hub input cleanup did not settle before restart", state.cleanupFailures);
        const restarted = await host.restart({ context, scenario: this, sink, driver: cdp, phase: "executing" });
        cdp = restarted.driver;
        input = new WebviewInput(cdp, { probeId: "hub-connection-settings-restored" });
        await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "hub-restarted-shell-ready" });
        await input.installProbe();
        await click(input, cdp, actionTarget("show-hub", "aside.sidebar"), sink);
        await click(input, cdp, { selector: "#hub-tab-models", identity: { tag: "BUTTON", id: "hub-tab-models" } }, sink);
        await openManualConnection(input, cdp, sink);
        const restored = await waitSurface(cdp, "Restart restores saved Hub choices without restoring a connection", (value) => hubRestartPreferencesReady(value, durable));
        const stableStarted = Date.now();
        const stable = await waitForObservation({ label: "Restored Hub remains disconnected without automatic HTTP", timeoutMs: 6000, pollMs: 100,
          retrySampleErrors: false,
          sample: async () => ({ surface: await observeSurface(cdp), ledger: fixture.requestLedger }),
          accept: (value) => {
            if (!hubRestartPreferencesReady(value.surface, durable) || !same(value.ledger, ledgerBeforeRestart)) {
              throw productFailure("Restart changed saved Hub choices or started implicit HTTP", { observation: value, expected_settings: durable, ledger_before_restart: ledgerBeforeRestart });
            }
            return Date.now() - stableStarted >= 2200;
          } });
        const restoredText = await readFile(path.join(context.paths.config, "hub-settings.json"), "utf8");
        if (restoredText !== durableText || fixture.containsCredential(JSON.stringify(stable.value.surface))) {
          throw productFailure("Restart rewrote Hub settings or exposed a credential", { settings_unchanged: restoredText === durableText, credential_leak: fixture.containsCredential(JSON.stringify(stable.value.surface)) });
        }
        await sink.record("hub-restart-restores-preferences-without-credentials-or-http", {
          restart: restarted.restart, first_restored: restored, later_restored: stable.value.surface,
          stable_observation_ms: stable.elapsed_ms, ledger_before_restart: ledgerBeforeRestart, ledger_after_restart: stable.value.ledger,
          settings_unchanged: true, credentials_absent: true,
        }, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "hub-restart-saved-disconnected", owner: OWNER });
        await click(input, cdp, actionTarget("close-overlay", `${DIALOG} .hub-modal-footer`), sink);
        await waitSurface(cdp, "Restored Hub settings close", (value) => value.overlay === "none" && value.dialog_count === 0);
        state.acceptedLedger = fixture.requestLedger;
        return { acquisition: "pass", oracle: "pass", manual: "pending" };
      } catch (error) { primaryError = error; throw error; }
      finally {
        await releaseInput();
        if (!primaryError && state.cleanupFailures.length) throw new DesktopE2eError("harness", "hub-input-cleanup", "Hub input cleanup failed", state.cleanupFailures);
      }
    },
    async quiesce({ inputs }) {
      state.quiesce ??= await quiesceProviderResource({ provider: state.fixture, acceptedLedger: state.acceptedLedger, inputs });
      return structuredClone(state.quiesce);
    },
    async cleanup() {
      return { input: state.quiesce?.input === "pass" && state.cleanupFailures.length === 0 ? "pass" : "fail",
        resources: [{ kind: "hub-settings-input", failures: state.cleanupFailures }] };
    },
  });
}
