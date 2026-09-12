const SEMANTIC_IDENTITY_FIELDS = Object.freeze([
  "tag",
  "id",
  "action",
  "focusKey",
  "configKey",
  "sideSetting",
  "sessionSetting",
  "sessionSettingsTrigger",
  "surface",
  "modal",
  "step",
  "field",
  "detailsKey",
  "href",
]);
const STABLE_IDENTITY_FIELDS = Object.freeze(SEMANTIC_IDENTITY_FIELDS.filter((field) => field !== "tag"));
const PROBE_ID = /^[a-z0-9][a-z0-9._-]{2,127}$/;
const DEFAULT_TARGET_ACQUISITION_TIMEOUT_MS = 1_500;
const DEFAULT_TARGET_ACQUISITION_POLL_MS = 16;
const EXISTING_SCROLL_STABLE_HIT_SAMPLES = 3;

const MODIFIER_BITS = Object.freeze({
  Alt: 1,
  Control: 2,
  Meta: 4,
  Shift: 8,
});

const NAMED_KEYS = Object.freeze({
  Alt: { key: "Alt", code: "AltLeft", virtualKey: 18, modifierBit: MODIFIER_BITS.Alt },
  ArrowDown: { key: "ArrowDown", code: "ArrowDown", virtualKey: 40 },
  ArrowLeft: { key: "ArrowLeft", code: "ArrowLeft", virtualKey: 37 },
  ArrowRight: { key: "ArrowRight", code: "ArrowRight", virtualKey: 39 },
  Backspace: { key: "Backspace", code: "Backspace", virtualKey: 8 },
  Control: { key: "Control", code: "ControlLeft", virtualKey: 17, modifierBit: MODIFIER_BITS.Control },
  Enter: { key: "Enter", code: "Enter", virtualKey: 13, text: "\r" },
  End: { key: "End", code: "End", virtualKey: 35 },
  Escape: { key: "Escape", code: "Escape", virtualKey: 27 },
  F8: { key: "F8", code: "F8", virtualKey: 119 },
  F9: { key: "F9", code: "F9", virtualKey: 120 },
  Home: { key: "Home", code: "Home", virtualKey: 36 },
  Meta: { key: "Meta", code: "MetaLeft", virtualKey: 91, modifierBit: MODIFIER_BITS.Meta },
  Shift: { key: "Shift", code: "ShiftLeft", virtualKey: 16, modifierBit: MODIFIER_BITS.Shift },
  Tab: { key: "Tab", code: "Tab", virtualKey: 9 },
});

const PAGE_IDENTITY_SOURCE = `
  const semanticIdentity = (value) => {
    if (!(value instanceof Element)) {
      return { tag: "", id: null, action: null, focusKey: null, configKey: null, sideSetting: null, sessionSetting: null, sessionSettingsTrigger: null, surface: null, modal: null, step: null, field: null, detailsKey: null, href: null };
    }
    const owner = value.closest(
      '[data-action], [data-focus-key], [data-config-key], [data-side-chat-setting], [data-session-setting], [data-session-settings-trigger], [data-surface], [data-modal], [data-step], [data-field], [data-details-key], button, input, textarea, select, a[href], [tabindex], [role]'
    ) ?? value;
    return {
      tag: owner.tagName.toUpperCase(),
      id: owner.id || null,
      action: owner instanceof HTMLElement ? (owner.dataset.action ?? null) : null,
      focusKey: owner instanceof HTMLElement ? (owner.dataset.focusKey ?? null) : null,
      configKey: owner instanceof HTMLElement ? (owner.dataset.configKey ?? null) : null,
      sideSetting: owner instanceof HTMLElement ? (owner.dataset.sideChatSetting ?? null) : null,
      sessionSetting: owner instanceof HTMLElement ? (owner.dataset.sessionSetting ?? null) : null,
      sessionSettingsTrigger: owner instanceof HTMLElement ? (owner.dataset.sessionSettingsTrigger ?? null) : null,
      surface: owner instanceof HTMLElement ? (owner.dataset.surface ?? null) : null,
      modal: owner instanceof HTMLElement ? (owner.dataset.modal ?? null) : null,
      step: owner instanceof HTMLElement ? (owner.dataset.step ?? null) : null,
      field: owner instanceof HTMLElement ? (owner.dataset.field ?? null) : null,
      detailsKey: owner instanceof HTMLElement ? (owner.dataset.detailsKey ?? null) : null,
      href: owner instanceof HTMLAnchorElement ? owner.getAttribute('href') : null,
    };
  };
`;

function invariant(condition, message) {
  if (!condition) throw new TypeError(message);
}

function clone(value) {
  return value === undefined ? undefined : structuredClone(value);
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

function finiteNumber(value) {
  return typeof value === "number" && Number.isFinite(value);
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function nullableIdentityValue(value, field) {
  if (value === undefined) return undefined;
  if (value === null) return null;
  invariant(typeof value === "string", `semantic identity ${field} must be a string or null`);
  const normalized = field === "tag" ? value.trim().toUpperCase() : value;
  if (field !== "tag" && normalized.length === 0) return null;
  invariant(normalized.length > 0, `semantic identity ${field} must not be empty`);
  return normalized;
}

export class WebviewInputError extends Error {
  constructor(code, message, evidence = null) {
    super(message);
    invariant(typeof code === "string" && /^[a-z0-9][a-z0-9._-]+$/.test(code), "invalid WebView input error code");
    this.name = "WebviewInputError";
    this.code = code;
    this.evidence = evidence === null ? null : clone(evidence);
  }
}

export function normalizeSemanticIdentity(value, { partial = false } = {}) {
  invariant(value !== null && typeof value === "object" && !Array.isArray(value), "semantic identity must be an object");
  const identity = {};
  for (const field of SEMANTIC_IDENTITY_FIELDS) {
    const normalized = nullableIdentityValue(value[field], field);
    if (normalized !== undefined || !partial) identity[field] = normalized ?? null;
  }
  return identity;
}

function identityMatches(actual, expected) {
  const normalizedActual = normalizeSemanticIdentity(actual);
  const normalizedExpected = normalizeSemanticIdentity(expected, { partial: true });
  return Object.entries(normalizedExpected).every(([field, value]) => normalizedActual[field] === value);
}

export function normalizeSemanticLocator(value) {
  invariant(value !== null && typeof value === "object" && !Array.isArray(value), "semantic locator must be an object");
  invariant(typeof value.selector === "string", "semantic locator selector must be a string");
  const selector = value.selector.trim();
  invariant(selector.length > 0 && selector.length <= 512 && !selector.includes("\0"), "semantic locator selector is invalid");
  const identity = normalizeSemanticIdentity(value.identity, { partial: true });
  invariant(
    STABLE_IDENTITY_FIELDS.some((field) => Object.hasOwn(identity, field) && identity[field] !== null),
    "semantic locator requires id, action, focusKey, configKey, sideSetting, sessionSetting, sessionSettingsTrigger, surface, modal, step, field, detailsKey, or href identity",
  );
  invariant(value.requireVisible === undefined || typeof value.requireVisible === "boolean", "requireVisible must be boolean");
  invariant(value.requireEnabled === undefined || typeof value.requireEnabled === "boolean", "requireEnabled must be boolean");
  return {
    selector,
    identity,
    requireVisible: value.requireVisible ?? true,
    requireEnabled: value.requireEnabled ?? true,
  };
}

function targetFailure(code, message, observation, locator) {
  throw new WebviewInputError(code, message, { locator, observation });
}

export function assertExactSemanticTarget(observation, locatorValue) {
  const locator = normalizeSemanticLocator(locatorValue);
  if (observation === null || typeof observation !== "object" || Array.isArray(observation)) {
    targetFailure("semantic-target-observation-invalid", "semantic target observation is invalid", observation, locator);
  }
  if (observation.count !== 1) {
    targetFailure("semantic-target-cardinality", `expected exactly one semantic target, found ${observation.count}`, observation, locator);
  }
  if (observation.connected !== true) {
    targetFailure("semantic-target-detached", "semantic target is not connected", observation, locator);
  }
  if (locator.requireVisible && observation.visible !== true) {
    targetFailure("semantic-target-hidden", "semantic target is not visibly actionable", observation, locator);
  }
  if (locator.requireEnabled && observation.enabled !== true) {
    targetFailure("semantic-target-disabled", "semantic target is disabled or inert", observation, locator);
  }
  if (!identityMatches(observation.identity, locator.identity)) {
    targetFailure("semantic-target-identity", "semantic target identity does not match the locator", observation, locator);
  }
  if (
    observation.center_hit !== true
    || !finiteNumber(observation?.center?.x)
    || !finiteNumber(observation?.center?.y)
  ) {
    targetFailure("semantic-target-hit-test", "semantic target center is not its browser hit-test owner", observation, locator);
  }
  if (!identityMatches(observation.hit_identity, normalizeSemanticIdentity(observation.identity))) {
    targetFailure("semantic-target-hit-identity", "semantic target center resolves to a different semantic owner", observation, locator);
  }
  return {
    locator,
    identity: normalizeSemanticIdentity(observation.identity),
    hit_identity: normalizeSemanticIdentity(observation.hit_identity),
    center: { x: observation.center.x, y: observation.center.y },
    rect: clone(observation.rect),
  };
}

function targetCanSettleThroughExistingScroll(error) {
  return error instanceof WebviewInputError
    && error.code === "semantic-target-hit-test"
    && (
      error.evidence?.observation?.center_in_viewport === false
      || error.evidence?.observation?.center_in_scroll_clip === false
    );
}

function sameTargetGeometry(left, right) {
  return left !== null
    && right !== null
    && left.center.x === right.center.x
    && left.center.y === right.center.y
    && left.rect?.left === right.rect?.left
    && left.rect?.top === right.rect?.top
    && left.rect?.right === right.rect?.right
    && left.rect?.bottom === right.rect?.bottom
    && left.rect?.width === right.rect?.width
    && left.rect?.height === right.rect?.height;
}

function exactTargetExpression(locator) {
  return `(() => {
    ${PAGE_IDENTITY_SOURCE}
    const locator = ${JSON.stringify(locator)};
    const nodes = Array.from(document.querySelectorAll(locator.selector));
    if (nodes.length !== 1) return { count: nodes.length };
    const target = nodes[0];
    const rect = target.getBoundingClientRect();
    const style = target instanceof HTMLElement ? getComputedStyle(target) : null;
    const visible = target instanceof HTMLElement
      && target.isConnected
      && style.display !== 'none'
      && style.visibility !== 'hidden'
      && Number(style.opacity) !== 0
      && rect.width > 0
      && rect.height > 0
      && target.closest('[hidden], [inert], [aria-hidden="true"]') === null;
    const enabled = target instanceof HTMLElement
      && !target.matches(':disabled')
      && target.getAttribute('aria-disabled') !== 'true'
      && target.closest('[inert]') === null
      && style?.pointerEvents !== 'none';
    const center = { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
    const viewport = {
      width: document.documentElement.clientWidth,
      height: document.documentElement.clientHeight,
    };
    const centerInViewport = center.x >= 0 && center.x < viewport.width
      && center.y >= 0 && center.y < viewport.height;
    const scrollClip = { left: 0, top: 0, right: viewport.width, bottom: viewport.height };
    const clippingValues = new Set(['auto', 'scroll', 'hidden', 'clip']);
    for (let ancestor = target.parentElement; ancestor; ancestor = ancestor.parentElement) {
      const ancestorStyle = getComputedStyle(ancestor);
      const ancestorRect = ancestor.getBoundingClientRect();
      const clientLeft = ancestorRect.left + ancestor.clientLeft;
      const clientTop = ancestorRect.top + ancestor.clientTop;
      const clientRight = clientLeft + ancestor.clientWidth;
      const clientBottom = clientTop + ancestor.clientHeight;
      if (clippingValues.has(ancestorStyle.overflowX)) {
        scrollClip.left = Math.max(scrollClip.left, clientLeft);
        scrollClip.right = Math.min(scrollClip.right, clientRight);
      }
      if (clippingValues.has(ancestorStyle.overflowY)) {
        scrollClip.top = Math.max(scrollClip.top, clientTop);
        scrollClip.bottom = Math.min(scrollClip.bottom, clientBottom);
      }
    }
    const centerInScrollClip = center.x >= scrollClip.left && center.x < scrollClip.right
      && center.y >= scrollClip.top && center.y < scrollClip.bottom;
    const hit = visible && centerInViewport && centerInScrollClip
      ? document.elementFromPoint(center.x, center.y)
      : null;
    return {
      count: 1,
      connected: target.isConnected,
      visible,
      enabled,
      identity: semanticIdentity(target),
      hit_identity: semanticIdentity(hit),
      center_hit: hit instanceof Element && (hit === target || target.contains(hit)),
      center_in_viewport: centerInViewport,
      center_in_scroll_clip: centerInScrollClip,
      center,
      viewport,
      scroll_clip: scrollClip,
      rect: {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
        width: rect.width,
        height: rect.height,
      },
    };
  })()`;
}

function normalizeProbeId(probeId) {
  invariant(typeof probeId === "string" && PROBE_ID.test(probeId), "invalid WebView event probe id");
  return probeId;
}

function installProbeExpression(probeId, maxEvents) {
  return `(() => {
    ${PAGE_IDENTITY_SOURCE}
    const probeId = ${JSON.stringify(probeId)};
    const registryKey = Symbol.for('moyai.desktop_e2e.webview_input.probes.v1');
    let registry = globalThis[registryKey];
    if (!(registry instanceof Map)) {
      registry = new Map();
      Object.defineProperty(globalThis, registryKey, { value: registry, configurable: true });
    }
    if (registry.has(probeId)) return { installed: false, reason: 'probe-already-installed' };
    const controller = new AbortController();
    const state = { sequence: 0, droppedThrough: 0, events: [], controller };
    const record = (event) => {
      const identity = semanticIdentity(event.target);
      state.sequence += 1;
      state.events.push({
        sequence: state.sequence,
        type: event.type,
        isTrusted: event.isTrusted,
        timeStamp: event.timeStamp,
        ...identity,
        active: semanticIdentity(document.activeElement),
        key: event instanceof KeyboardEvent ? event.key : null,
        code: event instanceof KeyboardEvent ? event.code : null,
        repeat: event instanceof KeyboardEvent ? event.repeat : null,
        isComposing: event instanceof KeyboardEvent || event instanceof InputEvent ? event.isComposing : null,
        ctrlKey: event instanceof KeyboardEvent || event instanceof MouseEvent ? event.ctrlKey : null,
        shiftKey: event instanceof KeyboardEvent || event instanceof MouseEvent ? event.shiftKey : null,
        altKey: event instanceof KeyboardEvent || event instanceof MouseEvent ? event.altKey : null,
        metaKey: event instanceof KeyboardEvent || event instanceof MouseEvent ? event.metaKey : null,
        pointerId: event instanceof PointerEvent ? event.pointerId : null,
        button: event instanceof MouseEvent ? event.button : null,
        buttons: event instanceof MouseEvent ? event.buttons : null,
        detail: event instanceof MouseEvent ? event.detail : null,
        inputType: event instanceof InputEvent ? event.inputType : null,
        data: event instanceof InputEvent ? event.data : null,
      });
      if (state.events.length > ${maxEvents}) {
        const removed = state.events.splice(0, state.events.length - ${maxEvents});
        state.droppedThrough = removed.at(-1)?.sequence ?? state.droppedThrough;
      }
    };
    for (const type of ['pointermove', 'pointerdown', 'pointerup', 'click', 'keydown', 'keyup', 'input', 'change', 'focusin']) {
      document.addEventListener(type, record, { capture: true, signal: controller.signal });
    }
    registry.set(probeId, state);
    return { installed: true, probe_id: probeId, sequence: 0 };
  })()`;
}

function snapshotProbeExpression(probeId, afterSequence) {
  return `(() => {
    ${PAGE_IDENTITY_SOURCE}
    const probeId = ${JSON.stringify(probeId)};
    const registry = globalThis[Symbol.for('moyai.desktop_e2e.webview_input.probes.v1')];
    const state = registry instanceof Map ? registry.get(probeId) : null;
    if (!state) return { found: false, probe_id: probeId };
    return {
      found: true,
      probe_id: probeId,
      sequence: state.sequence,
      dropped_through: state.droppedThrough,
      active: semanticIdentity(document.activeElement),
      events: state.events.filter((event) => event.sequence > ${afterSequence}),
    };
  })()`;
}

function removeProbeExpression(probeId) {
  return `(() => {
    const probeId = ${JSON.stringify(probeId)};
    const registryKey = Symbol.for('moyai.desktop_e2e.webview_input.probes.v1');
    const registry = globalThis[registryKey];
    const state = registry instanceof Map ? registry.get(probeId) : null;
    if (!state) return { removed: false, probe_id: probeId };
    state.controller.abort();
    registry.delete(probeId);
    if (registry.size === 0) delete globalThis[registryKey];
    return { removed: true, probe_id: probeId, sequence: state.sequence };
  })()`;
}

function activeSemanticIdentityExpression() {
  return `(() => {
    ${PAGE_IDENTITY_SOURCE}
    return semanticIdentity(document.activeElement);
  })()`;
}

function probeFailure(code, message, evidence) {
  throw new WebviewInputError(code, message, evidence);
}

export function assertTrustedProbeSequence(snapshot, { afterSequence, expected }) {
  invariant(Number.isInteger(afterSequence) && afterSequence >= 0, "probe afterSequence must be a non-negative integer");
  invariant(Array.isArray(expected) && expected.length > 0, "expected probe sequence must be non-empty");
  if (snapshot?.found !== true || !Number.isInteger(snapshot.sequence) || !Array.isArray(snapshot.events)) {
    probeFailure("event-probe-snapshot-invalid", "WebView event probe snapshot is invalid", snapshot);
  }
  if (!Number.isInteger(snapshot.dropped_through) || snapshot.dropped_through > afterSequence) {
    probeFailure("event-probe-overflow", "required WebView events were dropped before observation", { afterSequence, snapshot });
  }
  let previousSequence = afterSequence;
  for (const event of snapshot.events) {
    if (!Number.isInteger(event?.sequence) || event.sequence <= previousSequence || event.sequence > snapshot.sequence) {
      probeFailure("event-probe-order", "WebView event probe sequence is not strictly ordered", { afterSequence, snapshot });
    }
    previousSequence = event.sequence;
  }
  const expectedTypes = new Set(expected.map((event) => event?.type));
  invariant(!expectedTypes.has(undefined), "every expected probe event requires a type");
  const observed = snapshot.events.filter((event) => expectedTypes.has(event.type));
  if (observed.length !== expected.length) {
    probeFailure("event-probe-cardinality", "WebView event probe did not observe the exact event sequence", { afterSequence, expected, observed });
  }
  for (let index = 0; index < expected.length; index += 1) {
    const wanted = expected[index];
    const actual = observed[index];
    if (actual.type !== wanted.type) {
      probeFailure("event-probe-type", "WebView event types were observed out of order", { index, wanted, actual });
    }
    if (actual.isTrusted !== true) {
      probeFailure("event-probe-untrusted", "WebView input event was not browser-trusted", { index, wanted, actual });
    }
    if (wanted.identity && !identityMatches(actual, wanted.identity)) {
      probeFailure("event-probe-target", "WebView input event target identity drifted", { index, wanted, actual });
    }
    for (const field of ["key", "code", "button", "buttons", "pointerId", "inputType", "data"]) {
      if (Object.hasOwn(wanted, field) && actual[field] !== wanted[field]) {
        probeFailure("event-probe-detail", `WebView input event ${field} did not match`, { index, field, wanted, actual });
      }
    }
  }
  return {
    after_sequence: afterSequence,
    last_sequence: observed.at(-1).sequence,
    events: clone(observed),
  };
}

export function assertTrustedTextInsertion(snapshot, { afterSequence, identity, text }) {
  invariant(identity !== null && typeof identity === "object" && !Array.isArray(identity), "text insertion identity is required");
  invariant(typeof text === "string" && text.length > 0, "inserted probe text must be non-empty");
  const inputEvents = Array.isArray(snapshot?.events)
    ? snapshot.events.filter((event) => event?.type === "input")
    : [];
  if (inputEvents.length === 0) {
    probeFailure("event-probe-cardinality", "WebView text insertion produced no trusted input event", {
      afterSequence,
      identity,
      text,
      snapshot,
    });
  }
  const acquired = assertTrustedProbeSequence(snapshot, {
    afterSequence,
    expected: inputEvents.map(() => ({ type: "input", identity, inputType: "insertText" })),
  });
  const reconstructed = acquired.events.map((event, index) => {
    if (event.data === null) return "\n";
    if (typeof event.data === "string") return event.data;
    probeFailure("event-probe-detail", "WebView text insertion event data was neither text nor a newline boundary", {
      index,
      event,
    });
  }).join("");
  if (reconstructed !== text) {
    probeFailure("event-probe-text", "WebView trusted input events did not reconstruct the exact inserted text", {
      expected: text,
      reconstructed,
      events: acquired.events,
    });
  }
  return {
    ...acquired,
    reconstructed_text: reconstructed,
    segment_count: acquired.events.length,
  };
}

function printableKey(character) {
  if (/^[a-z]$/.test(character)) {
    return { key: character, code: `Key${character.toUpperCase()}`, virtualKey: character.toUpperCase().charCodeAt(0), text: character };
  }
  if (/^[0-9]$/.test(character)) {
    return { key: character, code: `Digit${character}`, virtualKey: character.charCodeAt(0), text: character };
  }
  if (character === " ") return { key: " ", code: "Space", virtualKey: 32, text: " " };
  if (character === "-") return { key: "-", code: "Minus", virtualKey: 189, text: "-" };
  return null;
}

export function normalizeWebviewKey(value) {
  invariant(typeof value === "string" && value.length > 0, "WebView key must be a non-empty string");
  const named = NAMED_KEYS[value];
  const key = named ?? (Array.from(value).length === 1 ? printableKey(value) : null);
  invariant(key !== null && key !== undefined, `unsupported WebView key: ${value}`);
  return { ...key, modifierBit: key.modifierBit ?? 0 };
}

function validateCdp(cdp) {
  invariant(cdp !== null && typeof cdp === "object", "CDP client is required");
  invariant(typeof cdp.call === "function", "CDP client must expose call");
  invariant(typeof cdp.evaluate === "function", "CDP client must expose evaluate");
  return cdp;
}

function aggregateInputError(message, failures) {
  const error = new AggregateError(failures.map((failure) => failure.error), message);
  error.evidence = failures.map(({ owner, error: failure }) => ({ owner, message: errorMessage(failure) }));
  return error;
}

export class WebviewInput {
  #cdp;
  #pressedKeys = new Map();
  #pressedPointer = null;
  #probeInstalled = false;

  constructor(
    cdp,
    {
      probeId = "webview-input",
      maxProbeEvents = 4096,
      targetAcquisitionTimeoutMs = DEFAULT_TARGET_ACQUISITION_TIMEOUT_MS,
      targetAcquisitionPollMs = DEFAULT_TARGET_ACQUISITION_POLL_MS,
      now = Date.now,
      wait = delay,
    } = {},
  ) {
    this.#cdp = validateCdp(cdp);
    this.probeId = normalizeProbeId(probeId);
    invariant(Number.isInteger(maxProbeEvents) && maxProbeEvents >= 32 && maxProbeEvents <= 65_536, "maxProbeEvents is invalid");
    invariant(
      Number.isInteger(targetAcquisitionTimeoutMs)
        && targetAcquisitionTimeoutMs >= 1
        && targetAcquisitionTimeoutMs <= 10_000,
      "targetAcquisitionTimeoutMs is invalid",
    );
    invariant(
      Number.isInteger(targetAcquisitionPollMs)
        && targetAcquisitionPollMs >= 0
        && targetAcquisitionPollMs <= targetAcquisitionTimeoutMs,
      "targetAcquisitionPollMs is invalid",
    );
    invariant(typeof now === "function", "WebView input clock is invalid");
    invariant(typeof wait === "function", "WebView input wait owner is invalid");
    this.maxProbeEvents = maxProbeEvents;
    this.targetAcquisitionTimeoutMs = targetAcquisitionTimeoutMs;
    this.targetAcquisitionPollMs = targetAcquisitionPollMs;
    this.now = now;
    this.wait = wait;
  }

  get pressedKeys() {
    return Array.from(this.#pressedKeys.values(), (entry) => ({
      key: entry.key,
      code: entry.code,
      delivery: entry.delivery,
    }));
  }

  get pointerPressed() {
    return this.#pressedPointer !== null;
  }

  get probeInstalled() {
    return this.#probeInstalled;
  }

  async observeExactTarget(locatorValue) {
    const locator = normalizeSemanticLocator(locatorValue);
    return {
      locator,
      observation: clone(await this.#cdp.evaluate(exactTargetExpression(locator))),
    };
  }

  #currentModifiers(extra = 0) {
    let modifiers = extra;
    for (const key of this.#pressedKeys.values()) modifiers |= key.modifierBit;
    return modifiers;
  }

  async resolveExactTarget(locatorValue, { stableHitSamples: requiredStableHitSamples = 1 } = {}) {
    const locator = normalizeSemanticLocator(locatorValue);
    invariant(
      Number.isInteger(requiredStableHitSamples)
        && requiredStableHitSamples >= 1
        && requiredStableHitSamples <= 10,
      "stableHitSamples must be an integer between 1 and 10",
    );
    const started = this.now();
    invariant(Number.isFinite(started), "WebView input clock returned an invalid value");
    const deadline = started + this.targetAcquisitionTimeoutMs;
    let attempts = 0;
    let initialObservation = null;
    let lastObservation = null;
    let existingScrollObserved = false;
    let stableTarget = null;
    let stableHitSamples = 0;
    while (true) {
      const observation = await this.#cdp.evaluate(exactTargetExpression(locator));
      attempts += 1;
      initialObservation ??= clone(observation);
      lastObservation = clone(observation);
      try {
        const acquired = assertExactSemanticTarget(observation, locator);
        if (!existingScrollObserved && requiredStableHitSamples === 1) {
          return {
            ...acquired,
            acquisition: {
              kind: "immediate",
              attempts,
              elapsed_ms: this.now() - started,
              stable_hit_samples: 1,
              initial_observation: initialObservation,
              final_observation: clone(observation),
            },
          };
        }
        if (sameTargetGeometry(stableTarget, acquired)) stableHitSamples += 1;
        else {
          stableTarget = clone(acquired);
          stableHitSamples = 1;
        }
        const settledSamples = existingScrollObserved
          ? Math.max(EXISTING_SCROLL_STABLE_HIT_SAMPLES, requiredStableHitSamples)
          : requiredStableHitSamples;
        if (stableHitSamples >= settledSamples) {
          return {
            ...acquired,
            acquisition: {
              kind: existingScrollObserved ? "existing-scroll-settled" : "stable-hit-settled",
              attempts,
              elapsed_ms: this.now() - started,
              stable_hit_samples: stableHitSamples,
              initial_observation: initialObservation,
              final_observation: clone(observation),
            },
          };
        }
      } catch (error) {
        if (!targetCanSettleThroughExistingScroll(error)) throw error;
        existingScrollObserved = true;
        stableTarget = null;
        stableHitSamples = 0;
      }
      {
        const now = this.now();
        invariant(Number.isFinite(now), "WebView input clock returned an invalid value");
        if (now >= deadline) {
          throw new WebviewInputError(
            existingScrollObserved
              ? "semantic-target-viewport-timeout"
              : "semantic-target-stability-timeout",
            existingScrollObserved
              ? "semantic target did not enter the viewport through the existing product scroll"
              : "semantic target did not remain stable for the required hit-test samples",
            {
              locator,
              attempts,
              elapsed_ms: now - started,
              stable_hit_samples: stableHitSamples,
              initial_observation: initialObservation,
              last_observation: lastObservation,
            },
          );
        }
        await this.wait(Math.min(this.targetAcquisitionPollMs, deadline - now));
      }
    }
  }

  async pointerDown(locatorValue, acquisitionOptions) {
    if (this.#pressedPointer !== null) {
      throw new WebviewInputError("pointer-already-pressed", "a WebView pointer press is already active", this.#pressedPointer);
    }
    const target = await this.resolveExactTarget(locatorValue, acquisitionOptions);
    const modifiers = this.#currentModifiers();
    await this.#cdp.call("Input.dispatchMouseEvent", {
      type: "mouseMoved",
      x: target.center.x,
      y: target.center.y,
      button: "none",
      buttons: 0,
      modifiers,
    });
    this.#pressedPointer = { ...target, modifiers, delivery: "pending" };
    try {
      await this.#cdp.call("Input.dispatchMouseEvent", {
        type: "mousePressed",
        x: target.center.x,
        y: target.center.y,
        button: "left",
        buttons: 1,
        clickCount: 1,
        modifiers,
      });
      this.#pressedPointer.delivery = "confirmed";
    } catch (error) {
      this.#pressedPointer.delivery = "ambiguous";
      throw error;
    }
    return clone(target);
  }

  async hover(locatorValue) {
    if (this.#pressedPointer !== null) {
      throw new WebviewInputError("pointer-already-pressed", "a WebView pointer press is already active", this.#pressedPointer);
    }
    const target = await this.resolveExactTarget(locatorValue);
    await this.#cdp.call("Input.dispatchMouseEvent", {
      type: "mouseMoved",
      x: target.center.x,
      y: target.center.y,
      button: "none",
      buttons: 0,
      modifiers: this.#currentModifiers(),
    });
    return clone(target);
  }

  async pointerUp() {
    if (this.#pressedPointer === null) {
      throw new WebviewInputError("pointer-not-pressed", "no WebView pointer press is active");
    }
    const pressed = this.#pressedPointer;
    await this.#cdp.call("Input.dispatchMouseEvent", {
      type: "mouseReleased",
      x: pressed.center.x,
      y: pressed.center.y,
      button: "left",
      buttons: 0,
      clickCount: 1,
      modifiers: this.#currentModifiers(),
    });
    this.#pressedPointer = null;
    return clone(pressed);
  }

  async click(locatorValue, acquisitionOptions) {
    const target = await this.pointerDown(locatorValue, acquisitionOptions);
    await this.pointerUp();
    return target;
  }

  async keyDown(keyValue) {
    const key = normalizeWebviewKey(keyValue);
    if (this.#pressedKeys.has(key.code)) {
      throw new WebviewInputError("key-already-pressed", `WebView key is already pressed: ${key.code}`, { key: key.key, code: key.code });
    }
    const modifiers = this.#currentModifiers(key.modifierBit);
    const params = {
      type: "keyDown",
      key: key.key,
      code: key.code,
      windowsVirtualKeyCode: key.virtualKey,
      nativeVirtualKeyCode: key.virtualKey,
      modifiers,
      autoRepeat: false,
      isKeypad: false,
    };
    if (key.text !== undefined && (modifiers & (MODIFIER_BITS.Alt | MODIFIER_BITS.Control | MODIFIER_BITS.Meta)) === 0) {
      params.text = key.text;
      params.unmodifiedText = key.text;
    }
    const pressed = { ...key, delivery: "pending" };
    this.#pressedKeys.set(key.code, pressed);
    try {
      await this.#cdp.call("Input.dispatchKeyEvent", params);
      pressed.delivery = "confirmed";
    } catch (error) {
      pressed.delivery = "ambiguous";
      throw error;
    }
    return { key: key.key, code: key.code, modifiers };
  }

  async #releaseKeyEntry(key) {
    await this.#cdp.call("Input.dispatchKeyEvent", {
      type: "keyUp",
      key: key.key,
      code: key.code,
      windowsVirtualKeyCode: key.virtualKey,
      nativeVirtualKeyCode: key.virtualKey,
      modifiers: this.#currentModifiers(),
      isKeypad: false,
    });
    this.#pressedKeys.delete(key.code);
    return { key: key.key, code: key.code };
  }

  async keyUp(keyValue) {
    const requested = normalizeWebviewKey(keyValue);
    const pressed = this.#pressedKeys.get(requested.code);
    if (!pressed) {
      throw new WebviewInputError("key-not-pressed", `WebView key is not pressed: ${requested.code}`, { key: requested.key, code: requested.code });
    }
    return this.#releaseKeyEntry(pressed);
  }

  async pressKey(keyValue) {
    await this.keyDown(keyValue);
    return this.keyUp(keyValue);
  }

  async typeText(text) {
    invariant(typeof text === "string", "WebView text must be a string");
    const characters = Array.from(text);
    characters.forEach((character) => normalizeWebviewKey(character));
    for (const character of characters) {
      await this.pressKey(character);
    }
    return { text, character_count: characters.length };
  }

  async insertText(locatorValue, text) {
    invariant(typeof text === "string" && text.length > 0, "WebView inserted text must be non-empty");
    invariant(!text.includes("\0") && Buffer.byteLength(text, "utf8") <= 1_000_000, "WebView inserted text is invalid");
    const target = await this.resolveExactTarget(locatorValue);
    const activeIdentity = normalizeSemanticIdentity(await this.#cdp.evaluate(activeSemanticIdentityExpression()));
    if (!identityMatches(activeIdentity, target.identity)) {
      throw new WebviewInputError(
        "text-insert-focus-owner",
        "WebView text insertion target is not the exact active semantic owner",
        { target, active_identity: activeIdentity },
      );
    }
    try {
      await this.#cdp.call("Input.insertText", { text });
    } catch (error) {
      throw new WebviewInputError(
        "text-insert-delivery-ambiguous",
        "WebView text insertion delivery is ambiguous and must not be retried",
        { target, active_identity: activeIdentity, message: errorMessage(error) },
      );
    }
    return {
      target,
      active_identity: activeIdentity,
      text,
      character_count: Array.from(text).length,
      utf8_byte_count: Buffer.byteLength(text, "utf8"),
      delivery: "confirmed",
    };
  }

  async releasePressedKeys() {
    const failures = [];
    const released = [];
    for (const key of Array.from(this.#pressedKeys.values()).reverse()) {
      try {
        released.push(await this.#releaseKeyEntry(key));
      } catch (error) {
        failures.push({ owner: `key:${key.code}`, error });
      }
    }
    if (failures.length > 0) throw aggregateInputError("one or more pressed WebView keys could not be released", failures);
    return { released };
  }

  async releasePressedInputs() {
    const failures = [];
    let pointerReleased = false;
    if (this.#pressedPointer !== null) {
      try {
        await this.pointerUp();
        pointerReleased = true;
      } catch (error) {
        failures.push({ owner: "pointer:left", error });
      }
    }
    let keys = { released: [] };
    try {
      keys = await this.releasePressedKeys();
    } catch (error) {
      const nested = Array.isArray(error?.evidence) ? error.evidence : [{ owner: "keys", message: errorMessage(error) }];
      failures.push(...nested.map((failure) => ({ owner: failure.owner, error: new Error(failure.message) })));
    }
    if (failures.length > 0) throw aggregateInputError("one or more pressed WebView inputs could not be released", failures);
    return { pointer_released: pointerReleased, released_keys: keys.released };
  }

  async installProbe() {
    if (this.#probeInstalled) throw new WebviewInputError("event-probe-owned", "this WebView input driver already owns an event probe");
    const result = await this.#cdp.evaluate(installProbeExpression(this.probeId, this.maxProbeEvents));
    if (result?.installed !== true || result.probe_id !== this.probeId || result.sequence !== 0) {
      throw new WebviewInputError("event-probe-install", "WebView event probe installation failed", result);
    }
    this.#probeInstalled = true;
    return clone(result);
  }

  async snapshotProbe(afterSequence = 0) {
    invariant(Number.isInteger(afterSequence) && afterSequence >= 0, "probe afterSequence must be a non-negative integer");
    if (!this.#probeInstalled) throw new WebviewInputError("event-probe-not-owned", "this WebView input driver does not own an event probe");
    const result = await this.#cdp.evaluate(snapshotProbeExpression(this.probeId, afterSequence));
    if (result?.found !== true || result.probe_id !== this.probeId) {
      throw new WebviewInputError("event-probe-snapshot", "WebView event probe disappeared before observation", result);
    }
    return clone(result);
  }

  async removeProbe() {
    if (!this.#probeInstalled) return { removed: false, probe_id: this.probeId, sequence: null };
    const result = await this.#cdp.evaluate(removeProbeExpression(this.probeId));
    if (result?.removed !== true || result.probe_id !== this.probeId) {
      throw new WebviewInputError("event-probe-remove", "WebView event probe removal failed", result);
    }
    this.#probeInstalled = false;
    return clone(result);
  }

  async cleanup() {
    const failures = [];
    let inputs = null;
    let probe = null;
    try { inputs = await this.releasePressedInputs(); }
    catch (error) { failures.push({ owner: "pressed-inputs", error }); }
    try { probe = await this.removeProbe(); }
    catch (error) { failures.push({ owner: "event-probe", error }); }
    if (failures.length > 0) throw aggregateInputError("WebView input cleanup did not settle exactly", failures);
    return { inputs, probe };
  }
}
