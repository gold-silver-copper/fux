/**
 * Turns campaign artifacts into a compact, quotable evidence table.
 *
 * This produces measurements and pointers, not conclusions: classifying a
 * finding as a fux bug, a documentation gap or an agent mistake is a judgement
 * that belongs in the written report, with the run ids cited here.
 */
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { renderFindings, type RunArtifact } from "./findings.ts";
import type { CampaignSummary, RunResult } from "./runner.ts";

export function loadCampaign(artifactsRoot: string): CampaignSummary {
  return JSON.parse(readFileSync(join(artifactsRoot, "campaign.json"), "utf8")) as CampaignSummary;
}

/** The run's artifact, by its recorded path or, if the campaign moved, beside `campaign.json`. */
function loadArtifact(artifactsRoot: string, run: RunResult): RunArtifact {
  const candidates = [run.artifactPath, join(artifactsRoot, run.runId, "run.json")];
  const path = candidates.find((candidate) => existsSync(candidate));
  if (path === undefined) throw new Error(`no artifact for ${run.runId}`);
  return JSON.parse(readFileSync(path, "utf8")) as RunArtifact;
}

function methodHistogram(artifactsRoot: string, run: RunResult): Map<string, number> {
  const artifact = loadArtifact(artifactsRoot, run);
  const counts = new Map<string, number>();
  for (const call of artifact.journal.toolCalls) {
    const key = call.rejected === null ? call.method : `${call.method} (rejected)`;
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return counts;
}

export function renderReport(artifactsRoot: string): string {
  const campaign = loadCampaign(artifactsRoot);
  const lines: string[] = [];
  const { totals, model, config } = campaign;

  lines.push("# fux agent exercise campaign — measurements");
  lines.push("");
  lines.push(`- Started: ${campaign.startedAt}`);
  lines.push(`- Finished: ${campaign.finishedAt}`);
  lines.push(`- Model: \`${model.providerId}/${model.modelId}\` (${String(model.displayName)})`);
  const verification = model.verification as { verifiedAt?: string; sources?: string[]; ok?: boolean; liveApiVersion?: string | null };
  lines.push(
    `- Model verification: ${verification.ok ? "passed" : "NOT passed"} at ${String(verification.verifiedAt)}; sources: ${(verification.sources ?? []).join(", ")}; live API version \`${String(verification.liveApiVersion)}\``,
  );
  lines.push(`- Rejected Flash aliases: ${(model.rejectedAliases as string[]).map((id) => `\`${id}\``).join(", ")}`);
  lines.push(`- pi: ${String((campaign.environment as Record<string, unknown>).piVersion)}`);
  lines.push(
    `- fux: \`${String((campaign.environment as Record<string, unknown>).fuxGitRevision)}\`` +
      `${(campaign.environment as Record<string, unknown>).fuxGitDirty ? " (worktree dirty)" : ""}`,
  );
  lines.push(
    `- Budgets: ${config.maxToolCalls} tool calls, ${config.runDeadlineMs} ms wall clock, ${config.requestTimeoutMs} ms per request; thinking level \`${config.thinkingLevel}\``,
  );
  lines.push(`- Documentation supplied to the agent: sha256 \`${config.documentationSha256}\``);
  lines.push("");
  lines.push(
    `**Outcomes:** ${totals.pass} pass, ${totals.partial} partial, ${totals.fail} fail, ${totals.error} error, out of ${totals.runs} runs.`,
  );
  lines.push(
    totals.serverCrashes === undefined
      ? "**fux server crashes:** not recorded by the harness version that produced this campaign."
      : `**fux server crashes:** ${totals.serverCrashes} run(s) ended with the run's fux server process no longer alive.`,
  );
  lines.push(
    `**Cost as reported by pi:** ${totals.cost.toFixed(4)} across ${totals.tokens.toLocaleString("en-US")} tokens and ${totals.acceptedToolCalls} accepted BRP requests.`,
  );
  lines.push("");

  lines.push("## Per-run results");
  lines.push("");
  lines.push("| Run | Outcome | BRP calls | Wall (s) | Tokens | Cost | Server died | Aborted |");
  lines.push("| --- | --- | ---: | ---: | ---: | ---: | --- | --- |");
  for (const run of campaign.runs) {
    lines.push(
      `| \`${run.runId}\` | ${run.outcome} | ${run.acceptedToolCalls} | ${(run.wallMs / 1000).toFixed(1)} | ${
        run.usage?.tokens.total ?? 0
      } | ${(run.usage?.cost ?? 0).toFixed(4)} | ${
        run.serverCrashed === undefined ? "unrecorded" : run.serverCrashed ? "**yes**" : "no"
      } | ${run.abortReason ?? "—"} |`,
    );
  }
  lines.push("");

  lines.push("## Failed and soft-failed checks");
  lines.push("");
  let anyFailure = false;
  for (const run of campaign.runs) {
    const failed = run.checks.filter((check) => !check.ok);
    if (failed.length === 0 && run.errorMessage === null) continue;
    anyFailure = true;
    lines.push(`### \`${run.runId}\` — ${run.outcome}`);
    lines.push("");
    if (run.serverCrashed) {
      lines.push("- **the run's fux server process died before cleanup**");
    }
    if (run.errorMessage) {
      lines.push(`- error: \`${run.errorMessage.split("\n")[0]}\``);
    }
    for (const check of failed) {
      lines.push(`- **${check.name}** — ${check.detail}`);
    }
    lines.push("");
  }
  if (!anyFailure) {
    lines.push("Every check passed in every run.");
    lines.push("");
  }

  lines.push("## Request mix");
  lines.push("");
  lines.push("| Run | Methods used (count) |");
  lines.push("| --- | --- |");
  for (const run of campaign.runs) {
    try {
      const histogram = [...methodHistogram(artifactsRoot, run).entries()]
        .sort((left, right) => right[1] - left[1])
        .map(([method, count]) => `${method}×${count}`)
        .join(", ");
      lines.push(`| \`${run.runId}\` | ${histogram || "—"} |`);
    } catch {
      lines.push(`| \`${run.runId}\` | (artifact unavailable) |`);
    }
  }
  lines.push("");

  lines.push("## Verifier notes");
  lines.push("");
  for (const run of campaign.runs) {
    if (run.notes.length === 0) continue;
    lines.push(`### \`${run.runId}\``);
    lines.push("");
    for (const note of run.notes) lines.push(`- ${note}`);
    lines.push("");
  }

  const artifacts: RunArtifact[] = [];
  const unavailable: string[] = [];
  for (const run of campaign.runs) {
    try {
      artifacts.push(loadArtifact(artifactsRoot, run));
    } catch {
      unavailable.push(run.runId);
    }
  }
  lines.push(...renderFindings(artifacts));
  if (unavailable.length > 0) {
    lines.push(`Artifacts unavailable for: ${unavailable.map((id) => `\`${id}\``).join(", ")}.`);
    lines.push("");
  }

  return `${lines.join("\n")}\n`;
}
