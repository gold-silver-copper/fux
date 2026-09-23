/**
 * The per-finding metrics, checked against real artifacts under
 * `tests/fixtures/`. The expected numbers are the ones FINDINGS.md quotes for
 * those runs, so a drift here is a drift in the written record.
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import {
  hierarchyMetrics,
  resourceEntityMetrics,
  noisyMetrics,
  partialPayloads,
  recoveryMetrics,
  renderFindings,
  verificationBeforeActing,
  type RunArtifact,
} from "../src/findings.ts";
import type { ToolCallRecord } from "../src/journal.ts";

const FIXTURES = join(import.meta.dirname, "fixtures");

function fixture(name: string): RunArtifact {
  return JSON.parse(readFileSync(join(FIXTURES, `${name}.run.json`), "utf8")) as RunArtifact;
}

function call(index: number, method: string, params: unknown, response = "{}"): ToolCallRecord {
  return {
    index,
    startedAt: "",
    finishedAt: "",
    durationMs: 0,
    method,
    paramsJson: params === undefined ? null : JSON.stringify(params),
    rejected: null,
    httpStatus: 200,
    transportError: null,
    jsonRpcError: null,
    modelVisible: { applied: false, originalBytes: 0, keptBytes: 0 },
    artifact: { applied: false, originalBytes: 0, keptBytes: 0 },
    response,
  };
}

const control = (viewer: number, command: unknown) => ({
  event: "fux::control::Control",
  value: { viewer, command },
});

test("noisy metrics reproduce the campaign 04 numbers for noisy-a-r1", () => {
  const metrics = noisyMetrics(fixture("campaign-04-noisy-a-r1"));
  assert.ok(metrics);
  assert.deepEqual(metrics, {
    scrollAttempts: 1,
    scrollRejections: 0,
    firstScrollAccepted: true,
    directScrollbackWrites: 3,
    frameCalls: 5,
    acceptedCalls: 11,
    finalOffset: 130,
    linesBackFromBottom: 139,
    outcome: "pass",
    claimedCode: "E-4417",
    expectedCode: "E-4417",
    claimSeenInAnyResponse: true,
  });
});

test("a fabricated code is one that appears in no response the run received (F2)", () => {
  const metrics = noisyMetrics(fixture("campaign-02-noisy-a-r1"));
  assert.ok(metrics);
  assert.equal(metrics.claimedCode, "E-9412");
  assert.equal(metrics.expectedCode, "E-4417");
  assert.equal(metrics.claimSeenInAnyResponse, false);
  assert.equal(metrics.outcome, "fail");
});

test("noisy metrics count the rejected scroll guess and the direct write in campaign 01", () => {
  const metrics = noisyMetrics(fixture("campaign-01-noisy-b-r2"));
  assert.ok(metrics);
  assert.equal(metrics.scrollAttempts, 1);
  assert.equal(metrics.scrollRejections, 1);
  assert.equal(metrics.firstScrollAccepted, false);
  assert.equal(metrics.directScrollbackWrites, 1);
});

test("hierarchy metrics find the wrong guess and the list_components detour", () => {
  assert.deepEqual(hierarchyMetrics(fixture("campaign-03-discovery-b-r2")), {
    listComponentsCalls: [16],
    wrongPathGuesses: [15],
    firstCorrectHierarchyUse: 17,
  });
  assert.deepEqual(hierarchyMetrics(fixture("campaign-04-noisy-a-r1")), {
    listComponentsCalls: [],
    wrongPathGuesses: [],
    firstCorrectHierarchyUse: null,
  });
});

test("partial payloads report the F1 request, its omitted optional field and the dead server", () => {
  const found = partialPayloads(fixture("campaign-01-noisy-b-r2"));
  assert.deepEqual(found, [
    {
      index: 20,
      method: "world.insert_components",
      component: "fux::model::Viewer",
      missing: ["notice"],
      missingRequired: [],
      answer: "transport error: fetch failed",
    },
  ]);
  assert.deepEqual(partialPayloads(fixture("campaign-04-noisy-a-r1")), []);
});

test("recovery metrics reproduce campaign 04: fired on the close, not verified before the next command", () => {
  const metrics = recoveryMetrics(fixture("campaign-04-recovery-a-r1"));
  assert.ok(metrics);
  assert.equal(metrics.applied, true);
  assert.equal(metrics.triggeredByCallIndex, 6);
  assert.equal(metrics.triggeringMethod, "world.trigger_event");
  assert.equal(metrics.sawStaleTargetNotice, false);
  assert.deepEqual(metrics.verification, { verified: false, verificationRequest: null, nextControlIndex: 7 });
  assert.equal(recoveryMetrics(fixture("campaign-04-noisy-a-r1")), null);
});

test("verification before acting again distinguishes looking from acting", () => {
  const viewer = 42;
  assert.deepEqual(verificationBeforeActing([], null, viewer), {
    verified: null,
    verificationRequest: null,
    nextControlIndex: null,
  });
  const close = call(3, "world.trigger_event", control(viewer, { kind: "close", subject: { pane: 7 } }));
  const focus = call(4, "world.trigger_event", control(viewer, { kind: "focus", pane: 9 }));
  const ownFrame = call(4, "fux.frame", { viewer });
  const otherFrame = call(4, "fux.frame", { viewer: 43 });
  const viewerQuery = call(4, "world.query", { data: { components: ["fux::model::Viewer"] } });
  const nameQuery = call(4, "world.query", { data: { components: ["bevy_ecs::name::Name"] } });

  assert.deepEqual(verificationBeforeActing([close, focus], 3, viewer), {
    verified: false,
    verificationRequest: null,
    nextControlIndex: 4,
  });
  assert.deepEqual(verificationBeforeActing([close, ownFrame, { ...focus, index: 5 }], 3, viewer), {
    verified: true,
    verificationRequest: { index: 4, method: "fux.frame" },
    nextControlIndex: null,
  });
  assert.equal(verificationBeforeActing([close, otherFrame, { ...focus, index: 5 }], 3, viewer).verified, false);
  assert.equal(verificationBeforeActing([close, viewerQuery, { ...focus, index: 5 }], 3, viewer).verified, true);
  assert.equal(verificationBeforeActing([close, nameQuery, { ...focus, index: 5 }], 3, viewer).verified, false);
  // Never acting again and never looking is not verifying either.
  assert.equal(verificationBeforeActing([close, nameQuery], 3, viewer).verified, false);
  // A recorded field takes precedence over recomputation when the artifact has one.
  const recorded = fixture("campaign-04-recovery-a-r1");
  recorded.verification!.evidence.verifiedBeforeActingAgain = { verified: true, verificationRequest: null, nextControlIndex: null };
  assert.equal(recoveryMetrics(recorded)?.verification.verified, true);
});

test("the findings section renders every table from the fixtures", () => {
  const text = renderFindings([
    fixture("campaign-04-noisy-a-r1"),
    fixture("campaign-04-recovery-a-r1"),
    fixture("campaign-03-discovery-b-r2"),
    fixture("campaign-01-noisy-b-r2"),
    fixture("campaign-02-noisy-a-r1"),
  ]).join("\n");
  assert.match(text, /^## Findings$/m);
  assert.match(text, /\| `noisy-a-r1` \| pass \| 139 \| 1 \| 0 \| yes \| 3 \| 5 \| 11 \| 130 \| E-4417 \| E-4417 \| yes \|/);
  assert.match(text, /\| `noisy-a-r1` \| fail \| 139 \| .* \| E-9412 \| E-4417 \| no \|/);
  // campaign-01 noisy-b-r2 died before answering, so it is not among the answered.
  assert.match(text, /fabricated\): 1 of 2 answered\./);
  assert.match(text, /\| `noisy-b-r2` \| — \| 131 \| 1 \| 1 \| no \| 1 \| 8 \| 23 \| — \| — \| — \| — \|/);
  assert.match(text, /\| `discovery-b-r2` \| 16 \| 15 \| 17 \|/);
  assert.match(text, /Runs with a `list_components` call: 1 of 5\. Runs with a wrong hierarchy path guess: 1 of 5\./);
  assert.match(text, /\| `noisy-b-r2` \| 20 \| world\.insert_components \| `fux::model::Viewer` \| notice \| none \| transport error: fetch failed \|/);
  assert.match(text, /\| `recovery-a-r1` \| yes \| 6 \| world\.trigger_event \| no \| no \| — \| 7 \|/);
  assert.match(text, /Agents that read state before acting again: 0 of 1\./);
});

test("resource entity metrics count what Bevy 0.20 made visible", () => {
  // Campaign 05 predates the move to Bevy 0.20, so its agents met resource
  // entities only where a component listing named one, and never queried
  // unfiltered. These numbers are the before side of campaign 06's comparison.
  const noisy = fixture("campaign-04-noisy-a-r1");
  const metrics = resourceEntityMetrics(noisy);
  assert.deepEqual(metrics.unfilteredQueries, []);
  assert.deepEqual(metrics.requestsNamingResourceEntity, []);

  // A synthetic run: one unfiltered query, one response carrying IsResource,
  // and one request naming an id in the resource range.
  const synthetic = {
    ...noisy,
    journal: {
      ...noisy.journal,
      toolCalls: [
        { ...noisy.journal.toolCalls[0], index: 0, method: "world.query", paramsJson: JSON.stringify({ data: {} }), response: "{}" },
        {
          ...noisy.journal.toolCalls[0],
          index: 1,
          method: "world.list_components",
          paramsJson: JSON.stringify({ entity: 4294967295 }),
          response: JSON.stringify({ result: ["bevy_ecs::resource::IsResource"] }),
        },
        {
          ...noisy.journal.toolCalls[0],
          index: 2,
          method: "world.query",
          paramsJson: JSON.stringify({ data: { components: ["fux::model::Viewer"] } }),
          response: "{}",
        },
      ],
    },
  };
  const found = resourceEntityMetrics(synthetic as typeof noisy);
  assert.deepEqual(found.unfilteredQueries, [0]);
  assert.deepEqual(found.responsesShowingResourceEntities, [1]);
  assert.deepEqual(found.requestsNamingResourceEntity, [1]);

  const text = renderFindings([synthetic as typeof noisy]).join("\n");
  assert.match(text, /resource entities in what the agent sees/);
  assert.match(text, /Runs that sent a request naming one: 1 of 1/);
});
