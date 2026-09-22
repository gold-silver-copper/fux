/**
 * Loads every module and asserts the scenario set is well formed.
 *
 * Node strips types at runtime instead of typechecking, so this keeps a syntax
 * or import error from hiding in a file the other tests do not reach.
 */
import assert from "node:assert/strict";
import { readdirSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import { SCENARIOS, scenarioById } from "../src/scenarios/index.ts";

test("every source module loads", async () => {
  const root = join(import.meta.dirname, "..", "src");
  const files: string[] = [];
  const walk = (directory: string) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (entry.name.endsWith(".ts")) files.push(path);
    }
  };
  walk(root);
  assert.ok(files.length >= 10, `expected the harness modules, found ${files.length}`);
  for (const file of files) {
    await import(file);
  }
});

test("the five exercises are registered with distinct ids and variants", () => {
  assert.equal(SCENARIOS.length, 5);
  assert.deepEqual(
    SCENARIOS.map((scenario) => scenario.id).sort(),
    ["discovery", "launch", "modal", "noisy", "recovery"],
  );
  for (const scenario of SCENARIOS) {
    assert.ok(scenario.title.length > 0, `${scenario.id} needs a title`);
    assert.ok(scenario.variants.length >= 1, `${scenario.id} needs variants`);
    assert.equal(new Set(scenario.variants).size, scenario.variants.length);
    assert.equal(scenarioById(scenario.id), scenario);
    // A run object must be constructible for each variant without a server.
    for (const variant of scenario.variants) {
      const run = scenario.create(variant);
      assert.equal(typeof run.setup, "function");
      assert.equal(typeof run.verify, "function");
    }
  }
  assert.equal(scenarioById("nope"), undefined);
});

test("unknown variants are rejected rather than silently substituted", () => {
  for (const id of ["launch", "noisy", "modal"]) {
    const scenario = scenarioById(id);
    assert.ok(scenario);
    assert.throws(() => scenario.create("not-a-variant"), /unknown/);
  }
});
