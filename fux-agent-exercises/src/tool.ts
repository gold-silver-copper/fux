/**
 * The single tool the exercised agent gets.
 *
 * It forwards one JSON-RPC request to this run's fux server and returns the raw
 * response. It deliberately does not discover entities, translate intent into
 * request sequences, repair malformed JSON, decode ANSI frames, or wait for a
 * command to finish. Those are exactly the costs the exercises are measuring.
 */
import type { BrpClient, BrpOutcome } from "./brp.ts";
import { MODEL_VISIBLE_LIMIT, ARTIFACT_LIMIT, RunJournal, type ToolCallRecord, truncate } from "./journal.ts";
import type { PiSdk } from "./pi-sdk.ts";

export const TOOL_NAME = "fux_rpc";

export interface ToolBudget {
  maxToolCalls: number;
  requestTimeoutMs: number;
}

export interface ToolHooks {
  /** Called after every completed call, for milestone-triggered scenario events. */
  onCall?: (record: ToolCallRecord, outcome: BrpOutcome | null) => void | Promise<void>;
  /** Called when a budget is exhausted so the runner can stop the session. */
  onBudgetExhausted?: (reason: string) => void;
}

export interface FuxRpcTool {
  definition: unknown;
  /** Number of accepted calls so far, including ones the server rejected. */
  readonly used: number;
}

export function createFuxRpcTool(
  sdk: PiSdk,
  client: BrpClient,
  journal: RunJournal,
  budget: ToolBudget,
  hooks: ToolHooks = {},
): FuxRpcTool {
  const { Type } = sdk;
  let used = 0;

  const definition = sdk.defineTool({
    name: TOOL_NAME,
    label: "fux rpc",
    description:
      "Send exactly one JSON-RPC request to this session's fux server and return its raw response. " +
      "`method` is a method name such as rpc.discover, registry.schema, world.query, world.get_components, " +
      "world.spawn_entity, world.trigger_event, fux.attach or fux.frame. `params_json` is the request's " +
      "params encoded as a JSON string, exactly as the fux README shows after `fux rpc METHOD`. " +
      "Omit params_json for methods that take none. The response is returned verbatim and is not " +
      "interpreted, decoded or retried for you.",
    parameters: Type.Object({
      method: Type.String({ description: "JSON-RPC method name." }),
      params_json: Type.Optional(
        Type.String({ description: "Request params as a JSON string, or omitted when there are none." }),
      ),
    }),
    execute: async (_toolCallId: string, params: { method: string; params_json?: string }) => {
      const index = journal.toolCalls.length;
      const startedAt = new Date().toISOString();
      const started = performance.now();

      const finish = async (
        record: Omit<ToolCallRecord, "index" | "startedAt" | "finishedAt" | "durationMs">,
        outcome: BrpOutcome | null,
        visible: string,
        isError: boolean,
      ) => {
        const full: ToolCallRecord = {
          index,
          startedAt,
          finishedAt: new Date().toISOString(),
          durationMs: performance.now() - started,
          ...record,
        };
        journal.toolCalls.push(full);
        // Awaited so a scenario's milestone disruption is complete before the
        // agent reads the response that triggered it. That ordering is the
        // point: the agent observed the target, then the world moved.
        try {
          await hooks.onCall?.(full, outcome);
        } catch (error) {
          journal.note(`scenario hook failed on call ${index}: ${(error as Error).message}`);
        }
        return {
          content: [{ type: "text", text: visible }],
          details: {},
          isError,
        };
      };

      if (used >= budget.maxToolCalls) {
        const reason = `tool-call budget of ${budget.maxToolCalls} exhausted`;
        hooks.onBudgetExhausted?.(reason);
        return await finish(
          {
            method: params.method,
            paramsJson: params.params_json ?? null,
            rejected: reason,
            httpStatus: null,
            transportError: null,
            jsonRpcError: null,
            modelVisible: { applied: false, originalBytes: 0, keptBytes: 0 },
            artifact: { applied: false, originalBytes: 0, keptBytes: 0 },
            response: null,
          },
          null,
          `HARNESS: ${reason}. No request was sent. Stop and report what you have established so far.`,
          true,
        );
      }

      let parsed: unknown;
      if (params.params_json !== undefined && params.params_json.trim() !== "") {
        try {
          parsed = JSON.parse(params.params_json);
        } catch (error) {
          // Reporting the parse failure is not the same as repairing it.
          return await finish(
            {
              method: params.method,
              paramsJson: params.params_json,
              rejected: "params_json was not valid JSON",
              httpStatus: null,
              transportError: null,
              jsonRpcError: null,
              modelVisible: { applied: false, originalBytes: 0, keptBytes: 0 },
              artifact: { applied: false, originalBytes: 0, keptBytes: 0 },
              response: null,
            },
            null,
            `HARNESS: params_json is not valid JSON (${(error as Error).message}). No request was sent.`,
            true,
          );
        }
      }

      used += 1;
      const outcome = await client.call(params.method, parsed, budget.requestTimeoutMs);
      const body = outcome.body ?? "";
      const visibleSource = outcome.transportError
        ? `HARNESS: transport error contacting the fux server: ${outcome.transportError}`
        : body;
      const visible = truncate(visibleSource, MODEL_VISIBLE_LIMIT);
      const artifact = truncate(body, ARTIFACT_LIMIT);

      return await finish(
        {
          method: params.method,
          paramsJson: params.params_json ?? null,
          rejected: null,
          httpStatus: outcome.httpStatus,
          transportError: outcome.transportError,
          jsonRpcError: outcome.error ?? null,
          modelVisible: visible.truncation,
          artifact: artifact.truncation,
          response: artifact.text,
        },
        outcome,
        visible.text,
        Boolean(outcome.transportError),
      );
    },
  });

  return {
    definition,
    get used() {
      return used;
    },
  };
}
