import assert from "node:assert/strict";
import test from "node:test";
import { synchronizeRetainedSettingsSurface } from "../src/settings_surface.ts";

// Models browser subtree moves, static querySelectorAll results and browser-owned
// interaction. In particular, replacing a parent detaches its original children.
interface DocumentFixture {
  root: ElementFixture | null;
  activeElement: ElementFixture | null;
  selected: ElementFixture | null;
}

class ElementFixture {
  parentNode: ElementFixture | null = null;
  children: ElementFixture[] = [];
  attributes = new Map<string, string>();
  textContent = "";
  innerHTML = "";
  disabled = false;
  hidden = false;
  scrollTop = 0;
  scrollLeft = 0;
  ownerDocument: DocumentFixture;
  readonly tagName: string;
  constructor(ownerDocument: DocumentFixture, tagName = "DIV") {
    this.ownerDocument = ownerDocument; this.tagName = tagName;
  }
  get dataset(): Record<string, string> {
    return Object.fromEntries([...this.attributes].filter(([key]) => key.startsWith("data-"))
      .map(([key, value]) => [key.slice(5).replace(/-([a-z])/g, (_, letter: string) => letter.toUpperCase()), value]));
  }
  get open(): boolean { return this.hasAttribute("open"); }
  set open(value: boolean) { if (value) this.setAttribute("open", ""); else this.removeAttribute("open"); }
  getAttribute(name: string): string | null { return this.attributes.get(name) ?? null; }
  hasAttribute(name: string): boolean { return this.attributes.has(name); }
  setAttribute(name: string, value: string): void { this.attributes.set(name, value); }
  removeAttribute(name: string): void { this.attributes.delete(name); }
  contains(node: ElementFixture | null): boolean {
    return node !== null && (node === this || this.children.some(child => child.contains(node)));
  }
  isEqualNode(other: ElementFixture): boolean {
    return this.tagName === other.tagName && this.textContent === other.textContent
      && this.attributes.size === other.attributes.size
      && [...this.attributes].every(([key, value]) => other.getAttribute(key) === value)
      && this.children.length === other.children.length
      && this.children.every((child, index) => child.isEqualNode(other.children[index]));
  }
  private adopt(document: DocumentFixture): void {
    this.ownerDocument = document;
    for (const child of this.children) child.adopt(document);
  }
  append(child: ElementFixture): ElementFixture {
    child.remove(); child.adopt(this.ownerDocument); child.parentNode = this; this.children.push(child);
    return child;
  }
  remove(): void {
    if (!this.parentNode) return;
    if (this.ownerDocument.root?.contains(this)) {
      if (this.contains(this.ownerDocument.activeElement)) this.ownerDocument.activeElement = null;
      if (this.contains(this.ownerDocument.selected)) this.ownerDocument.selected = null;
    }
    this.parentNode.children.splice(this.parentNode.children.indexOf(this), 1);
    this.parentNode = null;
  }
  replaceWith(next: ElementFixture): void {
    const parent = this.parentNode;
    if (!parent || next === this) return;
    next.remove();
    const index = parent.children.indexOf(this);
    this.remove();
    next.adopt(parent.ownerDocument); next.parentNode = parent; parent.children.splice(index, 0, next);
  }
  querySelectorAll(selector: string): ElementFixture[] {
    const descendants = this.children.flatMap(child => [child, ...child.querySelectorAll("*")]);
    if (selector === "*") return descendants;
    if (selector === "button, input, select, textarea") return descendants.filter(node => ["BUTTON", "INPUT", "SELECT", "TEXTAREA"].includes(node.tagName));
    if (selector === "[data-settings-passive]") return descendants.filter(node => node.hasAttribute("data-settings-passive"));
    if (selector === "details[data-details-key]") return descendants.filter(node => node.tagName === "DETAILS" && node.hasAttribute("data-details-key"));
    return [];
  }
  querySelector(): null { return null; }
}

function peerSurface(result = "TCPに接続できません", name = "WinB") {
  const document: DocumentFixture = { root: null, activeElement: null, selected: null };
  const root = new ElementFixture(document); document.root = root;
  const peers = root.append(new ElementFixture(document));
  peers.setAttribute("data-settings-passive", "peers");
  peers.setAttribute("data-settings-preserve-focused-region", "");
  const label = peers.append(new ElementFixture(document, "STRONG")); label.textContent = name;
  const details = peers.append(new ElementFixture(document, "DETAILS"));
  details.setAttribute("data-details-key", "peer-B-diagnostic");
  const summary = details.append(new ElementFixture(document, "SUMMARY")); summary.textContent = "接続診断の結果";
  const diagnostic = details.append(new ElementFixture(document));
  diagnostic.setAttribute("data-settings-passive", "peer-B-diagnostic");
  const text = diagnostic.append(new ElementFixture(document, "SPAN")); text.textContent = result;
  return { document, root, peers, label, details, summary, diagnostic, text };
}

function synchronize(current: ReturnType<typeof peerSurface>, next: ReturnType<typeof peerSurface>): void {
  synchronizeRetainedSettingsSurface(current.root as unknown as HTMLElement, next.root as unknown as HTMLElement, false);
}

test("nested diagnostic stays mounted when its passive parent changes", () => {
  const current = peerSurface();
  const next = peerSurface("接続を確認しました", "WinB updated");
  synchronize(current, next);
  const diagnostic = current.root.querySelectorAll("[data-settings-passive]").find(node => node.dataset.settingsPassive === "peer-B-diagnostic");
  assert.ok(diagnostic, "fresh diagnostic remains inside the mounted peer list");
  assert.equal(diagnostic.children[0].textContent, "接続を確認しました");
  assert.equal(current.root.contains(diagnostic), true);
  assert.equal(current.peers.contains(diagnostic), false, "fresh child never moves into the detached old parent");
});

for (const open of [false, true]) {
  test(`unchanged polling keeps ${open ? "open" : "closed"} peer details and their connected interaction`, () => {
    const current = peerSurface();
    current.details.open = open;
    current.peers.scrollTop = 240; current.diagnostic.scrollLeft = 12;
    current.document.selected = current.text;
    for (let poll = 0; poll < 4; poll++) {
      synchronize(current, peerSurface());
      assert.equal(current.root.children[0], current.peers, "unchanged peer subtree is not replaced");
      assert.equal(current.details.open, open, "the user's details state is authoritative");
      assert.equal(current.diagnostic.children[0], current.text);
      assert.equal(current.document.selected, current.text, "selected result text remains mounted");
      assert.equal(current.peers.scrollTop, 240);
      assert.equal(current.diagnostic.scrollLeft, 12);
    }
    current.document.activeElement = current.summary;
    synchronize(current, peerSurface());
    assert.equal(current.document.activeElement, current.summary);
    assert.equal(current.document.selected, current.text);
  });
}

test("focused peer keeps its details while changed diagnostics still refresh", () => {
  const current = peerSurface();
  current.details.open = true; current.document.activeElement = current.summary;
  synchronize(current, peerSurface("TLSの確認が完了しました"));
  assert.equal(current.root.children[0], current.peers);
  assert.equal(current.details.open, true);
  assert.equal(current.document.activeElement, current.summary);
  assert.equal(current.details.children[1].children[0].textContent, "TLSの確認が完了しました");
});

test("focused peer switch synchronizes checked state and nearby saved status without replacing focus or disclosures", () => {
  function surface(selected: boolean, saving = false) {
    const view = peerSurface();
    const button = view.peers.append(new ElementFixture(view.document, "BUTTON"));
    button.setAttribute("id", "use-peer-B"); button.setAttribute("role", "switch");
    button.setAttribute("aria-checked", String(selected)); button.setAttribute("aria-busy", String(saving));
    button.setAttribute("aria-label", "WinB · Project · この端末を利用");
    button.innerHTML = selected ? "ON" : "OFF";
    const status = view.peers.append(new ElementFixture(view.document));
    status.setAttribute("data-settings-passive", "peer-B-selection");
    status.textContent = saving ? "保存しています…" : selected ? "利用先に選択済み" : "利用先に選択していません";
    return { ...view, button, status };
  }
  const current = surface(false); current.document.activeElement = current.button; current.details.open = true;
  synchronize(current, surface(false, true));
  assert.equal(current.button.getAttribute("aria-busy"), "true");
  assert.equal(current.button.getAttribute("aria-checked"), "false");
  synchronize(current, surface(true));
  assert.equal(current.document.activeElement, current.button);
  assert.equal(current.button.getAttribute("aria-checked"), "true");
  assert.equal(current.button.getAttribute("aria-busy"), "false");
  assert.equal(current.button.getAttribute("aria-label"), "WinB · Project · この端末を利用");
  assert.equal(current.button.innerHTML, "ON");
  assert.equal(current.details.open, true);
  const status = current.root.querySelectorAll("[data-settings-passive]").find(node => node.dataset.settingsPassive === "peer-B-selection");
  assert.equal(status?.textContent, "利用先に選択済み");
});

test("a changed passive parent preserves an existing details choice and adopts new facts", () => {
  const current = peerSurface(); current.details.open = true;
  synchronize(current, peerSurface("接続を確認しました", "New display name"));
  const details = current.root.querySelectorAll("details[data-details-key]")[0];
  assert.equal(details.open, true);
  assert.equal(current.root.children[0].children[0].textContent, "New display name");
  assert.equal(details.children[1].children[0].textContent, "接続を確認しました");
});
