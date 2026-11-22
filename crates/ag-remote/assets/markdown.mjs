// Markdown for assistant answers, notices and question text. Model output
// is untrusted, so the module has two halves that never meet a markup
// parser:
//
// - `parseMarkdown(text)` and `parseInlineMarkdown(text)` return a tree of
//   plain objects and touch no DOM.
// - `renderMarkdown(tree, document, options)` builds that tree with
//   `createElement`, `createTextNode` and `append`. Tag names come from
//   `TAGS`, never from the input; text only ever becomes text nodes; the
//   only attributes set are `href`, `rel`, `target`, `class`, `start`,
//   `type` and `aria-*`.
//
// Raw HTML in the input stays text. Links keep only the `http:`, `https:`
// and `mailto:` schemes. The parser works in linear passes, without
// backtracking regular expressions, and nesting deeper than
// `MAX_NESTING` levels stays text, so a hostile answer cannot freeze the
// page. The subset matches what the TUI renders; see
// crates/ag-remote/tests/web/markdown.test.mjs for each construct.

// Longest text that is parsed. A longer message renders as plain text.
export const MAX_MARKDOWN_LENGTH = 256 * 1024;
// Deepest nesting of block quotes and lists, and separately of emphasis
// and links. Deeper content stays text.
export const MAX_NESTING = 16;
// Widest table. A wider header row stays a paragraph, so a short input
// cannot ask for millions of empty cells.
export const MAX_TABLE_COLUMNS = 64;
// How long the copy button says `copied`.
const COPIED_MS = 1500;
// Longest link destination and title read after `](`.
const MAX_DESTINATION_LENGTH = 2048;

// Every element the renderer creates, by the part it plays. Nothing else
// reaches `createElement`.
const TAGS = Object.freeze({
  blockquote: "blockquote",
  break: "br",
  code: "code",
  codeBlock: "div",
  codeHead: "div",
  codeLabel: "span",
  copyButton: "button",
  emphasis: "em",
  heading1: "h1",
  heading2: "h2",
  heading3: "h3",
  link: "a",
  listItem: "li",
  orderedList: "ol",
  paragraph: "p",
  plain: "div",
  pre: "pre",
  rule: "hr",
  strike: "del",
  strong: "strong",
  table: "table",
  tableBody: "tbody",
  tableCell: "td",
  tableHead: "thead",
  tableHeader: "th",
  tableRow: "tr",
  tableScroll: "div",
  task: "span",
  unlinked: "span",
  unorderedList: "ul",
});

// The tag names `renderMarkdown` may create.
export const MARKDOWN_TAGS = Object.freeze(new Set(Object.values(TAGS)));

const SAFE_PROTOCOLS = new Set(["http:", "https:", "mailto:"]);
const ASCII_PUNCTUATION = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";
const UNICODE_PUNCTUATION = /[\p{P}\p{S}]/u;
const WHITESPACE = /\s/;
// Characters a bare URL does not end with.
const URL_TRAILING = ".,:;!?*_~'\"";

// The address `value` links to, or `null` when it is not an absolute
// `http:`, `https:` or `mailto:` URL. Relative paths, `javascript:`,
// `data:` and every other scheme return `null`.
export function safeLink(value) {
  let url;
  try {
    url = new URL(String(value ?? "").trim());
  } catch (_error) {
    return null;
  }

  return SAFE_PROTOCOLS.has(url.protocol) ? url.href : null;
}

// Parses `text` into `{ type: "document", children }` of block nodes. A
// text longer than `MAX_MARKDOWN_LENGTH` becomes one `plain` node.
export function parseMarkdown(text) {
  const source = String(text ?? "");
  if (source.length > MAX_MARKDOWN_LENGTH) {
    return { children: [{ text: source, type: "plain" }], type: "document" };
  }

  return { children: parseBlocks(splitLines(source), 0), type: "document" };
}

// Parses `text` as one run of inline content, as notices and questions
// use: `{ type: "inline", children }`. Block syntax stays text.
export function parseInlineMarkdown(text) {
  const source = String(text ?? "");
  if (source.length > MAX_MARKDOWN_LENGTH) {
    return { children: [{ text: source, type: "text" }], type: "inline" };
  }

  return { children: parseInline(source), type: "inline" };
}

// Builds `tree` into a document fragment of `document`. `window`, when
// given, supplies the clipboard for the copy buttons; without a clipboard
// the code blocks have none.
export function renderMarkdown(tree, document, { window = null } = {}) {
  const clipboard = window?.navigator?.clipboard;
  const context = {
    clipboard: typeof clipboard?.writeText === "function" ? clipboard : null,
    document: document,
    window: window,
  };
  const fragment = document.createDocumentFragment();
  for (const node of tree.children) {
    fragment.append(renderNode(node, context, false));
  }

  return fragment;
}

// ---- Blocks ----------------------------------------------------------

function splitLines(text) {
  return text.split(/\r\n|\r|\n/).map(expandLeadingTabs);
}

// Replaces tabs in the indentation with spaces to the next multiple of
// four, so indentation is counted in spaces.
function expandLeadingTabs(line) {
  if (!line.includes("\t")) {
    return line;
  }
  let column = 0;
  let index = 0;
  while (index < line.length && (line[index] === " " || line[index] === "\t")) {
    column = line[index] === "\t" ? column + 4 - (column % 4) : column + 1;
    index += 1;
  }

  return " ".repeat(column) + line.slice(index);
}

function leadingSpaces(line) {
  let count = 0;
  while (line[count] === " ") {
    count += 1;
  }

  return count;
}

function isBlank(line) {
  return line.trim() === "";
}

function removeIndent(line, count) {
  return line.slice(Math.min(count, leadingSpaces(line)));
}

function parseBlocks(lines, depth) {
  const blocks = [];
  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
    if (isBlank(line)) {
      index += 1;
      continue;
    }
    if (depth >= MAX_NESTING) {
      // Too deep: whatever is left is one paragraph of text.
      blocks.push(paragraph(lines.slice(index).filter((rest) => !isBlank(rest))));
      break;
    }
    const fence = fenceOpening(line);
    if (fence) {
      index = parseFence(lines, index, fence, blocks);
      continue;
    }
    const heading = headingOf(line);
    if (heading) {
      blocks.push({
        children: parseInline(heading.text),
        level: heading.level,
        type: "heading",
      });
      index += 1;
      continue;
    }
    if (isThematicBreak(line)) {
      blocks.push({ type: "rule" });
      index += 1;
      continue;
    }
    if (isQuoteLine(line)) {
      index = parseQuote(lines, index, depth, blocks);
      continue;
    }
    const marker = listMarker(line);
    if (marker) {
      index = parseList(lines, index, marker, depth, blocks);
      continue;
    }
    if (tableColumns(lines, index)) {
      index = parseTable(lines, index, blocks);
      continue;
    }
    index = parseParagraph(lines, index, blocks);
  }

  return blocks;
}

// Whether `line`, inside a paragraph, starts a different block.
function interruptsParagraph(lines, index) {
  const line = lines[index];
  if (fenceOpening(line) || headingOf(line) || isThematicBreak(line) || isQuoteLine(line)) {
    return true;
  }
  const marker = listMarker(line);
  if (marker && !marker.isEmpty && (!marker.ordered || marker.start === 1)) {
    return true;
  }

  return Boolean(tableColumns(lines, index));
}

function parseParagraph(lines, start, blocks) {
  const collected = [lines[start]];
  let index = start + 1;
  while (index < lines.length && !isBlank(lines[index]) && !interruptsParagraph(lines, index)) {
    collected.push(lines[index]);
    index += 1;
  }
  blocks.push(paragraph(collected));

  return index;
}

// A paragraph of `lines`. Each line break stays a break, as in GitHub
// comments; a trailing backslash or two spaces, the CommonMark hard
// break, mean the same.
function paragraph(lines) {
  const text = lines
    .map((line) => {
      const trimmed = line.trim();

      return trimmed.endsWith("\\") && !trimmed.endsWith("\\\\") ? trimmed.slice(0, -1) : trimmed;
    })
    .join("\n");

  return { children: parseInline(text), type: "paragraph" };
}

function headingOf(line) {
  const indent = leadingSpaces(line);
  if (indent > 3) {
    return null;
  }
  let end = indent;
  while (line[end] === "#" && end - indent < 7) {
    end += 1;
  }
  const level = end - indent;
  if (level < 1 || level > 6 || (end < line.length && line[end] !== " ")) {
    return null;
  }
  let text = line.slice(end).trim();
  // An optional closing run of `#`, after a space.
  let close = text.length;
  while (close > 0 && text[close - 1] === "#") {
    close -= 1;
  }
  if (close === 0) {
    text = "";
  } else if (close < text.length && text[close - 1] === " ") {
    text = text.slice(0, close).trimEnd();
  }

  return { level: level, text: text };
}

function isThematicBreak(line) {
  if (leadingSpaces(line) > 3) {
    return false;
  }
  const text = line.trim();
  const mark = text[0];
  if (mark !== "*" && mark !== "-" && mark !== "_") {
    return false;
  }
  let count = 0;
  for (const character of text) {
    if (character === mark) {
      count += 1;
    } else if (character !== " ") {
      return false;
    }
  }

  return count >= 3;
}

function fenceOpening(line) {
  const indent = leadingSpaces(line);
  const mark = line[indent];
  if (indent > 3 || (mark !== "`" && mark !== "~")) {
    return null;
  }
  let end = indent;
  while (line[end] === mark) {
    end += 1;
  }
  const length = end - indent;
  const info = line.slice(end).trim();
  if (length < 3 || (mark === "`" && info.includes("`"))) {
    return null;
  }

  return { indent: indent, info: info, length: length, mark: mark };
}

function isFenceClosing(line, fence) {
  const indent = leadingSpaces(line);
  if (indent > 3) {
    return false;
  }
  let end = indent;
  while (line[end] === fence.mark) {
    end += 1;
  }

  return end - indent >= fence.length && line.slice(end).trim() === "";
}

// A fenced code block. An unclosed fence runs to the end of its
// container.
function parseFence(lines, start, fence, blocks) {
  const content = [];
  let index = start + 1;
  while (index < lines.length && !isFenceClosing(lines[index], fence)) {
    content.push(removeIndent(lines[index], fence.indent));
    index += 1;
  }
  blocks.push({
    language: fence.info.split(/\s/)[0],
    text: content.join("\n"),
    type: "code_block",
  });

  return index + 1;
}

function isQuoteLine(line) {
  const indent = leadingSpaces(line);

  return indent <= 3 && line[indent] === ">";
}

function parseQuote(lines, start, depth, blocks) {
  const inner = [];
  let index = start;
  while (index < lines.length && isQuoteLine(lines[index])) {
    const line = lines[index];
    const after = leadingSpaces(line) + 1;
    inner.push(line.slice(line[after] === " " ? after + 1 : after));
    index += 1;
  }
  blocks.push({ children: parseBlocks(inner, depth + 1), type: "blockquote" });

  return index;
}

// The list marker at the start of `line`: its kind, the column its
// content starts at, and for ordered lists the number.
function listMarker(line) {
  const indent = leadingSpaces(line);
  if (indent > 3) {
    return null;
  }
  let end = indent;
  let ordered = false;
  let start = 1;
  let delimiter = line[indent];
  if (delimiter === "-" || delimiter === "*" || delimiter === "+") {
    end += 1;
  } else {
    while (end - indent < 9 && line[end] >= "0" && line[end] <= "9") {
      end += 1;
    }
    delimiter = line[end];
    if (end === indent || (delimiter !== "." && delimiter !== ")")) {
      return null;
    }
    ordered = true;
    start = Number(line.slice(indent, end));
    end += 1;
  }
  if (end < line.length && line[end] !== " ") {
    return null;
  }
  const spaces = leadingSpaces(line.slice(end));
  const isEmpty = end + spaces >= line.length;
  // Five or more spaces after the marker would be indented code, which
  // this renderer does not have; the content then starts after one.
  const gap = isEmpty || spaces > 4 ? 1 : spaces;

  return {
    content: line.slice(end + Math.min(gap, spaces)),
    contentIndent: end + gap,
    delimiter: delimiter,
    isEmpty: isEmpty,
    ordered: ordered,
    start: start,
  };
}

function sameList(marker, other) {
  return other !== null && other.ordered === marker.ordered && other.delimiter === marker.delimiter;
}

function parseList(lines, start, first, depth, blocks) {
  const items = [];
  let isLoose = false;
  let index = start;
  let marker = first;
  while (marker) {
    const content = [marker.content];
    let sawBlank = false;
    index += 1;
    while (index < lines.length) {
      const line = lines[index];
      if (isBlank(line)) {
        sawBlank = true;
        content.push("");
      } else if (leadingSpaces(line) >= marker.contentIndent) {
        // A blank line between two blocks of one item loosens the list.
        isLoose = isLoose || sawBlank;
        content.push(line.slice(marker.contentIndent));
        sawBlank = false;
      } else if (!sawBlank && !interruptsParagraph(lines, index) && !listMarker(line)) {
        // A lazy continuation of the item's paragraph.
        content.push(line.trim());
      } else {
        break;
      }
      index += 1;
    }
    while (content.length > 1 && content.at(-1) === "") {
      content.pop();
    }
    items.push(listItem(content, depth));
    const next = index < lines.length ? listMarker(lines[index]) : null;
    if (!sameList(first, next)) {
      break;
    }
    isLoose = isLoose || sawBlank;
    marker = next;
  }
  blocks.push({
    children: items,
    loose: isLoose,
    ordered: first.ordered,
    start: first.start,
    type: "list",
  });

  return index;
}

function listItem(content, depth) {
  let task = null;
  const head = content[0];
  if (/^\[[ xX]\](?: |$)/.test(head)) {
    task = head[1] !== " ";
    content[0] = head.slice(4);
  }

  return { children: parseBlocks(content, depth + 1), task: task, type: "list_item" };
}

// ---- Tables ----------------------------------------------------------

// Splits a table row into its cells. `\|` is a pipe inside a cell.
function tableCells(line) {
  let text = line.trim();
  if (text.startsWith("|")) {
    text = text.slice(1);
  }
  if (text.endsWith("|") && !text.endsWith("\\|")) {
    text = text.slice(0, -1);
  }
  const cells = [];
  let cell = "";
  for (let index = 0; index < text.length; index += 1) {
    const character = text[index];
    if (character === "\\" && text[index + 1] === "|") {
      cell += "|";
      index += 1;
    } else if (character === "|") {
      cells.push(cell.trim());
      cell = "";
    } else {
      cell += character;
    }
  }
  cells.push(cell.trim());

  return cells;
}

function alignmentOf(cell) {
  if (cell.length === 0) {
    return undefined;
  }
  const left = cell.startsWith(":");
  const right = cell.endsWith(":");
  const dashes = cell.slice(left ? 1 : 0, right ? -1 : undefined);
  if (dashes.length === 0 || [...dashes].some((character) => character !== "-")) {
    return undefined;
  }
  if (left && right) {
    return "center";
  }

  return right ? "right" : left ? "left" : null;
}

// The alignments of the table starting at `lines[index]`, or `null` when
// no table starts there: a header row with a pipe, then a delimiter row
// with as many cells.
function tableColumns(lines, index) {
  const header = lines[index];
  const delimiter = lines[index + 1];
  if (delimiter === undefined || !header.includes("|") || !delimiter.includes("-")) {
    return null;
  }
  if (leadingSpaces(header) > 3 || leadingSpaces(delimiter) > 3) {
    return null;
  }
  const alignments = tableCells(delimiter).map(alignmentOf);
  if (alignments.length > MAX_TABLE_COLUMNS || alignments.includes(undefined)) {
    return null;
  }

  return tableCells(header).length === alignments.length ? alignments : null;
}

function parseTable(lines, start, blocks) {
  const alignments = tableColumns(lines, start);
  const row = (line) => {
    const cells = tableCells(line);

    // A row with fewer cells gets empty ones; extra cells are dropped.
    return alignments.map((_alignment, column) => parseInline(cells[column] ?? ""));
  };
  const header = row(lines[start]);
  const rows = [];
  let index = start + 2;
  while (index < lines.length && !isBlank(lines[index]) && !interruptsParagraph(lines, index)) {
    rows.push(row(lines[index]));
    index += 1;
  }
  blocks.push({ alignments: alignments, header: header, rows: rows, type: "table" });

  return index;
}

// ---- Inline ----------------------------------------------------------

function isWhitespace(character) {
  return (
    character === undefined || character <= " " || (character > "~" && WHITESPACE.test(character))
  );
}

function isPunctuation(character) {
  return character !== undefined && UNICODE_PUNCTUATION.test(character);
}

function isAsciiAlphanumeric(character) {
  return character !== undefined && /[A-Za-z0-9]/.test(character);
}

// Start positions of every run of backticks, by run length, so a code
// span finds its closing run without searching the text again.
function backtickRuns(text) {
  const runs = new Map();
  let index = text.indexOf("`");
  while (index !== -1) {
    let end = index;
    while (text[end] === "`") {
      end += 1;
    }
    const length = end - index;
    if (!runs.has(length)) {
      runs.set(length, { next: 0, positions: [] });
    }
    runs.get(length).positions.push(index);
    index = text.indexOf("`", end);
  }

  return runs;
}

function closingRun(runs, length, after) {
  const run = runs.get(length);
  if (!run) {
    return -1;
  }
  while (run.next < run.positions.length && run.positions[run.next] <= after) {
    run.next += 1;
  }

  return run.next < run.positions.length ? run.positions[run.next] : -1;
}

// The index of `character` in `text` from `start` and before `limit`, or
// -1.
function boundedIndexOf(text, character, start, limit) {
  const found = text.slice(start, limit).indexOf(character);

  return found === -1 ? -1 : start + found;
}

// Reads the `(destination "title")` after a link's `]` at `open`, the
// index of `(`. Returns the destination and the index after `)`, or
// `null`.
function linkDestination(text, open) {
  const limit = Math.min(text.length, open + MAX_DESTINATION_LENGTH);
  let index = open + 1;
  const skipSpaces = () => {
    while (index < limit && (text[index] === " " || text[index] === "\n")) {
      index += 1;
    }
  };
  skipSpaces();
  let destination = "";
  if (text[index] === "<") {
    const end = boundedIndexOf(text, ">", index, limit);
    if (end === -1 || text.slice(index, end).includes("\n")) {
      return null;
    }
    destination = text.slice(index + 1, end);
    index = end + 1;
  } else {
    let parentheses = 0;
    const begin = index;
    while (index < limit && !isWhitespace(text[index])) {
      const character = text[index];
      if (character === "\\" && index + 1 < limit) {
        index += 2;
        continue;
      }
      if (character === "(") {
        parentheses += 1;
      } else if (character === ")") {
        if (parentheses === 0) {
          break;
        }
        parentheses -= 1;
      }
      index += 1;
    }
    destination = text.slice(begin, index);
  }
  skipSpaces();
  const quote = text[index];
  if (quote === '"' || quote === "'") {
    const end = boundedIndexOf(text, quote, index + 1, limit);
    if (end === -1) {
      return null;
    }
    index = end + 1;
    skipSpaces();
  }

  return text[index] === ")" ? { destination: destination, end: index + 1 } : null;
}

// An autolink `<scheme:…>` or `<name@host>` starting at `index`. Returns
// the address and the index after `>`, or `null`.
function autolink(text, index) {
  let end = index + 1;
  while (end < text.length && text[end] !== ">" && text[end] !== "<" && !isWhitespace(text[end])) {
    end += 1;
  }
  if (text[end] !== ">") {
    return null;
  }
  const inner = text.slice(index + 1, end);
  const isEmail = /^[\w.+-]+@[\w-]+(?:\.[\w-]+)+$/.test(inner) && !inner.includes(":");
  const href = safeLink(isEmail ? "mailto:" + inner : inner);

  return href && (isEmail || /^[A-Za-z][A-Za-z0-9+.-]{1,31}:/.test(inner))
    ? { end: end + 1, href: href, text: inner }
    : null;
}

// A bare `http:` or `https:` URL starting at `index`, or `null`. Inside
// a link text, `]` ends it. A URL longer than `MAX_DESTINATION_LENGTH`
// is not linked, so a failed attempt never reads far.
function bareUrl(text, index, isInBrackets) {
  if (!text.startsWith("http", index) || isAsciiAlphanumeric(text[index - 1])) {
    return null;
  }
  let scheme = index + 4;
  if (text[scheme] === "s") {
    scheme += 1;
  }
  if (!text.startsWith("://", scheme)) {
    return null;
  }
  const limit = index + MAX_DESTINATION_LENGTH;
  let end = scheme + 3;
  while (
    end < text.length &&
    !isWhitespace(text[end]) &&
    text[end] !== "<" &&
    !(isInBrackets && text[end] === "]")
  ) {
    end += 1;
    if (end > limit) {
      return null;
    }
  }
  // Trailing punctuation belongs to the sentence, and so does a closing
  // parenthesis the URL did not open.
  let opened = 0;
  for (let position = index; position < end; position += 1) {
    opened += text[position] === "(" ? 1 : text[position] === ")" ? -1 : 0;
  }
  while (end > scheme + 3) {
    const last = text[end - 1];
    if (URL_TRAILING.includes(last)) {
      end -= 1;
    } else if (last === ")" && opened < 0) {
      end -= 1;
      opened += 1;
    } else {
      break;
    }
  }
  const raw = text.slice(index, end);
  const href = end > scheme + 3 ? safeLink(raw) : null;

  return href ? { end: end, href: href, text: raw } : null;
}

// A delimiter run of `*`, `_` or `~~` at `index`, with whether it can
// open or close emphasis by the CommonMark flanking rules.
function delimiterRun(text, index) {
  const mark = text[index];
  let end = index;
  while (text[end] === mark) {
    end += 1;
  }
  const count = end - index;
  const before = text[index - 1];
  const after = text[end];
  const leftFlanking =
    !isWhitespace(after) && (!isPunctuation(after) || isWhitespace(before) || isPunctuation(before));
  const rightFlanking =
    !isWhitespace(before) && (!isPunctuation(before) || isWhitespace(after) || isPunctuation(after));
  let canOpen = leftFlanking;
  let canClose = rightFlanking;
  if (mark === "_") {
    canOpen = leftFlanking && (!rightFlanking || isPunctuation(before));
    canClose = rightFlanking && (!leftFlanking || isPunctuation(after));
  }
  if (mark === "~" && count !== 2) {
    canOpen = false;
    canClose = false;
  }

  return { canClose: canClose, canOpen: canOpen, count: count, mark: mark, type: "delimiter" };
}

function parseInline(text) {
  const runs = backtickRuns(text);
  // Open `[` and `![`, innermost last, each with the tokens after it.
  const frames = [{ tokens: [] }];
  let buffer = "";
  const tokens = () => frames.at(-1).tokens;
  const flush = () => {
    if (buffer) {
      tokens().push({ text: buffer, type: "text" });
      buffer = "";
    }
  };
  const push = (token) => {
    flush();
    tokens().push(token);
  };
  // Returns an unclosed or unlinked bracket's tokens to the frame around
  // it, after its opening bracket as text.
  const reopen = (frame) => {
    const outer = tokens();
    outer.push({ text: frame.isImage ? "![" : "[", type: "text" });
    for (const token of frame.tokens) {
      outer.push(token);
    }
  };

  let index = 0;
  while (index < text.length) {
    const character = text[index];
    if (character === "\\" && ASCII_PUNCTUATION.includes(text[index + 1] ?? "")) {
      buffer += text[index + 1];
      index += 2;
    } else if (character === "\n") {
      push({ type: "break" });
      index += 1;
    } else if (character === "`") {
      let end = index;
      while (text[end] === "`") {
        end += 1;
      }
      const length = end - index;
      const close = closingRun(runs, length, index);
      if (close === -1) {
        buffer += text.slice(index, end);
        index = end;
      } else {
        push({ text: codeSpanText(text.slice(end, close)), type: "code" });
        index = close + length;
      }
    } else if (character === "*" || character === "_" || character === "~") {
      const run = delimiterRun(text, index);
      if (run.canOpen || run.canClose) {
        push(run);
      } else {
        buffer += character.repeat(run.count);
      }
      index += run.count;
    } else if (character === "[" || (character === "!" && text[index + 1] === "[")) {
      if (frames.length > MAX_NESTING) {
        buffer += character;
        index += 1;
        continue;
      }
      flush();
      const isImage = character === "!";
      frames.push({ isImage: isImage, tokens: [] });
      index += isImage ? 2 : 1;
    } else if (character === "]" && frames.length > 1) {
      flush();
      const frame = frames.pop();
      const target = text[index + 1] === "(" ? linkDestination(text, index + 1) : null;
      const href = target ? safeLink(target.destination) : null;
      const content = href ? processEmphasis(frame.tokens) : null;
      if (content && content.depth < MAX_NESTING) {
        tokens().push(
          frame.isImage
            ? { alt: plainText(content.nodes), depth: 1, href: href, type: "image" }
            : { children: content.nodes, depth: content.depth + 1, href: href, type: "link" },
        );
        index = target.end;
      } else {
        // Not a link: the brackets are text and the inside is parsed
        // with what surrounds it. An unsafe destination stays text too.
        reopen(frame);
        buffer = "]";
        index += 1;
      }
    } else if (character === "<") {
      const link = autolink(text, index);
      if (link) {
        push({ children: [{ text: link.text, type: "text" }], depth: 1, href: link.href, type: "link" });
        index = link.end;
      } else {
        buffer += character;
        index += 1;
      }
    } else if (character === "h") {
      const link = bareUrl(text, index, frames.length > 1);
      if (link) {
        push({ children: [{ text: link.text, type: "text" }], depth: 1, href: link.href, type: "link" });
        index = link.end;
      } else {
        buffer += character;
        index += 1;
      }
    } else {
      buffer += character;
      index += 1;
    }
  }
  flush();
  // Brackets left open are text.
  while (frames.length > 1) {
    const frame = frames.pop();
    reopen(frame);
  }

  return mergeText(processEmphasis(frames[0].tokens).nodes);
}

// The inside of a code span: line breaks become spaces, and one space on
// each side is dropped when both are there.
function codeSpanText(raw) {
  const text = raw.replaceAll("\n", " ");
  if (text.length > 1 && text.startsWith(" ") && text.endsWith(" ") && text.trim() !== "") {
    return text.slice(1, -1);
  }

  return text;
}

// Pairs delimiter runs into emphasis, strong and strikethrough nodes, as
// the CommonMark "process emphasis" step does, in one pass: each closer
// takes the nearest opener of its mark, and the openers between them
// become text. Returns the nodes and how deeply they nest; a pair that
// would nest deeper than `MAX_NESTING` stays text.
function processEmphasis(tokens) {
  const out = [];
  // Open delimiter runs, innermost last, each with where its text sits
  // in `out` and the deepest node after it.
  const stack = [];
  const openers = { "*": 0, _: 0, "~": 0 };
  let outerDepth = 0;
  const note = (depth) => {
    if (stack.length > 0) {
      stack.at(-1).depth = Math.max(stack.at(-1).depth, depth);
    } else {
      outerDepth = Math.max(outerDepth, depth);
    }
  };
  const drop = () => {
    const entry = stack.pop();
    openers[entry.mark] -= 1;
    note(entry.depth);

    return entry;
  };

  for (const token of tokens) {
    if (token.type !== "delimiter") {
      out.push(token);
      note(token.depth ?? 0);
      continue;
    }
    let remaining = token.count;
    while (token.canClose && remaining > 0 && openers[token.mark] > 0) {
      while (stack.at(-1).mark !== token.mark) {
        drop();
      }
      const opener = stack.at(-1);
      if (opener.depth + 1 > MAX_NESTING) {
        drop();
        break;
      }
      const used = token.mark === "~" ? 2 : remaining >= 2 && opener.remaining >= 2 ? 2 : 1;
      const children = mergeText(out.splice(opener.index + 1));
      const depth = opener.depth + 1;
      opener.remaining -= used;
      opener.text.text = token.mark.repeat(opener.remaining);
      opener.depth = 0;
      remaining -= used;
      if (opener.remaining === 0) {
        out.pop();
        stack.pop();
        openers[token.mark] -= 1;
      }
      const type = used === 1 ? "emphasis" : token.mark === "~" ? "strike" : "strong";
      out.push({ children: children, depth: depth, type: type });
      note(depth);
    }
    if (remaining === 0) {
      continue;
    }
    const text = { text: token.mark.repeat(remaining), type: "text" };
    out.push(text);
    if (token.canOpen) {
      stack.push({ depth: 0, index: out.length - 1, mark: token.mark, remaining: remaining, text: text });
      openers[token.mark] += 1;
    }
  }
  const depth = stack.reduce((deepest, entry) => Math.max(deepest, entry.depth), outerDepth);

  return { depth: depth, nodes: out.filter((node) => node.type !== "text" || node.text !== "") };
}

// Joins neighbouring text nodes.
function mergeText(nodes) {
  const merged = [];
  for (const node of nodes) {
    const last = merged.at(-1);
    if (node.type === "text" && last?.type === "text") {
      merged[merged.length - 1] = { text: last.text + node.text, type: "text" };
    } else if (node.type !== "text" || node.text !== "") {
      merged.push(node);
    }
  }

  return merged;
}

// The text of inline `nodes`, as an image's alternative text.
function plainText(nodes) {
  return nodes
    .map((node) => {
      if (node.children) {
        return plainText(node.children);
      }

      return node.type === "break" ? " " : (node.alt ?? node.text ?? "");
    })
    .join("");
}

// ---- Rendering -------------------------------------------------------

function create(context, part, className) {
  const node = context.document.createElement(TAGS[part]);
  if (className) {
    node.className = className;
  }

  return node;
}

function appendInline(parent, nodes, context, isInLink) {
  for (const node of nodes) {
    parent.append(renderNode(node, context, isInLink));
  }

  return parent;
}

function renderNode(node, context, isInLink) {
  const document = context.document;
  switch (node.type) {
    case "text":
      return document.createTextNode(node.text);
    case "break":
      return create(context, "break");
    case "code": {
      const code = create(context, "code");
      code.textContent = node.text;

      return code;
    }
    case "emphasis":
    case "strong":
    case "strike":
      return appendInline(create(context, node.type), node.children, context, isInLink);
    case "link":
      // A link inside a link text is only its text.
      return isInLink
        ? appendInline(document.createDocumentFragment(), node.children, context, true)
        : renderLink(node, context);
    case "image":
      return renderLink(
        { children: [{ text: node.alt || node.href, type: "text" }], href: node.href, image: true },
        context,
      );
    case "paragraph":
      return appendInline(create(context, "paragraph"), node.children, context, false);
    case "heading":
      return appendInline(
        create(context, "heading" + Math.min(node.level, 3)),
        node.children,
        context,
        false,
      );
    case "rule":
      return create(context, "rule");
    case "blockquote":
      return appendInline(create(context, "blockquote"), node.children, context, false);
    case "list":
      return renderList(node, context);
    case "code_block":
      return renderCodeBlock(node, context);
    case "table":
      return renderTable(node, context);
    default: {
      // `plain`: too large to parse.
      const plain = create(context, "plain", "md-plain");
      plain.textContent = node.text ?? "";

      return plain;
    }
  }
}

function renderLink(node, context) {
  const href = safeLink(node.href);
  if (!href) {
    return appendInline(create(context, "unlinked"), node.children, context, true);
  }
  const link = create(context, "link", node.image ? "md-image" : null);
  link.setAttribute("href", href);
  link.setAttribute("rel", "noopener noreferrer");
  link.setAttribute("target", "_blank");

  return appendInline(link, node.children, context, true);
}

function renderList(node, context) {
  const list = create(context, node.ordered ? "orderedList" : "unorderedList");
  if (node.ordered && node.start !== 1) {
    list.setAttribute("start", String(node.start));
  }
  for (const entry of node.children) {
    const item = create(context, "listItem");
    if (entry.task !== null) {
      const task = create(context, "task", "md-task");
      task.textContent = entry.task ? "[x]" : "[ ]";
      task.setAttribute("aria-label", entry.task ? "Done" : "Not done");
      item.classList.add("md-task-item");
      item.append(task, " ");
    }
    for (const child of entry.children) {
      // A tight list shows its paragraphs without paragraph spacing.
      if (!node.loose && child.type === "paragraph") {
        appendInline(item, child.children, context, false);
      } else {
        item.append(renderNode(child, context, false));
      }
    }
    list.append(item);
  }

  return list;
}

function renderCodeBlock(node, context) {
  const block = create(context, "codeBlock", "code-block");
  const head = create(context, "codeHead", "code-head");
  const label = create(context, "codeLabel");
  label.textContent = node.language || "text";
  head.append(label);
  if (context.clipboard) {
    head.append(copyButton(node.text, context));
  }
  const pre = create(context, "pre");
  const code = create(context, "code");
  code.textContent = node.text;
  pre.append(code);
  block.append(head, pre);

  return block;
}

// Copies `text` and says so for a moment. A refused write changes
// nothing.
function copyButton(text, context) {
  const button = create(context, "copyButton", "copy-btn");
  button.type = "button";
  button.textContent = "copy";
  button.setAttribute("aria-label", "Copy code");
  button.addEventListener("click", () => {
    void Promise.resolve()
      .then(() => context.clipboard.writeText(text))
      .then(() => {
        button.textContent = "copied";
        context.window?.setTimeout?.(() => {
          button.textContent = "copy";
        }, COPIED_MS);
      })
      .catch(() => {
        // The clipboard refused; the code is still there to select.
      });
  });

  return button;
}

function renderTable(node, context) {
  const scroll = create(context, "tableScroll", "table-scroll");
  const table = create(context, "table");
  const cell = (part, content, column) => {
    const alignment = node.alignments[column];

    return appendInline(
      create(context, part, alignment ? "align-" + alignment : null),
      content,
      context,
      false,
    );
  };
  const head = create(context, "tableHead");
  const headRow = create(context, "tableRow");
  node.header.forEach((content, column) => headRow.append(cell("tableHeader", content, column)));
  head.append(headRow);
  table.append(head);
  if (node.rows.length > 0) {
    const body = create(context, "tableBody");
    for (const row of node.rows) {
      const line = create(context, "tableRow");
      row.forEach((content, column) => line.append(cell("tableCell", content, column)));
      body.append(line);
    }
    table.append(body);
  }
  scroll.append(table);

  return scroll;
}
