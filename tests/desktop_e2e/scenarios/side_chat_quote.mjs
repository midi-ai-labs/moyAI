import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import {
  SCRIPTED_PROVIDER_MODEL_ID,
  SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_ARTIFACT_PATH,
  SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_FIRST_RESPONSE,
  SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAIN_PROMPT,
  SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAIN_RESPONSE,
  SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_TRANSCRIPT_SELECTION,
  createSideChatQuoteProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";
import {
  WebviewInput,
  assertTrustedProbeSequence,
  assertTrustedTextInsertion,
} from "../drivers/webview_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import {
  captureScenarioScreenshot,
  selectedNavigationIdentity,
} from "./observations.mjs";
import {
  providerRestartFixtureConfig,
  quiesceProviderResource,
} from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:side-chat.quote";
const SIDE_PROVIDER_PROFILE = "openai_responses";
const MAIN_DRAFT_SENTINEL = "main composer remains owned";
const MAX_REVERSE_TAB_STEPS = 64;

const MAIN_PROMPT = Object.freeze({
  selector: "section.composer textarea#prompt",
  identity: { tag: "TEXTAREA", id: "prompt" },
});
const MAIN_SEND = Object.freeze({
  selector: 'section.composer button[data-action="send"]',
  identity: { tag: "BUTTON", action: "send" },
});
const SHOW_SETTINGS = Object.freeze({
  selector: 'aside.sidebar button.settings[data-action="show-config"][title="設定"]',
  identity: { tag: "BUTTON", action: "show-config" },
});
const SIDE_SETTINGS_NAV = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] nav.settings-nav a[href="#settings-side-chat"]',
  identity: { tag: "A", href: "#settings-side-chat" },
});
const SIDE_MANUAL_DETAILS = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] details[data-details-key="side-chat-manual-model"] > summary',
  identity: { tag: "DETAILS", detailsKey: "side-chat-manual-model" },
});
const SIDE_MANUAL_MODEL = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] input#side-chat-model-manual[data-config-key="side_chat.model"]',
  identity: { tag: "INPUT", id: "side-chat-model-manual", configKey: "side_chat.model" },
});
const SIDE_BASE_URL = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] input#side-chat-base-url[data-config-key="side_chat.base_url"]',
  identity: { tag: "INPUT", id: "side-chat-base-url", configKey: "side_chat.base_url" },
});
const SIDE_PROVIDER_PROFILE_CONTROL = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] select#side-chat-provider-profile[data-config-key="side_chat.provider_profile"]',
  identity: { tag: "SELECT", id: "side-chat-provider-profile", configKey: "side_chat.provider_profile" },
});
const SIDE_SYSTEM_PROMPT = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] textarea#side-chat-system-prompt[data-config-key="side_chat.system_prompt"]',
  identity: { tag: "TEXTAREA", id: "side-chat-system-prompt", configKey: "side_chat.system_prompt" },
});
const SAVE_GLOBAL_CONFIG = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="save-global-config"]',
  identity: { tag: "BUTTON", action: "save-global-config" },
});
const CLOSE_SETTINGS = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="close-overlay"]',
  identity: { tag: "BUTTON", action: "close-overlay" },
});
const SIDE_PROMPT = Object.freeze({
  selector: 'aside.side-chat-pane[data-pane-mode="side-chat"] textarea#side-chat-prompt',
  identity: { tag: "TEXTAREA", id: "side-chat-prompt" },
});
const SIDE_SEND = Object.freeze({
  selector: 'aside.side-chat-pane[data-pane-mode="side-chat"] button[data-action="send-side-chat"]',
  identity: { tag: "BUTTON", action: "send-side-chat" },
});
const SIDE_STOP = Object.freeze({
  selector: 'aside.side-chat-pane[data-pane-mode="side-chat"] button[data-action="cancel-side-chat"]',
  identity: { tag: "BUTTON", action: "cancel-side-chat" },
});
const SHOW_SIDE = Object.freeze({
  selector: 'button[data-action="show-side-chat-pane"]',
  identity: { tag: "BUTTON", action: "show-side-chat-pane" },
});

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function productFailure(code, message, evidence) {
  return new DesktopE2eError("product", code, message, evidence);
}

function errorObservation(error) {
  return {
    name: error?.name ?? "Error",
    code: error?.code ?? null,
    message: error?.message ?? String(error),
    evidence: error?.evidence ?? null,
  };
}

function quoteDraft(selectedText) {
  return `> Side Chat 引用\n> ${selectedText}\n\n`;
}

function submittedQuoteText(selectedText) {
  return quoteDraft(selectedText).trim();
}

function responseRows(ledger) {
  return Array.isArray(ledger)
    ? ledger.filter((row) => row?.route === "responses")
    : [];
}

function responseRoles(ledger) {
  return responseRows(ledger).map((row) => row?.contract?.role ?? null);
}

function exactResponseRoles(ledger, expectedRoles, { finalHeld = false } = {}) {
  const rows = responseRows(ledger);
  if (!sameValue(responseRoles(ledger), expectedRoles)) return false;
  return rows.every((row, index) => row?.contract?.pass === true
    && (finalHeld && index === rows.length - 1
      ? row.response_status === null && ["held", "peer_closed"].includes(row.response_phase)
      : row.response_status === 200 && row.response_phase === "completed"));
}

function canonicalSourceRows(projection) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  const assistant = rows.find((row) => row?.row_kind === "assistant"
    && row?.body === SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAIN_RESPONSE) ?? null;
  const artifact = rows.find((row) => row?.row_kind === "file_changes"
    && Array.isArray(row?.file_changes)
    && row.file_changes.some((change) => change?.path === SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_ARTIFACT_PATH)) ?? null;
  const assistantIdentity = typeof assistant?.stable_history_identity === "string"
    && assistant.stable_history_identity.length > 0
    ? assistant.stable_history_identity
    : null;
  const artifactIdentity = typeof artifact?.stable_history_identity === "string"
    && artifact.stable_history_identity.length > 0
    ? artifact.stable_history_identity
    : null;
  return {
    assistant,
    artifact,
    assistant_identity: assistantIdentity,
    artifact_identity: artifactIdentity,
    pass: assistant !== null
      && artifact !== null
      && assistantIdentity !== null
      && artifactIdentity !== null
      && assistantIdentity !== artifactIdentity,
  };
}

function mainSeedSettled(projection) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  const sources = canonicalSourceRows(projection);
  return projection?.run_status_key === "completed"
    && projection?.task_activity_state === "idle"
    && projection?.busy === false
    && projection?.agent_tree_active === false
    && projection?.post_run_refresh_pending === false
    && projection?.background_mutation_pending === false
    && projection?.navigation_loading === false
    && projection?.navigation_admission_open === true
    && projection?.draft_prompt === ""
    && projection?.composer_submit_mode === "new_request"
    && projection?.can_submit === true
    && rows.some((row) => row?.row_kind === "user"
      && row?.body === SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAIN_PROMPT)
    && sources.pass;
}

export async function observeSideChatQuoteSurface(cdp) {
  return cdp.evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function') throw new Error('tauri-invoke-unavailable');
    const projection = await invoke('desktop_state');
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const visible = (element) => {
      if (!(element instanceof HTMLElement) || !element.isConnected) return false;
      const style = getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity) !== 0
        && rect.width > 0 && rect.height > 0;
    };
    const enabled = (element) => element instanceof HTMLElement
      && !element.matches(':disabled')
      && element.getAttribute('aria-disabled') !== 'true'
      && element.closest('[inert]') === null;
    const mainPrompt = document.querySelector('section.composer textarea#prompt');
    const mainSend = document.querySelector('section.composer button[data-action="send"]');
    const mainStop = document.querySelector('section.run-strip button[data-action="cancel-run"]');
    const sidePane = document.querySelector('aside.side-chat-pane[data-pane-mode="side-chat"]');
    const sidePrompt = sidePane?.querySelector('textarea#side-chat-prompt') ?? null;
    const sideSend = sidePane?.querySelector('button[data-action="send-side-chat"]') ?? null;
    const sideStop = sidePane?.querySelector('button[data-action="cancel-side-chat"]') ?? null;
    const pending = sidePane?.querySelector('.side-chat-pending-quote') ?? null;
    const settings = document.querySelector('[role="dialog"][aria-labelledby="config-dialog-title"]');
    const sideSettings = settings?.querySelector('section#settings-side-chat') ?? null;
    const profile = sideSettings?.querySelector('select#side-chat-provider-profile') ?? null;
    const base = sideSettings?.querySelector('input#side-chat-base-url') ?? null;
    const manualDetails = sideSettings?.querySelector('details[data-details-key="side-chat-manual-model"]') ?? null;
    const manualModel = sideSettings?.querySelector('input#side-chat-model-manual') ?? null;
    const systemPrompt = sideSettings?.querySelector('textarea#side-chat-system-prompt') ?? null;
    const save = settings?.querySelector('button[data-action="save-global-config"]') ?? null;
    const quoteActions = Array.from(document.querySelectorAll(
      'article.message[data-history-identity] button[data-action="quote-selection-to-side-chat"]'
    )).map((button) => {
      const row = button.closest('article.message');
      return {
        source_history_item_id: button instanceof HTMLElement
          ? (button.dataset.sourceHistoryItemId ?? null)
          : null,
        history_identity: row?.getAttribute('data-history-identity') ?? null,
        source_kind: row instanceof HTMLElement ? (row.dataset.sideChatQuoteSourceKind ?? null) : null,
        owner_session_id: row instanceof HTMLElement
          ? (row.dataset.sideChatQuoteOwnerSessionId ?? null)
          : null,
        visible: visible(button),
        enabled: enabled(button),
      };
    });
    return {
      projection,
      main: {
        primary_rows: Array.from(document.querySelectorAll('article.message.user, article.message.assistant, article.message.error')).map((row) => ({
          id: row.getAttribute('data-history-identity'),
          kind: row.classList.contains('user') ? 'user' : row.classList.contains('assistant') ? 'assistant' : 'error',
          body: (row.querySelector('.message-body > .markdown-body')?.textContent ?? '').trim(),
        })),
        prompt_value: mainPrompt instanceof HTMLTextAreaElement ? mainPrompt.value : null,
        prompt_visible: visible(mainPrompt),
        prompt_enabled: enabled(mainPrompt),
        send_visible: visible(mainSend),
        send_enabled: enabled(mainSend),
        stop_count: document.querySelectorAll('section.run-strip button[data-action="cancel-run"]').length,
        stop_visible: visible(mainStop),
        stop_enabled: enabled(mainStop),
      },
      side: {
        pane_count: document.querySelectorAll('aside.side-chat-pane[data-pane-mode="side-chat"]').length,
        pane_visible: visible(sidePane),
        setup_visible: visible(sidePane?.querySelector('.side-chat-setup')),
        owner_session_id: sidePane instanceof HTMLElement ? (sidePane.dataset.sideChatOwner ?? null) : null,
        prompt_value: sidePrompt instanceof HTMLTextAreaElement ? sidePrompt.value : null,
        prompt_visible: visible(sidePrompt),
        prompt_enabled: enabled(sidePrompt),
        send_visible: visible(sideSend),
        send_enabled: enabled(sideSend),
        stop_visible: visible(sideStop),
        stop_enabled: enabled(sideStop),
        metadata: Array.from(sidePane?.querySelectorAll('.side-chat-context-meta > span') ?? [])
          .map((node) => (node.textContent ?? '').trim()),
        truncated_count: sidePane?.querySelectorAll('.side-chat-context-truncated').length ?? 0,
        pending_count: sidePane?.querySelectorAll('.side-chat-pending-quote').length ?? 0,
        pending_label: (pending?.querySelector('strong')?.textContent ?? '').trim(),
        pending_position: (pending?.querySelector('small')?.textContent ?? '').trim(),
        pending_text: (pending?.querySelector('blockquote')?.textContent ?? '').trim(),
        messages: Array.from(sidePane?.querySelectorAll('article.side-chat-message') ?? []).map((row) => ({
          id: row.getAttribute('data-side-chat-message-id'),
          role: row.classList.contains('side-chat-message-user') ? 'user'
            : row.classList.contains('side-chat-message-assistant') ? 'assistant' : 'error',
          content: (row.querySelector('.markdown-body, .side-chat-message-error')?.textContent ?? '').trim(),
        })),
      },
      settings: {
        visible: visible(settings),
        side_visible: visible(sideSettings),
        profile_value: profile instanceof HTMLSelectElement ? profile.value : null,
        base_value: base instanceof HTMLInputElement ? base.value : null,
        manual_details_open: manualDetails instanceof HTMLDetailsElement ? manualDetails.open : null,
        manual_value: manualModel instanceof HTMLInputElement ? manualModel.value : null,
        manual_visible: visible(manualModel),
        manual_enabled: enabled(manualModel),
        system_prompt_value: systemPrompt instanceof HTMLTextAreaElement ? systemPrompt.value : null,
        system_prompt_visible: visible(systemPrompt),
        system_prompt_enabled: enabled(systemPrompt),
        dirty: settings?.querySelectorAll('.dirty-badge.visible').length === 1,
        save_visible: visible(save),
        save_enabled: enabled(save),
      },
      quote_actions: quoteActions,
      visible_fatal_count: Array.from(document.querySelectorAll('.fatal')).filter(visible).length,
      visible_recoverable_error_count: Array.from(document.querySelectorAll('.ui-error-notice')).filter(visible).length,
      visible_dialog_count: Array.from(document.querySelectorAll('[role="dialog"], [role="alertdialog"]')).filter(visible).length,
    };
  })()`);
}

export function surfaceHasNoErrors(surface) {
  return surface?.visible_fatal_count === 0
    && surface?.visible_recoverable_error_count === 0;
}

export async function waitForProductStage({ label, timeoutMs = 30_000, sample, accept, code, message }) {
  try {
    return await waitForObservation({
      label,
      timeoutMs,
      pollMs: 75,
      retrySampleErrors: false,
      sample,
      accept,
    });
  } catch (error) {
    if (error?.code === "observation-timeout"
      && error?.evidence?.last_value !== null
      && error?.evidence?.last_error === null) {
      throw productFailure(code, message, { observation: error.evidence });
    }
    throw error;
  }
}

export async function trustedClick(input, locator) {
  const start = (await input.snapshotProbe()).sequence;
  const target = await input.click(locator);
  const snapshot = await input.snapshotProbe(start);
  return {
    target,
    probe: assertTrustedProbeSequence(snapshot, {
      afterSequence: start,
      expected: [
        { type: "pointerdown", identity: locator.identity, button: 0, buttons: 1 },
        { type: "pointerup", identity: locator.identity, button: 0, buttons: 0 },
        { type: "click", identity: locator.identity, button: 0, buttons: 0 },
      ],
    }),
  };
}

export async function trustedInsert(input, locator, text) {
  const focus = await trustedClick(input, locator);
  const start = (await input.snapshotProbe()).sequence;
  const insertion = await input.insertText(locator, text);
  const probe = assertTrustedTextInsertion(await input.snapshotProbe(start), {
    afterSequence: start,
    identity: locator.identity,
    text,
  });
  return { focus, insertion, probe };
}

async function trustedReplace(input, locator, text) {
  const focus = await trustedClick(input, locator);
  const clearStart = (await input.snapshotProbe()).sequence;
  await input.keyDown("Control");
  try {
    await input.pressKey("a");
  } finally {
    await input.keyUp("Control");
  }
  await input.pressKey("Backspace");
  const cleared = await input.snapshotProbe(clearStart);
  const clearProbe = assertTrustedProbeSequence(cleared, {
    afterSequence: clearStart,
    expected: [
      { type: "keydown", identity: locator.identity, key: "Control", code: "ControlLeft" },
      { type: "keydown", identity: locator.identity, key: "a", code: "KeyA" },
      { type: "keyup", identity: locator.identity, key: "a", code: "KeyA" },
      { type: "keyup", identity: locator.identity, key: "Control", code: "ControlLeft" },
      { type: "keydown", identity: locator.identity, key: "Backspace", code: "Backspace" },
      { type: "keyup", identity: locator.identity, key: "Backspace", code: "Backspace" },
    ],
  });
  let insertion = null;
  if (text.length > 0) {
    const insertStart = cleared.sequence;
    const inserted = await input.insertText(locator, text);
    insertion = {
      inserted,
      probe: assertTrustedTextInsertion(await input.snapshotProbe(insertStart), {
        afterSequence: insertStart,
        identity: locator.identity,
        text,
      }),
    };
  }
  return { focus, clear_probe: clearProbe, insertion };
}

async function trustedSelectSideProviderProfile(input) {
  const focus = await trustedClick(input, SIDE_PROVIDER_PROFILE_CONTROL);
  const start = (await input.snapshotProbe()).sequence;
  await input.pressKey("Home");
  await input.pressKey("ArrowDown");
  await input.pressKey("ArrowDown");
  await input.pressKey("Enter");
  return {
    focus,
    probe: assertTrustedProbeSequence(await input.snapshotProbe(start), {
      afterSequence: start,
      expected: [{ type: "change", identity: SIDE_PROVIDER_PROFILE_CONTROL.identity }],
    }),
  };
}

function globalConfigValues(projection, overrides) {
  const fields = Array.isArray(projection?.config_fields) ? projection.config_fields : [];
  return fields.map((field) => ({
    key: field.key,
    text: Object.hasOwn(overrides, field.key) ? overrides[field.key] : field.value,
  }));
}

function configFieldValue(projection, key) {
  const matches = (projection?.config_fields ?? []).filter((field) => field?.key === key);
  return matches.length === 1 ? matches[0].value : null;
}

function quoteActionLocator(historyItemId) {
  if (typeof historyItemId !== "string" || !/^[A-Za-z0-9:_-]+$/.test(historyItemId)) {
    throw new TypeError("quote action requires one selector-safe canonical history identity");
  }
  return Object.freeze({
    selector: `article.message[data-history-identity="${historyItemId}"] button[data-action="quote-selection-to-side-chat"][data-source-history-item-id="${historyItemId}"]`,
    identity: { tag: "BUTTON", action: "quote-selection-to-side-chat" },
  });
}

async function selectExactRowText(cdp, historyItemId, selectedText, { refocusAction = false } = {}) {
  return cdp.evaluate(`(() => {
    const historyItemId = ${JSON.stringify(historyItemId)};
    const selectedText = ${JSON.stringify(selectedText)};
    const row = Array.from(document.querySelectorAll('article.message[data-history-identity]'))
      .find((candidate) => candidate.getAttribute('data-history-identity') === historyItemId) ?? null;
    if (!(row instanceof HTMLElement)) return { pass: false, reason: 'row-not-found' };
    const action = row.querySelector('button[data-action="quote-selection-to-side-chat"]');
    if (!(action instanceof HTMLButtonElement)) return { pass: false, reason: 'action-not-found' };
    const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT);
    let node = walker.nextNode();
    while (node !== null && !(node.textContent ?? '').includes(selectedText)) node = walker.nextNode();
    if (!(node instanceof Text)) return { pass: false, reason: 'text-not-found' };
    const start = (node.textContent ?? '').indexOf(selectedText);
    const range = document.createRange();
    range.setStart(node, start);
    range.setEnd(node, start + selectedText.length);
    const selection = window.getSelection();
    if (selection === null) return { pass: false, reason: 'selection-unavailable' };
    if (${refocusAction ? "true" : "false"}) action.focus({ preventScroll: true });
    selection.removeAllRanges();
    selection.addRange(range);
    return {
      pass: selection.toString() === selectedText
        && (!${refocusAction ? "true" : "false"} || document.activeElement === action),
      selected_text: selection.toString(),
      history_identity: row.dataset.historyIdentity ?? null,
      source_kind: row.dataset.sideChatQuoteSourceKind ?? null,
      owner_session_id: row.dataset.sideChatQuoteOwnerSessionId ?? null,
      action_focused: document.activeElement === action,
    };
  })()`);
}

async function reverseTabToQuoteAction(input, cdp, historyItemId) {
  const steps = [];
  for (let index = 0; index < MAX_REVERSE_TAB_STEPS; index += 1) {
    await input.keyDown("Shift");
    await input.pressKey("Tab");
    await input.keyUp("Shift");
    const active = await cdp.evaluate(`(() => {
      const node = document.activeElement;
      return {
        tag: node instanceof Element ? node.tagName.toUpperCase() : null,
        action: node instanceof HTMLElement ? (node.dataset.action ?? null) : null,
        source_history_item_id: node instanceof HTMLElement
          ? (node.dataset.sourceHistoryItemId ?? null)
          : null,
      };
    })()`);
    steps.push(active);
    if (active?.action === "quote-selection-to-side-chat"
      && active?.source_history_item_id === historyItemId) {
      return { pass: true, step_count: index + 1, active, steps };
    }
  }
  throw productFailure(
    "side-chat-quote-keyboard-focus",
    "reverse Tab navigation did not reach the exact artifact quote action",
    { history_item_id: historyItemId, steps },
  );
}

function expectedSubmitCommand(projection, sourceKind, sourceHistoryItemId, selectedText) {
  const side = projection?.side_chat;
  return {
    command: "submit_side_chat",
    args: {
      ownerSessionId: side.owner_session_id,
      chatId: side.chat_id,
      expectedGeneration: side.generation,
      expectedDraftRevision: side.draft_revision,
      expectedOwnerAppendPosition: side.context_as_of_append_position,
      quote: {
        sourceKind,
        sourceHistoryItemId,
        sourceAppendPosition: side.context_as_of_append_position,
        selectedText,
      },
      text: submittedQuoteText(selectedText),
    },
  };
}

function expectedCancelCommand(projection) {
  const side = projection?.side_chat;
  return {
    command: "cancel_side_chat",
    args: {
      ownerSessionId: side.owner_session_id,
      chatId: side.chat_id,
      expectedGeneration: side.generation,
    },
  };
}

function exactQuoteSurface(surface, {
  ownerSessionId,
  selectedText,
  sourceKind,
  mainDraft = MAIN_DRAFT_SENTINEL,
}) {
  const side = surface?.projection?.side_chat;
  const sourceLabel = sourceKind === "artifact" ? "作業結果から引用" : "会話から引用";
  return surfaceHasNoErrors(surface)
    && side?.configured === true
    && side.deleting === false
    && side.owner_session_id === ownerSessionId
    && typeof side.chat_id === "string"
    && side.chat_id.length > 0
    && side.context_scope === "owner_session"
    && typeof side.context_as_of_append_position === "string"
    && /^\d+$/.test(side.context_as_of_append_position)
    && side.context_truncated === false
    && side.draft_text === quoteDraft(selectedText)
    && surface?.main?.prompt_value === mainDraft
    && surface.main.prompt_visible === true
    && surface.main.prompt_enabled === true
    && surface.main.send_visible === true
    && surface.main.send_enabled === true
    && surface?.side?.pane_count === 1
    && surface.side.pane_visible === true
    && surface.side.owner_session_id === ownerSessionId
    && surface.side.prompt_value === quoteDraft(selectedText)
    && surface.side.prompt_visible === true
    && surface.side.prompt_enabled === true
    && surface.side.send_visible === true
    && surface.side.send_enabled === true
    && sameValue(surface.side.metadata, [
      "参照: このタスクの履歴",
      `履歴位置 ${side.context_as_of_append_position}`,
    ])
    && surface.side.truncated_count === 0
    && surface.side.pending_count === 1
    && surface.side.pending_label === sourceLabel
    && surface.side.pending_position === `履歴位置 ${side.context_as_of_append_position}`
    && surface.side.pending_text === selectedText;
}

export async function saveGlobalSideChatAndOpen({
  cdp,
  input,
  providerBaseUrl,
  ownerSessionId,
  systemPrompt = "",
}) {
  const open = await trustedClick(input, SHOW_SETTINGS);
  await waitForProductStage({
    label: "Side Chat quote Settings overlay",
    timeoutMs: 10_000,
    sample: () => observeSideChatQuoteSurface(cdp),
    accept: (surface) => surface?.projection?.overlay === "config"
      && surface?.settings?.visible === true
      && surfaceHasNoErrors(surface),
    code: "side-chat-quote-settings-open",
    message: "trusted Settings activation did not open the global Settings owner",
  });
  const navigate = await trustedClick(input, SIDE_SETTINGS_NAV);
  const ready = await waitForProductStage({
    label: "global Side Chat settings section",
    timeoutMs: 10_000,
    sample: () => observeSideChatQuoteSurface(cdp),
    accept: (surface) => surface?.settings?.side_visible === true
      && typeof surface.settings.profile_value === "string"
      && typeof surface.settings.base_value === "string"
      && surface.settings.system_prompt_value === ""
      && surface.settings.system_prompt_visible === true
      && surface.settings.system_prompt_enabled === true
      && surfaceHasNoErrors(surface),
    code: "side-chat-quote-global-settings",
    message: "Settings did not expose the independent global Side Chat fields",
  });
  let details = null;
  if (ready.value.settings.manual_details_open !== true) {
    details = await trustedClick(input, SIDE_MANUAL_DETAILS);
  }
  const manual = await waitForProductStage({
    label: "Side Chat quote manual model input",
    timeoutMs: 10_000,
    sample: () => observeSideChatQuoteSurface(cdp),
    accept: (surface) => surface?.settings?.manual_details_open === true
      && surface.settings.manual_visible === true
      && surface.settings.manual_enabled === true
      && typeof surface.settings.manual_value === "string",
    code: "side-chat-quote-manual-model",
    message: "the global Side Chat manual model input was not editable",
  });
  const profile = ready.value.settings.profile_value === SIDE_PROVIDER_PROFILE
    ? null
    : await trustedSelectSideProviderProfile(input);
  const baseUrl = await trustedReplace(input, SIDE_BASE_URL, providerBaseUrl);
  const model = await trustedReplace(input, SIDE_MANUAL_MODEL, SCRIPTED_PROVIDER_MODEL_ID);
  const systemPromptTyping = await trustedReplace(input, SIDE_SYSTEM_PROMPT, systemPrompt);
  const committable = await waitForProductStage({
    label: "global Side Chat settings save admission",
    timeoutMs: 10_000,
    sample: () => observeSideChatQuoteSurface(cdp),
    accept: (surface) => surface?.settings?.profile_value === SIDE_PROVIDER_PROFILE
      && surface.settings.base_value === providerBaseUrl
      && surface.settings.manual_value === SCRIPTED_PROVIDER_MODEL_ID
      && surface.settings.system_prompt_value === systemPrompt
      && surface.settings.dirty === true
      && surface.settings.save_visible === true
      && surface.settings.save_enabled === true
      && surfaceHasNoErrors(surface),
    code: "side-chat-quote-global-save-admission",
    message: "the exact global tool-less Side Chat defaults did not become saveable",
  });
  const expectedSave = {
    command: "save_global_config",
    args: {
      values: globalConfigValues(ready.value.projection, {
        "side_chat.base_url": providerBaseUrl,
        "side_chat.model": SCRIPTED_PROVIDER_MODEL_ID,
        "side_chat.system_prompt": systemPrompt.trim(),
        "side_chat.provider_profile": SIDE_PROVIDER_PROFILE,
      }),
      expectedTarget: structuredClone(ready.value.projection.config_target),
    },
  };
  const save = await trustedClick(input, SAVE_GLOBAL_CONFIG);
  const saved = await waitForProductStage({
    label: "global Side Chat defaults saved",
    timeoutMs: 30_000,
    sample: () => observeSideChatQuoteSurface(cdp),
    accept: (surface) => {
      const side = surface?.projection?.side_chat;
      return side?.configured === false
        && side.owner_session_id === ownerSessionId
        && side.base_url === providerBaseUrl
        && side.model === SCRIPTED_PROVIDER_MODEL_ID
        && side.system_prompt === systemPrompt.trim()
        && side.provider_profile === SIDE_PROVIDER_PROFILE
        && side.status === "idle"
        && side.chat_id === null
        && side.can_send === false
        && side.can_cancel === false
        && configFieldValue(surface.projection, "side_chat.base_url") === providerBaseUrl
        && configFieldValue(surface.projection, "side_chat.model") === SCRIPTED_PROVIDER_MODEL_ID
        && configFieldValue(surface.projection, "side_chat.system_prompt") === systemPrompt.trim()
        && configFieldValue(surface.projection, "side_chat.provider_profile") === SIDE_PROVIDER_PROFILE
        && surface.settings.dirty === false
        && surfaceHasNoErrors(surface);
    },
    code: "side-chat-quote-global-save-settlement",
    message: "global Side Chat defaults did not persist independently of a conversation binding",
  });
  const close = await trustedClick(input, CLOSE_SETTINGS);
  const closed = await waitForProductStage({
    label: "Side Chat quote Settings close",
    timeoutMs: 10_000,
    sample: () => observeSideChatQuoteSurface(cdp),
    accept: (surface) => surface?.projection?.overlay === "none"
      && surface?.settings?.visible === false
      && surfaceHasNoErrors(surface),
    code: "side-chat-quote-settings-close",
    message: "saved global Side Chat Settings did not close cleanly",
  });
  const ensureExpected = {
    command: "ensure_side_chat",
    args: {
      ownerSessionId,
      expectedConfigGeneration: closed.value.projection.config_target.configGeneration,
    },
  };
  const openSide = await trustedClick(input, SHOW_SIDE);
  const configured = await waitForProductStage({
    label: "Side Chat binding materialized from global defaults",
    timeoutMs: 30_000,
    sample: () => observeSideChatQuoteSurface(cdp),
    accept: (surface) => {
      const side = surface?.projection?.side_chat;
      return side?.configured === true
        && side.owner_session_id === ownerSessionId
        && side.base_url === providerBaseUrl
        && side.model === SCRIPTED_PROVIDER_MODEL_ID
        && side.system_prompt === systemPrompt.trim()
        && side.provider_profile === SIDE_PROVIDER_PROFILE
        && side.status === "idle"
        && side.context_scope === "owner_session"
        && typeof side.context_as_of_append_position === "string"
        && /^\d+$/.test(side.context_as_of_append_position)
        && side.context_truncated === false
        && side.can_send === true
        && side.can_cancel === false
        && surface.side.pane_count === 1
        && surface.side.pane_visible === true
        && surface.side.setup_visible === false
        && surface.side.prompt_value === ""
        && surfaceHasNoErrors(surface);
    },
    code: "side-chat-quote-ensure-settlement",
    message: "opening Side Chat did not snapshot the saved global defaults for the selected session",
  });
  return {
    open,
    navigate,
    details,
    manual: manual.value,
    profile,
    base_url: baseUrl,
    model,
    system_prompt_typing: systemPromptTyping,
    committable: committable.value,
    save,
    saved: saved.value.settings,
    expected_save_command: expectedSave,
    expected_ensure_command: ensureExpected,
    expected_commands: [expectedSave, ensureExpected],
    open_side: openSide,
    configured: configured.value.projection.side_chat,
    close,
    closed_projection_revision: closed.value.projection.projection_revision,
  };
}

export async function inspectGlobalSideChatSettings({
  cdp,
  input,
  sink,
  providerBaseUrl,
  model,
  systemPrompt,
  evidenceName,
  evidenceOwner = OWNER,
}) {
  const open = await trustedClick(input, SHOW_SETTINGS);
  await waitForProductStage({
    label: "persisted Side Chat Settings overlay",
    timeoutMs: 10_000,
    sample: () => observeSideChatQuoteSurface(cdp),
    accept: (surface) => surface?.projection?.overlay === "config"
      && surface?.settings?.visible === true
      && surfaceHasNoErrors(surface),
    code: "side-chat-settings-reopen",
    message: "trusted Settings activation did not reopen the persisted global Side Chat defaults",
  });
  const navigate = await trustedClick(input, SIDE_SETTINGS_NAV);
  const restored = await waitForProductStage({
    label: "persisted Side Chat Settings values",
    timeoutMs: 10_000,
    sample: () => observeSideChatQuoteSurface(cdp),
    accept: (surface) => surface?.settings?.side_visible === true
      && surface.settings.profile_value === SIDE_PROVIDER_PROFILE
      && surface.settings.base_value === providerBaseUrl
      && surface.settings.manual_value === model
      && surface.settings.system_prompt_value === systemPrompt
      && surface.settings.system_prompt_visible === true
      && surface.settings.system_prompt_enabled === true
      && surfaceHasNoErrors(surface),
    code: "side-chat-settings-values-not-restored",
    message: "Side Chat Settings did not restore the exact persisted global values",
  });
  const prompt = await trustedClick(input, SIDE_SYSTEM_PROMPT);
  const screenshot = await captureScenarioScreenshot({
    cdp,
    sink,
    name: evidenceName,
    owner: evidenceOwner,
  });
  const close = await trustedClick(input, CLOSE_SETTINGS);
  const closed = await waitForProductStage({
    label: "persisted Side Chat Settings close",
    timeoutMs: 10_000,
    sample: () => observeSideChatQuoteSurface(cdp),
    accept: (surface) => surface?.projection?.overlay === "none"
      && surface?.settings?.visible === false
      && surfaceHasNoErrors(surface),
    code: "side-chat-settings-reopen-close",
    message: "persisted Side Chat Settings did not close cleanly",
  });
  return {
    open,
    navigate,
    restored: restored.value.settings,
    prompt,
    screenshot,
    close,
    closed_projection_revision: closed.value.projection.projection_revision,
  };
}

async function settleResources(state, input, commands, primaryError) {
  const outcome = { input: null, command_probe: null, failures: [] };
  try { outcome.input = await input.cleanup(); }
  catch (error) { outcome.failures.push({ owner: "webview-input", error: errorObservation(error) }); }
  try { outcome.command_probe = await commands.remove(); }
  catch (error) { outcome.failures.push({ owner: "desktop-command-probe", error: errorObservation(error) }); }
  state.resourceOutcome = outcome;
  if (outcome.failures.length > 0 && primaryError === null) {
    throw new DesktopE2eError(
      "harness",
      "side-chat-quote-resource-cleanup-failed",
      "Side Chat quote input and command probes did not settle",
      outcome,
    );
  }
}

export function createSideChatQuoteScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    resourceOutcome: null,
  };
  return Object.freeze({
    id: "side-chat.quote",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({
        expectedPrompt: SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAIN_PROMPT,
        script: createSideChatQuoteProviderScript(),
      });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: providerRestartFixtureConfig(state.provider.baseUrl, { supportsTools: true }),
        sentinelName: "E2E_SIDE_CHAT_QUOTE.txt",
        sentinelText: "moyAI Desktop E2E Side Chat quote fixture.\n",
      });
      await sink.record("side-chat-quote-provider-started", state.provider.resourceObservation(), {
        phase,
        owner: OWNER,
      });
    },
    async execute({ context, driver: cdp, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("Side Chat quote provider was not prepared");
      await acquireInteractiveShell({ context, driver: cdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "side-chat-quote-shell-ready",
      });
      if (provider.requestLedger.length !== 0) {
        throw productFailure(
          "side-chat-quote-cold-start-request",
          "Desktop contacted the scripted provider before trusted Main Send",
          { ledger: provider.requestLedger },
        );
      }

      const input = new WebviewInput(cdp, { probeId: "side-chat-quote" });
      const commands = new DesktopCommandProbe(cdp, {
        probeId: "side-chat-quote-commands",
        commands: [
          "save_global_config",
          "ensure_side_chat",
          "submit_side_chat",
          "cancel_side_chat",
          "submit_prompt",
          "cancel_run",
        ],
      });
      let primaryError = null;
      try {
        await input.installProbe();
        const mainTyping = await trustedInsert(
          input,
          MAIN_PROMPT,
          SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAIN_PROMPT,
        );
        const mainSend = await trustedClick(input, MAIN_SEND);
        const seed = await waitForProductStage({
          label: "canonical settled Side Chat quote source rows",
          timeoutMs: 45_000,
          sample: async () => ({
            surface: await observeSideChatQuoteSurface(cdp),
            ledger: provider.requestLedger,
          }),
          accept: (sample) => exactResponseRoles(sample?.ledger, [
            "side_quote_main_initial",
            "side_quote_main_continuation",
          ])
            && mainSeedSettled(sample?.surface?.projection)
            && surfaceHasNoErrors(sample?.surface),
          code: "side-chat-quote-main-seed",
          message: "trusted Main Send did not create canonical settled assistant and file-change quote sources",
        });
        const sources = canonicalSourceRows(seed.value.surface.projection);
        const owner = selectedNavigationIdentity(seed.value.surface.projection);
        if (owner.session_id === null) {
          throw productFailure(
            "side-chat-quote-main-owner",
            "the canonical quote source did not belong to a selected main session",
            { owner, projection: seed.value.surface.projection.draft_target },
          );
        }

        await commands.install();
        const configuration = await saveGlobalSideChatAndOpen({
          cdp,
          input,
          providerBaseUrl: provider.baseUrl,
          ownerSessionId: owner.session_id,
        });
        const configurationCommands = assertExactDesktopCommandSequence(
          await commands.snapshot(),
          { expected: configuration.expected_commands },
        );
        if (!exactResponseRoles(provider.requestLedger, [
          "side_quote_main_initial",
          "side_quote_main_continuation",
        ])) {
          throw productFailure(
            "side-chat-quote-config-network",
            "saving global Side Chat defaults and materializing the binding issued an unexpected provider generation request",
            { ledger: provider.requestLedger },
          );
        }

        const mainSentinel = await trustedInsert(input, MAIN_PROMPT, MAIN_DRAFT_SENTINEL);
        const configured = await waitForProductStage({
          label: "Side Chat quote configured source actions",
          timeoutMs: 10_000,
          sample: () => observeSideChatQuoteSurface(cdp),
          accept: (surface) => {
            const actions = surface?.quote_actions ?? [];
            const assistant = actions.find((action) => action.history_identity === sources.assistant_identity);
            const artifact = actions.find((action) => action.history_identity === sources.artifact_identity);
            return surface?.main?.prompt_value === MAIN_DRAFT_SENTINEL
              && assistant?.source_kind === "transcript"
              && assistant.source_history_item_id === sources.assistant_identity
              && assistant.owner_session_id === owner.session_id
              && assistant.visible === true
              && assistant.enabled === true
              && artifact?.source_kind === "artifact"
              && artifact.source_history_item_id === sources.artifact_identity
              && artifact.owner_session_id === owner.session_id
              && artifact.enabled === true
              && surfaceHasNoErrors(surface);
          },
          code: "side-chat-quote-source-actions",
          message: "settled canonical source rows did not expose owner-bound Side Chat quote actions",
        });
        const transcriptSelection = await selectExactRowText(
          cdp,
          sources.assistant_identity,
          SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_TRANSCRIPT_SELECTION,
        );
        if (transcriptSelection.pass !== true
          || transcriptSelection.source_kind !== "transcript"
          || transcriptSelection.owner_session_id !== owner.session_id) {
          throw productFailure(
            "side-chat-quote-transcript-selection",
            "the deterministic DOM range did not bind wholly to the exact settled assistant row",
            transcriptSelection,
          );
        }
        const transcriptQuoteAction = await trustedClick(
          input,
          quoteActionLocator(sources.assistant_identity),
        );
        const pointerQuote = await waitForProductStage({
          label: "pointer Side Chat transcript quote",
          timeoutMs: 15_000,
          sample: async () => ({
            surface: await observeSideChatQuoteSurface(cdp),
            ledger: provider.requestLedger,
            commands: await commands.snapshot(configurationCommands.last_sequence),
          }),
          accept: (sample) => exactQuoteSurface(sample?.surface, {
            ownerSessionId: owner.session_id,
            selectedText: SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_TRANSCRIPT_SELECTION,
            sourceKind: "transcript",
          })
            && exactResponseRoles(sample?.ledger, [
              "side_quote_main_initial",
              "side_quote_main_continuation",
            ])
            && sample?.commands?.calls?.length === 0,
          code: "side-chat-quote-pointer-action",
          message: "trusted pointer quote did not update only the Side draft without auto-send",
        });
        const noPointerAutoSend = assertExactDesktopCommandSequence(pointerQuote.value.commands, {
          afterSequence: configurationCommands.last_sequence,
          expected: [],
        });
        const pointerScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "side-chat-pointer-quote",
          owner: OWNER,
        });

        const firstExpected = expectedSubmitCommand(
          pointerQuote.value.surface.projection,
          "transcript",
          sources.assistant_identity,
          SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_TRANSCRIPT_SELECTION,
        );
        const firstCommandStart = pointerQuote.value.commands.sequence;
        const firstSend = await trustedClick(input, SIDE_SEND);
        const firstCommandObserved = await waitForObservation({
          label: "typed transcript quote submit command",
          timeoutMs: 10_000,
          pollMs: 25,
          retrySampleErrors: false,
          sample: () => commands.snapshot(firstCommandStart),
          accept: (snapshot) => snapshot.calls.length >= 1,
        });
        const firstCommand = assertExactDesktopCommandSequence(firstCommandObserved.value, {
          afterSequence: firstCommandStart,
          expected: [firstExpected],
        });
        const firstTerminal = await waitForProductStage({
          label: "Side Chat transcript quote terminal",
          timeoutMs: 30_000,
          sample: async () => ({
            surface: await observeSideChatQuoteSurface(cdp),
            ledger: provider.requestLedger,
          }),
          accept: (sample) => {
            const side = sample?.surface?.projection?.side_chat;
            return exactResponseRoles(sample?.ledger, [
              "side_quote_main_initial",
              "side_quote_main_continuation",
              "side_quote_transcript",
            ])
              && side?.status === "completed"
              && side.can_send === true
              && side.can_cancel === false
              && side.draft_text === ""
              && sameValue(side.messages.map((message) => [message.role, message.content]), [
                ["user", submittedQuoteText(SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_TRANSCRIPT_SELECTION)],
                ["assistant", SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_FIRST_RESPONSE],
              ])
              && sample?.surface?.main?.prompt_value === MAIN_DRAFT_SENTINEL
              && sample.surface.side.pending_count === 0
              && surfaceHasNoErrors(sample.surface);
          },
          code: "side-chat-quote-first-terminal",
          message: "the typed transcript quote did not complete as one independent Side Chat turn",
        });

        const sidePromptFocus = await trustedClick(input, SIDE_PROMPT);
        const keyboardTraversal = await reverseTabToQuoteAction(
          input,
          cdp,
          sources.artifact_identity,
        );
        const artifactSelection = await selectExactRowText(
          cdp,
          sources.artifact_identity,
          SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_ARTIFACT_PATH,
          { refocusAction: true },
        );
        if (artifactSelection.pass !== true
          || artifactSelection.source_kind !== "artifact"
          || artifactSelection.owner_session_id !== owner.session_id
          || artifactSelection.action_focused !== true) {
          throw productFailure(
            "side-chat-quote-artifact-selection",
            "the deterministic DOM range did not bind wholly to the keyboard-focused file-change row",
            artifactSelection,
          );
        }
        const keyboardStart = (await input.snapshotProbe()).sequence;
        await input.pressKey("Enter");
        const keyboardProbe = assertTrustedProbeSequence(await input.snapshotProbe(keyboardStart), {
          afterSequence: keyboardStart,
          expected: [
            {
              type: "keydown",
              identity: { tag: "BUTTON", action: "quote-selection-to-side-chat" },
              key: "Enter",
              code: "Enter",
            },
          ],
        });
        const keyboardQuote = await waitForProductStage({
          label: "keyboard Side Chat artifact quote",
          timeoutMs: 15_000,
          sample: async () => ({
            surface: await observeSideChatQuoteSurface(cdp),
            ledger: provider.requestLedger,
            commands: await commands.snapshot(firstCommand.last_sequence),
          }),
          accept: (sample) => exactQuoteSurface(sample?.surface, {
            ownerSessionId: owner.session_id,
            selectedText: SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_ARTIFACT_PATH,
            sourceKind: "artifact",
          })
            && exactResponseRoles(sample?.ledger, [
              "side_quote_main_initial",
              "side_quote_main_continuation",
              "side_quote_transcript",
            ])
            && sample?.commands?.calls?.length === 0,
          code: "side-chat-quote-keyboard-action",
          message: "trusted Enter quote did not update only the Side draft without auto-send",
        });
        const noKeyboardAutoSend = assertExactDesktopCommandSequence(keyboardQuote.value.commands, {
          afterSequence: firstCommand.last_sequence,
          expected: [],
        });
        const keyboardScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "side-chat-keyboard-quote",
          owner: OWNER,
        });

        const secondExpected = expectedSubmitCommand(
          keyboardQuote.value.surface.projection,
          "artifact",
          sources.artifact_identity,
          SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_ARTIFACT_PATH,
        );
        const secondCommandStart = keyboardQuote.value.commands.sequence;
        const secondSend = await trustedClick(input, SIDE_SEND);
        const secondCommandObserved = await waitForObservation({
          label: "typed artifact quote submit command",
          timeoutMs: 10_000,
          pollMs: 25,
          retrySampleErrors: false,
          sample: () => commands.snapshot(secondCommandStart),
          accept: (snapshot) => snapshot.calls.length >= 1,
        });
        const secondCommand = assertExactDesktopCommandSequence(secondCommandObserved.value, {
          afterSequence: secondCommandStart,
          expected: [secondExpected],
        });
        const held = await waitForProductStage({
          label: "held Side Chat artifact quote request",
          timeoutMs: 30_000,
          sample: async () => ({
            surface: await observeSideChatQuoteSurface(cdp),
            ledger: provider.requestLedger,
            provider: provider.resourceObservation(),
          }),
          accept: (sample) => {
            const side = sample?.surface?.projection?.side_chat;
            const rows = responseRows(sample?.ledger);
            return exactResponseRoles(sample?.ledger, [
              "side_quote_main_initial",
              "side_quote_main_continuation",
              "side_quote_transcript",
              "side_quote_artifact_held",
            ], { finalHeld: true })
              && rows.at(-1)?.response_phase === "held"
              && side?.status === "running"
              && side.can_send === false
              && side.can_cancel === true
              && sample?.surface?.side?.stop_visible === true
              && sample.surface.side.stop_enabled === true
              && sample.surface.main.prompt_value === MAIN_DRAFT_SENTINEL
              && sample.surface.main.send_visible === true
              && sample.surface.main.send_enabled === true
              && sample.surface.projection.run_status_key === "completed"
              && sample.surface.projection.task_activity_state === "idle"
              && sample.surface.projection.can_cancel_run === false
              && sample.surface.main.stop_enabled === false
              && surfaceHasNoErrors(sample.surface);
          },
          code: "side-chat-quote-held-request",
          message: "artifact quote Send did not create one independently cancellable Side Chat request",
        });
        const heldScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "side-chat-artifact-running",
          owner: OWNER,
        });

        const cancelExpected = expectedCancelCommand(held.value.surface.projection);
        const cancelCommandStart = secondCommand.last_sequence;
        const sideStop = await trustedClick(input, SIDE_STOP);
        const cancelObserved = await waitForObservation({
          label: "exact independent Side Stop command",
          timeoutMs: 10_000,
          pollMs: 25,
          retrySampleErrors: false,
          sample: () => commands.snapshot(cancelCommandStart),
          accept: (snapshot) => snapshot.calls.length >= 1,
        });
        const cancelCommand = assertExactDesktopCommandSequence(cancelObserved.value, {
          afterSequence: cancelCommandStart,
          expected: [cancelExpected],
        });
        const terminal = await waitForProductStage({
          label: "independent Side Stop terminal",
          timeoutMs: 45_000,
          sample: async () => ({
            surface: await observeSideChatQuoteSurface(cdp),
            ledger: provider.requestLedger,
            provider: provider.resourceObservation(),
          }),
          accept: (sample) => {
            const side = sample?.surface?.projection?.side_chat;
            const rows = responseRows(sample?.ledger);
            return exactResponseRoles(sample?.ledger, [
              "side_quote_main_initial",
              "side_quote_main_continuation",
              "side_quote_transcript",
              "side_quote_artifact_held",
            ], { finalHeld: true })
              && rows.at(-1)?.response_phase === "peer_closed"
              && side?.status === "cancelled"
              && side.can_send === true
              && side.can_cancel === false
              && sample?.surface?.main?.prompt_value === MAIN_DRAFT_SENTINEL
              && sample.surface.main.send_visible === true
              && sample.surface.main.send_enabled === true
              && sample.surface.projection.run_status_key === "completed"
              && sample.surface.projection.task_activity_state === "idle"
              && sample.surface.projection.can_cancel_run === false
              && sample.surface.main.stop_enabled === false
              && surfaceHasNoErrors(sample.surface);
          },
          code: "side-chat-quote-stop-terminal",
          message: "trusted Side Stop did not cancel only the Side request and preserve the Main owner",
        });
        const finalCommands = assertExactDesktopCommandSequence(
          await commands.snapshot(),
          {
            expected: [
              ...configuration.expected_commands,
              firstExpected,
              secondExpected,
              cancelExpected,
            ],
          },
        );
        const finalScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "side-chat-artifact-stopped",
          owner: OWNER,
        });

        state.acceptedLedger = structuredClone(terminal.value.ledger);
        await sink.record("side-chat-quote-completed", {
          input_kind: "browser_trusted",
          selection_precondition: "deterministic DOM Range wholly within one canonical row",
          main: {
            typing: mainTyping,
            send: mainSend,
            draft_sentinel: mainSentinel,
            owner,
            sources: {
              assistant_history_item_id: sources.assistant_identity,
              artifact_history_item_id: sources.artifact_identity,
              artifact_path: SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_ARTIFACT_PATH,
            },
          },
          configuration,
          configuration_command_evidence: configurationCommands,
          pointer_quote: {
            configured_surface: configured.value,
            selection: transcriptSelection,
            action: transcriptQuoteAction,
            no_auto_send: noPointerAutoSend,
            submit_action: firstSend,
            submit_command: firstCommand,
            terminal: firstTerminal.value.surface.projection.side_chat,
            screenshot: pointerScreenshot,
          },
          keyboard_quote: {
            side_prompt_focus: sidePromptFocus,
            traversal: keyboardTraversal,
            selection: artifactSelection,
            activation_probe: keyboardProbe,
            no_auto_send: noKeyboardAutoSend,
            submit_action: secondSend,
            submit_command: secondCommand,
            screenshot: keyboardScreenshot,
          },
          independent_stop: {
            held_projection: held.value.surface.projection.side_chat,
            held_provider: held.value.provider,
            stop_action: sideStop,
            stop_command: cancelCommand,
            terminal_projection: terminal.value.surface.projection.side_chat,
            final_commands: finalCommands,
            held_screenshot: heldScreenshot,
            terminal_screenshot: finalScreenshot,
          },
          provider_ledger: state.acceptedLedger,
          provider_resource: terminal.value.provider,
        }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        await settleResources(state, input, commands, primaryError);
      }
    },
    async quiesce({ inputs }) {
      if (state.quiesceOutcome !== null) return structuredClone(state.quiesceOutcome);
      state.quiesceOutcome = await quiesceProviderResource({
        provider: state.provider,
        acceptedLedger: state.acceptedLedger,
        inputs,
      });
      return structuredClone(state.quiesceOutcome);
    },
    async cleanup() {
      const quiesced = state.quiesceOutcome !== null;
      const pass = quiesced
        && state.quiesceOutcome.input === "pass"
        && state.resourceOutcome !== null
        && state.resourceOutcome.failures.length === 0;
      return {
        input: pass ? "pass" : "fail",
        resources: [{
          kind: "side-chat-quote-verification",
          quiesced,
          quiesce_input: state.quiesceOutcome?.input ?? null,
          resource_outcome: state.resourceOutcome,
        }],
      };
    },
  });
}
