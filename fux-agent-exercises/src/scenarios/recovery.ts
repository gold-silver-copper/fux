/**
 * Scenario 5 — recovery and coexistence.
 *
 * A second viewer is attached with its own navigation state. Once the agent has
 * actually observed the pane it was asked to close, the harness closes that pane
 * from a separate operator viewer. The agent has to notice that its target is
 * gone, recover, and still finish the rest of the task without disturbing an
 * unrelated process or the observer's view.
 *
 * The disruption fires on an observable milestone — the target's entity id
 * appearing in a response the agent received — not on a timer.
 */
import type { BrpOutcome } from "../brp.ts";
import { attach, eventually, triggerControl } from "../brp.ts";
import type { ToolCallRecord } from "../journal.ts";
import {
  addProcessPane,
  addTab,
  paneViewOf,
  processByEntity,
  viewerSnapshot,
  workspaceByName,
} from "../world.ts";
import { type Check, type Scenario, type ScenarioContext, type ScenarioRun, type ScenarioSetup, summarize } from "./types.ts";

const TARGET = "target-a";
const KEEP = "target-b";
const BYSTANDER = "bystander";

interface State {
  agentViewer: number;
  observerViewer: number;
  operatorViewer: number;
  workTab: number;
  sideTab: number;
  targetProcess: number;
  targetPaneView: number;
  targetPid: number | undefined;
  keepProcess: number;
  keepPaneView: number;
  bystanderProcess: number;
  bystanderPid: number | undefined;
  observerBaseline: { viewing: number | null; onTab: number | null; focused: number | null };
}

interface Disruption {
  applied: boolean;
  at: string | null;
  triggeredByCallIndex: number | null;
  triggeringMethod: string | null;
  error: string | null;
  skippedReason: string | null;
}

function create(variant: string): ScenarioRun {
  let state: State;
  const disruption: Disruption = {
    applied: false,
    at: null,
    triggeredByCallIndex: null,
    triggeringMethod: null,
    error: null,
    skippedReason: null,
  };
  let firing = false;

  return {
    async setup(ctx: ScenarioContext): Promise<ScenarioSetup> {
      const { client } = ctx;
      const workspace = await workspaceByName(client, "main");
      if (workspace === undefined) throw new Error("initial workspace 'main' is missing");

      const workTab = await addTab(client, workspace, "work");
      const sideTab = await addTab(client, workspace, variant === "b" ? "scratch" : "side");

      const bystander = await addProcessPane(client, {
        tab: workTab,
        argv: ["/bin/sh", "-c", `echo ${BYSTANDER} ready; exec sleep 600`],
        cwd: ctx.server.workDir,
        name: BYSTANDER,
      });
      const keep = await addProcessPane(client, {
        tab: workTab,
        argv: ["/bin/sh", "-c", `echo ${KEEP} ready; exec sleep 600`],
        cwd: ctx.server.workDir,
        name: KEEP,
      });
      const target = await addProcessPane(client, {
        tab: sideTab,
        argv: ["/bin/sh", "-c", `echo ${TARGET} ready; exec sleep 600`],
        cwd: ctx.server.workDir,
        name: TARGET,
      });

      await eventually(
        async () => {
          const states = await Promise.all(
            [bystander.process, keep.process, target.process].map((entity) =>
              processByEntity(client, entity),
            ),
          );
          return states.every((process) => process?.status.kind === "running");
        },
        15_000,
        "all three fixture processes running",
      );

      // The observer's navigation state must survive the run untouched.
      const observerViewer = await attach(client, 24, 80, "main");
      await triggerControl(client, observerViewer, { kind: "select", scope: "tab", entity: workTab });
      await triggerControl(client, observerViewer, { kind: "focus", pane: bystander.paneView });

      // A separate viewer performs the disruption so the observer stays inert.
      const operatorViewer = await attach(client, 24, 80, "main");
      await triggerControl(client, operatorViewer, { kind: "select", scope: "tab", entity: sideTab });

      const agentViewer = await attach(client, 24, 80, "main");
      await triggerControl(client, agentViewer, { kind: "select", scope: "tab", entity: workTab });

      const observerBaseline = await viewerSnapshot(client, observerViewer);
      const targetSnapshot = await processByEntity(client, target.process);
      const bystanderSnapshot = await processByEntity(client, bystander.process);

      state = {
        agentViewer,
        observerViewer,
        operatorViewer,
        workTab,
        sideTab,
        targetProcess: target.process,
        targetPaneView: target.paneView,
        targetPid: targetSnapshot?.status.pid,
        keepProcess: keep.process,
        keepPaneView: keep.paneView,
        bystanderProcess: bystander.process,
        bystanderPid: bystanderSnapshot?.status.pid,
        observerBaseline: {
          viewing: observerBaseline.viewing,
          onTab: observerBaseline.onTab,
          focused: observerBaseline.focused,
        },
      };

      const prompt = [
        `You are operating a running fux server. Your viewer entity id is ${agentViewer}.`,
        "",
        "Do both of the following:",
        `1. Close the pane whose process is named \`${TARGET}\`, so that process is no longer running.`,
        `2. Leave the pane whose process is named \`${KEEP}\` focused in your viewer (${agentViewer}).`,
        "",
        "Constraints:",
        `- The process named \`${BYSTANDER}\` must keep running with the same pid.`,
        "- Other viewers are attached to this server. Do not change which workspace or tab they are",
        "  looking at, and do not change which pane they have focused.",
        "- The server may change underneath you while you work. If something you planned to act on is",
        "  already gone, confirm the actual state and carry on with the rest of the task.",
        "",
        "When you are finished, describe in one short paragraph what you did and anything unexpected.",
      ].join("\n");

      return {
        prompt,
        baseline: {
          agentViewer,
          observerViewer,
          operatorViewer,
          workTab,
          sideTab,
          targetProcess: target.process,
          targetPaneView: target.paneView,
          targetPid: state.targetPid,
          keepProcess: keep.process,
          keepPaneView: keep.paneView,
          bystanderProcess: bystander.process,
          bystanderPid: state.bystanderPid,
          observerBaseline: state.observerBaseline,
        },
      };
    },

    async onToolCall(ctx: ScenarioContext, record: ToolCallRecord, _outcome: BrpOutcome | null) {
      if (disruption.applied || disruption.skippedReason !== null || firing) return;
      const body = record.response ?? "";
      const mentionsTarget =
        body.includes(String(state.targetProcess)) || body.includes(String(state.targetPaneView));
      if (!mentionsTarget || record.rejected !== null) return;

      firing = true;
      try {
        const live = await paneViewOf(ctx.client, state.targetProcess);
        if (live === undefined) {
          disruption.skippedReason = "the target pane was already gone when the milestone fired";
          return;
        }
        // A real close from an operator viewer, not a raw hierarchy edit.
        await triggerControl(ctx.client, state.operatorViewer, {
          kind: "close",
          subject: { pane: live },
        });
        disruption.applied = true;
        disruption.at = new Date().toISOString();
        disruption.triggeredByCallIndex = record.index;
        disruption.triggeringMethod = record.method;
        ctx.journal.note(
          `disruption: closed ${TARGET} (pane view ${live}) after tool call ${record.index} (${record.method}) surfaced its entity id`,
        );
      } catch (error) {
        disruption.error = (error as Error).message;
        ctx.journal.note(`disruption failed: ${disruption.error}`);
      } finally {
        firing = false;
      }
    },

    async verify(ctx: ScenarioContext) {
      const { client } = ctx;
      const checks: Check[] = [];
      const notes: string[] = [];

      const target = await processByEntity(client, state.targetProcess);
      const targetView = await paneViewOf(client, state.targetProcess);
      const targetGone =
        target === undefined || target.status.kind === "exited" || target.status.kind === "failed";
      checks.push({
        name: "target process is no longer running",
        ok: targetGone,
        detail: `process ${state.targetProcess} status=${JSON.stringify(target?.status ?? "entity absent")}; pane view=${String(targetView)}`,
      });

      const bystander = await processByEntity(client, state.bystanderProcess);
      checks.push({
        name: "unrelated process survived",
        ok: bystander?.status.kind === "running" && bystander?.status.pid === state.bystanderPid,
        detail: `process ${state.bystanderProcess} status=${JSON.stringify(bystander?.status ?? null)}; expected running pid ${String(state.bystanderPid)}`,
      });

      const keep = await processByEntity(client, state.keepProcess);
      const keepView = await paneViewOf(client, state.keepProcess);
      const agent = await viewerSnapshot(client, state.agentViewer);
      checks.push({
        name: "agent viewer focuses the surviving pane",
        ok: keepView !== undefined && agent.focused === keepView,
        detail: `viewer ${state.agentViewer} Focused=${String(agent.focused)} OnTab=${String(agent.onTab)}; expected Focused=${String(keepView)}`,
      });
      checks.push({
        name: "the pane to keep is still running",
        ok: keep?.status.kind === "running",
        detail: `process ${state.keepProcess} status=${JSON.stringify(keep?.status ?? null)}`,
      });

      const observer = await viewerSnapshot(client, state.observerViewer);
      const unchanged =
        observer.viewing === state.observerBaseline.viewing &&
        observer.onTab === state.observerBaseline.onTab &&
        observer.focused === state.observerBaseline.focused;
      checks.push({
        name: "observer viewer navigation unchanged",
        ok: unchanged,
        detail: `observer ${state.observerViewer} now Viewing=${String(observer.viewing)} OnTab=${String(observer.onTab)} Focused=${String(observer.focused)}; baseline ${JSON.stringify(state.observerBaseline)}`,
      });

      if (!disruption.applied) {
        notes.push(
          `the controlled disruption did not fire (${disruption.skippedReason ?? disruption.error ?? "the target id never appeared in a response the agent received"}); this run did not test recovery`,
        );
      } else {
        notes.push(
          `disruption applied at ${String(disruption.at)} after tool call ${String(disruption.triggeredByCallIndex)} (${String(disruption.triggeringMethod)})`,
        );
      }
      const sawStaleTarget = ctx.journal.toolCalls.some((call) =>
        (call.response ?? "").includes("target no longer exists"),
      );
      notes.push(
        sawStaleTarget
          ? "the agent received a 'target no longer exists' notice at least once"
          : "the agent never saw a 'target no longer exists' notice",
      );
      if (agent.notice) notes.push(`agent viewer notice at verification: ${JSON.stringify(agent.notice)}`);

      return summarize(checks, notes, {
        disruption,
        target: target ?? null,
        bystander: bystander ?? null,
        keep: keep ?? null,
        agentViewer: agent,
        observerViewer: observer,
        operatorViewer: await viewerSnapshot(client, state.operatorViewer),
        sawStaleTargetNotice: sawStaleTarget,
      });
    },
  };
}

export const recoveryScenario: Scenario = {
  id: "recovery",
  title: "Recovery and coexistence",
  variants: ["a", "b"],
  create,
};
