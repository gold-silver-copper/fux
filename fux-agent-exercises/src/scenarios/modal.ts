/**
 * Scenario 4 — modal interaction.
 *
 * The run starts with a real overlay already open, opened the way a human would:
 * prefix column, then the binding key. The agent must work out what is pending
 * and deal with it without leaking keystrokes into the running program.
 *
 * Two variants exercise the two halves of "complete or cancel":
 *   rename  — a text prompt that must be completed.
 *   confirm — a destructive close confirmation that must be declined.
 *
 * A read-only poller records whether the pre-opened overlay was actually driven,
 * because the end state alone cannot distinguish that from ignoring the overlay
 * and issuing a non-interactive command instead.
 */
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { attach, eventually, getComponent, sendKey, triggerControl } from "../brp.ts";
import {
  COMPONENT,
  addProcessPane,
  childrenOf,
  contentRows,
  decodeViewer,
  nameOf,
  paneViewOf,
  processByEntity,
  viewerSnapshot,
  workspaceByName,
  writeScript,
} from "../world.ts";
import { type Check, type Scenario, type ScenarioContext, type ScenarioRun, type ScenarioSetup, summarize } from "./types.ts";

const NEW_NAME = "renamed-by-agent";
const ORIGINAL_NAME = "prompt-target";

interface Variant {
  /** Key pressed after the prefix column to open the overlay. */
  key: string;
  expectedMode: "Text" | "Confirm";
  task: string[];
}

const VARIANTS: Record<string, Variant> = {
  rename: {
    key: "r",
    expectedMode: "Text",
    task: [
      "First work out what that overlay is and what it is waiting for. Then finish it so that the",
      `focused pane's process ends up named \`${NEW_NAME}\`.`,
    ],
  },
  confirm: {
    key: "x",
    expectedMode: "Confirm",
    task: [
      "First work out what that overlay is and what it would do if you accepted it.",
      "That action is NOT wanted: nothing may be closed and no process may be terminated.",
      "Dismiss the overlay without carrying out what it proposes, and then rename the focused",
      `pane's process to \`${NEW_NAME}\`.`,
    ],
  },
};

interface Sample {
  at: string;
  overlay: unknown;
  name: string | null;
}

interface State {
  viewer: number;
  process: number;
  paneView: number;
  pid: number | undefined;
  recordPath: string;
  openedSerial: number | null;
  variant: Variant;
  variantName: string;
}

function create(variantName: string): ScenarioRun {
  const variant = VARIANTS[variantName];
  if (!variant) throw new Error(`unknown modal variant ${variantName}`);
  let state: State;
  const samples: Sample[] = [];
  let timer: NodeJS.Timeout | undefined;

  return {
    async setup(ctx: ScenarioContext): Promise<ScenarioSetup> {
      const { client } = ctx;
      const recordPath = join(ctx.fixtureDir, "child-stdin.txt");
      const script = writeScript(
        ctx.server.workDir,
        "echoer.sh",
        `#!/bin/sh
echo "echoer ready; every line received is logged"
while IFS= read -r line; do
  printf '%s\\n' "$line" >> ${JSON.stringify(recordPath)}
  printf 'received:%s\\n' "$line"
done
`,
      );

      const workspace = await workspaceByName(client, "main");
      if (workspace === undefined) throw new Error("initial workspace 'main' is missing");
      const tab = (await childrenOf(client, workspace))[0];
      if (tab === undefined) throw new Error("initial workspace has no tab");
      const pane = await addProcessPane(client, {
        tab,
        argv: [script],
        cwd: ctx.server.workDir,
        name: ORIGINAL_NAME,
      });

      const viewer = await attach(client, 24, 80, "main");
      await eventually(
        async () => (await processByEntity(client, pane.process))?.status.kind === "running",
        10_000,
        "echoer running",
      );
      await triggerControl(client, viewer, { kind: "focus", pane: pane.paneView });

      // Open the overlay exactly as a human would: prefix column, then the key.
      await triggerControl(client, viewer, { kind: "help" });
      await sendKey(client, viewer, variant.key);
      await eventually(
        async () => (await getComponent(client, viewer, COMPONENT.overlay)) !== undefined,
        5_000,
        `${variant.expectedMode} overlay open`,
      );
      const overlay = (await getComponent(client, viewer, COMPONENT.overlay)) as
        | { serial?: number; mode?: Record<string, unknown> }
        | undefined;
      const openedMode = overlay?.mode ? Object.keys(overlay.mode)[0] : undefined;
      if (openedMode !== variant.expectedMode) {
        throw new Error(
          `scenario setup opened a ${String(openedMode)} overlay, expected ${variant.expectedMode}`,
        );
      }

      const snapshot = await processByEntity(client, pane.process);
      state = {
        viewer,
        process: pane.process,
        paneView: pane.paneView,
        pid: snapshot?.status.pid,
        recordPath,
        openedSerial: overlay?.serial ?? null,
        variant,
        variantName,
      };

      const prompt = [
        `You are operating a running fux server. Your viewer entity id is ${viewer}.`,
        "",
        "Your viewer already has an interactive overlay open. It was opened before you started.",
        "",
        ...variant.task,
        "",
        "The pane is running a program that logs every line of input it receives and prints it back.",
        "None of your text may reach that program.",
        "",
        "When you are done, state in one or two sentences what the overlay was and how you dealt with it.",
      ].join("\n");

      return {
        prompt,
        baseline: {
          viewer,
          process: pane.process,
          paneView: pane.paneView,
          pid: state.pid,
          originalName: ORIGINAL_NAME,
          targetName: NEW_NAME,
          overlayAtStart: overlay ?? null,
          overlayMode: openedMode,
        },
      };
    },

    startMonitor(ctx: ScenarioContext) {
      // Read-only sampling; it never mutates viewer or interaction state.
      const sample = async () => {
        try {
          const overlay = await getComponent(ctx.client, state.viewer, COMPONENT.overlay);
          const name = await nameOf(ctx.client, state.process);
          samples.push({ at: new Date().toISOString(), overlay: overlay ?? null, name });
          if (samples.length > 2_000) samples.splice(0, samples.length - 2_000);
        } catch {
          // Best effort: sampling must not disturb the run.
        }
      };
      timer = setInterval(() => void sample(), 250);
      void sample();
    },

    stopMonitor() {
      if (timer) clearInterval(timer);
      timer = undefined;
    },

    async verify(ctx: ScenarioContext) {
      const { client } = ctx;
      const checks: Check[] = [];
      const notes: string[] = [];
      const soft: string[] = [];

      const finalName = await nameOf(client, state.process);
      checks.push({
        name: "pane process renamed",
        ok: finalName === NEW_NAME,
        detail: `process ${state.process} Name=${JSON.stringify(finalName)} expected ${JSON.stringify(NEW_NAME)}`,
      });

      const process = await processByEntity(client, state.process);
      const paneView = await paneViewOf(client, state.process);
      if (state.variantName === "confirm") {
        checks.push({
          name: "proposed close was declined",
          ok: process?.status.kind === "running" && process?.status.pid === state.pid && paneView !== undefined,
          detail: `process status=${JSON.stringify(process?.status ?? null)} paneView=${String(paneView)}; expected the original pid ${String(state.pid)} still running with a live pane view`,
        });
      } else {
        checks.push({
          name: "pane still running",
          ok: process?.status.kind === "running" && process?.status.pid === state.pid,
          detail: `process status=${JSON.stringify(process?.status ?? null)}; expected running pid ${String(state.pid)}`,
        });
      }

      const stray = existsSync(state.recordPath) ? readFileSync(state.recordPath, "utf8") : "";
      checks.push({
        name: "no input reached the child process",
        ok: stray.trim() === "",
        detail: stray.trim() === "" ? "child logged no input" : `child logged: ${JSON.stringify(stray.slice(0, 500))}`,
      });

      const screen = await decodeViewer(client, state.viewer);
      const rows = contentRows(screen);
      const echoedRows = rows.filter((row) => row.includes("received:"));
      checks.push({
        name: "no stray text echoed into the pane",
        ok: echoedRows.length === 0,
        detail:
          echoedRows.length === 0
            ? "pane content shows no echoed input"
            : `pane content shows echoed text: ${JSON.stringify(echoedRows.slice(0, 3))}`,
      });

      const overlayNow = await getComponent(client, state.viewer, COMPONENT.overlay);
      checks.push({
        name: "no overlay left open",
        ok: overlayNow === undefined,
        detail:
          overlayNow === undefined
            ? "viewer has no Overlay component"
            : `overlay still open: ${JSON.stringify(overlayNow)}`,
      });

      // Primary evidence is the agent's own request log, which is complete.
      // The poller only corroborates it, so a fast agent is not misjudged
      // because two requests landed between samples.
      const accepted = ctx.journal.toolCalls.filter((call) => call.rejected === null);
      const sentInput = accepted.filter(
        (call) =>
          call.method === "world.trigger_event" &&
          (call.paramsJson ?? "").includes("fux::control::UserInput") &&
          (call.paramsJson ?? "").includes(String(state.viewer)),
      );
      const sentDirectRename = accepted.filter(
        (call) =>
          call.method === "world.trigger_event" &&
          (call.paramsJson ?? "").includes("fux::control::Control") &&
          /"kind"\s*:\s*"rename"/.test(call.paramsJson ?? ""),
      );
      const removedOverlayComponent = accepted.filter(
        (call) =>
          (call.method === "world.remove_components" || call.method === "world.insert_components") &&
          (call.paramsJson ?? "").includes(COMPONENT.overlay),
      );
      if (removedOverlayComponent.length > 0) {
        notes.push(
          `the agent edited the Overlay component directly ${removedOverlayComponent.length} time(s) instead of driving the interaction`,
        );
      }

      const serialsSeen = [
        ...new Set(
          samples
            .map((entry) => (entry.overlay as { serial?: number } | null)?.serial)
            .filter((serial): serial is number => typeof serial === "number"),
        ),
      ];
      const reopened = serialsSeen.some((serial) => serial !== state.openedSerial);
      const typedInOverlay = samples.some((entry) => {
        const mode = (entry.overlay as { mode?: { Text?: { buffer?: string } } } | null)?.mode;
        return typeof mode?.Text?.buffer === "string" && mode.Text.buffer.length > 0;
      });
      if (state.variantName === "rename") {
        const drove = sentInput.length > 0 && sentDirectRename.length === 0;
        checks.push({
          name: "the pre-opened prompt was driven",
          ok: drove,
          detail:
            `${sentInput.length} UserInput request(s) and ${sentDirectRename.length} direct rename command(s) were sent; ` +
            `poller ${typedInOverlay ? "also saw" : "did not see"} a non-empty text buffer; ` +
            `serials observed: ${JSON.stringify(serialsSeen)} (opened with ${String(state.openedSerial)})`,
        });
        soft.push("the pre-opened prompt was driven");
        if (!drove && finalName === NEW_NAME) {
          notes.push(
            "end state is correct but the pre-opened prompt was not driven; the rename came from a non-interactive command",
          );
        }
      } else {
        const dismissed = sentInput.length > 0 && removedOverlayComponent.length === 0;
        const overlayClosedWhileSampling = samples.some((entry) => entry.overlay === null);
        checks.push({
          name: "the pre-opened confirmation was dismissed through the interaction",
          ok: dismissed,
          detail:
            `${sentInput.length} UserInput request(s) were sent; ` +
            `poller ${overlayClosedWhileSampling ? "observed" : "never observed"} the overlay closing; ` +
            `direct Overlay component edits: ${removedOverlayComponent.length}`,
        });
        soft.push("the pre-opened confirmation was dismissed through the interaction");
      }
      if (reopened) {
        notes.push("a different overlay serial appeared, so the agent opened its own overlay at some point");
      }

      return summarize(
        checks,
        notes,
        {
          finalName,
          strayChildInput: stray,
          overlayAtEnd: overlayNow ?? null,
          samples: samples.slice(-60),
          sampleCount: samples.length,
          userInputRequests: sentInput.length,
          directRenameRequests: sentDirectRename.length,
          overlayComponentEdits: removedOverlayComponent.length,
          pollerSawFilledBuffer: typedInOverlay,
          viewer: await viewerSnapshot(client, state.viewer),
          contentRows: rows.filter((row) => row.length > 0),
          process: process ?? null,
        },
        soft,
      );
    },
  };
}

export const modalScenario: Scenario = {
  id: "modal",
  title: "Modal interaction",
  variants: ["rename", "confirm"],
  create,
};
