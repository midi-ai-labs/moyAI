const CANONICAL_ULID = /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/;
const U64_MAX = 18_446_744_073_709_551_615n;

/** Matches the uppercase 26-character string emitted by Rust's `ulid::Ulid` Display impl. */
export function isCanonicalUlid(value: unknown): value is string {
  return typeof value === "string" && CANONICAL_ULID.test(value);
}

export function isCanonicalOptionalUlid(value: unknown): value is string | null {
  return value === null || isCanonicalUlid(value);
}

/** Matches the canonical decimal spelling used for every Rust `u64` wire owner. */
export function isCanonicalU64(value: unknown): value is string {
  if (typeof value !== "string" || !/^(0|[1-9]\d*)$/.test(value)) return false;
  try {
    return BigInt(value) <= U64_MAX;
  } catch {
    return false;
  }
}

/** Validates the shared non-empty, NUL-free workspace owner carried by commands. */
export function isCanonicalWorkspace(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 && !value.includes("\0");
}
