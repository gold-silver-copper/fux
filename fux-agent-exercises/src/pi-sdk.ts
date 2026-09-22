/**
 * Resolves the installed pi coding agent without an npm install or a build step.
 *
 * Everything is imported through one resolved package root so typebox stays a
 * single module instance: pi validates tool arguments with typebox's compiler,
 * and a second copy of the library would reject schemas this harness builds.
 */
import { execFileSync } from "node:child_process";
import { existsSync, realpathSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";

export interface PiSdk {
  /** Absolute real path of the installed @earendil-works/pi-coding-agent. */
  root: string;
  version: string;
  createAgentSession: (options: Record<string, unknown>) => Promise<any>;
  DefaultResourceLoader: new (options: Record<string, unknown>) => any;
  SessionManager: any;
  SettingsManager: any;
  ModelRuntime: any;
  defineTool: (tool: Record<string, unknown>) => any;
  /** typebox schema builders, from the same instance pi validates with. */
  Type: any;
}

function candidateRoots(): string[] {
  const roots: string[] = [];
  const configured = process.env.FUX_PI_PACKAGE_ROOT;
  if (configured) roots.push(configured);
  try {
    const globalRoot = execFileSync("npm", ["root", "-g"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
    if (globalRoot) roots.push(join(globalRoot, "@earendil-works", "pi-coding-agent"));
  } catch {
    // npm is optional; the executable probe below may still succeed.
  }
  try {
    const cli = execFileSync("sh", ["-c", "command -v pi"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
    if (cli) {
      // .../pi-coding-agent/dist/bundle/cli.js -> .../pi-coding-agent
      let dir = dirname(realpathSync(cli));
      for (let i = 0; i < 4; i += 1) {
        if (existsSync(join(dir, "package.json")) && existsSync(join(dir, "dist", "index.js"))) {
          roots.push(dir);
          break;
        }
        dir = dirname(dir);
      }
    }
  } catch {
    // Fall through to the aggregate error below.
  }
  return roots;
}

/** First existing path among `candidates`, or undefined. */
function firstExisting(candidates: string[]): string | undefined {
  return candidates.find((candidate) => existsSync(candidate));
}

async function importAbsolute(file: string): Promise<any> {
  return import(pathToFileURL(file).href);
}

export async function loadPiSdk(): Promise<PiSdk> {
  const tried = candidateRoots();
  const root = firstExisting(tried.map((candidate) => join(candidate, "dist", "index.js")));
  if (!root) {
    throw new Error(
      `cannot locate @earendil-works/pi-coding-agent; set FUX_PI_PACKAGE_ROOT. Tried: ${
        tried.join(", ") || "(nothing)"
      }`,
    );
  }
  const packageRoot = realpathSync(dirname(dirname(root)));
  const sdk = await importAbsolute(join(packageRoot, "dist", "index.js"));

  // typebox is a dependency of the installed package, not of this harness.
  const typeboxEntry = firstExisting([
    join(packageRoot, "node_modules", "typebox", "build", "index.mjs"),
    join(dirname(dirname(packageRoot)), "typebox", "build", "index.mjs"),
  ]);
  if (!typeboxEntry) {
    throw new Error(`cannot locate typebox next to ${packageRoot}`);
  }
  const typebox = await importAbsolute(typeboxEntry);

  const missing = [
    "createAgentSession",
    "DefaultResourceLoader",
    "SessionManager",
    "SettingsManager",
    "ModelRuntime",
    "defineTool",
    "VERSION",
  ].filter((name) => sdk[name] === undefined);
  if (missing.length > 0) {
    throw new Error(`installed pi SDK is missing expected exports: ${missing.join(", ")}`);
  }
  if (typeof typebox.Type?.Object !== "function") {
    throw new Error(`typebox at ${typeboxEntry} does not expose Type.Object`);
  }

  return {
    root: packageRoot,
    version: String(sdk.VERSION),
    createAgentSession: sdk.createAgentSession,
    DefaultResourceLoader: sdk.DefaultResourceLoader,
    SessionManager: sdk.SessionManager,
    SettingsManager: sdk.SettingsManager,
    ModelRuntime: sdk.ModelRuntime,
    defineTool: sdk.defineTool,
    Type: typebox.Type,
  };
}
