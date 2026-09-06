import { synchronizeRetainedControlValue } from "./settings_surface.ts";

/** Keeps credentials in the connected password input; projections never contain them. */
export function synchronizeHubControlValues(currentModal: HTMLElement, nextModal: HTMLElement): void {
  for (const next of nextModal.querySelectorAll<HTMLElement>("[data-hub-panel]")) {
    const current = currentModal.querySelector<HTMLElement>(`#${CSS.escape(next.id)}`);
    if (current) current.hidden = next.hidden;
  }
  for (const next of nextModal.querySelectorAll<HTMLInputElement | HTMLSelectElement>("[data-hub-field]")) {
    if (next.dataset.hubField === "token") continue;
    const current = currentModal.querySelector<HTMLInputElement | HTMLSelectElement>(`#${CSS.escape(next.id)}`);
    if (current && current !== current.ownerDocument.activeElement) synchronizeRetainedControlValue(current, next);
  }
}
