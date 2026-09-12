// The GUI inventory is a public field/option contract, not a source-location test.
export const SETTINGS_PROFILES = Object.freeze([
  "lm_studio", "openai_compatible", "openai_responses", "lm_studio_chat_completions",
]);
export const SETTINGS_ACCESS_MODES = Object.freeze(["default", "auto_review", "full_access"]);
const modelStep = new Set(["model.base_url", "model.provider_profile", "model.api_key_env", "model.context_window"]);
const required = (key, kind, extra = {}) => Object.freeze({ key, kind, ...extra,
  initialStep: modelStep.has(key) ? "provider" : key.startsWith("model.") ? "model"
    : key === "permissions.access_mode" ? "permissions"
      : /^(docling|mcp)\./.test(key) ? "tools" : "finish" });

export const SETTINGS_CONTROL_PLAN = Object.freeze([
  required("model.base_url", "url"),
  required("model.model", "model"),
  required("model.system_prompt", "prompt"),
  required("model.provider_profile", "enum", { options: SETTINGS_PROFILES }),
  required("model.api_key_env", "env"),
  required("side_chat.base_url", "url"),
  required("side_chat.model", "model"),
  required("side_chat.system_prompt", "prompt"),
  required("side_chat.provider_profile", "enum", { options: SETTINGS_PROFILES }),
  required("side_chat.context_window", "integer"),
  required("side_chat.request_timeout_ms", "integer"),
  required("side_chat.connect_timeout_ms", "integer"),
  required("side_chat.max_retries", "integer"),
  required("permissions.access_mode", "enum", { options: SETTINGS_ACCESS_MODES }),
  required("multi_agent.enabled", "boolean"),
  required("multi_agent.mode", "enum", { options: ["explicit_request_only", "proactive"] }),
  required("multi_agent.max_concurrent_agents", "integer"),
  required("multi_agent.max_concurrent_model_requests", "integer"),
  required("model.context_window", "integer"),
  required("model.request_timeout_ms", "integer"),
  required("model.connect_timeout_ms", "integer"),
  required("model.max_retries", "integer"),
  required("model.supports_tools", "boolean"),
  required("model.supports_images", "boolean"),
  required("model.parallel_tool_calls", "boolean"),
  required("model.max_parallel_predictions", "integer"),
  required("model.extra_headers_json", "headers", { sensitive: true }),
  required("shell.hide_windows", "boolean"),
  required("inspection.default_max_depth", "integer"),
  required("inspection.default_max_entries_per_dir", "integer"),
  required("inspection.max_extensions_reported", "integer"),
  required("inspection.include_hidden_by_default", "boolean"),
  required("file_guard.max_inline_read_bytes", "integer"),
  required("file_guard.large_file_warning_bytes", "integer"),
  required("file_guard.blocked_read_extensions", "extensions"),
  required("file_guard.structured_document_extensions", "extensions"),
  required("docling.enabled", "boolean"),
  required("docling.base_url", "url"),
  required("docling.timeout_ms", "integer"),
  required("docling.api_key_env", "env"),
  required("docling.headers_json", "headers", { sensitive: true }),
  required("mcp.enabled", "boolean"),
  required("mcp.servers_json", "servers", { sensitive: true }),
]);

export const SESSION_CONTROL_PLAN = Object.freeze([
  { key: "base-url", projectionKey: "base_url", kind: "url" },
  { key: "provider-profile", projectionKey: "provider_profile", kind: "enum", options: SETTINGS_PROFILES },
  { key: "api-key-env", projectionKey: "api_key_env", kind: "env" },
  { key: "model", projectionKey: "model", kind: "model" },
  { key: "context-window", projectionKey: "context_window", kind: "integer", inheritedEmpty: true },
  { key: "access-mode", projectionKey: "access_mode", kind: "enum", options: SETTINGS_ACCESS_MODES },
].map(Object.freeze));

export function assertSettingsInventory(fields) {
  const actual = fields.map(field => field.key).sort();
  const expected = SETTINGS_CONTROL_PLAN.map(field => field.key).sort();
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`Settings field inventory drift: ${JSON.stringify({ actual, expected })}`);
  for (const row of SETTINGS_CONTROL_PLAN) {
    const field = fields.find(field => field.key === row.key);
    const type = ["headers", "servers"].includes(row.kind) ? "json"
      : ["url", "model", "prompt", "env", "extensions"].includes(row.kind) ? "string" : row.kind;
    if (field.value_type !== type || JSON.stringify(field.options) !== JSON.stringify(row.options ?? [])) {
      throw new Error(`Settings type/options drift: ${row.key}`);
    }
  }
  return true;
}

export function controlValidValue(row, field, { baseUrl, variant = 1 } = {}) {
  if (row.kind === "enum") return row.options[variant % row.options.length];
  if (row.kind === "boolean") return variant % 2 === 1 ? "true" : "false";
  if (row.kind === "url") return `${baseUrl}/audit${variant}`;
  if (row.kind === "model") return `e2e/audit-model-${variant}`;
  if (row.kind === "prompt") return `設定試験 ${variant}\nsecond line`;
  if (row.kind === "env") return `MOYAI_E2E_UNUSED_AUDIT_${variant}`;
  if (row.kind === "extensions") return variant === 1 ? "audit, fixture" : "sample, audit";
  if (row.kind === "headers") return JSON.stringify({ "X-Moyai-Audit": `throwaway-${variant}` });
  // Disabled HTTP entry tests structured data persistence without starting an external client.
  if (row.kind === "servers") return JSON.stringify([{ id: `audit-${variant}`, enabled: false, transport: "http", base_url: `${baseUrl}/mcp`, timeout_ms: 1000, headers: {} }]);
  if (row.kind === "integer") {
    const current = Number(field.value || 0);
    const next = Math.max(field.min_value ?? 0, current + variant);
    const maximum = field.max_value ?? Number.MAX_SAFE_INTEGER;
    return String(next <= maximum ? next : Math.max(field.min_value ?? 0, current - variant));
  }
  throw new TypeError(`unhandled Settings field: ${row.key}`);
}

export function controlInvalidValues(row, field) {
  if (row.kind === "url") return ["not-a-url", "http://127.0.0.1:9/?invalid=1"];
  if (row.kind === "model") return [""];
  if (row.kind === "headers" || row.kind === "servers") return ["{"];
  if (row.kind === "integer") {
    const values = ["not-a-number", String((field.min_value ?? 1) - 1)];
    if (field.max_value !== null && field.max_value !== undefined && field.max_value < Number.MAX_SAFE_INTEGER) values.push(String(field.max_value + 1));
    return [...new Set(values)];
  }
  // Native enums/checkboxes cannot create arbitrary invalid tokens. Optional strings
  // have no rejected ordinary token; long prompt boundary remains an explicit separate case.
  return [];
}

export function publicFieldMatches(field, expected, row) {
  if (!field) return false;
  if (row.sensitive) return field.sensitive === true && field.configured === true && field.value === "";
  return field.value === expected;
}
