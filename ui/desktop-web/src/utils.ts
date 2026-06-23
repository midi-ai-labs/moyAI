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

export function validateConfigInput(field: string, rawValue: string): { ok: boolean; message: string } {
  const value = rawValue.trim();
  if (value.length === 0) {
    return { ok: true, message: "空欄は継承または削除として扱います。" };
  }
  if (field.endsWith("base_url")) {
    try {
      const url = new URL(value);
      if (url.protocol !== "http:" && url.protocol !== "https:") {
        return { ok: false, message: "URL は http:// または https:// で始めてください。" };
      }
    } catch {
      return { ok: false, message: "URL として解釈できません。" };
    }
  }
  if (field.endsWith("_json") || field.endsWith("servers_json")) {
    try {
      JSON.parse(value);
    } catch (error) {
      return { ok: false, message: `JSON として解釈できません: ${String(error)}` };
    }
  }
  if (field.includes("enabled") || field.includes("supports_") || field.includes("include_hidden") || field.includes("parallel_tool_calls")) {
    if (!["true", "false"].includes(value.toLowerCase())) {
      return { ok: false, message: "true または false を入力してください。" };
    }
  }
  if (
    field.includes("timeout_ms") ||
    field.includes("retries") ||
    field.includes("tokens") ||
    field.includes("context_window") ||
    field.includes("max_") ||
    field.includes("top_k") ||
    field.includes("seed")
  ) {
    if (!Number.isFinite(Number(value)) || Number(value) < 0) {
      return { ok: false, message: "0 以上の数値を入力してください。" };
    }
  }
  if (field === "permissions.access_mode" && !["default", "auto_review", "full_access"].includes(value)) {
    return { ok: false, message: "default / auto_review / full_access のいずれかを入力してください。" };
  }
  return { ok: true, message: "入力形式は問題ありません。" };
}

export function shortenPath(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts.slice(-2).join(" / ") || path;
}

export function displayAccessLabel(label: string): string {
  if (label === "default") return "標準";
  if (label === "auto_review") return "自動レビュー";
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

export function humanizeError(message: string): HumanError {
  const text = message.trim();
  const lower = text.toLowerCase();
  if (lower.includes("error sending request") || lower.includes("connection refused") || lower.includes("failed to connect")) {
    return {
      title: "LLM provider に接続できません",
      hint: "LM Studio が起動しているか、Base URL が http://127.0.0.1:1234 のように到達可能なURLになっているか確認してください。",
      details: text,
    };
  }
  if (lower.includes("model") && (lower.includes("not found") || lower.includes("404"))) {
    return {
      title: "指定したモデルが見つかりません",
      hint: "Provider設定でモデル一覧を読み込み、現在ロード済みのモデルを選択してください。",
      details: text,
    };
  }
  if (lower.includes("permission") || lower.includes("denied") || lower.includes("access")) {
    return {
      title: "操作が許可されませんでした",
      hint: "アクセスモードと対象ファイルの場所を確認してください。必要なら確認ダイアログで許可してください。",
      details: text,
    };
  }
  return {
    title: "処理に失敗しました",
    hint: "設定と対象ワークスペースを確認してください。原因の切り分けには技術詳細を参照してください。",
    details: text,
  };
}

export function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}
