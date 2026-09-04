import { commandErrorInfo } from "./command_error.ts";
import type { ConfigFieldProjection, ProviderStatusProjection } from "./types.ts";

export interface ProviderBaseUrlValidation {
  ok: boolean;
  message: string;
  canonicalBaseUrl: string;
}

/** Mirrors Rust's ProviderEndpoint parser without returning rejected endpoint text. */
export function validateProviderBaseUrl(rawValue: string): ProviderBaseUrlValidation {
  const value = rawValue.trim();
  if (value.length === 0) {
    return { ok: false, message: "URL を入力してください。", canonicalBaseUrl: "" };
  }
  try {
    const url = new URL(value);
    if (url.protocol !== "http:" && url.protocol !== "https:") {
      return {
        ok: false,
        message: "URL は http:// または https:// で始めてください。",
        canonicalBaseUrl: "",
      };
    }
    if (!url.hostname) {
      return { ok: false, message: "URL にはホスト名が必要です。", canonicalBaseUrl: "" };
    }
    const schemeBoundary = value.indexOf("://");
    const remainder = schemeBoundary >= 0 ? value.slice(schemeBoundary + 3) : "";
    const authorityEnd = remainder.search(/[/?#]/);
    const authority = authorityEnd >= 0 ? remainder.slice(0, authorityEnd) : remainder;
    const suffix = authorityEnd >= 0 ? remainder.slice(authorityEnd) : "";
    if (url.username || url.password || authority.includes("@")) {
      return {
        ok: false,
        message: "URL に認証情報を含めず、API key またはheader設定を使用してください。",
        canonicalBaseUrl: "",
      };
    }
    if (url.search || suffix.includes("?")) {
      return { ok: false, message: "URL にquery stringは指定できません。", canonicalBaseUrl: "" };
    }
    if (url.hash || suffix.includes("#")) {
      return { ok: false, message: "URL にfragmentは指定できません。", canonicalBaseUrl: "" };
    }

    const path = url.pathname.replace(/\/+$/, "");
    url.pathname = path || "/";
    return {
      ok: true,
      message: "URL の形式は問題ありません。",
      canonicalBaseUrl: url.toString().replace(/\/+$/, ""),
    };
  } catch {
    return { ok: false, message: "URL として解釈できません。", canonicalBaseUrl: "" };
  }
}

export interface ProviderOverlayFeedback {
  baseUrl: ProviderBaseUrlValidation;
  status: ProviderStatusProjection;
}

/** Keeps the Provider overlay's visible feedback and action gate on the same URL parser result. */
export function providerOverlayFeedback(
  rawBaseUrl: string,
  providerStatus: ProviderStatusProjection,
): ProviderOverlayFeedback {
  const baseUrl = validateProviderBaseUrl(rawBaseUrl);
  return {
    baseUrl,
    status: baseUrl.ok
      ? providerStatus
      : {
        kind: "error",
        title: "ベースURLを確認してください",
        hint: baseUrl.message,
        details: "",
      },
  };
}

export const USER_CONFIGURED_SYSTEM_PROMPT_MAX_CHARS = 16_384;

function unicodeCharacterCount(value: string): number {
  return Array.from(value).length;
}

export function fileName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

export function lineValue(text: string, label: string): string {
  const prefix = `${label}:`;
  const line = text
    .split("\n")
    .map((value) => value.trim())
    .find((value) => value.startsWith(prefix));
  return line ? line.slice(prefix.length).trim() : "";
}

export interface ConfigFieldValue {
  key: string;
  text: string;
}

/** Mirrors Rust's canonical Docling base URL plus its explicit `/ready` route. */
export function doclingReadinessEndpoint(rawBaseUrl: string): string | null {
  try {
    const url = new URL(rawBaseUrl.trim());
    if ((url.protocol !== "http:" && url.protocol !== "https:")
      || !url.hostname
      || url.username
      || url.password
      || url.search
      || url.hash
    ) return null;
    const path = url.pathname.replace(/\/+$/, "");
    url.pathname = `${path || ""}/ready`;
    return url.toString();
  } catch {
    return null;
  }
}

export function validateConfigInput(
  field: ConfigFieldProjection,
  rawValue: string,
  contextualValues: readonly ConfigFieldValue[] = [],
): { ok: boolean; message: string } {
  const value = rawValue.trim();
  const doclingDisabled = contextualValues.find(({ key }) => key === "docling.enabled")
    ?.text.trim().toLowerCase() === "false";
  if (field.key === "docling.base_url" && doclingDisabled) {
    return { ok: true, message: "Doclingが無効な間は入力値を保持します。" };
  }
  if (value.length === 0) {
    if (field.required) return { ok: false, message: "値を入力してください。" };
    return { ok: true, message: "空欄は継承または削除として扱います。" };
  }
  if (
    (field.key === "model.system_prompt" || field.key === "side_chat.system_prompt")
    && unicodeCharacterCount(value) > USER_CONFIGURED_SYSTEM_PROMPT_MAX_CHARS
  ) {
    return {
      ok: false,
      message: `追加システムプロンプトは${USER_CONFIGURED_SYSTEM_PROMPT_MAX_CHARS.toLocaleString("ja-JP")}文字以内で入力してください。`,
    };
  }
  if (
    field.key === "model.base_url"
    || field.key === "side_chat.base_url"
    || field.key === "docling.base_url"
  ) {
    const validation = validateProviderBaseUrl(value);
    if (!validation.ok) return { ok: false, message: validation.message };
  } else if (field.key.endsWith("base_url")) {
    try {
      const url = new URL(value);
      if (url.protocol !== "http:" && url.protocol !== "https:") {
        return { ok: false, message: "URL は http:// または https:// で始めてください。" };
      }
    } catch {
      return { ok: false, message: "URL として解釈できません。" };
    }
  }
  if (field.value_type === "json") {
    try {
      JSON.parse(value);
    } catch (error) {
      return { ok: false, message: `JSON として解釈できません: ${String(error)}` };
    }
  }
  if (field.value_type === "boolean") {
    if (!["true", "false"].includes(value.toLowerCase())) {
      return { ok: false, message: "true または false を入力してください。" };
    }
  }
  if (field.value_type === "integer") {
    if (!/^[+-]?\d+$/.test(value) || !Number.isSafeInteger(Number(value))) {
      return { ok: false, message: "整数を入力してください。" };
    }
  }
  if (field.value_type === "number") {
    const decimal = /^[+-]?(?:(?:\d+(?:\.\d*)?)|(?:\.\d+))(?:[eE][+-]?\d+)?$/;
    if (!decimal.test(value) || !Number.isFinite(Number(value))) {
      return { ok: false, message: "有限の数値を入力してください。" };
    }
  }
  if (field.value_type === "enum" && !field.options.includes(value)) {
    return { ok: false, message: `${field.options.join(" / ")} のいずれかを入力してください。` };
  }
  if ((field.value_type === "integer" || field.value_type === "number") && field.min_value !== null) {
    if (Number(value) < field.min_value) {
      return { ok: false, message: `${field.min_value} 以上の数値を入力してください。` };
    }
  }
  if ((field.value_type === "integer" || field.value_type === "number") && field.max_value !== null) {
    if (Number(value) > field.max_value) {
      return { ok: false, message: `${field.max_value} 以下の数値を入力してください。` };
    }
  }
  return { ok: true, message: "入力形式は問題ありません。" };
}

export interface ConfigFieldValidationResult {
  ok: boolean;
  invalidKey: string | null;
  message: string;
}

/** Use the same field parser metadata for live DOM validation and render-time commit gating. */
export function validateConfigFieldValues(
  fields: readonly ConfigFieldProjection[],
  values: readonly ConfigFieldValue[] = fields.map((field) => ({ key: field.key, text: field.value })),
): ConfigFieldValidationResult {
  const fieldsByKey = new Map(fields.map((field) => [field.key, field]));
  for (const value of values) {
    const field = fieldsByKey.get(value.key);
    if (!field) {
      return {
        ok: false,
        invalidKey: value.key,
        message: "設定項目が見つかりません。",
      };
    }
    const validation = validateConfigInput(field, value.text, values);
    if (!validation.ok) {
      return {
        ok: false,
        invalidKey: value.key,
        message: validation.message,
      };
    }
  }
  return { ok: true, invalidKey: null, message: "入力形式は問題ありません。" };
}

export function configCommitControlState(
  capabilityOpen: boolean,
  validationOk: boolean,
): { disabled: boolean; ariaDisabled: "true" | "false" } {
  const disabled = !capabilityOpen || !validationOk;
  return { disabled, ariaDisabled: disabled ? "true" : "false" };
}

export function shortenPath(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts.slice(-2).join(" / ") || path;
}

export function displayAccessLabel(label: string): string {
  if (label === "default") return "承認を求める";
  if (label === "auto_review") return "代理で承認";
  if (label === "full_access") return "フルアクセス";
  return label;
}

export function goalSlashCommandHint(prompt: string): string | null {
  const match = prompt.match(/^\s*\/goal(?:\s|$)/);
  if (!match) return null;
  const arg = prompt.slice(match[0].length).trim();
  const normalized = arg.toLowerCase();
  if (arg.length === 0) return "現在のgoalを表示します。指定: objective | clear | pause | resume";
  if (normalized === "clear") return "現在のgoalを削除します。";
  if (normalized === "pause") return "現在のgoalを一時停止します。";
  if (normalized === "resume") return "一時停止中のgoalを再開します。";
  return "このobjectiveをgoalに設定して、そのまま実行します。";
}

export interface HumanError {
  title: string;
  hint: string;
  details: string;
}

export function humanizeError(error: unknown): HumanError {
  const info = commandErrorInfo(error);
  const details = info.message.trim();
  switch (info.code) {
    case "provider_transport":
      return {
        title: "LLM provider に接続できません",
        hint: "Provider が起動しているか、Base URL が到達可能か確認してください。",
        details,
      };
    case "model_unavailable":
      return {
        title: "指定したモデルが見つかりません",
        hint: "Provider設定でモデル一覧を読み込み、利用可能なモデルを選択してください。",
        details,
      };
    case "image_unsupported":
      return {
        title: "このモデルは画像入力に対応していません",
        hint: "画像対応モデルを選択するか、画像添付を解除してください。",
        details,
      };
    case "permission_policy_denied":
      return {
        title: "操作が許可されませんでした",
        hint: "アクセスモードと操作対象を確認してください。",
        details,
      };
    case "unknown":
    case "runtime_failure":
    case "storage_failure":
      return {
        title: "処理に失敗しました",
        hint: "設定と対象ワークスペースを確認してください。原因の切り分けには技術詳細を参照してください。",
        details,
      };
  }
}

export function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}
