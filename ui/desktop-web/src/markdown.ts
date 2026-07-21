import { escapeHtml } from "./utils.ts";

export function renderMarkdown(value: string): string {
  const lines = value.replace(/\r\n/g, "\n").split("\n");
  let html = "";
  let paragraph: string[] = [];
  let listItems: string[] = [];
  let orderedItems: string[] = [];
  let quoteLines: string[] = [];
  let tableLines: string[] = [];
  let codeLines: string[] = [];
  let inCode = false;

  const flushParagraph = () => {
    if (paragraph.length === 0) return;
    html += `<p>${renderInlineMarkdown(paragraph.join(" "))}</p>`;
    paragraph = [];
  };
  const flushList = () => {
    if (listItems.length > 0) {
      html += `<ul>${listItems.map((item) => `<li>${renderInlineMarkdown(item)}</li>`).join("")}</ul>`;
      listItems = [];
    }
    if (orderedItems.length > 0) {
      html += `<ol>${orderedItems.map((item) => `<li>${renderInlineMarkdown(item)}</li>`).join("")}</ol>`;
      orderedItems = [];
    }
  };
  const flushQuote = () => {
    if (quoteLines.length === 0) return;
    html += `<blockquote>${quoteLines.map((line) => `<p>${renderInlineMarkdown(line)}</p>`).join("")}</blockquote>`;
    quoteLines = [];
  };
  const flushTable = () => {
    if (tableLines.length < 2) {
      if (tableLines.length > 0) paragraph.push(...tableLines);
      tableLines = [];
      return;
    }
    const rows = parseTableRows(tableLines);
    tableLines = [];
    if (rows.length < 2 || !isAlignmentRow(rows[1])) {
      paragraph.push(...rows.map((row) => `| ${row.join(" | ")} |`));
      return;
    }
    const headers = rows[0];
    const bodyRows = rows.slice(2);
    html += `<div class="md-table-wrap"><table class="md-table"><thead><tr>${headers
      .map((cell) => `<th>${renderInlineMarkdown(cell)}</th>`)
      .join("")}</tr></thead><tbody>${bodyRows
      .map(
        (row) =>
          `<tr>${headers
            .map((_, index) => `<td>${renderInlineMarkdown(row[index] ?? "")}</td>`)
            .join("")}</tr>`
      )
      .join("")}</tbody></table></div>`;
  };
  const flushTextBlocks = () => {
    flushTable();
    flushParagraph();
    flushList();
    flushQuote();
  };

  for (const line of lines) {
    const fence = line.match(/^```/);
    if (fence) {
      if (inCode) {
        html += `<pre class="md-code"><code>${escapeHtml(codeLines.join("\n"))}</code></pre>`;
        codeLines = [];
        inCode = false;
      } else {
        flushTextBlocks();
        inCode = true;
      }
      continue;
    }
    if (inCode) {
      codeLines.push(line);
      continue;
    }
    const trimmed = line.trim();
    if (trimmed.length === 0) {
      flushTextBlocks();
      continue;
    }
    if (/^(?:-{3,}|\*{3,}|_{3,})$/.test(trimmed)) {
      flushTextBlocks();
      html += '<hr class="md-divider">';
      continue;
    }
    if (looksLikeTableLine(trimmed)) {
      flushParagraph();
      flushList();
      flushQuote();
      tableLines.push(trimmed);
      continue;
    }
    flushTable();
    const heading = trimmed.match(/^(#{1,3})\s+(.+)$/);
    if (heading) {
      flushTextBlocks();
      const level = heading[1].length + 2;
      html += `<h${level}>${renderInlineMarkdown(heading[2])}</h${level}>`;
      continue;
    }
    const bullet = trimmed.match(/^[-*]\s+(.+)$/);
    if (bullet) {
      flushParagraph();
      flushQuote();
      listItems.push(bullet[1]);
      continue;
    }
    const ordered = trimmed.match(/^\d+[.)]\s+(.+)$/);
    if (ordered) {
      flushParagraph();
      flushQuote();
      orderedItems.push(ordered[1]);
      continue;
    }
    const quote = trimmed.match(/^>\s?(.*)$/);
    if (quote) {
      flushParagraph();
      flushList();
      quoteLines.push(quote[1]);
      continue;
    }
    flushList();
    flushQuote();
    paragraph.push(line);
  }

  if (inCode) {
    html += `<pre class="md-code"><code>${escapeHtml(codeLines.join("\n"))}</code></pre>`;
  }
  flushTextBlocks();
  return html || `<p>${escapeHtml(value)}</p>`;
}

function looksLikeTableLine(line: string): boolean {
  return line.includes("|") && line.split("|").length >= 3;
}

function parseTableRows(lines: string[]): string[][] {
  return lines.map((line) => {
    const trimmed = line.trim().replace(/^\|/, "").replace(/\|$/, "");
    return trimmed.split("|").map((cell) => cell.trim());
  });
}

function isAlignmentRow(row: string[]): boolean {
  return row.length > 0 && row.every((cell) => /^:?-{3,}:?$/.test(cell.trim()));
}

function renderInlineMarkdown(value: string): string {
  let cursor = 0;
  let html = "";
  while (cursor < value.length) {
    const token = nextInlineToken(value, cursor);
    if (!token) {
      html += escapeHtml(value.slice(cursor));
      break;
    }
    html += escapeHtml(value.slice(cursor, token.start));
    if (token.kind === "code") {
      html += `<code>${escapeHtml(token.content)}</code>`;
    } else if (token.kind === "strong") {
      html += `<strong>${renderInlineMarkdown(token.content)}</strong>`;
    } else {
      const href = safeMarkdownHref(token.href);
      html += href
        ? `<a href="${escapeHtml(href)}" target="_blank" rel="noreferrer">${renderInlineMarkdown(token.content)}</a>`
        : renderInlineMarkdown(token.content);
    }
    cursor = token.end;
  }
  return html;
}

type InlineToken =
  | { kind: "code"; start: number; end: number; content: string }
  | { kind: "strong"; start: number; end: number; content: string }
  | { kind: "link"; start: number; end: number; content: string; href: string };

function nextInlineToken(value: string, cursor: number): InlineToken | null {
  const candidates = [
    delimitedInlineToken(value, cursor, "`", "code"),
    delimitedInlineToken(value, cursor, "**", "strong"),
    delimitedInlineToken(value, cursor, "__", "strong"),
    markdownLinkToken(value, cursor),
  ].filter((candidate): candidate is InlineToken => candidate !== null);
  candidates.sort((left, right) => left.start - right.start || inlineTokenPriority(left) - inlineTokenPriority(right));
  return candidates[0] ?? null;
}

function delimitedInlineToken(
  value: string,
  cursor: number,
  delimiter: string,
  kind: "code" | "strong",
): InlineToken | null {
  let start = value.indexOf(delimiter, cursor);
  while (start >= 0) {
    const end = value.indexOf(delimiter, start + delimiter.length);
    if (end >= 0) {
      return {
        kind,
        start,
        end: end + delimiter.length,
        content: value.slice(start + delimiter.length, end),
      };
    }
    start = value.indexOf(delimiter, start + delimiter.length);
  }
  return null;
}

function markdownLinkToken(value: string, cursor: number): InlineToken | null {
  const expression = /\[([^\]]+)\]\(([^)\s]+)\)/g;
  expression.lastIndex = cursor;
  const match = expression.exec(value);
  if (!match || match.index > 0 && value[match.index - 1] === "!") return null;
  return {
    kind: "link",
    start: match.index,
    end: match.index + match[0].length,
    content: match[1],
    href: match[2],
  };
}

function safeMarkdownHref(value: string): string | null {
  const trimmed = value.trim();
  if (trimmed.startsWith("#")) return trimmed;
  try {
    const parsed = new URL(trimmed);
    return ["http:", "https:", "mailto:"].includes(parsed.protocol) ? trimmed : null;
  } catch {
    return null;
  }
}

function inlineTokenPriority(token: InlineToken): number {
  if (token.kind === "strong") return 0;
  if (token.kind === "code") return 1;
  return 2;
}
