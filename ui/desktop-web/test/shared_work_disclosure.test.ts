import assert from "node:assert/strict";
import test from "node:test";
import { retainSharedWorkSurface } from "../src/shared_work_render.ts";

// The production retainer receives connected current nodes and a detached new render.
function region(name: string, key: string, open: boolean, text = "unchanged") {
  const detail = { dataset: { detailsKey: key }, open };
  const value = {
    dataset: { sharedRegion: name }, detail, text, replacement: null as unknown,
    contains: () => false,
    querySelector: () => null,
    querySelectorAll: (selector: string) => selector === "details[data-details-key]" ? [detail] : [],
    isEqualNode(other: ReturnType<typeof region>): boolean { return value.text === other.text && detail.open === other.detail.open; },
    replaceWith(other: unknown) { value.replacement = other; },
  };
  return value;
}
function retain(currentRegion: ReturnType<typeof region>, nextRegion: ReturnType<typeof region>, nextOwner = "person/project/job") {
  const current = { dataset: { sharedOwner: "person/project/job" }, querySelector: () => currentRegion };
  const next = { dataset: { sharedOwner: nextOwner }, querySelectorAll: () => [nextRegion] };
  const original = Object.getOwnPropertyDescriptor(globalThis, "document");
  Object.defineProperty(globalThis, "document", { configurable: true, value: { activeElement: null } });
  try { return retainSharedWorkSurface(current as unknown as HTMLElement, next as unknown as HTMLElement); }
  finally { if (original) Object.defineProperty(globalThis, "document", original); else delete (globalThis as Record<string, unknown>).document; }
}

test("an open account stays connected across an unchanged poll so Logout remains reachable", () => {
  const current = region("account", "hub-project-account", true);
  const next = region("account", "hub-project-account", false);
  assert.equal(retain(current, next), true);
  assert.equal(next.detail.open, true);
  assert.equal(current.replacement, null, "unchanged account nodes and focused controls stay connected");
});

test("updated draft and attachment regions inherit only matching disclosure open state", () => {
  for (const [name, key] of [["draft", "hub-new-chat-options"], ["inputs", "hub-inputs"], ["followup", "hub-followup-options"]]) {
    for (const open of [false, true]) {
      const current = region(name, key, open, "before");
      const next = region(name, key, !open, "after");
      assert.equal(retain(current, next), true);
      assert.equal(next.detail.open, open);
      assert.equal(current.replacement, next, "new content is rendered without resetting the disclosure");
    }
    const current = region(name, key, true);
    const different = region(name, "different-detail", false);
    assert.equal(retain(current, different), true);
    assert.equal(different.detail.open, false);
  }
});

test("a different person, project or conversation never inherits disclosure state", () => {
  const current = region("account", "hub-project-account", true);
  const next = region("account", "hub-project-account", false);
  assert.equal(retain(current, next, "other-person/project/job"), false);
  assert.equal(next.detail.open, false);
  assert.equal(current.replacement, null);
});
