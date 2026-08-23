const BOUNDARY_TAGS = new Set(["BODY", "HTML"]);

function normalizedTag(value) {
  return typeof value === "string" ? value.trim().toUpperCase() : "";
}
function exactIdentity(value) {
  return {
    tag: normalizedTag(value?.tag),
    id: value?.id ?? null,
    action: value?.action ?? null,
    focusKey: value?.focusKey ?? null,
    configKey: value?.configKey ?? null,
    sideSetting: value?.sideSetting ?? null,
  };
}

function identityMatches(left, right) {
  return JSON.stringify(exactIdentity(left)) === JSON.stringify(exactIdentity(right));
}

export function isDocumentBoundary(value) {
  return BOUNDARY_TAGS.has(normalizedTag(value?.tag));
}

export function isInDocument(value) {
  const tag = normalizedTag(value?.tag);
  return tag.length > 0 && !BOUNDARY_TAGS.has(tag);
}

function freshEvents(observation) {
  if (!Number.isInteger(observation?.beforeSequence) || observation.beforeSequence < 0) return [];
  return (Array.isArray(observation?.events) ? observation.events : []).filter(
    (event) => Number.isInteger(event?.sequence) && event.sequence > observation.beforeSequence,
  );
}

function validateTrustedTab(observation) {
  const events = freshEvents(observation);
  const tabDown = events.filter((event) => event?.type === "keydown" && event?.key === "Tab");
  const tabUp = events.filter((event) => event?.type === "keyup" && event?.key === "Tab");
  const reasons = [];
  if (!Number.isInteger(observation?.beforeSequence) || observation.beforeSequence < 0) reasons.push("before-sequence-invalid");
  if (observation?.dispatchError != null) reasons.push("tab-dispatch-error");
  if (tabDown.length !== 1 || tabUp.length !== 1) reasons.push("tab-key-pair-not-exact");
  if ([...tabDown, ...tabUp].some((event) => event?.isTrusted !== true)) reasons.push("tab-event-not-trusted");
  if (tabDown[0] && tabUp[0] && tabUp[0].sequence <= tabDown[0].sequence) reasons.push("tab-event-order-invalid");
  if (tabDown[0] && !identityMatches(tabDown[0], observation?.beforeActive)) reasons.push("keydown-source-identity-mismatch");
  if (tabUp[0] && !identityMatches(tabUp[0], observation?.afterActive)) reasons.push("keyup-destination-identity-mismatch");
  return { events, tabDown: tabDown[0] ?? null, tabUp: tabUp[0] ?? null, reasons };
}

function harnessNg(reasons, pendingBoundary = null) {
  return {
    classification: "harness_ng",
    transition: null,
    pending_boundary: pendingBoundary === null ? null : structuredClone(pendingBoundary),
    reasons,
  };
}

export class TabFocusNavigator {
  #pendingBoundary = null;

  get pendingBoundary() {
    return this.#pendingBoundary === null ? null : structuredClone(this.#pendingBoundary);
  }

  observe(observation) {
    const key = validateTrustedTab(observation);
    if (key.reasons.length > 0) return harnessNg(key.reasons, this.#pendingBoundary);
    const focusins = key.events.filter((event) => event?.type === "focusin");
    if (focusins.some((event) => event?.isTrusted !== true)) {
      return harnessNg(["focusin-event-not-trusted"], this.#pendingBoundary);
    }

    if (this.#pendingBoundary !== null) {
      const reasons = [];
      if (!isDocumentBoundary(observation?.beforeActive)) reasons.push("boundary-owner-source-mismatch");
      if (!identityMatches(observation?.beforeActive, this.#pendingBoundary.active)) reasons.push("boundary-owner-identity-drift");
      if (observation.beforeSequence !== this.#pendingBoundary.sequence) reasons.push("boundary-next-tab-not-contiguous");
      if (!isInDocument(observation?.afterActive)) reasons.push("boundary-reentry-destination-missing");
      if (focusins.length !== 1) reasons.push("boundary-reentry-focusin-not-exact");
      if (focusins[0] && !identityMatches(focusins[0], observation?.afterActive)) reasons.push("boundary-reentry-focusin-identity-mismatch");
      if (reasons.length > 0) return harnessNg(reasons, this.#pendingBoundary);
      const consumed = this.#pendingBoundary;
      this.#pendingBoundary = null;
      return {
        classification: "acquired",
        transition: "document_reentry",
        pending_boundary: null,
        consumed_boundary: structuredClone(consumed),
        reasons: [],
      };
    }

    if (!isInDocument(observation?.beforeActive)) {
      return harnessNg(["unowned-document-boundary-source"]);
    }

    if (isDocumentBoundary(observation?.afterActive)) {
      if (focusins.length !== 0) return harnessNg(["document-boundary-focusin-contradiction"]);
      this.#pendingBoundary = {
        sequence: key.tabUp.sequence,
        active: exactIdentity(observation.afterActive),
        source: exactIdentity(observation.beforeActive),
      };
      return {
        classification: "acquired",
        transition: "document_boundary_pending",
        pending_boundary: this.pendingBoundary,
        reasons: [],
      };
    }

    if (!isInDocument(observation?.afterActive)) return harnessNg(["tab-destination-unobservable"]);
    if (focusins.length !== 1) return harnessNg(["in-document-focusin-not-exact"]);
    if (!identityMatches(focusins[0], observation.afterActive)) return harnessNg(["in-document-focusin-identity-mismatch"]);
    return {
      classification: "acquired",
      transition: "in_document",
      pending_boundary: null,
      reasons: [],
    };
  }
}
