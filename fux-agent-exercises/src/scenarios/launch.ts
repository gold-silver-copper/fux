/**
 * Scenario 2 — exact launch and completion.
 *
 * The agent must start a fixture with an exact argv and cwd, decide whether it
 * succeeded, and leave its final output readable. The fixture records what it
 * actually received, so the verifier never has to trust the agent's account.
 */
import { existsSync, mkdirSync, readFileSync, realpathSync } from "node:fs";
import { join, resolve } from "node:path";
import { attach } from "../brp.ts";
import {
  COMPONENT,
  contentRows,
  decodeViewer,
  processes,
  viewerSnapshot,
  writeScript,
} from "../world.ts";
import { type Check, type Scenario, type ScenarioContext, type ScenarioRun, type ScenarioSetup, summarize } from "./types.ts";

interface Variant {
  exitCode: number;
  marker: string;
  verdict: "SUCCESS" | "FAILURE";
}

const VARIANTS: Record<string, Variant> = {
  pass: { exitCode: 0, marker: "CHECKS-COMPLETE ok=12 failed=0", verdict: "SUCCESS" },
  fail: { exitCode: 3, marker: "CHECKS-COMPLETE ok=9 failed=3", verdict: "FAILURE" },
};

const ARGUMENT = "--mode=check";

interface State {
  viewer: number;
  scriptPath: string;
  projectDir: string;
  recordPath: string;
  variant: Variant;
}

/** Compares paths through realpath so /tmp vs /private/tmp does not matter. */
function samePath(left: string | null | undefined, right: string): boolean {
  if (!left) return false;
  const canonical = (value: string) => {
    try {
      return realpathSync(value);
    } catch {
      return resolve(value);
    }
  };
  return canonical(left) === canonical(right);
}

function create(variantName: string): ScenarioRun {
  const variant = VARIANTS[variantName];
  if (!variant) throw new Error(`unknown launch variant ${variantName}`);
  let state: State;

  return {
    async setup(ctx: ScenarioContext): Promise<ScenarioSetup> {
      const { client } = ctx;
      const projectDir = join(ctx.server.workDir, "project");
      mkdirSync(projectDir, { recursive: true });
      const recordPath = join(ctx.fixtureDir, "launch-record.txt");

      // The record path is baked in: fux children inherit the server's
      // environment, not the harness's, so an env var would not arrive.
      const scriptPath = writeScript(
        ctx.server.workDir,
        "check.sh",
        `#!/bin/sh
{
  printf 'argv0=%s\\n' "$0"
  printf 'argc=%s\\n' "$#"
  printf 'args=%s\\n' "$*"
  printf 'cwd=%s\\n' "$(pwd)"
} > ${JSON.stringify(recordPath)}
echo "running project checks"
echo "${variant.marker}"
exit ${variant.exitCode}
`,
      );

      const viewer = await attach(client, 24, 80, "main");
      state = { viewer, scriptPath, projectDir, recordPath, variant };

      const prompt = [
        `You are operating a running fux server. Your viewer entity id is ${viewer}.`,
        "",
        "Start this program in a new pane in the tab your viewer is currently on:",
        "",
        `  program: ${scriptPath}`,
        `  single argument: ${ARGUMENT}`,
        `  working directory: ${projectDir}`,
        "",
        "It must run with exactly that argument vector and exactly that working directory.",
        "",
        "Then wait until it has finished, determine whether it succeeded, and leave its final output",
        "visible in your viewer.",
        "",
        "Finish your reply with exactly one line, and nothing after it:",
        "VERDICT: SUCCESS",
        "or",
        "VERDICT: FAILURE",
      ].join("\n");

      return {
        prompt,
        baseline: {
          viewer,
          scriptPath,
          projectDir,
          expectedArgv: [scriptPath, ARGUMENT],
          expectedExitCode: variant.exitCode,
          expectedVerdict: variant.verdict,
          marker: variant.marker,
        },
      };
    },

    async verify(ctx: ScenarioContext) {
      const { client, journal } = ctx;
      const checks: Check[] = [];
      const notes: string[] = [];

      // 1. What the fixture itself observed.
      const recorded = existsSync(state.recordPath)
        ? readFileSync(state.recordPath, "utf8")
        : null;
      const fields = new Map<string, string>();
      for (const line of (recorded ?? "").split("\n")) {
        const index = line.indexOf("=");
        if (index > 0) fields.set(line.slice(0, index), line.slice(index + 1));
      }
      checks.push({
        name: "fixture ran",
        ok: recorded !== null,
        detail: recorded === null ? `no record at ${state.recordPath}` : `record: ${JSON.stringify(recorded)}`,
      });
      checks.push({
        name: "fixture received the exact argument vector",
        ok: samePath(fields.get("argv0"), state.scriptPath) && fields.get("args") === ARGUMENT && fields.get("argc") === "1",
        detail: `argv0=${String(fields.get("argv0"))} argc=${String(fields.get("argc"))} args=${String(fields.get("args"))}; expected ${state.scriptPath} with single argument ${ARGUMENT}`,
      });
      checks.push({
        name: "fixture ran in the requested working directory",
        ok: samePath(fields.get("cwd"), state.projectDir),
        detail: `cwd=${String(fields.get("cwd"))} expected ${state.projectDir}`,
      });

      // 2. The launch recipe fux actually holds.
      const all = await processes(client);
      const launched = all.filter((process) =>
        (process.argv ?? []).some((entry) => entry.includes("check.sh")),
      );
      const exact = launched.find(
        (process) =>
          process.argv?.length === 2 &&
          samePath(process.argv[0], state.scriptPath) &&
          process.argv[1] === ARGUMENT &&
          samePath(process.cwd, state.projectDir),
      );
      checks.push({
        name: "Launch recipe records the exact argv and cwd",
        ok: exact !== undefined,
        detail:
          launched.length === 0
            ? "no process entity references check.sh"
            : `candidates: ${JSON.stringify(launched.map((process) => ({ entity: process.entity, argv: process.argv, cwd: process.cwd })))}`,
      });

      const finished = launched.find((process) => process.status.kind === "exited");
      checks.push({
        name: "process reached the expected exit status",
        ok: finished?.status.code === state.variant.exitCode,
        detail: `observed ${JSON.stringify(launched.map((process) => process.status))}; expected exited code ${state.variant.exitCode}`,
      });

      // 3. What a human would now see.
      const screen = await decodeViewer(client, state.viewer);
      const visible = contentRows(screen).some((row) => row.includes(state.variant.marker));
      checks.push({
        name: "final output left visible",
        ok: visible,
        detail: visible
          ? `marker present in the viewer's content rows`
          : `marker ${JSON.stringify(state.variant.marker)} not in content rows: ${JSON.stringify(contentRows(screen).filter((row) => row.length > 0).slice(-6))}`,
      });

      // 4. The agent's own claim, checked against the truth.
      const answer = journal.finalAnswer ?? "";
      const verdicts = [...answer.matchAll(/VERDICT:\s*(SUCCESS|FAILURE)/g)].map((match) => match[1]);
      const claimed = verdicts.at(-1) ?? null;
      checks.push({
        name: "agent reported the correct verdict",
        ok: claimed === state.variant.verdict,
        detail: `claimed=${String(claimed)} expected=${state.variant.verdict}${verdicts.length > 1 ? ` (multiple verdict lines: ${verdicts.join(",")})` : ""}`,
      });

      if (exact === undefined && launched.length > 0) {
        notes.push(
          "the program started, but not through an exact Launch recipe; check whether a shell wrapper was used",
        );
      }

      return summarize(checks, notes, {
        record: recorded,
        launched,
        viewer: await viewerSnapshot(client, state.viewer),
        contentRows: contentRows(screen).filter((row) => row.length > 0),
        claimedVerdict: claimed,
        launchComponent: COMPONENT.launch,
      });
    },
  };
}

export const launchScenario: Scenario = {
  id: "launch",
  title: "Exact launch and completion",
  variants: ["pass", "fail"],
  create,
};
