/**
 * Resolves and verifies the Gemini Flash model the campaign must use.
 *
 * The exercise specification requires the latest *stable* Gemini Flash: not
 * Flash-Lite, not a preview, and not a floating `-latest` alias. Nothing here
 * hard-codes a version. The id comes from pi's own catalog and is then checked
 * against Google's live model list and published model documentation.
 */
import type { PiSdk } from "./pi-sdk.ts";

/** `hl=en` pins the locale; the page is otherwise served in the caller's language. */
const DOCS_URL = "https://ai.google.dev/gemini-api/docs/models?hl=en";
const LIST_URL = "https://generativelanguage.googleapis.com/v1beta/models";

/** `gemini-<major>[.<minor>]-flash` and nothing else: no lite/preview/image/tts/live suffix. */
const STABLE_FLASH = /^gemini-(\d+)(?:\.(\d+))?-flash$/;

export interface FlashCandidate {
  id: string;
  rank: number;
}

export interface ModelVerification {
  verifiedAt: string;
  sources: string[];
  catalogIds: string[];
  liveApiIds: string[] | null;
  docsIds: string[] | null;
  docsExcerpt: string | null;
  liveApiVersion: string | null;
  warnings: string[];
  ok: boolean;
}

export interface ResolvedModel {
  providerId: string;
  modelId: string;
  /** The pi catalog model object handed to createAgentSession. */
  model: unknown;
  displayName: string | null;
  /** Aliases deliberately rejected, recorded so the report can show why. */
  rejectedAliases: string[];
  verification: ModelVerification;
}

export function rankFlashId(id: string): number | null {
  const match = STABLE_FLASH.exec(id);
  if (!match) return null;
  const major = Number(match[1]);
  const minor = match[2] === undefined ? 0 : Number(match[2]);
  if (!Number.isFinite(major) || !Number.isFinite(minor)) return null;
  return major * 1000 + minor;
}

/** Stable Flash ids from a list of model ids, best last. */
export function stableFlashCandidates(ids: readonly string[]): FlashCandidate[] {
  const seen = new Map<string, number>();
  for (const id of ids) {
    const rank = rankFlashId(id);
    if (rank !== null) seen.set(id, rank);
  }
  return [...seen.entries()]
    .map(([id, rank]) => ({ id, rank }))
    .sort((a, b) => a.rank - b.rank);
}

export function pickBestFlash(ids: readonly string[]): FlashCandidate | undefined {
  const candidates = stableFlashCandidates(ids);
  return candidates.at(-1);
}

/** Aliases and near-misses that must never be selected, for the record. */
export function rejectedFlashAliases(ids: readonly string[]): string[] {
  return ids
    .filter((id) => id.includes("flash") && rankFlashId(id) === null)
    .sort();
}

async function fetchText(url: string, timeoutMs: number): Promise<string> {
  const response = await fetch(url, {
    signal: AbortSignal.timeout(timeoutMs),
    headers: { "user-agent": "fux-agent-exercises/1.0", "accept-language": "en" },
  });
  if (!response.ok) throw new Error(`${url} -> HTTP ${response.status}`);
  return await response.text();
}

export function extractFlashIdsFromDocs(html: string): string[] {
  const text = html.replace(/<[^>]+>/g, " ");
  const found = new Set<string>();
  for (const match of text.matchAll(/gemini-\d+(?:\.\d+)?-flash(?:-[a-z0-9-]+)?/g)) {
    found.add(match[0]);
  }
  return [...found].sort();
}

export function docsExcerptFor(html: string, modelId: string): string | null {
  const text = html.replace(/<[^>]+>/g, " ").replace(/\s+/g, " ");
  const index = text.indexOf(modelId);
  if (index < 0) return null;
  return text.slice(Math.max(0, index - 200), index + 200).trim();
}

/**
 * Cross-checks the catalog choice against Google. Network failures are recorded
 * as warnings and reported as unverified rather than silently accepted.
 */
export async function verifyAgainstGoogle(
  modelId: string,
  catalogIds: string[],
  options: { timeoutMs?: number; apiKey?: string } = {},
): Promise<ModelVerification> {
  const timeoutMs = options.timeoutMs ?? 45_000;
  const warnings: string[] = [];
  const sources: string[] = ["pi-catalog"];
  let liveApiIds: string[] | null = null;
  let liveApiVersion: string | null = null;
  let docsIds: string[] | null = null;
  let docsExcerpt: string | null = null;

  if (options.apiKey) {
    try {
      const body = await fetchText(
        `${LIST_URL}?key=${encodeURIComponent(options.apiKey)}&pageSize=200`,
        timeoutMs,
      );
      const parsed = JSON.parse(body) as { models?: Array<{ name?: string; version?: string }> };
      const models = parsed.models ?? [];
      liveApiIds = models
        .map((model) => (model.name ?? "").replace(/^models\//, ""))
        .filter((id) => id.length > 0)
        .sort();
      liveApiVersion =
        models.find((model) => (model.name ?? "").replace(/^models\//, "") === modelId)?.version ??
        null;
      sources.push("generativelanguage.googleapis.com/v1beta/models");
    } catch (error) {
      warnings.push(`live model list unavailable: ${(error as Error).message}`);
    }
  } else {
    warnings.push("no Gemini API key available for the live model list check");
  }

  try {
    const html = await fetchText(DOCS_URL, timeoutMs);
    docsIds = extractFlashIdsFromDocs(html);
    docsExcerpt = docsExcerptFor(html, modelId);
    sources.push(DOCS_URL);
  } catch (error) {
    warnings.push(`model documentation unavailable: ${(error as Error).message}`);
  }

  if (liveApiIds && !liveApiIds.includes(modelId)) {
    warnings.push(`${modelId} is not served by the live Gemini model list`);
  }
  if (liveApiVersion && /preview|exp/i.test(liveApiVersion)) {
    warnings.push(`${modelId} reports a preview/experimental version: ${liveApiVersion}`);
  }
  for (const [label, ids] of [
    ["live model list", liveApiIds],
    ["model documentation", docsIds],
  ] as const) {
    if (!ids) continue;
    const better = stableFlashCandidates(ids).filter(
      (candidate) => candidate.rank > (rankFlashId(modelId) ?? -1),
    );
    if (better.length > 0) {
      warnings.push(
        `${label} advertises a newer stable Flash release: ${better.map((c) => c.id).join(", ")}`,
      );
    }
    if (!ids.includes(modelId)) {
      warnings.push(`${modelId} does not appear in the ${label}`);
    }
  }
  if (!catalogIds.includes(modelId)) {
    warnings.push(`${modelId} is not in pi's catalog`);
  }

  // Verified means: a documentation source confirmed it and nothing newer/stabler exists.
  const confirmedBySource = Boolean(liveApiIds?.includes(modelId) || docsIds?.includes(modelId));
  return {
    verifiedAt: new Date().toISOString(),
    sources,
    catalogIds,
    liveApiIds,
    docsIds,
    docsExcerpt,
    liveApiVersion,
    warnings,
    ok: confirmedBySource && warnings.length === 0,
  };
}

export interface ResolveOptions {
  /** Provider id in pi's catalog. Default: google. */
  providerId?: string;
  /** Skip the Google cross-check. Only for offline dry runs. */
  skipVerification?: boolean;
  /** Pin an explicit id instead of picking the newest stable Flash. */
  pinnedModelId?: string;
  timeoutMs?: number;
}

export async function resolveFlashModel(
  sdk: PiSdk,
  modelRuntime: any,
  options: ResolveOptions = {},
): Promise<ResolvedModel> {
  const providerId = options.providerId ?? "google";
  const catalog = modelRuntime.getModels(providerId) as Array<{ id: string; name?: string }>;
  if (!catalog || catalog.length === 0) {
    throw new Error(`pi's catalog has no models for provider "${providerId}"`);
  }
  const catalogIds = catalog.map((model) => model.id).sort();
  const chosenId = options.pinnedModelId ?? pickBestFlash(catalogIds)?.id;
  if (!chosenId) {
    throw new Error(
      `no stable Gemini Flash model in pi's "${providerId}" catalog; saw: ${catalogIds.join(", ")}`,
    );
  }
  if (rankFlashId(chosenId) === null) {
    throw new Error(
      `${chosenId} is not a stable Gemini Flash id (Flash-Lite, previews and -latest aliases are excluded)`,
    );
  }
  const model = modelRuntime.getModel(providerId, chosenId);
  if (!model) {
    throw new Error(`pi cannot resolve ${providerId}/${chosenId}`);
  }

  const verification = options.skipVerification
    ? {
        verifiedAt: new Date().toISOString(),
        sources: ["pi-catalog"],
        catalogIds,
        liveApiIds: null,
        docsIds: null,
        docsExcerpt: null,
        liveApiVersion: null,
        warnings: ["verification skipped by configuration"],
        ok: false,
      }
    : await verifyAgainstGoogle(chosenId, catalogIds, {
        timeoutMs: options.timeoutMs,
        apiKey: process.env.GEMINI_API_KEY ?? process.env.GOOGLE_API_KEY,
      });

  return {
    providerId,
    modelId: chosenId,
    model,
    displayName: (model as { name?: string }).name ?? null,
    rejectedAliases: rejectedFlashAliases(catalogIds),
    verification,
  };
}
