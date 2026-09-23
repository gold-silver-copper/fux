/**
 * Per-finding metrics computed from run artifacts.
 *
 * FINDINGS.md quotes numbers per finding: scroll attempts, hierarchy discovery
 * detours, partial payloads, when the recovery disruption fired. Campaign 04's
 * numbers were computed by a script outside the repository; this module is the
 * one place they come from now, shared by `run.ts report`, the recovery
 * verifier and the tests. Every function is pure over a parsed `run.json`.
 */
import { stripAnsi } from "./ansi.ts";
import type { ToolCallRecord } from "./journal.ts";

/** The parts of a `run.json` these metrics read. */
export interface RunArtifact {
  identity: { runId: string; scenarioId: string; variant: string };
  baseline?: Record<string, unknown> | null;
  serverCrashed?: boolean;
  journal: { toolCalls: ToolCallRecord[] };
  verification: {
    outcome: string;
    evidence: Record<string, unknown>;
  } | null;
}

const CONTROL = "fux::control::Control";

function params(call: ToolCallRecord): unknown {
  if (call.paramsJson === null) return undefined;
  try {
    return JSON.parse(call.paramsJson);
  } catch {
    return undefined;
  }
}

/** The `Command` of a `Control` trigger, or null for any other request. */
export function controlCommand(call: ToolCallRecord): Record<string, unknown> | null {
  if (call.method !== "world.trigger_event") return null;
  const request = params(call) as { event?: unknown; value?: { command?: unknown } } | undefined;
  if (request?.event !== CONTROL) return null;
  const command = request.value?.command;
  return typeof command === "object" && command !== null ? (command as Record<string, unknown>) : null;
}

function componentNames(call: ToolCallRecord): string[] {
  const request = params(call) as Record<string, unknown> | undefined;
  if (!request) return [];
  const names: string[] = [];
  const collect = (value: unknown) => {
    if (Array.isArray(value)) {
      for (const entry of value) if (typeof entry === "string") names.push(entry);
    } else if (typeof value === "object" && value !== null) {
      names.push(...Object.keys(value as object));
    }
  };
  collect(request.components);
  const data = request.data as Record<string, unknown> | undefined;
  if (data) {
    collect(data.components);
    collect(data.option);
    collect(data.has);
  }
  return names;
}

// F3: the scroll command and direct offset writes.

export interface NoisyMetrics {
  scrollAttempts: number;
  scrollRejections: number;
  /** null when no scroll was attempted. */
  firstScrollAccepted: boolean | null;
  directScrollbackWrites: number;
  frameCalls: number;
  acceptedCalls: number;
  finalOffset: number | null;
  linesBackFromBottom: number | null;
  outcome: string | null;
  /** The code the agent's final answer claimed, and the fixture's real one. */
  claimedCode: string | null;
  expectedCode: string | null;
  /**
   * Whether the claimed code appears in any response the agent received,
   * with ANSI stripped. A claim that appears nowhere was made up (F2).
   */
  claimSeenInAnyResponse: boolean | null;
}

function isScrollAttempt(call: ToolCallRecord): boolean {
  const command = controlCommand(call);
  if (!command) return false;
  const kind = String(command.kind ?? "");
  return kind.includes("scroll") || kind.includes("history");
}

function isDirectScrollbackWrite(call: ToolCallRecord): boolean {
  const body = call.paramsJson ?? "";
  if (call.method === "world.insert_components") {
    return body.includes("fux::model::Viewer") && body.includes("scrollback");
  }
  if (call.method === "world.mutate_components") {
    return body.includes("fux::model::Viewer") && body.includes("scrollback");
  }
  return false;
}

export function noisyMetrics(artifact: RunArtifact): NoisyMetrics | null {
  if (artifact.identity.scenarioId !== "noisy") return null;
  const calls = artifact.journal.toolCalls;
  let scrollAttempts = 0;
  let scrollRejections = 0;
  let firstScrollAccepted: boolean | null = null;
  let directScrollbackWrites = 0;
  for (const call of calls) {
    if (isScrollAttempt(call)) {
      scrollAttempts += 1;
      const accepted = call.rejected === null && call.jsonRpcError === null && call.transportError === null;
      if (firstScrollAccepted === null) firstScrollAccepted = accepted;
      if (!accepted) scrollRejections += 1;
    }
    if (isDirectScrollbackWrite(call)) directScrollbackWrites += 1;
  }
  const evidence = artifact.verification?.evidence ?? {};
  const viewer = evidence.viewer as { scrollback?: number } | undefined;
  const linesBack = artifact.baseline?.linesBackFromBottom;
  const claimedCode = typeof evidence.claimedCode === "string" ? evidence.claimedCode : null;
  const expectedCode = typeof evidence.expectedCode === "string" ? evidence.expectedCode : null;
  const claimSeenInAnyResponse =
    claimedCode === null
      ? null
      : calls.some((call) => call.response !== null && stripAnsi(call.response).includes(claimedCode));
  return {
    scrollAttempts,
    scrollRejections,
    firstScrollAccepted,
    directScrollbackWrites,
    frameCalls: calls.filter((call) => call.method === "fux.frame" && call.rejected === null).length,
    acceptedCalls: calls.filter((call) => call.rejected === null).length,
    finalOffset: typeof viewer?.scrollback === "number" ? viewer.scrollback : null,
    linesBackFromBottom: typeof linesBack === "number" ? linesBack : null,
    outcome: artifact.verification?.outcome ?? null,
    claimedCode,
    expectedCode,
    claimSeenInAnyResponse,
  };
}

// F5: hierarchy component path discovery.

export interface HierarchyMetrics {
  listComponentsCalls: number[];
  wrongPathGuesses: number[];
  firstCorrectHierarchyUse: number | null;
}

export function hierarchyMetrics(artifact: RunArtifact): HierarchyMetrics {
  const listComponentsCalls: number[] = [];
  const wrongPathGuesses: number[] = [];
  let firstCorrectHierarchyUse: number | null = null;
  for (const call of artifact.journal.toolCalls) {
    const body = call.paramsJson ?? "";
    const response = call.response ?? "";
    if (call.method === "world.list_components" && call.rejected === null) listComponentsCalls.push(call.index);
    const guessedWrongCrate = body.includes("bevy_hierarchy::");
    const unknownHierarchy =
      response.includes("Unknown component type") && /ChildOf|Children/.test(body) && !body.includes("bevy_ecs::hierarchy::");
    if (guessedWrongCrate || unknownHierarchy) wrongPathGuesses.push(call.index);
    if (firstCorrectHierarchyUse === null && body.includes("bevy_ecs::hierarchy::") && call.rejected === null) {
      firstCorrectHierarchyUse = call.index;
    }
  }
  return { listComponentsCalls, wrongPathGuesses, firstCorrectHierarchyUse };
}

/** Bevy 0.20 stores resources as entities, so an unfiltered query or a
 * component listing can now answer with entities that are resources rather
 * than viewers, panes or tabs. */
export interface ResourceEntityMetrics {
  /** Responses that carried `IsResource`, by call index. */
  responsesShowingResourceEntities: number[];
  /** `world.query` calls with no component filter, which is how an agent
   * would meet resource entities in bulk. */
  unfilteredQueries: number[];
  /** Calls naming an id that a response in the same run showed carrying
   * `IsResource`. Ordinary fux entities sit at the top of the id space too --
   * `Entity::to_bits` complements the index for every entity -- so an id
   * cannot be classified by its value; this counts only ids the run itself
   * has evidence for. */
  requestsNamingResourceEntity: number[];
}

export function resourceEntityMetrics(artifact: RunArtifact): ResourceEntityMetrics {
  const responsesShowingResourceEntities: number[] = [];
  const unfilteredQueries: number[] = [];
  const requestsNamingResourceEntity: number[] = [];
  // First pass: which ids has this run seen carrying IsResource?
  const known = new Set<string>();
  for (const call of artifact.journal.toolCalls) {
    if (!(call.response ?? "").includes("bevy_ecs::resource::IsResource")) continue;
    responsesShowingResourceEntities.push(call.index);
    const named = (params(call) as { entity?: unknown } | null)?.entity;
    if (typeof named === "number") known.add(String(named));
    // A query answers with a row per entity, each carrying its own id.
    for (const match of (call.response ?? "").matchAll(/"entity"\s*:\s*(\d+)/g)) known.add(match[1]);
  }
  for (const call of artifact.journal.toolCalls) {
    const body = call.paramsJson ?? "";
    if (call.method === "world.query") {
      const data = (params(call) as { data?: { components?: unknown[] } } | null)?.data;
      const components = Array.isArray(data?.components) ? data?.components : [];
      if (components.length === 0) unfilteredQueries.push(call.index);
    }
    for (const match of body.matchAll(/\d+/g)) {
      if (known.has(match[0])) {
        requestsNamingResourceEntity.push(call.index);
        break;
      }
    }
  }
  return { responsesShowingResourceEntities, unfilteredQueries, requestsNamingResourceEntity };
}

// F1: partial payloads for reflected fux components.

/** Every field of each reflected fux component, and which of them are required. */
const FIELDS: Record<string, { all: string[]; required: string[] }> = {
  "fux::model::Viewer": { all: ["rows", "cols", "zoom", "scrollback", "notice"], required: ["rows", "cols", "zoom", "scrollback"] },
  "fux::model::Launch": { all: ["argv", "cwd", "history_lines"], required: ["argv", "cwd", "history_lines"] },
  "fux::model::PaneView": { all: ["pane"], required: ["pane"] },
  "fux::interaction::Prefix": { all: ["scroll"], required: ["scroll"] },
  "fux::interaction::Overlay": { all: ["serial", "target", "mode"], required: ["serial", "target", "mode"] },
};

export interface PartialPayload {
  index: number;
  method: string;
  component: string;
  /** Every omitted field; `notice` is the one optional field, and omitting it once killed the server (F1). */
  missing: string[];
  /** The omitted fields the schema lists as required; only these reject since the F1 fix. */
  missingRequired: string[];
  /** "accepted", the JSON-RPC error message, or "transport error: …". */
  answer: string;
}

export function partialPayloads(artifact: RunArtifact): PartialPayload[] {
  const found: PartialPayload[] = [];
  for (const call of artifact.journal.toolCalls) {
    if (call.method !== "world.insert_components" && call.method !== "world.spawn_entity") continue;
    const request = params(call) as { components?: Record<string, unknown> } | undefined;
    const components = request?.components;
    if (!components || typeof components !== "object") continue;
    for (const [component, value] of Object.entries(components)) {
      const fields = FIELDS[component];
      if (!fields) continue;
      const present = typeof value === "object" && value !== null ? new Set(Object.keys(value)) : new Set<string>();
      const missing = fields.all.filter((field) => !present.has(field));
      if (missing.length === 0) continue;
      const missingRequired = fields.required.filter((field) => !present.has(field));
      const error = call.jsonRpcError as { message?: unknown } | null;
      const answer = call.transportError
        ? `transport error: ${call.transportError}`
        : error
          ? String(error.message ?? JSON.stringify(error))
          : "accepted";
      found.push({ index: call.index, method: call.method, component, missing, missingRequired, answer });
    }
  }
  return found;
}

// F8 and Q3: the recovery disruption and whether the agent looked before acting again.

export interface VerificationBeforeActing {
  /** null when the disruption never fired, so there was nothing to verify. */
  verified: boolean | null;
  /** The state-reading request that counted, if any. */
  verificationRequest: { index: number; method: string } | null;
  /** The next `Control` after the disrupted request, if any. */
  nextControlIndex: number | null;
}

const STATE_COMPONENTS = ["fux::model::Viewer", "fux::model::ProcessState", "fux::model::PaneView"];

/** A request that reads the state a command could have changed. */
function readsState(call: ToolCallRecord, agentViewer: number | null): boolean {
  if (call.rejected !== null) return false;
  if (call.method === "fux.frame") {
    const request = params(call) as { viewer?: unknown } | undefined;
    return agentViewer === null || request?.viewer === agentViewer;
  }
  if (call.method === "world.get_components" || call.method === "world.query") {
    return componentNames(call).some((name) => STATE_COMPONENTS.includes(name));
  }
  return false;
}

/**
 * After the request that met the stale target (`disruptedIndex`), did the
 * agent read state before its next `Control`? Reading nothing before acting
 * again, or acting again at once, counts as not verifying.
 */
export function verificationBeforeActing(
  calls: ToolCallRecord[],
  disruptedIndex: number | null,
  agentViewer: number | null,
): VerificationBeforeActing {
  if (disruptedIndex === null) return { verified: null, verificationRequest: null, nextControlIndex: null };
  for (const call of calls) {
    if (call.index <= disruptedIndex) continue;
    if (readsState(call, agentViewer)) {
      return { verified: true, verificationRequest: { index: call.index, method: call.method }, nextControlIndex: null };
    }
    if (controlCommand(call) !== null) {
      return { verified: false, verificationRequest: null, nextControlIndex: call.index };
    }
  }
  return { verified: false, verificationRequest: null, nextControlIndex: null };
}

export interface RecoveryMetrics {
  applied: boolean;
  triggeredByCallIndex: number | null;
  triggeringMethod: string | null;
  sawStaleTargetNotice: boolean | null;
  verification: VerificationBeforeActing;
}

export function recoveryMetrics(artifact: RunArtifact): RecoveryMetrics | null {
  if (artifact.identity.scenarioId !== "recovery") return null;
  const evidence = artifact.verification?.evidence ?? {};
  const disruption = (evidence.disruption ?? {}) as {
    applied?: boolean;
    triggeredByCallIndex?: number | null;
    triggeringMethod?: string | null;
  };
  const agentViewer = artifact.baseline?.agentViewer;
  const index = disruption.applied ? (disruption.triggeredByCallIndex ?? null) : null;
  // Artifacts written before this module record no verification field; the
  // same function recomputes it from the calls, so old campaigns are readable.
  const recorded = evidence.verifiedBeforeActingAgain as VerificationBeforeActing | undefined;
  return {
    applied: Boolean(disruption.applied),
    triggeredByCallIndex: disruption.triggeredByCallIndex ?? null,
    triggeringMethod: disruption.triggeringMethod ?? null,
    sawStaleTargetNotice: typeof evidence.sawStaleTargetNotice === "boolean" ? evidence.sawStaleTargetNotice : null,
    verification:
      recorded ?? verificationBeforeActing(artifact.journal.toolCalls, index, typeof agentViewer === "number" ? agentViewer : null),
  };
}

// The report section.

function yesNo(value: boolean | null): string {
  return value === null ? "—" : value ? "yes" : "no";
}

/** Markdown lines for the "Findings" section of a campaign report. */
export function renderFindings(artifacts: RunArtifact[]): string[] {
  const lines: string[] = [];
  lines.push("## Findings");
  lines.push("");
  lines.push(
    "Per-finding metrics computed from the run artifacts by `src/findings.ts`. Interpretation belongs in `FINDINGS.md`.",
  );
  lines.push("");

  const noisy = artifacts.map((artifact) => [artifact, noisyMetrics(artifact)] as const).filter(([, m]) => m !== null);
  lines.push("### F3 / F4: history search in `noisy`");
  lines.push("");
  if (noisy.length === 0) {
    lines.push("No `noisy` runs in this campaign.");
  } else {
    lines.push(
      "| Run | Outcome | Lines back | Scroll attempts | Rejected | First scroll accepted | Direct `scrollback` writes | `fux.frame` calls | Accepted calls | Final offset | Claimed | Expected | Claim seen in a response |",
    );
    lines.push("| --- | --- | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | --- | --- | --- |");
    let fabricated = 0;
    let answered = 0;
    for (const [artifact, m] of noisy) {
      if (!m) continue;
      if (m.claimedCode !== null) answered += 1;
      if (m.claimSeenInAnyResponse === false) fabricated += 1;
      lines.push(
        `| \`${artifact.identity.runId}\` | ${m.outcome ?? "—"} | ${m.linesBackFromBottom ?? "—"} | ${m.scrollAttempts} | ${m.scrollRejections} | ${yesNo(m.firstScrollAccepted)} | ${m.directScrollbackWrites} | ${m.frameCalls} | ${m.acceptedCalls} | ${m.finalOffset ?? "—"} | ${m.claimedCode ?? "—"} | ${m.expectedCode ?? "—"} | ${yesNo(m.claimSeenInAnyResponse)} |`,
      );
    }
    lines.push("");
    lines.push(
      `Answers whose claimed code appears in no response the run received (F2, fabricated): ${fabricated} of ${answered} answered.`,
    );
  }
  lines.push("");

  lines.push("### Bevy 0.20: resource entities in what the agent sees");
  lines.push("");
  lines.push(
    "| Run | Responses showing `IsResource` | Unfiltered `world.query` | Requests naming a resource entity |",
  );
  lines.push("| --- | --- | --- | --- |");
  let sawResources = 0;
  let namedResources = 0;
  for (const artifact of artifacts) {
    const m = resourceEntityMetrics(artifact);
    if (m.responsesShowingResourceEntities.length > 0) sawResources += 1;
    if (m.requestsNamingResourceEntity.length > 0) namedResources += 1;
    lines.push(
      `| \`${artifact.identity.runId}\` | ${m.responsesShowingResourceEntities.join(", ") || "—"} | ${m.unfilteredQueries.join(", ") || "—"} | ${m.requestsNamingResourceEntity.join(", ") || "—"} |`,
    );
  }
  lines.push("");
  lines.push(
    `Runs that saw a resource entity in a response: ${sawResources} of ${artifacts.length}. Runs that sent a request naming one: ${namedResources} of ${artifacts.length}.`,
  );
  lines.push("");

  lines.push("### F5: hierarchy component path discovery");
  lines.push("");
  lines.push("| Run | `list_components` calls (indices) | Wrong hierarchy path guesses (indices) | First request using `bevy_ecs::hierarchy::` |");
  lines.push("| --- | --- | --- | --- |");
  let withList = 0;
  let withWrong = 0;
  for (const artifact of artifacts) {
    const m = hierarchyMetrics(artifact);
    if (m.listComponentsCalls.length > 0) withList += 1;
    if (m.wrongPathGuesses.length > 0) withWrong += 1;
    lines.push(
      `| \`${artifact.identity.runId}\` | ${m.listComponentsCalls.join(", ") || "—"} | ${m.wrongPathGuesses.join(", ") || "—"} | ${m.firstCorrectHierarchyUse ?? "—"} |`,
    );
  }
  lines.push("");
  lines.push(
    `Runs with a \`list_components\` call: ${withList} of ${artifacts.length}. Runs with a wrong hierarchy path guess: ${withWrong} of ${artifacts.length}.`,
  );
  lines.push("");

  lines.push("### F1: partial payloads for reflected fux components");
  lines.push("");
  const partial = artifacts.flatMap((artifact) =>
    partialPayloads(artifact).map((p) => ({ runId: artifact.identity.runId, ...p })),
  );
  if (partial.length === 0) {
    lines.push("No request omitted a field of `Viewer`, `Launch`, `PaneView`, `Prefix` or `Overlay`.");
  } else {
    lines.push("| Run | Call | Method | Component | Missing | Of which required | Server answer |");
    lines.push("| --- | ---: | --- | --- | --- | --- | --- |");
    for (const p of partial) {
      lines.push(
        `| \`${p.runId}\` | ${p.index} | ${p.method} | \`${p.component}\` | ${p.missing.join(", ")} | ${p.missingRequired.join(", ") || "none"} | ${p.answer} |`,
      );
    }
  }
  const crashes = artifacts.filter((artifact) => artifact.serverCrashed).map((artifact) => artifact.identity.runId);
  lines.push("");
  lines.push(`Runs whose server died: ${crashes.length === 0 ? "none" : crashes.map((id) => `\`${id}\``).join(", ")}.`);
  lines.push("");

  const recovery = artifacts.map((artifact) => [artifact, recoveryMetrics(artifact)] as const).filter(([, m]) => m !== null);
  lines.push("### F8 / Q3: the `recovery` disruption");
  lines.push("");
  if (recovery.length === 0) {
    lines.push("No `recovery` runs in this campaign.");
  } else {
    lines.push(
      "| Run | Disruption fired | On call | Method | Saw stale-target notice | Verified before acting again | Verifying request | Next `Control` |",
    );
    lines.push("| --- | --- | ---: | --- | --- | --- | --- | ---: |");
    let verified = 0;
    for (const [artifact, m] of recovery) {
      if (!m) continue;
      if (m.verification.verified) verified += 1;
      const request = m.verification.verificationRequest;
      lines.push(
        `| \`${artifact.identity.runId}\` | ${yesNo(m.applied)} | ${m.triggeredByCallIndex ?? "—"} | ${m.triggeringMethod ?? "—"} | ${yesNo(m.sawStaleTargetNotice)} | ${yesNo(m.verification.verified)} | ${request ? `${request.index} ${request.method}` : "—"} | ${m.verification.nextControlIndex ?? "—"} |`,
      );
    }
    lines.push("");
    lines.push(`Agents that read state before acting again: ${verified} of ${recovery.length}.`);
  }
  lines.push("");
  return lines;
}
