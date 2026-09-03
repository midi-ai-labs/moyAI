import type {
  SideChatPendingQuote,
  SideChatQuoteSourceKind,
} from "./types.ts";

export interface SideChatQuoteSourceDescriptor {
  sourceKind: SideChatQuoteSourceKind;
  sourceHistoryItemId: string;
}

export interface SideChatQuoteSelectionSample {
  activatedSource: SideChatQuoteSourceDescriptor | null;
  startSource: SideChatQuoteSourceDescriptor | null;
  endSource: SideChatQuoteSourceDescriptor | null;
  selectedText: string;
  sourceAppendPosition: string | null;
}

export function pendingSideChatQuoteFromSelection(
  sample: SideChatQuoteSelectionSample,
): SideChatPendingQuote | null {
  const selectedText = normalizedSelectedText(sample.selectedText);
  if (
    selectedText.length === 0
    || sample.sourceAppendPosition === null
    || sample.activatedSource === null
    || sample.startSource === null
    || sample.endSource === null
    || !sameQuoteSource(sample.activatedSource, sample.startSource)
    || !sameQuoteSource(sample.activatedSource, sample.endSource)
  ) return null;

  return {
    ...sample.activatedSource,
    sourceAppendPosition: sample.sourceAppendPosition,
    selectedText,
  };
}

export function pendingSideChatQuoteFromDomSelection(
  trigger: Element,
  selection: Selection | null,
  sourceAppendPosition: string | null,
): SideChatPendingQuote | null {
  if (!selection || selection.isCollapsed || selection.rangeCount !== 1) return null;
  const range = selection.getRangeAt(0);
  const activatedRow = eligibleQuoteRow(trigger);
  const startRow = eligibleQuoteRow(selectionElement(range.startContainer));
  const endRow = eligibleQuoteRow(selectionElement(range.endContainer));
  if (!activatedRow || activatedRow !== startRow || activatedRow !== endRow) return null;

  return pendingSideChatQuoteFromSelection({
    activatedSource: quoteSourceDescriptor(activatedRow),
    startSource: quoteSourceDescriptor(startRow),
    endSource: quoteSourceDescriptor(endRow),
    selectedText: selection.toString(),
    sourceAppendPosition,
  });
}

export function sideChatQuoteOwnerSessionIdFromTrigger(trigger: Element): string | null {
  return eligibleQuoteRow(trigger)?.dataset.sideChatQuoteOwnerSessionId?.trim() || null;
}

export function appendReadableSideChatQuote(
  currentDraft: string,
  selectedText: string,
): string {
  const body = normalizedSelectedText(selectedText)
    .split("\n")
    .map((line) => `> ${line}`)
    .join("\n");
  const quoteBlock = `> Side Chat 引用\n${body}`;
  const preceding = currentDraft.trimEnd();
  return preceding.length > 0
    ? `${preceding}\n\n${quoteBlock}\n\n`
    : `${quoteBlock}\n\n`;
}

export function replaceReadableSideChatQuote(
  currentDraft: string,
  previousSelectedText: string | null,
  selectedText: string,
): string {
  const previousBlock = previousSelectedText === null
    ? null
    : appendReadableSideChatQuote("", previousSelectedText);
  let base = currentDraft;
  if (previousBlock !== null) {
    if (base === previousBlock) {
      base = "";
    } else {
      const separatedBlock = `\n\n${previousBlock}`;
      if (base.endsWith(separatedBlock)) {
        base = base.slice(0, -separatedBlock.length);
      }
    }
  }
  return appendReadableSideChatQuote(base, selectedText);
}

export function sideChatQuoteKeyboardActivation(key: string, repeat: boolean): boolean {
  return !repeat && (key === "Enter" || key === " ");
}

function eligibleQuoteRow(element: Element | null): HTMLElement | null {
  return element?.closest<HTMLElement>(
    "[data-side-chat-quote-source-kind][data-history-identity]",
  ) ?? null;
}

function selectionElement(node: Node | null): Element | null {
  if (!node) return null;
  return node.nodeType === Node.ELEMENT_NODE
    ? node as Element
    : node.parentElement;
}

function quoteSourceDescriptor(row: HTMLElement): SideChatQuoteSourceDescriptor | null {
  const sourceKind = row.dataset.sideChatQuoteSourceKind;
  const sourceHistoryItemId = row.dataset.historyIdentity?.trim() ?? "";
  if (
    (sourceKind !== "transcript" && sourceKind !== "artifact")
    || sourceHistoryItemId.length === 0
  ) return null;
  return { sourceKind, sourceHistoryItemId };
}

function sameQuoteSource(
  left: SideChatQuoteSourceDescriptor,
  right: SideChatQuoteSourceDescriptor,
): boolean {
  return left.sourceKind === right.sourceKind
    && left.sourceHistoryItemId === right.sourceHistoryItemId;
}

function normalizedSelectedText(value: string): string {
  return value.replace(/\r\n?/g, "\n").trim();
}
