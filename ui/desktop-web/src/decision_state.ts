export interface PermissionDecisionOwner {
  permissionDecisionPending: boolean;
  permissionDecisionAllow: boolean | null;
  permissionDecisionConfirmationId: number | null;
  permissionDecisionError: string;
}

export interface LocalDecisionOwner {
  localConfirmationDecisionPending: boolean;
  localConfirmationDecisionError: string;
}

export function beginPermissionDecision(
  owner: PermissionDecisionOwner,
  confirmationId: number | null,
  allow: boolean,
): boolean {
  if (confirmationId === null || owner.permissionDecisionPending) return false;
  owner.permissionDecisionPending = true;
  owner.permissionDecisionAllow = allow;
  owner.permissionDecisionConfirmationId = confirmationId;
  owner.permissionDecisionError = "";
  return true;
}

export function finishPermissionDecision(owner: PermissionDecisionOwner): void {
  owner.permissionDecisionPending = false;
  owner.permissionDecisionAllow = null;
  owner.permissionDecisionConfirmationId = null;
  owner.permissionDecisionError = "";
}

export function failPermissionDecision(owner: PermissionDecisionOwner, message: string): void {
  owner.permissionDecisionPending = false;
  owner.permissionDecisionAllow = null;
  owner.permissionDecisionConfirmationId = null;
  owner.permissionDecisionError = message;
}

export function permissionDecisionResponseAccepted(
  expectedConfirmationId: number,
  confirmationVisible: boolean,
  currentConfirmationId: number | null,
): boolean {
  return !confirmationVisible || currentConfirmationId !== expectedConfirmationId;
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
