
import { existsSync, unlinkSync, writeFileSync } from 'node:fs';
const { ZorOpenCode } = await import(process.env.ZOR_FIXTURE_PLUGIN);
export const ReloadFixture = async options => {
  let hooks = await ZorOpenCode(options), generation = 1, pending = false;
  const path = process.env.ZOR_FIXTURE_RELOAD;
  const record = () => writeFileSync(path + '.json', JSON.stringify({ generation, pid: process.pid }));
  record();
  const timer = setInterval(async () => {
    if (pending || !existsSync(path)) return;
    pending = true; unlinkSync(path);
    try { await hooks.dispose(); hooks = await ZorOpenCode(options); generation++; record(); }
    catch (error) { writeFileSync(path + '.error', String(error.stack)); }
    finally { pending = false; }
  }, 25);
  return {
    'chat.message': (...args) => hooks['chat.message'](...args),
    event: (...args) => hooks.event(...args),
    dispose: async () => { clearInterval(timer); await hooks.dispose(); },
  };
};
