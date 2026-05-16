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

export function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}
