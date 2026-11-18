// Safe Markdown for assistant replies. A small GitHub-flavoured subset is
// turned into DOM nodes directly; HTML in the text is never parsed, so a reply
// cannot add markup, scripts, styles or remote images. The renderer is pure
// and is re-run on every streamed prefix, so partial input must never throw.

const MAX_BLOCK_DEPTH = 16;
const MAX_INLINE_DEPTH = 64;
// Padding short rows to the header width must not amplify a small reply.
const MAX_TABLE_CELLS = 10_000;
const MAX_DESTINATION = 2_048;
const ALLOWED_SCHEMES = new Set(["http", "https", "mailto"]);
const ASCII_PUNCTUATION = /[!-/:-@[-`{-~]/;
const ESCAPED = /\\([!-/:-@[-`{-~])/g;
const WHITESPACE = /\s/;
const WORD = /[\p{L}\p{N}]/u;
const ASCII_ALNUM = /[A-Za-z0-9]/;
// [\s\S]* (not .*) consumes U+2028 and U+2029, so no split is ever retried.
const FENCE = /^( {0,3})(`{3,}|~{3,})([\s\S]*)$/;
const FENCE_CLOSE = /^ {0,3}(`{3,}|~{3,})[ \t]*$/;
const HEADING = /^ {0,3}(#{1,6})(?:[ \t]+([\s\S]*))?$/;
const QUOTE = /^ {0,3}> ?([\s\S]*)$/;
const ITEM = /^( {0,3})(?:([-+*])|(\d{1,9})([.)]))(?:([ \t]+)([\s\S]*))?$/;
const TASK = /^\[([ xX])\](?:[ \t]+|$)/;
// A list item or quote marker at a sticky position, stripped before tracking blocks.
const CONTAINER_MARKER = /(?:[-+*]|\d{1,9}[.)])(?:[ \t]+|$)|>[ \t]?/y;
// Characters that end a bare URL: whitespace, markup and CJK punctuation.
const URL_STOP = new Set([..."<>\"'`、。，．！？：；「」『』（）【】〈〉《》〔〕［］｛｝…‥・　"]);
const URL_TRAILING = new Set([..."?!.,:;*_~'\")]}"]);

export function renderMarkdown(source, doc = document) {
  const lines = String(source ?? "").replace(/\r\n?/g, "\n").split("\n");
  const fragment = doc.createDocumentFragment();
  for (const block of parseBlocks(lines, 0)) fragment.append(renderBlock(block, doc));
  return fragment;
}

// ---------------------------------------------------------------- blocks

const isBlank = (line) => /^[ \t]*$/.test(line);
const isSpace = (char) => char === " " || char === "\t";

function trimEnd(line) {
  let end = line.length;
  while (end > 0 && isSpace(line[end - 1])) end -= 1;
  return line.slice(0, end);
}

function trimLine(line) {
  let start = 0;
  while (start < line.length && isSpace(line[start])) start += 1;
  return trimEnd(line.slice(start));
}

function indentOf(line) {
  let index = 0;
  while (line[index] === " ") index += 1;
  return index;
}

function stripIndent(line, count) {
  let index = 0;
  while (index < count && line[index] === " ") index += 1;
  return line.slice(index);
}

function isThematicBreak(line) {
  if (indentOf(line) > 3) return false;
  const compact = line.replace(/[ \t]/g, "");
  return compact.length >= 3 && /^[-*_]$/.test(compact[0]) && [...compact].every((char) => char === compact[0]);
}

function headingText(raw = "") {
  let text = trimEnd(raw);
  let end = text.length;
  while (end > 0 && text[end - 1] === "#") end -= 1;
  // A closing sequence must be all hashes or follow a space.
  if (end < text.length && (end === 0 || text[end - 1] === " " || text[end - 1] === "\t")) text = text.slice(0, end);
  return trimLine(text);
}

function matchItem(line) {
  const match = ITEM.exec(line);
  if (!match) return null;
  const ordered = match[3] !== undefined;
  const marker = ordered ? match[4] : match[2];
  const markerWidth = ordered ? match[3].length + 1 : 1;
  const spacing = match[5] ? match[5].replace(/\t/g, "    ").length : 0;
  const rest = match[6] ?? "";
  const gap = rest === "" ? 1 : spacing > 4 ? 1 : spacing;
  return {
    ordered,
    marker,
    start: ordered ? Number(match[3]) : 1,
    contentIndent: match[1].length + markerWidth + gap,
    rest: spacing > 4 ? " ".repeat(spacing - 1) + rest : rest,
  };
}

function startsBlock(line, depth) {
  return FENCE.test(line) || HEADING.test(line) || isThematicBreak(line) ||
    (depth < MAX_BLOCK_DEPTH && (QUOTE.test(line) || Boolean(matchItem(line))));
}

function splitRow(line) {
  let text = trimLine(line);
  if (text.startsWith("|")) text = text.slice(1);
  if (text.endsWith("|") && !text.endsWith("\\|")) text = text.slice(0, -1);
  const cells = [];
  let start = 0;
  for (let index = 0; index < text.length; index += 1) {
    if (text[index] === "\\") index += 1;
    else if (text[index] === "|") {
      cells.push(trimLine(text.slice(start, index)));
      start = index + 1;
    }
  }
  cells.push(trimLine(text.slice(start)));
  return cells.map((cell) => cell.replace(/\\\|/g, "|"));
}

function delimiterRow(line) {
  if (!line.includes("-") || indentOf(line) > 3) return null;
  const cells = splitRow(line);
  if (!line.includes("|") && cells.length < 2) return null;
  const align = [];
  for (const cell of cells) {
    if (!/^:?-+:?$/.test(cell)) return null;
    const left = cell.startsWith(":");
    const right = cell.endsWith(":");
    align.push(left && right ? "center" : right ? "right" : left ? "left" : "");
  }
  return align;
}

function parseBlocks(lines, depth) {
  const blocks = [];
  let paragraph = null;
  const flush = () => {
    if (paragraph) blocks.push({ type: "paragraph", text: paragraph.join("\n") });
    paragraph = null;
  };
  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
    if (isBlank(line)) {
      flush();
      index += 1;
      continue;
    }
    const fence = FENCE.exec(line);
    if (fence && !(fence[2][0] === "`" && fence[3].includes("`"))) {
      flush();
      const body = [];
      let next = index + 1;
      let closed = false;
      for (; next < lines.length; next += 1) {
        const close = FENCE_CLOSE.exec(lines[next]);
        if (close && close[1][0] === fence[2][0] && close[1].length >= fence[2].length) {
          closed = true;
          break;
        }
        body.push(stripIndent(lines[next], fence[1].length));
      }
      blocks.push({ type: "code", language: trimLine(fence[3]).split(/[ \t]/)[0], text: body.join("\n") });
      index = closed ? next + 1 : next;
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading) {
      flush();
      blocks.push({ type: "heading", level: heading[1].length, text: headingText(heading[2]) });
      index += 1;
      continue;
    }
    if (isThematicBreak(line)) {
      flush();
      blocks.push({ type: "hr" });
      index += 1;
      continue;
    }
    if (depth < MAX_BLOCK_DEPTH && QUOTE.test(line)) {
      flush();
      const inner = [];
      while (index < lines.length) {
        const quoted = QUOTE.exec(lines[index]);
        if (!quoted) break;
        inner.push(quoted[1]);
        index += 1;
      }
      blocks.push({ type: "quote", children: parseBlocks(inner, depth + 1) });
      continue;
    }
    const item = depth < MAX_BLOCK_DEPTH ? matchItem(line) : null;
    // An ordered list interrupts a paragraph only when it starts at 1.
    if (item && !(paragraph && item.ordered && item.start !== 1)) {
      flush();
      const list = parseList(lines, index, item, depth);
      blocks.push(list.block);
      index = list.next;
      continue;
    }
    const align = line.includes("|") && index + 1 < lines.length ? delimiterRow(lines[index + 1]) : null;
    const head = align ? splitRow(line) : null;
    if (head && head.length === align.length && head.length <= MAX_TABLE_CELLS) {
      flush();
      const rows = [];
      let next = index + 2;
      let total = head.length;
      // Rows beyond the cell budget continue as ordinary text.
      while (next < lines.length && !isBlank(lines[next]) && !startsBlock(lines[next], depth) && total + head.length <= MAX_TABLE_CELLS) {
        const cells = splitRow(lines[next]);
        rows.push(Array.from(head, (_, column) => cells[column] ?? ""));
        total += head.length;
        next += 1;
      }
      blocks.push({ type: "table", align, head, rows });
      index = next;
      continue;
    }
    (paragraph ??= []).push(trimLine(line));
    index += 1;
  }
  flush();
  return blocks;
}

function parseList(lines, start, first, depth) {
  const items = [];
  let loose = false;
  let index = start;
  while (index < lines.length) {
    const item = matchItem(lines[index]);
    if (!item || item.ordered !== first.ordered || item.marker !== first.marker || isThematicBreak(lines[index])) break;
    const body = [];
    let fenceOpen = "";
    let fenceQuotes = 0;
    let tableOpen = false;
    let tableQuotes = 0;
    let paragraphOpen = false;
    let previous = "";
    // Lazy continuation is allowed only while the innermost open block is a
    // paragraph. Fences and tables are tracked through nested item and quote
    // markers, so a block opened in a nested item also stops it. A fence ends
    // only at a matching close inside the quotes it opened in; any other line,
    // a rule or a list marker included, is code.
    const add = (content) => {
      body.push(content);
      let inner = trimLine(content);
      if (!fenceOpen && isThematicBreak(inner)) {
        tableOpen = false;
        paragraphOpen = false;
        previous = "";
        return;
      }
      let offset = 0;
      let quotes = 0;
      let items = 0;
      for (;;) {
        CONTAINER_MARKER.lastIndex = offset;
        const marker = CONTAINER_MARKER.exec(inner);
        if (!marker || !marker[0] || (fenceOpen && (marker[0][0] !== ">" || quotes === fenceQuotes))) break;
        if (marker[0][0] === ">") quotes += 1;
        else items += 1;
        offset = CONTAINER_MARKER.lastIndex;
      }
      inner = inner.slice(offset);
      // As in parseBlocks, a table row keeps its container and indentation and
      // ends at a blank line or a block start.
      const row = tableOpen && quotes === tableQuotes && items === 0 && !startsBlock(offset ? inner : content, depth + 1);
      const fence = /^(`{3,}|~{3,})/.exec(inner);
      if (fenceOpen) {
        if (quotes === fenceQuotes && fence && fence[1][0] === fenceOpen[0] && fence[1].length >= fenceOpen.length && trimLine(inner).length === fence[1].length) fenceOpen = "";
        paragraphOpen = false;
      } else if (fence && !(fence[1][0] === "`" && inner.slice(fence[1].length).includes("`"))) {
        fenceOpen = fence[1];
        fenceQuotes = quotes;
        tableOpen = false;
        paragraphOpen = false;
      } else if (inner === "") {
        tableOpen = false;
        paragraphOpen = false;
      } else if (row) {
        // Table rows continue without pipes.
        paragraphOpen = false;
      } else if (previous.includes("|") && delimiterRow(inner)) {
        tableOpen = true;
        tableQuotes = quotes;
        paragraphOpen = false;
      } else {
        tableOpen = false;
        paragraphOpen = !HEADING.test(inner);
      }
      previous = inner;
    };
    add(item.rest);
    let next = index + 1;
    let blank = false;
    for (; next < lines.length; next += 1) {
      const line = lines[next];
      if (isBlank(line)) {
        blank = true;
        add("");
        continue;
      }
      if (indentOf(line) >= item.contentIndent) {
        add(line.slice(item.contentIndent));
        blank = false;
        continue;
      }
      if (blank || fenceOpen || !paragraphOpen || startsBlock(line, depth + 1)) break;
      add(line); // A lazy paragraph continuation.
    }
    while (body.length > 1 && isBlank(body[body.length - 1])) body.pop();
    if (body.some(isBlank)) loose = true;
    const children = parseBlocks(body, depth + 1);
    let task = null;
    const lead = children[0];
    const checkbox = lead?.type === "paragraph" ? TASK.exec(lead.text) : null;
    if (checkbox) {
      task = checkbox[1] !== " ";
      lead.text = lead.text.slice(checkbox[0].length);
    }
    items.push({ task, children });
    index = next;
    const following = index < lines.length ? matchItem(lines[index]) : null;
    if (blank && following && following.ordered === first.ordered && following.marker === first.marker) loose = true;
  }
  return { block: { type: "list", ordered: first.ordered, start: first.start, loose, items }, next: index };
}

function renderBlock(block, doc) {
  switch (block.type) {
    case "paragraph": {
      const paragraph = doc.createElement("p");
      renderInline(parseInline(block.text, true), paragraph, doc);
      return paragraph;
    }
    case "heading": {
      const heading = doc.createElement(`h${block.level}`);
      renderInline(parseInline(block.text, true), heading, doc);
      return heading;
    }
    case "code": {
      const wrapper = doc.createElement("div");
      wrapper.className = "code-block";
      if (block.language) wrapper.setAttribute("data-language", block.language);
      const toolbar = doc.createElement("div");
      toolbar.className = "code-toolbar";
      const label = doc.createElement("span");
      label.className = "code-language";
      label.textContent = block.language || "コード";
      const copy = doc.createElement("button");
      copy.setAttribute("type", "button");
      copy.className = "code-copy";
      copy.setAttribute("aria-label", "コードをコピー");
      copy.textContent = "コピー";
      toolbar.append(label, copy);
      const pre = doc.createElement("pre");
      const code = doc.createElement("code");
      code.textContent = block.text;
      pre.append(code);
      wrapper.append(toolbar, pre);
      return wrapper;
    }
    case "quote": {
      const quote = doc.createElement("blockquote");
      for (const child of block.children) quote.append(renderBlock(child, doc));
      return quote;
    }
    case "hr":
      return doc.createElement("hr");
    case "list": {
      const list = doc.createElement(block.ordered ? "ol" : "ul");
      if (block.ordered && block.start !== 1) list.setAttribute("start", String(block.start));
      if (block.items.some((item) => item.task !== null)) list.className = "task-list";
      for (const item of block.items) {
        const entry = doc.createElement("li");
        if (item.task !== null) {
          entry.className = "task-item";
          const box = doc.createElement("input");
          box.setAttribute("type", "checkbox");
          box.setAttribute("disabled", "");
          if (item.task) box.setAttribute("checked", "");
          entry.append(box);
        }
        for (const child of item.children) {
          if (!block.loose && child.type === "paragraph") renderInline(parseInline(child.text, true), entry, doc);
          else entry.append(renderBlock(child, doc));
        }
        list.append(entry);
      }
      return list;
    }
    case "table": {
      const scroll = doc.createElement("div");
      scroll.className = "table-scroll";
      const table = doc.createElement("table");
      const row = (cells, tag) => {
        const tr = doc.createElement("tr");
        cells.forEach((cell, column) => {
          const element = doc.createElement(tag);
          if (block.align[column]) element.setAttribute("data-align", block.align[column]);
          renderInline(parseInline(cell, true), element, doc);
          tr.append(element);
        });
        return tr;
      };
      const head = doc.createElement("thead");
      head.append(row(block.head, "th"));
      table.append(head);
      if (block.rows.length) {
        const body = doc.createElement("tbody");
        for (const cells of block.rows) body.append(row(cells, "td"));
        table.append(body);
      }
      scroll.append(table);
      return scroll;
    }
    default:
      return doc.createTextNode("");
  }
}

// ---------------------------------------------------------------- inlines

function safeHref(raw) {
  const value = raw.trim();
  if (!value || /[\u0000- \u007f]/.test(value)) return null;
  const scheme = /^([A-Za-z][A-Za-z0-9+.-]*):/.exec(value);
  if (!scheme || !ALLOWED_SCHEMES.has(scheme[1].toLowerCase())) return null;
  try {
    const url = new URL(value);
    if (url.protocol !== "mailto:" && !url.hostname) return null;
  } catch {
    return null;
  }
  return value;
}

// Matches code spans left to right; a run is closed by the next unused run of
// the same length. Each length keeps a moving pointer, so matching is linear.
function codeSpans(text) {
  const runs = [];
  for (let index = 0; index < text.length; index += 1) {
    if (text[index] !== "`") continue;
    let end = index;
    while (text[end] === "`") end += 1;
    let slashes = 0;
    while (text[index - 1 - slashes] === "\\") slashes += 1;
    // Escapes only matter outside code: an escaped first backtick cannot open
    // a span, but the whole run can still close one (as in `C:\`).
    runs.push({ start: index, length: end - index, escaped: slashes % 2 === 1 });
    index = end - 1;
  }
  const byLength = new Map();
  runs.forEach((run, position) => {
    if (!byLength.has(run.length)) byLength.set(run.length, { positions: [], next: 0 });
    byLength.get(run.length).positions.push(position);
  });
  const spans = new Map();
  let cursor = 0;
  runs.forEach((run, position) => {
    const start = run.escaped ? run.start + 1 : run.start;
    const length = run.escaped ? run.length - 1 : run.length;
    if (start < cursor || length === 0) return;
    const same = byLength.get(length);
    if (!same) return;
    while (same.next < same.positions.length && same.positions[same.next] <= position) same.next += 1;
    if (same.next >= same.positions.length) return;
    const closer = runs[same.positions[same.next]];
    same.next += 1;
    let content = text.slice(start + length, closer.start).replace(/\n/g, " ");
    if (content.length > 1 && content[0] === " " && content[content.length - 1] === " " && content.trim()) content = content.slice(1, -1);
    spans.set(start, { end: closer.start + closer.length, content });
    cursor = closer.start + closer.length;
  });
  return spans;
}

function matchBrackets(text, spans) {
  const matches = new Map();
  const open = [];
  for (let index = 0; index < text.length; index += 1) {
    const span = spans.get(index);
    if (span) index = span.end - 1;
    else if (text[index] === "\\") index += 1;
    else if (text[index] === "[") open.push(index);
    else if (text[index] === "]" && open.length) matches.set(open.pop(), index);
  }
  return matches;
}

// For every position, the next character that can end a link destination,
// computed once per text, so each candidate is rejected in constant time.
function nextStops(text, scan, key, stops) {
  if (!scan[key]) {
    const next = new Int32Array(text.length + 1);
    next[text.length] = text.length;
    for (let index = text.length - 1; index >= 0; index -= 1) next[index] = stops(text[index]) ? index : next[index + 1];
    scan[key] = next;
  }
  return scan[key];
}
const terminators = (text, scan) => nextStops(text, scan, "terminators", (char) => char === ")" || WHITESPACE.test(char));
const angleStops = (text, scan) => nextStops(text, scan, "angles", (char) => char === ">" || char === "\n");

// A failed scan that reached the end of the text without a terminator proves
// that every later candidate fails too, so the cache keeps link parsing linear.
function linkTail(text, at, scan) {
  if (at >= scan.deadEnd) return null;
  let index = at + 1;
  const skipSpaces = () => { while (text[index] === " " || text[index] === "\t") index += 1; };
  skipSpaces();
  let href = "";
  if (text[index] === "<") {
    const end = angleStops(text, scan)[index + 1];
    if (text[end] !== ">" || end - index > MAX_DESTINATION) return null;
    href = text.slice(index + 1, end);
    index = end + 1;
  } else {
    const start = index;
    if (terminators(text, scan)[start] - start > MAX_DESTINATION) return null;
    let depth = 0;
    let closed = false;
    for (; index < text.length; index += 1) {
      if (index - start > MAX_DESTINATION) return null;
      const char = text[index];
      if (char === "\\" && index + 1 < text.length) index += 1;
      else if (char === "(") depth += 1;
      else if (char === ")") {
        if (depth === 0) break;
        depth -= 1;
        closed = true;
      } else if (WHITESPACE.test(char)) break;
    }
    if (index >= text.length) {
      if (!closed) scan.deadEnd = Math.min(scan.deadEnd, at);
      return null;
    }
    if (depth !== 0) return null;
    href = text.slice(start, index);
  }
  skipSpaces();
  let title = null;
  const quote = text[index];
  if (quote === "\"" || quote === "'" || quote === "(") {
    const close = quote === "(" ? ")" : quote;
    let end = index + 1;
    for (; end < text.length && text[end] !== close && end - index <= MAX_DESTINATION; end += 1) if (text[end] === "\\") end += 1;
    if (end >= text.length || text[end] !== close) return null;
    title = text.slice(index + 1, end).replace(ESCAPED, "$1");
    index = end + 1;
    skipSpaces();
  }
  if (text[index] !== ")") return null;
  return { href: href.replace(ESCAPED, "$1"), title, end: index + 1 };
}

const AUTHORITY = /[A-Za-z0-9.:@%[\]_-]/;

function bareUrlEnd(text, start) {
  const head = text.slice(start, start + 8).toLowerCase();
  if (!head.startsWith("http://") && !head.startsWith("https://")) return 0;
  // A URL wrapped in * or ~ emphasis (**https://...**) ends at that delimiter.
  // Underscores are common in paths, so an `_` wrapper is left to the
  // trailing-punctuation trim below.
  const wrapper = "*~".includes(text[start - 1] ?? "") ? text[start - 1] : "";
  let end = start + head.indexOf("//") + 2;
  // The host is ASCII, so Japanese text or markup right after it is not part of it.
  while (end < text.length && AUTHORITY.test(text[end])) end += 1;
  // An invalid host is rejected before its path is read, so repeats stay
  // linear. One that is valid only without trailing punctuation (`:8443.` or
  // `]`) can link on its own, never with a path.
  if (!safeHref(text.slice(start, end))) {
    let host = end;
    while (host > start && URL_TRAILING.has(text[host - 1])) host -= 1;
    return host < end ? host : 0;
  }
  let opens = 0;
  let closes = 0;
  if ("/?#".includes(text[end] ?? " ")) {
    // Controls end the URL too, since safeHref would reject them after a full scan.
    while (end < text.length && !URL_STOP.has(text[end]) && !WHITESPACE.test(text[end]) && text[end] !== wrapper && text.charCodeAt(end) > 0x1f && text.charCodeAt(end) !== 0x7f) {
      if (text[end] === "(") opens += 1;
      else if (text[end] === ")") closes += 1;
      end += 1;
    }
  }
  while (end > start && URL_TRAILING.has(text[end - 1])) {
    if (text[end - 1] === ")") {
      if (closes <= opens) break;
      closes -= 1;
    }
    end -= 1;
  }
  return end - start > head.indexOf("//") + 2 ? end : 0;
}

function flanking(char, before, after) {
  const spaceBefore = before === undefined || WHITESPACE.test(before);
  const spaceAfter = after === undefined || WHITESPACE.test(after);
  if (char !== "_") return { canOpen: !spaceAfter, canClose: !spaceBefore };
  // Underscores never emphasise inside a word, so identifiers stay literal.
  return {
    canOpen: !spaceAfter && !(before !== undefined && WORD.test(before)),
    canClose: !spaceBefore && !(after !== undefined && WORD.test(after)),
  };
}

function tokenize(text, allowLinks) {
  const spans = codeSpans(text);
  const brackets = allowLinks ? matchBrackets(text, spans) : null;
  const tokens = [];
  const scan = { deadEnd: Infinity, nextClose: -1 };
  let textStart = 0;
  const flush = (end) => {
    if (end > textStart) tokens.push({ type: "text", value: text.slice(textStart, end) });
  };
  let index = 0;
  while (index < text.length) {
    const char = text[index];
    const span = spans.get(index);
    if (span) {
      flush(index);
      tokens.push({ type: "code", value: span.content });
      index = textStart = span.end;
      continue;
    }
    if (char === "\\" && index + 1 < text.length && ASCII_PUNCTUATION.test(text[index + 1])) {
      flush(index);
      tokens.push({ type: "text", value: text[index + 1] });
      index = textStart = index + 2;
      continue;
    }
    if (char === "\n") {
      flush(index);
      tokens.push({ type: "br" });
      index = textStart = index + 1;
      continue;
    }
    if (allowLinks && char === "<") {
      if (scan.nextClose !== -2 && scan.nextClose < index) scan.nextClose = text.indexOf(">", index + 1);
      if (scan.nextClose === -1) scan.nextClose = -2;
      const end = scan.nextClose;
      const candidate = end > index ? text.slice(index + 1, end) : "";
      const href = candidate && !/[\s<]/.test(candidate) ? safeHref(candidate) : null;
      if (href) {
        flush(index);
        tokens.push({ type: "link", href, title: null, children: [{ type: "text", value: candidate }] });
        index = textStart = end + 1;
        continue;
      }
    }
    if (allowLinks && (char === "h" || char === "H") && !(index > 0 && ASCII_ALNUM.test(text[index - 1]))) {
      const end = bareUrlEnd(text, index);
      const href = end ? safeHref(text.slice(index, end)) : null;
      if (href) {
        flush(index);
        tokens.push({ type: "link", href, title: null, children: [{ type: "text", value: href }] });
        index = textStart = end;
        continue;
      }
    }
    if (allowLinks && (char === "[" || (char === "!" && text[index + 1] === "["))) {
      const open = char === "!" ? index + 1 : index;
      const close = brackets.get(open);
      const tail = close !== undefined && text[close + 1] === "(" ? linkTail(text, close + 1, scan) : null;
      if (tail) {
        flush(index);
        const label = text.slice(open + 1, close);
        const href = safeHref(tail.href);
        if (char === "!") {
          const alt = plainText(parseInline(label, false));
          tokens.push(href
            ? { type: "link", href, title: tail.title, children: [{ type: "text", value: alt || "画像" }] }
            : { type: "text", value: alt });
        } else {
          const children = parseInline(label, false);
          tokens.push(href ? { type: "link", href, title: tail.title, children } : { type: "group", children });
        }
        index = textStart = tail.end;
        continue;
      }
    }
    if (char === "*" || char === "_" || char === "~") {
      let end = index;
      while (text[end] === char) end += 1;
      const count = end - index;
      if (char !== "~" || count === 2) {
        flush(index);
        const { canOpen, canClose } = flanking(char, text[index - 1], text[end]);
        tokens.push({ type: "delim", char, count, remaining: count, canOpen, canClose, opens: [], closes: [] });
        textStart = end;
      }
      index = end;
      continue;
    }
    index += 1;
  }
  flush(text.length);
  return tokens;
}

// CommonMark-style delimiter matching. Matches are recorded on the runs and
// always nest, so the tree is built afterwards in a single pass.
function matchEmphasis(tokens) {
  const stacks = { "*": [], _: [], "~": [] };
  // After a closer finds nothing, later closers of the same delimiter, length
  // class and openness skip the openers it already rejected (CommonMark's
  // openers_bottom), so mismatched runs are never rescanned.
  const bottom = new Map(); // closer class -> highest token position already rejected
  tokens.forEach((token, position) => {
    if (token.type !== "delim") return;
    const stack = stacks[token.char];
    const key = `${token.char}${token.canOpen ? 1 : 0}${token.count % 3}`;
    if (token.canClose) {
      while (token.remaining > 0) {
        let found = -1;
        const floor = bottom.get(key) ?? -1;
        for (let index = stack.length - 1; index >= 0 && stack[index] > floor; index -= 1) {
          const opener = tokens[stack[index]];
          if (token.char === "~" && (opener.remaining !== 2 || token.remaining !== 2)) continue;
          const sum = opener.count + token.count;
          if (token.char !== "~" && (opener.canClose || token.canOpen) && sum % 3 === 0 && !(opener.count % 3 === 0 && token.count % 3 === 0)) continue;
          found = index;
          break;
        }
        if (found < 0) {
          bottom.set(key, stack.length ? stack[stack.length - 1] : -1);
          break;
        }
        const openerPosition = stack[found];
        const opener = tokens[openerPosition];
        const use = token.char === "~" ? 2 : opener.remaining >= 2 && token.remaining >= 2 ? 2 : 1;
        const kind = token.char === "~" ? "del" : use === 2 ? "strong" : "em";
        opener.remaining -= use;
        token.remaining -= use;
        opener.opens.push(kind);
        token.closes.push(kind);
        stack.length = opener.remaining > 0 ? found + 1 : found;
        // Delimiters inside a matched pair can no longer open, whatever their kind.
        for (const other of Object.values(stacks)) {
          while (other.length && other[other.length - 1] > openerPosition) other.pop();
        }
      }
    }
    if (token.canOpen && token.remaining > 0) stack.push(position);
  });
}

function parseInline(text, allowLinks) {
  const tokens = tokenize(text, allowLinks);
  matchEmphasis(tokens);
  const root = { flat: false, children: [] };
  const stack = [root];
  for (const token of tokens) {
    if (token.type !== "delim") {
      stack[stack.length - 1].children.push(token);
      continue;
    }
    for (const kind of token.closes) {
      const frame = stack.pop();
      if (!frame.flat) stack[stack.length - 1].children.push({ type: kind, children: frame.children });
    }
    if (token.remaining) stack[stack.length - 1].children.push({ type: "text", value: token.char.repeat(token.remaining) });
    // Later matches are outer, so opening kinds are used from the last.
    for (let index = token.opens.length - 1; index >= 0; index -= 1) {
      const kind = token.opens[index];
      const parent = stack[stack.length - 1];
      // Past the limit, emphasis is dropped and children go straight to the
      // deepest kept frame, so nothing is copied or spread later.
      stack.push(stack.length > MAX_INLINE_DEPTH ? { flat: true, children: parent.children } : { kind, flat: false, children: [] });
    }
  }
  return root.children;
}

function plainText(nodes) {
  return nodes.map((node) => node.type === "text" || node.type === "code" ? node.value : node.children ? plainText(node.children) : "").join("");
}

function renderInline(nodes, parent, doc) {
  for (const node of nodes) {
    if (node.type === "text") parent.append(doc.createTextNode(node.value));
    else if (node.type === "br") parent.append(doc.createElement("br"));
    else if (node.type === "code") {
      const code = doc.createElement("code");
      code.textContent = node.value;
      parent.append(code);
    } else if (node.type === "link") {
      const anchor = doc.createElement("a");
      anchor.setAttribute("href", node.href);
      if (node.title) anchor.setAttribute("title", node.title);
      anchor.setAttribute("rel", "noopener noreferrer");
      anchor.setAttribute("target", "_blank");
      renderInline(node.children, anchor, doc);
      parent.append(anchor);
    } else if (node.type === "group") renderInline(node.children, parent, doc);
    else {
      const element = doc.createElement(node.type);
      renderInline(node.children, element, doc);
      parent.append(element);
    }
  }
}
