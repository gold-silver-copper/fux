/**
 * Positive controls for the verifiers.
 *
 * These are harness self-tests, not agent runs. Each one drives the real
 * scenario through the real agent tool with a scripted sequence and asserts the
 * verifier reports `pass`. Without them, a verifier bug would look exactly like
 * an agent failure in every measured run.
 *
 * They also exercise the runner end to end — budgets, artifacts, cleanup — with
 * no model contacted, by injecting a scripted session.
 */
import assert from "node:assert/strict";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { loadDocumentation } from "../src/prompt.ts";
import { loadPiSdk } from "../src/pi-sdk.ts";
import { processAlive } from "../src/server.ts";
import { runOnce, type CampaignConfig, type CreateSessionFn } from "../src/runner.ts";
import { scenarioById, type Scenario } from "../src/scenarios/index.ts";

const FUX = join(import.meta.dirname, "..", "..", "target", "release", "fux");
const README = join(import.meta.dirname, "..", "..", "README.md");

/** Sends one request through the agent's tool and returns the raw response text. */
type Rpc = (method: string, params?: unknown) => Promise<string>;

/**
 * Builds a session that runs `solve` instead of contacting a model. `abort()`
 * interrupts the script, the way aborting a real session interrupts a turn.
 */
function scriptedSession(
  solve: (rpc: Rpc, task: string, signal: AbortSignal) => Promise<string>,
): CreateSessionFn {
  return async ({ toolDefinition }) => {
    const definition = toolDefinition as {
      execute: (id: string, params: unknown) => Promise<{ content: Array<{ text: string }> }>;
    };
    let sequence = 0;
    const rpc: Rpc = async (method, params) => {
      sequence += 1;
      const result = await definition.execute(`scripted-${sequence}`, {
        method,
        ...(params === undefined ? {} : { params_json: JSON.stringify(params) }),
      });
      return result.content[0]?.text ?? "";
    };
    const listeners: Array<(event: unknown) => void> = [];
    const controller = new AbortController();
    const session = {
      subscribe(listener: (event: unknown) => void) {
        listeners.push(listener);
        return () => undefined;
      },
      async prompt(task: string) {
        let answer: string;
        try {
          answer = await solve(rpc, task, controller.signal);
        } catch (error) {
          if (!controller.signal.aborted) throw error;
          answer = `(scripted session aborted: ${(error as Error).message})`;
        }
        for (const listener of listeners) {
          listener({ type: "message_end", message: { content: [{ type: "text", text: answer }] } });
        }
      },
      async abort() {
        controller.abort(new Error("aborted by the runner"));
      },
      getSessionStats() {
        return {
          tokens: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
          cost: 0,
          toolCalls: sequence,
          assistantMessages: 1,
        };
      },
      messages: [] as unknown[],
      dispose() {},
    };
    return { session, dispose: () => session.dispose() };
  };
}

async function driveScenario(
  scenario: Scenario,
  variant: string,
  solve: (rpc: Rpc, task: string, signal: AbortSignal) => Promise<string>,
  overrides: Partial<CampaignConfig> = {},
) {
  const sdk = await loadPiSdk();
  const artifactsRoot = mkdtempSync(join(tmpdir(), "fux-positive-"));
  const config: CampaignConfig = {
    fuxBinary: FUX,
    repoRoot: join(import.meta.dirname, "..", ".."),
    artifactsRoot,
    documentation: loadDocumentation(README),
    thinkingLevel: "off",
    maxToolCalls: 60,
    runDeadlineMs: 120_000,
    requestTimeoutMs: 15_000,
    repetitions: 1,
    keepArtifacts: "always",
    dryRun: false,
    environment: { piVersion: "test", fuxGitRevision: "test", node: process.version, scripted: true },
    ...overrides,
  };
  try {
    const result = await runOnce(
      sdk,
      null,
      { providerId: "none", modelId: "scripted", model: undefined, displayName: null, rejectedAliases: [], verification: {} } as never,
      config,
      scenario,
      {
        runId: `${scenario.id}-${variant}-positive`,
        scenarioId: scenario.id,
        scenarioTitle: scenario.title,
        variant,
        repetition: 1,
      },
      { createSession: scriptedSession(solve) },
    );
    return { result, artifactsRoot };
  } catch (error) {
    rmSync(artifactsRoot, { recursive: true, force: true });
    throw error;
  }
}

function jsonOf(text: string): any {
  return JSON.parse(text);
}

function requireScenario(id: string): Scenario {
  const scenario = scenarioById(id);
  assert.ok(scenario, `scenario ${id} must exist`);
  return scenario;
}

const viewerFrom = (task: string): number => {
  const match = /viewer entity id is (\d+)/.exec(task);
  assert.ok(match, `task prompt must state the viewer id: ${task.slice(0, 200)}`);
  return Number(match[1]);
};

const control = (viewer: number, command: unknown) => ({
  event: "fux::control::Control",
  value: { viewer, command },
});

const userInput = (viewer: number, input: unknown) => ({
  event: "fux::control::UserInput",
  value: { viewer, input },
});

async function processesByName(rpc: Rpc): Promise<Map<string, number>> {
  const rows = jsonOf(
    await rpc("world.query", {
      data: { components: ["fux::model::ProcessState"], option: ["bevy_ecs::name::Name"] },
    }),
  ).result as Array<{ entity: number; components: Record<string, unknown> }>;
  const found = new Map<string, number>();
  for (const row of rows) {
    const name = row.components["bevy_ecs::name::Name"];
    if (typeof name === "string") found.set(name, row.entity);
  }
  return found;
}

async function paneViewFor(rpc: Rpc, process: number): Promise<number | undefined> {
  const rows = jsonOf(await rpc("world.query", { data: { components: ["fux::model::PaneView"] } }))
    .result as Array<{ entity: number; components: Record<string, { pane: number }> }>;
  return rows.find((row) => row.components["fux::model::PaneView"].pane === process)?.entity;
}

test("positive control: discovery verifier passes when the pane is actually moved", async () => {
  const { result, artifactsRoot } = await driveScenario(requireScenario("discovery"), "a", async (rpc, task) => {
    const viewer = viewerFrom(task);
    const byName = await processesByName(rpc);
    const target = byName.get("audit-target");
    assert.ok(target, "fixture process audit-target must exist");
    const paneView = await paneViewFor(rpc, target);
    assert.ok(paneView, "audit-target must have a pane view");

    const workspaces = jsonOf(
      await rpc("world.query", {
        data: { components: ["fux::model::Workspace"], option: ["bevy_ecs::name::Name"] },
      }),
    ).result as Array<{ entity: number; components: Record<string, unknown> }>;
    const destination = workspaces.find(
      (row) => row.components["bevy_ecs::name::Name"] === "beta",
    )?.entity;
    assert.ok(destination, "workspace beta must exist");

    await rpc("world.trigger_event", control(viewer, { kind: "focus", pane: paneView }));
    await rpc(
      "world.trigger_event",
      control(viewer, { kind: "move", to: { kind: "workspace", workspace: destination } }),
    );
    await rpc(
      "world.trigger_event",
      control(viewer, { kind: "rename", subject: { pane: paneView }, name: "audited" }),
    );
    return "Moved the audit-target pane into workspace beta, renamed it audited and left it focused.";
  });

  try {
    assert.equal(result.outcome, "pass", JSON.stringify(result.checks.filter((c) => !c.ok), null, 2));
    assert.ok(existsSync(result.artifactPath));
  } finally {
    rmSync(artifactsRoot, { recursive: true, force: true });
  }
});

test("positive control: launch verifier passes on an exact Launch recipe", async () => {
  const { result, artifactsRoot } = await driveScenario(requireScenario("launch"), "fail", async (rpc, task) => {
    const viewer = viewerFrom(task);
    const program = /program: (\S+)/.exec(task)?.[1];
    const argument = /single argument: (\S+)/.exec(task)?.[1];
    const cwd = /working directory: (\S+)/.exec(task)?.[1];
    assert.ok(program && argument && cwd, "task prompt must state program, argument and cwd");

    const tab = jsonOf(
      await rpc("world.get_components", {
        entity: viewer,
        components: ["fux::model::OnTab"],
        strict: true,
      }),
    ).result["fux::model::OnTab"] as number;

    const process = jsonOf(
      await rpc("world.spawn_entity", {
        components: {
          "fux::model::Launch": { argv: [program, argument], cwd, history_lines: 500 },
          "bevy_ecs::name::Name": "checks",
        },
      }),
    ).result.entity as number;
    const paneView = jsonOf(
      await rpc("world.spawn_entity", {
        components: { "fux::model::PaneView": { pane: process } },
      }),
    ).result.entity as number;
    await rpc("world.reparent_entities", { entities: [paneView], parent: tab });

    let status: any = null;
    for (let attempt = 0; attempt < 60; attempt += 1) {
      status = jsonOf(
        await rpc("world.get_components", {
          entity: process,
          components: ["fux::model::ProcessState"],
          strict: true,
        }),
      ).result["fux::model::ProcessState"].status;
      if (status?.kind === "exited" || status?.kind === "failed") break;
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
    await rpc("world.trigger_event", control(viewer, { kind: "focus", pane: paneView }));
    return `The program exited with code ${String(status?.code)}.\nVERDICT: ${status?.code === 0 ? "SUCCESS" : "FAILURE"}`;
  });

  try {
    assert.equal(result.outcome, "pass", JSON.stringify(result.checks.filter((c) => !c.ok), null, 2));
  } finally {
    rmSync(artifactsRoot, { recursive: true, force: true });
  }
});

test("positive control: noisy verifier passes when history is actually searched", async () => {
  const { result, artifactsRoot } = await driveScenario(requireScenario("noisy"), "a", async (rpc, task) => {
    const viewer = viewerFrom(task);
    const base = jsonOf(
      await rpc("world.get_components", {
        entity: viewer,
        components: ["fux::model::Viewer"],
        strict: true,
      }),
    ).result["fux::model::Viewer"];

    // Walk back through history the way a client must: set an offset, repaint, read.
    let found: { offset: number; code: string } | null = null;
    for (let offset = 20; offset <= 200 && found === null; offset += 20) {
      await rpc("world.insert_components", {
        entity: viewer,
        components: { "fux::model::Viewer": { ...base, scrollback: offset } },
      });
      const paint = jsonOf(await rpc("fux.frame", { viewer })).result.paint as string;
      const plain = paint.replace(/\u001b\[[0-9;?]*[ -/]*[@-~]/g, "");
      const match = /FAILURE (E-\d+)/.exec(plain);
      if (match) found = { offset, code: match[1] };
    }
    assert.ok(found, "the diagnostic must be reachable through history");
    return `Found it while scrolling back.\nCODE: ${found.code}`;
  });

  try {
    assert.equal(result.outcome, "pass", JSON.stringify(result.checks.filter((c) => !c.ok), null, 2));
  } finally {
    rmSync(artifactsRoot, { recursive: true, force: true });
  }
});

test("positive control: modal rename verifier passes only via the open prompt", async () => {
  const { result, artifactsRoot } = await driveScenario(requireScenario("modal"), "rename", async (rpc, task) => {
    const viewer = viewerFrom(task);
    const overlay = jsonOf(
      await rpc("world.get_components", {
        entity: viewer,
        components: ["fux::interaction::Overlay"],
        strict: true,
      }),
    ).result["fux::interaction::Overlay"];
    assert.ok(overlay.mode.Text, "the pre-opened overlay should be a text prompt");

    for (const character of "renamed-by-agent") {
      await rpc(
        "world.trigger_event",
        userInput(viewer, { kind: "key", key: character, ctrl: false, alt: false, shift: false }),
      );
    }
    await rpc(
      "world.trigger_event",
      userInput(viewer, { kind: "key", key: "enter", ctrl: false, alt: false, shift: false }),
    );
    return "It was a rename prompt for the focused pane; I typed the new name and pressed Enter.";
  });

  try {
    assert.equal(result.outcome, "pass", JSON.stringify(result.checks.filter((c) => !c.ok), null, 2));
  } finally {
    rmSync(artifactsRoot, { recursive: true, force: true });
  }
});

test("modal rename verifier reports partial when the prompt is bypassed", async () => {
  const { result, artifactsRoot } = await driveScenario(requireScenario("modal"), "rename", async (rpc, task) => {
    const viewer = viewerFrom(task);
    // Cancel the prompt, then rename non-interactively: the end state is right
    // but the interaction under test was never exercised.
    await rpc(
      "world.trigger_event",
      userInput(viewer, { kind: "key", key: "escape", ctrl: false, alt: false, shift: false }),
    );
    const byName = await processesByName(rpc);
    const target = byName.get("prompt-target");
    assert.ok(target);
    const paneView = await paneViewFor(rpc, target);
    await rpc(
      "world.trigger_event",
      control(viewer, { kind: "rename", subject: { pane: paneView }, name: "renamed-by-agent" }),
    );
    return "Cancelled the prompt and renamed the pane with a command instead.";
  });

  try {
    assert.equal(result.outcome, "partial", JSON.stringify(result.checks, null, 2));
    const failed = result.checks.filter((check) => !check.ok).map((check) => check.name);
    assert.deepEqual(failed, ["the pre-opened prompt was driven"]);
  } finally {
    rmSync(artifactsRoot, { recursive: true, force: true });
  }
});

test("positive control: modal confirm verifier passes when the close is declined", async () => {
  const { result, artifactsRoot } = await driveScenario(requireScenario("modal"), "confirm", async (rpc, task) => {
    const viewer = viewerFrom(task);
    const overlay = jsonOf(
      await rpc("world.get_components", {
        entity: viewer,
        components: ["fux::interaction::Overlay"],
        strict: true,
      }),
    ).result["fux::interaction::Overlay"];
    assert.ok(overlay.mode.Confirm, "the pre-opened overlay should be a confirmation");

    await rpc(
      "world.trigger_event",
      userInput(viewer, { kind: "key", key: "n", ctrl: false, alt: false, shift: false }),
    );
    const byName = await processesByName(rpc);
    const target = byName.get("prompt-target");
    assert.ok(target);
    const paneView = await paneViewFor(rpc, target);
    await rpc(
      "world.trigger_event",
      control(viewer, { kind: "rename", subject: { pane: paneView }, name: "renamed-by-agent" }),
    );
    return "It proposed closing the focused pane; I declined with n and then renamed the pane.";
  });

  try {
    assert.equal(result.outcome, "pass", JSON.stringify(result.checks.filter((c) => !c.ok), null, 2));
  } finally {
    rmSync(artifactsRoot, { recursive: true, force: true });
  }
});

test("positive control: recovery verifier passes after the injected disruption", async () => {
  const { result, artifactsRoot } = await driveScenario(requireScenario("recovery"), "a", async (rpc, task) => {
    const viewer = viewerFrom(task);
    // This query surfaces target-a's entity id, which fires the disruption.
    const byName = await processesByName(rpc);
    const target = byName.get("target-a");
    const keep = byName.get("target-b");
    assert.ok(target && keep);

    const view = await paneViewFor(rpc, target);
    let closeResponse = "(target already gone)";
    if (view !== undefined) {
      closeResponse = await rpc(
        "world.trigger_event",
        control(viewer, { kind: "close", subject: { pane: view } }),
      );
    }
    // Confirm the real state rather than assuming the close did it.
    for (let attempt = 0; attempt < 40; attempt += 1) {
      const status = jsonOf(
        await rpc("world.get_components", {
          entity: target,
          components: ["fux::model::ProcessState"],
        }),
      ).result?.components?.["fux::model::ProcessState"]?.status;
      if (status === undefined || status?.kind === "exited" || status?.kind === "failed") break;
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
    const keepView = await paneViewFor(rpc, keep);
    assert.ok(keepView);
    await rpc("world.trigger_event", control(viewer, { kind: "focus", pane: keepView }));
    return `target-a is gone (close response: ${closeResponse.slice(0, 60)}), target-b is focused.`;
  });

  try {
    assert.equal(result.outcome, "pass", JSON.stringify(result.checks.filter((c) => !c.ok), null, 2));
    const artifact = JSON.parse(readFileSync(result.artifactPath, "utf8"));
    assert.equal(
      artifact.verification.evidence.disruption.applied,
      true,
      "the milestone disruption must have fired",
    );
    assert.equal(typeof artifact.verification.evidence.disruption.triggeredByCallIndex, "number");
  } finally {
    rmSync(artifactsRoot, { recursive: true, force: true });
  }
});

test("runner records the wall-clock abort, writes the artifact and stops the server", async () => {
  let observedPid = 0;
  const { result, artifactsRoot } = await driveScenario(
    requireScenario("discovery"),
    "a",
    async (rpc, _task, signal) => {
      // Never finishes on its own: the deadline must stop this run.
      const discover = jsonOf(await rpc("rpc.discover"));
      observedPid = discover.result ? 1 : 0;
      await new Promise((_resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("scripted work was never interrupted")), 30_000);
        signal.addEventListener("abort", () => {
          clearTimeout(timer);
          reject(new Error("interrupted"));
        });
      });
      return "should not be reached";
    },
    { runDeadlineMs: 1_500 },
  );

  try {
    assert.equal(observedPid, 1, "the scripted session did reach the server");
    assert.match(String(result.abortReason), /wall-clock budget of 1500ms exhausted/);
    assert.ok(existsSync(result.artifactPath));
    const artifact = JSON.parse(readFileSync(result.artifactPath, "utf8"));
    assert.match(String(artifact.journal.abortReason), /wall-clock budget/);
    assert.equal(artifact.identity.runId, "discovery-a-positive");
    assert.ok(artifact.server.pid > 0);
    assert.equal(processAlive(artifact.server.pid), false, "the run's fux server must be stopped");
    assert.ok(artifact.prompts.task.includes("audit-target"));
    assert.ok(artifact.prompts.system.includes("fux_rpc"));
    assert.equal(artifact.documentation.sha256.length, 64);
    // Every run must stand alone as evidence, without the campaign summary.
    assert.equal(artifact.environment.scripted, true);
    assert.equal(artifact.environment.fuxGitRevision, "test");
    assert.equal(artifact.model.requestedModelId, "scripted");
    assert.equal(artifact.model.requestedThinkingLevel, "off");
  } finally {
    rmSync(artifactsRoot, { recursive: true, force: true });
  }
});

test("a scenario setup failure is reported as a harness error, not an agent verdict", async () => {
  const broken: Scenario = {
    id: "broken",
    title: "Deliberately broken setup",
    variants: ["only"],
    create: () => ({
      async setup() {
        throw new Error("setup exploded on purpose");
      },
      async verify() {
        throw new Error("verify must not run");
      },
    }),
  };
  const { result, artifactsRoot } = await driveScenario(broken, "only", async () => "unused");
  try {
    assert.equal(result.outcome, "error");
    assert.match(String(result.errorMessage), /setup exploded on purpose/);
    const artifact = JSON.parse(readFileSync(result.artifactPath, "utf8"));
    assert.equal(artifact.verification, null);
    assert.equal(processAlive(artifact.server.pid), false, "the server must still be cleaned up");
  } finally {
    rmSync(artifactsRoot, { recursive: true, force: true });
  }
});

test("a fux server that dies mid-run is reported as a crash, not a harness defect", async () => {
  const { result, artifactsRoot } = await driveScenario(requireScenario("discovery"), "a", async (rpc) => {
    // A partial Viewer payload: every field except `notice`.
    const viewers = jsonOf(await rpc("world.query", { data: { components: ["fux::model::Viewer"] } }))
      .result as Array<{ entity: number; components: Record<string, any> }>;
    const viewer = viewers[0];
    const { notice, ...partial } = viewer.components["fux::model::Viewer"];
    await rpc("world.insert_components", {
      entity: viewer.entity,
      components: { "fux::model::Viewer": partial },
    });
    await rpc("rpc.discover");
    return "sent a partial component payload";
  });

  try {
    assert.equal(result.serverCrashed, true, "the runner must notice the server is gone");
    const artifact = JSON.parse(readFileSync(result.artifactPath, "utf8"));
    assert.equal(artifact.serverCrashed, true);
    assert.match(String(artifact.serverLogTail), /panicked at/);
    assert.ok(
      artifact.journal.notes.some((note: string) => note.includes("panic")),
      "the crash must be noted in the journal",
    );
  } finally {
    rmSync(artifactsRoot, { recursive: true, force: true });
  }
});
