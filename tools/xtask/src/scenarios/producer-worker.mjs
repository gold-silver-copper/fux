
import { readFileSync, writeFileSync, existsSync, unlinkSync } from 'node:fs';
const root = process.cwd();
const plugin = JSON.parse(process.env.OPENCODE_CONFIG_CONTENT).plugin.at(-1);
const { ZorOpenCode } = await import(plugin);
let hooks, generation = 0, count = 0, current = null, queued = Promise.resolve();
const session = 'fixture-root';
const response = value => ({ data: new ReadableStream({ start(controller) {
  controller.enqueue(new TextEncoder().encode(JSON.stringify(value))); controller.close();
} }) });
const client = { session: { get: async () => response({ id: session }),
  message: async () => response(current) } };
function state() { writeFileSync(root + '/worker.json', JSON.stringify({ generation, count, pid: process.pid })); }
async function reload() {
  await hooks?.dispose();
  if (generation === 2) process.env.HOME = root + '/different-home';
  hooks = await ZorOpenCode({ client });
  generation++; state();
}
function fatal(error) { writeFileSync(root + '/error', String(error.stack)); process.exit(1); }
await reload();
// A second live plugin cannot steal the endpoint or retire the registered owner.
let duplicateRejected = false;
try { await ZorOpenCode({ client }); } catch { duplicateRejected = true; }
if (!duplicateRejected) throw new Error('live endpoint replaced');
writeFileSync(root + '/duplicate-rejected', 'yes');
process.stdin.setRawMode(true); process.stdin.setEncoding('utf8'); process.stdin.resume();
let line = '';
process.stdin.on('data', chunk => {
  for (const char of chunk) {
    if (char !== '\r') { line += char; continue; }
    const text = line; line = '';
    queued = queued.then(async () => {
      const index = ++count, user = 'user-' + index, assistant = 'assistant-' + index;
      await hooks['chat.message']({ sessionID: session }, { message: { id: user, sessionID: session },
        parts: [{ type: 'text', text }] });
      const info = { id: assistant, role: 'assistant', parentID: user, sessionID: session,
        time: { created: index, completed: index + 1 }, finish: 'stop' };
      current = { info, parts: [{ messageID: assistant, sessionID: session, type: 'text', text: 'done' }] };
      if (text !== 'HOLD') {
        await hooks.event({ event: { type: 'message.updated', properties: { info } } });
        await hooks.event({ event: { type: 'session.idle', properties: { sessionID: session } } });
      }
      state();
    }).catch(fatal);
  }
});
setInterval(() => {
  if (!existsSync(root + '/reload')) return;
  unlinkSync(root + '/reload');
  queued = queued.then(reload).catch(fatal);
}, 25);
setTimeout(() => fatal(new Error('fixture lifetime exceeded')), 90000);
