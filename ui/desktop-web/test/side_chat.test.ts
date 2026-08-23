import assert from "node:assert/strict";
import test from "node:test";

import {
  actionById,
  persistSideChatDraft,
  type ActionContext,
} from "../src/actions.ts";
import {
  handleSettingsSelectAllShortcut,
  shortcutActionForComposer,
} from "../src/events.ts";
import {
  renderArtifactPane,
  renderOverlay,
  renderSideChatDeleteConfirmation,
} from "../src/render.ts";
import {
  createDesktopRenderModel,
  DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
  type DesktopRenderLocalPresentation,
} from "../src/render_projection.ts";
import type {
  DesktopViewState,
  SideChatCatalogResult,
  SideChatProjection,
} from "../src/types.ts";
import {
  beginSideChatCatalogLoad,
  canonicalSideChatCatalogBaseUrl,
  canonicalSideChatProviderBaseUrl,
  createUiLocalState,
  finishSideChatCatalogLoad,
  sideChatCatalogLoadOpen,
  sideChatCatalogViewForState,
  sideChatDeleteConfirmationStillTargets,
  sideChatDraftForState,
  sideChatModelOptions,
  sideChatOperationsOpen,
  type SideChatCatalogView,
} from "../src/ui_state.ts";

function sideChat(overrides: Partial<SideChatProjection> = {}): SideChatProjection {
  return {
    configured: true,
    deleting: false,
    chat_id: "side-a",
    owner_session_id: "session-a",
    model: "gemma-test",
    base_url: "http://127.0.0.1:1234/v1",
    provider_profile: "openai_compatible",
    status: "idle",
    phase: "idle",
    last_error: "",
    generation: "4",
    draft_text: "",
    draft_revision: "0",
    messages: [],
    can_send: true,
    can_cancel: false,
    ...overrides,
  };
}

function state(
  sideOverrides: Partial<SideChatProjection> = {},
  ownerSessionId = "session-a",
): DesktopViewState {
  return {
    workspace_path: "C:/workspace",
    draft_target: {
      workspacePath: "C:/workspace",
      sessionId: ownerSessionId,
      ownerGeneration: "1",
    },
    provider_base_url: "http://127.0.0.1:1234/v1",
    provider_profile: "openai_compatible",
    provider_api_key_env: "",
    provider_effective_base_url: "http://127.0.0.1:1234/v1",
    provider_effective_profile: "openai_compatible",
    provider_effective_api_key_env: "",
    provider_catalog_base_url: null,
    provider_catalog_profile: null,
    provider_catalog_api_key_env: null,
    provider_model_ids: [],
    provider_models: [],
    config_target: {
      workspacePath: "C:/workspace",
      sessionId: ownerSessionId,
      configGeneration: "7",
    },
    overlay: "none",
    startup: {
      initial_setup_required: false,
      action_overlay: "none",
    },
    config_fields: [],
    docling_readiness: {
      status: "idle",
      endpoint: "",
      httpStatus: null,
      message: "Docling readiness has not been checked.",
    },
    side_chat: sideChat({ owner_session_id: ownerSessionId, ...sideOverrides }),
    agent_activity_rows: [],
    config_draft: {
      dirty: false,
      edit_enabled: true,
      discard_enabled: false,
      commit_enabled: false,
      external_owner_mutation_open: true,
      access_mode_mutation_enabled: true,
    },
  } as DesktopViewState;
}

function useSidePane(overrides: {
  draft?: string;
  baseUrl?: string;
  providerProfile?: "lm_studio" | "openai_compatible" | "openai_responses" | "lm_studio_chat_completions";
  model?: string;
  pending?: boolean;
  confirmingDelete?: boolean;
  catalog?: SideChatCatalogView;
  catalogLoadEnabled?: boolean;
  configPending?: boolean;
  configDirty?: boolean;
  configDraftEditOpen?: boolean;
  operationsOpen?: boolean;
} = {}): {
  artifactPane: (view: DesktopViewState) => string;
  overlay: (view: DesktopViewState) => string;
  deleteConfirmation: (view: DesktopViewState) => string;
} {
  const local: DesktopRenderLocalPresentation = {
    ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
    artifactPane: {
      ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.artifactPane,
      collapsed: false,
      mode: "side_chat",
    },
    configMutationPending: overrides.configPending ?? false,
    sideChat: {
      ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.sideChat,
      draft: overrides.draft ?? "",
      setupBaseUrl: overrides.baseUrl ?? "",
      setupProviderProfile: overrides.providerProfile ?? "openai_compatible",
      setupModel: overrides.model ?? "",
      catalog: overrides.catalog ?? DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.sideChat.catalog,
      catalogLoadEnabled: overrides.catalogLoadEnabled ?? false,
      mutationPending: overrides.pending ?? false,
      operationsOpen: overrides.operationsOpen ?? !(overrides.configPending ?? false),
      deleteConfirmation: overrides.confirmingDelete
        ? { ownerSessionId: "session-a", chatId: "side-a", expectedGeneration: "4" }
        : null,
    },
  };
  return {
    artifactPane: (view) => renderArtifactPane(view, local),
    overlay: (view) => renderOverlay(view, local),
    deleteConfirmation: (view) => renderSideChatDeleteConfirmation(view, local),
  };
}

test("unconfigured right side chat only links to Settings and owns no provider inputs", () => {
  const renderer = useSidePane({
    baseUrl: "http://main.test/v1?mode=<safe>",
    model: "gemma-3",
  });
  const html = renderer.artifactPane(state({
    configured: false,
    chat_id: null,
    model: "",
    base_url: "",
    generation: "0",
    can_send: false,
  }));

  assert.match(html, /data-pane-mode="side-chat"/);
  assert.match(html, /data-action="show-config"/);
  assert.doesNotMatch(html, /id="side-chat-base-url"/);
  assert.doesNotMatch(html, /id="side-chat-model"/);
  assert.doesNotMatch(html, /data-action="configure-side-chat"/);
  assert.doesNotMatch(html, /data-action="send"(?:\s|>)/);
});

test("Settings owns the session-scoped side provider fields and action", () => {
  const renderer = useSidePane({
    baseUrl: "http://side.test/v1/path",
    model: "gemma-settings",
  });
  const html = renderer.overlay({ ...state(), overlay: "config" });

  assert.match(html, /href="#settings-side-chat"/);
  assert.match(html, /id="settings-side-chat"[^>]*data-side-chat-settings-owner="session-a"/);
  assert.match(html, /id="side-chat-base-url"[^>]*value="http:\/\/side\.test\/v1\/path"/);
  assert.match(html, /<label for="side-chat-model">Model<\/label>/);
  assert.match(
    html,
    /id="side-chat-model"[^>]*aria-describedby="[^"]*side-chat-model-help[^"]*side-chat-settings-help[^"]*side-chat-model-catalog-status[^"]*side-chat-settings-status[^"]*"/,
  );
  assert.match(html, /<option value="gemma-settings" selected>gemma-settings（現在の設定）<\/option>/);
  assert.match(html, /一覧にないモデルIDを入力/);
  assert.match(html, /id="side-chat-model-manual"[^>]*value="gemma-settings"/);
  assert.match(html, /data-action="load-side-chat-models"[^>]*aria-controls="side-chat-model side-chat-model-catalog-status"/);
  assert.match(html, /data-action="configure-side-chat"/);
  assert.match(html, /上部の「UIセッションに適用」「設定ファイルに保存」とは別に保存/);
});

test("Settings rejects invalid Side provider targets before configure or catalog commands", () => {
  const invalidRenderer = useSidePane({
    baseUrl: "https://user:secret@side.test/v1?hidden=true",
    model: "gemma-settings",
    catalogLoadEnabled: true,
  });
  const invalidUrl = invalidRenderer.overlay({ ...state(), overlay: "config" });
  assert.match(invalidUrl, /id="side-chat-base-url"[^>]*aria-invalid="true"/);
  assert.match(invalidUrl, /id="side-chat-settings-status"[^>]*class="side-chat-settings-status error"/);
  assert.match(invalidUrl, /URL に認証情報を含めず/);
  assert.match(invalidUrl, /data-action="configure-side-chat"[^>]*aria-disabled="true"[^>]*disabled/);
  assert.match(invalidUrl, /data-action="load-side-chat-models"[^>]*aria-disabled="true"[^>]*disabled/);

  const missingRenderer = useSidePane({
    baseUrl: "http://side.test/proxy/v1",
    model: "   ",
    catalogLoadEnabled: true,
  });
  const missingModel = missingRenderer.overlay({ ...state(), overlay: "config" });
  assert.match(missingModel, /id="side-chat-model-manual"[^>]*aria-invalid="true"/);
  assert.match(missingModel, /モデルIDを入力してください。/);
  assert.match(missingModel, /data-action="configure-side-chat"[^>]*aria-disabled="true"[^>]*disabled/);
  assert.match(missingModel, /data-action="load-side-chat-models"[^>]*aria-disabled="false"/);
});

test("Settings presents Main and Side LLM URL and native model selection consistently without merging their owners", () => {
  const renderer = useSidePane({
    baseUrl: "http://side.test/v1",
    model: "gemma-side",
    catalogLoadEnabled: true,
  });
  const html = renderer.overlay({
    ...state(),
    overlay: "config",
    provider_catalog_base_url: "http://main.test",
    provider_catalog_profile: "openai_compatible",
    provider_catalog_api_key_env: null,
    provider_model_ids: ["qwen-main", "qwen-main-alt"],
    provider_models: ["Qwen Main（ロード済み）", "Qwen Main Alt（未ロード）"],
    config_fields: [
      {
        key: "model.base_url",
        value: "http://main.test/v1",
        env_override: null,
        value_type: "string",
        required: true,
        min_value: null,
        max_value: null,
        options: [],
      },
      {
        key: "model.model",
        value: "qwen-main",
        env_override: null,
        value_type: "string",
        required: true,
        min_value: null,
        max_value: null,
        options: [],
      },
      {
        key: "model.provider_profile",
        value: "openai_compatible",
        env_override: null,
        value_type: "enum",
        required: true,
        min_value: null,
        max_value: null,
        options: ["lm_studio", "openai_compatible", "openai_responses", "lm_studio_chat_completions"],
      },
      {
        key: "model.api_key_env",
        value: "",
        env_override: null,
        value_type: "string",
        required: false,
        min_value: null,
        max_value: null,
        options: [],
      },
    ],
  });
  const mainStart = html.indexOf('<section id="settings-provider"');
  const mainEnd = html.indexOf('<section id="settings-model"');
  const sideStart = html.indexOf('<section id="settings-side-chat"');
  const sideEnd = html.indexOf('<section id="settings-permissions"');
  assert.ok(mainStart >= 0 && mainEnd > mainStart && sideStart > mainEnd && sideEnd > sideStart);
  const main = html.slice(mainStart, mainEnd);
  const side = html.slice(sideStart, sideEnd);

  assert.match(main, /<h3 id="settings-provider-title">メインLLM<\/h3>/);
  assert.ok(main.indexOf("LLM URL") < main.indexOf('for="main-provider-model">Model'));
  assert.match(main, /<select id="main-provider-model"[^>]*data-config-key="model\.model"/);
  assert.match(main, /<option value="qwen-main" selected>Qwen Main（ロード済み）<\/option>/);
  assert.match(main, /<option value="qwen-main-alt" >Qwen Main Alt（未ロード）<\/option>/);
  assert.match(main, /data-action="show-provider"[^>]*aria-controls="main-provider-model main-provider-model-catalog-status"/);
  assert.match(main, /メインチャットのUIセッション、または設定ファイル/);
  assert.doesNotMatch(main, /data-side-chat-setting/);

  assert.match(side, /<h3 id="settings-side-chat-title">サイドチャットLLM<\/h3>/);
  assert.ok(side.indexOf("LLM URL") < side.indexOf('<label for="side-chat-model">Model'));
  assert.match(side, /<select id="side-chat-model"/);
  assert.match(side, /選択中の通常チャットだけに適用/);
  assert.doesNotMatch(side, /data-config-key/);
});

test("Main Settings never offers model rows from a catalog owned by another URL", () => {
  const renderer = useSidePane();
  const html = renderer.overlay({
    ...state(),
    overlay: "config",
    provider_catalog_base_url: "http://stale-main.test",
    provider_catalog_profile: "openai_compatible",
    provider_catalog_api_key_env: null,
    provider_model_ids: ["stale-model"],
    provider_models: ["Stale model"],
    config_fields: [
      {
        key: "model.base_url",
        value: "http://current-main.test/v1",
        env_override: null,
        value_type: "string",
        required: true,
        min_value: null,
        max_value: null,
        options: [],
      },
      {
        key: "model.model",
        value: "current-model",
        env_override: null,
        value_type: "string",
        required: true,
        min_value: null,
        max_value: null,
        options: [],
      },
      {
        key: "model.provider_profile",
        value: "openai_compatible",
        env_override: null,
        value_type: "enum",
        required: true,
        min_value: null,
        max_value: null,
        options: ["openai_compatible"],
      },
      {
        key: "model.api_key_env",
        value: "",
        env_override: null,
        value_type: "string",
        required: false,
        min_value: null,
        max_value: null,
        options: [],
      },
    ],
  });
  const main = html.slice(
    html.indexOf('<section id="settings-provider"'),
    html.indexOf('<section id="settings-model"'),
  );

  assert.match(main, /<option value="current-model" selected>current-model（現在の設定）<\/option>/);
  assert.doesNotMatch(main, /stale-model|Stale model/);
  assert.match(main, /現在のLLM URLとConnection typeに対応する候補を取得/);
});

test("Settings model dropdown exposes loaded options and retains a current model outside the catalog", () => {
  const renderer = useSidePane({
    baseUrl: "http://side.test/v1",
    model: "manual<&model",
    catalogLoadEnabled: true,
    catalog: {
      status: "ready",
      source: "side",
      ownerSessionId: "session-a",
      baseUrl: "http://side.test",
      models: [
        { id: "google/gemma-4-12b-qat", label: "Gemma <QAT>", loadState: "loaded" },
        { id: "qwen/qwen3.6-27b", label: "Qwen & 27B", loadState: "not_loaded" },
      ],
      error: "",
    },
  });
  const html = renderer.overlay({ ...state(), overlay: "config" });

  assert.match(html, /<option value="manual&lt;&amp;model" selected>manual&lt;&amp;model（現在の設定）<\/option>/);
  assert.match(html, /<option value="google\/gemma-4-12b-qat" >Gemma &lt;QAT&gt;（ロード済み）<\/option>/);
  assert.match(html, /<option value="qwen\/qwen3\.6-27b" >Qwen &amp; 27B（未ロード）<\/option>/);
  assert.match(html, /2件のモデルから選択できます/);
  assert.match(html, /data-action="load-side-chat-models"[^>]*>モデル読込<\/button>/);
});

test("an unconfigured catalog keeps an explicit placeholder until the user selects a model", () => {
  const renderer = useSidePane({
    baseUrl: "http://side.test/v1",
    model: "",
    catalogLoadEnabled: true,
    catalog: {
      status: "ready",
      source: "side",
      ownerSessionId: "session-a",
      baseUrl: "http://side.test",
      models: [{
        id: "google/gemma-4-12b-qat",
        label: "Gemma 4 12B QAT",
        loadState: "loaded",
      }],
      error: "",
    },
  });
  const html = renderer.overlay({ ...state({
    configured: false,
    chat_id: null,
    model: "",
    base_url: "",
    generation: "0",
    can_send: false,
  }), overlay: "config" });

  assert.match(html, /<option value="" selected disabled>モデルを選択してください<\/option>/);
  assert.match(html, /<option value="google\/gemma-4-12b-qat" >Gemma 4 12B QAT（ロード済み）<\/option>/);
  assert.match(html, /id="side-chat-model"[^>]*(?!disabled)>/);
  assert.match(html, /data-action="configure-side-chat"[^>]*aria-disabled="true"[^>]*disabled/);
});

test("Settings exposes catalog loading and failure through an accessible live status", () => {
  const loadingRenderer = useSidePane({
    baseUrl: "http://side.test/v1",
    model: "gemma-current",
    catalog: {
      status: "loading",
      source: "side",
      ownerSessionId: "session-a",
      baseUrl: "http://side.test",
      models: [],
      error: "",
    },
  });
  const loading = loadingRenderer.overlay({ ...state(), overlay: "config" });
  assert.match(loading, /id="settings-side-chat"[^>]*aria-busy="true"/);
  assert.match(loading, /data-action="load-side-chat-models"[^>]*disabled[^>]*>読込中…<\/button>/);
  assert.match(loading, /id="side-chat-model-catalog-status"[^>]*role="status" aria-live="polite">モデル一覧を読み込んでいます…<\/p>/);

  const failedRenderer = useSidePane({
    baseUrl: "http://side.test/v1",
    model: "gemma-current",
    catalogLoadEnabled: true,
    catalog: {
      status: "error",
      source: "side",
      ownerSessionId: "session-a",
      baseUrl: "http://side.test",
      models: [],
      error: "接続できません <retry>",
    },
  });
  const failed = failedRenderer.overlay({ ...state(), overlay: "config" });
  assert.match(failed, /id="side-chat-model-catalog-status" class="side-chat-model-catalog-status error"[^>]*>接続できません &lt;retry&gt;<\/p>/);
  assert.match(failed, /data-action="load-side-chat-models"[^>]*>モデル読込<\/button>/);
});

test("Side Chat model catalog load is explicit, canonical, and session scoped", async () => {
  const ui = createUiLocalState();
  const current = state();
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.setupBaseUrl = "http://side.test/v1/";
  draft.setupModel = "google/gemma-4-12b-qat";
  let args: Record<string, unknown> | null = null;
  let rerenders = 0;
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    loadSideChatModels: async (input: Record<string, unknown>) => {
      args = input;
      return {
        ownerSessionId: "session-a",
        baseUrl: "http://side.test",
        providerProfile: "openai_compatible" as const,
        configGeneration: "7",
        models: [{
          id: "google/gemma-4-12b-qat",
          label: "google/gemma-4-12b-qat",
          loadState: "loaded" as const,
        }],
      };
    },
    recoverCommandConflict: () => false,
    rerender: () => { rerenders += 1; },
  } as unknown as ActionContext;

  const load = actionById("load-side-chat-models");
  assert.ok(load);
  await load.run(current, context, { index: -1, value: "" });

  assert.deepEqual(args, {
    ownerSessionId: "session-a",
    baseUrl: "http://side.test",
    providerProfile: "openai_compatible",
    expectedConfigGeneration: "7",
  });
  assert.equal(rerenders, 2);
  assert.equal(sideChatCatalogViewForState(ui, current).status, "ready");
  assert.deepEqual(
    sideChatModelOptions(sideChatCatalogViewForState(ui, current), draft.setupModel).map((model) => model.id),
    ["google/gemma-4-12b-qat"],
  );
});

test("Side Chat catalog ABA settlement rerenders reload guidance and only a fresh load can settle", async () => {
  const ui = createUiLocalState();
  const current = state();
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.setupBaseUrl = "http://first.test/v1";
  draft.setupModel = "manual-current";
  draft.text = "unsent side draft";
  draft.revision += 1;

  let releaseOld!: (result: SideChatCatalogResult) => void;
  const oldResult = new Promise<SideChatCatalogResult>((resolve) => {
    releaseOld = resolve;
  });
  let loads = 0;
  let rerenders = 0;
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    loadSideChatModels: async () => {
      loads += 1;
      if (loads === 1) return oldResult;
      return {
        ownerSessionId: "session-a",
        baseUrl: "http://first.test",
        providerProfile: "openai_compatible" as const,
        configGeneration: "7",
        models: [{ id: "fresh-model", label: "Fresh", loadState: "loaded" as const }],
      };
    },
    recoverCommandConflict: () => false,
    rerender: () => { rerenders += 1; },
  } as unknown as ActionContext;
  const load = actionById("load-side-chat-models");
  assert.ok(load);

  const oldLoad = Promise.resolve(load.run(current, context, { index: -1, value: "" }));
  assert.equal(rerenders, 1);
  assert.equal(sideChatCatalogViewForState(ui, current).status, "loading");

  draft.setupBaseUrl = "http://second.test/v1";
  draft.setupRevision += 1;
  draft.setupBaseUrl = "http://first.test/v1";
  draft.setupRevision += 1;
  releaseOld({
    ownerSessionId: "session-a",
    baseUrl: "http://first.test",
    providerProfile: "openai_compatible",
    configGeneration: "7",
    models: [{ id: "stale-model", label: "Stale", loadState: "loaded" }],
  });
  await oldLoad;

  const rejected = sideChatCatalogViewForState(ui, current);
  assert.equal(rerenders, 2, "clearing a current loading owner requires a DOM settlement render");
  assert.equal(rejected.status, "error");
  assert.match(rejected.error, /もう一度モデル一覧を読み込んでください/);
  assert.deepEqual(rejected.models, []);
  assert.equal(sideChatCatalogLoadOpen(ui, current), true);
  assert.equal(draft.setupBaseUrl, "http://first.test/v1");
  assert.equal(draft.setupModel, "manual-current");
  assert.equal(draft.text, "unsent side draft");

  const rejectedRenderer = useSidePane({
    draft: draft.text,
    baseUrl: draft.setupBaseUrl,
    model: draft.setupModel,
    catalog: rejected,
    catalogLoadEnabled: sideChatCatalogLoadOpen(ui, current),
  });
  const rejectedHtml = rejectedRenderer.overlay({ ...current, overlay: "config" });
  assert.match(rejectedHtml, /id="settings-side-chat"[^>]*aria-busy="false"/);
  assert.match(
    rejectedHtml,
    /data-action="load-side-chat-models"[^>]*aria-disabled="false"(?![^>]*\sdisabled(?:\s|>|=))[^>]*>モデル読込<\/button>/,
  );
  assert.match(rejectedHtml, /もう一度モデル一覧を読み込んでください/);
  assert.match(rejectedHtml, /manual-current（現在の設定）/);
  assert.doesNotMatch(rejectedHtml, /読込中…/);
  assert.doesNotMatch(rejectedHtml, /stale-model/);

  await load.run(current, context, { index: -1, value: "" });
  const accepted = sideChatCatalogViewForState(ui, current);
  assert.equal(rerenders, 4);
  assert.equal(loads, 2);
  assert.equal(accepted.status, "ready");
  assert.deepEqual(accepted.models.map((model) => model.id), ["fresh-model"]);
  assert.equal(draft.setupModel, "manual-current");
  assert.equal(draft.text, "unsent side draft");
});

test("Side Chat catalog loading keeps native Select All owned by the connected Settings URL editor", () => {
  class FakeElement {
    isConnected = true;
    withinSettings = true;

    closest<T>(selector: string): T | null {
      return (selector === ".settings-modal" && this.withinSettings ? {} : null) as T | null;
    }
  }

  class FakeInput extends FakeElement {
    type = "url";
    disabled = false;
    readOnly = false;
    value = "http://side.test/v1/slow";
    selectionStart = 7;
    selectionEnd = 7;

    setSelectionRange(start: number, end: number): void {
      this.selectionStart = start;
      this.selectionEnd = end;
    }
  }

  class FakeTextArea extends FakeElement {
    disabled = false;
    readOnly = false;
  }

  const previousGlobals = new Map(
    ["Element", "HTMLInputElement", "HTMLTextAreaElement"]
      .map((name) => [name, Object.getOwnPropertyDescriptor(globalThis, name)] as const),
  );
  const defineGlobal = (name: string, value: unknown) => {
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
  };

  try {
    defineGlobal("Element", FakeElement);
    defineGlobal("HTMLInputElement", FakeInput);
    defineGlobal("HTMLTextAreaElement", FakeTextArea);

    const ui = createUiLocalState();
    const current = state();
    current.overlay = "config";
    current.confirmation_visible = false;
    const draft = sideChatDraftForState(ui, current);
    assert.ok(draft);
    draft.setupBaseUrl = "http://side.test/v1/slow";
    const request = beginSideChatCatalogLoad(ui, current);
    assert.ok(request);
    assert.equal(request.setupRevision, draft.setupRevision);
    assert.equal(sideChatCatalogViewForState(ui, current).status, "loading");

    const editor = new FakeInput();
    const activeElement = editor;
    const shortcut = {
      key: "a",
      ctrlKey: true,
      metaKey: false,
      altKey: false,
    };
    assert.equal(
      handleSettingsSelectAllShortcut(shortcut, current, activeElement as unknown as Element),
      true,
    );
    assert.deepEqual([editor.selectionStart, editor.selectionEnd], [0, editor.value.length]);
    assert.equal(activeElement, editor, "the connected input remains the focus owner while loading");
    assert.equal(draft.setupBaseUrl, "http://side.test/v1/slow");

    assert.deepEqual(finishSideChatCatalogLoad(ui, current, request, {
      ownerSessionId: "session-a",
      baseUrl: request.baseUrl,
      providerProfile: "openai_compatible",
      configGeneration: "7",
      models: [{ id: "model-ready", label: "Ready", loadState: "loaded" }],
    }), { catalogAccepted: true, localStateChanged: true });
    assert.equal(sideChatCatalogViewForState(ui, current).status, "ready");
    editor.selectionStart = 9;
    editor.selectionEnd = 9;
    assert.equal(
      handleSettingsSelectAllShortcut(shortcut, current, activeElement as unknown as Element),
      true,
    );
    assert.deepEqual([editor.selectionStart, editor.selectionEnd], [0, editor.value.length]);
    assert.equal(activeElement, editor, "catalog settlement does not replace or steal the editor owner");

    editor.isConnected = false;
    editor.selectionStart = 4;
    editor.selectionEnd = 4;
    assert.equal(
      handleSettingsSelectAllShortcut(shortcut, current, editor as unknown as Element),
      false,
      "a disconnected editor from an old Settings owner is never changed",
    );
    assert.deepEqual([editor.selectionStart, editor.selectionEnd], [4, 4]);
  } finally {
    for (const [name, descriptor] of previousGlobals) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor);
      else delete (globalThis as Record<string, unknown>)[name];
    }
  }
});

test("Side Chat catalog canonicalization matches provider URL ownership", () => {
  assert.equal(
    canonicalSideChatProviderBaseUrl(" HTTP://LOCALHOST:80/v1/ "),
    "http://localhost/v1",
  );
  assert.equal(
    canonicalSideChatCatalogBaseUrl(" HTTP://LOCALHOST:80/v1/ "),
    "http://localhost",
  );
  assert.equal(
    canonicalSideChatCatalogBaseUrl("https://Provider.Example:443/proxy/openai/v1/"),
    "https://provider.example/proxy/openai",
  );
  assert.equal(canonicalSideChatCatalogBaseUrl("http://localhost////v1/"), "http://localhost");
  for (const invalid of [
    "file:///tmp/provider.sock",
    "https://user:secret@provider.example/v1",
    "https://provider.example/v1?",
    "https://provider.example/v1#",
  ]) {
    assert.equal(canonicalSideChatCatalogBaseUrl(invalid), "");
  }
});

test("Side Chat catalog drops stale URL completions and never borrows a mismatched main catalog", () => {
  const ui = createUiLocalState();
  const current = state();
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.setupBaseUrl = "http://first.test/v1";
  const firstRequest = beginSideChatCatalogLoad(ui, current);
  assert.ok(firstRequest);
  draft.setupBaseUrl = "http://second.test/v1";
  const latestRequest = beginSideChatCatalogLoad(ui, current);
  assert.ok(latestRequest);

  assert.deepEqual(finishSideChatCatalogLoad(ui, current, firstRequest, {
    ownerSessionId: "session-a",
    baseUrl: "http://first.test",
    providerProfile: "openai_compatible",
    configGeneration: "7",
    models: [{ id: "stale-model", label: "Stale", loadState: "unknown" }],
  }), { catalogAccepted: false, localStateChanged: false });
  assert.deepEqual(finishSideChatCatalogLoad(ui, current, latestRequest, {
    ownerSessionId: "session-a",
    baseUrl: "http://second.test",
    providerProfile: "openai_compatible",
    configGeneration: "7",
    models: [{ id: "latest-model", label: "Latest", loadState: "loaded" }],
  }), { catalogAccepted: true, localStateChanged: true });
  assert.deepEqual(sideChatCatalogViewForState(ui, current).models.map((model) => model.id), ["latest-model"]);

  const seedUi = createUiLocalState();
  const seedState = state();
  const seedDraft = sideChatDraftForState(seedUi, seedState);
  assert.ok(seedDraft);
  seedDraft.setupBaseUrl = "http://second.test/v1";
  seedState.provider_catalog_base_url = "http://first.test";
  seedState.provider_catalog_profile = "openai_compatible";
  seedState.provider_model_ids = ["wrong-server-model"];
  seedState.provider_models = ["Wrong server model"];
  assert.deepEqual(sideChatCatalogViewForState(seedUi, seedState).models, []);

  seedState.provider_catalog_base_url = "http://second.test";
  assert.equal(sideChatCatalogViewForState(seedUi, seedState).source, "main");
  seedState.provider_catalog_profile = "lm_studio";
  assert.equal(sideChatCatalogViewForState(seedUi, seedState).source, "none");
});

test("Side Chat catalog drops an ABA setup completion", () => {
  const ui = createUiLocalState();
  const current = state();
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.setupBaseUrl = "http://first.test/v1";
  const request = beginSideChatCatalogLoad(ui, current);
  assert.ok(request);
  assert.equal(request.setupRevision, draft.setupRevision);

  draft.setupBaseUrl = "http://second.test/v1";
  draft.setupRevision += 1;
  draft.setupBaseUrl = "http://first.test/v1";
  draft.setupRevision += 1;
  assert.equal(
    sideChatCatalogViewForState(ui, current).status,
    "loading",
    "the in-flight entry remains visible when the URL returns to A",
  );

  assert.deepEqual(finishSideChatCatalogLoad(ui, current, request, {
    ownerSessionId: "session-a",
    baseUrl: "http://first.test",
    providerProfile: "openai_compatible",
    configGeneration: "7",
    models: [{ id: "stale-model", label: "Stale", loadState: "loaded" }],
  }), { catalogAccepted: false, localStateChanged: true });
  assert.equal(ui.sideChatCatalogTransaction.active, null);
  assert.equal(sideChatCatalogViewForState(ui, current).status, "error");
  assert.match(
    sideChatCatalogViewForState(ui, current).error,
    /もう一度モデル一覧を読み込んでください/,
  );
  assert.deepEqual(sideChatCatalogViewForState(ui, current).models, []);
  assert.equal(sideChatCatalogLoadOpen(ui, current), true);
});

test("an admitted Side Chat catalog result is dropped when Main Settings settlement takes ownership", () => {
  const ui = createUiLocalState();
  const current = state();
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.setupBaseUrl = "http://side.test/v1";
  const request = beginSideChatCatalogLoad(ui, current);
  assert.ok(request);

  ui.activeConfigMutationGeneration = 3n;
  assert.deepEqual(finishSideChatCatalogLoad(ui, current, request, {
    ownerSessionId: "session-a",
    baseUrl: "http://side.test",
    providerProfile: "openai_compatible",
    configGeneration: "7",
    models: [{ id: "must-not-settle", label: "Stale", loadState: "loaded" }],
  }), { catalogAccepted: false, localStateChanged: true });
  assert.equal(ui.sideChatCatalogTransaction.active, null);
  assert.deepEqual(sideChatCatalogViewForState(ui, current).models, []);
});

test("Side Chat catalog response must match the admitted owner and config generation", () => {
  for (const mismatch of [
    { ownerSessionId: "session-b", configGeneration: "7" },
    { ownerSessionId: "session-a", configGeneration: "8" },
  ]) {
    const ui = createUiLocalState();
    const current = state();
    const draft = sideChatDraftForState(ui, current);
    assert.ok(draft);
    draft.setupBaseUrl = "http://side.test/v1";
    const request = beginSideChatCatalogLoad(ui, current);
    assert.ok(request);

    assert.deepEqual(finishSideChatCatalogLoad(ui, current, request, {
      ownerSessionId: mismatch.ownerSessionId,
      baseUrl: "http://side.test",
      providerProfile: "openai_compatible",
      configGeneration: mismatch.configGeneration,
      models: [{ id: "wrong-owner", label: "Wrong owner", loadState: "loaded" }],
    }), { catalogAccepted: false, localStateChanged: true });
    const rejected = sideChatCatalogViewForState(ui, current);
    assert.equal(rejected.status, "error");
    assert.deepEqual(rejected.models, []);
    assert.equal(sideChatCatalogLoadOpen(ui, current), true);
    assert.equal(ui.sideChatCatalogTransaction.active, null);
  }
});

test("idle side configuration can update the selected durable owner with its explicit profile", async () => {
  const ui = createUiLocalState();
  const current = state();
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.setupBaseUrl = "http://side.test/v1";
  draft.setupModel = "gemma-explicit";
  draft.setupProviderProfile = "openai_responses";
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => { calls.push({ name, args }); },
    rerender: () => undefined,
  } as unknown as ActionContext;

  const configure = actionById("configure-side-chat");
  assert.ok(configure);
  await configure.run(current, context, { index: -1, value: "" });

  assert.deepEqual(calls, [{
    name: "configure_side_chat",
    args: {
      ownerSessionId: "session-a",
      baseUrl: "http://side.test/v1",
      model: "gemma-explicit",
      providerProfile: "openai_responses",
      expectedConfigGeneration: "7",
    },
  }]);
});

test("invalid Side provider settings never reach the configure command", async () => {
  const configure = actionById("configure-side-chat");
  assert.ok(configure);
  for (const [baseUrl, model] of [
    ["https://user:secret@side.test/v1", "gemma-explicit"],
    ["https://side.test/v1?token=hidden", "gemma-explicit"],
    ["https://side.test/v1#hidden", "gemma-explicit"],
    ["http://side.test/v1", "   "],
  ]) {
    const ui = createUiLocalState();
    const current = state();
    const draft = sideChatDraftForState(ui, current);
    assert.ok(draft);
    draft.setupBaseUrl = baseUrl;
    draft.setupModel = model;
    let calls = 0;
    const context = {
      uiState: ui,
      getProjection: () => current,
      getViewState: () => current,
      mutate: async () => { calls += 1; },
      rerender: () => undefined,
    } as unknown as ActionContext;

    await configure.run(current, context, { index: -1, value: "" });
    assert.equal(calls, 0, `${baseUrl} / ${model}`);
  }
});

test("accepted side configuration rebases the local setup and draft CAS owner from the canonical projection", async () => {
  const ui = createUiLocalState();
  let current = state({
    base_url: "http://old-side.test/v1",
    model: "old-model",
    draft_text: "durable before configure",
    draft_revision: "7",
  });
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.setupBaseUrl = " HTTP://SIDE.TEST:80/v1/ ";
  draft.setupModel = " gemma-explicit ";
  draft.setupProviderProfile = "lm_studio_chat_completions";
  draft.text = "local unsaved question";
  draft.revision += 1;
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      calls.push({ name, args });
      current = state({
        base_url: "http://side.test/v1",
        model: "gemma-explicit",
        provider_profile: "lm_studio_chat_completions",
        draft_text: "durable after configure",
        draft_revision: "9",
      });
    },
    rerender: () => undefined,
  } as unknown as ActionContext;

  const configure = actionById("configure-side-chat");
  assert.ok(configure);
  await configure.run(current, context, { index: -1, value: "" });

  assert.deepEqual(calls, [{
    name: "configure_side_chat",
    args: {
      ownerSessionId: "session-a",
      baseUrl: "HTTP://SIDE.TEST:80/v1/",
      model: "gemma-explicit",
      providerProfile: "lm_studio_chat_completions",
      expectedConfigGeneration: "7",
    },
  }]);
  const settled = sideChatDraftForState(ui, current);
  assert.ok(settled);
  assert.equal(settled.setupBaseUrl, "http://side.test/v1");
  assert.equal(settled.setupModel, "gemma-explicit");
  assert.equal(settled.setupProviderProfile, "lm_studio_chat_completions");
  assert.equal(settled.persistedText, "durable after configure");
  assert.equal(settled.persistedRevision, "9");
  assert.equal(settled.text, "local unsaved question");
});

test("a configure conflict from a newer Main config generation preserves the Side setup draft", async () => {
  const ui = createUiLocalState();
  let current = state({
    base_url: "http://old-side.test/v1",
    model: "old-model",
  });
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.setupBaseUrl = "http://requested-side.test/v1/";
  draft.setupModel = "requested-model";
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async () => {
      current = {
        ...state({ base_url: "http://old-side.test/v1", model: "old-model" }),
        config_target: {
          workspacePath: "C:/workspace",
          sessionId: "session-a",
          configGeneration: "8",
        },
      };
    },
    rerender: () => undefined,
  } as unknown as ActionContext;

  const configure = actionById("configure-side-chat");
  assert.ok(configure);
  await configure.run(current, context, { index: -1, value: "" });

  assert.equal(draft.setupBaseUrl, "http://requested-side.test/v1/");
  assert.equal(draft.setupModel, "requested-model");
});

test("running, deleting, and local mutation states guard side provider reconfiguration", async () => {
  const configure = actionById("configure-side-chat");
  assert.ok(configure);
  const model = (view: DesktopViewState) => createDesktopRenderModel(view, {
    ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
    sideChat: {
      ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.sideChat,
      setupBaseUrl: "http://127.0.0.1:1234/v1",
      setupModel: "gemma-replacement",
      operationsOpen: true,
    },
  });
  assert.equal(configure.enabled(model(state()), { index: -1, value: "" }), true);
  assert.equal(configure.enabled(model(state({ status: "running", can_send: false, can_cancel: true })), { index: -1, value: "" }), false);
  assert.equal(configure.enabled(model(state({ deleting: true })), { index: -1, value: "" }), false);

  const ui = createUiLocalState();
  const current = state();
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.setupModel = "gemma-replacement";
  ui.sideChatMutations.set("session-a", { kind: "send", chatId: "side-a", generation: "4" });
  let calls = 0;
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async () => { calls += 1; },
    rerender: () => undefined,
  } as unknown as ActionContext;

  await configure.run(current, context, { index: -1, value: "" });
  assert.equal(calls, 0);
});

test("a Main Settings transaction blocks every Side Chat mutation owner", async () => {
  const ui = createUiLocalState();
  const current = state({ can_send: true, can_cancel: true });
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.setupBaseUrl = "http://replacement.test/v1";
  draft.setupModel = "gemma-replacement";
  draft.text = "must remain local";
  draft.revision += 1;
  ui.activeConfigMutationGeneration = 12n;
  assert.equal(sideChatOperationsOpen(ui), false);

  const calls: string[] = [];
  let catalogLoads = 0;
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string) => { calls.push(name); },
    loadSideChatModels: async () => {
      catalogLoads += 1;
      throw new Error("must not load");
    },
    rerender: () => undefined,
  } as unknown as ActionContext;

  for (const id of [
    "load-side-chat-models",
    "configure-side-chat",
    "send-side-chat",
    "cancel-side-chat",
    "request-delete-side-chat",
  ]) {
    const action = actionById(id);
    assert.ok(action);
    await action.run(current, context, { index: -1, value: "" });
  }
  await persistSideChatDraft(current, context);
  ui.sideChatDeleteConfirmation = {
    ownerSessionId: "session-a",
    chatId: "side-a",
    expectedGeneration: "4",
  };
  for (const id of ["cancel-delete-side-chat", "confirm-delete-side-chat"]) {
    const action = actionById(id);
    assert.ok(action);
    await action.run(current, context, { index: -1, value: "" });
  }

  assert.deepEqual(calls, []);
  assert.equal(catalogLoads, 0);
  assert.equal(draft.text, "must remain local");
  assert.deepEqual(ui.sideChatDeleteConfirmation, {
    ownerSessionId: "session-a",
    chatId: "side-a",
    expectedGeneration: "4",
  });

  ui.activeConfigMutationGeneration = null;
  ui.externalConfigMutationPending = true;
  assert.equal(sideChatOperationsOpen(ui), false);
});

test("a dirty or invalid Main draft leaves the independent Side provider owner open", async () => {
  const ui = createUiLocalState();
  ui.configDirty = true;
  ui.configDraftValues.set("model.request_timeout_ms", "0");
  const current = state();
  current.config_fields = [{
    key: "model.request_timeout_ms",
    value: "0",
    env_override: "MOYAI_REQUEST_TIMEOUT_MS",
    value_type: "integer",
    required: true,
    min_value: 1,
    max_value: 3_600_000,
    options: [],
  }];
  current.config_draft = {
    ...current.config_draft,
    dirty: true,
    commit_enabled: false,
    external_owner_mutation_open: false,
    access_mode_mutation_enabled: false,
  };
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.setupBaseUrl = "http://replacement.test/v1";
  draft.setupModel = "gemma-replacement";

  assert.equal(sideChatOperationsOpen(ui), true);
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  await actionById("configure-side-chat")?.run(current, {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => { calls.push({ name, args }); },
    rerender: () => undefined,
  } as unknown as ActionContext, { index: -1, value: "" });
  assert.deepEqual(calls, [{
    name: "configure_side_chat",
    args: {
      ownerSessionId: "session-a",
      baseUrl: "http://replacement.test/v1",
      model: "gemma-replacement",
      providerProfile: "openai_compatible",
      expectedConfigGeneration: "7",
    },
  }]);

  const renderer = useSidePane({
    baseUrl: "http://replacement.test/v1",
    model: "gemma-replacement",
    configDirty: true,
    operationsOpen: true,
  });
  const html = renderer.overlay({ ...current, overlay: "config" });
  assert.match(
    html,
    /data-action="configure-side-chat" aria-disabled="false"(?![^>]*\sdisabled(?:\s|>|=))[^>]*>/,
  );
});

test("Settings renders side provider controls disabled while running, deleting, or mutating", () => {
  const renderer = useSidePane({ baseUrl: "http://replacement.test/v1", model: "gemma-replacement" });
  const running = renderer.overlay({
    ...state({ status: "running", can_send: false, can_cancel: true }),
    overlay: "config",
  });
  assert.match(running, /id="side-chat-base-url"[^>]*disabled/);
  assert.match(running, /id="side-chat-model"[^>]*disabled/);
  assert.match(running, /data-action="configure-side-chat"[^>]*disabled/);
  assert.match(running, /実行中は設定を変更できません/);

  const deleting = renderer.overlay({ ...state({ deleting: true, can_send: false }), overlay: "config" });
  assert.match(deleting, /id="side-chat-base-url"[^>]*disabled/);
  assert.match(deleting, /サイドチャットを削除しています/);

  const mutatingRenderer = useSidePane({
    baseUrl: "http://replacement.test/v1",
    model: "gemma-replacement",
    pending: true,
  });
  const mutating = mutatingRenderer.overlay({ ...state(), overlay: "config" });
  assert.match(mutating, /id="side-chat-base-url"[^>]*disabled/);
  assert.match(mutating, /data-action="configure-side-chat"[^>]*disabled/);
  assert.match(mutating, /サイドチャット設定を更新しています/);
});

test("Main Settings settlement disables Side settings, composer, Stop, and delete controls", () => {
  const renderer = useSidePane({
    draft: "wait for Main Settings",
    baseUrl: "http://replacement.test/v1/",
    model: "gemma-replacement",
    catalogLoadEnabled: true,
    configPending: true,
    configDraftEditOpen: false,
  });
  const current = state({ can_send: true, can_cancel: true });
  const settings = renderer.overlay({ ...current, overlay: "config" });
  assert.match(settings, /id="settings-side-chat"[^>]*aria-busy="true"/);
  assert.match(settings, /data-action="load-side-chat-models"[^>]*disabled/);
  assert.match(settings, /id="side-chat-base-url"[^>]*disabled/);
  assert.match(settings, /id="side-chat-model"[^>]*disabled/);
  assert.match(settings, /id="side-chat-model-manual"[^>]*disabled/);
  assert.match(settings, /data-action="configure-side-chat"[^>]*disabled/);
  assert.match(settings, /メインLLM設定の処理が完了するまで/);

  const pane = renderer.artifactPane(current);
  assert.match(pane, /data-action="request-delete-side-chat"[^>]*disabled/);
  assert.match(pane, /id="side-chat-prompt"[^>]*disabled/);
  assert.match(pane, /data-action="cancel-side-chat"[^>]*disabled/);
  assert.match(pane, /data-action="send-side-chat"[^>]*disabled/);

  const confirmationRenderer = useSidePane({
    draft: "wait for Main Settings",
    configPending: true,
    configDraftEditOpen: false,
    confirmingDelete: true,
  });
  const confirmation = confirmationRenderer.deleteConfirmation(current);
  assert.match(confirmation, /data-action="cancel-delete-side-chat" autofocus disabled/);
  assert.match(confirmation, /data-action="confirm-delete-side-chat" disabled/);
});

test("an unrelated Rust config-edit capability does not block independent Side Chat controls", () => {
  const renderer = useSidePane({
    draft: "independent side question",
    configDraftEditOpen: false,
    operationsOpen: true,
  });
  const pane = renderer.artifactPane(state({ can_send: true, can_cancel: true }));
  const prompt = pane.match(/<textarea id="side-chat-prompt"[^>]*>/)?.[0] ?? "";
  const send = pane.match(/<button class="send" data-action="send-side-chat"[^>]*>/)?.[0] ?? "";
  const stop = pane.match(/<button data-action="cancel-side-chat"[^>]*>/)?.[0] ?? "";
  const remove = pane.match(/<button class="pin danger-pin"[^>]*>/)?.[0] ?? "";
  for (const control of [prompt, send, stop, remove]) {
    assert.ok(control);
    assert.doesNotMatch(control, /disabled/);
  }
});

test("configured side chat renders its transcript and exposes the modal delete owner", () => {
  const renderer = useSidePane({ draft: "この引用を説明して", confirmingDelete: true });
  const current = state({
    status: "running",
    phase: "generating",
    can_cancel: true,
    messages: [
      { id: "m1", sequence_no: 1, role: "user", content: "質問 <unsafe>" },
      { id: "m2", sequence_no: 2, role: "assistant", content: "**回答**" },
    ],
  });
  const html = renderer.artifactPane(current);
  const confirmation = renderer.deleteConfirmation(current);

  assert.match(html, /role="log" aria-label="サイドチャット履歴"/);
  assert.match(html, /data-side-chat-message-id="m1"/);
  assert.match(html, /質問 &lt;unsafe&gt;/);
  assert.match(html, /<strong>回答<\/strong>/);
  assert.match(html, /id="side-chat-prompt"[^>]*>この引用を説明して<\/textarea>/);
  assert.match(html, /data-action="send-side-chat"/);
  assert.match(html, /data-action="cancel-side-chat"/);
  assert.match(html, /data-action="request-delete-side-chat"[^>]*aria-haspopup="dialog"[^>]*aria-controls="side-chat-delete-dialog"[^>]*aria-expanded="true"/);
  assert.doesNotMatch(html, /role="alertdialog"/);
  assert.doesNotMatch(html, /data-action="confirm-delete-side-chat"/);
  assert.doesNotMatch(html, /data-action="cancel-run"/);

  assert.match(confirmation, /class="modal-backdrop"[^>]*data-local-modal="side-chat-delete"/);
  assert.match(confirmation, /id="side-chat-delete-dialog"[^>]*data-modal[^>]*role="alertdialog"[^>]*aria-modal="true"/);
  assert.match(confirmation, /aria-labelledby="side-chat-delete-title"/);
  assert.match(confirmation, /aria-describedby="side-chat-delete-detail"/);
  assert.match(confirmation, /data-action="cancel-delete-side-chat" autofocus/);
  assert.match(confirmation, /data-action="confirm-delete-side-chat"/);
  assert.doesNotMatch(confirmation, /id="side-chat-delete-status"/);
  assert.ok(
    confirmation.indexOf('data-action="cancel-delete-side-chat"')
      < confirmation.indexOf('data-action="confirm-delete-side-chat"'),
    "the safe Cancel action must precede destructive confirmation in DOM order",
  );
});

test("side delete modal owns pending settlement and disables cancellation and confirmation", () => {
  const renderer = useSidePane({ confirmingDelete: true, pending: true });
  const confirmation = renderer.deleteConfirmation(state({ status: "completed" }));

  assert.match(confirmation, /role="alertdialog"[^>]*aria-busy="true"/);
  assert.match(confirmation, /id="side-chat-delete-status"[^>]*role="status"[^>]*tabindex="-1"[^>]*>削除を確定しています…/);
  assert.match(confirmation, /data-action="cancel-delete-side-chat" autofocus disabled/);
  assert.match(confirmation, /data-action="confirm-delete-side-chat" disabled/);
});

test("durable deletion pending is explicit and disables every side-chat mutation", () => {
  const renderer = useSidePane({ draft: "未送信の質問", confirmingDelete: true });
  const html = renderer.artifactPane(state({
    deleting: true,
    status: "running",
    phase: "cancelling",
    can_send: true,
    can_cancel: true,
  }));

  assert.match(html, /aria-busy="true"/);
  assert.match(html, /サイドチャットを削除しています/);
  assert.match(html, /削除処理中/);
  assert.match(html, /data-action="request-delete-side-chat"[^>]*disabled/);
  assert.match(html, /id="side-chat-prompt"[^>]*disabled/);
  assert.match(html, /data-action="cancel-side-chat"[^>]*disabled/);
  assert.match(html, /data-action="send-side-chat"[^>]*disabled/);
  assert.doesNotMatch(html, /role="alertdialog"/);
  assert.doesNotMatch(html, /data-action="confirm-delete-side-chat"/);
});

test("durable deletion keeps the unconfigured right pane read-only", () => {
  const renderer = useSidePane({ baseUrl: "http://side.test/v1", model: "gemma" });
  const html = renderer.artifactPane(state({
    configured: false,
    deleting: true,
    chat_id: null,
    generation: "0",
    can_send: false,
  }));

  assert.doesNotMatch(html, /id="side-chat-base-url"/);
  assert.doesNotMatch(html, /id="side-chat-model"/);
  assert.match(html, /data-action="show-config"[^>]*disabled/);
  assert.match(html, /サイドチャットを削除しています/);
});

test("delete confirmation is stale once durable deletion starts or the binding disappears", () => {
  const confirmation = {
    ownerSessionId: "session-a",
    chatId: "side-a",
    expectedGeneration: "4",
  };

  assert.equal(sideChatDeleteConfirmationStillTargets(confirmation, state()), true);
  assert.equal(sideChatDeleteConfirmationStillTargets(confirmation, state({ deleting: true })), false);
  assert.equal(sideChatDeleteConfirmationStillTargets(confirmation, state({ chat_id: null })), false);
});

test("side drafts remain isolated by owner and reset for a replacement chat identity", () => {
  const ui = createUiLocalState();
  const a = state();
  const aDraft = sideChatDraftForState(ui, a);
  assert.ok(aDraft);
  aDraft.text = "draft A";
  aDraft.revision += 1;

  const b = state({ chat_id: "side-b" }, "session-b");
  const bDraft = sideChatDraftForState(ui, b);
  assert.ok(bDraft);
  bDraft.text = "draft B";

  assert.equal(sideChatDraftForState(ui, a)?.text, "draft A");
  assert.equal(sideChatDraftForState(ui, b)?.text, "draft B");
  assert.equal(sideChatDraftForState(ui, state({ chat_id: "side-a-replacement" }))?.text, "");
});

test("a durable side draft hydrates after restart and a dirty local edit is not overwritten", () => {
  const ui = createUiLocalState();
  const restored = state({ draft_text: "restart draft", draft_revision: "7" });
  const draft = sideChatDraftForState(ui, restored);
  assert.ok(draft);
  assert.equal(draft.text, "restart draft");
  assert.equal(draft.persistedRevision, "7");

  draft.text = "unsaved local edit";
  draft.revision += 1;
  const externallyChanged = state({ draft_text: "other owner", draft_revision: "8" });
  assert.equal(sideChatDraftForState(ui, externallyChanged)?.text, "unsaved local edit");
  assert.equal(draft.persistedRevision, "7");
});

test("side draft persistence uses exact owner, chat, and draft revision", async () => {
  const ui = createUiLocalState();
  let current = state({ draft_text: "old", draft_revision: "7" });
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.text = "new durable draft";
  draft.revision += 1;
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      calls.push({ name, args });
      current = state({ draft_text: "new durable draft", draft_revision: "8" });
    },
    rerender: () => undefined,
  } as unknown as ActionContext;

  await persistSideChatDraft(current, context);

  assert.deepEqual(calls, [{
    name: "save_side_chat_draft",
    args: {
      ownerSessionId: "session-a",
      chatId: "side-a",
      expectedDraftRevision: "7",
      text: "new durable draft",
    },
  }]);
  assert.equal(draft.persistedText, "new durable draft");
  assert.equal(draft.persistedRevision, "8");
  assert.equal(draft.saveInFlight, false);
});

test("a stale durable draft CAS never replaces the losing local text", async () => {
  const ui = createUiLocalState();
  let current = state({ draft_text: "baseline", draft_revision: "7" });
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.text = "my unsent question";
  draft.revision += 1;
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async () => {
      current = state({ draft_text: "other process", draft_revision: "8" });
    },
    rerender: () => undefined,
  } as unknown as ActionContext;

  await persistSideChatDraft(current, context);

  assert.equal(draft.text, "my unsent question");
  assert.equal(draft.persistedText, "other process");
  assert.equal(draft.persistedRevision, "8");
  assert.equal(draft.saveInFlight, false);
});

test("opening side chat is frontend-local and leaves the main composer untouched", async () => {
  const ui = createUiLocalState();
  ui.drafts.prompt = "main draft";
  ui.artifactPaneCollapsed = true;
  let mutations = 0;
  const context = {
    uiState: ui,
    mutate: async () => { mutations += 1; },
    rerender: () => undefined,
  } as unknown as ActionContext;

  const action = actionById("show-side-chat-pane");
  assert.ok(action);
  await action.run(state(), context, { index: -1, value: "" });

  assert.equal(ui.artifactPaneMode, "side_chat");
  assert.equal(ui.artifactPaneCollapsed, false);
  assert.equal(ui.drafts.prompt, "main draft");
  assert.equal(mutations, 0);
});

test("Ctrl+Enter targets the focused side composer without changing other global shortcuts", () => {
  const ctrlEnter = { key: "Enter", ctrlKey: true, metaKey: false, repeat: false };
  assert.equal(shortcutActionForComposer(ctrlEnter, false), "send");
  assert.equal(shortcutActionForComposer(ctrlEnter, true), "send-side-chat");
  assert.equal(
    shortcutActionForComposer({ key: "n", ctrlKey: true, metaKey: false, repeat: false }, true),
    "new-chat",
  );
});

test("side send and Stop use only the exact side owner, chat, and generation", async () => {
  const ui = createUiLocalState();
  ui.drafts.prompt = "main draft";
  let current = state({ can_cancel: true });
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.text = "  side question  ";
  draft.revision = 2;
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      calls.push({ name, args });
      if (name === "submit_side_chat") {
        current = state({
          status: "running",
          generation: "5",
          can_send: false,
          can_cancel: true,
          messages: [],
        });
      }
    },
    rerender: () => undefined,
  } as unknown as ActionContext;

  const send = actionById("send-side-chat");
  assert.ok(send);
  await send.run(current, context, { index: -1, value: "" });
  assert.deepEqual(calls[0], {
    name: "submit_side_chat",
    args: {
      ownerSessionId: "session-a",
      chatId: "side-a",
      expectedGeneration: "4",
      expectedDraftRevision: "0",
      text: "side question",
    },
  });
  assert.equal(sideChatDraftForState(ui, current)?.text, "");
  assert.equal(ui.drafts.prompt, "main draft");

  const stop = actionById("cancel-side-chat");
  assert.ok(stop);
  await stop.run(current, context, { index: -1, value: "" });
  assert.deepEqual(calls[1], {
    name: "cancel_side_chat",
    args: {
      ownerSessionId: "session-a",
      chatId: "side-a",
      expectedGeneration: "5",
    },
  });
  assert.ok(calls.every((call) => call.name !== "submit_prompt" && call.name !== "cancel_run"));
});

test("side send retains a retryable draft when the exact generation was not admitted", async () => {
  const ui = createUiLocalState();
  const current = state();
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.text = "retry side";
  draft.revision = 1;
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async () => undefined,
    rerender: () => undefined,
  } as unknown as ActionContext;

  const send = actionById("send-side-chat");
  assert.ok(send);
  await send.run(current, context, { index: -1, value: "" });

  assert.equal(sideChatDraftForState(ui, current)?.text, "retry side");
});

test("side Send waits for queued autosaves and submits the settled draft revision", async () => {
  const ui = createUiLocalState();
  let current = state({ draft_text: "baseline", draft_revision: "7" });
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.text = "first edit";
  draft.revision = 1;
  let releaseFirstSave: (() => void) | null = null;
  const firstSaveGate = new Promise<void>((resolve) => { releaseFirstSave = resolve; });
  let durableRevision = 7;
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      calls.push({ name, args });
      if (name === "save_side_chat_draft") {
        if (durableRevision === 7) await firstSaveGate;
        durableRevision += 1;
        current = state({
          draft_text: String(args?.text ?? ""),
          draft_revision: String(durableRevision),
        });
      } else if (name === "submit_side_chat") {
        current = state({
          draft_text: "",
          draft_revision: String(durableRevision + 1),
          status: "running",
          generation: "5",
          can_send: false,
          can_cancel: true,
        });
      }
    },
    rerender: () => undefined,
  } as unknown as ActionContext;

  const firstSave = persistSideChatDraft(current, context);
  await Promise.resolve();
  draft.text = "final question";
  draft.revision = 2;
  const queuedSave = persistSideChatDraft(current, context);
  const send = actionById("send-side-chat");
  assert.ok(send);
  const sending = send.run(current, context, { index: -1, value: "" });
  await Promise.resolve();
  assert.deepEqual(calls.map((call) => call.name), ["save_side_chat_draft"]);

  assert.ok(releaseFirstSave);
  releaseFirstSave();
  await Promise.all([firstSave, queuedSave, sending]);

  assert.deepEqual(calls.map((call) => call.name), [
    "save_side_chat_draft",
    "save_side_chat_draft",
    "submit_side_chat",
  ]);
  assert.deepEqual(calls[2], {
    name: "submit_side_chat",
    args: {
      ownerSessionId: "session-a",
      chatId: "side-a",
      expectedGeneration: "4",
      expectedDraftRevision: "9",
      text: "final question",
    },
  });
});

test("side Send keeps local text when the draft revision CAS is rejected", async () => {
  const ui = createUiLocalState();
  let current = state({ draft_text: "baseline", draft_revision: "7" });
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.text = "keep this question";
  draft.revision = 1;
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      calls.push({ name, args });
      current = state({ draft_text: "other writer", draft_revision: "8" });
    },
    rerender: () => undefined,
  } as unknown as ActionContext;

  const send = actionById("send-side-chat");
  assert.ok(send);
  await send.run(current, context, { index: -1, value: "" });

  assert.deepEqual(calls, [{
    name: "submit_side_chat",
    args: {
      ownerSessionId: "session-a",
      chatId: "side-a",
      expectedGeneration: "4",
      expectedDraftRevision: "7",
      text: "keep this question",
    },
  }]);
  assert.equal(draft.text, "keep this question");
  assert.equal(draft.persistedText, "other writer");
  assert.equal(draft.persistedRevision, "8");
  assert.equal(current.side_chat.generation, "4");
});

test("side delete requires an exact local confirmation before mutation", async () => {
  const ui = createUiLocalState();
  let current = state();
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      calls.push({ name, args });
      current = state({ chat_id: null, owner_session_id: "session-a", generation: "5", messages: [] });
    },
    rerender: () => undefined,
  } as unknown as ActionContext;

  const request = actionById("request-delete-side-chat");
  const confirm = actionById("confirm-delete-side-chat");
  assert.ok(request && confirm);
  await request.run(current, context, { index: -1, value: "" });
  assert.equal(calls.length, 0);
  assert.deepEqual(ui.sideChatDeleteConfirmation, {
    ownerSessionId: "session-a",
    chatId: "side-a",
    expectedGeneration: "4",
  });

  await confirm.run(current, context, { index: -1, value: "" });
  assert.deepEqual(calls, [{
    name: "delete_side_chat",
    args: {
      ownerSessionId: "session-a",
      chatId: "side-a",
      expectedGeneration: "4",
    },
  }]);
  assert.equal(ui.sideChatDeleteConfirmation, null);
});

test("side delete cancellation keeps the exact target and cannot cancel an admitted mutation", async () => {
  const ui = createUiLocalState();
  const current = state();
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async () => undefined,
    rerender: () => undefined,
  } as unknown as ActionContext;
  const request = actionById("request-delete-side-chat");
  const cancel = actionById("cancel-delete-side-chat");
  assert.ok(request && cancel);

  await request.run(current, context, { index: -1, value: "" });
  const exactConfirmation = ui.sideChatDeleteConfirmation;
  assert.ok(exactConfirmation);
  ui.sideChatMutations.set("session-a", {
    kind: "delete",
    chatId: "side-a",
    generation: "4",
  });
  await cancel.run(current, context, { index: -1, value: "" });
  assert.deepEqual(ui.sideChatDeleteConfirmation, exactConfirmation);

  ui.sideChatMutations.clear();
  await cancel.run(state({ generation: "5" }), context, { index: -1, value: "" });
  assert.deepEqual(ui.sideChatDeleteConfirmation, exactConfirmation);

  await cancel.run(current, context, { index: -1, value: "" });
  assert.equal(ui.sideChatDeleteConfirmation, null);
});

test("accepted active deletion clears confirmation while its durable tombstone remains projected", async () => {
  const ui = createUiLocalState();
  let current = state({ status: "running", can_send: false, can_cancel: true });
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.text = "削除前の未送信 draft";
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      calls.push({ name, args });
      current = state({
        deleting: true,
        status: "running",
        phase: "cancelling",
        can_send: false,
        can_cancel: false,
      });
    },
    rerender: () => undefined,
  } as unknown as ActionContext;

  const request = actionById("request-delete-side-chat");
  const confirm = actionById("confirm-delete-side-chat");
  assert.ok(request && confirm);
  await request.run(current, context, { index: -1, value: "" });
  await confirm.run(current, context, { index: -1, value: "" });

  assert.deepEqual(calls, [{
    name: "delete_side_chat",
    args: {
      ownerSessionId: "session-a",
      chatId: "side-a",
      expectedGeneration: "4",
    },
  }]);
  assert.equal(ui.sideChatDeleteConfirmation, null);
  assert.equal(sideChatDraftForState(ui, current)?.text, "削除前の未送信 draft");
});

test("deleting projection closes side-chat action admission even with stale capabilities", async () => {
  const ui = createUiLocalState();
  const current = state({ deleting: true, can_send: true, can_cancel: true });
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  draft.text = "送信してはいけない";
  ui.sideChatDeleteConfirmation = {
    ownerSessionId: "session-a",
    chatId: "side-a",
    expectedGeneration: "4",
  };
  const calls: string[] = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string) => { calls.push(name); },
    rerender: () => undefined,
  } as unknown as ActionContext;

  for (const id of [
    "send-side-chat",
    "cancel-side-chat",
    "request-delete-side-chat",
    "confirm-delete-side-chat",
  ]) {
    const action = actionById(id);
    assert.ok(action);
    assert.equal(
      action.enabled(
        createDesktopRenderModel(current, DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION),
        { index: -1, value: "" },
      ),
      false,
      id,
    );
    await action.run(current, context, { index: -1, value: "" });
  }
  await persistSideChatDraft(current, context);

  const unconfiguredDeleting = state({ configured: false, deleting: true, chat_id: null });
  const configure = actionById("configure-side-chat");
  assert.ok(configure);
  assert.equal(
    configure.enabled(
      createDesktopRenderModel(unconfiguredDeleting, DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION),
      { index: -1, value: "" },
    ),
    false,
  );
  await configure.run(unconfiguredDeleting, context, { index: -1, value: "" });

  assert.deepEqual(calls, []);
});
