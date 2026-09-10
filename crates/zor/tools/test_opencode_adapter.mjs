// Run with node; all subprocesses/configuration are disposable and task-owned.
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, rmSync, copyFileSync, chmodSync, mkdirSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { createConnection } from 'node:net';
import { ZorOpenCode } from '../integrations/opencode.mjs';

const manifest = fileURLToPath(new URL('./xtask/Cargo.toml', import.meta.url));
const target = fileURLToPath(new URL('./xtask/target', import.meta.url));
execFileSync('cargo', ['build', '--manifest-path', manifest, '--target-dir', target, '--locked', '--bin', 'zor-adapter-fixture'], { stdio: 'inherit', timeout: 120000 });
const root = mkdtempSync('/tmp/zoa-');
const previous = { ...process.env };
for (const key of Object.keys(process.env)) delete process.env[key];
Object.assign(process.env, { HOME: root, PATH: '/usr/bin:/bin:/opt/homebrew/bin',
  XDG_CONFIG_HOME: root + '/config', XDG_STATE_HOME: root + '/state', XDG_CACHE_HOME: root + '/cache',
  ZOR_STATE_DIRECTORY: root, ZOR_BIN: root + '/zor', ZOR_TASK_ID: 'fixture',
  ZOR_LAUNCH_ID: 'a'.repeat(32), ZOR_ADAPTER_SOCKET: root + '/adapter.sock' });
for (const name of ['config', 'state', 'cache']) mkdirSync(root + '/' + name);
copyFileSync(target + '/debug/zor-adapter-fixture', root + '/zor');
chmodSync(root + '/zor', 0o700);
let hooks;
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
function calls() { try { return readFileSync(root + '/calls', 'utf8').trim().split('\n').map(JSON.parse); } catch { return []; } }
async function until(check) {
  const end = performance.now() + 5000;
  while (performance.now() < end) { if (check()) return; await sleep(10); }
  assert.fail('adapter fixture deadline');
}
function rpc(request, drop = false) {
  return new Promise((resolve, reject) => {
    const socket = createConnection(process.env.ZOR_ADAPTER_SOCKET);
    let data = '';
    const timer = setTimeout(() => socket.destroy(new Error('test RPC deadline')), 3000);
    socket.on('error', reject);
    socket.on('close', () => clearTimeout(timer));
    socket.on('connect', () => {
      socket.write(JSON.stringify({ v: 1, marker: process.env.ZOR_LAUNCH_ID, ...request }) + '\n');
      if (drop) { socket.end(); resolve(); }
    });
    socket.on('data', chunk => { data += chunk; if (data.includes('\n')) { resolve(JSON.parse(data)); socket.end(); } });
  });
}
const native = { session: 'root-session', parentID: undefined, current: null, huge: false };
const response = value => ({ data: new ReadableStream({ start(controller) {
  controller.enqueue(new TextEncoder().encode(JSON.stringify(value))); controller.close();
} }) });
try {
  let pendingRead = null, pendingSessionRead = null;
  hooks = await ZorOpenCode({ client: { session: {
    get: async () => {
      const value = { id: native.session, parentID: native.parentID };
      if (pendingSessionRead) { const wait = pendingSessionRead; pendingSessionRead = null; await wait(); }
      return response(value);
    },
    message: async () => {
      const value = structuredClone(native.huge ? { huge: 'x'.repeat(131073) } : native.current);
      if (pendingRead) { const wait = pendingRead; pendingRead = null; await wait(); }
      return response(value);
    },
  } } });
  const hello = await rpc({ op: 'hello' });
  const producer = hello.producer;
  assert.equal(hello.status, 'ready');
  assert.deepEqual(hello.storage_environment, {
    HOME: root, XDG_CONFIG_HOME: root + '/config', XDG_STATE_HOME: root + '/state',
    XDG_CACHE_HOME: root + '/cache', XDG_DATA_HOME: null,
  });
  process.env.HOME = root + '/changed-after-start';
  assert.equal((await rpc({ op: 'hello' })).storage_environment.HOME, root);
  process.env.HOME = root;
  await until(() => calls().some(c => c[0] === 'heartbeat-adapter'));
  const pulses = () => calls().filter(c => c[0] === 'heartbeat-adapter');
  const latestObservation = () => {
    const pulse = pulses().at(-1), index = pulse?.indexOf('--observation') ?? -1;
    return index < 0 ? null : JSON.parse(pulse[index + 1]);
  };
  assert(pulses()[0].includes(producer) && pulses()[0].includes(process.env.ZOR_LAUNCH_ID));
  const prompt = index => ({ operation: 'op-' + index, token: String(index).padStart(32, '0'),
    input_operation: index, text: 'same prompt', deadline_ms: Date.now() + 30000 });
  const arm = p => rpc({ op: 'arm', producer, prompt: p });
  const consume = id => hooks['chat.message']({ sessionID: native.session }, {
    message: { id, sessionID: native.session }, parts: [{ type: 'text', text: 'same prompt' }],
  });
  const event = (type, properties) => hooks.event({ event: { type, properties } });
  const setup = (index, parts) => {
    const id = 'assistant-' + index, user = 'user-' + index;
    const info = { id, role: 'assistant', parentID: user, sessionID: native.session,
      time: { created: index, completed: index + 1 }, finish: 'stop' };
    native.current = { info, parts: parts.map(p => ({ messageID: id, sessionID: native.session, ...p })) };
    return { info, id };
  };
  const reports = () => calls().filter(c => c[0] === 'report');
  const idle = () => event('session.idle', { sessionID: native.session });

  const first = prompt(1);
  assert.equal((await arm(first)).status, 'armed');
  assert.equal((await arm(Object.fromEntries(Object.entries(first).reverse()))).status, 'armed',
    'lost acknowledgement retry with reordered JSON fields');
  assert.equal((await arm(prompt(2))).status, 'rejected', 'unconsumed arm overwritten');
  native.parentID = 'parent-session';
  await consume('child-message');
  assert.equal(calls().filter(c => c[0] === 'bind-report').length, 0, 'child consumed root arm');
  native.parentID = undefined;
  await consume('user-1');
  assert.equal((await arm(first)).status, 'rejected', 'consumed arm rearmed');
  const one = setup(1, [{ type: 'text', text: 'current text' }]);
  await event('message.updated', { info: one.info });
  await idle();
  await until(() => reports().length === 1);
  assert(reports()[0].includes('user-1') && reports()[0].includes('op-1'));
  await until(() => latestObservation()?.state === 'idle');
  assert.equal(latestObservation().message.id, 'user-1');
  await event('message.part.updated', { part: { messageID: one.id, sessionID: native.session, type: 'text', text: '' } });
  await until(() => latestObservation()?.state === 'unknown');
  await idle();
  await until(() => latestObservation()?.state === 'idle');
  await event('message.part.updated', { part: { messageID: one.id, sessionID: native.session, type: 'tool' } });
  await until(() => latestObservation()?.state === 'unknown');

  // A stale native callback cannot consume the newly armed identical text.
  assert.equal((await arm(prompt(2))).status, 'armed');
  await consume('user-1');
  await consume('user-2');
  const two = setup(2, [{ type: 'tool', state: { status: 'completed' } }]);
  // The tool part arrives before its message; the bounded complete-message read must catch it.
  await event('message.part.updated', { part: native.current.parts[0] });
  await event('message.updated', { info: two.info });
  await idle();
  await sleep(100);
  assert.equal(reports().length, 1, 'intermediate tool stop reported final');
  // Previously visible text subsequently removed must not establish a response.
  await event('message.part.updated', { part: { messageID: two.id, sessionID: native.session, type: 'text', text: 'old' } });
  native.current.parts = [{ messageID: two.id, sessionID: native.session, type: 'text', text: '' }];
  await idle();
  await sleep(100);
  assert.equal(reports().length, 1, 'removed text reported');
  native.current.parts[0].text = 'new final';
  native.current.info.parentID = 'old-unrelated-user';
  await idle();
  await sleep(100);
  assert.equal(reports().length, 1, 'foreign ancestry reported');
  native.current.info.parentID = 'user-2';
  native.huge = true;
  await idle();
  await sleep(100);
  assert.equal(reports().length, 1, 'oversized native response accepted');
  native.huge = false;
  await idle();
  await until(() => reports().length === 2);
  assert(reports()[1].includes('user-2') && reports()[1].includes('op-2'));
  await idle();
  await sleep(100);
  assert.equal(reports().length, 2, 'duplicate terminal report');

  assert.equal((await arm(prompt(3))).status, 'armed');
  await consume('user-3');
  const three = setup(3, []);
  await event('message.updated', { info: three.info });
  await event('permission.asked', { sessionID: 'child-session', tool: { messageID: three.id } });
  await sleep(50);
  assert.equal(reports().length, 2);
  await event('permission.asked', { sessionID: native.session, tool: { messageID: three.id } });
  await until(() => reports().length === 3);
  assert(reports()[2].includes('needs-input'));

  // A second idle during an older native read must still check the newer final message.
  assert.equal((await arm(prompt(4))).status, 'armed');
  await consume('user-4');
  const four = setup(4, [{ type: 'text', text: 'older candidate' }]);
  await event('message.updated', { info: four.info });
  let releaseRead;
  pendingRead = () => new Promise(resolve => { releaseRead = resolve; });
  await idle();
  await until(() => !!releaseRead);
  native.current = { info: { ...four.info, id: 'newer-assistant-4', time: { created: 5, completed: 6 } },
    parts: [{ messageID: 'newer-assistant-4', sessionID: native.session, type: 'text', text: 'final candidate' }] };
  await event('message.updated', { info: native.current.info });
  await idle();
  releaseRead();
  await until(() => reports().length === 4);
  assert(reports()[3].includes('user-4') && reports()[3].includes('op-4'));

  // Newer contradictory evidence for the same message invalidates a delayed fetch.
  for (const [offset, kind] of ['tool', 'subtask', 'error', 'incomplete'].entries()) {
    const number = 90 + offset;
    assert.equal((await arm(prompt(number))).status, 'armed');
    await consume(`user-${number}`);
    const candidate = setup(number, [{ type: 'text', text: 'stale final snapshot' }]);
    await event('message.updated', { info: candidate.info });
    let releaseStale;
    pendingRead = () => new Promise(resolve => { releaseStale = resolve; });
    await idle();
    await until(() => !!releaseStale);
    if (kind === 'tool' || kind === 'subtask') {
      await event('message.part.updated', { part: { messageID: candidate.id, sessionID: native.session, type: kind } });
    } else {
      await event('message.updated', { info: { ...candidate.info,
        ...(kind === 'error' ? { error: { message: 'failed' } } : { time: { created: number } }) } });
    }
    releaseStale();
    await sleep(300);
    assert.equal(reports().length, 4, `${kind} did not invalidate delayed response snapshot`);
  }

  // Complete-frame and peer-token requirements fail closed without replacing an arm.
  assert.equal((await rpc({ op: 'arm', producer, marker: 'bad', prompt: prompt(5) })).status, 'rejected');
  const fifth = prompt(5);
  assert.equal((await arm(fifth)).status, 'armed');
  const socket = createConnection(process.env.ZOR_ADAPTER_SOCKET);
  socket.on('error', () => {});
  socket.on('connect', () => socket.write('{'));
  await until(() => socket.destroyed);
  assert.equal((await arm(prompt(6))).status, 'rejected');
  const disarm = p => rpc({ op: 'disarm', producer, prompt: p });
  assert.equal((await disarm({ ...fifth, token: 'f'.repeat(32) })).status, 'rejected');
  assert.equal((await disarm(Object.fromEntries(Object.entries(fifth).reverse()))).status, 'disarmed',
    'retirement identity changed with JSON field order');
  assert.equal((await disarm(fifth)).status, 'disarmed', 'lost disarm acknowledgement retry');
  assert.equal((await arm(fifth)).status, 'rejected', 'retired arm resurrected');
  const sixth = prompt(6);
  assert.equal((await arm(sixth)).status, 'armed');
  const before = calls().filter(c => c[0] === 'bind-report').length;
  let releaseSession;
  pendingSessionRead = () => new Promise(resolve => { releaseSession = resolve; });
  const oldChat = consume('user-6');
  await until(() => !!releaseSession);
  assert.equal((await disarm(sixth)).status, 'disarmed');
  assert.equal((await arm(prompt(7))).status, 'armed');
  releaseSession();
  await oldChat;
  assert.equal(calls().filter(c => c[0] === 'bind-report').length, before, 'retired callback bound newer arm');
  await consume('user-7');
  assert.equal(calls().filter(c => c[0] === 'bind-report').length, before + 1);
  const neverArrived = prompt(8);
  assert.equal((await disarm(neverArrived)).status, 'disarmed');
  assert.equal((await arm(neverArrived)).status, 'rejected', 'delayed arm passed tombstone');
  assert.equal((await arm(prompt(9))).status, 'armed');
  assert.equal((await disarm(fifth)).status, 'disarmed', 'old retirement retry failed');
  await consume('user-9');
  assert(calls().some(c => c[0] === 'bind-report' && c.includes('user-9') && c.includes('op-9')),
    'old retirement cleared a different arm');
  assert.equal((await arm(prompt(10))).status, 'armed');
  await consume('user-10');
  const ten = setup(10, []);
  await event('message.updated', { info: ten.info });
  for (const properties of [
    { sessionID: 'child-session', tool: { messageID: ten.id } },
    { sessionID: native.session },
    { sessionID: native.session, tool: { messageID: 'unknown-assistant' } },
    { sessionID: native.session, tool: { messageID: three.id } },
  ]) await event('question.asked', properties);
  await sleep(100);
  assert.equal(reports().length, 4, 'unbound, child, or old question produced a report');
  await event('question.asked', { sessionID: native.session, tool: { messageID: ten.id } });
  await until(() => reports().length === 5);
  assert(reports()[4].includes('needs-input') && reports()[4].includes('user-10') && reports()[4].includes('op-10'));
  await event('question.asked', { sessionID: native.session, tool: { messageID: ten.id } });
  await idle();
  await sleep(100);
  assert.equal(reports().length, 5, 'question duplicated or replaced its terminal report');
  await until(() => latestObservation()?.state === 'blocked');
  assert.equal(latestObservation().operation, 'op-10');
  await event('question.replied', { sessionID: native.session });
  await until(() => latestObservation()?.state === 'unknown');
  const continued = setup(11, [{ type: 'text', text: 'response after question' }]);
  native.current.info.parentID = 'user-10';
  delete native.current.info.time.completed;
  await event('message.updated', { info: native.current.info });
  await until(() => latestObservation()?.state === 'working');
  native.current.info.time.completed = 12;
  await event('message.updated', { info: native.current.info });
  await idle();
  await until(() => latestObservation()?.state === 'idle');
  assert.equal(reports().length, 5, 'current state rewrote historical NeedsInput report');
  await consume('human-input-without-arm');
  const priorPulse = pulses().length;
  await until(() => pulses().length > priorPulse && latestObservation() === null);
  assert.equal((await arm(prompt(12))).status, 'armed');
  writeFileSync(root + '/binding-delay', '');
  const delayedBinding = consume('user-12');
  await until(() => calls().some(c => c[0] === 'bind-report' && c.includes('user-12')));
  await consume('newer-unarmed-root-input');
  await delayedBinding;
  const afterBinding = pulses().length;
  await until(() => pulses().length > afterBinding);
  assert.equal(latestObservation(), null, 'late binding resurrected state after newer input');
  rmSync(root + '/binding-delay');
  writeFileSync(root + '/binding-hold', '');
  assert.equal((await arm(prompt(13))).status, 'armed');
  const oldBinding = consume('user-13');
  await until(() => calls().some(c => c[0] === 'bind-report' && c.includes('user-13')));
  assert.equal((await arm(prompt(14))).status, 'armed');
  await consume('user-14');
  const fourteen = setup(14, [{ type: 'text', text: 'newer response' }]);
  await event('message.updated', { info: fourteen.info });
  await idle();
  await until(() => reports().length === 6);
  rmSync(root + '/binding-hold');
  await oldBinding;
  await until(() => latestObservation()?.operation === 'op-14' && latestObservation()?.state === 'idle');
  await event('message.part.updated', { part: { sessionID: native.session,
    messageID: fourteen.id, type: 'text', text: '' } });
  await until(() => latestObservation()?.operation === 'op-14' && latestObservation()?.state === 'unknown');
  assert.equal(reports().length, 6, 'late binding cleanup changed historical report');
  await until(() => pulses().length >= 2);
  assert(pulses().every((pulse, index) => Number(pulse[pulse.indexOf('--sequence') + 1]) === index + 1));
  writeFileSync(root + '/heartbeat-delay', '');
  const beforeDelay = pulses().length;
  await until(() => pulses().length === beforeDelay + 1);
  await sleep(2200);
  assert.equal(pulses().length, beforeDelay + 1, 'slow heartbeat spawned overlapping children');
  assert.equal((await rpc({ op: 'hello' })).status, 'ready', 'heartbeat stalled adapter RPC');
  await hooks.dispose();
  const stoppedPulses = pulses().length;
  await sleep(2100);
  assert.equal(pulses().length, stoppedPulses, 'heartbeat continued after disposal');
  console.log('OpenCode adapter tests passed');
} finally {
  await hooks?.dispose?.();
  await sleep(100);
  for (const key of Object.keys(process.env)) delete process.env[key];
  Object.assign(process.env, previous);
  rmSync(root, { recursive: true, force: true });
}
