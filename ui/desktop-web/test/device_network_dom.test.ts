import assert from "node:assert/strict";
import test from "node:test";
import { synchronizeDeviceNetworkControls } from "../src/device_network_dom.ts";
import { renderDeviceArtifacts } from "../src/device_network_artifacts.ts";
import { deviceUiFixture } from "./device_network_fixture.ts";

// Focused DOM boundary: connected identity, subtree moves, focus and disclosure
// state are modeled; artifact eligibility comes from the actual renderer.
function surface(terminal: boolean) {
  const document = { activeElement: null as NodeFixture | null };
  class NodeFixture {
    ownerDocument = document;
    parent: NodeFixture | null = null;
    children: NodeFixture[] = [];
    dataset: Record<string, string> = {};
    id = "";
    className = "";
    open = false;
    scrollTop = 0;
    contains(node: NodeFixture | null): boolean { return node === this || this.children.some(child => child.contains(node)); }
    querySelectorAll(selector: string): NodeFixture[] {
      const all = this.children.flatMap(child => [child, ...child.querySelectorAll("*")]);
      if (selector === "*") return all;
      if (selector === "[data-network-job-id]") return all.filter(node => node.dataset.networkJobId !== undefined);
      return [];
    }
    querySelector(selector: string): NodeFixture | null {
      return this.querySelectorAll("*").find(node => selector === `#${node.id}`
        || selector === `.${node.className}`
        || (selector === '[data-action="device-network-stop-job"]' && node.dataset.action === "device-network-stop-job")) ?? null;
    }
    insertBefore(node: NodeFixture, reference: NodeFixture | null): NodeFixture {
      node.remove(); node.parent = this; node.adopt(this.ownerDocument);
      const index = reference === null ? this.children.length : this.children.indexOf(reference);
      if (index < 0) throw new Error("reference is not a child");
      this.children.splice(index, 0, node); return node;
    }
    private adopt(owner: typeof document): void { this.ownerDocument = owner; for (const child of this.children) child.adopt(owner); }
    remove(): void {
      if (!this.parent) return;
      this.parent.children.splice(this.parent.children.indexOf(this), 1); this.parent = null;
      if (this.contains(this.ownerDocument.activeElement)) this.ownerDocument.activeElement = null;
    }
    focus(): void { this.ownerDocument.activeElement = this; }
  }
  const create = (parent: NodeFixture) => parent.insertBefore(new NodeFixture(), null);
  const root = new NodeFixture();
  const refresh = create(root); refresh.id = "device-network-refresh";
  const jobs = create(root); jobs.id = "device-network-jobs-list";
  const row = create(jobs); row.dataset.networkJobId = "outgoing:reference-a";
  const result = create(row); result.open = true;
  const summary = create(result);
  const local = deviceUiFixture();
  if (terminal) { local.jobs.outgoing[0].state = "interrupted"; local.jobs.outgoing[0].can_stop = false; }
  let artifacts: NodeFixture | null = null;
  if (renderDeviceArtifacts(local, "reference-a")) {
    artifacts = create(row); artifacts.className = "device-network-artifacts";
    create(artifacts).id = "device-network-artifacts-reference-a";
  }
  const stop = create(row); stop.dataset.action = "device-network-stop-job";
  return { root, jobs, row, result, summary, artifacts, stop, refresh, document };
}
function synchronize(current: ReturnType<typeof surface>, next: ReturnType<typeof surface>): void {
  synchronizeDeviceNetworkControls(current.root as unknown as HTMLElement, next.root as unknown as HTMLElement);
}

test("a retained running job gains terminal artifact controls without replacing its interaction owners", () => {
  const current = surface(false); const next = surface(true);
  current.document.activeElement = current.summary; current.jobs.scrollTop = 320; current.result.scrollTop = 22;
  synchronize(current, next);
  assert.equal(current.row.querySelector(".device-network-artifacts"), next.artifacts, "terminal artifact controls are mounted in the existing row");
  assert.equal(current.jobs.children[0], current.row);
  assert.equal(current.row.children[0], current.result);
  assert.equal(current.result.open, true);
  assert.equal(current.document.activeElement, current.summary);
  assert.equal(current.jobs.scrollTop, 320); assert.equal(current.result.scrollTop, 22);
  assert.equal(current.row.children.at(-1), current.stop);
  assert.equal(current.row.children.length, 3);
});

test("later terminal polls preserve the mounted artifact disclosure and its focused control", () => {
  const current = surface(true); const artifacts = current.artifacts!;
  artifacts.open = true; artifacts.scrollTop = 80; current.document.activeElement = artifacts.children[0];
  for (let index = 0; index < 3; index++) synchronize(current, surface(true));
  assert.equal(current.row.querySelector(".device-network-artifacts"), artifacts);
  assert.equal(artifacts.open, true); assert.equal(artifacts.scrollTop, 80);
  assert.equal(current.document.activeElement, artifacts.children[0]);
  assert.equal(current.row.children.length, 3);
});

test("loss of artifact eligibility removes only that region and moves its focus to refresh", () => {
  const current = surface(true); current.document.activeElement = current.artifacts!.children[0];
  synchronize(current, surface(false));
  assert.equal(current.row.querySelector(".device-network-artifacts"), null);
  assert.equal(current.document.activeElement, current.refresh);
  assert.equal(current.jobs.children[0], current.row);
  assert.equal(current.row.children[0], current.result);
  assert.equal(current.result.open, true);
});
