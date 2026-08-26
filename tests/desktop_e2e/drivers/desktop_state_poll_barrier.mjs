const REGISTRY_SYMBOL = "moyai.desktop_e2e.desktop_state_poll_barriers.v1";
const BARRIER_ID = /^[a-z0-9][a-z0-9._-]{2,127}$/;

function invariant(condition, message) {
  if (!condition) throw new TypeError(message);
}

function clone(value) {
  return value === undefined ? undefined : structuredClone(value);
}

function normalizeBarrierId(value) {
  invariant(typeof value === "string" && BARRIER_ID.test(value), "invalid Desktop state poll barrier id");
  return value;
}

function installExpression(barrierId) {
  return `(() => {
    const barrierId = ${JSON.stringify(barrierId)};
    const registryKey = Symbol.for(${JSON.stringify(REGISTRY_SYMBOL)});
    const internals = window.__TAURI_INTERNALS__;
    if (!internals || typeof internals.invoke !== 'function' || typeof window.fetch !== 'function') {
      return { installed: false, reason: 'tauri-fetch-transport-unavailable' };
    }
    let registry = globalThis[registryKey];
    if (!(registry instanceof Map)) {
      registry = new Map();
      Object.defineProperty(globalThis, registryKey, { value: registry, configurable: true });
    }
    if (registry.size !== 0 || registry.has(barrierId)) {
      return { installed: false, reason: 'desktop-state-poll-barrier-already-installed' };
    }
    const originalFetch = window.fetch;
    const state = {
      phase: 'idle',
      originalFetch,
      wrapped: null,
      waiter: null,
      captured: null,
      interceptedCount: 0,
      bypassCount: 0,
      bypassDepth: 0,
    };
    const isDesktopStateRequest = (input, init) => {
      try {
        const url = new URL(input instanceof Request ? input.url : String(input), location.href);
        const method = String(init?.method ?? (input instanceof Request ? input.method : 'GET')).toUpperCase();
        return method === 'POST'
          && url.hostname === 'ipc.localhost'
          && decodeURIComponent(url.pathname.replace(/^\\/+/, '')) === 'desktop_state';
      } catch {
        return false;
      }
    };
    const wrapped = function(input, init) {
      if (state.bypassDepth > 0 || state.phase !== 'armed' || !isDesktopStateRequest(input, init)) {
        return originalFetch.call(window, input, init);
      }
      state.phase = 'waiting';
      state.interceptedCount += 1;
      return new Promise((resolve, reject) => {
        state.waiter = { input, init, resolve, reject };
      });
    };
    state.wrapped = wrapped;
    window.fetch = wrapped;
    if (window.fetch !== wrapped) {
      return {
        installed: false,
        reason: 'fetch-wrapper-not-owned',
        fetch_descriptor: Object.getOwnPropertyDescriptor(window, 'fetch') ?? null,
      };
    }
    registry.set(barrierId, state);
    return { installed: true, barrier_id: barrierId, phase: state.phase, transport: 'tauri-ipc-fetch' };
  })()`;
}

function armExpression(barrierId) {
  return `(() => {
    const barrierId = ${JSON.stringify(barrierId)};
    const registry = globalThis[Symbol.for(${JSON.stringify(REGISTRY_SYMBOL)})];
    const state = registry instanceof Map ? registry.get(barrierId) : null;
    if (!state) return { armed: false, barrier_id: barrierId, reason: 'barrier-missing' };
    if (state.phase !== 'idle' || state.waiter !== null || state.captured !== null) {
      return { armed: false, barrier_id: barrierId, phase: state.phase, reason: 'barrier-not-idle' };
    }
    state.phase = 'armed';
    return { armed: true, barrier_id: barrierId, phase: state.phase };
  })()`;
}

function snapshotExpression(barrierId) {
  return `(() => {
    const barrierId = ${JSON.stringify(barrierId)};
    const registry = globalThis[Symbol.for(${JSON.stringify(REGISTRY_SYMBOL)})];
    const state = registry instanceof Map ? registry.get(barrierId) : null;
    if (!state) return { found: false, barrier_id: barrierId };
    return {
      found: true,
      barrier_id: barrierId,
      phase: state.phase,
      intercepted_count: state.interceptedCount,
      bypass_count: state.bypassCount,
      captured: state.captured === null ? null : {
        projection_revision: state.captured.projection_revision ?? null,
        post_run_refresh_pending: state.captured.post_run_refresh_pending ?? null,
        composer_submit_mode: state.captured.composer_submit_mode ?? null,
        can_submit: state.captured.can_submit ?? null,
        run_target: structuredClone(state.captured.run_target ?? null),
      },
    };
  })()`;
}

function capturePendingExpression(barrierId, timeoutMs, pollMs) {
  return `(async () => {
    const barrierId = ${JSON.stringify(barrierId)};
    const registry = globalThis[Symbol.for(${JSON.stringify(REGISTRY_SYMBOL)})];
    const state = registry instanceof Map ? registry.get(barrierId) : null;
    if (!state || state.phase !== 'waiting' || state.waiter === null) {
      return { captured: false, barrier_id: barrierId, phase: state?.phase ?? null, reason: 'barrier-not-waiting' };
    }
    const deadline = performance.now() + ${timeoutMs};
    let lastProjection = null;
    while (performance.now() <= deadline) {
      state.bypassDepth += 1;
      try {
        lastProjection = await window.__TAURI_INTERNALS__.invoke('desktop_state', {});
      } finally {
        state.bypassDepth -= 1;
      }
      state.bypassCount += 1;
      if (lastProjection?.post_run_refresh_pending === true) {
        state.captured = structuredClone(lastProjection);
        state.phase = 'captured';
        return {
          captured: true,
          barrier_id: barrierId,
          phase: state.phase,
          projection: structuredClone(lastProjection),
          bypass_count: state.bypassCount,
        };
      }
      await new Promise((resolve) => setTimeout(resolve, ${pollMs}));
    }
    return {
      captured: false,
      barrier_id: barrierId,
      phase: state.phase,
      reason: 'pending-projection-timeout',
      last_projection: structuredClone(lastProjection),
      bypass_count: state.bypassCount,
    };
  })()`;
}

function sampleBackendExpression(barrierId) {
  return `(async () => {
    const barrierId = ${JSON.stringify(barrierId)};
    const registry = globalThis[Symbol.for(${JSON.stringify(REGISTRY_SYMBOL)})];
    const state = registry instanceof Map ? registry.get(barrierId) : null;
    if (!state) return { sampled: false, barrier_id: barrierId, reason: 'barrier-missing' };
    state.bypassDepth += 1;
    let projection;
    try {
      projection = await window.__TAURI_INTERNALS__.invoke('desktop_state', {});
    } finally {
      state.bypassDepth -= 1;
    }
    state.bypassCount += 1;
    return {
      sampled: true,
      barrier_id: barrierId,
      projection: structuredClone(projection),
      bypass_count: state.bypassCount,
    };
  })()`;
}

function releaseCapturedExpression(barrierId, rearm) {
  return `(() => {
    const barrierId = ${JSON.stringify(barrierId)};
    const registry = globalThis[Symbol.for(${JSON.stringify(REGISTRY_SYMBOL)})];
    const state = registry instanceof Map ? registry.get(barrierId) : null;
    if (!state || state.phase !== 'captured' || state.waiter === null || state.captured === null) {
      return { released: false, barrier_id: barrierId, phase: state?.phase ?? null, reason: 'captured-projection-missing' };
    }
    const waiter = state.waiter;
    const projection = structuredClone(state.captured);
    state.waiter = null;
    state.captured = null;
    state.phase = ${rearm ? "'armed'" : "'idle'"};
    waiter.resolve(new Response(JSON.stringify(projection), {
      status: 200,
      headers: { 'content-type': 'application/json', 'Tauri-Response': 'ok' },
    }));
    return {
      released: true,
      barrier_id: barrierId,
      phase: state.phase,
      projection_revision: projection.projection_revision ?? null,
    };
  })()`;
}

function resumeFreshExpression(barrierId) {
  return `(async () => {
    const barrierId = ${JSON.stringify(barrierId)};
    const registry = globalThis[Symbol.for(${JSON.stringify(REGISTRY_SYMBOL)})];
    const state = registry instanceof Map ? registry.get(barrierId) : null;
    if (!state) return { resumed: false, barrier_id: barrierId, reason: 'barrier-missing' };
    if (state.phase === 'armed') {
      state.phase = 'idle';
      return { resumed: true, barrier_id: barrierId, phase: state.phase, delivered: false };
    }
    if (state.phase !== 'waiting' || state.waiter === null) {
      return { resumed: false, barrier_id: barrierId, phase: state.phase, reason: 'barrier-not-waiting-or-armed' };
    }
    const waiter = state.waiter;
    state.waiter = null;
    state.phase = 'idle';
    try {
      const response = await state.originalFetch.call(window, waiter.input, waiter.init);
      waiter.resolve(response);
      return { resumed: true, barrier_id: barrierId, phase: state.phase, delivered: true };
    } catch (error) {
      waiter.reject(error);
      throw error;
    }
  })()`;
}

function removeExpression(barrierId) {
  return `(async () => {
    const barrierId = ${JSON.stringify(barrierId)};
    const registryKey = Symbol.for(${JSON.stringify(REGISTRY_SYMBOL)});
    const registry = globalThis[registryKey];
    const state = registry instanceof Map ? registry.get(barrierId) : null;
    if (!state) return { removed: false, barrier_id: barrierId, reason: 'barrier-missing' };
    if (window.fetch !== state.wrapped) {
      return { removed: false, barrier_id: barrierId, reason: 'fetch-owner-drift' };
    }
    let settled_waiter = false;
    if (state.waiter !== null) {
      const waiter = state.waiter;
      state.waiter = null;
      if (state.captured !== null) {
        waiter.resolve(new Response(JSON.stringify(state.captured), {
          status: 200,
          headers: { 'content-type': 'application/json', 'Tauri-Response': 'ok' },
        }));
      } else {
        try {
          waiter.resolve(await state.originalFetch.call(window, waiter.input, waiter.init));
        } catch (error) {
          waiter.reject(error);
        }
      }
      settled_waiter = true;
    }
    window.fetch = state.originalFetch;
    registry.delete(barrierId);
    if (registry.size === 0) delete globalThis[registryKey];
    return {
      removed: true,
      barrier_id: barrierId,
      settled_waiter,
      intercepted_count: state.interceptedCount,
      bypass_count: state.bypassCount,
    };
  })()`;
}

export class DesktopStatePollBarrierError extends Error {
  constructor(code, message, evidence = null) {
    super(message);
    this.name = "DesktopStatePollBarrierError";
    this.code = code;
    this.evidence = evidence === null ? null : clone(evidence);
  }
}

export class DesktopStatePollBarrier {
  #cdp;
  #installed = false;

  constructor(cdp, { barrierId = "desktop-state-poll" } = {}) {
    invariant(cdp !== null && typeof cdp === "object" && typeof cdp.evaluate === "function", "CDP client is required");
    this.#cdp = cdp;
    this.barrierId = normalizeBarrierId(barrierId);
  }

  get installed() {
    return this.#installed;
  }

  async install() {
    if (this.#installed) throw new DesktopStatePollBarrierError("desktop-state-poll-barrier-owned", "this driver already owns a Desktop state poll barrier");
    const result = await this.#cdp.evaluate(installExpression(this.barrierId));
    if (result?.installed !== true || result.barrier_id !== this.barrierId || result.phase !== "idle") {
      throw new DesktopStatePollBarrierError("desktop-state-poll-barrier-install", "Desktop state poll barrier installation failed", result);
    }
    this.#installed = true;
    return clone(result);
  }

  async arm() {
    this.#requireInstalled();
    const result = await this.#cdp.evaluate(armExpression(this.barrierId));
    if (result?.armed !== true || result.barrier_id !== this.barrierId || result.phase !== "armed") {
      throw new DesktopStatePollBarrierError("desktop-state-poll-barrier-arm", "Desktop state poll barrier could not arm", result);
    }
    return clone(result);
  }

  async snapshot() {
    this.#requireInstalled();
    const result = await this.#cdp.evaluate(snapshotExpression(this.barrierId));
    if (result?.found !== true || result.barrier_id !== this.barrierId) {
      throw new DesktopStatePollBarrierError("desktop-state-poll-barrier-snapshot", "Desktop state poll barrier disappeared", result);
    }
    return clone(result);
  }

  async capturePending({ timeoutMs = 10_000, pollMs = 5 } = {}) {
    this.#requireInstalled();
    invariant(Number.isInteger(timeoutMs) && timeoutMs >= 100 && timeoutMs <= 60_000, "pending capture timeout is invalid");
    invariant(Number.isInteger(pollMs) && pollMs >= 1 && pollMs <= 100, "pending capture poll interval is invalid");
    const result = await this.#cdp.evaluate(capturePendingExpression(this.barrierId, timeoutMs, pollMs));
    if (result?.captured !== true || result.barrier_id !== this.barrierId || result.phase !== "captured") {
      throw new DesktopStatePollBarrierError("desktop-state-poll-barrier-capture", "Desktop pending projection was not captured", result);
    }
    return clone(result);
  }

  async sampleBackend() {
    this.#requireInstalled();
    const result = await this.#cdp.evaluate(sampleBackendExpression(this.barrierId));
    if (result?.sampled !== true || result.barrier_id !== this.barrierId) {
      throw new DesktopStatePollBarrierError("desktop-state-poll-barrier-sample", "Desktop backend projection sampling failed", result);
    }
    return clone(result.projection);
  }

  async releaseCaptured({ rearm = false } = {}) {
    this.#requireInstalled();
    invariant(typeof rearm === "boolean", "poll barrier rearm must be boolean");
    const result = await this.#cdp.evaluate(releaseCapturedExpression(this.barrierId, rearm));
    if (result?.released !== true || result.barrier_id !== this.barrierId) {
      throw new DesktopStatePollBarrierError("desktop-state-poll-barrier-release", "Desktop captured projection was not released", result);
    }
    return clone(result);
  }

  async resumeFresh() {
    this.#requireInstalled();
    const result = await this.#cdp.evaluate(resumeFreshExpression(this.barrierId));
    if (result?.resumed !== true || result.barrier_id !== this.barrierId || result.phase !== "idle") {
      throw new DesktopStatePollBarrierError("desktop-state-poll-barrier-resume", "Desktop state polling did not resume", result);
    }
    return clone(result);
  }

  async remove() {
    if (!this.#installed) return { removed: false, barrier_id: this.barrierId };
    const result = await this.#cdp.evaluate(removeExpression(this.barrierId));
    if (result?.removed !== true || result.barrier_id !== this.barrierId) {
      throw new DesktopStatePollBarrierError("desktop-state-poll-barrier-remove", "Desktop state poll barrier removal failed", result);
    }
    this.#installed = false;
    return clone(result);
  }

  #requireInstalled() {
    if (!this.#installed) throw new DesktopStatePollBarrierError("desktop-state-poll-barrier-not-owned", "this driver does not own a Desktop state poll barrier");
  }
}
