/**
 * Scenario 1 — discovery and organization.
 *
 * Several named workspaces, tabs and processes exist. The agent must find one
 * specific process, move its pane to another workspace, rename it and focus it,
 * without disturbing an unrelated process.
 */
import { attach, eventually } from "../brp.ts";
import {
  addProcessPane,
  addTab,
  addWorkspace,
  childrenOf,
  nameOf,
  paneViewOf,
  processByEntity,
  resolveTabAndWorkspace,
  viewerSnapshot,
  workspaceByName,
} from "../world.ts";
import { type Scenario, type ScenarioContext, type ScenarioRun, type ScenarioSetup, type Check, summarize } from "./types.ts";

interface State {
  viewer: number;
  targetProcess: number;
  targetPid: number | undefined;
  bystanderProcess: number;
  bystanderPid: number | undefined;
  destinationWorkspace: number;
  destinationTab: number;
  originWorkspace: number;
}

const DESTINATION = "beta";
const TARGET = "audit-target";
const BYSTANDER = "keepalive";
const NEW_NAME = "audited";

function create(variant: string): ScenarioRun {
  let state: State;

  return {
    async setup(ctx: ScenarioContext): Promise<ScenarioSetup> {
      const { client } = ctx;
      const origin = await workspaceByName(client, "main");
      if (origin === undefined) throw new Error("initial workspace 'main' is missing");
      const originTab = (await childrenOf(client, origin))[0];
      if (originTab === undefined) throw new Error("initial workspace has no tab");

      // A second workspace the agent must move the pane into.
      const destination = await addWorkspace(client, DESTINATION);
      const destinationTab = await addTab(client, destination, variant === "b" ? "logs" : "inbox");

      // Two long-lived processes with distinct names in the origin tab.
      const target = await addProcessPane(client, {
        tab: originTab,
        argv: ["/bin/sh", "-c", `echo ${TARGET} ready; exec sleep 600`],
        cwd: ctx.server.workDir,
        name: TARGET,
      });
      const bystander = await addProcessPane(client, {
        tab: originTab,
        argv: ["/bin/sh", "-c", `echo ${BYSTANDER} ready; exec sleep 600`],
        cwd: ctx.server.workDir,
        name: BYSTANDER,
      });

      const viewer = await attach(client, 24, 80, "main");
      await eventually(
        async () => {
          const target_ = await processByEntity(client, target.process);
          const bystander_ = await processByEntity(client, bystander.process);
          return target_?.status.kind === "running" && bystander_?.status.kind === "running";
        },
        10_000,
        "fixture processes running",
      );

      const targetSnapshot = await processByEntity(client, target.process);
      const bystanderSnapshot = await processByEntity(client, bystander.process);
      state = {
        viewer,
        targetProcess: target.process,
        targetPid: targetSnapshot?.status.pid,
        bystanderProcess: bystander.process,
        bystanderPid: bystanderSnapshot?.status.pid,
        destinationWorkspace: destination,
        destinationTab,
        originWorkspace: origin,
      };

      const prompt = [
        `You are operating a running fux server. Your viewer entity id is ${viewer}.`,
        "",
        `This server has more than one workspace. One of the running processes is named \`${TARGET}\`.`,
        "",
        "Do all of the following:",
        `1. Find the pane that shows the process named \`${TARGET}\`.`,
        `2. Move that pane into the workspace named \`${DESTINATION}\`.`,
        `3. Rename that pane to \`${NEW_NAME}\`.`,
        `4. Leave it focused in your viewer (${viewer}), with your viewer looking at \`${DESTINATION}\`.`,
        "",
        `Do not terminate, close or restart any process. The process named \`${BYSTANDER}\` must keep running, keep its pid, and stay where it is.`,
        "",
        "When you are finished, describe in one short paragraph what you changed.",
      ].join("\n");

      return {
        prompt,
        baseline: {
          viewer,
          targetProcess: target.process,
          targetPaneView: target.paneView,
          targetPid: state.targetPid,
          bystanderProcess: bystander.process,
          bystanderPid: state.bystanderPid,
          originWorkspace: origin,
          originTab,
          destinationWorkspace: destination,
          destinationTab,
        },
      };
    },

    async verify(ctx: ScenarioContext) {
      const { client } = ctx;
      const checks: Check[] = [];
      const notes: string[] = [];

      const target = await processByEntity(client, state.targetProcess);
      const bystander = await processByEntity(client, state.bystanderProcess);
      const targetView = await paneViewOf(client, state.targetProcess);
      const viewer = await viewerSnapshot(client, state.viewer);

      checks.push({
        name: "target process identity preserved",
        ok: target?.status.kind === "running" && target?.status.pid === state.targetPid,
        detail: `entity ${state.targetProcess} status=${JSON.stringify(target?.status ?? null)} expected running pid=${state.targetPid}`,
      });

      const placement =
        targetView === undefined
          ? { tab: null, workspace: null }
          : await resolveTabAndWorkspace(client, targetView);
      checks.push({
        name: "pane moved into destination workspace",
        ok: placement.workspace === state.destinationWorkspace,
        detail: `pane view ${String(targetView)} resolves to workspace ${String(placement.workspace)} (tab ${String(placement.tab)}); expected workspace ${state.destinationWorkspace}`,
      });

      const renamed = await nameOf(client, state.targetProcess);
      checks.push({
        name: "pane renamed",
        ok: renamed === NEW_NAME,
        detail: `process ${state.targetProcess} Name=${JSON.stringify(renamed)} expected ${JSON.stringify(NEW_NAME)}`,
      });

      checks.push({
        name: "viewer focuses the moved pane",
        ok: targetView !== undefined && viewer.focused === targetView,
        detail: `viewer ${state.viewer} Focused=${String(viewer.focused)} Viewing=${String(viewer.viewing)}; expected Focused=${String(targetView)} Viewing=${state.destinationWorkspace}`,
      });
      checks.push({
        name: "viewer looks at destination workspace",
        ok: viewer.viewing === state.destinationWorkspace,
        detail: `viewer ${state.viewer} Viewing=${String(viewer.viewing)} expected ${state.destinationWorkspace}`,
      });

      checks.push({
        name: "unrelated process untouched",
        ok: bystander?.status.kind === "running" && bystander?.status.pid === state.bystanderPid,
        detail: `entity ${state.bystanderProcess} status=${JSON.stringify(bystander?.status ?? null)} expected running pid=${state.bystanderPid}`,
      });

      const bystanderView = await paneViewOf(client, state.bystanderProcess);
      const bystanderPlacement =
        bystanderView === undefined
          ? { tab: null, workspace: null }
          : await resolveTabAndWorkspace(client, bystanderView);
      checks.push({
        name: "unrelated pane stayed in its workspace",
        ok: bystanderPlacement.workspace === state.originWorkspace,
        detail: `bystander pane view ${String(bystanderView)} is under workspace ${String(bystanderPlacement.workspace)}; expected ${state.originWorkspace}`,
      });

      if (viewer.notice) notes.push(`viewer notice at verification: ${JSON.stringify(viewer.notice)}`);

      return summarize(checks, notes, {
        targetProcess: await processByEntity(client, state.targetProcess),
        bystanderProcess: bystander ?? null,
        viewer,
        targetPaneView: targetView ?? null,
        targetPlacement: placement,
      });
    },
  };
}

export const discoveryScenario: Scenario = {
  id: "discovery",
  title: "Discovery and organization",
  variants: ["a", "b"],
  create,
};
