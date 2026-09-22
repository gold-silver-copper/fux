import assert from "node:assert/strict";
import test from "node:test";
import { createServer } from "node:http";
import { decodeScreen, stripAnsi } from "../src/ansi.ts";
import { BrpClient } from "../src/brp.ts";
import { EVENT_LIMIT, MODEL_VISIBLE_LIMIT, RunJournal, truncate } from "../src/journal.ts";
import { fixtureEnvironment } from "../src/server.ts";
import { loadPiSdk } from "../src/pi-sdk.ts";
import { createFuxRpcTool } from "../src/tool.ts";

test("truncation reports both what was kept and the original size", () => {
  const short = truncate("abc", 10);
  assert.equal(short.text, "abc");
  assert.equal(short.truncation.applied, false);

  const long = truncate("x".repeat(50), 10);
  assert.equal(long.truncation.applied, true);
  assert.equal(long.truncation.originalBytes, 50);
  assert.equal(long.truncation.keptBytes, 10);
  assert.ok(long.text.startsWith("x".repeat(10)));
  assert.match(long.text, /TRUNCATED BY HARNESS: kept 10 of 50 characters/);
});

test("the journal bounds retained events and counts the overflow", () => {
  const journal = new RunJournal();
  for (let index = 0; index < EVENT_LIMIT + 25; index += 1) journal.recordEvent("tick");
  assert.equal(journal.events.length, EVENT_LIMIT);
  assert.equal(journal.eventsDropped, 25);
});

test("fixture environments carry no credential-shaped variables", () => {
  const env = fixtureEnvironment("/tmp/home-x", {
    PATH: "/usr/bin",
    GEMINI_API_KEY: "secret-value",
    ANTHROPIC_API_KEY: "secret-value",
    AWS_SESSION_TOKEN: "secret-value",
    MY_PASSWORD: "secret-value",
    GITHUB_AUTH: "secret-value",
    TERM: "xterm",
  });
  const serialized = JSON.stringify(env);
  assert.ok(!serialized.includes("secret-value"), `leaked a credential: ${serialized}`);
  assert.equal(env.HOME, "/tmp/home-x");
  assert.equal(env.PS1, "$ ");
  assert.equal(env.HISTFILE, "/dev/null");
  assert.equal(env.PATH, "/usr/bin");
});

test("the ANSI decoder reconstructs positioned rows", () => {
  const paint =
    "\u001b[?25l\u001b[H\u001b[2J" +
    "\u001b[1;1H\u001b[0mfirst row" +
    "\u001b[3;5H\u001b[1mindented" +
    "\u001b[4;1Hkeep\u001b[K" +
    "\u001b]52;c;AAA\u0007";
  const screen = decodeScreen(paint, 5, 20);
  assert.deepEqual(screen.rows, ["first row", "", "    indented", "keep", ""]);
  assert.equal(stripAnsi("\u001b[31mred\u001b[0m"), "red");
});

test("the decoder honours erase-to-end so stale text cannot be reported as visible", () => {
  const paint = "\u001b[1;1Hstale text here\u001b[1;1H\u001b[0Knew";
  const screen = decodeScreen(paint, 2, 20);
  assert.equal(screen.rows[0], "new");
});

/** A JSON-RPC stand-in so tool behaviour is tested without a fux server. */
async function withStubServer(
  handler: (body: unknown) => { status?: number; payload: unknown },
  run: (client: BrpClient, seen: unknown[]) => Promise<void>,
): Promise<void> {
  const seen: unknown[] = [];
  const server = createServer((request, response) => {
    const chunks: Buffer[] = [];
    request.on("data", (chunk) => chunks.push(chunk));
    request.on("end", () => {
      const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
      seen.push(body);
      const { status = 200, payload } = handler(body);
      response.writeHead(status, { "content-type": "application/json" });
      response.end(typeof payload === "string" ? payload : JSON.stringify(payload));
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  const port = typeof address === "object" && address ? address.port : 0;
  try {
    await run(new BrpClient(`http://127.0.0.1:${port}`), seen);
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}

test("the agent tool forwards requests verbatim and records them", async () => {
  const sdk = await loadPiSdk();
  await withStubServer(
    (body) => ({ payload: { jsonrpc: "2.0", id: (body as { id: number }).id, result: { ok: true } } }),
    async (client, seen) => {
      const journal = new RunJournal();
      const tool = createFuxRpcTool(sdk, client, journal, { maxToolCalls: 3, requestTimeoutMs: 2_000 });
      const definition = tool.definition as {
        execute: (id: string, params: unknown) => Promise<{ content: Array<{ text: string }>; isError?: boolean }>;
      };

      const result = await definition.execute("call-1", {
        method: "world.query",
        params_json: '{"data":{"components":["fux::model::Workspace"]}}',
      });
      assert.match(result.content[0].text, /"result":\{"ok":true\}/);
      assert.notEqual(result.isError, true);
      assert.deepEqual((seen[0] as { params: unknown }).params, {
        data: { components: ["fux::model::Workspace"] },
      });
      assert.equal((seen[0] as { method: string }).method, "world.query");
      assert.equal(journal.toolCalls.length, 1);
      assert.equal(journal.toolCalls[0].rejected, null);
      assert.equal(journal.sentCount, 1);
    },
  );
});

test("invalid params JSON is refused without being repaired or sent", async () => {
  const sdk = await loadPiSdk();
  await withStubServer(
    () => ({ payload: { jsonrpc: "2.0", id: 1, result: null } }),
    async (client, seen) => {
      const journal = new RunJournal();
      const tool = createFuxRpcTool(sdk, client, journal, { maxToolCalls: 3, requestTimeoutMs: 2_000 });
      const definition = tool.definition as {
        execute: (id: string, params: unknown) => Promise<{ content: Array<{ text: string }>; isError?: boolean }>;
      };
      const result = await definition.execute("call-1", { method: "world.query", params_json: "{not json" });
      assert.equal(result.isError, true);
      assert.match(result.content[0].text, /not valid JSON/);
      assert.equal(seen.length, 0, "nothing may reach the server");
      assert.equal(journal.toolCalls[0].rejected, "params_json was not valid JSON");
      assert.equal(journal.sentCount, 0);
    },
  );
});

test("the tool-call budget stops further requests and reports the abort", async () => {
  const sdk = await loadPiSdk();
  await withStubServer(
    (body) => ({ payload: { jsonrpc: "2.0", id: (body as { id: number }).id, result: 1 } }),
    async (client, seen) => {
      const journal = new RunJournal();
      const reasons: string[] = [];
      const tool = createFuxRpcTool(
        sdk,
        client,
        journal,
        { maxToolCalls: 2, requestTimeoutMs: 2_000 },
        { onBudgetExhausted: (reason) => reasons.push(reason) },
      );
      const definition = tool.definition as {
        execute: (id: string, params: unknown) => Promise<{ content: Array<{ text: string }>; isError?: boolean }>;
      };
      await definition.execute("a", { method: "rpc.discover" });
      await definition.execute("b", { method: "rpc.discover" });
      const third = await definition.execute("c", { method: "rpc.discover" });

      assert.equal(seen.length, 2, "only the budgeted requests may be sent");
      assert.equal(third.isError, true);
      assert.match(third.content[0].text, /budget of 2 exhausted/);
      assert.deepEqual(reasons, ["tool-call budget of 2 exhausted"]);
      assert.equal(journal.toolCalls.length, 3);
      assert.equal(journal.sentCount, 2);
    },
  );
});

test("oversized responses are truncated for the model and marked in both places", async () => {
  const sdk = await loadPiSdk();
  const huge = "y".repeat(MODEL_VISIBLE_LIMIT + 5_000);
  await withStubServer(
    (body) => ({ payload: { jsonrpc: "2.0", id: (body as { id: number }).id, result: huge } }),
    async (client) => {
      const journal = new RunJournal();
      const tool = createFuxRpcTool(sdk, client, journal, { maxToolCalls: 2, requestTimeoutMs: 5_000 });
      const definition = tool.definition as {
        execute: (id: string, params: unknown) => Promise<{ content: Array<{ text: string }> }>;
      };
      const result = await definition.execute("a", { method: "fux.frame" });
      assert.ok(result.content[0].text.length <= MODEL_VISIBLE_LIMIT + 200);
      assert.match(result.content[0].text, /TRUNCATED BY HARNESS/);
      assert.equal(journal.toolCalls[0].modelVisible.applied, true);
      assert.equal(journal.toolCalls[0].artifact.applied, false, "the artifact limit is far larger");
      assert.ok((journal.toolCalls[0].response ?? "").includes(huge.slice(0, 100)));
    },
  );
});

test("a transport failure is surfaced as a tool error, not a silent retry", async () => {
  const sdk = await loadPiSdk();
  const journal = new RunJournal();
  // Port 1 is not listening; the request cannot be completed.
  const tool = createFuxRpcTool(sdk, new BrpClient("http://127.0.0.1:1"), journal, {
    maxToolCalls: 2,
    requestTimeoutMs: 1_000,
  });
  const definition = tool.definition as {
    execute: (id: string, params: unknown) => Promise<{ content: Array<{ text: string }>; isError?: boolean }>;
  };
  const result = await definition.execute("a", { method: "rpc.discover" });
  assert.equal(result.isError, true);
  assert.match(result.content[0].text, /transport error/);
  assert.equal(journal.toolCalls.length, 1);
  assert.ok(journal.toolCalls[0].transportError);
});
