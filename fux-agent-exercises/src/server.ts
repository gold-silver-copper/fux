/**
 * One disposable fux server per exercise run.
 *
 * Each run gets its own loopback port, temporary HOME/workspace and shell
 * configuration, so an exercise never touches a server the user is already
 * running. Only the process this fixture spawned is signalled during cleanup.
 *
 * This is process isolation for repeatability, not a sandbox: fux's BRP is
 * unrestricted same-user command execution, so anything reachable through it
 * runs with this user's privileges.
 */
import { type ChildProcess, spawn } from "node:child_process";
import { createServer } from "node:net";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { BrpClient } from "./brp.ts";

/** Environment variables whose names suggest credentials; never passed to fixtures. */
const SECRET_PATTERN = /(KEY|TOKEN|SECRET|PASSWORD|CREDENTIAL|AUTH|COOKIE|SESSION)/i;

export interface ServerOptions {
  /** Path to the fux executable under test. */
  binary: string;
  /** Directory that will hold the config, HOME and fixture files. */
  directory: string;
  historyLines?: number;
  clipboard?: "disabled" | "write-only";
  shell?: string[];
  startupTimeoutMs?: number;
}

export interface ServerHandle {
  readonly client: BrpClient;
  readonly endpoint: string;
  readonly port: number;
  readonly directory: string;
  /** Working directory fixtures should use; distinct from the config/HOME dir. */
  readonly workDir: string;
  readonly pid: number;
  readonly logPath: string;
  /**
   * Whether this child has already exited. `process.kill(pid, 0)` cannot answer
   * that: an unreaped child is still addressable, so it would report a crashed
   * server as alive.
   */
  hasExited(): boolean;
  /** Waits briefly for a pending exit notification to land. */
  settleExit(timeoutMs?: number): Promise<boolean>;
  /** Graceful shutdown, then SIGTERM/SIGKILL for this child only. */
  stop(): Promise<{ graceful: boolean; exitCode: number | null; signal: string | null }>;
}

/** Reserves a loopback port by binding and releasing it. */
async function reservePort(): Promise<number> {
  return await new Promise<number>((resolve, reject) => {
    const probe = createServer();
    probe.on("error", reject);
    probe.listen(0, "127.0.0.1", () => {
      const address = probe.address();
      if (typeof address === "object" && address) {
        const { port } = address;
        probe.close(() => resolve(port));
      } else {
        probe.close(() => reject(new Error("could not reserve a loopback port")));
      }
    });
  });
}

/**
 * A deliberately small environment. Nothing that looks like a credential is
 * forwarded, so fixture processes cannot read the campaign's provider keys.
 */
export function fixtureEnvironment(home: string, base: NodeJS.ProcessEnv = process.env): NodeJS.ProcessEnv {
  const allowed = ["PATH", "LANG", "LC_ALL", "TZ", "TERM"];
  const env: NodeJS.ProcessEnv = {};
  for (const name of allowed) {
    const value = base[name];
    if (value !== undefined && !SECRET_PATTERN.test(name)) env[name] = value;
  }
  env.PATH ??= "/usr/bin:/bin:/usr/sbin:/sbin";
  env.TERM ??= "xterm-256color";
  env.HOME = home;
  env.TMPDIR = join(home, "tmp");
  env.SHELL = "/bin/sh";
  env.PS1 = "$ ";
  env.HISTFILE = "/dev/null";
  env.ENV = "";
  env.BASH_ENV = "";
  return env;
}

export async function startServer(options: ServerOptions): Promise<ServerHandle> {
  const { binary, directory } = options;
  const workDir = join(directory, "work");
  mkdirSync(workDir, { recursive: true });
  mkdirSync(join(directory, "tmp"), { recursive: true });

  const configPath = join(directory, "fux.json");
  writeFileSync(
    configPath,
    `${JSON.stringify(
      {
        shell: options.shell ?? ["/bin/sh"],
        history_lines: options.historyLines ?? 500,
        clipboard: options.clipboard ?? "disabled",
      },
      null,
      2,
    )}\n`,
  );

  const port = await reservePort();
  const logPath = join(directory, "server.log");
  const child: ChildProcess = spawn(
    binary,
    ["server", "--port", String(port), "--config", configPath],
    {
      cwd: workDir,
      env: fixtureEnvironment(directory),
      stdio: ["ignore", "pipe", "pipe"],
      detached: false,
    },
  );
  const logChunks: string[] = [];
  const record = (chunk: Buffer) => {
    logChunks.push(chunk.toString("utf8"));
    if (logChunks.length > 400) logChunks.splice(0, logChunks.length - 400);
  };
  child.stdout?.on("data", record);
  child.stderr?.on("data", record);
  const flushLog = () => {
    try {
      writeFileSync(logPath, logChunks.join(""));
    } catch {
      // Log capture is best effort; a failure here must not mask a result.
    }
  };

  let exited: { code: number | null; signal: string | null } | null = null;
  child.on("exit", (code, signal) => {
    exited = { code, signal };
    flushLog();
  });

  const endpoint = `http://127.0.0.1:${port}`;
  const client = new BrpClient(endpoint);
  const deadline = Date.now() + (options.startupTimeoutMs ?? 20_000);
  for (;;) {
    if (exited) {
      flushLog();
      throw new Error(
        `fux server exited during startup (code=${exited.code} signal=${exited.signal}); log: ${logChunks
          .join("")
          .slice(-2000)}`,
      );
    }
    const probe = await client.call("rpc.discover", undefined, 2_000);
    if (probe.result) break;
    if (Date.now() >= deadline) {
      flushLog();
      throw new Error(`fux server did not answer rpc.discover within the startup budget`);
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  flushLog();

  const stop = async () => {
    flushLog();
    if (exited) {
      return { graceful: true, exitCode: exited.code, signal: exited.signal };
    }
    const settled = new Promise<{ code: number | null; signal: string | null }>((resolve) => {
      if (exited) return resolve(exited);
      child.once("exit", (code, signal) => resolve({ code, signal }));
    });
    // Ask fux to shut down the way a client would.
    await client.call("world.trigger_event", { event: "fux::control::Shutdown", value: null }, 3_000);
    const graceful = await Promise.race([
      settled.then(() => true),
      new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 5_000)),
    ]);
    if (!graceful && child.pid !== undefined) {
      child.kill("SIGTERM");
      const stopped = await Promise.race([
        settled.then(() => true),
        new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 3_000)),
      ]);
      if (!stopped) child.kill("SIGKILL");
    }
    const final = await settled;
    flushLog();
    return { graceful, exitCode: final.code, signal: final.signal };
  };

  if (child.pid === undefined) throw new Error("fux server has no pid");

  const hasExited = () => exited !== null;
  const settleExit = async (timeoutMs = 500) => {
    const deadline = Date.now() + timeoutMs;
    while (!hasExited() && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
    return hasExited();
  };

  return {
    client,
    endpoint,
    port,
    directory,
    workDir,
    pid: child.pid,
    logPath,
    hasExited,
    settleExit,
    stop,
  };
}

/** Removes a run directory. Callers keep it when a run produced a failure. */
export function discardDirectory(directory: string): void {
  rmSync(directory, { recursive: true, force: true });
}

/** True when a pid is still addressable by this user. */
export function processAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "EPERM";
  }
}
