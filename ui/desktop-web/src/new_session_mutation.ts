export interface NewSessionMutationInteractionBoundary {
  readonly active: boolean;
  whenIdle(): Promise<void>;
}

export type NewSessionMutationName = "new_chat" | "new_project_session";

export interface NewSessionMutationRequest {
  mutationName: NewSessionMutationName;
  token: object;
}

export interface NewSessionMutationOwnerState {
  activeNewSessionMutation: NewSessionMutationRequest | null;
}

export async function dispatchNewSessionMutation<Response>(
  owner: NewSessionMutationOwnerState,
  mutationName: NewSessionMutationName,
  interaction: NewSessionMutationInteractionBoundary,
  renderAfterBegin: () => void,
  invoke: () => Promise<Response>,
  accept: (response: Response) => void,
  releaseAfterError: () => void,
): Promise<boolean> {
  if (owner.activeNewSessionMutation !== null) return false;
  const request: NewSessionMutationRequest = { mutationName, token: {} };
  owner.activeNewSessionMutation = request;
  try {
    renderAfterBegin();
    const response = await invoke();
    await interaction.whenIdle();
    if (owner.activeNewSessionMutation?.token !== request.token) return false;
    owner.activeNewSessionMutation = null;
    accept(response);
    return true;
  } finally {
    // Fast success and failure remain single-flight until the pointer/key activation releases.
    await interaction.whenIdle();
    if (owner.activeNewSessionMutation?.token === request.token) {
      owner.activeNewSessionMutation = null;
      releaseAfterError();
    }
  }
}

export function repeatedNewSessionPointerActivation(action: string, clickDetail: number): boolean {
  return (action === "new-chat" || action === "new-project-session") && clickDetail > 1;
}

export function mutationStartsNewSession(
  mutationName: string,
): mutationName is NewSessionMutationName {
  return mutationName === "new_chat" || mutationName === "new_project_session";
}
