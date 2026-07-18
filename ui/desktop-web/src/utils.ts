import { commandErrorInfo } from "./command_error.ts";
import type { ConfigFieldProjection } from "./types.ts";

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

export function validateConfigInput(
  field: ConfigFieldProjection,
  rawValue: string,
): { ok: boolean; message: string } {
  const value = rawValue.trim();
  if (value.length === 0) {
    if (field.required) return { ok: false, message: "値を入力してください。" };
    return { ok: true, message: "空欄は継承または削除として扱います。" };
  }
  if (field.key.endsWith("base_url")) {
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
    if (!Number.isFinite(Number(value))) {
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

export function shortenPath(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts.slice(-2).join(" / ") || path;
}

export function displayAccessLabel(label: string): string {
  if (label === "default") return "標準";
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
