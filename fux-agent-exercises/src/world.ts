/**
 * Harness-side helpers for building deterministic fixtures and for independent
 * verification. These use the same public BRP surface the agent has; they exist
 * so scenario code stays readable, and they are never exposed to the agent.
 */
import { chmodSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { decodeScreen, type DecodedScreen } from "./ansi.ts";
import { BrpClient, frame, getComponent, query, type QueryRow } from "./brp.ts";

export const COMPONENT = {
  workspace: "fux::model::Workspace",
  tab: "fux::model::Tab",
  paneView: "fux::model::PaneView",
  launch: "fux::model::Launch",
  processState: "fux::model::ProcessState",
  viewer: "fux::model::Viewer",
  viewing: "fux::model::Viewing",
  onTab: "fux::model::OnTab",
  focused: "fux::model::Focused",
  name: "bevy_ecs::name::Name",
  children: "bevy_ecs::hierarchy::Children",
  childOf: "bevy_ecs::hierarchy::ChildOf",
  prefix: "fux::interaction::Prefix",
  overlay: "fux::interaction::Overlay",
} as const;

export interface ProcessStatus {
  kind: "starting" | "running" | "exited" | "failed";
  pid?: number;
  code?: number;
  error?: string | null;
}

export interface ProcessSnapshot {
  entity: number;
  name: string | null;
  status: ProcessStatus;
  rows: number;
  cols: number;
  argv: string[] | null;
  cwd: string | null;
}

/** Writes an executable fixture script with no reliance on inherited env. */
export function writeScript(directory: string, name: string, body: string): string {
  const path = join(directory, name);
  writeFileSync(path, body.endsWith("\n") ? body : `${body}\n`);
  chmodSync(path, 0o755);
  return path;
}

export async function processes(client: BrpClient): Promise<ProcessSnapshot[]> {
  const rows = await query(client, [COMPONENT.processState], {
    option: [COMPONENT.name, COMPONENT.launch],
  });
  return rows.map((row) => {
    const state = row.components[COMPONENT.processState] as {
      status: ProcessStatus;
      rows: number;
      cols: number;
    };
    const launch = row.components[COMPONENT.launch] as
      | { argv: string[]; cwd: string }
      | undefined;
    return {
      entity: row.entity,
      name: (row.components[COMPONENT.name] as string | undefined) ?? null,
      status: state.status,
      rows: state.rows,
      cols: state.cols,
      argv: launch?.argv ?? null,
      cwd: launch?.cwd ?? null,
    };
  });
}

export async function processByName(
  client: BrpClient,
  name: string,
): Promise<ProcessSnapshot | undefined> {
  return (await processes(client)).find((process) => process.name === name);
}

export async function processByEntity(
  client: BrpClient,
  entity: number,
): Promise<ProcessSnapshot | undefined> {
  return (await processes(client)).find((process) => process.entity === entity);
}

export async function nameOf(client: BrpClient, entity: number): Promise<string | null> {
  const value = await getComponent(client, entity, COMPONENT.name);
  return typeof value === "string" ? value : null;
}

export async function childrenOf(client: BrpClient, entity: number): Promise<number[]> {
  const value = await getComponent(client, entity, COMPONENT.children);
  return Array.isArray(value) ? (value as number[]) : [];
}

export async function parentOf(client: BrpClient, entity: number): Promise<number | null> {
  const value = await getComponent(client, entity, COMPONENT.childOf);
  return typeof value === "number" ? value : null;
}

/** Every pane view under `root`, depth first. */
export async function paneViewsUnder(client: BrpClient, root: number): Promise<number[]> {
  const found: number[] = [];
  const pending = [root];
  const seen = new Set<number>();
  while (pending.length > 0) {
    const entity = pending.pop() as number;
    if (seen.has(entity)) continue;
    seen.add(entity);
    if ((await getComponent(client, entity, COMPONENT.paneView)) !== undefined) {
      found.push(entity);
    }
    pending.push(...(await childrenOf(client, entity)));
  }
  return found;
}

/** Pane view entity that refers to `process`, if any. */
export async function paneViewOf(
  client: BrpClient,
  process: number,
): Promise<number | undefined> {
  const rows = await query(client, [COMPONENT.paneView]);
  return rows.find((row) => (row.components[COMPONENT.paneView] as { pane: number }).pane === process)
    ?.entity;
}

export async function workspaces(client: BrpClient): Promise<Array<{ entity: number; name: string | null }>> {
  const rows = await query(client, [COMPONENT.workspace], { option: [COMPONENT.name] });
  return rows.map((row) => ({
    entity: row.entity,
    name: (row.components[COMPONENT.name] as string | undefined) ?? null,
  }));
}

export async function workspaceByName(client: BrpClient, name: string): Promise<number | undefined> {
  return (await workspaces(client)).find((workspace) => workspace.name === name)?.entity;
}

export async function tabs(client: BrpClient): Promise<Array<{ entity: number; name: string | null }>> {
  const rows: QueryRow[] = await query(client, [COMPONENT.tab], { option: [COMPONENT.name] });
  return rows.map((row) => ({
    entity: row.entity,
    name: (row.components[COMPONENT.name] as string | undefined) ?? null,
  }));
}

export async function tabByName(client: BrpClient, name: string): Promise<number | undefined> {
  return (await tabs(client)).find((tab) => tab.name === name)?.entity;
}

export interface ViewerSnapshot {
  entity: number;
  rows: number;
  cols: number;
  zoom: boolean;
  scrollback: number;
  notice: { text: string; error: boolean } | null;
  viewing: number | null;
  onTab: number | null;
  focused: number | null;
}

export async function viewerSnapshot(client: BrpClient, viewer: number): Promise<ViewerSnapshot> {
  const outcome = await client.call("world.get_components", {
    entity: viewer,
    components: [COMPONENT.viewer, COMPONENT.viewing, COMPONENT.onTab, COMPONENT.focused],
  });
  const components = (outcome.result as { components?: Record<string, unknown> } | null)?.components ?? {};
  const base = components[COMPONENT.viewer] as
    | { rows: number; cols: number; zoom: boolean; scrollback: number; notice: { text: string; error: boolean } | null }
    | undefined;
  return {
    entity: viewer,
    rows: base?.rows ?? 0,
    cols: base?.cols ?? 0,
    zoom: base?.zoom ?? false,
    scrollback: base?.scrollback ?? 0,
    notice: base?.notice ?? null,
    viewing: (components[COMPONENT.viewing] as number | undefined) ?? null,
    onTab: (components[COMPONENT.onTab] as number | undefined) ?? null,
    focused: (components[COMPONENT.focused] as number | undefined) ?? null,
  };
}

/** Creates a process entity plus a pane view under `tab`, with an exact recipe. */
export async function addProcessPane(
  client: BrpClient,
  options: { tab: number; argv: string[]; cwd: string; name: string; historyLines?: number },
): Promise<{ process: number; paneView: number }> {
  const spawned = (await client.expect("world.spawn_entity", {
    components: {
      [COMPONENT.launch]: {
        argv: options.argv,
        cwd: options.cwd,
        history_lines: options.historyLines ?? 500,
      },
      [COMPONENT.name]: options.name,
    },
  })) as { entity: number };
  const view = (await client.expect("world.spawn_entity", {
    components: { [COMPONENT.paneView]: { pane: spawned.entity } },
  })) as { entity: number };
  await client.expect("world.reparent_entities", {
    entities: [view.entity],
    parent: options.tab,
  });
  return { process: spawned.entity, paneView: view.entity };
}

/** Adds an empty tab to `workspace` and returns it. */
export async function addTab(client: BrpClient, workspace: number, name: string): Promise<number> {
  const spawned = (await client.expect("world.spawn_entity", {
    components: { [COMPONENT.tab]: {}, [COMPONENT.name]: name },
  })) as { entity: number };
  await client.expect("world.reparent_entities", {
    entities: [spawned.entity],
    parent: workspace,
  });
  return spawned.entity;
}

export async function addWorkspace(client: BrpClient, name: string): Promise<number> {
  const spawned = (await client.expect("world.spawn_entity", {
    components: { [COMPONENT.workspace]: {}, [COMPONENT.name]: name },
  })) as { entity: number };
  return spawned.entity;
}

/** Parent chain from `entity` upward, excluding `entity` itself. */
export async function ancestry(client: BrpClient, entity: number): Promise<number[]> {
  const chain: number[] = [];
  const seen = new Set<number>([entity]);
  let current: number | null = await parentOf(client, entity);
  while (current !== null && !seen.has(current)) {
    chain.push(current);
    seen.add(current);
    current = await parentOf(client, current);
  }
  return chain;
}

/** The tab and workspace a pane view actually lives under, through any splits. */
export async function resolveTabAndWorkspace(
  client: BrpClient,
  paneView: number,
): Promise<{ tab: number | null; workspace: number | null }> {
  let tab: number | null = null;
  let workspace: number | null = null;
  for (const ancestor of await ancestry(client, paneView)) {
    if (tab === null && (await getComponent(client, ancestor, COMPONENT.tab)) !== undefined) {
      tab = ancestor;
    }
    if ((await getComponent(client, ancestor, COMPONENT.workspace)) !== undefined) {
      workspace = ancestor;
      break;
    }
  }
  return { tab, workspace };
}

export async function decodeViewer(client: BrpClient, viewer: number): Promise<DecodedScreen> {
  const snapshot = await viewerSnapshot(client, viewer);
  const paint = await frame(client, viewer);
  return decodeScreen(paint, snapshot.rows, snapshot.cols);
}

/** Rows a human would read as pane content: everything above the status bar. */
export function contentRows(screen: DecodedScreen): string[] {
  return screen.rows.slice(0, Math.max(0, screen.rows.length - 1));
}
