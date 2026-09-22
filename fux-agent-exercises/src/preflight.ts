/**
 * Preflight: prove the campaign can actually run before spending anything.
 *
 * Checks the fux binary, the installed pi SDK, provider credentials, and the
 * required model — and resolves which Gemini Flash release that is. Nothing here
 * prints a credential; only whether one resolved.
 */
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { resolveFlashModel, type ResolvedModel } from "./model.ts";
import { loadPiSdk, type PiSdk } from "./pi-sdk.ts";

export interface PreflightOptions {
  fuxBinary: string;
  providerId: string;
  pinnedModelId?: string;
  skipModelVerification: boolean;
  /** Skip provider auth and model resolution entirely (offline harness checks). */
  offline: boolean;
}

export interface PreflightResult {
  ok: boolean;
  problems: string[];
  fux: { path: string; version: string | null; gitRevision: string | null; gitDirty: boolean | null };
  pi: { version: string; root: string };
  provider: { id: string; authenticated: boolean; method: string | null; availableModels: number } | null;
  model: ResolvedModel | null;
  sdk: PiSdk;
  modelRuntime: unknown;
}

function fuxVersion(binary: string): string | null {
  for (const args of [["--version"], ["-V"]]) {
    try {
      return execFileSync(binary, args, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
    } catch {
      // Try the next form; fux may not implement either.
    }
  }
  return null;
}

export function gitIdentity(cwd: string): { revision: string | null; dirty: boolean | null } {
  try {
    const revision = execFileSync("git", ["rev-parse", "HEAD"], {
      cwd,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
    const status = execFileSync("git", ["status", "--porcelain"], {
      cwd,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    return { revision, dirty: status.trim().length > 0 };
  } catch {
    return { revision: null, dirty: null };
  }
}

export async function preflight(options: PreflightOptions, repoRoot: string): Promise<PreflightResult> {
  const problems: string[] = [];

  if (!existsSync(options.fuxBinary)) {
    problems.push(`fux binary not found at ${options.fuxBinary}; build it with cargo build --release`);
  }
  const git = gitIdentity(repoRoot);

  const sdk = await loadPiSdk();

  let provider: PreflightResult["provider"] = null;
  let model: ResolvedModel | null = null;
  let modelRuntime: unknown = null;

  if (!options.offline) {
    modelRuntime = await sdk.ModelRuntime.create();
    const runtime = modelRuntime as {
      getModels: (id?: string) => Array<{ id: string }>;
      getAvailable: (id?: string) => Promise<ReadonlyArray<{ id: string }>>;
      checkAuth: (id: string) => Promise<{ method?: string; authenticated?: boolean } | undefined>;
    };

    const catalog = runtime.getModels(options.providerId);
    if (!catalog || catalog.length === 0) {
      problems.push(
        `pi's catalog has no models for provider "${options.providerId}"; run pi once to populate its model store`,
      );
    }

    let authenticated = false;
    let method: string | null = null;
    let available = 0;
    try {
      const auth = await runtime.checkAuth(options.providerId);
      method = auth?.method ?? null;
      const availableModels = await runtime.getAvailable(options.providerId);
      available = availableModels.length;
      authenticated = available > 0;
    } catch (error) {
      problems.push(`provider auth check failed for ${options.providerId}: ${(error as Error).message}`);
    }
    if (!authenticated) {
      problems.push(
        `no usable credential for provider "${options.providerId}"; set GEMINI_API_KEY or run pi's login for it`,
      );
    }
    provider = { id: options.providerId, authenticated, method, availableModels: available };

    if (catalog && catalog.length > 0) {
      try {
        model = await resolveFlashModel(sdk, modelRuntime, {
          providerId: options.providerId,
          pinnedModelId: options.pinnedModelId,
          skipVerification: options.skipModelVerification,
        });
        if (!model.verification.ok) {
          const detail = model.verification.warnings.join("; ") || "no confirming source";
          problems.push(`model verification did not pass for ${model.modelId}: ${detail}`);
        }
        const availableModels = await runtime.getAvailable(options.providerId);
        if (!availableModels.some((entry) => entry.id === model?.modelId)) {
          problems.push(
            `${options.providerId}/${model.modelId} is not in pi's available (authenticated) model list`,
          );
        }
      } catch (error) {
        problems.push(`model resolution failed: ${(error as Error).message}`);
      }
    }
  }

  return {
    ok: problems.length === 0,
    problems,
    fux: {
      path: options.fuxBinary,
      version: existsSync(options.fuxBinary) ? fuxVersion(options.fuxBinary) : null,
      gitRevision: git.revision,
      gitDirty: git.dirty,
    },
    pi: { version: sdk.version, root: sdk.root },
    provider,
    model,
    sdk,
    modelRuntime,
  };
}
