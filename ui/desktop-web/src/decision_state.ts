export type PermissionReviewDecision = "approved" | "denied" | "abort";
export type PermissionModalAction = PermissionReviewDecision | "stop";
export interface RemotePermissionTarget { job_id: string; profile_id: string }

export type PermissionDecisionState =
  | { phase: "ready"; requestId: string }
  | {
    phase: "submitting";
    requestId: string;
    submissionId: number;
    decision: PermissionModalAction;
  }
  | { phase: "failed"; requestId: string; error: string };

export interface PermissionDecisionSubmission {
  requestId: string;
  submissionId: number;
  decision: PermissionModalAction;
  remote?: RemotePermissionTarget;
}

export function permissionDecisionForEscape(
  confirmationVisible: boolean,
  repeat: boolean,
  remote = false,
): PermissionReviewDecision | null {
  return confirmationVisible && !repeat && !remote ? "abort" : null;
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
  remote: RemotePermissionTarget | null = null,
): PermissionDecisionSubmission | null {
  if (decision === "denied" && !remote) return null;
  return beginPermissionSubmission(owner, confirmationId, decision, remote);
}

export function beginPermissionStop(
  owner: PermissionDecisionOwner,
  confirmationId: string | null,
): PermissionDecisionSubmission | null {
  return beginPermissionSubmission(owner, confirmationId, "stop");
}

function beginPermissionSubmission(
  owner: PermissionDecisionOwner,
  confirmationId: string | null,
  decision: PermissionModalAction,
  remote: RemotePermissionTarget | null = null,
): PermissionDecisionSubmission | null {
  if (confirmationId === null) return null;
  reconcilePermissionDecision(owner, confirmationId);
  if (owner.permissionDecision?.phase === "submitting") return null;
  const submission: PermissionDecisionSubmission = {
    requestId: confirmationId,
    submissionId: owner.nextPermissionSubmissionId++,
    decision,
    ...(remote ? { remote: { job_id: remote.job_id, profile_id: remote.profile_id } } : {}),
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
  return settlementApplied && !submission.remote && submission.decision === "abort" && !confirmationVisible;
}

export function permissionDecisionCommandTarget(submission: PermissionDecisionSubmission): {
  confirmationId: string; remoteJobId?: string; remoteProfileId?: string;
} {
  return { confirmationId: submission.requestId,
    ...(submission.remote ? { remoteJobId: submission.remote.job_id, remoteProfileId: submission.remote.profile_id } : {}) };
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
