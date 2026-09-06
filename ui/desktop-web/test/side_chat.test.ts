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
import { renderTranscriptRows } from "../src/render_transcript.ts";
import {
  createDesktopRenderModel,
  DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
  type DesktopRenderLocalPresentation,
} from "../src/render_projection.ts";
import type {
  ConfigFieldProjection,
  DesktopViewState,
  SideChatCatalogResult,
  SideChatPendingQuote,
  SideChatProjection,
} from "../src/types.ts";
import { updateConfigDraftValue } from "../src/config_mutation.ts";
import {
  pendingSideChatQuoteFromSelection,
  sideChatQuoteKeyboardActivation,
} from "../src/side_chat_quote.ts";
import {
  appendQuoteToSideChatDraft,
  beginSideChatCatalogLoad,
  canonicalSideChatCatalogBaseUrl,
  canonicalSideChatProviderBaseUrl,
  createUiLocalState,
  finishSideChatCatalogLoad,
  recordSideChatCatalogConfigEdit,
  sideChatCatalogLoadOpen,
  sideChatCatalogViewForState,
  sideChatDeleteConfirmationStillTargets,
  sideChatDraftForState,
  sideChatModelOptions,
  sideChatOperationsOpen,
  updateSideChatDraftFromManualEdit,
  type SideChatCatalogView,
} from "../src/ui_state.ts";

function sideChat(overrides: Partial<SideChatProjection> = {}): SideChatProjection {
  return {
    configured: true,
    deleting: false,
    chat_id: "side-a",
    owner_session_id: "session-a",
    model: "gemma-test",
    system_prompt: "",
    base_url: "http://127.0.0.1:1234/v1",
    provider_profile: "openai_compatible",
    status: "idle",
    phase: "idle",
    last_error: "",
    generation: "4",
    draft_text: "",
    draft_quote: null,
    draft_revision: "0",
    context_scope: "owner_session",
    context_as_of_append_position: "42",
    context_truncated: false,
    messages: [],
    can_send: true,
    can_cancel: false,
    ...overrides,
  };
}

function configField(
  key: string,
  value: string,
  valueType: ConfigFieldProjection["value_type"] = "string",
  overrides: Partial<ConfigFieldProjection> = {},
): ConfigFieldProjection {
  return {
    key,
    value,
    sensitive: false,
    configured: true,
    env_override: null,
    value_type: valueType,
    required: true,
    min_value: null,
    max_value: null,
    options: [],
    ...overrides,
  };
}

function sideChatConfigFields(overrides: {
  baseUrl?: string;
  model?: string;
  systemPrompt?: string;
  providerProfile?: string;
} = {}): ConfigFieldProjection[] {
  return [
    configField("side_chat.base_url", overrides.baseUrl ?? "http://127.0.0.1:1234/v1"),
    configField("side_chat.model", overrides.model ?? "gemma-test"),
    configField(
      "side_chat.provider_profile",
      overrides.providerProfile ?? "openai_compatible",
      "enum",
      { options: ["lm_studio", "openai_compatible", "openai_responses", "lm_studio_chat_completions"] },
    ),
    configField("side_chat.system_prompt", overrides.systemPrompt ?? "", "string", { required: false }),
    configField("side_chat.context_window", "32768", "integer", { min_value: 1, max_value: 4_294_967_295 }),
    configField("side_chat.request_timeout_ms", "120000", "integer", { min_value: 1, max_value: 3_600_000 }),
    configField("side_chat.connect_timeout_ms", "10000", "integer", { min_value: 0 }),
    configField("side_chat.max_retries", "2", "integer", { min_value: 0, max_value: 255 }),
  ];
}

function projectedDraftQuote(
  quote: SideChatPendingQuote | null | undefined,
): SideChatProjection["draft_quote"] {
  return quote == null ? null : {
    source_kind: quote.sourceKind,
    source_history_item_id: quote.sourceHistoryItemId,
    source_append_position: quote.sourceAppendPosition,
    selected_text: quote.selectedText,
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
    config_fields: sideChatConfigFields(),
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
    config_draft_capabilities: {
      clean: {
        dirty: false,
        edit_enabled: true,
        discard_enabled: false,
        commit_enabled: false,
        external_owner_mutation_open: true,
        access_mode_mutation_enabled: true,
      },
      dirty: {
        dirty: true,
        edit_enabled: true,
        discard_enabled: true,
        commit_enabled: true,
        external_owner_mutation_open: false,
        access_mode_mutation_enabled: false,
      },
    },
    session_settings: {
      available: true,
      base_url: "http://127.0.0.1:1234/v1",
      model: "gemma-test",
      provider_profile: "openai_compatible",
      api_key_env: "",
      access_mode: "default",
      context_window: "32768",
      context_window_inherited: true,
      provider_mutation_enabled: true,
      access_mutation_enabled: true,
      unavailable_reason: "",
      target: {
        workspacePath: "C:/workspace",
        rootSessionId: ownerSessionId,
        settingsRevision: "1",
        configGeneration: "7",
        runtimeOwnerToken: "runtime-1",
      },
    },
  } as DesktopViewState;
}

function useSidePane(overrides: {
  draft?: string;
  baseUrl?: string;
  providerProfile?: "lm_studio" | "openai_compatible" | "openai_responses" | "lm_studio_chat_completions";
  model?: string;
  systemPrompt?: string;
  pending?: boolean;
  confirmingDelete?: boolean;
  catalog?: SideChatCatalogView;
  catalogLoadEnabled?: boolean;
  configPending?: boolean;
  configDirty?: boolean;
  configDraftEditOpen?: boolean;
  operationsOpen?: boolean;
  pendingQuote?: SideChatPendingQuote | null;
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
      pendingQuote: overrides.pendingQuote ?? null,
      catalog: overrides.catalog ?? DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.sideChat.catalog,
      catalogLoadEnabled: overrides.catalogLoadEnabled ?? false,
      mutationPending: overrides.pending ?? false,
      operationsOpen: overrides.operationsOpen ?? !(overrides.configPending ?? false),
      deleteConfirmation: overrides.confirmingDelete
        ? { ownerSessionId: "session-a", chatId: "side-a", expectedGeneration: "4" }
        : null,
    },
  };
  const withGlobalSideConfig = (view: DesktopViewState): DesktopViewState => {
    if (
      overrides.baseUrl === undefined
      && overrides.model === undefined
      && overrides.systemPrompt === undefined
      && overrides.providerProfile === undefined
    ) return view;
    const replacements = new Map(sideChatConfigFields({
      baseUrl: overrides.baseUrl,
      model: overrides.model,
      systemPrompt: overrides.systemPrompt,
      providerProfile: overrides.providerProfile,
    }).map((field) => [field.key, field]));
    const existingKeys = new Set(view.config_fields.map((field) => field.key));
    return {
      ...view,
      config_fields: [
        ...view.config_fields.map((field) => replacements.get(field.key) ?? field),
        ...[...replacements.values()].filter((field) => !existingKeys.has(field.key)),
      ],
    };
  };
  return {
    artifactPane: (view) => renderArtifactPane(view, local),
    overlay: (view) => renderOverlay(withGlobalSideConfig(view), local),
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
  assert.doesNotMatch(html, /data-action="send"(?:\s|>)/);
});

test("Settings owns the global Side Chat defaults and lifecycle explanation", () => {
  const renderer = useSidePane({
    baseUrl: "http://side.test/v1/path",
    model: "gemma-settings",
    systemPrompt: "  concise answers  ",
  });
  const html = renderer.overlay({ ...state(), overlay: "config" });

  assert.match(html, /href="#settings-side-chat"/);
  assert.match(html, /id="settings-side-chat"[^>]*aria-labelledby="settings-side-chat-title"/);
  assert.match(html, /id="side-chat-base-url"[^>]*value="http:\/\/side\.test\/v1\/path"/);
  assert.match(html, /<label for="side-chat-model">モデル<\/label>/);
  assert.match(html, /id="side-chat-model"[^>]*data-config-key="side_chat\.model"[^>]*aria-describedby="[^"]*side-chat-settings-help[^"]*settings-validation[^"]*side-chat-model-catalog-status[^"]*"/);
  assert.match(html, /<option value="gemma-settings" selected>gemma-settings（現在の設定）<\/option>/);
  assert.match(html, /一覧にないモデルIDを入力/);
  assert.match(html, /id="side-chat-model-manual"[^>]*value="gemma-settings"/);
  assert.match(html, /id="side-chat-system-prompt"[^>]*data-config-key="side_chat\.system_prompt"[^>]*>  concise answers  <\/textarea>/);
  assert.match(html, /組み込みの指示に追加します/);
  assert.match(html, /16,384文字以内/);
  assert.match(html, /data-action="load-side-chat-models"[^>]*aria-controls="side-chat-model side-chat-model-catalog-status"/);
  assert.match(html, /新しく開くサイドチャットの既定値です。文字のみの会話で、ツールは使用しません/);
  assert.match(html, /既存の会話の設定は変わりません/);
  assert.match(html, /削除すると、その会話の履歴と下書きも失われます/);
});

test("Settings validates global Side Chat provider fields before save or catalog commands", () => {
  const invalidRenderer = useSidePane({
    baseUrl: "https://user:secret@side.test/v1?hidden=true",
    model: "gemma-settings",
    catalogLoadEnabled: false,
  });
  const invalidUrl = invalidRenderer.overlay({ ...state(), overlay: "config" });
  assert.match(invalidUrl, /id="side-chat-base-url"[^>]*aria-invalid="true"/);
  assert.match(invalidUrl, /id="settings-validation" class="validation error"/);
  assert.match(invalidUrl, /認証情報/);
  assert.match(invalidUrl, /data-action="load-side-chat-models"[^>]*aria-disabled="true"[^>]*disabled/);

  const missingRenderer = useSidePane({
    baseUrl: "http://side.test/proxy/v1",
    model: "   ",
    catalogLoadEnabled: true,
  });
  const missingModel = missingRenderer.overlay({ ...state(), overlay: "config" });
  assert.match(missingModel, /id="side-chat-model-manual"[^>]*aria-invalid="true"/);
  assert.match(missingModel, /side_chat\.model: 値を入力してください。/);
  assert.match(missingModel, /id="settings-validation" class="validation error"/);
  assert.match(missingModel, /data-action="load-side-chat-models"[^>]*aria-disabled="false"/);

  const oversizedPrompt = useSidePane({
    baseUrl: "http://side.test/proxy/v1",
    model: "gemma-settings",
    systemPrompt: "😀".repeat(16_385),
    catalogLoadEnabled: true,
  }).overlay({ ...state(), overlay: "config" });
  assert.match(oversizedPrompt, /id="side-chat-system-prompt"[^>]*aria-invalid="true"/);
  assert.match(oversizedPrompt, /追加システムプロンプトは16,384文字以内/);
  assert.match(oversizedPrompt, /id="settings-validation" class="validation error"/);
  assert.match(oversizedPrompt, /data-action="load-side-chat-models"[^>]*aria-disabled="false"/);
});

test("Settings presents Main and Side LLM URL and native model selection consistently without merging their owners", () => {
  const renderer = useSidePane({
    baseUrl: "http://side.test/v1",
    model: "gemma-side",
    systemPrompt: "Side instructions",
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
      {
        key: "model.system_prompt",
        value: "Main instructions",
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
  const mainEnd = html.indexOf('<div id="settings-model"');
  const sideStart = html.indexOf('<section id="settings-side-chat"');
  const sideEnd = html.indexOf('<section id="settings-permissions"');
  assert.ok(mainStart >= 0 && mainEnd > mainStart && sideStart > mainEnd && sideEnd > sideStart);
  const main = html.slice(mainStart, mainEnd);
  const side = html.slice(sideStart, sideEnd);

  assert.match(main, /<h3 id="settings-provider-title">メインチャット<\/h3>/);
  assert.ok(main.indexOf("接続先URL") < main.indexOf('for="main-provider-model">モデル'));
  assert.match(main, /<select id="main-provider-model"[^>]*data-config-key="model\.model"/);
  assert.match(main, /<option value="qwen-main" selected>Qwen Main（ロード済み）<\/option>/);
  assert.match(main, /<option value="qwen-main-alt" >Qwen Main Alt（未ロード）<\/option>/);
  assert.match(main, /data-action="show-provider"[^>]*aria-controls="main-provider-model main-provider-model-catalog-status"/);
  assert.match(main, /メインチャットの共通の既定値/);
  assert.match(main, /data-config-key="model\.system_prompt"[^>]*>Main instructions<\/textarea>/);
  assert.doesNotMatch(main, /Side instructions/);
  assert.doesNotMatch(main, /data-side-chat-setting/);

  assert.match(side, /<h3 id="settings-side-chat-title">サイドチャット<\/h3>/);
  assert.ok(side.indexOf("接続先URL") < side.indexOf('<label for="side-chat-model">モデル'));
  assert.match(side, /<select id="side-chat-model"/);
  assert.match(side, /新しく開くサイドチャットの既定値です。文字のみの会話で、ツールは使用しません/);
  assert.match(side, /data-config-key="side_chat\.system_prompt"[^>]*>Side instructions<\/textarea>/);
  assert.doesNotMatch(side, /Main instructions/);
  assert.doesNotMatch(side, /data-config-key="model\./);
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
      source: "global",
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
      source: "global",
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
  assert.match(html, /id="settings-validation" class="validation error"[^>]*>side_chat\.model: 値を入力してください。/);
});

test("Settings exposes catalog loading and failure through an accessible live status", () => {
  const loadingRenderer = useSidePane({
    baseUrl: "http://side.test/v1",
    model: "gemma-current",
    catalog: {
      status: "loading",
      source: "global",
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
      source: "global",
      baseUrl: "http://side.test",
      models: [],
      error: "接続できません <retry>",
    },
  });
  const failed = failedRenderer.overlay({ ...state(), overlay: "config" });
  assert.match(failed, /id="side-chat-model-catalog-status" class="side-chat-model-catalog-status error"[^>]*>接続できません &lt;retry&gt;<\/p>/);
  assert.match(failed, /data-action="load-side-chat-models"[^>]*>モデル読込<\/button>/);
});

test("Global Side Chat model catalog load is explicit and canonical", async () => {
  const ui = createUiLocalState();
  const current = state();
  current.overlay = "config";
  updateConfigDraftValue(
    ui,
    current.config_target,
    current.config_fields.map((field) => ({ key: field.key, text: field.value })),
    "side_chat.base_url",
    "http://side.test/v1/",
  );
  updateConfigDraftValue(
    ui,
    current.config_target,
    current.config_fields.map((field) => ({ key: field.key, text: field.value })),
    "side_chat.model",
    "google/gemma-4-12b-qat",
  );
  let args: Record<string, unknown> | null = null;
  let rerenders = 0;
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    loadSideChatModels: async (input: Record<string, unknown>) => {
      args = input;
      return {
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
    baseUrl: "http://side.test",
    providerProfile: "openai_compatible",
    expectedConfigGeneration: "7",
  });
  assert.equal(rerenders, 2);
  const catalog = sideChatCatalogViewForState(ui, current);
  assert.equal(catalog.status, "ready");
  assert.equal(catalog.source, "global");
  assert.deepEqual(
    sideChatModelOptions(catalog, "google/gemma-4-12b-qat").map((model) => model.id),
    ["google/gemma-4-12b-qat"],
  );
});

test("a changed Global Side Chat draft rejects a stale catalog settlement without touching conversation draft", async () => {
  const ui = createUiLocalState();
  const current = state();
  current.overlay = "config";
  const values = current.config_fields.map((field) => ({ key: field.key, text: field.value }));
  updateConfigDraftValue(ui, current.config_target, values, "side_chat.base_url", "http://first.test/v1");
  const conversationDraft = sideChatDraftForState(ui, current);
  assert.ok(conversationDraft);
  conversationDraft.text = "unsent side draft";
  conversationDraft.revision += 1;

  let release!: (result: SideChatCatalogResult) => void;
  const pendingResult = new Promise<SideChatCatalogResult>((resolve) => { release = resolve; });
  let rerenders = 0;
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    loadSideChatModels: async () => pendingResult,
    recoverCommandConflict: () => false,
    rerender: () => { rerenders += 1; },
  } as unknown as ActionContext;
  const load = actionById("load-side-chat-models");
  assert.ok(load);

  const pending = Promise.resolve(load.run(current, context, { index: -1, value: "" }));
  assert.equal(sideChatCatalogViewForState(ui, current).status, "loading");
  updateConfigDraftValue(ui, current.config_target, values, "side_chat.base_url", "http://second.test/v1");
  release({
    baseUrl: "http://first.test",
    providerProfile: "openai_compatible",
    configGeneration: "7",
    models: [{ id: "stale-model", label: "Stale", loadState: "loaded" }],
  });
  await pending;

  assert.equal(rerenders, 2);
  assert.equal(sideChatCatalogViewForState(ui, current).status, "idle");
  assert.equal(sideChatCatalogViewForState(ui, current).baseUrl, "http://second.test");
  assert.equal(conversationDraft.text, "unsent side draft");
  assert.equal(ui.configDraftValues.get("side_chat.base_url"), "http://second.test/v1");
});

test("Side Chat catalog rejects an ABA connection edit by browser-owned identity revision", () => {
  const ui = createUiLocalState();
  const current = state();
  current.overlay = "config";
  const values = current.config_fields.map((field) => ({ key: field.key, text: field.value }));
  const request = beginSideChatCatalogLoad(ui, current);
  assert.ok(request);

  recordSideChatCatalogConfigEdit(
    ui,
    "side_chat.base_url",
    "http://side.test/v1",
    "http://other.test/v1",
  );
  updateConfigDraftValue(ui, current.config_target, values, "side_chat.base_url", "http://other.test/v1");
  recordSideChatCatalogConfigEdit(
    ui,
    "side_chat.base_url",
    "http://other.test/v1",
    "http://side.test/v1",
  );
  updateConfigDraftValue(ui, current.config_target, values, "side_chat.base_url", "http://side.test/v1");

  const settlement = finishSideChatCatalogLoad(ui, current, request, {
    baseUrl: "http://side.test",
    providerProfile: "openai_compatible",
    configGeneration: "7",
    models: [{ id: "stale-model", label: "Stale", loadState: "loaded" }],
  });

  assert.deepEqual(settlement, { catalogAccepted: false, localStateChanged: true });
  assert.equal(sideChatCatalogViewForState(ui, current).status, "idle");
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
    const request = beginSideChatCatalogLoad(ui, current);
    assert.ok(request);
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

    assert.deepEqual(finishSideChatCatalogLoad(ui, current, request, {
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
  current.overlay = "config";
  const values = current.config_fields.map((field) => ({ key: field.key, text: field.value }));
  updateConfigDraftValue(ui, current.config_target, values, "side_chat.base_url", "http://first.test/v1");
  const firstRequest = beginSideChatCatalogLoad(ui, current);
  assert.ok(firstRequest);
  updateConfigDraftValue(ui, current.config_target, values, "side_chat.base_url", "http://second.test/v1");
  const latestRequest = beginSideChatCatalogLoad(ui, current);
  assert.ok(latestRequest);

  assert.deepEqual(finishSideChatCatalogLoad(ui, current, firstRequest, {
    baseUrl: "http://first.test",
    providerProfile: "openai_compatible",
    configGeneration: "7",
    models: [{ id: "stale-model", label: "Stale", loadState: "unknown" }],
  }), { catalogAccepted: false, localStateChanged: false });
  assert.deepEqual(finishSideChatCatalogLoad(ui, current, latestRequest, {
    baseUrl: "http://second.test",
    providerProfile: "openai_compatible",
    configGeneration: "7",
    models: [{ id: "latest-model", label: "Latest", loadState: "loaded" }],
  }), { catalogAccepted: true, localStateChanged: true });
  assert.deepEqual(sideChatCatalogViewForState(ui, current).models.map((model) => model.id), ["latest-model"]);

  const seedUi = createUiLocalState();
  const seedState = state();
  seedState.overlay = "config";
  seedState.config_fields = sideChatConfigFields({ baseUrl: "http://second.test/v1" });
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

test("an admitted Side Chat catalog result is dropped when Main Settings settlement takes ownership", () => {
  const ui = createUiLocalState();
  const current = state();
  current.overlay = "config";
  current.config_fields = sideChatConfigFields({ baseUrl: "http://side.test/v1" });
  const request = beginSideChatCatalogLoad(ui, current);
  assert.ok(request);

  ui.activeConfigMutationGeneration = 3n;
  assert.deepEqual(finishSideChatCatalogLoad(ui, current, request, {
    baseUrl: "http://side.test",
    providerProfile: "openai_compatible",
    configGeneration: "7",
    models: [{ id: "must-not-settle", label: "Stale", loadState: "loaded" }],
  }), { catalogAccepted: false, localStateChanged: true });
  assert.equal(ui.sideChatCatalogTransaction.active, null);
  assert.deepEqual(sideChatCatalogViewForState(ui, current).models, []);
});

test("Side Chat catalog response must match the admitted URL, profile, and config generation", () => {
  for (const mismatch of [
    { baseUrl: "http://other.test", providerProfile: "openai_compatible" as const, configGeneration: "7" },
    { baseUrl: "http://side.test", providerProfile: "lm_studio" as const, configGeneration: "7" },
    { baseUrl: "http://side.test", providerProfile: "openai_compatible" as const, configGeneration: "8" },
  ]) {
    const ui = createUiLocalState();
    const current = state();
    current.overlay = "config";
    current.config_fields = sideChatConfigFields({ baseUrl: "http://side.test/v1" });
    const request = beginSideChatCatalogLoad(ui, current);
    assert.ok(request);

    assert.deepEqual(finishSideChatCatalogLoad(ui, current, request, {
      baseUrl: mismatch.baseUrl,
      providerProfile: mismatch.providerProfile,
      configGeneration: mismatch.configGeneration,
      models: [{ id: "wrong-target", label: "Wrong target", loadState: "loaded" }],
    }), { catalogAccepted: false, localStateChanged: true });
    const rejected = sideChatCatalogViewForState(ui, current);
    assert.equal(rejected.status, "error");
    assert.deepEqual(rejected.models, []);
    assert.equal(sideChatCatalogLoadOpen(ui, current), true);
    assert.equal(ui.sideChatCatalogTransaction.active, null);
  }
});

test("Global Side Chat Settings stay editable while an existing snapshot is running or deleting", () => {
  const renderer = useSidePane({
    baseUrl: "http://global-side.test/v1",
    model: "global-model",
    systemPrompt: "future chats only",
    catalogLoadEnabled: true,
  });

  for (const runtime of [
    state({
      status: "running",
      can_send: false,
      can_cancel: true,
      base_url: "http://captured-side.test/v1",
      model: "captured-model",
      system_prompt: "captured prompt",
    }),
    state({
      deleting: true,
      can_send: false,
      base_url: "http://captured-side.test/v1",
      model: "captured-model",
      system_prompt: "captured prompt",
    }),
  ]) {
    const html = renderer.overlay({ ...runtime, overlay: "config" });
    for (const id of [
      "side-chat-provider-profile",
      "side-chat-base-url",
      "side-chat-model",
      "side-chat-model-manual",
      "side-chat-system-prompt",
    ]) {
      const control = html.match(new RegExp(`id="${id}"[^>]*>`))?.[0] ?? "";
      assert.ok(control, id);
      assert.doesNotMatch(control, /disabled/, id);
    }
    assert.match(html, /value="http:\/\/global-side\.test\/v1"/);
    assert.match(html, /global-model（現在の設定）/);
    assert.match(html, />future chats only<\/textarea>/);
    assert.doesNotMatch(html, /captured-model|captured prompt/);
  }
});

test("a Global Settings transaction blocks every Side Chat conversation mutation and model load", async () => {
  const ui = createUiLocalState();
  const current = state({ can_send: true, can_cancel: true });
  current.overlay = "config";
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
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
    "show-side-chat-pane",
    "load-side-chat-models",
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

test("an unrelated invalid Main field does not replace the Global Side Chat catalog target", () => {
  const ui = createUiLocalState();
  ui.configDirty = true;
  const current = state();
  current.overlay = "config";
  current.config_fields = [
    configField("model.request_timeout_ms", "0", "integer", {
      env_override: "MOYAI_REQUEST_TIMEOUT_MS",
      min_value: 1,
      max_value: 3_600_000,
    }),
    ...sideChatConfigFields({
      baseUrl: "http://replacement.test/v1",
      model: "gemma-replacement",
    }),
  ];
  const values = current.config_fields.map((field) => ({ key: field.key, text: field.value }));
  updateConfigDraftValue(ui, current.config_target, values, "model.request_timeout_ms", "0");

  assert.equal(sideChatOperationsOpen(ui), true);
  assert.equal(sideChatCatalogLoadOpen(ui, current), true);
  const request = beginSideChatCatalogLoad(ui, current);
  assert.ok(request);
  assert.equal(request.baseUrl, "http://replacement.test");
  assert.equal(request.providerProfile, "openai_compatible");
});

test("Global Settings settlement blocks Side conversation controls without replacing Settings editors", () => {
  const renderer = useSidePane({
    draft: "wait for Global Settings",
    baseUrl: "http://replacement.test/v1/",
    model: "gemma-replacement",
    catalogLoadEnabled: true,
    configPending: true,
    configDraftEditOpen: false,
  });
  const current = state({ can_send: true, can_cancel: true });
  const settings = renderer.overlay({ ...current, overlay: "config" });
  assert.match(settings, /class="modal settings-modal [^"]*"[^>]*aria-busy="true"/);
  assert.match(settings, /id="side-chat-base-url"/);
  assert.match(settings, /id="side-chat-model"/);
  assert.match(settings, /id="side-chat-system-prompt"/);

  const pane = renderer.artifactPane(current);
  assert.match(pane, /data-action="request-delete-side-chat"[^>]*disabled/);
  assert.match(pane, /id="side-chat-prompt"[^>]*disabled/);
  assert.doesNotMatch(pane, /data-action="cancel-side-chat"/);
  assert.match(pane, /data-action="send-side-chat"[^>]*disabled/);
  const runningPane = renderer.artifactPane(state({ status: "running", can_send: false, can_cancel: true }));
  assert.match(runningPane, /data-action="cancel-side-chat"[^>]*disabled/);

  const confirmationRenderer = useSidePane({
    draft: "wait for Global Settings",
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
  const remove = pane.match(/<button class="pin danger-pin"[^>]*>/)?.[0] ?? "";
  for (const control of [prompt, send, remove]) {
    assert.ok(control);
    assert.doesNotMatch(control, /disabled/);
  }
  assert.doesNotMatch(pane, /data-action="cancel-side-chat"/);
  const runningPane = renderer.artifactPane(state({ status: "running", can_send: false, can_cancel: true }));
  const stop = runningPane.match(/<button\b[^>]*data-action="cancel-side-chat"[^>]*>/)?.[0] ?? "";
  assert.ok(stop, "an active Side turn retains its Stop control independently of config-edit capability");
  assert.doesNotMatch(stop, /disabled/);
});

test("Side stop feedback uses a neutral Japanese notice only for the typed canonical user stop", () => {
  const renderer = useSidePane({ draft: "次の質問" });
  for (const configured of [true, false]) {
    const html = renderer.artifactPane(state({
      configured, status: "cancelled", phase: "", last_error: "run stopped by user", can_cancel: false,
    }));
    const notice = html.match(/<p class="side-chat-notice"[^>]*>[^<]*<\/p>/)?.[0] ?? "";
    assert.match(notice, /role="status">サイドチャットの実行を停止しました。<\/p>/);
    assert.doesNotMatch(html, /run stopped by user|class="side-chat-error"|data-action="cancel-side-chat"/);
  }
});

test("Side stop feedback preserves real errors and does not classify an error string alone as cancellation", () => {
  const renderer = useSidePane();
  for (const error of [
    { status: "failed" as const, last_error: "run stopped by user" },
    { status: "cancelled" as const, last_error: "draft save failed <storage>" },
    { status: "cancelled" as const, deleting: true, last_error: "deletion failed <storage>" },
  ]) {
    const html = renderer.artifactPane(state(error));
    assert.match(html, /class="side-chat-error" role="alert"/);
    assert.doesNotMatch(html, /class="side-chat-notice"|サイドチャットの実行を停止しました/);
    if (error.last_error.includes("<storage>")) assert.match(html, /&lt;storage&gt;/);
  }
});

test("Side stop feedback gives pending deletion priority and adds no notice without a terminal error", () => {
  const renderer = useSidePane();
  const deleting = renderer.artifactPane(state({
    deleting: true, status: "cancelled", last_error: "run stopped by user",
  }));
  assert.match(deleting, /サイドチャットを削除しています/);
  assert.doesNotMatch(deleting, /class="side-chat-notice"|class="side-chat-error"|run stopped by user/);
  const empty = renderer.artifactPane(state({ status: "cancelled", last_error: "" }));
  assert.doesNotMatch(empty, /class="side-chat-notice"|class="side-chat-error"/);
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
  assert.doesNotMatch(html, /data-action="cancel-side-chat"/);
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

test("a durable typed side draft rehydrates after restart and a dirty local edit is not overwritten", () => {
  const ui = createUiLocalState();
  const quote: SideChatPendingQuote = {
    sourceKind: "artifact",
    sourceHistoryItemId: "01J00000000000000000000007",
    sourceAppendPosition: "42",
    selectedText: "restart authority",
  };
  const restored = state({
    draft_text: "> Side Chat 引用\n> restart authority\n\nrestart draft",
    draft_quote: projectedDraftQuote(quote),
    draft_revision: "7",
  });
  const draft = sideChatDraftForState(ui, restored);
  assert.ok(draft);
  assert.equal(draft.text, "> Side Chat 引用\n> restart authority\n\nrestart draft");
  assert.deepEqual(draft.pendingQuote, quote);
  assert.deepEqual(draft.persistedQuote, quote);
  assert.equal(draft.persistedRevision, "7");

  draft.text = "unsaved local edit";
  draft.revision += 1;
  const externallyChanged = state({
    draft_text: "other owner",
    draft_quote: null,
    draft_revision: "8",
  });
  assert.equal(sideChatDraftForState(ui, externallyChanged)?.text, "unsaved local edit");
  assert.deepEqual(draft.pendingQuote, quote);
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
      quote: null,
    },
  }]);
  assert.equal(draft.persistedText, "new durable draft");
  assert.equal(draft.persistedQuote, null);
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

test("draft CAS settlement compares typed quote authority even when display text matches", async () => {
  const ui = createUiLocalState();
  let current = state({ draft_text: "shared display", draft_revision: "7" });
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  const localQuote: SideChatPendingQuote = {
    sourceKind: "transcript",
    sourceHistoryItemId: "01J00000000000000000000011",
    sourceAppendPosition: "42",
    selectedText: "local authority",
  };
  const winningQuote: SideChatPendingQuote = {
    sourceKind: "artifact",
    sourceHistoryItemId: "01J00000000000000000000012",
    sourceAppendPosition: "42",
    selectedText: "winning authority",
  };
  draft.pendingQuote = localQuote;
  draft.revision += 1;
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      calls.push({ name, args });
      current = state({
        draft_text: "shared display",
        draft_quote: projectedDraftQuote(winningQuote),
        draft_revision: "8",
      });
    },
    rerender: () => undefined,
  } as unknown as ActionContext;

  await persistSideChatDraft(current, context);

  assert.deepEqual(calls[0]?.args?.quote, localQuote);
  assert.deepEqual(draft.pendingQuote, localQuote);
  assert.deepEqual(draft.persistedQuote, winningQuote);
  assert.equal(draft.persistedText, "shared display");
  assert.equal(draft.persistedRevision, "8");
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

test("opening an unconfigured Side Chat ensures it from Global defaults before showing the pane", async () => {
  const ui = createUiLocalState();
  ui.drafts.prompt = "keep main draft";
  ui.artifactPaneCollapsed = true;
  const initial = state({
    configured: false,
    chat_id: null,
    model: "",
    system_prompt: "",
    base_url: "",
    generation: "0",
    can_send: false,
  });
  let current = initial;
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  let rerenders = 0;
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      calls.push({ name, args });
      current = state({
        chat_id: "side-created",
        generation: "1",
        model: "global-model",
        system_prompt: "global prompt",
        base_url: "http://global-side.test/v1",
      });
    },
    rerender: () => { rerenders += 1; },
  } as unknown as ActionContext;

  const action = actionById("show-side-chat-pane");
  assert.ok(action);
  await action.run(initial, context, { index: -1, value: "" });

  assert.deepEqual(calls, [{
    name: "ensure_side_chat",
    args: {
      ownerSessionId: "session-a",
      expectedConfigGeneration: "7",
    },
  }]);
  assert.equal(ui.sideChatMutations.size, 0);
  assert.equal(ui.artifactPaneMode, "side_chat");
  assert.equal(ui.artifactPaneCollapsed, false);
  assert.equal(ui.drafts.prompt, "keep main draft");
  assert.equal(sideChatDraftForState(ui, current)?.chatId, "side-created");
  assert.equal(rerenders, 2);
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

test("settled canonical transcript and artifact rows expose one native quote action", () => {
  const html = renderTranscriptRows([
    {
      row_kind: "user",
      stable_history_identity: "01J00000000000000000000001",
      step: "1",
      title: "User",
      body: "main question",
      file_changes: [],
    },
    {
      row_kind: "assistant",
      stable_history_identity: "01J00000000000000000000002",
      step: "2",
      title: "Assistant",
      body: "settled answer",
      file_changes: [],
    },
    {
      row_kind: "file_changes",
      stable_history_identity: "01J00000000000000000000003",
      step: "3",
      title: "File changes",
      body: "updated src/main.rs",
      file_changes: [],
    },
    {
      row_kind: "work_summary_running",
      stable_history_identity: "turn:synthetic:work-summary",
      step: "4",
      title: "Running",
      body: "not settled evidence",
      file_changes: [],
    },
  ], { sideChatQuoteOwnerSessionId: "session-a" });

  assert.equal((html.match(/data-action="quote-selection-to-side-chat"/g) ?? []).length, 3);
  assert.match(html, /data-history-identity="01J00000000000000000000001"[\s\S]*data-side-chat-quote-source-kind="transcript"/);
  assert.match(html, /data-history-identity="01J00000000000000000000003"[\s\S]*data-side-chat-quote-source-kind="artifact"/);
  assert.match(html, /<button type="button" class="message-quote-action"[\s\S]*>Side Chatで引用<\/button>/);
  assert.doesNotMatch(html, /data-source-history-item-id="turn:synthetic:work-summary"/);
});

test("quote selection accepts only one exact canonical source row and projection fence", () => {
  const transcript = {
    sourceKind: "transcript" as const,
    sourceHistoryItemId: "01J00000000000000000000001",
  };
  const accepted = pendingSideChatQuoteFromSelection({
    activatedSource: transcript,
    startSource: transcript,
    endSource: transcript,
    selectedText: "  selected\r\ntext  ",
    sourceAppendPosition: "42",
  });

  assert.deepEqual(accepted, {
    ...transcript,
    sourceAppendPosition: "42",
    selectedText: "selected\ntext",
  });
  assert.equal(pendingSideChatQuoteFromSelection({
    activatedSource: transcript,
    startSource: transcript,
    endSource: { ...transcript, sourceHistoryItemId: "01J00000000000000000000002" },
    selectedText: "cross-row selection",
    sourceAppendPosition: "42",
  }), null);
  assert.equal(pendingSideChatQuoteFromSelection({
    activatedSource: transcript,
    startSource: transcript,
    endSource: transcript,
    selectedText: "selected",
    sourceAppendPosition: null,
  }), null);
});

test("quote selection creates a first Side Chat, then opens and persists the exact quote", async () => {
  const ui = createUiLocalState();
  ui.drafts.prompt = "keep main composer";
  ui.artifactPaneCollapsed = true;
  const initial = state({
    configured: false,
    chat_id: null,
    model: "",
    system_prompt: "",
    base_url: "",
    generation: "0",
    can_send: false,
  });
  let current = initial;
  const quote: SideChatPendingQuote = {
    sourceKind: "transcript",
    sourceHistoryItemId: "01J00000000000000000000001",
    sourceAppendPosition: "42",
    selectedText: "first-use owner evidence",
  };
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      calls.push({ name, args });
      if (name === "ensure_side_chat") {
        current = state({
          chat_id: "side-created",
          generation: "1",
          model: "global-model",
          system_prompt: "global prompt",
          base_url: "http://global-side.test/v1",
        });
      } else if (name === "save_side_chat_draft") {
        current = state({
          chat_id: "side-created",
          generation: "1",
          model: "global-model",
          system_prompt: "global prompt",
          base_url: "http://global-side.test/v1",
          draft_text: String(args?.text ?? ""),
          draft_quote: projectedDraftQuote((args?.quote ?? null) as SideChatPendingQuote | null),
          draft_revision: "1",
        });
      }
    },
    rerender: () => undefined,
  } as unknown as ActionContext;

  const action = actionById("quote-selection-to-side-chat");
  assert.ok(action);
  await action.run(initial, context, {
    index: -1,
    value: "",
    sideChatQuote: quote,
    sideChatQuoteOwnerSessionId: "session-a",
  });

  assert.deepEqual(calls[0], {
    name: "ensure_side_chat",
    args: {
      ownerSessionId: "session-a",
      expectedConfigGeneration: "7",
    },
  });
  assert.deepEqual(calls[1], {
    name: "save_side_chat_draft",
    args: {
      ownerSessionId: "session-a",
      chatId: "side-created",
      expectedDraftRevision: "0",
      text: "> Side Chat 引用\n> first-use owner evidence\n\n",
      quote,
    },
  });
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  assert.deepEqual(draft.pendingQuote, quote);
  assert.deepEqual(draft.persistedQuote, quote);
  assert.equal(draft.persistedRevision, "1");
  assert.equal(ui.artifactPaneMode, "side_chat");
  assert.equal(ui.artifactPaneCollapsed, false);
  assert.equal(ui.drafts.prompt, "keep main composer");
});

test("quote first use revalidates the owner and append fence after ensure", async () => {
  const quote: SideChatPendingQuote = {
    sourceKind: "transcript",
    sourceHistoryItemId: "01J00000000000000000000001",
    sourceAppendPosition: "42",
    selectedText: "must remain fenced",
  };
  for (const changed of ["owner", "append"] as const) {
    const ui = createUiLocalState();
    ui.artifactPaneCollapsed = true;
    const initial = state({
      configured: false,
      chat_id: null,
      model: "",
      base_url: "",
      generation: "0",
      can_send: false,
    });
    let current = initial;
    const calls: string[] = [];
    const context = {
      uiState: ui,
      getProjection: () => current,
      getViewState: () => current,
      mutate: async (name: string) => {
        calls.push(name);
        current = changed === "owner"
          ? state({ chat_id: "other-side", generation: "1" }, "session-b")
          : state({ chat_id: "side-created", generation: "1", context_as_of_append_position: "43" });
      },
      rerender: () => undefined,
    } as unknown as ActionContext;

    const action = actionById("quote-selection-to-side-chat");
    assert.ok(action);
    await action.run(initial, context, {
      index: -1,
      value: "",
      sideChatQuote: quote,
      sideChatQuoteOwnerSessionId: "session-a",
    });

    assert.deepEqual(calls, ["ensure_side_chat"], changed);
    assert.equal(ui.artifactPaneMode, "output", changed);
    assert.equal(ui.artifactPaneCollapsed, true, changed);
    assert.equal(sideChatDraftForState(ui, current)?.pendingQuote ?? null, null, changed);
  }
});

test("quote action appends to only the Side draft, never auto-sends, and manual edit clears it", async () => {
  const ui = createUiLocalState();
  ui.drafts.prompt = "keep main composer";
  ui.artifactPaneCollapsed = true;
  let current = state();
  const quote: SideChatPendingQuote = {
    sourceKind: "transcript",
    sourceHistoryItemId: "01J00000000000000000000001",
    sourceAppendPosition: "42",
    selectedText: "settled owner evidence",
  };
  let durableRevision = 0;
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      calls.push({ name, args });
      if (name === "save_side_chat_draft") {
        durableRevision += 1;
        current = state({
          draft_text: String(args?.text ?? ""),
          draft_quote: projectedDraftQuote(
            (args?.quote ?? null) as SideChatPendingQuote | null,
          ),
          draft_revision: String(durableRevision),
        });
      }
    },
    rerender: () => undefined,
  } as unknown as ActionContext;

  const action = actionById("quote-selection-to-side-chat");
  assert.ok(action);
  await action.run(current, context, {
    index: -1,
    value: "",
    sideChatQuote: quote,
    sideChatQuoteOwnerSessionId: "session-b",
  });
  assert.equal(sideChatDraftForState(ui, current)?.text, "");
  assert.deepEqual(calls, []);
  await action.run(current, context, {
    index: -1,
    value: "",
    sideChatQuote: quote,
    sideChatQuoteOwnerSessionId: "session-a",
  });

  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  assert.match(draft.text, /^> Side Chat 引用\n> settled owner evidence\n\n$/);
  assert.deepEqual(draft.pendingQuote, quote);
  assert.equal(ui.drafts.prompt, "keep main composer");
  assert.equal(ui.artifactPaneMode, "side_chat");
  assert.equal(ui.artifactPaneCollapsed, false);
  assert.deepEqual(calls.map((call) => call.name), ["save_side_chat_draft"]);
  assert.deepEqual(calls[0]?.args?.quote, quote);
  assert.ok(calls.every((call) => call.name !== "submit_side_chat" && call.name !== "submit_prompt"));

  const previousRevision = draft.revision;
  updateSideChatDraftFromManualEdit(draft, `${draft.text}explain this`);
  assert.equal(draft.pendingQuote, null);
  assert.equal(draft.revision, previousRevision + 1);
  await persistSideChatDraft(current, context);
  assert.deepEqual(calls.map((call) => call.name), [
    "save_side_chat_draft",
    "save_side_chat_draft",
  ]);
  assert.equal(calls[1]?.args?.quote, null);
  assert.equal(current.side_chat.draft_quote, null);
  assert.equal(draft.persistedQuote, null);
  assert.equal(draft.persistedText, draft.text);
  assert.equal(ui.drafts.prompt, "keep main composer");
});

test("a second typed quote replaces and persists without orphaning the first authority", async () => {
  const ui = createUiLocalState();
  let current = state();
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  const first: SideChatPendingQuote = {
    sourceKind: "transcript",
    sourceHistoryItemId: "01J00000000000000000000001",
    sourceAppendPosition: "42",
    selectedText: "first evidence",
  };
  const second: SideChatPendingQuote = {
    sourceKind: "artifact",
    sourceHistoryItemId: "01J00000000000000000000002",
    sourceAppendPosition: "42",
    selectedText: "second evidence",
  };

  appendQuoteToSideChatDraft(draft, first);
  appendQuoteToSideChatDraft(draft, second);

  assert.equal(draft.text, "> Side Chat 引用\n> second evidence\n\n");
  assert.deepEqual(draft.pendingQuote, second);
  assert.doesNotMatch(draft.text, /first evidence/);

  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      calls.push({ name, args });
      current = state({
        draft_text: String(args?.text ?? ""),
        draft_quote: projectedDraftQuote(
          (args?.quote ?? null) as SideChatPendingQuote | null,
        ),
        draft_revision: "1",
      });
    },
    rerender: () => undefined,
  } as unknown as ActionContext;
  await persistSideChatDraft(current, context);

  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0]?.args?.quote, second);
  assert.doesNotMatch(String(calls[0]?.args?.text), /first evidence/);
  assert.deepEqual(draft.persistedQuote, second);
});

test("typed quote buttons activate exactly on non-repeated Enter or Space", () => {
  assert.equal(sideChatQuoteKeyboardActivation("Enter", false), true);
  assert.equal(sideChatQuoteKeyboardActivation(" ", false), true);
  assert.equal(sideChatQuoteKeyboardActivation("Enter", true), false);
  assert.equal(sideChatQuoteKeyboardActivation("Spacebar", false), false);
  assert.equal(sideChatQuoteKeyboardActivation("Escape", false), false);
});

test("Side pane shows owner snapshot metadata and the typed pending quote", () => {
  const quote: SideChatPendingQuote = {
    sourceKind: "artifact",
    sourceHistoryItemId: "01J00000000000000000000003",
    sourceAppendPosition: "42",
    selectedText: "updated src/main.rs",
  };
  const html = useSidePane({
    draft: "引用を確認して",
    pendingQuote: quote,
  }).artifactPane(state({ context_truncated: true }));

  assert.match(html, /id="side-chat-context-description"/);
  assert.match(html, /参照: このタスクの履歴/);
  assert.match(html, /履歴位置 42/);
  assert.match(html, /長い履歴の一部を省略/);
  assert.match(html, /class="side-chat-pending-quote"/);
  assert.match(html, /作業結果から引用/);
  assert.match(html, /updated src\/main\.rs/);
});

test("Side Send carries the exact owner fence and typed quote then clears only the admitted quote", async () => {
  const ui = createUiLocalState();
  ui.drafts.prompt = "main remains";
  let current = state();
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  const quote: SideChatPendingQuote = {
    sourceKind: "transcript",
    sourceHistoryItemId: "01J00000000000000000000001",
    sourceAppendPosition: "42",
    selectedText: "settled owner evidence",
  };
  appendQuoteToSideChatDraft(draft, quote);
  draft.text += "Explain this.";
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      calls.push({ name, args });
      if (name === "submit_side_chat") {
        current = state({
          draft_text: "",
          draft_revision: "1",
          generation: "5",
          status: "running",
          can_send: false,
          can_cancel: true,
        });
      }
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
      expectedDraftRevision: "0",
      expectedOwnerAppendPosition: "42",
      quote,
      text: "> Side Chat 引用\n> settled owner evidence\n\nExplain this.",
    },
  }]);
  assert.equal(draft.pendingQuote, null);
  assert.equal(draft.text, "");
  assert.equal(ui.drafts.prompt, "main remains");
});

test("Side Send preserves a typed quote rehydrated from the durable restart projection", async () => {
  const ui = createUiLocalState();
  const quote: SideChatPendingQuote = {
    sourceKind: "artifact",
    sourceHistoryItemId: "01J00000000000000000000009",
    sourceAppendPosition: "42",
    selectedText: "durable restart evidence",
  };
  const text = "> Side Chat 引用\n> durable restart evidence\n\nExplain after restart.";
  let current = state({
    draft_text: text,
    draft_quote: projectedDraftQuote(quote),
    draft_revision: "7",
  });
  const calls: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => current,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      calls.push({ name, args });
      current = state({
        draft_text: "",
        draft_quote: null,
        draft_revision: "8",
        generation: "5",
        status: "running",
        can_send: false,
        can_cancel: true,
      });
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
      expectedOwnerAppendPosition: "42",
      quote,
      text,
    },
  }]);
  const draft = sideChatDraftForState(ui, current);
  assert.ok(draft);
  assert.equal(draft.text, "");
  assert.equal(draft.pendingQuote, null);
  assert.equal(draft.persistedQuote, null);
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
      expectedOwnerAppendPosition: "42",
      quote: null,
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
      expectedOwnerAppendPosition: "42",
      quote: null,
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
      expectedOwnerAppendPosition: "42",
      quote: null,
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
  ui.artifactPaneMode = "side_chat";
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
  assert.equal(ui.artifactPaneMode, "output");
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
  ui.artifactPaneMode = "side_chat";
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
  assert.equal(ui.artifactPaneMode, "output");
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


  assert.deepEqual(calls, []);
});
