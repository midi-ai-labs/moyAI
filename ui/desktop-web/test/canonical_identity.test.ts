import assert from "node:assert/strict";
import test from "node:test";

import {
  isCanonicalOptionalUlid,
  isCanonicalU64,
  isCanonicalUlid,
  isCanonicalWorkspace,
} from "../src/canonical_identity.ts";
import {
  isExactDraftActionTarget,
  isExactPromptReviewMutationTarget,
} from "../src/composer_target_contract.ts";

test("canonical wire primitives match Rust ULID, u64, and workspace spelling", () => {
  assert.equal(isCanonicalUlid("01ARZ3NDEKTSV4RRFFQ69G5FAV"), true);
  assert.equal(isCanonicalOptionalUlid(null), true);
  assert.equal(isCanonicalU64("0"), true);
  assert.equal(isCanonicalU64("18446744073709551615"), true);
  assert.equal(isCanonicalWorkspace("C:/workspace"), true);

  for (const invalid of [
    "01arz3ndektsv4rrffq69g5fav",
    "81ARZ3NDEKTSV4RRFFQ69G5FAV",
    "01ARZ3NDEKTSV4RRFFQ69G5FAI",
    "00000000-0000-0000-0000-000000000010",
  ]) {
    assert.equal(isCanonicalUlid(invalid), false, invalid);
  }
  for (const invalid of ["", "00", "01", "-1", "18446744073709551616", 7]) {
    assert.equal(isCanonicalU64(invalid), false, String(invalid));
  }
  assert.equal(isCanonicalWorkspace(""), false);
  assert.equal(isCanonicalWorkspace("C:/work\0space"), false);
});

test("composer and Prompt Review targets preserve exact u64 owners beyond JS safe integers", () => {
  const draft = {
    workspacePath: "C:/workspace",
    sessionId: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    ownerGeneration: "18446744073709551615",
  };
  const review = {
    ...draft,
    requestId: "9007199254740993",
    expectedState: {
      kind: "idle",
      latestTurnId: "01ARZ3NDEKTSV4RRFFQ69G5FAW",
      admissionRevision: "9007199254740994",
    },
  };
  assert.equal(isExactDraftActionTarget(draft), true);
  assert.equal(isExactPromptReviewMutationTarget(review), true);
  for (const invalid of [
    { ...draft, ownerGeneration: 9_007_199_254_740_993 },
    { ...draft, ownerGeneration: "018" },
    { ...draft, ownerGeneration: "18446744073709551616" },
    { ...draft, sessionId: "00000000-0000-0000-0000-000000000010" },
    { ...review, requestId: 9_007_199_254_740_992 },
    { ...review, compatibilityFlag: false },
  ]) {
    assert.equal(
      "requestId" in invalid
        ? isExactPromptReviewMutationTarget(invalid)
        : isExactDraftActionTarget(invalid),
      false,
      JSON.stringify(invalid),
    );
  }
});
