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
  assert.match(html, /<a href="https:\/\/example\.com\/result" data-markdown-web-link rel="noreferrer">検証結果<\/a>/);
  assert.doesNotMatch(html, /\*\*|<p>---<\/p>/);
});

test("markdown escapes HTML and never activates an unsafe link scheme", () => {
  const html = renderMarkdown("<script>alert(1)</script> [危険](javascript:alert(1))");

  assert.match(html, /&lt;script&gt;alert\(1\)&lt;\/script&gt;/);
  assert.match(html, /危険/);
  assert.doesNotMatch(html, /<script|href="javascript:/);
});

test("numbered lists keep their start, loose items and nested blocks in source order", () => {
  const html = renderMarkdown([
    "3. 最初", "", "4. 次", "   - 補足", "     - 詳細",
    "   ```sh", "   # コメント", "     echo '<ok>'", "   ```",
    "5. 最後", "", "- 別のリスト", "", "本文",
  ].join("\n"));
  assert.equal(html, '<ol start="3"><li>最初</li><li>次<ul><li>補足<ul><li>詳細</li></ul></li></ul>'
    + '<pre class="md-code"><code># コメント\n  echo &#039;&lt;ok&gt;&#039;</code></pre></li><li>最後</li></ol>'
    + '<ul><li>別のリスト</li></ul><p>本文</p>');
});

test("renders unfinished fenced code, deep headings and inline emphasis without interpreting code", () => {
  for (const level of [4, 5, 6]) {
    assert.match(renderMarkdown("#".repeat(level) + " 見出し"), /^<h[1-6]>見出し<\/h[1-6]>$/);
  }
  assert.equal(renderMarkdown("*強調* ~~旧~~ `*code* ~~old~~` 2 * 3 * 4"),
    "<p><em>強調</em> <del>旧</del> <code>*code* ~~old~~</code> 2 * 3 * 4</p>");
  assert.equal(renderMarkdown("- 手順\n  ```\n  # 未完"), '<ul><li>手順<pre class="md-code"><code># 未完</code></pre></li></ul>');
});
