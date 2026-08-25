import { classifySemanticTargetSettlement } from "./semantic_target_settlement.mjs";

export function selectedRestartSessionRow(projection) {
  const rows = projection?.selected_project_index >= 0
    ? projection?.session_rows
    : projection?.chat_session_rows;
  if (!Array.isArray(rows) || !Number.isInteger(projection?.selected_session_index)) return null;
  return projection.selected_session_index >= 0
    ? rows[projection.selected_session_index] ?? null
    : null;
}

export function restartTurnPageMetadata(projection) {
  const offset = projection?.turn_page_offset;
  const limit = projection?.turn_page_limit;
  const total = projection?.turn_page_total;
  if (!Number.isSafeInteger(offset) || offset < 0
    || !Number.isSafeInteger(limit) || limit <= 0
    || !Number.isSafeInteger(total) || total < 0
    || offset > total
    || typeof projection?.turn_page_has_more !== "boolean") {
    return null;
  }
  return { offset, limit, total, has_more: projection.turn_page_has_more };
}

export function classifyRestartTurnPage(
  projection,
  {
    expectedSessionId,
    expectedTurnId,
    expectedAdmissionRevision,
    expectedTotal,
    expectedLimit,
    requireLatestSuffix = false,
  } = {},
) {
  for (const [name, value] of Object.entries({
    expectedSessionId,
    expectedTurnId,
    expectedAdmissionRevision,
  })) {
    if (typeof value !== "string" || value.length === 0) {
      throw new TypeError(`${name} must be a non-empty string`);
    }
  }
  if (!Number.isSafeInteger(expectedTotal) || expectedTotal < 1) {
    throw new TypeError("expectedTotal must be a positive safe integer");
  }
  if (!Number.isSafeInteger(expectedLimit) || expectedLimit < 1) {
    throw new TypeError("expectedLimit must be a positive safe integer");
  }
  if (typeof requireLatestSuffix !== "boolean") {
    throw new TypeError("requireLatestSuffix must be boolean");
  }

  const failures = [];
  const row = selectedRestartSessionRow(projection);
  const expectedState = projection?.run_target?.expectedState;
  const metadata = restartTurnPageMetadata(projection);
  if (row?.session_id !== expectedSessionId) failures.push("restart-session-id-mismatch");
  if (expectedState?.kind !== "idle" || expectedState.latestTurnId !== expectedTurnId) {
    failures.push("restart-latest-turn-mismatch");
  }
  if (expectedState?.admissionRevision !== expectedAdmissionRevision
    || row?.admission_revision !== expectedAdmissionRevision) {
    failures.push("restart-admission-revision-mismatch");
  }
  if (metadata === null) {
    failures.push("restart-turn-page-invalid");
  } else {
    if (metadata.total !== expectedTotal) failures.push("restart-turn-page-total-mismatch");
    if (metadata.limit !== expectedLimit) failures.push("restart-turn-page-limit-mismatch");
    if (metadata.has_more !== false) failures.push("restart-turn-page-tail-missing");
    if (requireLatestSuffix
      && metadata.offset !== Math.max(0, metadata.total - metadata.limit)) {
      failures.push("restart-turn-page-not-latest-suffix");
    }
  }
  if (failures.length > 0) {
    return { decision: "fail", failures: [...new Set(failures)], metadata };
  }
  const pending = projection?.navigation_loading === true
    || projection?.turn_page_admission_open !== true
    || (Array.isArray(projection?.pending_async_operations)
      && projection.pending_async_operations.includes("turn_page_load"));
  if (pending) return { decision: "pending", failures: [], metadata };
  return {
    decision: metadata.offset === 0 ? "ready" : "page_needed",
    failures: [],
    metadata,
  };
}

export function restartPreviousPageTransitionFailures({ before, after }) {
  const failures = [];
  if (before === null || after === null) return ["restart-turn-page-transition-invalid"];
  if (before.total !== after.total) failures.push("restart-turn-page-total-drift");
  if (before.limit !== after.limit) failures.push("restart-turn-page-limit-drift");
  if (before.has_more !== false || after.has_more !== false) {
    failures.push("restart-turn-page-tail-drift");
  }
  const expectedOffset = Math.max(0, before.offset - before.limit);
  if (after.offset !== expectedOffset || after.offset >= before.offset) {
    failures.push("restart-turn-page-offset-drift");
  }
  return [...new Set(failures)];
}

export function classifyRestartHistoryTarget(target) {
  const classified = classifySemanticTargetSettlement(target, {
    expectedIdentity: { tag: "BUTTON", action: "load-previous-turn-page" },
  });
  return {
    decision: classified.decision,
    failures: classified.failures.map((failure) => failure.replace(
      /^semantic-target-/,
      "restart-history-target-",
    )),
  };
}
