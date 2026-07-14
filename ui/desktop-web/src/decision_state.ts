export type PermissionReviewDecision = "approved" | "abort";

export type PermissionDecisionState =
  | { phase: "ready"; requestId: string }
  | {
    phase: "submitting";
    requestId: string;
    submissionId: number;
    decision: PermissionReviewDecision;
  }
  | { phase: "failed"; requestId: string; error: string };

export interface PermissionDecisionSubmission {
  requestId: string;
  submissionId: number;
  decision: PermissionReviewDecision;
}

export function permissionDecisionForEscape(
  confirmationVisible: boolean,
  repeat: boolean,
): PermissionReviewDecision | null {
  return confirmationVisible && !repeat ? "abort" : null;
}

export interface PermissionDecisionOwner {
  permissionDecision: PermissionDecisionState | null;
  nextPermissionSubmissionId: number;
}

export interface LocalDecisionOwner {
  localConfirmationDecisionPending: boolean;
  localConfirmationDecisionError: string;
}

export function beginPermissionDecision(
  owner: PermissionDecisionOwner,
  confirmationId: string | null,
  decision: PermissionReviewDecision,
): PermissionDecisionSubmission | null {
  if (confirmationId === null) return null;
  reconcilePermissionDecision(owner, confirmationId);
  if (owner.permissionDecision?.phase === "submitting") return null;
  const submission: PermissionDecisionSubmission = {
    requestId: confirmationId,
    submissionId: owner.nextPermissionSubmissionId++,
    decision,
  };
  owner.permissionDecision = { phase: "submitting", ...submission };
  return submission;
}

export function finishPermissionDecision(
  owner: PermissionDecisionOwner,
  submission: PermissionDecisionSubmission,
): boolean {
  if (!permissionDecisionSubmissionIsCurrent(owner.permissionDecision, submission)) return false;
  owner.permissionDecision = { phase: "ready", requestId: submission.requestId };
  return true;
}

export function failPermissionDecision(
  owner: PermissionDecisionOwner,
  submission: PermissionDecisionSubmission,
  message: string,
): boolean {
  if (!permissionDecisionSubmissionIsCurrent(owner.permissionDecision, submission)) return false;
  owner.permissionDecision = {
    phase: "failed",
    requestId: submission.requestId,
    error: message,
  };
  return true;
}

export function recoverPermissionDecisionFromConflict(
  owner: PermissionDecisionOwner,
  submission: PermissionDecisionSubmission,
  confirmationId: string | null,
): boolean {
  if (!permissionDecisionSubmissionIsCurrent(owner.permissionDecision, submission)) return false;
  owner.permissionDecision = confirmationId === null
    ? null
    : { phase: "ready", requestId: confirmationId };
  return true;
}

export function reconcilePermissionDecision(
  owner: PermissionDecisionOwner,
  confirmationId: string | null,
): void {
  if (confirmationId === null) {
    owner.permissionDecision = null;
    return;
  }
  if (owner.permissionDecision?.requestId !== confirmationId) {
    owner.permissionDecision = { phase: "ready", requestId: confirmationId };
  }
}

export function permissionDecisionShouldFocusComposer(
  submission: PermissionDecisionSubmission,
  settlementApplied: boolean,
  confirmationVisible: boolean,
): boolean {
  return settlementApplied && submission.decision === "abort" && !confirmationVisible;
}

export function permissionDecisionResponseAccepted(
  expectedConfirmationId: string,
  confirmationVisible: boolean,
  currentConfirmationId: string | null,
): boolean {
  return !confirmationVisible || currentConfirmationId !== expectedConfirmationId;
}

function permissionDecisionSubmissionIsCurrent(
  state: PermissionDecisionState | null,
  submission: PermissionDecisionSubmission,
): boolean {
  return state?.phase === "submitting"
    && state.requestId === submission.requestId
    && state.submissionId === submission.submissionId;
}

export function beginLocalDecision(owner: LocalDecisionOwner, confirmationOpen: boolean): boolean {
  if (!confirmationOpen || owner.localConfirmationDecisionPending) return false;
  owner.localConfirmationDecisionPending = true;
  owner.localConfirmationDecisionError = "";
  return true;
}

export function finishLocalDecision(owner: LocalDecisionOwner): void {
  owner.localConfirmationDecisionPending = false;
  owner.localConfirmationDecisionError = "";
}

export function failLocalDecision(owner: LocalDecisionOwner, message: string): void {
  owner.localConfirmationDecisionPending = false;
  owner.localConfirmationDecisionError = message;
}
