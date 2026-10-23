import assert from "node:assert/strict";
import test from "node:test";

import { createStartupReadyNotification } from "../src/startup_ready.ts";

test("startup waits for a rendered frame before notifying and only reports once", () => {
  const frames: FrameRequestCallback[] = [];
  let notifications = 0;
  const rendered = createStartupReadyNotification(
    () => { notifications += 1; },
    (callback) => frames.push(callback),
  );
  assert.equal(notifications, 0);
  assert.equal(frames.length, 0);

  rendered();
  rendered();
  assert.equal(frames.length, 1);
  assert.equal(notifications, 0);

  frames.shift()!(0);
  assert.equal(frames.length, 1);
  assert.equal(notifications, 0);

  frames.shift()!(16);
  assert.equal(notifications, 1);
  assert.equal(frames.length, 0);
  rendered();
  assert.equal(notifications, 1);
  assert.equal(frames.length, 0);
});
