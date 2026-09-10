
import { appendFileSync } from "node:fs";
export const ZorEventProbe = async () => {
  let bytes = 0;
  const log = (value) => {
    const text = JSON.stringify(value) + "\n";
    bytes += Buffer.byteLength(text);
    if (bytes > 1048576) throw new Error("fixture event limit");
    appendFileSync(process.env.ZOR_PROBE_LOG, text, { mode: 0o600 });
  };
  log({ hook: "loaded" });
  return {
    "chat.message": async (input, output) => log({ hook: "chat.message", input, output }),
    event: async ({ event }) => {
      if (["message.updated", "message.part.updated", "session.status", "session.idle",
           "session.error", "permission.asked", "permission.replied"].includes(event.type))
        log({ hook: "event", event });
    },
  };
};
