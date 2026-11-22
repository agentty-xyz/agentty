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
// Deepest nesting of parentheses in a bare link destination. CommonMark
// asks for at least three.
const MAX_DESTINATION_PARENTHESES = 8;
// Characters the look-ahead of link destinations and `nextIndex` has read
// since the module loaded. The tests compare it with a text's length to
// check that link attempts that never close do not read the text again
// and again, without timing the parser.
let lookAheadReads = 0;

// How many characters the parser's look-ahead has read so far; see
// `lookAheadReads`.
export function lookAheadReadCount() {
  return lookAheadReads;
}

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
    index = parseBlock(lines, index, depth, blocks);
  }

  return blocks;
}

// Parses the block starting at the non-blank `lines[index]` into
// `blocks`, and returns the index after it.
function parseBlock(lines, index, depth, blocks) {
  const line = lines[index];
  const fence = fenceOpening(line);
  if (fence) {
    return parseFence(lines, index, fence, blocks);
  }
  const heading = headingOf(line);
  if (heading) {
    blocks.push({ children: parseInline(heading.text), level: heading.level, type: "heading" });

    return index + 1;
  }
  if (isThematicBreak(line)) {
    blocks.push({ type: "rule" });

    return index + 1;
  }
  if (isQuoteLine(line)) {
    return parseQuote(lines, index, depth, blocks);
  }
  const marker = listMarker(line);
  if (marker) {
    return parseList(lines, index, marker, depth, blocks);
  }
  if (tableColumns(lines, index)) {
    return parseTable(lines, index, blocks);
  }

  return parseParagraph(lines, index, blocks);
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

// Collects the lines of the list item whose marker is on `lines[start]`.
// Returns its content lines without their indentation, the index after
// it, whether a blank line separated two of its blocks, and whether it
// ended on a blank line.
function itemLines(lines, start, marker) {
  const content = [marker.content];
  let hasInnerBlank = false;
  let sawBlank = false;
  let index = start + 1;
  for (; index < lines.length; index += 1) {
    const line = lines[index];
    if (isBlank(line)) {
      sawBlank = true;
      content.push("");
    } else if (leadingSpaces(line) >= marker.contentIndent) {
      hasInnerBlank = hasInnerBlank || sawBlank;
      content.push(line.slice(marker.contentIndent));
      sawBlank = false;
    } else if (!sawBlank && !interruptsParagraph(lines, index) && !listMarker(line)) {
      // A lazy continuation of the item's paragraph.
      content.push(line.trim());
    } else {
      break;
    }
  }
  while (content.length > 1 && content.at(-1) === "") {
    content.pop();
  }

  return { content: content, end: index, endsBlank: sawBlank, hasInnerBlank: hasInnerBlank };
}

// A list whose first marker is on `lines[start]`. A blank line between
// items, or between two blocks of one item, loosens it.
function parseList(lines, start, first, depth, blocks) {
  const items = [];
  let isLoose = false;
  let index = start;
  let marker = first;
  while (marker) {
    const item = itemLines(lines, index, marker);
    items.push(listItem(item.content, depth));
    isLoose = isLoose || item.hasInnerBlank;
    index = item.end;
    const next = index < lines.length ? listMarker(lines[index]) : null;
    if (!sameList(first, next)) {
      break;
    }
    isLoose = isLoose || item.endsBlank;
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
  if (text.endsWith("|") && !text.endsWith(String.raw`\|`)) {
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
  if (right) {
    return "right";
  }

  return left ? "left" : null;
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

// The index of `character` from `start` and before `limit`, or -1.
function boundedIndexOf(scanner, character, start, limit) {
  const found = scanner.nextIndex(character, start);

  return found < limit ? found : -1;
}

// Reads the `(destination "title")` after a link's `]` at `open`, the
// index of `(`. Returns the destination and the index after `)`, or
// `null`.
function linkDestination(scanner, open) {
  const text = scanner.text;
  const limit = Math.min(text.length, open + MAX_DESTINATION_LENGTH);
  const read = readDestination(scanner, skipLinkSpaces(text, open + 1, limit), limit);
  if (!read) {
    return null;
  }
  let index = skipLinkSpaces(text, read.end, limit);
  const quote = text[index];
  if (quote === '"' || quote === "'") {
    const end = boundedIndexOf(scanner, quote, index + 1, limit);
    if (end === -1) {
      return null;
    }
    index = skipLinkSpaces(text, end + 1, limit);
  }

  return text[index] === ")" ? { destination: read.destination, end: index + 1 } : null;
}

function skipLinkSpaces(text, index, limit) {
  let next = index;
  while (next < limit && (text[next] === " " || text[next] === "\n")) {
    next += 1;
  }
  lookAheadReads += next - index + 1;

  return next;
}

// A destination in `<…>`, or a bare one that ends at a space or at a
// `)` it did not open. Returns it and the index after it, or `null`. A
// bare destination with more than `MAX_DESTINATION_PARENTHESES` open
// parentheses is none: every later `](` inside it adds one, so no read
// runs far past the next few.
function readDestination(scanner, index, limit) {
  const text = scanner.text;
  if (text[index] === "<") {
    const end = boundedIndexOf(scanner, ">", index, limit);
    if (end === -1 || scanner.nextIndex("\n", index) < end) {
      return null;
    }

    return { destination: text.slice(index + 1, end), end: end + 1 };
  }
  let parentheses = 0;
  let end = index;
  while (end < limit && !isWhitespace(text[end])) {
    const character = text[end];
    if (character === "\\" && end + 1 < limit) {
      end += 2;
      continue;
    }
    if (character === ")" && parentheses === 0) {
      break;
    }
    parentheses += parenthesisStep(character);
    if (parentheses > MAX_DESTINATION_PARENTHESES) {
      lookAheadReads += end - index + 1;

      return null;
    }
    end += 1;
  }
  lookAheadReads += end - index + 1;

  return { destination: text.slice(index, end), end: end };
}

// How `character` changes the count of open parentheses.
function parenthesisStep(character) {
  if (character === "(") {
    return 1;
  }

  return character === ")" ? -1 : 0;
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
function bareUrl(scanner, index, isInBrackets) {
  const text = scanner.text;
  const host = urlHostStart(text, index);
  if (host === -1) {
    return null;
  }
  const end = urlEnd(scanner, index, host, isInBrackets);
  const raw = end > host ? text.slice(index, end) : "";
  const href = raw ? safeLink(raw) : null;

  return href ? { end: end, href: href, text: raw } : null;
}

// Where the host starts after the `http` or `https` scheme and its `://`
// at `index`, or -1
// when no URL starts there.
function urlHostStart(text, index) {
  if (!text.startsWith("http", index) || isAsciiAlphanumeric(text[index - 1])) {
    return -1;
  }
  const scheme = text[index + 4] === "s" ? index + 5 : index + 4;

  return text.startsWith("://", scheme) ? scheme + 3 : -1;
}

// Where a bare URL whose host starts at `host` ends, or -1 when it runs
// past `MAX_DESTINATION_LENGTH`. Trailing punctuation belongs to the
// sentence, and so does a closing parenthesis the URL did not open.
function urlEnd(scanner, index, host, isInBrackets) {
  let end = scanner.urlStop(host, isInBrackets);
  if (end > index + MAX_DESTINATION_LENGTH) {
    return -1;
  }
  let opened = scanner.parenthesesBetween(host, end);
  while (end > host) {
    const last = scanner.text[end - 1];
    if (last === ")" && opened < 0) {
      opened += 1;
    } else if (!URL_TRAILING.includes(last)) {
      break;
    }
    end -= 1;
  }

  return end;
}

function endsBareUrl(character, isInBrackets) {
  return isWhitespace(character) || character === "<" || (isInBrackets && character === "]");
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

// The state of one inline parse: the position, text not yet made a
// token, the backtick runs, and the open `[` and `![`, innermost last,
// each with the tokens after it.
class InlineScanner {
  constructor(text) {
    this.text = text;
    this.index = 0;
    this.buffer = "";
    this.runs = backtickRuns(text);
    this.frames = [{ isImage: false, tokens: [] }];
    // The next character that ends a bare URL, outside and inside link
    // text, as found by the last search. Bare URLs start further along
    // the text each time, so the searches together read it once.
    this.urlStops = [-1, -1];
    this.parentheses = null;
    // The last position found for each character `nextIndex` searched.
    this.found = new Map();
  }

  // The first index of `character` at or after `from`, or the text's
  // length. Links ask from further along the text each time, so the
  // searches for one character together read the text once.
  nextIndex(character, from) {
    const last = this.found.get(character);
    if (last !== undefined && last.from <= from && last.at >= from) {
      return last.at;
    }
    const at = this.text.indexOf(character, from);
    const found = at === -1 ? this.text.length : at;
    lookAheadReads += Math.max(0, found - from) + 1;
    this.found.set(character, { at: found, from: from });

    return found;
  }

  // The first character at or after `from` that ends a bare URL.
  urlStop(from, isInBrackets) {
    const slot = isInBrackets ? 1 : 0;
    if (this.urlStops[slot] < from) {
      let stop = from;
      while (stop < this.text.length && !endsBareUrl(this.text[stop], isInBrackets)) {
        stop += 1;
      }
      this.urlStops[slot] = stop;
    }

    return this.urlStops[slot];
  }

  // Open parentheses minus closed ones in `text[start..end]`, from
  // running counts made once.
  parenthesesBetween(start, end) {
    if (!this.parentheses) {
      this.parentheses = new Int32Array(this.text.length + 1);
      for (let index = 0; index < this.text.length; index += 1) {
        this.parentheses[index + 1] = this.parentheses[index] + parenthesisStep(this.text[index]);
      }
    }

    return this.parentheses[end] - this.parentheses[start];
  }

  get tokens() {
    return this.frames.at(-1).tokens;
  }

  flush() {
    if (this.buffer) {
      this.tokens.push({ text: this.buffer, type: "text" });
      this.buffer = "";
    }
  }

  push(token) {
    this.flush();
    this.tokens.push(token);
  }

  // Returns an unclosed or unlinked bracket's tokens to the frame around
  // it, after its opening bracket as text.
  reopen(frame) {
    const outer = this.tokens;
    outer.push({ text: frame.isImage ? "![" : "[", type: "text" });
    for (const token of frame.tokens) {
      outer.push(token);
    }
  }
}

// What each special character starts. A scanner returns `false` when the
// character is plain text after all.
const INLINE_SCANNERS = {
  "\n": scanBreak,
  "!": scanOpenBracket,
  "*": scanDelimiter,
  "<": scanAutolink,
  "[": scanOpenBracket,
  "\\": scanEscape,
  "]": scanCloseBracket,
  _: scanDelimiter,
  "`": scanCodeSpan,
  h: scanBareUrl,
  "~": scanDelimiter,
};

function parseInline(text) {
  const scanner = new InlineScanner(text);
  while (scanner.index < text.length) {
    const scan = INLINE_SCANNERS[text[scanner.index]];
    if (!scan?.(scanner)) {
      scanner.buffer += text[scanner.index];
      scanner.index += 1;
    }
  }
  scanner.flush();
  // Brackets left open are text.
  while (scanner.frames.length > 1) {
    scanner.reopen(scanner.frames.pop());
  }

  return mergeText(processEmphasis(scanner.frames[0].tokens).nodes);
}

function scanEscape(scanner) {
  const next = scanner.text[scanner.index + 1];
  if (next === undefined || !ASCII_PUNCTUATION.includes(next)) {
    return false;
  }
  scanner.buffer += next;
  scanner.index += 2;

  return true;
}

function scanBreak(scanner) {
  scanner.push({ type: "break" });
  scanner.index += 1;

  return true;
}

// A code span, or its backticks as text when no run of the same length
// closes it.
function scanCodeSpan(scanner) {
  const { index, text } = scanner;
  let end = index;
  while (text[end] === "`") {
    end += 1;
  }
  const length = end - index;
  const close = closingRun(scanner.runs, length, index);
  if (close === -1) {
    scanner.buffer += text.slice(index, end);
    scanner.index = end;
  } else {
    scanner.push({ text: codeSpanText(text.slice(end, close)), type: "code" });
    scanner.index = close + length;
  }

  return true;
}

function scanDelimiter(scanner) {
  const run = delimiterRun(scanner.text, scanner.index);
  if (run.canOpen || run.canClose) {
    scanner.push(run);
  } else {
    scanner.buffer += run.mark.repeat(run.count);
  }
  scanner.index += run.count;

  return true;
}

function scanOpenBracket(scanner) {
  const isImage = scanner.text[scanner.index] === "!";
  if ((isImage && scanner.text[scanner.index + 1] !== "[") || scanner.frames.length > MAX_NESTING) {
    return false;
  }
  scanner.flush();
  scanner.frames.push({ isImage: isImage, tokens: [] });
  scanner.index += isImage ? 2 : 1;

  return true;
}

// Closes the innermost bracket: a link when a safe destination follows,
// otherwise text, with the inside parsed with what surrounds it.
function scanCloseBracket(scanner) {
  if (scanner.frames.length <= 1) {
    return false;
  }
  scanner.flush();
  const frame = scanner.frames.pop();
  const link = bracketLink(scanner, frame);
  if (link) {
    scanner.tokens.push(link.token);
    scanner.index = link.end;
  } else {
    scanner.reopen(frame);
    scanner.buffer = "]";
    scanner.index += 1;
  }

  return true;
}

// The link or image `frame` makes with the `(destination)` after the
// `]` the scanner is at, and the index after it, or `null`. An unsafe
// destination makes none.
function bracketLink(scanner, frame) {
  const open = scanner.index + 1;
  const target = scanner.text[open] === "(" ? linkDestination(scanner, open) : null;
  const href = target ? safeLink(target.destination) : null;
  if (!href) {
    return null;
  }
  const content = processEmphasis(frame.tokens);
  if (content.depth >= MAX_NESTING) {
    return null;
  }
  const token = frame.isImage
    ? { alt: plainText(content.nodes), depth: 1, href: href, type: "image" }
    : { children: content.nodes, depth: content.depth + 1, href: href, type: "link" };

  return { end: target.end, token: token };
}

function scanAutolink(scanner) {
  return pushLink(scanner, autolink(scanner.text, scanner.index));
}

function scanBareUrl(scanner) {
  return pushLink(scanner, bareUrl(scanner, scanner.index, scanner.frames.length > 1));
}

function pushLink(scanner, link) {
  if (!link) {
    return false;
  }
  scanner.push({ children: [{ text: link.text, type: "text" }], depth: 1, href: link.href, type: "link" });
  scanner.index = link.end;

  return true;
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
// takes the nearest opener of its mark that the rule of 3 allows, and
// the openers between them become text. Returns the nodes and how deeply they nest; a pair that
// would nest deeper than `MAX_NESTING` stays text.
function processEmphasis(tokens) {
  const pairing = new EmphasisPairing();
  for (const token of tokens) {
    if (token.type === "delimiter") {
      pairing.delimiter(token);
    } else {
      pairing.append(token, token.depth ?? 0);
    }
  }

  return pairing.result();
}

// The state of `processEmphasis`: the nodes so far, and the open
// delimiter runs, innermost last, each with where its text sits in `out`
// and the deepest node after it.
class EmphasisPairing {
  constructor() {
    this.out = [];
    this.stack = [];
    this.openers = { "*": 0, _: 0, "~": 0 };
    this.outerDepth = 0;
    // For each kind of closer, the stack height below which a search
    // already found no opener for it, as CommonMark's `openers_bottom`.
    this.floors = new Map();
  }

  append(node, depth) {
    this.out.push(node);
    this.note(depth);
  }

  // Records a node of `depth` after the innermost open run.
  note(depth) {
    const top = this.stack.at(-1);
    if (top) {
      top.depth = Math.max(top.depth, depth);
    } else {
      this.outerDepth = Math.max(this.outerDepth, depth);
    }
  }

  // Forgets the innermost open run; its text stays text.
  drop() {
    const entry = this.stack.pop();
    this.openers[entry.mark] -= 1;
    for (const [kind, floor] of this.floors) {
      if (floor > this.stack.length) {
        this.floors.set(kind, this.stack.length);
      }
    }
    this.note(entry.depth);
  }

  delimiter(token) {
    let remaining = token.count;
    while (token.canClose && remaining > 0) {
      const position = this.openerFor(token);
      if (position === -1) {
        break;
      }
      const used = this.close(token, remaining, position);
      if (used === 0) {
        break;
      }
      remaining -= used;
    }
    if (remaining === 0) {
      return;
    }
    const text = { text: token.mark.repeat(remaining), type: "text" };
    this.out.push(text);
    if (token.canOpen) {
      this.stack.push({
        canClose: token.canClose,
        count: token.count,
        depth: 0,
        index: this.out.length - 1,
        mark: token.mark,
        remaining: remaining,
        text: text,
      });
      this.openers[token.mark] += 1;
    }
  }

  // The stack position of the nearest open run `token` can close, or -1.
  // A failed search records its floor, so the next closer of the same
  // kind does not search the same runs again.
  openerFor(token) {
    if (this.openers[token.mark] === 0) {
      return -1;
    }
    const kind = token.mark + (token.count % 3) + (token.canOpen ? "o" : "");
    const floor = this.floors.get(kind) ?? 0;
    for (let position = this.stack.length - 1; position >= floor; position -= 1) {
      const entry = this.stack[position];
      if (entry.mark === token.mark && canPair(entry, token)) {
        return position;
      }
    }
    this.floors.set(kind, this.stack.length);

    return -1;
  }

  // Pairs a closing run with `remaining` marks against the open run at
  // `position`, after dropping the runs above it, and returns how many
  // marks that used: 0 when the pair would nest too deep.
  close(token, remaining, position) {
    while (this.stack.length - 1 > position) {
      this.drop();
    }
    const opener = this.stack.at(-1);
    if (opener.depth + 1 > MAX_NESTING) {
      this.drop();

      return 0;
    }
    const used = marksUsed(token.mark, remaining, opener.remaining);
    const children = mergeText(this.out.splice(opener.index + 1));
    const depth = opener.depth + 1;
    opener.remaining -= used;
    opener.text.text = token.mark.repeat(opener.remaining);
    opener.depth = 0;
    if (opener.remaining === 0) {
      this.out.pop();
      this.drop();
    }
    this.append({ children: children, depth: depth, type: pairType(token.mark, used) }, depth);

    return used;
  }

  result() {
    const depth = this.stack.reduce((deepest, entry) => Math.max(deepest, entry.depth), this.outerDepth);

    return { depth: depth, nodes: this.out.filter((node) => node.type !== "text" || node.text !== "") };
  }
}

// CommonMark's rule of 3: when either run could both open and close, the
// two may pair only if their lengths do not add up to a multiple of 3,
// unless both lengths are. So `*foo**bar**baz*` nests `bar` in `foo…baz`.
function canPair(opener, closer) {
  if (closer.mark === "~" || (!opener.canClose && !closer.canOpen)) {
    return true;
  }

  return (opener.count + closer.count) % 3 !== 0 || (opener.count % 3 === 0 && closer.count % 3 === 0);
}

// How many marks a pair takes: `~~` always two, `*` and `_` two for
// strong when both runs have them, else one.
function marksUsed(mark, closing, opening) {
  if (mark === "~" || (closing >= 2 && opening >= 2)) {
    return 2;
  }

  return 1;
}

function pairType(mark, used) {
  if (used === 1) {
    return "emphasis";
  }

  return mark === "~" ? "strike" : "strong";
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

// How each node type renders. A type not listed, `plain`, is a message
// too large to parse.
const RENDERERS = {
  blockquote: (node, context) => appendInline(create(context, "blockquote"), node.children, context, false),
  break: (_node, context) => create(context, "break"),
  code: (node, context) => withText(create(context, "code"), node.text),
  code_block: (node, context) => renderCodeBlock(node, context),
  emphasis: renderSpan,
  heading: (node, context) =>
    appendInline(create(context, "heading" + Math.min(node.level, 3)), node.children, context, false),
  image: (node, context) =>
    renderLink({ children: [{ text: node.alt || node.href, type: "text" }], href: node.href, image: true }, context),
  // A link inside a link text is only its text.
  link: (node, context, isInLink) =>
    isInLink
      ? appendInline(context.document.createDocumentFragment(), node.children, context, true)
      : renderLink(node, context),
  list: (node, context) => renderList(node, context),
  paragraph: (node, context) => appendInline(create(context, "paragraph"), node.children, context, false),
  rule: (_node, context) => create(context, "rule"),
  strike: renderSpan,
  strong: renderSpan,
  table: (node, context) => renderTable(node, context),
  text: (node, context) => context.document.createTextNode(node.text),
};

function renderNode(node, context, isInLink) {
  const render = Object.hasOwn(RENDERERS, node.type) ? RENDERERS[node.type] : null;
  if (render) {
    return render(node, context, isInLink);
  }

  return withText(create(context, "plain", "md-plain"), node.text ?? "");
}

// `emphasis`, `strong` and `strike`, whose part names are their types.
function renderSpan(node, context, isInLink) {
  return appendInline(create(context, node.type), node.children, context, isInLink);
}

function withText(element, text) {
  element.textContent = text;

  return element;
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
    list.append(renderListItem(entry, node.loose, context));
  }

  return list;
}

function renderListItem(entry, isLoose, context) {
  const item = create(context, "listItem");
  if (entry.task !== null) {
    item.classList.add("md-task-item");
    item.append(renderTask(entry.task, context), " ");
  }
  for (const child of entry.children) {
    // A tight list shows its paragraphs without paragraph spacing.
    if (!isLoose && child.type === "paragraph") {
      appendInline(item, child.children, context, false);
    } else {
      item.append(renderNode(child, context, false));
    }
  }

  return item;
}

function renderTask(isDone, context) {
  const task = create(context, "task", "md-task");
  task.textContent = isDone ? "[x]" : "[ ]";
  task.setAttribute("aria-label", isDone ? "Done" : "Not done");

  return task;
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
