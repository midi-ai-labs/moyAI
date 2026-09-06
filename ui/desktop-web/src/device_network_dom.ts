import { synchronizeRetainedControlValue } from "./settings_surface.ts";

export function synchronizeDeviceNetworkControls(current: HTMLElement, next: HTMLElement): void {
  const jobs = current.querySelector<HTMLElement>("#device-network-jobs-list");
  const nextJobs = next.querySelector<HTMLElement>("#device-network-jobs-list");
  if (jobs && nextJobs) {
    const existing = new Map([...jobs.querySelectorAll<HTMLElement>("[data-network-job-id]")].map(row => [row.dataset.networkJobId!, row]));
    const wanted = new Set<string>();
    for (const [index, row] of [...nextJobs.querySelectorAll<HTMLElement>("[data-network-job-id]")].entries()) {
      const id = row.dataset.networkJobId!;
      wanted.add(id);
      const retained = existing.get(id);
      // Shared Settings synchronization updates the retained row's passive facts and
      // button availability. Keep its details, keyboard focus and scroll owner mounted.
      if (!retained) jobs.insertBefore(row, jobs.children[index] ?? null);
      else if (jobs.children[index] !== retained) jobs.insertBefore(retained, jobs.children[index] ?? null);
    }
    for (const [id, row] of existing) if (!wanted.has(id)) {
      if (row.contains(current.ownerDocument.activeElement)) current.querySelector<HTMLElement>("#device-network-refresh")?.focus({ preventScroll: true });
      row.remove();
    }
  }
  for (const nextControl of next.querySelectorAll<HTMLInputElement | HTMLSelectElement>("[data-network-field]")) {
    if (nextControl.dataset.networkField === "code") continue;
    const control = current.querySelector<HTMLInputElement | HTMLSelectElement>(`#${CSS.escape(nextControl.id)}`);
    if (control && control !== current.ownerDocument.activeElement) synchronizeRetainedControlValue(control, nextControl);
  }
  for (const key of ["join", "member"]) {
    const source = [...next.querySelectorAll<HTMLElement>(`[data-network-visible="${key}"]`)];
    const target = [...current.querySelectorAll<HTMLElement>(`[data-network-visible="${key}"]`)];
    for (const [index, region] of target.entries()) if (source[index]) region.hidden = source[index].hidden;
  }
}
