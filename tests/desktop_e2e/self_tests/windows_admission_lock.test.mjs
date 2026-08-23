import assert from "node:assert/strict";
import test from "node:test";

import { acquireDesktopAdmission } from "../drivers/windows_admission_lock.mjs";

test("Desktop admission is atomic and released for the next execution", { skip: process.platform !== "win32" }, async (context) => {
  const first = await acquireDesktopAdmission();
  context.after(() => first.release().catch(() => {}));
  await assert.rejects(
    () => acquireDesktopAdmission(),
    (error) => error.code === "desktop-e2e-admission-busy",
  );
  await first.release();
  const next = await acquireDesktopAdmission();
  await next.release();
  assert.equal(first.identity.acquired, true);
  assert.equal(next.identity.acquired, true);
});
