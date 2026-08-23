const SEMANTIC_IDENTITY_FIELDS = Object.freeze([
  "tag",
  "id",
  "action",
  "focusKey",
  "configKey",
  "sideSetting",
]);
const STABLE_IDENTITY_FIELDS = Object.freeze(SEMANTIC_IDENTITY_FIELDS.filter((field) => field !== "tag"));
const PROBE_ID = /^[a-z0-9][a-z0-9._-]{2,127}$/;

const MODIFIER_BITS = Object.freeze({
  Alt: 1,
  Control: 2,
  Meta: 4,
  Shift: 8,
});

const NAMED_KEYS = Object.freeze({
  Alt: { key: "Alt", code: "AltLeft", virtualKey: 18, modifierBit: MODIFIER_BITS.Alt },
  Backspace: { key: "Backspace", code: "Backspace", virtualKey: 8 },
  Control: { key: "Control", code: "ControlLeft", virtualKey: 17, modifierBit: MODIFIER_BITS.Control },
  Enter: { key: "Enter", code: "Enter", virtualKey: 13 },
  Escape: { key: "Escape", code: "Escape", virtualKey: 27 },
  Meta: { key: "Meta", code: "MetaLeft", virtualKey: 91, modifierBit: MODIFIER_BITS.Meta },
  Shift: { key: "Shift", code: "ShiftLeft", virtualKey: 16, modifierBit: MODIFIER_BITS.Shift },
  Tab: { key: "Tab", code: "Tab", virtualKey: 9 },
});

const PAGE_IDENTITY_SOURCE = `
  const semanticIdentity = (value) => {
    if (!(value instanceof Element)) {
      return { tag: "", id: null, action: null, focusKey: null, configKey: null, sideSetting: null };
    }
    const owner = value.closest(
      '[data-action], [data-focus-key], [data-config-key], [data-side-chat-setting], button, input, textarea, select, a[href], [tabindex], [role]'
    ) ?? value;
    return {
      tag: owner.tagName.toUpperCase(),
      id: owner.id || null,
      action: owner instanceof HTMLElement ? (owner.dataset.action ?? null) : null,
      focusKey: owner instanceof HTMLElement ? (owner.dataset.focusKey ?? null) : null,
      configKey: owner instanceof HTMLElement ? (owner.dataset.configKey ?? null) : null,
      sideSetting: owner instanceof HTMLElement ? (owner.dataset.sideChatSetting ?? null) : null,
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

function nullableIdentityValue(value, field) {
  if (value === undefined) return undefined;
  if (value === null) return null;
  invariant(typeof value === "string", `semantic identity ${field} must be a string or null`);
  const normalized = field === "tag" ? value.trim().toUpperCase() : value;
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
    "semantic locator requires id, action, focusKey, configKey, or sideSetting identity",
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
    const hit = visible ? document.elementFromPoint(center.x, center.y) : null;
    return {
      count: 1,
      connected: target.isConnected,
      visible,
      enabled,
      identity: semanticIdentity(target),
      hit_identity: semanticIdentity(hit),
      center_hit: hit instanceof Element && (hit === target || target.contains(hit)),
      center,
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
    for (const type of ['pointerdown', 'pointerup', 'click', 'keydown', 'keyup', 'input', 'focusin']) {
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

  constructor(cdp, { probeId = "webview-input", maxProbeEvents = 4096 } = {}) {
    this.#cdp = validateCdp(cdp);
    this.probeId = normalizeProbeId(probeId);
    invariant(Number.isInteger(maxProbeEvents) && maxProbeEvents >= 32 && maxProbeEvents <= 65_536, "maxProbeEvents is invalid");
    this.maxProbeEvents = maxProbeEvents;
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

  #currentModifiers(extra = 0) {
    let modifiers = extra;
    for (const key of this.#pressedKeys.values()) modifiers |= key.modifierBit;
    return modifiers;
  }

  async resolveExactTarget(locatorValue) {
    const locator = normalizeSemanticLocator(locatorValue);
    const observation = await this.#cdp.evaluate(exactTargetExpression(locator));
    return assertExactSemanticTarget(observation, locator);
  }

  async pointerDown(locatorValue) {
    if (this.#pressedPointer !== null) {
      throw new WebviewInputError("pointer-already-pressed", "a WebView pointer press is already active", this.#pressedPointer);
    }
    const target = await this.resolveExactTarget(locatorValue);
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

  async click(locatorValue) {
    const target = await this.pointerDown(locatorValue);
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
