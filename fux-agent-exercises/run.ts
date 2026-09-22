#!/usr/bin/env node
/**
 * Opt-in entry point for the fux agent exercise suite.
 *
 *   node run.ts preflight
 *   node run.ts dry-run
 *   node run.ts campaign
 *   node run.ts report --artifacts runs/<id>
 *
 * `campaign` spends real money on real model calls. `preflight` and `dry-run`
 * do not contact a model.
 */
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { preflight } from "./src/preflight.ts";
import { loadDocumentation } from "./src/prompt.ts";
import { renderReport } from "./src/report.ts";
import { runCampaign, type CampaignConfig } from "./src/runner.ts";
import { SCENARIOS, scenarioById } from "./src/scenarios/index.ts";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..");

interface Options {
  command: string;
  fux: string;
  scenarios: string[];
  repetitions: number;
  thinking: string;
  maxCalls: number;
  deadlineMs: number;
  requestTimeoutMs: number;
  artifacts: string;
  keep: "always" | "on-failure";
  provider: string;
  model?: string;
  skipModelVerification: boolean;
  readme: string;
}

function parseArgs(argv: string[]): Options {
  const options: Options = {
    command: argv[0] ?? "help",
    fux: join(repoRoot, "target", "release", "fux"),
    scenarios: SCENARIOS.map((scenario) => scenario.id),
    repetitions: 2,
    thinking: "low",
    maxCalls: 40,
    deadlineMs: 300_000,
    requestTimeoutMs: 15_000,
    artifacts: join(here, "runs", new Date().toISOString().replace(/[:.]/g, "-")),
    keep: "always",
    provider: "google",
    skipModelVerification: false,
    readme: join(repoRoot, "README.md"),
  };
  for (let index = 1; index < argv.length; index += 1) {
    const flag = argv[index];
    const value = argv[index + 1];
    const needsValue = () => {
      if (value === undefined) throw new Error(`${flag} needs a value`);
      index += 1;
      return value;
    };
    switch (flag) {
      case "--fux":
        options.fux = resolve(needsValue());
        break;
      case "--scenarios":
        options.scenarios = needsValue()
          .split(",")
          .map((entry) => entry.trim())
          .filter((entry) => entry.length > 0);
        break;
      case "--repetitions":
        options.repetitions = Number(needsValue());
        break;
      case "--thinking":
        options.thinking = needsValue();
        break;
      case "--max-calls":
        options.maxCalls = Number(needsValue());
        break;
      case "--deadline-ms":
        options.deadlineMs = Number(needsValue());
        break;
      case "--request-timeout-ms":
        options.requestTimeoutMs = Number(needsValue());
        break;
      case "--artifacts": {
        const provided = needsValue();
        options.artifacts = isAbsolute(provided) ? provided : resolve(provided);
        break;
      }
      case "--keep":
        options.keep = needsValue() === "on-failure" ? "on-failure" : "always";
        break;
      case "--provider":
        options.provider = needsValue();
        break;
      case "--model":
        options.model = needsValue();
        break;
      case "--skip-model-verification":
        options.skipModelVerification = true;
        break;
      case "--readme":
        options.readme = resolve(needsValue());
        break;
      default:
        throw new Error(`unknown flag ${flag}`);
    }
  }
  return options;
}

function usage(): void {
  process.stdout.write(
    [
      "fux agent exercise suite",
      "",
      "  node run.ts preflight    check fux, pi, credentials and resolve the required model",
      "  node run.ts dry-run      run every scenario's setup and verifier without any model call",
      "  node run.ts campaign     run the measured campaign (spends money)",
      "  node run.ts report       re-render the measurement table from artifacts",
      "",
      "Flags: --fux PATH --scenarios a,b --repetitions N --thinking LEVEL --max-calls N",
      "       --deadline-ms MS --request-timeout-ms MS --artifacts DIR --keep always|on-failure",
      "       --provider ID --model ID --skip-model-verification --readme PATH",
      "",
      `Scenarios: ${SCENARIOS.map((scenario) => scenario.id).join(", ")}`,
      "",
    ].join("\n"),
  );
}

const options = parseArgs(process.argv.slice(2));

if (options.command === "help" || options.command === "--help") {
  usage();
  process.exit(0);
}

if (options.command === "report") {
  process.stdout.write(renderReport(options.artifacts));
  process.exit(0);
}

const selected = options.scenarios.map((id) => {
  const scenario = scenarioById(id);
  if (!scenario) throw new Error(`unknown scenario ${id}; known: ${SCENARIOS.map((entry) => entry.id).join(", ")}`);
  return scenario;
});

const isDryRun = options.command === "dry-run";
const result = await preflight(
  {
    fuxBinary: options.fux,
    providerId: options.provider,
    pinnedModelId: options.model,
    skipModelVerification: options.skipModelVerification,
    offline: isDryRun,
  },
  repoRoot,
);

process.stderr.write(`fux: ${result.fux.path} (${String(result.fux.version)})\n`);
process.stderr.write(`fux revision: ${String(result.fux.gitRevision)}${result.fux.gitDirty ? " (dirty)" : ""}\n`);
process.stderr.write(`pi: ${result.pi.version} at ${result.pi.root}\n`);
if (result.provider) {
  process.stderr.write(
    `provider ${result.provider.id}: credential ${result.provider.authenticated ? "resolved" : "MISSING"}, ` +
      `${result.provider.availableModels} models available\n`,
  );
}
if (result.model) {
  process.stderr.write(
    `model: ${result.model.providerId}/${result.model.modelId} (${String(result.model.displayName)}), ` +
      `verification ${result.model.verification.ok ? "passed" : "FAILED"}\n`,
  );
  for (const warning of result.model.verification.warnings) {
    process.stderr.write(`  model warning: ${warning}\n`);
  }
  process.stderr.write(`  sources: ${result.model.verification.sources.join(", ")}\n`);
}
for (const problem of result.problems) {
  process.stderr.write(`PROBLEM: ${problem}\n`);
}

if (options.command === "preflight") {
  process.exit(result.ok ? 0 : 1);
}

if (!isDryRun && !result.ok) {
  process.stderr.write("\nRefusing to start the campaign: preflight did not pass.\n");
  process.exit(1);
}
if (options.command !== "campaign" && !isDryRun) {
  usage();
  process.exit(1);
}

const documentation = loadDocumentation(options.readme);
mkdirSync(options.artifacts, { recursive: true });

// Copied into every run artifact so one run.json stands alone as evidence.
const environment = {
  piVersion: result.pi.version,
  piRoot: result.pi.root,
  fuxPath: result.fux.path,
  fuxVersion: result.fux.version,
  fuxGitRevision: result.fux.gitRevision,
  fuxGitDirty: result.fux.gitDirty,
  node: process.version,
  platform: `${process.platform} ${process.arch}`,
  dryRun: isDryRun,
};

const config: CampaignConfig = {
  fuxBinary: options.fux,
  repoRoot,
  artifactsRoot: options.artifacts,
  documentation,
  thinkingLevel: options.thinking,
  maxToolCalls: options.maxCalls,
  runDeadlineMs: options.deadlineMs,
  requestTimeoutMs: options.requestTimeoutMs,
  repetitions: options.repetitions,
  keepArtifacts: options.keep,
  dryRun: isDryRun,
  environment,
};

const placeholderModel = {
  providerId: options.provider,
  modelId: "(none: dry run)",
  model: undefined,
  displayName: null,
  rejectedAliases: [],
  verification: {
    verifiedAt: new Date().toISOString(),
    sources: [],
    catalogIds: [],
    liveApiIds: null,
    docsIds: null,
    docsExcerpt: null,
    liveApiVersion: null,
    warnings: ["dry run: no model was resolved"],
    ok: false,
  },
};

const summary = await runCampaign(
  result.sdk,
  result.modelRuntime,
  (result.model ?? placeholderModel) as never,
  config,
  selected,
);

const report = renderReport(options.artifacts);
writeFileSync(join(options.artifacts, "measurements.md"), report);
process.stderr.write(
  `\n${summary.totals.pass} pass / ${summary.totals.partial} partial / ${summary.totals.fail} fail / ${summary.totals.error} error\n`,
);
process.stderr.write(`artifacts: ${options.artifacts}\n`);
process.exit(summary.totals.error > 0 ? 2 : 0);
