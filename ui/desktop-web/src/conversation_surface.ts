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
