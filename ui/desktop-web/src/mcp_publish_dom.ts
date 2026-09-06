import { synchronizeRetainedControlValue } from "./settings_surface.ts";

export function clearPublishSecret(): void {
  const input = document.querySelector<HTMLInputElement>("#mcp-publish-token");
  if (input) input.value = "";
}

/** Explicit Add navigation happens before render captures the retained editor's scroll. */
export function revealPublishEditorStart(): void {
  const content = document.querySelector<HTMLElement>('[data-modal="mcp_publish"] .mcp-publish-content');
  if (content) content.scrollTop = 0;
}

/** Secret text stays in this connected DOM input and never enters the render projection. */
export function synchronizePublishControlValues(currentModal: HTMLElement, nextModal: HTMLElement): void {
  const changedProfile = currentModal.dataset.profileId !== nextModal.dataset.profileId;
  const changedCredential = currentModal.dataset.credentialId !== nextModal.dataset.credentialId;
  if (changedProfile || changedCredential) {
    const token = currentModal.querySelector<HTMLInputElement>("#mcp-publish-token");
    if (token) token.value = "";
  }
  currentModal.dataset.profileId = nextModal.dataset.profileId;
  currentModal.dataset.credentialId = nextModal.dataset.credentialId;
  for (const next of nextModal.querySelectorAll<HTMLElement>("[data-mcp-section]")) {
    const current = currentModal.querySelector<HTMLElement>(`[data-mcp-section="${next.dataset.mcpSection}"]`);
    if (current) current.hidden = next.hidden;
  }
  for (const next of nextModal.querySelectorAll<HTMLInputElement | HTMLSelectElement>("[data-mcp-publish-field]")) {
    if (next.id === "mcp-publish-token") continue;
    const current = currentModal.querySelector<HTMLInputElement | HTMLSelectElement>(`#${CSS.escape(next.id)}`);
    if (!current || current === current.ownerDocument.activeElement) continue;
    if (current.tagName === "SELECT" && current.innerHTML !== next.innerHTML) current.innerHTML = next.innerHTML;
    synchronizeRetainedControlValue(current, next);
  }
}
