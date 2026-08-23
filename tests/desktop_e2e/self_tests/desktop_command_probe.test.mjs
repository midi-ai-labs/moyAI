import assert from "node:assert/strict";
import test from "node:test";

import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";

const TARGET = Object.freeze({
  workspacePath: "C:\\e2e\\workspace",
  rootSessionId: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
  agentPath: "/root/child",
  childSessionId: "01ARZ3NDEKTSV4RRFFQ69G5FAX",
  expectedTurnId: "01ARZ3NDEKTSV4RRFFQ69G5FAY",
  admissionRevision: "3",
});

function snapshot(calls) {
  return {
    found: true,
    probe_id: "agent-interrupt-command",
    sequence: calls.length,
    dropped_through: 0,
    calls: calls.map((call, index) => ({ sequence: index + 1, ...call })),
  };
}

test("exact command acquisition rejects missing, duplicate, and drifted mutation targets", () => {
  const expected = [{ command: "interrupt_agent", args: { expectedTarget: TARGET } }];
  const exact = snapshot(expected);
  assert.deepEqual(assertExactDesktopCommandSequence(exact, { expected }).calls, exact.calls);

  for (const invalid of [
    snapshot([]),
    snapshot([...expected, ...expected]),
    snapshot([{ command: "cancel_run", args: { expectedTarget: TARGET } }]),
    snapshot([{
      command: "interrupt_agent",
      args: { expectedTarget: { ...TARGET, expectedTurnId: "01ARZ3NDEKTSV4RRFFQ69G5FAZ" } },
    }]),
    snapshot([{
      command: "interrupt_agent",
      args: { expectedTarget: { ...TARGET, admissionRevision: "4" } },
    }]),
    snapshot([{ command: "interrupt_agent", args: {} }]),
  ]) {
    assert.throws(
      () => assertExactDesktopCommandSequence(invalid, { expected }),
      (error) => ["desktop-command-probe-cardinality", "desktop-command-probe-call-mismatch"].includes(error.code),
    );
  }
});

class FakeCdp {
  constructor(results) {
    this.results = [...results];
    this.expressions = [];
  }

  async evaluate(expression) {
    this.expressions.push(expression);
    assert.ok(this.results.length > 0, "unexpected evaluate call");
    return structuredClone(this.results.shift());
  }
}

test("probe owns one observer and restores it exactly", async () => {
  const cdp = new FakeCdp([
    { installed: true, probe_id: "agent-interrupt-command", sequence: 0 },
    snapshot([{ command: "interrupt_agent", args: { expectedTarget: TARGET } }]),
    { removed: true, probe_id: "agent-interrupt-command", sequence: 1 },
  ]);
  const probe = new DesktopCommandProbe(cdp, {
    probeId: "agent-interrupt-command",
    commands: ["interrupt_agent"],
  });

  await probe.install();
  assert.equal(probe.installed, true);
  assert.equal((await probe.snapshot()).calls.length, 1);
  await probe.remove();
  assert.equal(probe.installed, false);
  assert.equal(cdp.expressions.length, 3);
  assert.match(cdp.expressions[0], /commands\.has\(observation\.name\)/);
  assert.match(cdp.expressions[2], /delete globalThis\[observerKey\]/);
  assert.doesNotMatch(cdp.expressions[2], /state\.calls\.push/);
});
