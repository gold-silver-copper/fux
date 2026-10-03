// xterm.js, as @xterm/headless, served to fux-vt-compare: one process for
// many terminals, one JSON object per line each way. src/engines/xterm_js.rs
// starts it and says how it is driven and read.
//
// Requests, each answered by one line, {"ok":true,...} or
// {"ok":false,"error":"..."}, but for a write without `ack` and a dispose,
// which are never answered:
//
//   {"op":"create","id":N,"rows":R,"cols":C}
//   {"op":"write","id":N,"data":"<base64>","ack":true}  answered once
//       xterm.js has parsed and applied the bytes
//   {"op":"resize","id":N,"rows":R,"cols":C}
//   {"op":"snapshot","id":N,"history":H}
//   {"op":"dispose","id":N}
//
// Requests are handled in order, each after the one before has finished;
// a write without `ack` is only queued, and xterm.js applies queued writes
// in order, so the answer to the last write of a run means all are done.
// A request that is not answered and fails makes the next answer its error.
import headless from '@xterm/headless';
import unicodeGraphemes from '@xterm/addon-unicode-graphemes';
import { writeSync } from 'node:fs';

const { Terminal } = headless;

// History kept, in rows: far more than fux-vt keeps in any case.
const SCROLLBACK = 100000;
// Bytes written but not yet applied past which input is paused, so a long
// workload streamed without acknowledgements never makes xterm.js discard
// data (it does past 50 MB pending).
const HIGH_WATER = 16 << 20;

const terminals = new Map();
let pending = 0;

// A cell's colour: -1 the default, 0-255 a palette entry, 0x1000000 plus
// the RGB value for a direct colour.
function fg(c) {
  if (c.isFgDefault()) return -1;
  return c.isFgRGB() ? 0x1000000 + c.getFgColor() : c.getFgColor();
}

function bg(c) {
  if (c.isBgDefault()) return -1;
  return c.isBgRGB() ? 0x1000000 + c.getBgColor() : c.getBgColor();
}

// A cell with a hyperlink (OSC 8) reads as underlined, dashed, whatever its
// SGR says: xterm.js draws links so (its ExtendedAttrs' `underlineStyle` is
// 5 while `urlId` is set). Its SGR underline is then read from the core's
// own flag, which SGR 4, 4:n, 24 and 0 set and clear as `isUnderline`
// reads them on a cell without a link (not in the public API).
const FG_UNDERLINE = 0x10000000;

function underlined(c) {
  if (c.hasExtendedAttrs() && c.extended.urlId !== 0) return (c.fg & FG_UNDERLINE) !== 0;
  return Boolean(c.isUnderline());
}

function flags(c) {
  return (
    (c.isBold() ? 1 : 0) |
    (c.isDim() ? 2 : 0) |
    (c.isItalic() ? 4 : 0) |
    (underlined(c) ? 8 : 0) |
    (c.isBlink() ? 16 : 0) |
    (c.isInverse() ? 32 : 0) |
    (c.isInvisible() ? 64 : 0) |
    (c.isStrikethrough() ? 128 : 0)
  );
}

// A row y is soft-wrapped when the line after it continues it: xterm.js
// keeps the flag on the continuation.
function wrapped(buffer, y) {
  const next = buffer.getLine(y + 1);
  return next !== undefined && next.isWrapped;
}

// A cell's hyperlink (OSC 8), as [uri, link id], or null. Not in the public
// API: the cell's ExtendedAttrs hold the link's number (`urlId`, 0 for
// none), and the core's OscLinkService its URI. The service numbers each
// OSC 8 without an id anew, and gives one with an id and a URI it has seen
// the same number, so cells with the same number are one link. A cell
// loaded without extended attributes keeps the last one's, so the flag is
// asked first.
function link(term, c) {
  if (!c.hasExtendedAttrs()) return null;
  const id = c.extended.urlId;
  if (!id) return null;
  const data = term._core._oscLinkService.getLinkData(id);
  if (data === undefined) throw new Error(`no link data for link ${id}`);
  return [data.uri, id];
}

// A row as cells: [text, width (xterm.js's: 0 for the second half of a wide
// glyph), fg, bg, flags, link].
function cells(term, line, cols, cell) {
  const out = [];
  for (let x = 0; x < cols; x++) {
    const c = line === undefined ? undefined : line.getCell(x, cell);
    if (c === undefined) {
      out.push(['', 1, -1, -1, 0, null]);
    } else {
      out.push([c.getChars(), c.getWidth(), fg(c), bg(c), flags(c), link(term, c)]);
    }
  }
  return out;
}

// A history row as text, a blank as a space, with no trailing spaces: what
// Line::text makes of it on the Rust side.
function text(line, cols, cell) {
  let out = '';
  for (let x = 0; x < cols; x++) {
    const c = line.getCell(x, cell);
    if (c === undefined || c.getWidth() === 0) continue;
    const chars = c.getChars();
    out += chars === '' ? ' ' : chars;
  }
  return out.replace(/ +$/, '');
}

// ED 1 (erase above) with the cursor in the last column clears the soft-wrap
// flag of buffer line y + 1, without adding the history above the screen:
// the wrong line once there is history, and none at all on the last row
// before there is any, where xterm.js 6.0.0 throws (`Cannot set properties
// of undefined`) before erasing the rows above, and its write never ends.
// Here that missing line reads as a stand-in, so xterm.js does all it means
// to do; the wrong line it clears otherwise is left as it is.
function guardEraseAbove(term) {
  const handler = term._core._inputHandler;
  const erase = handler.eraseInDisplay;
  if (typeof erase !== 'function') throw new Error('xterm.js has no eraseInDisplay');
  handler.eraseInDisplay = function (params, ...rest) {
    const buffer = this._activeBuffer;
    const lines = buffer.lines;
    const get = lines.get;
    const below = buffer.y + 1;
    lines.get = (i) => get.call(lines, i) ?? (i === below ? { isWrapped: false } : undefined);
    try {
      return erase.call(this, params, ...rest);
    } finally {
      delete lines.get;
    }
  };
}

function create(req) {
  const term = new Terminal({
    rows: req.rows,
    cols: req.cols,
    scrollback: SCROLLBACK,
    // The cursor's own line is reflowed too, as fux-vt and Ghostty do. Off
    // (the default), xterm.js cuts it at the new width instead, leaving it
    // to a shell to redraw.
    reflowCursorLine: true,
    // The graphemes addon needs it.
    allowProposedApi: true,
    // xterm.js logs every sequence it cannot parse to the console.
    logLevel: 'off',
  });
  // Widths by grapheme cluster, from Unicode 15, as fux-vt measures them
  // (and Ghostty with mode 2027). Without it xterm.js measures each code
  // point by Unicode 6, where most emoji are narrow.
  term.loadAddon(new unicodeGraphemes.UnicodeGraphemesAddon());
  guardEraseAbove(term);
  const t = { term, title: '', replies: '' };
  term.onTitleChange((title) => {
    t.title = title;
  });
  term.onData((data) => {
    t.replies += data;
  });
  terminals.set(req.id, t);
  return {};
}

function snapshot(t, req) {
  const term = t.term;
  const buffer = term.buffer.active;
  const cell = buffer.getNullCell();
  const base = buffer.baseY;
  const screen = [];
  for (let y = 0; y < term.rows; y++) {
    screen.push({
      w: wrapped(buffer, base + y),
      c: cells(term, buffer.getLine(base + y), term.cols, cell),
    });
  }
  const history = [];
  for (let y = Math.max(0, base - req.history); y < base; y++) {
    const line = buffer.getLine(y);
    if (line !== undefined) {
      history.push([text(line, term.cols, cell), wrapped(buffer, y)]);
    }
  }
  const modes = term.modes;
  return {
    rows: term.rows,
    cols: term.cols,
    x: buffer.cursorX,
    y: buffer.cursorY,
    // Not in the public API: the core's own flag, set by DECTCEM.
    hidden: term._core.coreService.isCursorHidden === true,
    autowrap: modes.wraparoundMode,
    origin: modes.originMode,
    alternate: buffer.type === 'alternate',
    application_cursor: modes.applicationCursorKeysMode,
    application_keypad: modes.applicationKeypadMode,
    bracketed_paste: modes.bracketedPasteMode,
    synchronized_output: modes.synchronizedOutputMode,
    focus_reporting: modes.sendFocusMode,
    title: t.title,
    replies: t.replies,
    screen,
    history,
  };
}

function write(t, req) {
  const bytes = Buffer.from(req.data, 'base64');
  pending += bytes.length;
  if (pending > HIGH_WATER) process.stdin.pause();
  return new Promise((resolve) => {
    t.term.write(bytes, () => {
      pending -= bytes.length;
      if (pending <= HIGH_WATER) process.stdin.resume();
      resolve(req.ack ? {} : undefined);
    });
    if (!req.ack) resolve(undefined);
  });
}

async function handle(req) {
  if (req.op === 'create') return create(req);
  const t = terminals.get(req.id);
  if (t === undefined) throw new Error(`no terminal ${req.id}`);
  switch (req.op) {
    case 'write':
      return write(t, req);
    case 'resize':
      t.term.resize(req.cols, req.rows);
      return {};
    case 'snapshot':
      return snapshot(t, req);
    case 'dispose':
      t.term.dispose();
      terminals.delete(req.id);
      return undefined;
    default:
      throw new Error(`no request ${req.op}`);
  }
}

// The failure of a request that is not answered (a write without `ack`, a
// dispose), given as the next answer instead of it.
let failed;

function answered(req) {
  return req.op !== 'dispose' && !(req.op === 'write' && !req.ack);
}

function send(object) {
  process.stdout.write(JSON.stringify(object) + '\n');
}

let chain = Promise.resolve();
function request(line) {
  if (line.length === 0) return;
  chain = chain.then(async () => {
    let req = { op: 'unknown' };
    try {
      req = JSON.parse(line);
      const reply = await handle(req);
      if (!answered(req)) return;
      if (failed !== undefined) {
        send({ ok: false, error: failed });
        failed = undefined;
      } else {
        send({ ok: true, ...reply });
      }
    } catch (e) {
      const error = `${req.op}: ${e}`;
      if (answered(req)) {
        send({ ok: false, error: failed ?? error });
        failed = undefined;
      } else {
        failed ??= error;
      }
    }
  });
}

process.on('uncaughtException', (e) => {
  writeSync(2, `xterm.js engine: ${e.stack}\n`);
  process.exit(1);
});

// Lines can be megabytes long (a workload streamed in large writes), so
// the pieces of a line are kept and joined once.
let pieces = [];
process.stdin.on('data', (chunk) => {
  let start = 0;
  for (;;) {
    const end = chunk.indexOf(10, start);
    if (end < 0) break;
    pieces.push(chunk.subarray(start, end));
    request(Buffer.concat(pieces).toString('utf8'));
    pieces = [];
    start = end + 1;
  }
  if (start < chunk.length) pieces.push(chunk.subarray(start));
});
process.stdin.on('end', () => {
  chain.then(() => process.exit(0));
});
