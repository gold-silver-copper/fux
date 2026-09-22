/**
 * Minimal JSON-RPC client for a fux server's Bevy Remote Protocol socket.
 *
 * This is the harness's own transport. It performs no fux-specific translation:
 * the agent-facing tool and the verifiers both send raw method names and params.
 * fux serves HTTP only on a Unix domain socket; `fetch` cannot dial one, so this
 * uses `node:http` with `socketPath` and a keep-alive agent, which pools
 * connections as `fetch` did.
 */
import { Agent, request as httpRequest } from "node:http";

export interface BrpOutcome {
  method: string;
  params: unknown;
  /** Raw JSON-RPC envelope as returned by the server, when one was parsed. */
  envelope: unknown;
  result: unknown;
  error: unknown;
  httpStatus: number | null;
  /** Transport-level failure (timeout, refused connection, invalid JSON). */
  transportError: string | null;
  durationMs: number;
  /** Raw response body, retained for artifacts. */
  body: string | null;
}

export class BrpClient {
  #socket: string;
  #agent = new Agent({ keepAlive: true });
  #nextId = 1;

  constructor(socket: string) {
    this.#socket = socket;
  }

  /** The Unix domain socket this client talks to. */
  get socket(): string {
    return this.#socket;
  }

  /** Releases pooled connections. */
  close(): void {
    this.#agent.destroy();
  }

  /** One HTTP exchange over the socket: status and the whole body as text. */
  #post(payload: string, timeoutMs: number): Promise<{ status: number | null; text: string }> {
    return new Promise((resolve, reject) => {
      const request = httpRequest(
        {
          socketPath: this.#socket,
          agent: this.#agent,
          path: "/",
          method: "POST",
          headers: {
            host: "fux",
            "content-type": "application/json",
            "content-length": Buffer.byteLength(payload),
          },
          signal: AbortSignal.timeout(timeoutMs),
        },
        (response) => {
          const chunks: Buffer[] = [];
          response.on("data", (chunk: Buffer) => chunks.push(chunk));
          response.on("error", reject);
          response.on("end", () =>
            resolve({ status: response.statusCode ?? null, text: Buffer.concat(chunks).toString("utf8") }),
          );
        },
      );
      request.on("error", reject);
      request.end(payload);
    });
  }

  async call(method: string, params: unknown, timeoutMs = 10_000): Promise<BrpOutcome> {
    const started = performance.now();
    const request = {
      jsonrpc: "2.0",
      id: this.#nextId++,
      method,
      ...(params === undefined ? {} : { params }),
    };
    let httpStatus: number | null = null;
    let body: string | null = null;
    try {
      const response = await this.#post(JSON.stringify(request), timeoutMs);
      httpStatus = response.status;
      body = response.text;
      let envelope: unknown;
      try {
        envelope = JSON.parse(body);
      } catch (error) {
        return {
          method,
          params,
          envelope: null,
          result: null,
          error: null,
          httpStatus,
          transportError: `response was not JSON: ${(error as Error).message}`,
          durationMs: performance.now() - started,
          body,
        };
      }
      const record = envelope as { result?: unknown; error?: unknown };
      return {
        method,
        params,
        envelope,
        result: record.result ?? null,
        error: record.error ?? null,
        httpStatus,
        transportError: null,
        durationMs: performance.now() - started,
        body,
      };
    } catch (error) {
      return {
        method,
        params,
        envelope: null,
        result: null,
        error: null,
        httpStatus,
        transportError: (error as Error).message,
        durationMs: performance.now() - started,
        body,
      };
    }
  }

  /** Calls and throws unless the server returned a result. For harness setup/verification only. */
  async expect(method: string, params?: unknown, timeoutMs = 10_000): Promise<unknown> {
    const outcome = await this.call(method, params, timeoutMs);
    if (outcome.transportError) {
      throw new Error(`${method}: ${outcome.transportError}`);
    }
    if (outcome.error) {
      throw new Error(`${method}: ${JSON.stringify(outcome.error)}`);
    }
    return outcome.result;
  }
}

/** One `world.query` row. */
export interface QueryRow {
  entity: number;
  components: Record<string, unknown>;
  has?: Record<string, boolean>;
}

export async function query(
  client: BrpClient,
  components: string[],
  options: { option?: string[]; has?: string[] } = {},
): Promise<QueryRow[]> {
  const result = await client.expect("world.query", {
    data: {
      components,
      ...(options.option ? { option: options.option } : {}),
      ...(options.has ? { has: options.has } : {}),
    },
  });
  return (result as QueryRow[]) ?? [];
}

/** One component value, or undefined when the entity does not have it. */
export async function getComponent(
  client: BrpClient,
  entity: number,
  component: string,
): Promise<unknown> {
  const outcome = await client.call("world.get_components", {
    entity,
    components: [component],
  });
  if (outcome.transportError) throw new Error(outcome.transportError);
  const result = outcome.result as { components?: Record<string, unknown> } | null;
  return result?.components?.[component];
}

export async function listComponents(client: BrpClient, entity: number): Promise<string[]> {
  const result = await client.expect("world.list_components", { entity });
  return (result as string[]) ?? [];
}

export async function triggerControl(
  client: BrpClient,
  viewer: number,
  command: unknown,
): Promise<void> {
  await client.expect("world.trigger_event", {
    event: "fux::control::Control",
    value: { viewer, command },
  });
}

export async function triggerInput(
  client: BrpClient,
  viewer: number,
  input: unknown,
): Promise<void> {
  await client.expect("world.trigger_event", {
    event: "fux::control::UserInput",
    value: { viewer, input },
  });
}

export async function sendKey(
  client: BrpClient,
  viewer: number,
  key: string,
  modifiers: { ctrl?: boolean; alt?: boolean; shift?: boolean } = {},
): Promise<void> {
  await triggerInput(client, viewer, {
    kind: "key",
    key,
    ctrl: modifiers.ctrl ?? false,
    alt: modifiers.alt ?? false,
    shift: modifiers.shift ?? false,
  });
}

export async function attach(
  client: BrpClient,
  rows = 24,
  cols = 80,
  workspace?: string,
): Promise<number> {
  const result = (await client.expect("fux.attach", {
    rows,
    cols,
    ...(workspace ? { workspace } : {}),
  })) as { viewer?: number };
  if (typeof result?.viewer !== "number") {
    throw new Error("fux.attach did not return a viewer");
  }
  return result.viewer;
}

export async function frame(client: BrpClient, viewer: number): Promise<string> {
  const result = (await client.expect("fux.frame", { viewer })) as { paint?: string };
  return result?.paint ?? "";
}

/** Polls `observe` until it returns true, or throws after `timeoutMs`. */
export async function eventually(
  observe: () => Promise<boolean>,
  timeoutMs = 5_000,
  label = "condition",
  intervalMs = 50,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (await observe()) return;
    if (Date.now() >= deadline) {
      throw new Error(`${label} did not settle within ${timeoutMs}ms`);
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
}
