import assert from "node:assert/strict";
import test from "node:test";

import {
  cancelRunCommand,
  interruptSessionCommand,
  isExactStopMutationTarget,
} from "../src/stop_contract.ts";
import type { SessionRow, StopMutationTarget } from "../src/types.ts";

const SESSION_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

function rootTarget(overrides: Record<string, unknown> = {}): StopMutationTarget {
  return {
    kind: "root",
    workspacePath: "C:/workspace",
    sessionId: SESSION_ID,
    rootGeneration: "11",
    latestTurnId: TURN_ID,
    admissionRevision: "7",
    permissionConfirmationId: "41",
    ...overrides,
  } as StopMutationTarget;
}

function turnTarget(overrides: Record<string, unknown> = {}): StopMutationTarget {
  return {
    kind: "turn",
    workspacePath: "C:/workspace",
    sessionId: SESSION_ID,
    turnId: TURN_ID,
    admissionRevision: "7",
    rootEpoch: "11",
    ...overrides,
  } as StopMutationTarget;
}

function deepFreeze<T>(value: T): T {
  if (value !== null && typeof value === "object") {
    Object.values(value).forEach(deepFreeze);
    Object.freeze(value);
  }
  return value;
}

test("Stop target accepts only the exact Rust tagged union with canonical u64 and identities", () => {
  assert.equal(isExactStopMutationTarget(rootTarget()), true);
  assert.equal(isExactStopMutationTarget(rootTarget({ sessionId: null, latestTurnId: null, permissionConfirmationId: null })), true);
  assert.equal(isExactStopMutationTarget(turnTarget()), true);
  assert.equal(isExactStopMutationTarget(turnTarget({ admissionRevision: "18446744073709551615" })), true);

  for (const invalid of [
    rootTarget({ rootGeneration: "01" }),
    rootTarget({ admissionRevision: "-1" }),
    rootTarget({ admissionRevision: 7 }),
    rootTarget({ permissionConfirmationId: "041" }),
    rootTarget({ latestTurnId: TURN_ID.toLowerCase() }),
    turnTarget({ sessionId: "00000000-0000-0000-0000-000000000010" }),
    turnTarget({ rootEpoch: "011" }),
    turnTarget({ sessionId: null }),
    turnTarget({ admissionRevision: "18446744073709551616" }),
    turnTarget({ turnId: "not-a-turn" }),
    { ...turnTarget(), compatibilityFlag: false },
    (() => { const value = { ...turnTarget() } as Record<string, unknown>; delete value.admissionRevision; return value; })(),
  ]) {
    assert.equal(isExactStopMutationTarget(invalid), false, JSON.stringify(invalid));
  }
});

test("current and row Stop reject missing targets before invoke and forward an immutable exact snapshot once", async () => {
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const invocations: Array<{ name: string; args: Record<string, unknown> }> = [];
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: {
      __TAURI_INTERNALS__: {
        invoke: (name: string, args: Record<string, unknown>) => {
          invocations.push({ name, args: structuredClone(args) });
          const forwarded = (name === "cancel_run" ? args.expectedTarget : args.expectedStopTarget) as Record<string, unknown>;
          forwarded.admissionRevision = "999";
          return Promise.resolve({ projection_revision: "1" });
        },
      },
    },
  });

  try {
    const currentTarget = deepFreeze(turnTarget());
    await cancelRunCommand(currentTarget);
    assert.equal(currentTarget.admissionRevision, "7");
    assert.deepEqual(invocations[0], {
      name: "cancel_run",
      args: { expectedTarget: turnTarget() },
    });

    const rowTarget = deepFreeze(turnTarget({ admissionRevision: "8" }));
    const rowArgs = deepFreeze({
      index: 2,
      expectedTarget: {
        workspacePath: "C:/workspace",
        ownerProjectId: "project-1",
        ownerSessionId: SESSION_ID,
        rowId: SESSION_ID,
      },
      expectedStopTarget: rowTarget,
    });
    await interruptSessionCommand(rowArgs);
    assert.equal(rowTarget.admissionRevision, "8");
    assert.deepEqual(invocations[1], {
      name: "interrupt_session",
      args: rowArgs,
    });

    const missingRevision = { ...turnTarget() } as Record<string, unknown>;
    delete missingRevision.admissionRevision;
    await assert.rejects(
      cancelRunCommand(missingRevision as StopMutationTarget),
      /exact canonical Stop mutation target/,
    );
    await assert.rejects(
      interruptSessionCommand({ expectedStopTarget: { ...missingRevision } }),
      /exact canonical Stop mutation target/,
    );
    assert.equal(invocations.length, 2, "invalid Stop payloads never reach the Tauri invoke boundary");
  } finally {
    if (previousWindow) Object.defineProperty(globalThis, "window", previousWindow);
    else delete (globalThis as Record<string, unknown>).window;
  }
});

test("SessionRow and both Stop variants require their admission revision at compile time", () => {
  const row: SessionRow = {
    session_id: SESSION_ID,
    title: "running",
    status: "running",
    loaded_status: "active",
    archived: false,
    active_turn_id: TURN_ID,
    active_turn_sequence_no: 1,
    admission_revision: "7",
    interrupt_target: turnTarget(),
    pending_permission_requests: 0,
    pending_user_input_requests: 0,
    short_id: "00000000",
    label: "running",
  };
  assert.equal(row.admission_revision, "7");

  // @ts-expect-error admission_revision is required on every projected SessionRow.
  const missingRowRevision: SessionRow = {
    session_id: SESSION_ID,
    title: "idle",
    status: "idle",
    loaded_status: "idle",
    archived: false,
    pending_permission_requests: 0,
    pending_user_input_requests: 0,
    short_id: "00000000",
    label: "idle",
  };
  // @ts-expect-error admissionRevision is required on the root Stop variant.
  const missingRootRevision: StopMutationTarget = {
    kind: "root",
    workspacePath: "C:/workspace",
    sessionId: null,
    rootGeneration: "1",
    latestTurnId: null,
    permissionConfirmationId: null,
  };
  // @ts-expect-error admissionRevision is required on the turn Stop variant.
  const missingTurnRevision: StopMutationTarget = {
    kind: "turn",
    workspacePath: "C:/workspace",
    sessionId: SESSION_ID,
    turnId: TURN_ID,
    rootEpoch: "1",
  };
  const missingTurnSession: StopMutationTarget = {
    kind: "turn",
    workspacePath: "C:/workspace",
    // @ts-expect-error an admitted Turn Stop always has one concrete canonical session ULID.
    sessionId: null,
    turnId: TURN_ID,
    admissionRevision: "1",
    rootEpoch: "1",
  };
  void missingRowRevision;
  void missingRootRevision;
  void missingTurnRevision;
  void missingTurnSession;
});
