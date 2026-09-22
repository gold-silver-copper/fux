import type { BrpClient, BrpOutcome } from "../brp.ts";
import type { RunJournal, ToolCallRecord } from "../journal.ts";
import type { ServerHandle } from "../server.ts";

export interface ScenarioContext {
  server: ServerHandle;
  client: BrpClient;
  journal: RunJournal;
  variant: string;
  /** Run-private directory for fixture records the agent is not told about. */
  fixtureDir: string;
}

export interface ScenarioSetup {
  /** The task text handed to the agent. Recorded verbatim. */
  prompt: string;
  /** Baseline observations the verifier compares against. Recorded. */
  baseline: Record<string, unknown>;
}

export interface Check {
  name: string;
  ok: boolean;
  /** Required: what was actually observed, not a restatement of the check. */
  detail: string;
}

export interface Verification {
  outcome: "pass" | "partial" | "fail";
  checks: Check[];
  notes: string[];
  evidence: Record<string, unknown>;
}

export interface ScenarioRun {
  setup(ctx: ScenarioContext): Promise<ScenarioSetup>;
  /** Milestone hook: called after each agent tool call completes. */
  onToolCall?(ctx: ScenarioContext, record: ToolCallRecord, outcome: BrpOutcome | null): Promise<void> | void;
  /** Read-only observation started before the agent runs and stopped after. */
  startMonitor?(ctx: ScenarioContext): void;
  stopMonitor?(): void;
  verify(ctx: ScenarioContext, setup: ScenarioSetup): Promise<Verification>;
}

export interface Scenario {
  id: string;
  title: string;
  /** Deterministic variants; each repetition cycles through them. */
  variants: string[];
  /** Retained history for this scenario's server. */
  historyLines?: number;
  create(variant: string): ScenarioRun;
}

/** Derives an overall outcome: every required check must pass. */
export function summarize(checks: Check[], notes: string[], evidence: Record<string, unknown>, partialWhen: string[] = []): Verification {
  const failed = checks.filter((check) => !check.ok);
  if (failed.length === 0) {
    return { outcome: "pass", checks, notes, evidence };
  }
  const onlySoft = failed.every((check) => partialWhen.includes(check.name));
  return { outcome: onlySoft ? "partial" : "fail", checks, notes, evidence };
}
