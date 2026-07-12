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
  return value
    .split(/(`[^`]*`)/g)
    .map((part) => {
      if (part.startsWith("`") && part.endsWith("`")) {
        return `<code>${escapeHtml(part.slice(1, -1))}</code>`;
      }
      return escapeHtml(part)
        .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
        .replace(/__([^_]+)__/g, "<strong>$1</strong>");
    })
    .join("");
}
