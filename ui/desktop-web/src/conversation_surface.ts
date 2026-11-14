import { escapeHtml } from "./utils.ts";

/** Storage and command ownership differ; the visible conversation structure does not. */
export function renderConversationMain(parts: {
  className?: string;
  attributes?: string;
  topbar: string;
  activity: string;
  thread: string;
  threadClassName?: string;
  composer: string;
}): string {
  return `<main class="conversation ${parts.className ?? ""}" ${parts.attributes ?? ""}>
    ${parts.topbar}
    <div class="run-activity-stack">${parts.activity}</div>
    <section class="thread ${parts.threadClassName ?? ""}" id="thread" tabindex="-1" aria-label="会話履歴">
      ${parts.thread}
    </section>
    ${parts.composer}
  </main>`;
}

function syncAttributes(live: Element, next: Element): void {
  for (const name of live.getAttributeNames()) if (!next.hasAttribute(name)) live.removeAttribute(name);
  for (const { name, value } of Array.from(next.attributes)) {
    if (live.getAttribute(name) !== value) live.setAttribute(name, value);
  }
}

function historyRowKey(row: Element): string {
  const identity = row.getAttribute("data-history-identity");
  // The newest-answer rail marker changes when another answer starts. The
  // canonical history identity of the already displayed answer stays the same.
  return identity ? `history:${identity}` : `anchor:${row.getAttribute("data-history-anchor")}`;
}

/** Update a local conversation without disconnecting unchanged history rows from the document. */
export function retainConversationThread(root: HTMLElement, nextRoot: DocumentFragment): boolean {
  const path = ".app-frame > .shell > main.conversation > #thread";
  const thread = root.querySelector<HTMLElement>(path);
  const nextThread = nextRoot.querySelector<HTMLElement>(path);
  if (!thread || !nextThread) return false;

  const currentRows = new Map(Array.from(thread.querySelectorAll<HTMLElement>(":scope > [data-history-anchor]"),
    row => [historyRowKey(row), row]));
  const children = Array.from(nextThread.children, next => {
    if (!next.hasAttribute("data-history-anchor")) return next;
    const key = historyRowKey(next);
    const old = currentRows.get(key);
    if (!old || old.tagName !== next.tagName) return next;
    currentRows.delete(key);
    syncAttributes(old, next);
    if (old.isEqualNode(next)) return old;
    const body = old.querySelector<HTMLElement>(":scope > .message-body > .markdown-body");
    const nextBody = next.querySelector<HTMLElement>(":scope > .message-body > .markdown-body");
    if (!body || !nextBody || !body.isEqualNode(nextBody)) return next;
    retainBranch(body, nextBody, old);
    return old;
  });
  const retained = new Set(children);
  for (const old of Array.from(thread.children)) if (!retained.has(old)) old.remove();
  let cursor = thread.firstElementChild;
  for (const child of children) {
    if (child === cursor) cursor = cursor.nextElementSibling;
    else thread.insertBefore(child, cursor);
  }

  // Keep the thread and its ancestors attached. Sibling surfaces receive the normal
  // fresh markup and event bindings, including newly added or removed overlays.
  retainBranch(thread, nextThread, root);
  return true;
}

function retainBranch(live: HTMLElement, next: HTMLElement, root: HTMLElement): void {
  while (live !== root) {
    syncAttributes(live, next);
    const siblings = Array.from(next.parentNode!.childNodes);
    const index = siblings.indexOf(next);
    while (live.previousSibling) live.previousSibling.remove();
    while (live.nextSibling) live.nextSibling.remove();
    live.before(...siblings.slice(0, index));
    live.after(...siblings.slice(index + 1));
    live = live.parentElement!;
    if (live === root) break;
    next = next.parentElement!;
  }
}

export function renderConversationInput(input: {
  id: string;
  value: string;
  label: string;
  placeholder: string;
  attributes?: string;
}): string {
  return `<label class="sr-only" for="${escapeHtml(input.id)}">${escapeHtml(input.label)}</label>
    <textarea id="${escapeHtml(input.id)}" placeholder="${escapeHtml(input.placeholder)}" ${input.attributes ?? ""}>${escapeHtml(input.value)}</textarea>`;
}
