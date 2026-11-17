import assert from "node:assert/strict";
import test from "node:test";

import { renderMarkdown } from "../src/markdown.ts";

test("escaped pipes remain in a table cell and preserve the following result", () => {
  const html = renderMarkdown([
    "| チェック | 結果 |",
    "| :--- | ---: |",
    '| `echo "2 * 3" \\| python calculator.py` | `6`（exit 0） |',
  ].join("\n"));
  assert.match(html, /<td><code>echo &quot;2 \* 3&quot; \| python calculator\.py<\/code><\/td><td><code>6<\/code>（exit 0）<\/td>/);
});

test("table separators respect escaped final pipes, backslash pairs and empty cells", () => {
  const table = renderMarkdown([
    "Left | Middle | Right", "--- | --- | ---",
    String.raw`| a\\| | c\|`,
    String.raw`| a\\\|b | x | y |`,
    "- an item | with | pipes",
  ].join("\n"));
  assert.ok(table.includes(String.raw`<td>a\\</td><td></td><td>c|</td>`));
  assert.ok(table.includes(String.raw`<td>a\\|b</td><td>x</td><td>y</td>`));
  assert.match(table, /<\/table><\/div><ul><li>an item \| with \| pipes/);
  assert.match(renderMarkdown(["left | right", "--- | ---", String.raw`plain | end\|`].join("\n")),
    /<td>plain<\/td><td>end\|<\/td>/);
  assert.equal(renderMarkdown(String.raw`two \| literal \| pipes`), String.raw`<p>two \| literal \| pipes</p>`);
});

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

test("spaced thematic breaks render as dividers, not as list items", () => {
  for (const divider of ["- - -", "* * *", "_ _ _", "-  -  -  -", "***", "___"]) {
    const html = renderMarkdown(["前の段落", "", divider, "", "後の段落"].join("\n"));
    assert.match(html, /<hr class="md-divider">/, divider);
    assert.doesNotMatch(html, /<ul>|<li>/, divider);
  }
});

test("a list item whose text starts with markers stays a list item", () => {
  const html = renderMarkdown(["- - x", "- * 強調ではない"].join("\n"));
  assert.doesNotMatch(html, /md-divider/);
  assert.equal((html.match(/<li>/g) ?? []).length, 2);
});

test("a spaced divider after a list closes the list", () => {
  const html = renderMarkdown(["- 項目", "", "- - -", "", "本文"].join("\n"));
  assert.match(html, /<ul><li>項目<\/li><\/ul><hr class="md-divider"><p>本文<\/p>/);
});

test("a spaced divider inside a code block stays code", () => {
  const html = renderMarkdown(["```", "- - -", "```"].join("\n"));
  assert.doesNotMatch(html, /md-divider/);
  assert.match(html, /<pre class="md-code"><code>- - -<\/code><\/pre>/);
});
