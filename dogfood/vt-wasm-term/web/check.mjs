// Headless check: node check.mjs MODULE.wasm RECORDING [ROWS COLS [CHUNK]]
// Plays a recording (.cast or raw output) through the module, a chunk a
// frame (raw output cut into CHUNK bytes, default 256), and prints the
// final screen with what the frames held.
import { readFileSync } from "node:fs";
import { load, Term, parseRecording } from "./term.js";

const [wasmPath, recPath, rows = "24", cols = "80", chunk = "256"] = process.argv.slice(2);
if (!wasmPath || !recPath) {
  console.error("usage: node check.mjs MODULE.wasm RECORDING [ROWS COLS]");
  process.exit(2);
}
const wasm = await load(readFileSync(wasmPath));
const rec = parseRecording(new Uint8Array(readFileSync(recPath)));
if (rec.rows === null) {
  const [[, all]] = rec.events;
  rec.events = [];
  for (let i = 0; i < all.length; i += Number(chunk)) rec.events.push([0, all.subarray(i, i + Number(chunk))]);
}
const term = new Term(wasm, rec.rows || Number(rows), rec.cols || Number(cols));
let frames = 0, full = 0, rowsSent = 0, bytes = 0;
const colours = new Map();
const started = performance.now();
for (const [, data] of rec.events) {
  term.feed(data);
  bytes += data.length;
  const frame = term.render();
  frames++;
  if (frame.full) full++;
  rowsSent += frame.changed.length;
  for (const { runs } of frame.changed) for (const r of runs) if (r.text.trim()) colours.set(r.fg, r.text.trim());
}
const ms = (performance.now() - started).toFixed(1);
const [r, c] = term.size;
console.log(`${bytes} bytes in ${rec.events.length} chunk(s) -> ${frames} frames (${full} full), ${rowsSent} rows sent, ${ms} ms`);
console.log(`screen ${r}x${c}, cursor ${term.cursor.join(",")}, title ${JSON.stringify(term.title)}, ${term.replies} reply bytes, ${wasm.vt_refused()} refused`);
console.log(`${colours.size} foreground colours, e.g. ${[...colours].slice(0, 4).map(([k, v]) => `${k} "${v.slice(0, 12)}"`).join("; ")}`);
console.log("+" + "-".repeat(c) + "+");
for (const line of term.text()) console.log("|" + line.padEnd(c) + "|");
console.log("+" + "-".repeat(c) + "+");
