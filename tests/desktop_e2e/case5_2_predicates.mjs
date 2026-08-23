const REQUIRED_DOCUMENTS = Object.freeze([
  "README.md",
  "basic_design.md",
  "detail_design.md",
  "evidence_matrix.md",
  "cancel_contract.md",
]);

const STAGE1_DOCUMENTS = Object.freeze(REQUIRED_DOCUMENTS.slice(0, 4));
const DECIMAL_U64 = /^\d+$/;
const NON_CONVERGENCE_ELAPSED_MS = 10 * 60 * 1000;
const NON_CONVERGENCE_REPEAT_COUNT = 3;

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function uniqueSortedStrings(value) {
  if (!Array.isArray(value) || value.some((entry) => typeof entry !== "string")) return null;
  const sorted = [...value].sort((left, right) => left.localeCompare(right));
  return new Set(sorted).size === sorted.length ? sorted : null;
}

function selectedSessionRow(projection) {
  const rows = projection?.selected_project_index >= 0
    ? projection?.session_rows
    : projection?.chat_session_rows;
  if (!Array.isArray(rows) || !Number.isInteger(projection?.selected_session_index)) return null;
  return projection.selected_session_index >= 0
    ? rows[projection.selected_session_index] ?? null
    : null;
}

function lastRowOfKind(rows, kind) {
  return Array.isArray(rows)
    ? rows.findLast((row) => row?.row_kind === kind) ?? null
    : null;
}

export function case52NormalTerminalFailures(
  projection,
  {
    expectedSessionId = null,
    expectedTurnId = null,
    expectedPrompt = null,
    minimumCompletedSummaryCount = 1,
  } = {},
) {
  if (!Number.isInteger(minimumCompletedSummaryCount) || minimumCompletedSummaryCount < 1) {
    throw new TypeError("minimumCompletedSummaryCount must be a positive integer");
  }
  for (const [name, value] of Object.entries({ expectedSessionId, expectedTurnId, expectedPrompt })) {
    if (value !== null && (typeof value !== "string" || value.length === 0)) {
      throw new TypeError(`${name} must be a non-empty string or null`);
    }
  }

  const failures = [];
  if (projection === null || typeof projection !== "object" || Array.isArray(projection)) {
    return ["projection-invalid"];
  }
  if (projection.run_status_key !== "completed") failures.push("run-not-completed");
  if (projection.task_activity_state !== "idle") failures.push("task-activity-not-idle");
  if (projection.busy !== false
    || projection.agent_tree_active !== false
    || projection.post_run_refresh_pending !== false
    || projection.background_mutation_pending !== false
    || projection.async_polling_required !== false
    || !Array.isArray(projection.pending_async_operations)
    || projection.pending_async_operations.length !== 0
    || projection.navigation_loading !== false
    || projection.navigation_admission_open !== true
    || projection.turn_page_admission_open !== true
    || projection.provider_loading !== false) {
    failures.push("projection-not-settled");
  }
  if (projection.overlay !== "none"
    || projection.confirmation_visible !== false
    || projection.confirmation_id !== null
    || projection.confirmation != null) {
    failures.push("blocking-interaction-visible");
  }
  if (projection.draft_prompt !== ""
    || projection.composer_submit_mode !== "new_request"
    || projection.can_submit !== true) {
    failures.push("composer-not-rearmed");
  }

  const row = selectedSessionRow(projection);
  if (row === null) {
    failures.push("selected-session-row-missing");
  } else {
    if (typeof row.session_id !== "string" || row.session_id.length === 0) {
      failures.push("selected-session-id-invalid");
    }
    if (expectedSessionId !== null && row.session_id !== expectedSessionId) {
      failures.push("selected-session-id-mismatch");
    }
    if (row.status !== "completed"
      || row.loaded_status !== "idle"
      || row.active_turn_id != null
      || row.interrupt_target != null
      || row.pending_permission_requests !== 0
      || row.pending_user_input_requests !== 0) {
      failures.push("selected-session-not-terminal");
    }
  }

  const expectedState = projection?.run_target?.expectedState;
  if (expectedState?.kind !== "idle"
    || typeof expectedState.latestTurnId !== "string"
    || expectedState.latestTurnId.length === 0
    || typeof expectedState.admissionRevision !== "string"
    || !DECIMAL_U64.test(expectedState.admissionRevision)) {
    failures.push("idle-run-owner-invalid");
  } else {
    if (expectedTurnId !== null && expectedState.latestTurnId !== expectedTurnId) {
      failures.push("terminal-turn-id-mismatch");
    }
    if (row !== null && row.admission_revision !== expectedState.admissionRevision) {
      failures.push("terminal-admission-revision-mismatch");
    }
  }

  const transcript = projection.transcript_rows;
  const expectedSummaryIdentity = expectedTurnId === null
    ? null
    : `turn:${expectedTurnId}:work-summary`;
  const completedSummaryCount = Array.isArray(transcript)
    ? transcript.filter((item) => item?.row_kind === "work_summary_completed"
      && (expectedSummaryIdentity === null
        || item?.stable_history_identity === expectedSummaryIdentity)).length
    : 0;
  if (completedSummaryCount < minimumCompletedSummaryCount) {
    failures.push("completed-summary-missing");
  }
  if (expectedPrompt !== null && lastRowOfKind(transcript, "user")?.body !== expectedPrompt) {
    failures.push("terminal-user-prompt-mismatch");
  }
  return [...new Set(failures)];
}

export function classifyCase52NormalTerminal(projection, options = {}) {
  const failures = case52NormalTerminalFailures(projection, options);
  const selected = selectedSessionRow(projection);
  const expectedSessionId = options.expectedSessionId ?? null;
  const selectedDurableFailure = selected !== null
    && (expectedSessionId === null || selected.session_id === expectedSessionId)
    && (selected.status === "failed" || selected.status === "cancelled");
  const terminalProductFailure = projection?.run_status_key === "failed"
    || projection?.run_status_key === "cancelled"
    || selectedDurableFailure;
  const activeWork = projection !== null
    && typeof projection === "object"
    && !Array.isArray(projection)
    && (
      projection.run_status_key === "running"
      || projection.task_activity_state === "running"
      || projection.task_activity_state === "finalizing"
      || projection.task_activity_state === "attention"
      || projection.busy === true
      || projection.agent_tree_active === true
      || projection.post_run_refresh_pending === true
      || projection.background_mutation_pending === true
      || projection.async_polling_required === true
      || (Array.isArray(projection.pending_async_operations) && projection.pending_async_operations.length > 0)
      || projection.navigation_loading === true
      || projection.provider_loading === true
    );
  return {
    decision: failures.length === 0 ? "pass" : terminalProductFailure || !activeWork ? "fail" : "pending",
    failures,
  };
}

export function case52RestartContinuityFailures({
  beforeSessionId,
  afterSessionId,
  beforeHistory,
  afterHistory,
}) {
  const failures = [];
  if (typeof beforeSessionId !== "string" || beforeSessionId.length === 0
    || typeof afterSessionId !== "string" || afterSessionId.length === 0) {
    failures.push("restart-session-id-invalid");
  } else if (beforeSessionId !== afterSessionId) {
    failures.push("restart-session-id-mismatch");
  }
  if (!Array.isArray(beforeHistory) || beforeHistory.length === 0 || !Array.isArray(afterHistory)) {
    failures.push("restart-history-invalid");
  } else if (afterHistory.length < beforeHistory.length) {
    failures.push("restart-history-truncated");
  } else {
    const mismatchIndex = beforeHistory.findIndex((row, index) => !sameValue(row, afterHistory[index]));
    if (mismatchIndex >= 0) failures.push("restart-history-prefix-mismatch");
  }
  return failures;
}

export function case52RestartContinuityAccepted(value) {
  return case52RestartContinuityFailures(value).length === 0;
}

export function classifyCase52RestartContinuity(
  projection,
  {
    beforeSessionId,
    beforeHistory,
    expectedTurnId = null,
    expectedPrompt = null,
    minimumCompletedSummaryCount = 1,
  } = {},
) {
  const terminal = classifyCase52NormalTerminal(projection, {
    expectedSessionId: beforeSessionId ?? null,
    expectedTurnId,
    expectedPrompt,
    minimumCompletedSummaryCount,
  });
  const continuityFailures = case52RestartContinuityFailures({
    beforeSessionId,
    afterSessionId: selectedSessionRow(projection)?.session_id ?? null,
    beforeHistory,
    afterHistory: Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : null,
  });
  const failures = [...new Set([...terminal.failures, ...continuityFailures])];
  return {
    decision: terminal.decision === "pending"
      ? "pending"
      : terminal.decision === "fail" || continuityFailures.length > 0
        ? "fail"
        : "pass",
    failures,
    terminal_failures: terminal.failures,
    continuity_failures: continuityFailures,
  };
}

function documentMap(manifest) {
  if (!Array.isArray(manifest?.documents)) return null;
  const result = new Map();
  for (const row of manifest.documents) {
    if (typeof row?.name !== "string" || result.has(row.name)) return null;
    result.set(row.name, row);
  }
  return result;
}

function fileMap(manifest) {
  if (!Array.isArray(manifest?.files)) return null;
  const result = new Map();
  for (const row of manifest.files) {
    if (typeof row?.path !== "string"
      || typeof row?.sha256 !== "string"
      || !Number.isInteger(row?.bytes)
      || result.has(row.path)) return null;
    result.set(row.path, row);
  }
  return result;
}

function manifestDiffFailures(manifest, expectedAdded) {
  const failures = [];
  const modified = uniqueSortedStrings(manifest?.diff?.modified);
  const deleted = uniqueSortedStrings(manifest?.diff?.deleted);
  const added = uniqueSortedStrings(manifest?.diff?.added);
  if (modified === null || deleted === null || added === null) return ["stage-diff-invalid"];
  if (modified.length !== 0) failures.push("stage-baseline-file-modified");
  if (deleted.length !== 0) failures.push("stage-baseline-file-deleted");
  if (!sameValue(added, [...expectedAdded].sort((left, right) => left.localeCompare(right)))) {
    failures.push("stage-added-paths-mismatch");
  }
  return failures;
}

export function case52Stage1ManifestFailures(manifest) {
  const failures = manifestDiffFailures(manifest, STAGE1_DOCUMENTS);
  const documents = documentMap(manifest);
  if (documents === null) {
    failures.push("stage-documents-invalid");
  } else {
    for (const name of STAGE1_DOCUMENTS) {
      const row = documents.get(name);
      if (row?.exists !== true || !Number.isInteger(row?.bytes) || row.bytes <= 0) {
        failures.push(`stage1-document-missing:${name}`);
      }
    }
    if (documents.get("cancel_contract.md")?.exists === true) {
      failures.push("stage1-cancel-contract-created-early");
    }
  }
  if (!Number.isInteger(manifest?.evidence_matrix_rows) || manifest.evidence_matrix_rows < 25) {
    failures.push("stage1-evidence-matrix-too-small");
  }
  return failures;
}

export function case52Stage2ManifestFailures(stage1, stage2) {
  const failures = manifestDiffFailures(stage2, REQUIRED_DOCUMENTS);
  if (typeof stage1?.baseline_aggregate_sha256 !== "string"
    || stage1.baseline_aggregate_sha256.length === 0
    || stage2?.baseline_aggregate_sha256 !== stage1.baseline_aggregate_sha256) {
    failures.push("stage-baseline-identity-mismatch");
  }
  const stage1Files = fileMap(stage1);
  const stage2Files = fileMap(stage2);
  if (stage1Files === null || stage2Files === null) {
    failures.push("stage-files-invalid");
  } else {
    const added = [...stage2Files.keys()].filter((name) => !stage1Files.has(name)).sort();
    const deleted = [...stage1Files.keys()].filter((name) => !stage2Files.has(name)).sort();
    const modified = [...stage1Files.entries()]
      .filter(([name, row]) => stage2Files.has(name) && !sameValue(row, stage2Files.get(name)))
      .map(([name]) => name)
      .sort();
    if (!sameValue(added, ["cancel_contract.md"]) || deleted.length !== 0 || modified.length !== 0) {
      failures.push("stage2-only-cancel-contract-not-preserved");
    }
  }
  const documents = documentMap(stage2);
  if (documents === null) {
    failures.push("stage-documents-invalid");
  } else {
    for (const name of REQUIRED_DOCUMENTS) {
      const row = documents.get(name);
      if (row?.exists !== true || !Number.isInteger(row?.bytes) || row.bytes <= 0) {
        failures.push(`stage2-document-missing:${name}`);
      }
    }
  }
  if (!Number.isInteger(stage2?.evidence_matrix_rows) || stage2.evidence_matrix_rows < 25) {
    failures.push("stage2-evidence-matrix-too-small");
  }
  return [...new Set(failures)];
}

export function case52EvaluatorFailures(report) {
  if (report === null || typeof report !== "object" || Array.isArray(report)) {
    return ["evaluation-report-invalid"];
  }
  const failures = [];
  if (report.public_suite_pass !== true || report?.public_suite?.exit_code !== 0) {
    failures.push("public-suite-failed");
  }
  if (report.hidden_oracle_pass !== true || report?.hidden_oracle?.exit_code !== 0) {
    failures.push("hidden-oracle-failed");
  }
  if (report.all_required_documents !== true) failures.push("required-documents-incomplete");
  const documents = documentMap(report);
  if (documents === null) {
    failures.push("evaluation-documents-invalid");
  } else {
    for (const name of REQUIRED_DOCUMENTS) {
      const row = documents.get(name);
      if (row?.exists !== true || !Number.isInteger(row?.bytes) || row.bytes <= 0) {
        failures.push(`evaluation-document-missing:${name}`);
      }
    }
  }
  return failures;
}

export function case52EvaluatorAccepted(report) {
  return case52EvaluatorFailures(report).length === 0;
}

function nonNegativeInteger(value, name) {
  if (!Number.isInteger(value) || value < 0) throw new TypeError(`${name} must be a non-negative integer`);
  return value;
}

export function classifyCase52NonConvergence({
  elapsedMs,
  requiredArtifactCount,
  repeatedNextActionCount,
  repeatedSourceReadCount,
}) {
  if (!Number.isFinite(elapsedMs) || elapsedMs < 0) {
    throw new TypeError("elapsedMs must be a non-negative finite number");
  }
  nonNegativeInteger(requiredArtifactCount, "requiredArtifactCount");
  nonNegativeInteger(repeatedNextActionCount, "repeatedNextActionCount");
  nonNegativeInteger(repeatedSourceReadCount, "repeatedSourceReadCount");
  const noArtifact = requiredArtifactCount === 0;
  const elapsedExceeded = elapsedMs > NON_CONVERGENCE_ELAPSED_MS;
  const repeatedAction = repeatedNextActionCount >= NON_CONVERGENCE_REPEAT_COUNT;
  const repeatedRead = repeatedSourceReadCount >= NON_CONVERGENCE_REPEAT_COUNT;
  const stop = noArtifact && elapsedExceeded && (repeatedAction || repeatedRead);
  return {
    decision: stop ? "stop" : "continue",
    stop,
    triggers: {
      no_required_artifact: noArtifact,
      elapsed_exceeded: elapsedExceeded,
      repeated_next_action: repeatedAction,
      repeated_source_read: repeatedRead,
    },
    thresholds: {
      elapsed_ms_exclusive: NON_CONVERGENCE_ELAPSED_MS,
      repeated_count_inclusive: NON_CONVERGENCE_REPEAT_COUNT,
    },
  };
}

export const CASE5_2_REQUIRED_DOCUMENTS = REQUIRED_DOCUMENTS;
