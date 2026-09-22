/**
 * Scenario 3 — noisy-output investigation.
 *
 * A finished program left one diagnostic line that has already scrolled out of
 * the visible screen but is still inside retained history. The agent must find
 * it and report the code. The verifier knows the code independently.
 */
import { attach, eventually, triggerControl } from "../brp.ts";
import {
  addProcessPane,
  addTab,
  contentRows,
  decodeViewer,
  processByEntity,
  viewerSnapshot,
  workspaceByName,
  writeScript,
} from "../world.ts";
import { type Check, type Scenario, type ScenarioContext, type ScenarioRun, type ScenarioSetup, summarize } from "./types.ts";

interface Variant {
  code: string;
  failureLine: number;
  totalLines: number;
}

const VARIANTS: Record<string, Variant> = {
  a: { code: "E-4417", failureLine: 41, totalLines: 179 },
  b: { code: "E-8823", failureLine: 55, totalLines: 185 },
};

/** Retained history must comfortably exceed the distance to the diagnostic. */
const HISTORY_LINES = 500;

interface State {
  viewer: number;
  process: number;
  paneView: number;
  variant: Variant;
  diagnostic: string;
}

function create(variantName: string): ScenarioRun {
  const variant = VARIANTS[variantName];
  if (!variant) throw new Error(`unknown noisy variant ${variantName}`);
  const diagnostic = `FAILURE ${variant.code}: configuration parse error at line 12`;
  let state: State;

  return {
    async setup(ctx: ScenarioContext): Promise<ScenarioSetup> {
      const { client } = ctx;
      const script = writeScript(
        ctx.server.workDir,
        "build.sh",
        `#!/bin/sh
i=1
while [ $i -lt ${variant.failureLine} ]; do
  echo "step $i: compiling module_$i"
  i=$((i+1))
done
echo ${JSON.stringify(diagnostic)}
i=${variant.failureLine + 1}
while [ $i -le ${variant.totalLines} ]; do
  echo "step $i: compiling module_$i"
  i=$((i+1))
done
echo "build finished with errors"
`,
      );

      const workspace = await workspaceByName(client, "main");
      if (workspace === undefined) throw new Error("initial workspace 'main' is missing");
      // Its own tab, so the pane is full width and the evidence is unambiguous.
      const tab = await addTab(client, workspace, "build");
      const pane = await addProcessPane(client, {
        tab,
        argv: [script],
        cwd: ctx.server.workDir,
        name: "build",
        historyLines: HISTORY_LINES,
      });

      const viewer = await attach(client, 24, 80, "main");
      await triggerControl(client, viewer, { kind: "select", scope: "tab", entity: tab });
      await eventually(
        async () => (await processByEntity(client, pane.process))?.status.kind === "exited",
        20_000,
        "fixture build finished",
      );
      await triggerControl(client, viewer, { kind: "focus", pane: pane.paneView });

      state = { viewer, process: pane.process, paneView: pane.paneView, variant, diagnostic };

      const screen = await decodeViewer(client, viewer);
      const leaked = contentRows(screen).some((row) => row.includes(variant.code));
      if (leaked) {
        throw new Error("scenario setup is invalid: the diagnostic is already on screen");
      }

      const prompt = [
        `You are operating a running fux server. Your viewer entity id is ${viewer}.`,
        "",
        "A build program has already finished in the pane your viewer is focused on. Its output",
        "contains exactly one line that starts with `FAILURE `. That line has already scrolled out",
        "of the visible screen, but it is still in the pane's retained history.",
        "",
        "Find that line and report its error code. The code is the letter E, a hyphen, and four digits.",
        "",
        "Also leave the pane's view positioned so that the FAILURE line is visible in your viewer",
        "when you are done.",
        "",
        "Finish your reply with exactly one line, and nothing after it:",
        "CODE: E-NNNN",
        "(with the four real digits in place of NNNN; never answer with the literal N characters)",
      ].join("\n");

      return {
        prompt,
        baseline: {
          // Bumped when the task wording changes, so runs are never pooled
          // across different prompts by accident.
          promptRevision: "noisy/2 (placeholder is no longer answer-shaped)",
          viewer,
          process: pane.process,
          paneView: pane.paneView,
          historyLines: HISTORY_LINES,
          totalOutputLines: variant.totalLines + 1,
          failureLineNumber: variant.failureLine,
          linesBackFromBottom: variant.totalLines + 1 - variant.failureLine,
        },
      };
    },

    async verify(ctx: ScenarioContext) {
      const { client, journal } = ctx;
      const checks: Check[] = [];
      const notes: string[] = [];

      const answer = journal.finalAnswer ?? "";
      const codes = [...answer.matchAll(/CODE:\s*(E-\d+)/g)].map((match) => match[1]);
      const claimed = codes.at(-1) ?? null;
      checks.push({
        name: "agent reported the correct error code",
        ok: claimed === state.variant.code,
        detail: `claimed=${String(claimed)} expected=${state.variant.code}${codes.length > 1 ? ` (multiple CODE lines: ${codes.join(",")})` : ""}`,
      });

      const screen = await decodeViewer(client, state.viewer);
      const rows = contentRows(screen);
      const visible = rows.some((row) => row.includes(state.variant.code));
      checks.push({
        name: "diagnostic left visible in the viewer",
        ok: visible,
        detail: visible
          ? "the FAILURE line is in the viewer's content rows"
          : `not visible; scrollback=${(await viewerSnapshot(client, state.viewer)).scrollback}, last content rows: ${JSON.stringify(rows.filter((row) => row.length > 0).slice(-4))}`,
      });

      const mentionedInProse = answer.includes(state.variant.code);
      if (!mentionedInProse && claimed === null) {
        notes.push("the reply contained no CODE line and never mentioned the code");
      }
      const frameCalls = journal.toolCalls.filter((call) => call.method === "fux.frame").length;
      notes.push(`fux.frame calls: ${frameCalls}; total accepted calls: ${journal.sentCount}`);

      return summarize(
        checks,
        notes,
        {
          claimedCode: claimed,
          expectedCode: state.variant.code,
          viewer: await viewerSnapshot(client, state.viewer),
          contentRows: rows.filter((row) => row.length > 0),
          frameCalls,
        },
        // Finding the code is the task; leaving it framed is secondary.
        ["diagnostic left visible in the viewer"],
      );
    },
  };
}

export const noisyScenario: Scenario = {
  id: "noisy",
  title: "Noisy-output investigation",
  variants: ["a", "b"],
  historyLines: HISTORY_LINES,
  create,
};
