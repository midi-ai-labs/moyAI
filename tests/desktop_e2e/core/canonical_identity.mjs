const CANONICAL_ULID = /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/;
const U64_MAX = 18_446_744_073_709_551_615n;

export function canonicalUlid(value) {
  return typeof value === "string" && CANONICAL_ULID.test(value);
}

export function canonicalOptionalUlid(value) {
  return value === null || canonicalUlid(value);
}

export function canonicalU64(value) {
  if (typeof value !== "string" || !/^(0|[1-9]\d*)$/.test(value)) return false;
  try { return BigInt(value) <= U64_MAX; }
  catch { return false; }
}

export function canonicalWorkspace(value) {
  return typeof value === "string" && value.length > 0 && !value.includes("\0");
}
