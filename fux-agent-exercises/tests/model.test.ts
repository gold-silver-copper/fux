import assert from "node:assert/strict";
import test from "node:test";
import {
  docsExcerptFor,
  extractFlashIdsFromDocs,
  pickBestFlash,
  rankFlashId,
  rejectedFlashAliases,
  stableFlashCandidates,
} from "../src/model.ts";

test("only bare gemini-<version>-flash ids rank as stable Flash", () => {
  assert.equal(rankFlashId("gemini-3.8-flash"), 3008);
  assert.equal(rankFlashId("gemini-4-flash"), 4000);
  for (const rejected of [
    "gemini-flash-latest",
    "gemini-flash-lite-latest",
    "gemini-3.8-flash-lite",
    "gemini-3-flash-preview",
    "gemini-3.1-flash-live-preview",
    "gemini-3.1-flash-image",
    "gemini-2.5-flash-preview-tts",
    "gemini-3.8-pro",
  ]) {
    assert.equal(rankFlashId(rejected), null, `${rejected} must not rank as stable Flash`);
  }
});

test("the newest stable Flash wins regardless of catalog order", () => {
  const ids = [
    "gemini-3.6-flash",
    "gemini-2.5-flash",
    "gemini-flash-latest",
    "gemini-3.8-flash",
    "gemini-3.8-flash-lite",
    "gemini-3.7-flash",
    "gemini-4-flash-preview",
  ];
  assert.equal(pickBestFlash(ids)?.id, "gemini-3.8-flash");
  assert.deepEqual(
    stableFlashCandidates(ids).map((candidate) => candidate.id),
    ["gemini-2.5-flash", "gemini-3.6-flash", "gemini-3.7-flash", "gemini-3.8-flash"],
  );
});

test("minor versions order numerically, not lexically", () => {
  assert.equal(pickBestFlash(["gemini-3.9-flash", "gemini-3.10-flash"])?.id, "gemini-3.10-flash");
  assert.equal(pickBestFlash(["gemini-3.8-flash", "gemini-10-flash"])?.id, "gemini-10-flash");
});

test("no stable Flash yields no candidate", () => {
  assert.equal(pickBestFlash(["gemini-flash-latest", "gemini-3-flash-preview"]), undefined);
});

test("rejected aliases are reported for the record", () => {
  assert.deepEqual(rejectedFlashAliases(["gemini-3.8-flash", "gemini-flash-latest", "gemini-3.8-flash-lite", "gemini-3.8-pro"]), [
    "gemini-3.8-flash-lite",
    "gemini-flash-latest",
  ]);
});

test("documentation ids survive HTML markup and excerpts are located", () => {
  const html =
    "<div><h2>Gemini 3.8 Flash</h2><code>gemini-3.8-flash</code> and <code>gemini-3.8-flash-lite</code>" +
    "<p>previous: <code>gemini-3.7-flash</code></p></div>";
  assert.deepEqual(extractFlashIdsFromDocs(html), [
    "gemini-3.7-flash",
    "gemini-3.8-flash",
    "gemini-3.8-flash-lite",
  ]);
  const excerpt = docsExcerptFor(html, "gemini-3.8-flash");
  assert.ok(excerpt && excerpt.includes("Gemini 3.8 Flash"));
  assert.equal(docsExcerptFor(html, "gemini-9.9-flash"), null);
});
