/**
 * The campaign runner: one fresh fux server and one fresh pi session per run.
 *
 * Budgets are finite and enforced here, not left to the agent. Every run writes
 * its own artifact file with the exact prompt, the raw BRP traffic, the verifier's
 * findings and the usage pi reported, then cleans up the server it started.
 */
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { ResolvedModel } from "./model.ts";
import type { PiSdk } from "./pi-sdk.ts";
import { ARTIFACT_LIMIT, MODEL_VISIBLE_LIMIT, RunJournal, truncate } from "./journal.ts";
import { buildSystemPrompt, type Documentation } from "./prompt.ts";
import { discardDirectory, processAlive, startServer, type ServerHandle } from "./server.ts";
import { createFuxRpcTool, TOOL_NAME } from "./tool.ts";
import type { Scenario, ScenarioContext, ScenarioSetup, Verification } from "./scenarios/index.ts";

export interface CampaignConfig {
  fuxBinary: string;
  repoRoot: string;
  artifactsRoot: string;
  documentation: Documentation;
  thinkingLevel: string;
  maxToolCalls: number;
  runDeadlineMs: number;
  requestTimeoutMs: number;
  repetitions: number;
  keepArtifacts: "always" | "on-failure";
  /** When set, no model is contacted: setup and verification run, the agent does not. */
  dryRun: boolean;
  /**
   * Build and toolchain identity, copied into every run artifact.
   *
   * Each run has to stand on its own as evidence: reading one `run.json` must
   * answer which fux build, which pi, and which verified model produced it,
   * without needing the campaign summary beside it.
   */
  environment: Record<string, unknown>;
}

export interface RunIdentity {
  runId: string;
  scenarioId: string;
  scenarioTitle: string;
  variant: string;
  repetition: number;
}

export interface RunResult extends RunIdentity {
  startedAt: string;
  finishedAt: string;
  wallMs: number;
  outcome: Verification["outcome"] | "error";
  checks: Verification["checks"];
  notes: string[];
  toolCalls: number;
  acceptedToolCalls: number;
  abortReason: string | null;
  errorMessage: string | null;
  /** True when this run's fux server process died before cleanup. */
  serverCrashed: boolean;
  usage: RunJournal["usage"];
  artifactPath: string;
  serverLogPath: string;
  directoryKept: boolean;
}

export interface SessionBundle {
  session: any;
  dispose: () => void;
}

export interface CreateSessionArgs {
  sdk: PiSdk;
  modelRuntime: unknown;
  model: ResolvedModel;
  config: CampaignConfig;
  runDir: string;
  toolDefinition: unknown;
  systemPrompt: string;
}

/** Seam so tests can exercise budgets and cleanup without contacting a model. */
export type CreateSessionFn = (args: CreateSessionArgs) => Promise<SessionBundle>;

/** Everything ambient is switched off so the experiment is the experiment. */
export const createSession: CreateSessionFn = async ({
  sdk,
  modelRuntime,
  model,
  config,
  runDir,
  toolDefinition,
  systemPrompt,
}) => {
  const settingsManager = sdk.SettingsManager.inMemory({
    // Comparable runs: no auto-compaction, no long retry ladders, no packages.
    compaction: { enabled: false },
    retry: { enabled: true, maxRetries: 2 },
    quietStartup: true,
    enableSkillCommands: false,
  });
  const loader = new sdk.DefaultResourceLoader({
    cwd: runDir,
    agentDir: runDir,
    settingsManager,
    noExtensions: true,
    noSkills: true,
    noPromptTemplates: true,
    noThemes: true,
    noContextFiles: true,
    systemPrompt,
  });
  await loader.reload();

  // Assert the isolation actually held, rather than trusting the flags.
  const leaks: string[] = [];
  if ((loader.getExtensions().extensions ?? []).length > 0) leaks.push("extensions");
  if ((loader.getSkills().skills ?? []).length > 0) leaks.push("skills");
  if ((loader.getPrompts().prompts ?? []).length > 0) leaks.push("prompt templates");
  if ((loader.getAgentsFiles().agentsFiles ?? []).length > 0) leaks.push("context files");
  if (loader.getSystemPrompt() !== systemPrompt) leaks.push("system prompt override");
  if ((loader.getAppendSystemPrompt() ?? []).length > 0) leaks.push("appended system prompt");
  if (leaks.length > 0) {
    throw new Error(`resource isolation failed; ambient ${leaks.join(", ")} would have been loaded`);
  }

  const { session } = await sdk.createAgentSession({
    cwd: runDir,
    agentDir: runDir,
    modelRuntime,
    model: model.model,
    thinkingLevel: config.thinkingLevel,
    noTools: "all",
    tools: [TOOL_NAME],
    customTools: [toolDefinition],
    resourceLoader: loader,
    sessionManager: sdk.SessionManager.inMemory(runDir),
    settingsManager,
  });

  return { session, dispose: () => session.dispose() };
};

function collectText(message: unknown): string {
  const content = (message as { content?: unknown })?.content;
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .filter((part) => (part as { type?: string })?.type === "text")
    .map((part) => String((part as { text?: string }).text ?? ""))
    .join("");
}

export async function runOnce(
  sdk: PiSdk,
  modelRuntime: unknown,
  model: ResolvedModel,
  config: CampaignConfig,
  scenario: Scenario,
  identity: RunIdentity,
  overrides: { createSession?: CreateSessionFn } = {},
): Promise<RunResult> {
  const startedAt = new Date().toISOString();
  const started = performance.now();
  const runDir = join(config.artifactsRoot, identity.runId);
  const fixtureDir = join(runDir, "fixture-truth");
  mkdirSync(fixtureDir, { recursive: true });
  const artifactPath = join(runDir, "run.json");

  const journal = new RunJournal();
  const run = scenario.create(identity.variant);
  let server: ServerHandle | undefined;
  let bundle: SessionBundle | undefined;
  let setup: ScenarioSetup | undefined;
  let verification: Verification | undefined;
  let errorMessage: string | null = null;
  let deadlineTimer: NodeJS.Timeout | undefined;
  let serverStop: Awaited<ReturnType<ServerHandle["stop"]>> | undefined;
  let effectiveThinkingLevel: string | null = null;
  let effectiveModelId: string | null = null;
  let serverCrashed = false;
  let serverLogTail: string | null = null;

  try {
    server = await startServer({
      binary: config.fuxBinary,
      directory: join(runDir, "server"),
      historyLines: scenario.historyLines ?? 500,
    });
    const ctx: ScenarioContext = {
      server,
      client: server.client,
      journal,
      variant: identity.variant,
      fixtureDir,
    };

    setup = await run.setup(ctx);
    run.startMonitor?.(ctx);

    const systemPrompt = buildSystemPrompt(config.documentation, config.maxToolCalls);

    if (!config.dryRun) {
      const tool = createFuxRpcTool(
        sdk,
        server.client,
        journal,
        { maxToolCalls: config.maxToolCalls, requestTimeoutMs: config.requestTimeoutMs },
        {
          beforeCall: (method, params, index) => run.beforeToolCall?.(ctx, method, params, index),
          onCall: (record, outcome) => run.onToolCall?.(ctx, record, outcome),
          onBudgetExhausted: (reason) => {
            if (journal.abortReason === null) journal.abortReason = reason;
          },
        },
      );
      bundle = await (overrides.createSession ?? createSession)({
        sdk,
        modelRuntime,
        model,
        config,
        runDir,
        toolDefinition: tool.definition,
        systemPrompt,
      });
      const { session } = bundle;
      // The requested level may be clamped to what the model supports.
      effectiveThinkingLevel = session.thinkingLevel ?? null;
      effectiveModelId = session.model?.id ?? null;

      session.subscribe((event: { type: string } & Record<string, unknown>) => {
        switch (event.type) {
          case "tool_execution_start":
            journal.recordEvent(event.type, { toolName: event.toolName, args: event.args });
            break;
          case "tool_execution_end":
            journal.recordEvent(event.type, { toolName: event.toolName, isError: event.isError });
            break;
          case "message_end": {
            const text = collectText(event.message);
            if (text.trim().length > 0) journal.finalAnswer = text;
            journal.recordEvent(event.type, { chars: text.length });
            break;
          }
          case "auto_retry_start":
            journal.recordEvent(event.type, {
              attempt: event.attempt,
              maxAttempts: event.maxAttempts,
              errorMessage: event.errorMessage,
            });
            break;
          case "compaction_start":
          case "compaction_end":
            journal.recordEvent(event.type, { reason: event.reason });
            journal.note(`context compaction occurred (${event.type})`);
            break;
          case "agent_end":
            journal.recordEvent(event.type, { willRetry: event.willRetry });
            break;
          default:
            journal.recordEvent(event.type);
        }
      });

      deadlineTimer = setTimeout(() => {
        if (journal.abortReason === null) {
          journal.abortReason = `wall-clock budget of ${config.runDeadlineMs}ms exhausted`;
        }
        void session.abort();
      }, config.runDeadlineMs);

      await session.prompt(setup.prompt);
      if (journal.abortReason !== null) {
        // A budget stop may have landed mid-turn; let the session settle.
        await session.abort().catch(() => undefined);
      }

      const stats = session.getSessionStats();
      journal.usage = {
        tokens: stats.tokens,
        cost: stats.cost,
        toolCalls: stats.toolCalls,
        assistantMessages: stats.assistantMessages,
      };
      if (journal.finalAnswer === null) {
        const messages = session.messages as unknown[];
        for (let index = messages.length - 1; index >= 0; index -= 1) {
          const text = collectText(messages[index]);
          if (text.trim().length > 0) {
            journal.finalAnswer = text;
            break;
          }
        }
      }
    } else {
      journal.note("dry run: no model was contacted and the agent did not act");
    }

    run.stopMonitor?.();
    verification = await run.verify(ctx, setup);
  } catch (error) {
    errorMessage = (error as Error).stack ?? (error as Error).message;
    journal.errorMessage = errorMessage;
    try {
      run.stopMonitor?.();
    } catch {
      // Already failing; monitor teardown must not mask the cause.
    }
  } finally {
    if (deadlineTimer) clearTimeout(deadlineTimer);
    try {
      bundle?.dispose();
    } catch (error) {
      journal.note(`session dispose failed: ${(error as Error).message}`);
    }
    if (server) {
      // Distinguish "fux died" from "the harness broke": both surface as a
      // failed verification otherwise, and they mean very different things.
      serverCrashed = await server.settleExit();
      if (serverCrashed) {
        journal.note(`fux server pid ${server.pid} was no longer running when the run ended`);
        try {
          serverLogTail = readFileSync(server.logPath, "utf8").slice(-4_000);
        } catch {
          serverLogTail = null;
        }
        if (serverLogTail && /panicked at/.test(serverLogTail)) {
          journal.note("the fux server log contains a panic");
        }
      }
      try {
        serverStop = await server.stop();
        if (processAlive(server.pid)) {
          journal.note(`fux server pid ${server.pid} still alive after stop`);
        }
      } catch (error) {
        journal.note(`server stop failed: ${(error as Error).message}`);
      }
    }
  }

  const finishedAt = new Date().toISOString();
  const wallMs = performance.now() - started;
  const outcome: RunResult["outcome"] = errorMessage !== null ? "error" : (verification?.outcome ?? "error");

  const artifact = {
    schema: "fux-agent-exercise-run/1",
    identity,
    startedAt,
    finishedAt,
    wallMs,
    outcome,
    dryRun: config.dryRun,
    environment: config.environment,
    model: {
      requestedProviderId: model.providerId,
      requestedModelId: model.modelId,
      effectiveModelId,
      displayName: model.displayName ?? null,
      rejectedAliases: model.rejectedAliases ?? [],
      verification: model.verification ?? null,
      requestedThinkingLevel: config.thinkingLevel,
      effectiveThinkingLevel,
    },
    limits: {
      maxToolCalls: config.maxToolCalls,
      runDeadlineMs: config.runDeadlineMs,
      requestTimeoutMs: config.requestTimeoutMs,
      modelVisibleResponseChars: MODEL_VISIBLE_LIMIT,
      artifactResponseChars: ARTIFACT_LIMIT,
    },
    documentation: {
      path: config.documentation.path,
      sha256: config.documentation.sha256,
      lines: config.documentation.lines,
    },
    prompts: {
      system: truncate(buildSystemPrompt(config.documentation, config.maxToolCalls), ARTIFACT_LIMIT).text,
      task: setup?.prompt ?? null,
    },
    baseline: setup?.baseline ?? null,
    server: server
      ? { endpoint: server.endpoint, port: server.port, pid: server.pid, directory: server.directory }
      : null,
    serverStop: serverStop ?? null,
    serverCrashed,
    serverLogTail,
    journal: {
      toolCalls: journal.toolCalls,
      events: journal.events,
      eventsDropped: journal.eventsDropped,
      notes: journal.notes,
      finalAnswer: journal.finalAnswer,
      usage: journal.usage,
      abortReason: journal.abortReason,
      errorMessage: journal.errorMessage,
    },
    verification: verification ?? null,
  };
  writeFileSync(artifactPath, `${JSON.stringify(artifact, null, 2)}\n`);

  const keep = config.keepArtifacts === "always" || outcome !== "pass";
  if (!keep && server) {
    discardDirectory(join(runDir, "server"));
  }

  return {
    ...identity,
    startedAt,
    finishedAt,
    wallMs,
    outcome,
    checks: verification?.checks ?? [],
    notes: [...(verification?.notes ?? []), ...journal.notes],
    toolCalls: journal.toolCalls.length,
    acceptedToolCalls: journal.sentCount,
    abortReason: journal.abortReason,
    errorMessage,
    serverCrashed,
    usage: journal.usage,
    artifactPath,
    serverLogPath: server?.logPath ?? "",
    directoryKept: keep,
  };
}

export interface CampaignSummary {
  startedAt: string;
  finishedAt: string;
  config: Omit<CampaignConfig, "documentation"> & { documentationSha256: string };
  environment: Record<string, unknown>;
  model: Record<string, unknown>;
  runs: RunResult[];
  totals: {
    runs: number;
    pass: number;
    partial: number;
    fail: number;
    error: number;
    acceptedToolCalls: number;
    cost: number;
    tokens: number;
    serverCrashes: number;
  };
}

export async function runCampaign(
  sdk: PiSdk,
  modelRuntime: unknown,
  model: ResolvedModel,
  config: CampaignConfig,
  scenarios: Scenario[],
): Promise<CampaignSummary> {
  const startedAt = new Date().toISOString();
  mkdirSync(config.artifactsRoot, { recursive: true });
  const runs: RunResult[] = [];

  for (const scenario of scenarios) {
    for (let repetition = 1; repetition <= config.repetitions; repetition += 1) {
      const variant = scenario.variants[(repetition - 1) % scenario.variants.length];
      const identity: RunIdentity = {
        runId: `${scenario.id}-${variant}-r${repetition}`,
        scenarioId: scenario.id,
        scenarioTitle: scenario.title,
        variant,
        repetition,
      };
      process.stderr.write(`\n=== ${identity.runId} ===\n`);
      const result = await runOnce(sdk, modelRuntime, model, config, scenario, identity);
      runs.push(result);
      process.stderr.write(
        `${identity.runId}: ${result.outcome} (${result.acceptedToolCalls} calls, ${Math.round(result.wallMs)}ms` +
          `${result.abortReason ? `, aborted: ${result.abortReason}` : ""}` +
          `${result.serverCrashed ? ", FUX SERVER DIED" : ""}` +
          `${result.errorMessage ? ", error" : ""})\n`,
      );
      for (const check of result.checks.filter((entry) => !entry.ok)) {
        process.stderr.write(`  failed check: ${check.name} — ${check.detail}\n`);
      }
    }
  }

  const totals = {
    runs: runs.length,
    pass: runs.filter((run) => run.outcome === "pass").length,
    partial: runs.filter((run) => run.outcome === "partial").length,
    fail: runs.filter((run) => run.outcome === "fail").length,
    error: runs.filter((run) => run.outcome === "error").length,
    serverCrashes: runs.filter((run) => run.serverCrashed).length,
    acceptedToolCalls: runs.reduce((sum, run) => sum + run.acceptedToolCalls, 0),
    cost: runs.reduce((sum, run) => sum + (run.usage?.cost ?? 0), 0),
    tokens: runs.reduce((sum, run) => sum + (run.usage?.tokens.total ?? 0), 0),
  };

  const { documentation, environment, ...rest } = config;
  const summary: CampaignSummary = {
    startedAt,
    finishedAt: new Date().toISOString(),
    config: { ...rest, environment, documentationSha256: documentation.sha256 },
    environment,
    model: {
      providerId: model.providerId,
      modelId: model.modelId,
      displayName: model.displayName,
      rejectedAliases: model.rejectedAliases,
      verification: model.verification,
    },
    runs,
    totals,
  };
  writeFileSync(
    join(config.artifactsRoot, "campaign.json"),
    `${JSON.stringify(summary, null, 2)}\n`,
  );
  return summary;
}
