interface MainSurface {
  hub_project_open?: boolean;
  overlay: string;
  confirmation_visible: boolean;
}

/** Partial main updates cannot install or remove modal siblings of the app frame. */
export function shouldRetainSharedWorkMain(
  previous: MainSurface | null,
  current: MainSurface,
  previousLocalModal: string | null,
  currentLocalModal: string | null,
  backgroundInert: boolean,
): boolean {
  return previous?.hub_project_open === true && current.hub_project_open === true
    && previous.overlay === "none" && current.overlay === "none"
    && !previous.confirmation_visible && !current.confirmation_visible
    && previousLocalModal === null && currentLocalModal === null && !backgroundInert;
}
