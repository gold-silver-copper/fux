/**
 * Per-run evidence: every agent tool call, the raw BRP traffic it produced,
 * bounded session events, budget accounting and the final answer.
 *
 * Truncation is recorded twice, because the two audiences differ: what the model
 * was allowed to see, and what was kept on disk.
 */

/** Characters of a BRP response the agent is allowed to see in one tool result. */
export const MODEL_VISIBLE_LIMIT = 8_000;
/** Characters of a BRP response retained in the artifact file. */
export const ARTIFACT_LIMIT = 200_000;
/** Bounded number of session events retained per run. */
export const EVENT_LIMIT = 4_000;

export interface Truncation {
  applied: boolean;
  originalBytes: number;
  keptBytes: number;
}

export function truncate(text: string, limit: number): { text: string; truncation: Truncation } {
  const originalBytes = Buffer.byteLength(text, "utf8");
  if (text.length <= limit) {
    return { text, truncation: { applied: false, originalBytes, keptBytes: originalBytes } };
  }
  const kept = text.slice(0, limit);
  const marker = `\n[TRUNCATED BY HARNESS: kept ${limit} of ${text.length} characters]`;
  return {
    text: kept + marker,
    truncation: {
      applied: true,
      originalBytes,
      keptBytes: Buffer.byteLength(kept, "utf8"),
    },
  };
}

export interface ToolCallRecord {
  index: number;
  startedAt: string;
  finishedAt: string;
  method: string;
  paramsJson: string | null;
  /** Set when the harness refused to send the request at all. */
  rejected: string | null;
  httpStatus: number | null;
  transportError: string | null;
  jsonRpcError: unknown;
  durationMs: number;
  modelVisible: Truncation;
  artifact: Truncation;
  response: string | null;
}

export interface EventRecord {
  at: string;
  type: string;
  detail?: unknown;
}

export interface UsageRecord {
  tokens: { input: number; output: number; cacheRead: number; cacheWrite: number; total: number };
  cost: number;
  toolCalls: number;
  assistantMessages: number;
}

export class RunJournal {
  readonly toolCalls: ToolCallRecord[] = [];
  readonly events: EventRecord[] = [];
  readonly notes: string[] = [];
  eventsDropped = 0;
  finalAnswer: string | null = null;
  usage: UsageRecord | null = null;
  abortReason: string | null = null;
  errorMessage: string | null = null;

  recordEvent(type: string, detail?: unknown): void {
    if (this.events.length >= EVENT_LIMIT) {
      this.eventsDropped += 1;
      return;
    }
    this.events.push({ at: new Date().toISOString(), type, ...(detail === undefined ? {} : { detail }) });
  }

  note(text: string): void {
    this.notes.push(text);
  }

  /** Counts only requests that actually reached the server. */
  get sentCount(): number {
    return this.toolCalls.filter((call) => call.rejected === null).length;
  }
}
