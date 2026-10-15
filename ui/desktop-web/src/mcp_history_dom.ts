/** Text selection remains browser-owned while a recorded task continues to update. */
export function mcpHistoryRegionHasSelection(region: HTMLElement): boolean {
  const selection = region.ownerDocument.getSelection();
  return Boolean(selection && !selection.isCollapsed && selection.rangeCount > 0
    && selection.containsNode(region, true));
}
function updateMarkup(current: HTMLElement, next: HTMLElement, changedOwner = false): void {
  if (current.innerHTML === next.innerHTML) return;
  if (!changedOwner && (mcpHistoryRegionHasSelection(current)
    || (current.contains(current.ownerDocument.activeElement) && current !== current.ownerDocument.activeElement))) return;
  current.innerHTML = next.innerHTML;
}
/**
 * Retains the dialog, scroll containers and row buttons. A poll updates row cells and
 * document markup only when the user is not selecting or interacting with that content.
 * Distinct page/detail identities let explicit navigation replace the relevant content.
 */
export function synchronizeMcpHistorySurface(current: HTMLElement, next: HTMLElement): void {
  const changedPage = current.dataset.historyPage !== next.dataset.historyPage;
  const changedDetail = current.dataset.historyDetailOwner !== next.dataset.historyDetailOwner;
  for (const button of Array.from(current.querySelectorAll<HTMLButtonElement>("button[id]"))) {
    const replacement = next.querySelector<HTMLButtonElement>(`#${CSS.escape(button.id)}`);
    if (!replacement) continue;
    button.disabled = replacement.disabled;
    button.setAttribute("aria-disabled", String(button.disabled));
    button.hidden = replacement.hidden;
    const pressed = replacement.getAttribute("aria-pressed");
    if (pressed !== null) button.setAttribute("aria-pressed", pressed);
  }
  const list = current.querySelector<HTMLElement>("[data-history-list]");
  const nextList = next.querySelector<HTMLElement>("[data-history-list]");
  if (list && nextList) {
    const currentRows = Array.from(list.querySelectorAll<HTMLElement>("[data-history-row]"));
    const nextRows = Array.from(nextList.querySelectorAll<HTMLElement>("[data-history-row]"));
    const sameRows = currentRows.length === nextRows.length
      && currentRows.every((row, index) => row.dataset.historyRow === nextRows[index].dataset.historyRow);
    if (!changedPage && sameRows && currentRows.length) {
      for (let index = 0; index < currentRows.length; index++) {
        const row = currentRows[index];
        const nextRow = nextRows[index];
        for (const cell of Array.from(row.querySelectorAll<HTMLElement>("[data-history-cell]"))) {
          const newCell = nextRow.querySelector<HTMLElement>(`[data-history-cell="${cell.dataset.historyCell}"]`);
          if (!newCell) continue;
          if (!mcpHistoryRegionHasSelection(cell) && cell.textContent !== newCell.textContent) cell.textContent = newCell.textContent;
          if (newCell.dataset.state) cell.dataset.state = newCell.dataset.state;
        }
      }
    } else {
      updateMarkup(list, nextList, changedPage);
      if (changedPage) list.scrollTop = 0;
    }
  }
  for (const region of Array.from(current.querySelectorAll<HTMLElement>("[data-history-region]"))) {
    const replacement = next.querySelector<HTMLElement>(`[data-history-region="${region.dataset.historyRegion}"]`);
    if (replacement) updateMarkup(region, replacement, changedDetail);
  }
  if (changedDetail) {
    const scroll = current.querySelector<HTMLElement>("[data-history-scroll]");
    if (scroll) { scroll.scrollTop = 0; scroll.scrollLeft = 0; }
  }
  current.dataset.historyPage = next.dataset.historyPage;
  current.dataset.historyDetailOwner = next.dataset.historyDetailOwner;
}
