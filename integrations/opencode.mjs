// Zor-owned OpenCode integration. No terminal ownership or task-success inference.
import { createServer } from 'node:net';
import { randomBytes, createHash } from 'node:crypto';
import { chmodSync, lstatSync } from 'node:fs';
import { dirname, isAbsolute } from 'node:path';
import { spawn } from 'node:child_process';

const identifier = value => typeof value === 'string' && /^[A-Za-z0-9_-]{1,64}$/.test(value);
// Identity is the protocol's fields, independent of JSON object insertion order.
const digest = p => createHash('sha256').update(JSON.stringify(
  [p.operation, p.token, p.input_operation, p.text, p.deadline_ms])).digest('hex');
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

async function boundedData(method, options) {
  const result = await method({ ...options, parseAs: 'stream', signal: AbortSignal.timeout(2000) });
  const reader = result.data?.getReader?.();
  if (!reader) throw new Error('native response unavailable');
  let size = 0;
  const chunks = [];
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > 131072) throw new Error('native response limit');
      chunks.push(value);
    }
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(chunks)));
  } finally { await reader.cancel().catch(() => {}); }
}

function command(binary, root, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(binary, ['--state-directory', root, 'task', ...args], { stdio: ['ignore', 'pipe', 'pipe'] });
    let output = '', error = '', size = 0, failure;
    const timer = setTimeout(() => { failure = new Error('zor adapter command timed out'); child.kill('SIGKILL'); }, 6500);
    const collect = (chunk, stderr) => {
      size += chunk.length;
      if (size > 65536) { failure = new Error('zor adapter command output limit'); child.kill('SIGKILL'); return; }
      if (stderr) error += chunk.toString(); else output += chunk.toString();
    };
    child.stdout.on('data', chunk => collect(chunk, false));
    child.stderr.on('data', chunk => collect(chunk, true));
    child.on('error', value => { failure = value; });
    child.on('close', code => {
      clearTimeout(timer);
      if (failure) reject(failure);
      else if (code !== 0) reject(new Error(error.slice(0, 1024)));
      else { try { resolve(JSON.parse(output)); } catch (error) { reject(error); } }
    });
  });
}

export const ZorOpenCode = async ({ client }) => {
  const env = process.env;
  // Snapshot the running application's namespace, not the controller's environment.
  // Missing XDG variables stay null so a later launch can explicitly unset them.
  const keys = ['HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME'];
  let storageEnvironment = Object.fromEntries(keys.map(key => [key, env[key] ?? null]));
  const paths = Object.values(storageEnvironment).filter(value => value !== null);
  if (!storageEnvironment.HOME || paths.some(value => !value || !isAbsolute(value) || /[\x00-\x1f\x7f-\x9f]/.test(value))
      || paths.reduce((sum, value) => sum + Buffer.byteLength(value), 0) > 2048
      || Buffer.byteLength(JSON.stringify(storageEnvironment)) > 3072
      || env.OPENCODE_TEST_HOME) storageEnvironment = null;

  const binary = env.ZOR_BIN, root = env.ZOR_STATE_DIRECTORY, path = env.ZOR_ADAPTER_SOCKET;
  const launch = env.ZOR_TASK_ID, marker = env.ZOR_LAUNCH_ID;
  if (!binary || !root || !path || !identifier(launch) || !/^[a-f0-9]{32}$/.test(marker ?? '')) return {};
  const parent = lstatSync(dirname(path));
  if (!parent.isDirectory() || parent.uid !== process.getuid() || (parent.mode & 0o077)) throw new Error('unsafe zor adapter directory');
  const producer = randomBytes(16).toString('hex');
  let armed = null, sequence = 0, nativeSession = null, stopped = false, pending = 0, connections = 0;
  let current = null, currentState = 'unknown', observationEpoch = 0, inputEpoch = 0;
  let queue = Promise.resolve();
  const history = new Map(), entries = new Map(), assistants = new Map();
  const next = () => { if (++sequence >= Number.MAX_SAFE_INTEGER) throw new Error('adapter sequence exhausted'); return sequence; };
  const call = async args => {
    const until = performance.now() + 10000;
    while (true) {
      try { return await command(binary, root, args); }
      catch (error) {
        if (performance.now() >= until || !/journal is busy|managed launch is not ready/.test(String(error))) throw error;
        await sleep(25);
      }
    }
  };
  const enqueue = action => {
    if (stopped || pending >= 32) return false;
    pending++;
    queue = queue.then(action).catch(() => {}).finally(() => { pending--; });
    return true;
  };
  const report = (entry, kind) => {
    if (entry.reported || entry.failed || !entry.bound) return;
    entry.reported = true;
    const seq = next();
    const p = entry.prompt;
    if (!enqueue(async () => {
      try {
        await call(['report', p.operation, '--token', p.token, '--producer', producer,
          '--sequence', String(seq), '--input-operation', String(p.input_operation),
          '--agent-session', entry.session, '--message', entry.id, '--kind', kind]);
      } catch { entry.failed = true; }
      finally {
        for (const [id, value] of assistants) if (value.entry === entry && entry !== current) assistants.delete(id);
      }
    })) entry.failed = true;
  };
  const considerResponse = entry => {
    if (entry.failed || (entry.reported && entry !== current) || !entry.bound) return;
    const state = assistants.get(entry.latest);
    if (!state?.info?.completed || state.info.finish !== 'stop' || state.info.error
        || state.info.summary || state.tool) return;
    if (entry.checking) { entry.recheck = true; return; }
    entry.checking = true;
    const candidate = entry.latest;
    const observedEpoch = observationEpoch;
    const messageVersion = state.version;
    if (!enqueue(async () => {
      try {
        // Read the complete current message with a byte cap: early tool parts or later
        // text removal must not be hidden by the order of incremental hook callbacks.
        const value = await boundedData(options => client.session.message(options),
          { path: { id: entry.session, messageID: candidate } });
        const info = value?.info, parts = value?.parts;
        if (entry.latest === candidate && assistants.get(candidate) === state
            && state.version === messageVersion && info?.id === candidate && info.role === 'assistant'
            && info.parentID === entry.id && info.sessionID === entry.session && info.time?.completed
            && info.finish === 'stop' && !info.error && !info.summary && Array.isArray(parts)
            && parts.length <= 256 && !parts.some(p => p.type === 'tool' || p.type === 'subtask')
            && parts.every(p => p.messageID === candidate && p.sessionID === entry.session)
            && parts.some(p => p.type === 'text' && typeof p.text === 'string' && p.text.trim()))
        {
          if (entry === current && observationEpoch === observedEpoch) currentState = 'idle';
          report(entry, 'response-observed');
        }
      } catch { /* Missing, oversized or stale evidence cannot produce a response report. */ }
      finally {
        entry.checking = false;
        if (entry.recheck) { entry.recheck = false; considerResponse(entry); }
      }
    })) { entry.checking = false; entry.failed = true; }
  };
  const server = createServer(socket => {
    if (connections >= 8) { socket.destroy(); return; }
    connections++;
    let data = Buffer.alloc(0), replied = false;
    const timer = setTimeout(() => socket.destroy(), 2000);
    socket.on('close', () => { clearTimeout(timer); connections--; });
    socket.on('error', () => {});
    socket.on('data', chunk => {
      if (replied) return;
      if (data.length + chunk.length > 131072) { socket.destroy(); return; }
      data = Buffer.concat([data, chunk]);
      if (!data.includes(10)) return;
      replied = true;
      let response = { v: 1, status: 'rejected' };
      try {
        const request = JSON.parse(data.toString());
        if (request.v !== 1 || request.marker !== marker || stopped) throw new Error('invalid peer');
        if (request.op === 'hello') response = { v: 1, producer, status: 'ready', storage_environment: storageEnvironment };
        else if (['arm', 'disarm'].includes(request.op) && request.producer === producer) {
          const p = request.prompt;
          if (!p || !identifier(p.operation) || !/^[a-fA-F0-9]{32}$/.test(p.token)
              || !Number.isSafeInteger(p.input_operation) || p.input_operation < 1
              || !Number.isSafeInteger(p.deadline_ms) || typeof p.text !== 'string'
              || Buffer.byteLength(p.text) > 65536 || !p.text.length || /[\x00-\x1f\x7f]/.test(p.text)) throw new Error('invalid arm');
          const hash = digest(p), old = history.get(p.operation);
          if (old && old.hash !== hash) throw new Error('arm identity changed');
          if (!old && history.size >= 1024) throw new Error('arm history limit');
          if (request.op === 'disarm') {
            // Retain a tombstone even if the original arm never arrived. Never clear
            // another operation, or let a delayed callback/retry resurrect this one.
            if (armed && digest(armed) === hash) armed = null;
            history.set(p.operation, { hash, consumed: old?.consumed ?? false, disarmed: true });
          } else {
            if (old?.consumed || old?.disarmed) throw new Error('arm already consumed or retired');
            if (armed && digest(armed) !== hash) throw new Error('unconsumed arm cannot be replaced');
            if (!old && Date.now() >= p.deadline_ms) throw new Error('arm deadline');
            armed = p;
            history.set(p.operation, { hash, consumed: false, disarmed: false });
          }
          response = { v: 1, producer, status: request.op === 'disarm' ? 'disarmed' : 'armed',
            operation: p.operation, token: p.token, input_operation: p.input_operation };
        }
      } catch {}
      socket.end(JSON.stringify(response) + '\n');
    });
  });
  // Never unlink an existing endpoint. After clean disposal a new producer may bind
  // and register; zor fences the old lifetime without replaying its armed prompts.
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(path, resolve); });
  chmodSync(path, 0o600);
  server.unref();
  try { await call(['register-adapter', launch, '--marker', marker, '--producer', producer]); }
  catch (error) { server.close(); throw error; }

  // Independent liveness sequence: a pulse never refreshes a prompt claim or advances
  // native binding/report order. Skip ticks while one bounded child is outstanding.
  let heartbeatSequence = 0, heartbeatPending = null;
  const heartbeat = () => {
    if (stopped || heartbeatPending || heartbeatSequence >= Number.MAX_SAFE_INTEGER) return;
    const observation = current && !current.failed ? { state: currentState,
      operation: current.prompt.operation, input_operation: current.prompt.input_operation,
      message: { session: current.session, id: current.id } } : null;
    heartbeatPending = command(binary, root, ['heartbeat-adapter', launch, '--marker', marker,
      '--producer', producer, '--sequence', String(++heartbeatSequence),
      ...(observation ? ['--observation', JSON.stringify(observation)] : [])])
      .catch(() => {}).finally(() => { heartbeatPending = null; });
  };
  heartbeat();
  const heartbeatTimer = setInterval(heartbeat, 2000);
  heartbeatTimer.unref();

  return {
    'chat.message': async (input, output) => {
      const selectedInputEpoch = ++inputEpoch;
      // Snapshot the arm before any await. A delayed callback must never read a newer arm.
      const selected = armed;
      const message = output?.message;
      if (input.sessionID === nativeSession) { current = null; currentState = 'unknown'; observationEpoch++; }
      if (!selected || !identifier(message?.id) || !identifier(message?.sessionID)
          || input.sessionID !== message.sessionID || entries.has(message.id)) return;
      let session;
      try { session = await boundedData(options => client.session.get(options), { path: { id: message.sessionID } }); }
      catch { return; }
      if (!session || session.id !== message.sessionID || session.parentID) return;
      if (nativeSession && nativeSession !== message.sessionID) return;
      if (armed !== selected) return;
      // Consume even mismatching root input: uncertainty must not leave an old arm reusable.
      armed = null;
      history.get(selected.operation).consumed = true;
      const parts = output.parts;
      if (!Array.isArray(parts) || parts.length !== 1 || parts[0].type !== 'text' || parts[0].text !== selected.text) return;
      nativeSession = message.sessionID;
      const { text: _, ...prompt } = selected;
      const entry = { prompt, id: message.id, session: message.sessionID, bound: false,
        reported: false, failed: false, latest: null, created: -1 };
      entries.set(message.id, entry);
      const seq = next();
      try {
        await call(['bind-report', prompt.operation, '--token', prompt.token, '--producer', producer,
          '--sequence', String(seq), '--input-operation', String(prompt.input_operation),
          '--agent-session', entry.session, '--message', entry.id]);
        entry.bound = true;
        if (inputEpoch === selectedInputEpoch) {
          for (const [id, value] of assistants) if (value.entry.reported) assistants.delete(id);
          current = entry;
          currentState = 'unknown';
          observationEpoch++;
        }
      } catch { entry.failed = true; }
    },
    event: async ({ event }) => {
      if (stopped) return;
      const props = event?.properties;
      if (!props) return;
      if ((props.sessionID ?? props.info?.sessionID ?? props.part?.sessionID) === current?.session) observationEpoch++;
      if (event.type === 'message.updated') {
        const info = props.info;
        const entry = entries.get(info?.parentID);
        if (!entry || (entry.reported && entry !== current) || entry.failed || !entry.bound || info.role !== 'assistant' || info.sessionID !== entry.session) return;
        if (!identifier(info.id) || !Number.isSafeInteger(info.time?.created)) { entry.failed = true; return; }
        let state = assistants.get(info.id);
        if (!state) {
          if (assistants.size >= 256) { entry.failed = true; return; }
          state = { entry, info: null, tool: false };
          assistants.set(info.id, state);
          if (info.time.created > entry.created) { entry.latest = info.id; entry.created = info.time.created; }
          else if (info.time.created === entry.created && entry.latest !== info.id) entry.failed = true;
        }
        state.version = Symbol();
        state.info = { finish: info.finish, completed: info.time?.completed, error: !!info.error, summary: !!info.summary };
        if (entry === current && entry.latest === info.id) {
          if (currentState === 'idle') currentState = 'unknown';
          if (info.error) currentState = 'unknown';
          else if (!info.time?.completed && currentState !== 'blocked') currentState = 'working';
        }
      } else if (event.type === 'message.part.updated') {
        const part = props.part, state = assistants.get(part?.messageID);
        if (!state || part.sessionID !== state.entry.session) return;
        if (state.entry === current && state.entry.latest === part.messageID && currentState === 'idle') currentState = 'unknown';
        state.version = Symbol();
        if (part.type === 'tool' || part.type === 'subtask') state.tool = true;
      } else if (event.type === 'session.error') {
        if (current?.session === props.sessionID) { current.failed = true; currentState = 'unknown'; }
        for (const entry of entries.values()) if (entry.session === props.sessionID && !entry.reported) entry.failed = true;
      } else if (event.type === 'permission.asked' || event.type === 'question.asked') {
        const state = assistants.get(props.tool?.messageID);
        if (state && state.entry.session === props.sessionID) {
          if (state.entry === current && state.entry.latest === props.tool?.messageID) currentState = 'blocked';
          report(state.entry, 'needs-input');
        }
      } else if (['permission.replied', 'question.replied', 'question.rejected'].includes(event.type)) {
        if (current?.session === props.sessionID) currentState = 'unknown';
      } else if (event.type === 'session.idle') {
        for (const entry of entries.values()) {
          if (entry.session !== props.sessionID || entry.failed || (entry.reported && entry !== current) || !entry.bound) continue;
          const state = assistants.get(entry.latest);
          if (state?.info?.completed && state.info.finish === 'stop' && !state.info.error
              && !state.info.summary && !state.tool) considerResponse(entry);
        }
      }
    },
    dispose: async () => {
      stopped = true;
      clearInterval(heartbeatTimer);
      server.close();
      await heartbeatPending;
    },
  };
};
