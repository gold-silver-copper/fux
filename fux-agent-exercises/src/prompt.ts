/**
 * The exercised agent's whole context: what it is driving, the one tool it has,
 * and fux's own documentation.
 *
 * Nothing here explains how to accomplish any scenario. Working that out from
 * the README is the thing being measured.
 */
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { TOOL_NAME } from "./tool.ts";

export interface Documentation {
  path: string;
  text: string;
  sha256: string;
  lines: number;
}

export function loadDocumentation(path: string): Documentation {
  const text = readFileSync(path, "utf8");
  return {
    path,
    text,
    sha256: createHash("sha256").update(text).digest("hex"),
    lines: text.split("\n").length,
  };
}

export function buildSystemPrompt(documentation: Documentation, maxToolCalls: number): string {
  return [
    "You are operating a running instance of fux, a terminal multiplexer, over its JSON-RPC API.",
    "",
    "# Your only tool",
    "",
    `You have exactly one tool: \`${TOOL_NAME}\`. It sends one JSON-RPC request to this fux server`,
    "and returns the server's raw response.",
    "",
    "- `method` is the JSON-RPC method name.",
    "- `params_json` is the request's `params`, encoded as a JSON string. Omit it when a method takes none.",
    "",
    "The fux documentation below shows command-line examples in the form",
    "",
    "    fux rpc METHOD 'JSON'",
    "",
    `Translate those into tool calls: \`method\` = METHOD, \`params_json\` = the JSON text. For example,`,
    "`fux rpc world.query '{\"data\":{\"components\":[\"fux::model::Workspace\"]}}'` becomes a call with",
    'method `world.query` and params_json `{"data":{"components":["fux::model::Workspace"]}}`.',
    "",
    "# What you do not have",
    "",
    "- No shell, no filesystem access, no editor, no network tools, and no second fux tool.",
    "- No interactive user. Nobody will answer questions, so do not ask any; decide and act.",
    "- Nothing is done on your behalf: responses are returned verbatim, entity ids are never looked up",
    "  for you, terminal output is never decoded for you, and no request is ever retried for you.",
    "",
    `You have at most ${maxToolCalls} \`${TOOL_NAME}\` calls for this task. Spend them deliberately.`,
    "",
    "Entity ids in this API are plain numbers. Reuse the exact ids the server returns.",
    "",
    "Work out what to do from the documentation below, then do it. When you are finished, report what",
    "you changed and what you verified.",
    "",
    `# fux documentation (${documentation.path}, sha256 ${documentation.sha256.slice(0, 16)})`,
    "",
    documentation.text,
  ].join("\n");
}
