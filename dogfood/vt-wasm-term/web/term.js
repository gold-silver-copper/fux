// The page's side of vt-wasm-term: load the module, feed it bytes, read
// its frames, paint them. Runs in a browser and in node (no DOM needed
// unless paint is called).

export async function load(wasmBytes) {
  const { instance } = await WebAssembly.instantiate(wasmBytes, {});
  return instance.exports;
}

export class Term {
  constructor(exports, rows, cols, historyKib = 1024) {
    this.x = exports;
    if (!this.x.vt_new(rows, cols, historyKib)) throw new Error(`no terminal of ${rows}x${cols}`);
    this.lines = [];
    this.cursor = [0, 0];
    this.title = "";
    this.replies = 0;
  }

  // Feeds output bytes; gives the replies a program would get, or null.
  feed(bytes) {
    const ptr = this.x.vt_input(bytes.length);
    new Uint8Array(this.x.memory.buffer, ptr, bytes.length).set(bytes);
    const n = this.x.vt_feed(bytes.length);
    if (!n) return null;
    this.replies += n;
    return new Uint8Array(this.x.memory.buffer, this.x.vt_replies(), n).slice();
  }

  resize(rows, cols) {
    return this.x.vt_resize(rows, cols) === 1;
  }

  // Reads a frame: the rows changed since the last one.
  render() {
    const len = this.x.vt_render();
    const view = new DataView(this.x.memory.buffer, this.x.vt_frame(), len);
    const bytes = new Uint8Array(this.x.memory.buffer, this.x.vt_frame(), len);
    const utf8 = new TextDecoder();
    let at = 0;
    const u16 = () => { const n = view.getUint16(at, true); at += 2; return n; };
    const u8 = () => view.getUint8(at++);
    const rgb = () => { const c = `rgb(${bytes[at]},${bytes[at + 1]},${bytes[at + 2]})`; at += 3; return c; };
    const text = () => { const n = u16(); const t = utf8.decode(bytes.subarray(at, at + n)); at += n; return t; };
    const frame = { rows: u16(), cols: u16(), cursor: [u16(), u16()] };
    const flags = u8();
    frame.cursorShown = (flags & 1) !== 0;
    frame.full = (flags & 2) !== 0;
    frame.bg = rgb();
    frame.title = text();
    frame.changed = [];
    for (let r = u16(); r > 0; r--) {
      const row = u16();
      const runs = [];
      for (let n = u16(); n > 0; n--) {
        runs.push({ fg: rgb(), bg: rgb(), attrs: u8(), col: u16(), cols: u16(), text: text() });
      }
      frame.changed.push({ row, runs });
    }
    if (frame.full) this.lines.length = frame.rows;
    for (const { row, runs } of frame.changed) this.lines[row] = runs;
    this.cursor = frame.cursor;
    this.title = frame.title;
    this.size = [frame.rows, frame.cols];
    return frame;
  }

  // The screen as text, a line a row.
  text() {
    return Array.from(this.lines, (runs) => (runs || []).map((r) => r.text).join("").trimEnd());
  }
}

// An asciinema v2 cast's output events as [seconds, bytes], and its size;
// anything else is taken as raw output (`script`'s typescript).
export function parseRecording(bytes) {
  const enc = new TextEncoder();
  const head = new TextDecoder().decode(bytes.subarray(0, 1));
  if (head === "{") {
    const lines = new TextDecoder().decode(bytes).split("\n").filter((l) => l.trim());
    const header = JSON.parse(lines[0]);
    const events = [];
    for (const line of lines.slice(1)) {
      const [t, kind, data] = JSON.parse(line);
      if (kind === "o") events.push([t, enc.encode(data)]);
    }
    return { rows: header.height, cols: header.width, events };
  }
  let start = 0;
  const firstLine = new TextDecoder().decode(bytes.subarray(0, 64));
  if (firstLine.startsWith("Script started")) start = bytes.indexOf(10) + 1;
  // And its "Script done on ..." trailer.
  let end = bytes.length;
  const tail = new TextDecoder().decode(bytes.subarray(Math.max(start, end - 200)));
  const done = tail.lastIndexOf("\nScript done on ");
  if (done >= 0) end -= new TextEncoder().encode(tail.slice(done)).length;
  return { rows: null, cols: null, events: [[0, bytes.subarray(start, end)]] };
}

const BOLD = 1, ITALIC = 2, UNDERLINE = 4, STRIKE = 8, DIM = 16, WIDE = 32;

// Paints a frame's changed rows into `screen`, a monospace element.
export function paint(screen, frame) {
  const doc = screen.ownerDocument;
  screen.style.background = frame.bg;
  if (frame.full) {
    screen.replaceChildren();
    for (let r = 0; r < frame.rows; r++) {
      const div = doc.createElement("div");
      div.className = "row";
      screen.append(div);
    }
    const cursor = doc.createElement("div");
    cursor.className = "cursor";
    screen.append(cursor);
  }
  for (const { row, runs } of frame.changed) {
    const div = screen.children[row];
    if (!div) continue;
    div.replaceChildren(...runs.map((run) => {
      const span = doc.createElement("span");
      span.textContent = run.text;
      const s = span.style;
      s.color = run.fg;
      s.background = run.bg;
      if (run.attrs & WIDE) s.width = `${run.cols}ch`;
      if (run.attrs & BOLD) s.fontWeight = "bold";
      if (run.attrs & ITALIC) s.fontStyle = "italic";
      if (run.attrs & DIM) s.opacity = "0.6";
      const lines = [];
      if (run.attrs & UNDERLINE) lines.push("underline");
      if (run.attrs & STRIKE) lines.push("line-through");
      if (lines.length) s.textDecoration = lines.join(" ");
      return span;
    }));
  }
  const cursor = screen.querySelector(".cursor");
  if (cursor) {
    cursor.style.display = frame.cursorShown ? "block" : "none";
    cursor.style.left = `${frame.cursor[1]}ch`;
    cursor.style.top = `calc(${frame.cursor[0]} * var(--line))`;
  }
}
