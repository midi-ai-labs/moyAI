const PRIVATE_KEYS = new Set([
  "model.api_key_env", "model.extra_headers_json", "model.extra_body_json",
  "side_chat.api_key_env", "approve.api_key_env",
]);

// The public projection is the saved baseline; only explicit edits can reuse
// private connection settings after changing that role's URL or profile.
export function expectedConfigCommandValues(projection, overrides = {}, { editedKeys = [] } = {}) {
  if (!Array.isArray(projection?.config_fields)) throw new TypeError("Config command expectation requires projected baseline fields");
  const baseline = new Map(projection.config_fields.map(field => [field.key, field.value]));
  for (const key of Object.keys(overrides)) {
    if (!baseline.has(key)) throw new TypeError(`Unknown projected config field: ${key}`);
  }
  const values = projection.config_fields.map(field => ({
    key: field.key,
    text: Object.hasOwn(overrides, field.key) ? overrides[field.key] : field.value,
  }));
  const changedRoles = new Set(values.filter(value =>
    (value.key.endsWith(".base_url") || value.key.endsWith(".provider_profile"))
    && value.text !== baseline.get(value.key)).map(value => value.key.split(".")[0]));
  const edited = new Set(editedKeys);
  return values.filter(value => {
    if (!value.key.startsWith("approve.") && !PRIVATE_KEYS.has(value.key)) return true;
    if (value.text !== baseline.get(value.key)) return true;
    return PRIVATE_KEYS.has(value.key) && changedRoles.has(value.key.split(".")[0]) && edited.has(value.key);
  });
}
