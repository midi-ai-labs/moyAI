import assert from "node:assert/strict";
import test from "node:test";
import { mcpHistoryRegionHasSelection, synchronizeMcpHistorySurface } from "../src/mcp_history_dom.ts";

// A focused DOM fixture: membership, connected node identity and replacement effects
// are modeled explicitly; no private source text or layout placement is asserted.
function surface() {
  let selected: ElementStub | null = null;
  const document = { activeElement: null as ElementStub | null, getSelection: () => ({
    isCollapsed: selected === null, rangeCount: selected ? 1 : 0,
    containsNode: (element: ElementStub) => element.contains(selected),
  }) };
  class ElementStub {
    ownerDocument = document;
    dataset: Record<string, string> = {};
    id = "";
    disabled = false;
    hidden = false;
    textContent = "";
    replacements = 0;
    private markup = "";
    get innerHTML() { return this.markup; }
    set innerHTML(value: string) { this.markup = value; this.replacements++; }
    scrollTop = 0;
    scrollLeft = 0;
    attributes: Record<string, string> = {};
    children: ElementStub[] = [];
    contains(node: ElementStub | null): boolean { return node === this || this.children.some(child => child.contains(node)); }
    getAttribute(name: string) { return this.attributes[name] ?? null; }
    setAttribute(name: string, value: string) { this.attributes[name] = value; }
    querySelectorAll(query: string): ElementStub[] {
      const descendants = this.children.flatMap(child => [child, ...child.querySelectorAll("*")]);
      if (query === "*") return descendants;
      if (query === "button[id]") return descendants.filter(node => node.id !== "");
      const match = query.match(/^\[data-history-(row|cell|region)\]$/);
      if (match) return descendants.filter(node => `history${match[1][0].toUpperCase()}${match[1].slice(1)}` in node.dataset);
      return [];
    }
    querySelector(query: string): ElementStub | null {
      const all = this.querySelectorAll("*");
      if (query.startsWith("#")) return all.find(node => node.id === query.slice(1)) ?? null;
      if (query === "[data-history-list]") return all.find(node => "historyList" in node.dataset) ?? null;
      if (query === "[data-history-scroll]") return all.find(node => "historyScroll" in node.dataset) ?? null;
      const match = query.match(/^\[data-history-(cell|region)="([^"]+)"\]$/);
      if (match) return all.find(node => node.dataset[`history${match[1][0].toUpperCase()}${match[1].slice(1)}`] === match[2]) ?? null;
      return null;
    }
  }
  const modal = new ElementStub(); modal.dataset = { historyPage: "instruction:0", historyDetailOwner: "instruction:ref-a" };
  const list = new ElementStub(); list.dataset.historyList = ""; list.scrollTop = 137;
  const row = new ElementStub(); row.dataset.historyRow = "ref-a"; row.id = "mcp-history-row-ref-a"; row.attributes["aria-pressed"] = "true";
  const title = new ElementStub(); title.dataset.historyCell = "title"; title.textContent = "CPU調査";
  const state = new ElementStub(); state.dataset = { historyCell: "state", state: "running" }; state.textContent = "実行中";
  row.children = [title, state]; list.children = [row];
  const scroll = new ElementStub(); scroll.dataset.historyScroll = ""; scroll.scrollTop = 419; scroll.scrollLeft = 12;
  const body = new ElementStub(); body.dataset.historyRegion = "document"; body.innerHTML = "old document";
  const text = new ElementStub(); body.children = [text];
  scroll.children = [body]; modal.children = [list, scroll];
  return { modal, list, row, title, state, scroll, body, text, document, select(node: ElementStub | null) { selected = node; } };
}
function synchronize(current: ReturnType<typeof surface>, next: ReturnType<typeof surface>) {
  const previous = Object.getOwnPropertyDescriptor(globalThis, "CSS");
  Object.defineProperty(globalThis, "CSS", { configurable: true, value: { escape: (value: string) => value } });
  try { synchronizeMcpHistorySurface(current.modal as unknown as HTMLElement, next.modal as unknown as HTMLElement); }
  finally { if (previous) Object.defineProperty(globalThis, "CSS", previous); else delete (globalThis as Record<string, unknown>).CSS; }
}

test("poll updates state without replacing the focused row, list or scroll containers", () => {
  const current = surface(); const next = surface();
  current.document.activeElement = current.row;
  next.state.dataset.state = "completed"; next.state.textContent = "完了";
  next.body.innerHTML = "new result";
  const bodyReplacements = current.body.replacements;
  synchronize(current, next);
  assert.equal(current.document.activeElement, current.row);
  assert.equal(current.row.replacements, 0);
  assert.equal(current.list.replacements, 0);
  assert.equal(current.state.textContent, "完了");
  assert.equal(current.state.dataset.state, "completed");
  assert.equal(current.body.replacements, bodyReplacements + 1);
  assert.equal(current.list.scrollTop, 137);
  assert.equal(current.scroll.scrollTop, 419);
  assert.equal(current.scroll.scrollLeft, 12);
});

test("selected history text survives polling and catches up when selection is released", () => {
  const current = surface(); const next = surface();
  current.select(current.text);
  assert.equal(mcpHistoryRegionHasSelection(current.body as unknown as HTMLElement), true);
  next.body.innerHTML = "new result";
  const count = current.body.replacements;
  synchronize(current, next);
  assert.equal(current.body.innerHTML, "old document");
  assert.equal(current.body.replacements, count);
  current.select(null);
  synchronize(current, next);
  assert.equal(current.body.innerHTML, "new result");
  assert.equal(current.scroll.scrollTop, 419);
});

test("explicit task navigation changes the detail owner and starts its document at the top", () => {
  const current = surface(); const next = surface();
  current.select(current.text);
  next.modal.dataset.historyDetailOwner = "instruction:ref-b";
  next.body.innerHTML = "task B document";
  synchronize(current, next);
  assert.equal(current.body.innerHTML, "task B document");
  assert.equal(current.scroll.scrollTop, 0);
  assert.equal(current.scroll.scrollLeft, 0);
  assert.equal(current.list.scrollTop, 137);
});
