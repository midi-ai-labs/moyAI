import assert from "node:assert/strict";
import test from "node:test";

import {
  PROMPT_REVIEW_ENHANCED_TEXT,
  PROMPT_REVIEW_RAW_TEXT,
  createStablePromptReviewOpenedPredicate,
  exactPromptReviewProviderLedger,
  exactRunExpectedState,
  exactRunTarget,
  promptReviewCancelledFailures,
  promptReviewOpenedFailures,
  staleEnhanceRejectedFailures,
  staleRunTarget,
} from "../scenarios/prompt_review_cancel.mjs";

const WORKSPACE = "C:\\e2e\\prompt-review";

function expected() {
  return {
    workspacePath: WORKSPACE,
    sessionId: null,
    ownerGeneration: "7",
    runtimeOwnerToken: "idle:7",
    permissionConfirmationId: null,
    expectedState: { kind: "idle", latestTurnId: null, admissionRevision: "0" },
    runTarget: {
      workspacePath: WORKSPACE,
      sessionId: null,
      runtimeOwnerToken: "idle:7",
      permissionConfirmationId: null,
      expectedState: { kind: "idle", latestTurnId: null, admissionRevision: "0" },
    },
    composerCommitGeneration: "3",
    navigationIdentity: {
      workspace_path: WORKSPACE,
      project_id: "project-1",
      project_path: WORKSPACE,
      session_id: null,
      project_row_ids: ["project-1"],
      session_row_ids: [],
    },
    rawText: PROMPT_REVIEW_RAW_TEXT,
    enhancedText: PROMPT_REVIEW_ENHANCED_TEXT,
  };
}

function ledger() {
  return [
    {
      method: "GET",
      pathname: "/v1/models",
      response_status: 200,
    },
    {
      method: "POST",
      pathname: "/v1/responses",
      response_status: 200,
      contract: { pass: true },
    },
  ];
}

function projection(overrides = {}) {
  return {
    workspace_path: WORKSPACE,
    project_rows: [{ project_id: "project-1", path: WORKSPACE }],
    selected_project_index: 0,
    session_rows: [],
    chat_session_rows: [],
    selected_session_index: -1,
    draft_target: { workspacePath: WORKSPACE, sessionId: null, ownerGeneration: "7" },
    run_target: {
      workspacePath: WORKSPACE,
      sessionId: null,
      runtimeOwnerToken: "idle:7",
      permissionConfirmationId: null,
      expectedState: { kind: "idle", latestTurnId: null, admissionRevision: "0" },
    },
    composer_commit_generation: "3",
    draft_prompt: "",
    run_status_key: "idle",
    busy: false,
    agent_tree_active: false,
    background_mutation_pending: false,
    async_polling_required: false,
    pending_async_operations: [],
    overlay: "prompt_review",
    review_target: {
      workspacePath: WORKSPACE,
      sessionId: null,
      ownerGeneration: "7",
      requestId: "11",
      expectedState: { kind: "idle", latestTurnId: null, admissionRevision: "0" },
    },
    review_raw_text: PROMPT_REVIEW_RAW_TEXT,
    review_draft_text: PROMPT_REVIEW_ENHANCED_TEXT,
    send_enhanced_enabled: true,
    send_raw_enabled: true,
    ...overrides,
  };
}

function openedSurface(overrides = {}) {
  return {
    projection: projection(),
    shell_inert: true,
    dialog_count: 1,
    dialog_visible: true,
    review_draft: {
      count: 1,
      value: PROMPT_REVIEW_ENHANCED_TEXT,
      visible: true,
      enabled: true,
    },
    review_raw: { count: 1, text: PROMPT_REVIEW_RAW_TEXT },
    cancel_button: { count: 1, visible: true, enabled: true },
    prompt: { count: 1, value: PROMPT_REVIEW_RAW_TEXT, visible: true, enabled: false },
    enhance_enabled: false,
    active: { tag: "TEXTAREA", id: "review-draft", action: null },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_modal_backdrop_count: 1,
    ...overrides,
  };
}

function cancelledSurface(overrides = {}) {
  return {
    projection: projection({
      overlay: "none",
      review_target: null,
      review_raw_text: "",
      review_draft_text: "",
      send_enhanced_enabled: false,
      send_raw_enabled: false,
    }),
    shell_inert: false,
    dialog_count: 0,
    dialog_visible: false,
    review_draft: { count: 0, value: null, visible: false, enabled: false },
    review_raw: { count: 0, text: null },
    cancel_button: { count: 0, visible: false, enabled: false },
    prompt: { count: 1, value: PROMPT_REVIEW_RAW_TEXT, visible: true, enabled: true },
    enhance_enabled: true,
    active: { tag: "TEXTAREA", id: "prompt", action: null },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_modal_backdrop_count: 0,
    ...overrides,
  };
}

test("active-turn wire contract accepts only the exact tagged union and exact run target", () => {
  const idle = { kind: "idle", latestTurnId: null, admissionRevision: "0" };
  const priorIdle = {
    kind: "idle",
    latestTurnId: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    admissionRevision: "10",
  };
  const turn = {
    kind: "turn",
    turnId: "01ARZ3NDEKTSV4RRFFQ69G5FAW",
    admissionRevision: "11",
  };
  assert.equal(exactRunExpectedState(idle), true);
  assert.equal(exactRunExpectedState(priorIdle), true);
  assert.equal(exactRunExpectedState(turn), true);
  assert.equal(exactRunExpectedState(idle, { ...idle }), true);
  assert.equal(exactRunExpectedState(
    { latestTurnId: null, admissionRevision: "0", kind: "idle" },
    { kind: "idle", latestTurnId: null, admissionRevision: "0" },
  ), true);
  assert.equal(exactRunExpectedState(
    { turnId: turn.turnId, admissionRevision: "11", kind: "turn" },
    { kind: "turn", turnId: turn.turnId, admissionRevision: "11" },
  ), true);
  assert.equal(exactRunExpectedState(idle, priorIdle), false);
  assert.equal(exactRunExpectedState({ kind: "idle" }), false);
  assert.equal(exactRunExpectedState({ kind: "idle", latestTurnId: null }), false);
  assert.equal(exactRunExpectedState({ ...idle, admissionRevision: null }), false);
  assert.equal(exactRunExpectedState({ ...idle, admissionRevision: 0 }), false);
  assert.equal(exactRunExpectedState({ ...idle, admissionRevision: "00" }), false);
  assert.equal(exactRunExpectedState({
    ...idle,
    admissionRevision: "9223372036854775807",
  }), true);
  assert.equal(exactRunExpectedState({
    ...idle,
    admissionRevision: "9223372036854775808",
  }), true);
  assert.equal(exactRunExpectedState({
    ...idle,
    admissionRevision: "18446744073709551615",
  }), true);
  assert.equal(exactRunExpectedState({
    ...idle,
    admissionRevision: "18446744073709551616",
  }), false);
  assert.equal(exactRunExpectedState({ ...idle, turnId: "extra" }), false);
  assert.equal(exactRunExpectedState({ ...idle, latestTurnId: 4 }), false);
  assert.equal(exactRunExpectedState({ ...turn, turnId: "" }), false);
  assert.equal(exactRunExpectedState({ ...turn, latestTurnId: null }), false);
  assert.equal(exactRunExpectedState({ ...turn, kind: "running" }), false);

  const owner = expected();
  assert.equal(exactRunTarget(owner.runTarget, owner), true);
  assert.equal(exactRunTarget({ ...owner.runTarget, compatibilityFlag: false }, owner), false);
  assert.equal(exactRunTarget({
    ...owner.runTarget,
    expectedState: priorIdle,
  }, owner), false);

  const staleIdle = staleRunTarget(owner.runTarget);
  assert.equal(exactRunExpectedState(staleIdle.expectedState), true);
  assert.notDeepEqual(staleIdle.expectedState, owner.expectedState);
  assert.equal(staleIdle.expectedState.latestTurnId, owner.expectedState.latestTurnId);
  assert.notEqual(staleIdle.expectedState.admissionRevision, owner.expectedState.admissionRevision);
  assert.equal(exactRunTarget(staleIdle, owner), false);
  const activeTarget = {
    ...owner.runTarget,
    runtimeOwnerToken: "root:8",
    expectedState: turn,
  };
  const staleTurn = staleRunTarget(activeTarget);
  assert.equal(staleTurn.expectedState.kind, "turn");
  assert.equal(staleTurn.expectedState.turnId, turn.turnId);
  assert.notEqual(staleTurn.expectedState.admissionRevision, turn.admissionRevision);
  assert.equal(exactRunExpectedState(staleTurn.expectedState), true);

  const staleAtMaximum = staleRunTarget({
    ...owner.runTarget,
    expectedState: { ...idle, admissionRevision: "18446744073709551615" },
  });
  assert.equal(staleAtMaximum.expectedState.admissionRevision, "18446744073709551614");
  assert.equal(exactRunExpectedState(staleAtMaximum.expectedState), true);
});

test("stale Prompt Enhance predicate requires conflict before provider, review, draft, or run mutation", () => {
  const owner = expected();
  const idleProjection = projection({
    overlay: "none",
    review_target: null,
    review_raw_text: "",
    review_draft_text: "",
    send_enhanced_enabled: false,
    send_raw_enabled: false,
  });
  const sample = {
    outcome: {
      ok: false,
      error: { kind: "conflict", message: "stale run target", state: idleProjection },
      value: null,
    },
    surface: cancelledSurface({ projection: idleProjection }),
    ledger: [],
  };
  assert.equal(sample.outcome.error.state.draft_prompt, "", "Rust does not own the pre-admission text");
  assert.equal(sample.surface.projection.draft_prompt, "", "desktop_state remains canonical Rust state");
  assert.equal(sample.surface.prompt.value, PROMPT_REVIEW_RAW_TEXT, "the textarea owns the local draft");
  assert.deepEqual(staleEnhanceRejectedFailures(sample, owner), []);

  assert.ok(staleEnhanceRejectedFailures({
    ...sample,
    outcome: { ok: true, error: null, value: idleProjection },
  }, owner).includes("stale-enhance-not-conflict"));
  assert.ok(staleEnhanceRejectedFailures({ ...sample, ledger: ledger() }, owner)
    .includes("stale-enhance-provider-contacted"));
  assert.ok(staleEnhanceRejectedFailures({
    ...sample,
    surface: cancelledSurface({
      projection: projection({
        overlay: "none",
        review_target: null,
        review_raw_text: "",
        review_draft_text: "",
        send_enhanced_enabled: false,
        send_raw_enabled: false,
        run_target: staleRunTarget(owner.runTarget),
      }),
    }),
  }, owner).includes("stale-enhance-run-target-mutated"));
  assert.ok(staleEnhanceRejectedFailures({
    ...sample,
    surface: openedSurface(),
  }, owner).includes("stale-enhance-review-created"));
});

test("Prompt Review open predicate binds one canonical exact target to provider and rendered draft", () => {
  const opened = openedSurface();
  assert.equal(opened.projection.draft_prompt, "");
  assert.equal(opened.prompt.value, PROMPT_REVIEW_RAW_TEXT);
  assert.deepEqual(promptReviewOpenedFailures(opened, ledger(), expected()), []);

  const staleRequest = openedSurface({
    projection: projection({
      review_target: {
        workspacePath: WORKSPACE,
        sessionId: null,
        ownerGeneration: "7",
        requestId: "01",
        expectedState: { kind: "idle", latestTurnId: null, admissionRevision: "0" },
      },
    }),
  });
  assert.ok(promptReviewOpenedFailures(staleRequest, ledger(), expected()).includes("review-target-not-canonical"));

  const wrongSession = openedSurface({
    projection: projection({
      review_target: {
        workspacePath: WORKSPACE,
        sessionId: "other",
        ownerGeneration: "7",
        requestId: "11",
        expectedState: { kind: "idle", latestTurnId: null, admissionRevision: "0" },
      },
    }),
  });
  assert.ok(promptReviewOpenedFailures(wrongSession, ledger(), expected()).includes("review-target-not-canonical"));

  const staleOwner = openedSurface({
    projection: projection({
      draft_target: { workspacePath: WORKSPACE, sessionId: null, ownerGeneration: "8" },
      review_target: {
        workspacePath: WORKSPACE,
        sessionId: null,
        ownerGeneration: "8",
        requestId: "11",
        expectedState: { kind: "idle", latestTurnId: null, admissionRevision: "0" },
      },
    }),
  });
  assert.ok(promptReviewOpenedFailures(staleOwner, ledger(), expected()).includes("draft-target-drift"));
  assert.ok(promptReviewOpenedFailures(staleOwner, ledger(), expected()).includes("review-target-not-canonical"));

  const staleExpectedState = openedSurface({
    projection: projection({
      review_target: {
        workspacePath: WORKSPACE,
        sessionId: null,
        ownerGeneration: "7",
        requestId: "11",
        expectedState: {
          kind: "idle",
          latestTurnId: null,
          admissionRevision: "1",
        },
      },
    }),
  });
  assert.ok(promptReviewOpenedFailures(staleExpectedState, ledger(), expected())
    .includes("review-target-not-canonical"));

  const currentRunDrift = openedSurface({
    projection: projection({ run_target: staleRunTarget(expected().runTarget) }),
  });
  assert.ok(promptReviewOpenedFailures(currentRunDrift, ledger(), expected()).includes("run-target-drift"));

  const staleDom = openedSurface({
    review_draft: { count: 1, value: "stale enhanced text", visible: true, enabled: true },
  });
  assert.ok(promptReviewOpenedFailures(staleDom, ledger(), expected()).includes("review-draft-dom-drift"));
});

test("Prompt Review open predicate binds the generated review identity across observations", () => {
  const decide = createStablePromptReviewOpenedPredicate(expected());
  assert.equal(decide({ surface: openedSurface(), ledger: ledger() }), false);
  assert.equal(decide({ surface: openedSurface(), ledger: ledger() }), true);

  const drifted = openedSurface({
    projection: projection({
      review_target: {
        workspacePath: WORKSPACE,
        sessionId: null,
        ownerGeneration: "7",
        requestId: "12",
        expectedState: { kind: "idle", latestTurnId: null, admissionRevision: "0" },
      },
    }),
  });
  assert.equal(decide({ surface: drifted, ledger: ledger() }), false);
  assert.equal(decide({ surface: drifted, ledger: ledger() }), false);
});

test("Prompt Review provider predicate reuses the exact catalog-then-response contract", () => {
  assert.equal(exactPromptReviewProviderLedger(ledger()), true);
  assert.equal(exactPromptReviewProviderLedger([]), false);
  assert.equal(exactPromptReviewProviderLedger(ledger().slice(1)), false);
  assert.equal(exactPromptReviewProviderLedger(ledger().toReversed()), false);
  assert.equal(exactPromptReviewProviderLedger([...ledger(), ...ledger()]), false);
  assert.equal(exactPromptReviewProviderLedger([
    { ...ledger()[0], response_status: 422 },
    ledger()[1],
  ]), false);
  assert.equal(exactPromptReviewProviderLedger([
    ledger()[0],
    { ...ledger()[1], response_status: 422 },
  ]), false);
  assert.equal(exactPromptReviewProviderLedger([
    ledger()[0],
    { ...ledger()[1], contract: { pass: false } },
  ]), false);
});

test("Prompt Review cancel predicate requires exact clear while preserving composer owner and local draft", () => {
  assert.deepEqual(promptReviewCancelledFailures(cancelledSurface(), ledger(), expected()), []);

  const staleReview = cancelledSurface({
    projection: projection({
      overlay: "none",
      review_target: { workspacePath: WORKSPACE, sessionId: null, ownerGeneration: "7", requestId: "11" },
      review_raw_text: PROMPT_REVIEW_RAW_TEXT,
      review_draft_text: PROMPT_REVIEW_ENHANCED_TEXT,
      send_enhanced_enabled: false,
      send_raw_enabled: false,
    }),
  });
  assert.ok(promptReviewCancelledFailures(staleReview, ledger(), expected()).includes("review-target-not-cleared"));
  assert.ok(promptReviewCancelledFailures(staleReview, ledger(), expected()).includes("review-content-not-cleared"));

  const clearedComposer = cancelledSurface({
    projection: projection({
      overlay: "none",
      review_target: null,
      review_raw_text: "",
      review_draft_text: "",
      send_enhanced_enabled: false,
      send_raw_enabled: false,
      draft_prompt: "",
    }),
    prompt: { count: 1, value: "", visible: true, enabled: true },
  });
  assert.ok(promptReviewCancelledFailures(clearedComposer, ledger(), expected()).includes("composer-draft-drift"));

  const ownerChanged = cancelledSurface({
    projection: projection({
      overlay: "none",
      review_target: null,
      review_raw_text: "",
      review_draft_text: "",
      send_enhanced_enabled: false,
      send_raw_enabled: false,
      draft_target: { workspacePath: WORKSPACE, sessionId: null, ownerGeneration: "8" },
    }),
  });
  assert.ok(promptReviewCancelledFailures(ownerChanged, ledger(), expected()).includes("draft-target-drift"));

  const visibleError = cancelledSurface({ visible_recoverable_error_count: 1 });
  assert.ok(promptReviewCancelledFailures(visibleError, ledger(), expected()).includes("recoverable-error-visible"));
});
