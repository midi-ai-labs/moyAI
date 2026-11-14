import { escapeHtml } from "./utils.ts";

export function renderMarkdown(value: string): string {
  const lines = value.replace(/\r\n/g, "\n").split("\n");
  let html = "";
  let paragraph: string[] = [];
  const lists: { indent: number; tag: "ul" | "ol" }[] = [];
  let quoteLines: string[] = [];
  let tableLines: string[] = [];
  let codeLines: string[] = [];
  let fence: { indent: number; length: number } | null = null;

  const flushParagraph = () => {
    if (paragraph.length === 0) return;
    html += `<p>${renderInlineMarkdown(paragraph.join(" "))}</p>`;
    paragraph = [];
  };
  const closeList = () => { html += `</li></${lists.pop()!.tag}>`; };
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
    flushQuote();
  };

  for (const line of lines) {
    const trimmed = line.trim();
    const indent = line.length - line.trimStart().length;
    if (fence) {
      if (/^`{3,}$/.test(trimmed) && trimmed.length >= fence.length) {
        html += `<pre class="md-code"><code>${escapeHtml(codeLines.join("\n"))}</code></pre>`;
        codeLines = [];
        fence = null;
      } else {
        codeLines.push(line.slice(Math.min(indent, fence.indent)));
      }
      continue;
    }
    if (trimmed.length === 0) {
      flushTextBlocks();
      continue;
    }
    const item = trimmed.match(/^(?:([-*+])|(\d{1,9})[.)])\s+(.*)$/);
    if (item) {
      flushTextBlocks();
      const tag = item[1] ? "ul" : "ol";
      while (lists.length && lists.at(-1)!.indent > indent) closeList();
      if (lists.at(-1)?.indent === indent && lists.at(-1)!.tag !== tag) closeList();
      if (lists.at(-1)?.indent === indent) {
        html += "</li>";
      } else {
        const start = tag === "ol" && Number(item[2]) !== 1 ? ` start="${Number(item[2])}"` : "";
        html += `<${tag}${start}>`;
        lists.push({ indent, tag });
      }
      html += `<li>${renderInlineMarkdown(item[3])}`;
      continue;
    }
    if (lists.length && indent <= lists.at(-1)!.indent) {
      flushTextBlocks();
      while (lists.length && indent <= lists.at(-1)!.indent) closeList();
    }
    const opening = trimmed.match(/^(`{3,})[^`]*$/);
    if (opening) {
      flushTextBlocks();
      fence = { indent, length: opening[1].length };
      continue;
    }
    if (/^(?:-{3,}|\*{3,}|_{3,})$/.test(trimmed)) {
      flushTextBlocks();
      html += '<hr class="md-divider">';
      continue;
    }
    if (looksLikeTableLine(trimmed)) {
      flushParagraph();
      flushQuote();
      tableLines.push(trimmed);
      continue;
    }
    flushTable();
    const heading = trimmed.match(/^(#{1,6})\s+(.+)$/);
    if (heading) {
      flushTextBlocks();
      const level = Math.min(6, heading[1].length + 2);
      html += `<h${level}>${renderInlineMarkdown(heading[2])}</h${level}>`;
      continue;
    }
    const quote = trimmed.match(/^>\s?(.*)$/);
    if (quote) {
      flushParagraph();
      quoteLines.push(quote[1]);
      continue;
    }
    flushQuote();
    paragraph.push(lists.length ? trimmed : line);
  }

  if (fence) {
    html += `<pre class="md-code"><code>${escapeHtml(codeLines.join("\n"))}</code></pre>`;
  }
  flushTextBlocks();
  while (lists.length) closeList();
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
    } else if (token.kind === "link") {
      const href = safeMarkdownHref(token.href);
      html += href
        ? `<a href="${escapeHtml(href)}"${href.startsWith("#") ? "" : ' data-markdown-web-link rel="noreferrer"'}>${renderInlineMarkdown(token.content)}</a>`
        : renderInlineMarkdown(token.content);
    } else {
      html += `<${token.kind}>${renderInlineMarkdown(token.content)}</${token.kind}>`;
    }
    cursor = token.end;
  }
  return html;
}

type InlineToken =
  | { kind: "code"; start: number; end: number; content: string }
  | { kind: "strong" | "em" | "del"; start: number; end: number; content: string }
  | { kind: "link"; start: number; end: number; content: string; href: string };

function nextInlineToken(value: string, cursor: number): InlineToken | null {
  const candidates = [
    delimitedInlineToken(value, cursor, "`", "code"),
    delimitedInlineToken(value, cursor, "**", "strong"),
    delimitedInlineToken(value, cursor, "__", "strong"),
    matchedInlineToken(value, cursor, /(?<!\*)\*(?![\s*])([^*]+?)(?<!\s)\*(?!\*)/g, "em"),
    matchedInlineToken(value, cursor, /~~(?![\s~])([^~]+?)(?<!\s)~~/g, "del"),
    markdownLinkToken(value, cursor),
  ].filter((candidate): candidate is InlineToken => candidate !== null);
  candidates.sort((left, right) => left.start - right.start || inlineTokenPriority(left) - inlineTokenPriority(right));
  return candidates[0] ?? null;
}

function matchedInlineToken(value: string, cursor: number, expression: RegExp, kind: "em" | "del"): InlineToken | null {
  expression.lastIndex = cursor;
  const match = expression.exec(value);
  return match ? { kind, start: match.index, end: expression.lastIndex, content: match[1] } : null;
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
    return ["http:", "https:"].includes(parsed.protocol) && !parsed.username && !parsed.password ? trimmed : null;
  } catch {
    return null;
  }
}

function inlineTokenPriority(token: InlineToken): number {
  if (token.kind === "strong") return 0;
  if (token.kind === "code") return 1;
  return 2;
}
