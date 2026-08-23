import assert from "node:assert/strict";
import test from "node:test";

import { renderPlanProjection } from "../src/render.ts";
import type { DesktopWebState } from "../src/types.ts";

test("canonical plan projection stays separate from runtime activity", () => {
  const html = renderPlanProjection({
    plan: {
      explanation: "Inspect <owners> first",
      steps: [
        { step: "Read state", status: "completed" },
        { step: "Cut over UI", status: "in_progress" },
        { step: "Verify", status: "pending" },
      ],
    },
  } as DesktopWebState);

  assert.match(html, /aria-labelledby="output-plan-heading"/);
  assert.match(html, /<h3 id="output-plan-heading">計画<\/h3>/);
  assert.match(html, /Inspect &lt;owners&gt; first/);
  assert.match(html, /data-plan-status="completed"/);
  assert.match(html, /data-plan-status="in_progress"/);
  assert.match(html, /data-plan-status="pending"/);
  assert.match(html, /class="plan-step-status">進行中<\/span><span class="plan-step-copy">Cut over UI<\/span>/);
  assert.doesNotMatch(html, /class="activity"/);
});
