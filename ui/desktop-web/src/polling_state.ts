export interface AutoRefreshState {
  navigation_loading: boolean;
  confirmation_visible: boolean;
}

export function autoRefreshAllowed(state: AutoRefreshState, interactionActive: boolean): boolean {
  return state.navigation_loading || !interactionActive;
}
