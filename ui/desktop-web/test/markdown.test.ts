import assert from "node:assert/strict";
import test from "node:test";

import { renderMarkdown } from "../src/markdown.ts";

test("markdown renders Codex-style summary structure without leaking source markers", () => {
  const html = renderMarkdown([
    "作業を完了しました。",
    "",
    "---",
    "",
    "- **`navigation_review`（alpha.txt）**",
    "- [検証結果](https://example.com/result)",
  ].join("\n"));

  assert.match(html, /<hr class="md-divider">/);
  assert.match(html, /<strong><code>navigation_review<\/code>（alpha\.txt）<\/strong>/);
  assert.match(html, /<a href="https:\/\/example\.com\/result" target="_blank" rel="noreferrer">検証結果<\/a>/);
  assert.doesNotMatch(html, /\*\*|<p>---<\/p>/);
});

test("markdown escapes HTML and never activates an unsafe link scheme", () => {
  const html = renderMarkdown("<script>alert(1)</script> [危険](javascript:alert(1))");

  assert.match(html, /&lt;script&gt;alert\(1\)&lt;\/script&gt;/);
  assert.match(html, /危険/);
  assert.doesNotMatch(html, /<script|href="javascript:/);
});
