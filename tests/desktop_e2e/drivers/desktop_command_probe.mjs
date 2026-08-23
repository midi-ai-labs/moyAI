import { isDeepStrictEqual } from "node:util";

const OBSERVER_SYMBOL = "moyai.desktop.command-observer.v1";
const PROBE_ID = /^[a-z0-9][a-z0-9._-]{2,127}$/;
const COMMAND = /^[a-z][a-z0-9_]{1,127}$/;

function invariant(condition, message) {
  if (!condition) throw new TypeError(message);
}

function clone(value) {
  return value === undefined ? undefined : structuredClone(value);
}

function normalizeProbeId(value) {
  invariant(typeof value === "string" && PROBE_ID.test(value), "invalid Desktop command probe id");
  return value;
}

function normalizeCommands(values) {
  invariant(Array.isArray(values) && values.length > 0, "Desktop command probe commands must be non-empty");
  const commands = values.map((value) => {
    invariant(typeof value === "string" && COMMAND.test(value), `invalid Desktop command: ${value}`);
    return value;
  });
  invariant(new Set(commands).size === commands.length, "Desktop command probe commands must be unique");
  return commands;
}

function installExpression(probeId, commands, maxCalls) {
  return `(() => {
    const probeId = ${JSON.stringify(probeId)};
    const commands = new Set(${JSON.stringify(commands)});
    const observerKey = Symbol.for(${JSON.stringify(OBSERVER_SYMBOL)});
    const registryKey = Symbol.for('moyai.desktop_e2e.desktop_command.probes.v1');
    let registry = globalThis[registryKey];
    if (!(registry instanceof Map)) {
      registry = new Map();
      Object.defineProperty(globalThis, registryKey, { value: registry, configurable: true });
    }
    if (registry.size !== 0 || registry.has(probeId) || globalThis[observerKey] !== undefined) {
      return { installed: false, reason: 'desktop-command-probe-already-installed' };
    }
    const state = { sequence: 0, droppedThrough: 0, calls: [], observer: null };
    const observer = (observation) => {
      if (!observation || !commands.has(observation.name)) return;
      state.sequence += 1;
      state.calls.push({
        sequence: state.sequence,
        command: observation.name,
        args: structuredClone(observation.args ?? {}),
      });
      if (state.calls.length > ${maxCalls}) {
        const removed = state.calls.splice(0, state.calls.length - ${maxCalls});
        state.droppedThrough = removed.at(-1)?.sequence ?? state.droppedThrough;
      }
    };
    state.observer = observer;
    Object.defineProperty(globalThis, observerKey, {
      configurable: true,
      enumerable: false,
      writable: false,
      value: observer,
    });
    if (globalThis[observerKey] !== observer) {
      return { installed: false, reason: 'desktop-command-observer-not-owned' };
    }
    registry.set(probeId, state);
    return { installed: true, probe_id: probeId, sequence: 0 };
  })()`;
}

function snapshotExpression(probeId, afterSequence) {
  return `(() => {
    const probeId = ${JSON.stringify(probeId)};
    const registry = globalThis[Symbol.for('moyai.desktop_e2e.desktop_command.probes.v1')];
    const state = registry instanceof Map ? registry.get(probeId) : null;
    if (!state) return { found: false, probe_id: probeId };
    return {
      found: true,
      probe_id: probeId,
      sequence: state.sequence,
      dropped_through: state.droppedThrough,
      calls: state.calls.filter((call) => call.sequence > ${afterSequence}),
    };
  })()`;
}

function removeExpression(probeId) {
  return `(() => {
    const probeId = ${JSON.stringify(probeId)};
    const observerKey = Symbol.for(${JSON.stringify(OBSERVER_SYMBOL)});
    const registryKey = Symbol.for('moyai.desktop_e2e.desktop_command.probes.v1');
    const registry = globalThis[registryKey];
    const state = registry instanceof Map ? registry.get(probeId) : null;
    if (!state) return { removed: false, probe_id: probeId, sequence: null };
    if (globalThis[observerKey] !== state.observer) {
      return { removed: false, probe_id: probeId, sequence: state.sequence, reason: 'desktop-command-probe-owner-drift' };
    }
    delete globalThis[observerKey];
    registry.delete(probeId);
    if (registry.size === 0) delete globalThis[registryKey];
    return { removed: true, probe_id: probeId, sequence: state.sequence };
  })()`;
}

export class DesktopCommandProbeError extends Error {
  constructor(code, message, evidence = null) {
    super(message);
    this.name = "DesktopCommandProbeError";
    this.code = code;
    this.evidence = evidence === null ? null : clone(evidence);
  }
}

export function assertExactDesktopCommandSequence(snapshot, { afterSequence = 0, expected }) {
  invariant(Number.isInteger(afterSequence) && afterSequence >= 0, "command probe afterSequence must be non-negative");
  invariant(Array.isArray(expected), "expected Desktop command sequence must be an array");
  if (snapshot?.found !== true || !Number.isInteger(snapshot.sequence) || !Array.isArray(snapshot.calls)) {
    throw new DesktopCommandProbeError("desktop-command-probe-snapshot-invalid", "Desktop command probe snapshot is invalid", snapshot);
  }
  if (!Number.isInteger(snapshot.dropped_through) || snapshot.dropped_through > afterSequence) {
    throw new DesktopCommandProbeError("desktop-command-probe-overflow", "required Desktop commands were dropped", snapshot);
  }
  let previous = afterSequence;
  for (const call of snapshot.calls) {
    if (!Number.isInteger(call?.sequence) || call.sequence <= previous || call.sequence > snapshot.sequence) {
      throw new DesktopCommandProbeError("desktop-command-probe-order", "Desktop command probe sequence is not ordered", snapshot);
    }
    previous = call.sequence;
  }
  if (snapshot.calls.length !== expected.length) {
    throw new DesktopCommandProbeError(
      "desktop-command-probe-cardinality",
      `expected ${expected.length} Desktop commands, found ${snapshot.calls.length}`,
      { expected, snapshot },
    );
  }
  expected.forEach((wanted, index) => {
    invariant(typeof wanted?.command === "string" && COMMAND.test(wanted.command), "expected Desktop command is invalid");
    const actual = snapshot.calls[index];
    if (actual.command !== wanted.command || !isDeepStrictEqual(actual.args, wanted.args ?? {})) {
      throw new DesktopCommandProbeError(
        "desktop-command-probe-call-mismatch",
        "Desktop command or arguments did not match",
        { index, wanted, actual },
      );
    }
  });
  return { after_sequence: afterSequence, last_sequence: snapshot.sequence, calls: clone(snapshot.calls) };
}

export class DesktopCommandProbe {
  #cdp;
  #installed = false;

  constructor(cdp, { probeId = "desktop-command", commands, maxCalls = 256 }) {
    invariant(cdp !== null && typeof cdp === "object" && typeof cdp.evaluate === "function", "CDP client is required");
    invariant(Number.isInteger(maxCalls) && maxCalls >= 8 && maxCalls <= 4096, "maxCalls is invalid");
    this.#cdp = cdp;
    this.probeId = normalizeProbeId(probeId);
    this.commands = normalizeCommands(commands);
    this.maxCalls = maxCalls;
  }

  get installed() {
    return this.#installed;
  }

  async install() {
    if (this.#installed) throw new DesktopCommandProbeError("desktop-command-probe-owned", "this driver already owns a Desktop command probe");
    const result = await this.#cdp.evaluate(installExpression(this.probeId, this.commands, this.maxCalls));
    if (result?.installed !== true || result.probe_id !== this.probeId || result.sequence !== 0) {
      throw new DesktopCommandProbeError("desktop-command-probe-install", "Desktop command probe installation failed", result);
    }
    this.#installed = true;
    return clone(result);
  }

  async snapshot(afterSequence = 0) {
    invariant(Number.isInteger(afterSequence) && afterSequence >= 0, "command probe afterSequence must be non-negative");
    if (!this.#installed) throw new DesktopCommandProbeError("desktop-command-probe-not-owned", "this driver does not own a Desktop command probe");
    const result = await this.#cdp.evaluate(snapshotExpression(this.probeId, afterSequence));
    if (result?.found !== true || result.probe_id !== this.probeId) {
      throw new DesktopCommandProbeError("desktop-command-probe-snapshot", "Desktop command probe disappeared", result);
    }
    return clone(result);
  }

  async remove() {
    if (!this.#installed) return { removed: false, probe_id: this.probeId, sequence: null };
    const result = await this.#cdp.evaluate(removeExpression(this.probeId));
    if (result?.removed !== true || result.probe_id !== this.probeId) {
      throw new DesktopCommandProbeError("desktop-command-probe-remove", "Desktop command probe removal failed", result);
    }
    this.#installed = false;
    return clone(result);
  }
}
