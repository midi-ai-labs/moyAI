import { waitForObservation } from "../core/deadline.mjs";

export function normalizeDesktopCommandError(error) {
  let record = error !== null && typeof error === "object" && !Array.isArray(error)
    ? error
    : null;
  if (record === null && typeof error === "string" && error.trim().startsWith("{")) {
    try {
      const parsed = JSON.parse(error);
      if (parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)) record = parsed;
    } catch {
      // The original non-JSON string remains the diagnostic message below.
    }
  }
  return {
    kind: typeof record?.kind === "string" ? record.kind : null,
    message: error instanceof Error
      ? error.message
      : typeof record?.message === "string"
        ? record.message
        : String(error),
    state: record !== null && Object.prototype.hasOwnProperty.call(record, "state")
      ? record.state
      : null,
  };
}

export async function invokeDesktopCommandOutcome(cdp, command, args = undefined) {
  if (typeof command !== "string" || !/^[a-z][a-z0-9_]{1,95}$/.test(command)) {
    throw new TypeError(`invalid Desktop command: ${command}`);
  }
  const encodedCommand = JSON.stringify(command);
  const encodedArgs = args === undefined ? "undefined" : JSON.stringify(args);
  const normalizeError = `(${normalizeDesktopCommandError.toString()})`;
  const outcome = await cdp.evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function') {
      return { ok: false, error: { kind: null, message: 'tauri-invoke-unavailable', state: null }, value: null };
    }
    try {
      return { ok: true, error: null, value: await invoke(${encodedCommand}, ${encodedArgs}) };
    } catch (error) {
      return {
        ok: false,
        error: ${normalizeError}(error),
        value: null,
      };
    }
  })()`);
  return outcome;
}

export async function invokeDesktopCommand(cdp, command, args = undefined) {
  const outcome = await invokeDesktopCommandOutcome(cdp, command, args);
  if (outcome?.ok !== true) {
    throw new Error(`Desktop command ${command} failed: ${outcome?.error?.message ?? "unknown error"}`);
  }
  return outcome.value;
}

export function selectedNavigationIdentity(projection) {
  const project = projection?.selected_project_index >= 0
    ? projection?.project_rows?.[projection.selected_project_index] ?? null
    : null;
  const sessions = projection?.selected_project_index >= 0
    ? projection?.session_rows
    : projection?.chat_session_rows;
  const session = projection?.selected_session_index >= 0
    ? sessions?.[projection.selected_session_index] ?? null
    : null;
  return {
    workspace_path: projection?.workspace_path ?? null,
    project_id: project?.project_id ?? null,
    project_path: project?.path ?? null,
    session_id: session?.session_id ?? null,
    project_row_ids: Array.isArray(projection?.project_rows)
      ? projection.project_rows.map((row) => row.project_id)
      : [],
    session_row_ids: Array.isArray(sessions) ? sessions.map((row) => row.session_id) : [],
  };
}

export async function waitForDesktopProjection({
  cdp,
  label,
  accept,
  timeoutMs = 30_000,
  pollMs = 100,
}) {
  if (typeof accept !== "function") throw new TypeError("projection predicate is required");
  return waitForObservation({
    label,
    timeoutMs,
    pollMs,
    sample: () => invokeDesktopCommand(cdp, "desktop_state"),
    accept,
  });
}

export async function captureScenarioScreenshot({ cdp, sink, name, owner }) {
  if (!/^[a-z0-9][a-z0-9._-]{2,95}$/.test(name)) throw new TypeError(`invalid screenshot name: ${name}`);
  const identity = await sink.writeBytes(`screenshots/${name}.png`, await cdp.screenshot());
  await sink.record("scenario-screenshot", { name, ...identity }, { phase: "executing", owner });
  return identity;
}
