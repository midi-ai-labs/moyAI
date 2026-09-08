export type HubContext = "main" | "side_chat";
export type HubRouteMode = "direct" | "hub";
export interface HubActiveRoute {
  turn_id: string;
  phase: "waiting" | "running";
  logical_model_id: string | null;
}
export interface HubModel { id: string; label: string; capabilities: string[] }
export interface HubCatalog {
  hub_id: string;
  software_version: string;
  revision: string;
  models: HubModel[];
  changes: { revision: string; at_ms: number; summary: string }[];
}
export interface HubSelection {
  allowed_model_ids: string[];
  preferred_model_id: string;
  required_capabilities: string[];
  wait_policy: "wait_for_preferred" | "allow_selected_fallback";
  affinity_turns: number;
}
export interface HubReview { hub_id: string; reviewed_revision: string; selection: HubSelection }
export interface HubProjection {
  settings_revision: string;
  connection_generation: string;
  status: "disconnected" | "connecting" | "connected" | "stale" | "error";
  endpoint: string;
  label: string;
  hub_id: string | null;
  catalog: HubCatalog | null;
  main_review: HubReview | null;
  recommended_main_selection?: HubSelection | null;
  side_chat_review: HubReview | null;
  main_confirmation: "unconfirmed" | "confirmed" | "review_required";
  side_chat_confirmation: "unconfirmed" | "confirmed" | "review_required";
  main_mode: HubRouteMode;
  side_chat_mode: HubRouteMode;
  active_main: HubActiveRoute | null;
  active_side_chat: HubActiveRoute | null;
  can_enable_main_hub: boolean;
  can_enable_side_chat_hub: boolean;
  can_change_main_mode: boolean;
  can_change_side_chat_mode: boolean;
  error: string | null;
}
export interface HubReviewTarget {
  expectedSettingsRevision: string;
  expectedConnectionGeneration: string;
  expectedHubId: string;
  expectedCatalogRevision: string;
}
export interface HubDraft {
  selection: HubSelection;
  affinityText: string;
  capabilitiesText: string;
  dirty: boolean;
  target: HubReviewTarget | null;
}
export interface HubUiState {
  tab: "devices" | "models";
  projection: HubProjection | null;
  endpoint: string;
  label: string;
  connectionTouched: boolean;
  drafts: Record<HubContext, HubDraft>;
  pending: "load" | "connect" | "refresh" | "main" | "side_chat" | "main_mode" | "side_chat_mode" | "disconnect" | null;
  error: string;
  errorContext: HubContext | "connection" | null;
  requestSerial: number;
}
export type HubPresentation = Omit<HubUiState, "requestSerial">;

function emptyDraft(): HubDraft {
  return {
    selection: { allowed_model_ids: [], preferred_model_id: "", required_capabilities: [], wait_policy: "wait_for_preferred", affinity_turns: 1 },
    affinityText: "1", capabilitiesText: "", dirty: false, target: null,
  };
}
export function createHubUiState(): HubUiState {
  return {
    tab: "devices",
    projection: null, endpoint: "http://127.0.0.1:9470", label: "moyAI Desktop",
    connectionTouched: false, drafts: { main: emptyDraft(), side_chat: emptyDraft() },
    pending: null, error: "", errorContext: null, requestSerial: 0,
  };
}
export function hubPresentation(state: HubUiState): HubPresentation {
  const { requestSerial: _serial, ...presentation } = state;
  return presentation;
}
export function hubReviewTarget(projection: HubProjection): HubReviewTarget | null {
  if (!projection.catalog || !projection.hub_id) return null;
  return {
    expectedSettingsRevision: projection.settings_revision,
    expectedConnectionGeneration: projection.connection_generation,
    expectedHubId: projection.hub_id,
    expectedCatalogRevision: projection.catalog.revision,
  };
}
export function acceptHubProjection(
  state: HubUiState,
  projection: HubProjection,
  options: {
    refreshTargets?: boolean; savedContext?: HubContext; connected?: boolean;
    localSave?: { before: HubProjection; context: HubContext; kind: "review" | "mode" };
  } = {},
): boolean {
  const previous = state.projection;
  if (previous && (
    BigInt(projection.connection_generation) < BigInt(previous.connection_generation)
    || BigInt(projection.settings_revision) < BigInt(previous.settings_revision)
    || (projection.connection_generation === previous.connection_generation
      && projection.hub_id === previous.hub_id && projection.catalog && previous.catalog
      && BigInt(projection.catalog.revision) < BigInt(previous.catalog.revision))
  )) return false;
  if (previous && options.localSave
    && projection.connection_generation === previous.connection_generation
    && projection.settings_revision === previous.settings_revision
    && !localSaveReceiptMatchesCurrent(previous, projection, options.localSave)) return false;
  state.projection = projection;
  if (!state.connectionTouched || options.connected) {
    state.endpoint = projection.endpoint || state.endpoint;
    state.label = projection.label || state.label;
    state.connectionTouched = false;
  }
  const changedHub = previous?.hub_id && projection.hub_id && previous.hub_id !== projection.hub_id;
  for (const context of ["main", "side_chat"] as const) {
    let draft = state.drafts[context];
    const review = context === "main" ? projection.main_review : projection.side_chat_review;
    if (changedHub) draft = state.drafts[context] = emptyDraft();
    if (!draft.dirty || options.savedContext === context) {
      draft.selection = structuredClone(review?.selection ?? emptyDraft().selection);
      draft.affinityText = String(draft.selection.affinity_turns);
      draft.capabilitiesText = draft.selection.required_capabilities.join(", ");
      draft.dirty = false;
      draft.target = hubReviewTarget(projection);
    } else if (options.localSave && canAdvanceDraftAfterLocalSave(draft, context, options.localSave, projection)) {
      // This command changed only our other saved context or route mode. Keep the edit,
      // catalog review and connection target; advance only the local settings CAS baseline.
      draft.target = { ...draft.target!, expectedSettingsRevision: projection.settings_revision };
    } else if (options.refreshTargets) {
      // Explicit refresh lets the user review the visible draft against the new catalog.
      // Passive polling must not silently rebase an edit's mutation target.
      draft.target = hubReviewTarget(projection);
    }
  }
  return true;
}

function localSaveReceiptMatchesCurrent(
  current: HubProjection, receipt: HubProjection,
  save: { context: HubContext; kind: "review" | "mode" },
): boolean {
  if (JSON.stringify(current) === JSON.stringify(receipt)) return true;
  if (save.kind !== "review") return false;
  const confirmation = `${save.context}_confirmation` as const;
  const canEnable = `can_enable_${save.context}_hub` as const;
  const review = receipt[`${save.context}_review`];
  // A poll may observe the local durable save before its remote review acknowledgement.
  // Only that acknowledgement may advance this context; lifecycle, runtime, catalog and
  // other-context observations already received must never be replaced by an older receipt.
  if (current.status !== "connected" || receipt.status !== "connected"
    || current.error !== null || receipt.error !== null
    || current[confirmation] !== "unconfirmed" || receipt[confirmation] !== "confirmed"
    || current[canEnable] || !receipt[canEnable]
    || current[`active_${save.context}`] !== null || receipt[`active_${save.context}`] !== null
    || !review || review.hub_id !== receipt.hub_id
    || review.reviewed_revision !== receipt.catalog?.revision) return false;
  return JSON.stringify({ ...current, [confirmation]: receipt[confirmation], [canEnable]: receipt[canEnable] })
    === JSON.stringify(receipt);
}

function sameSelection(left: HubSelection, right: HubSelection): boolean {
  const sameSet = (a: string[], b: string[]) => JSON.stringify([...a].sort()) === JSON.stringify([...b].sort());
  return sameSet(left.allowed_model_ids, right.allowed_model_ids)
    && sameSet(left.required_capabilities, right.required_capabilities)
    && left.preferred_model_id === right.preferred_model_id
    && left.wait_policy === right.wait_policy && left.affinity_turns === right.affinity_turns;
}
function canAdvanceDraftAfterLocalSave(
  draft: HubDraft, context: HubContext,
  save: { before: HubProjection; context: HubContext; kind: "review" | "mode" }, after: HubProjection,
): boolean {
  const before = save.before;
  const target = draft.target;
  const beforeTarget = hubReviewTarget(before);
  if (!target || !beforeTarget || !before.catalog || !after.catalog
    || before.status !== "connected" || after.status !== "connected"
    || before.endpoint !== after.endpoint || before.label !== after.label
    || before.hub_id !== after.hub_id || before.connection_generation !== after.connection_generation
    || BigInt(after.settings_revision) !== BigInt(before.settings_revision) + 1n
    || JSON.stringify(before.catalog) !== JSON.stringify(after.catalog)
    || JSON.stringify(target) !== JSON.stringify(beforeTarget)) return false;
  for (const channel of ["main", "side_chat"] as const) {
    if (save.kind !== "review" || channel !== save.context) {
      if (JSON.stringify(before[`${channel}_review`]) !== JSON.stringify(after[`${channel}_review`])) return false;
    }
    if (save.kind !== "mode" || channel !== save.context) {
      if (before[`${channel}_mode`] !== after[`${channel}_mode`]) return false;
    }
  }
  return save.kind === "mode" || context !== save.context;
}
export function hubDraftHasChanges(state: HubPresentation, context: HubContext): boolean {
  const draft = state.drafts[context];
  if (!draft.dirty) return false;
  const selection = hubSelectionFromDraft(draft);
  const review = state.projection?.[`${context}_review`];
  return !selection || !review || !sameSelection(selection, review.selection);
}
export function hubSelectionFromDraft(draft: HubDraft): HubSelection | null {
  if (!/^(?:[1-9][0-9]?|100)$/.test(draft.affinityText.trim())) return null;
  const capabilities = [...new Set(draft.capabilitiesText.split(/[,\s]+/u).filter(Boolean))];
  if (capabilities.length > 32 || capabilities.some((value) => !/^[A-Za-z0-9_.-]{1,64}$/.test(value))) return null;
  return { ...draft.selection, affinity_turns: Number(draft.affinityText), required_capabilities: capabilities };
}
export function hubSelectionError(draft: HubDraft, catalog: HubCatalog | null): string | null {
  const selection = hubSelectionFromDraft(draft);
  if (!selection) return "継続ターン数は1〜100、必要な機能は英数字・ピリオド・ハイフン・アンダースコアで指定してください。";
  if (selection.allowed_model_ids.length === 0) return "利用候補のモデルを1つ以上選択してください。";
  if (!selection.allowed_model_ids.includes(selection.preferred_model_id)) return "選択したモデルから優先モデルを指定してください。";
  if (!catalog) return "Hubに接続してモデル情報を取得してください。";
  const selected = selection.allowed_model_ids.map((id) => catalog.models.find((model) => model.id === id));
  if (selected.some((model) => !model)) return "選択済みモデルが削除されています。選択を見直してください。";
  const capable = (model: HubModel) => selection.required_capabilities.every((capability) => model.capabilities.includes(capability));
  if (!selected.some((model) => model && capable(model))) return "選択モデルに必要な機能を満たすものがありません。";
  if (selection.wait_policy === "wait_for_preferred" && !capable(catalog.models.find((model) => model.id === selection.preferred_model_id)!)) {
    return "優先モデルが必要な機能を満たしていません。";
  }
  return null;
}
export function hubCanSave(state: HubPresentation, context: HubContext): boolean {
  return !state.pending && state.projection?.status === "connected"
    && !hubActiveRoute(state, context)
    && hubDraftTargetIsCurrent(state, context)
    && (hubDraftHasChanges(state, context) || state.projection[`${context}_confirmation`] !== "confirmed")
    && hubSelectionError(state.drafts[context], state.projection.catalog) === null;
}
export function hubSaveFeedback(state: HubPresentation, context: HubContext): string {
  if (state.pending === context) return "Hubで選択を確認して保存しています。";
  if (state.projection?.status !== "connected") return "Hubに接続すると、この選択を確認・保存できます。";
  if (hubActiveRoute(state, context)) return "このチャットの待機・実行が終了してから選択を保存できます。";
  const draft = state.drafts[context];
  const target = draft.target;
  const current = hubReviewTarget(state.projection);
  if (draft.dirty && target && current && !hubDraftTargetIsCurrent(state, context)) {
    if (target.expectedConnectionGeneration !== current.expectedConnectionGeneration || target.expectedHubId !== current.expectedHubId) {
      return "接続状態が変わりました。「最新情報を取得」で接続先を確認してください。入力した選択は保持しています。";
    }
    if (target.expectedCatalogRevision !== current.expectedCatalogRevision) {
      return "モデル情報が更新されました。「最新情報を取得」で変更を確認してから保存してください。入力した選択は保持しています。";
    }
    return "別の設定変更が反映されています。「最新情報を取得」で保存内容を確認してください。入力した選択は保持しています。";
  }
  const invalid = hubSelectionError(draft, state.projection.catalog);
  if (invalid) return invalid;
  if (hubDraftHasChanges(state, context)) return "未保存の変更があります。";
  if (state.projection[`${context}_confirmation`] === "confirmed") return "保存済みです。選択を変更すると再び保存できます。";
  return "現在のHubでは未確認です。選択内容を確認して保存してください。";
}
export function hubRouteMode(state: HubPresentation, context: HubContext): HubRouteMode | null {
  if (!state.projection) return null;
  return context === "main" ? state.projection.main_mode : state.projection.side_chat_mode;
}
export function hubActiveRoute(state: HubPresentation, context: HubContext): HubActiveRoute | null {
  if (!state.projection) return null;
  return context === "main" ? state.projection.active_main : state.projection.active_side_chat;
}
export function hubRouteModeBlocker(state: HubPresentation, context: HubContext, mode: HubRouteMode): string | null {
  if (!state.projection) return "送信先の設定を読み込んでいます。";
  if (state.pending) return "現在の操作が完了するまでお待ちください。";
  if (!(context === "main" ? state.projection.can_change_main_mode : state.projection.can_change_side_chat_mode)) return "このチャットの待機・実行が終了してから切り替えられます。";
  if (mode === "direct") return null;
  if (state.projection.status !== "connected") return "Hubに接続してから切り替えてください。";
  const confirmation = context === "main" ? state.projection.main_confirmation : state.projection.side_chat_confirmation;
  if (confirmation !== "confirmed") return "利用モデルの選択を確認・保存してからHubに切り替えてください。";
  if (hubDraftHasChanges(state, context)) return "編集中のモデル選択を保存してからHubに切り替えてください。";
  if (!(context === "main" ? state.projection.can_enable_main_hub : state.projection.can_enable_side_chat_hub)) return "現在はHubを利用できません。接続状態とモデルの対応状況を確認してください。";
  return null;
}
export function hubCanSetRouteMode(state: HubPresentation, context: HubContext, mode: HubRouteMode): boolean {
  return hubRouteMode(state, context) !== mode && hubRouteModeBlocker(state, context, mode) === null;
}
export function hubExecutionRoute(projection: HubProjection | null | undefined, context: HubContext): {
  modelLabel: string;
  endpointLabel: string;
  phaseLabel: string | null;
  blockedReason: string | null;
} | null {
  if (!projection) return null;
  const active = context === "main" ? projection.active_main : projection.active_side_chat;
  const mode = context === "main" ? projection.main_mode : projection.side_chat_mode;
  if (mode !== "hub" && !active) return null;
  const review = context === "main" ? projection.main_review : projection.side_chat_review;
  const modelId = active?.logical_model_id ?? review?.selection.preferred_model_id ?? "";
  const modelLabel = projection.catalog?.models.find((model) => model.id === modelId)?.label || modelId || "モデル未確認";
  const confirmation = context === "main" ? projection.main_confirmation : projection.side_chat_confirmation;
  return {
    modelLabel: `${active?.logical_model_id ? "使用中" : "優先モデル"}: ${modelLabel}`,
    endpointLabel: "Hubで割当",
    phaseLabel: active?.phase === "waiting" ? "Hubの実行枠を待っています" : null,
    blockedReason: active ? null : projection.status !== "connected"
      ? "Hubに接続してください。送信先はHubのままです。"
      : confirmation !== "confirmed" ? "Hubのモデル選択を再確認・保存してください。"
        : !(context === "main" ? projection.can_enable_main_hub : projection.can_enable_side_chat_hub)
          ? "現在はHubで実行できません。接続とモデルの対応状況を確認してください。" : null,
  };
}
export function hubDraftTargetIsCurrent(state: HubPresentation, context: HubContext): boolean {
  const target = state.drafts[context].target;
  const current = state.projection && hubReviewTarget(state.projection);
  return Boolean(target && current
    && target.expectedSettingsRevision === current.expectedSettingsRevision
    && target.expectedConnectionGeneration === current.expectedConnectionGeneration
    && target.expectedHubId === current.expectedHubId
    && target.expectedCatalogRevision === current.expectedCatalogRevision);
}
export function hubCanUseRecommendation(state: HubPresentation): boolean {
  return !state.pending && state.projection?.status === "connected" && !state.projection.active_main
    && Boolean(state.projection.recommended_main_selection && hubReviewTarget(state.projection));
}
export function useHubRecommendation(state: HubUiState): void {
  if (!hubCanUseRecommendation(state) || !state.projection?.recommended_main_selection) return;
  const selection = structuredClone(state.projection.recommended_main_selection);
  state.drafts.main = { selection, affinityText: String(selection.affinity_turns), capabilitiesText: selection.required_capabilities.join(", "),
    dirty: true, target: hubReviewTarget(state.projection) };
  state.error = ""; state.errorContext = null;
}
export function editHubField(state: HubUiState, field: string, value: string, checked: boolean): void {
  if (state.pending) return;
  if (field === "endpoint" || field === "label") {
    state[field] = value;
    state.connectionTouched = true;
    return;
  }
  if (field === "token") { state.connectionTouched = true; return; }
  const [context, key, ...idParts] = field.split(":");
  if (context !== "main" && context !== "side_chat") return;
  const draft = state.drafts[context];
  if (!draft.dirty && state.projection) draft.target = hubReviewTarget(state.projection);
  draft.dirty = true;
  if (key === "model") {
    const id = idParts.join(":");
    const allowed = new Set(draft.selection.allowed_model_ids);
    if (checked) allowed.add(id); else allowed.delete(id);
    draft.selection.allowed_model_ids = [...allowed];
    if (!allowed.has(draft.selection.preferred_model_id)) draft.selection.preferred_model_id = [...allowed][0] ?? "";
  }
  if (key === "preferred") draft.selection.preferred_model_id = value;
  if (key === "wait" && (value === "wait_for_preferred" || value === "allow_selected_fallback")) draft.selection.wait_policy = value;
  if (key === "affinity") draft.affinityText = value;
  if (key === "capabilities") draft.capabilitiesText = value;
}
export function hubErrorText(code: string | null | undefined): string {
  const messages: Record<string, string> = {
    unauthorized: "Hubの認証に失敗しました。接続用トークンを確認してください。",
    invalid_connection: "接続先とトークンを確認してください。HTTPはlocalhostのみ利用できます。",
    unavailable: "Hubに接続できません。Hubが起動しているか確認してください。",
    deadline: "Hubからの応答が時間内に届きませんでした。",
    different_hub: "保存済みとは異なるHubです。接続先とHubの識別情報を確認してください。",
    review_required: "Hubの内容が更新されました。最新情報を確認し、モデル選択を保存してください。",
    catalog_changed: "モデル情報が更新されました。再取得して選択内容を確認してください。",
    revision_rollback: "Hubの更新番号が保存済みの状態より古くなっています。Hubの設定を確認してください。",
    invalid_catalog: "Hubからのモデル情報を確認できませんでした。",
    settings_changed: "保存内容が更新されています。最新情報を取得してから、入力内容を確認して再保存してください。",
    connection_changed: "接続状態が変わりました。最新情報を取得してください。",
    settings_invalid: "Hub設定ファイルを読み込めません。保存データを確認してください。",
    settings_unavailable: "Hub設定を保存できません。保存先を確認してください。",
    settings_busy: "別の操作がHub設定を保存しています。少し待ってから再度お試しください。",
    model_removed: "選択したモデルが削除されています。選択を見直してください。",
    capability_mismatch: "選択モデルが必要な機能を満たしていません。",
    invalid_selection: "利用候補・優先モデル・継続ターン数を確認してください。",
    route_busy: "このチャットの待機・実行が終了してから送信先を切り替えてください。",
    gateway_unavailable: "Hubの実行ゲートウェイを利用できません。Hubの管理画面で起動状態を確認してください。",
  };
  return code ? messages[code] ?? "Hubの操作を完了できませんでした。接続状態を確認して再度お試しください。" : "";
}
