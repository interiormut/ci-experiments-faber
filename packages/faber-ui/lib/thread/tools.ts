/**
 * How a tool call is worded on the timeline.
 *
 * The wire names are the harness's verbs (`crates/harness/src/tools/schema.rs`
 * — `exec`, `read`, `patch`, …), written for the model. A reader scanning a run
 * wants the same two things a shell transcript gives them: what the agent did,
 * and *to what*. So each call gets a title in plain words and a meta line
 * carrying the one argument that identifies the call.
 *
 * The meta line is the always-visible second line of an {@link AgentStep}, not
 * its `description` — a description lives inside the fold, and putting the path
 * there would both hide it and make every row expansible for the sake of one
 * string.
 *
 * Unknown names are not an error: the schema is appended to over time and a
 * harness may yield tools of its own, so anything unrecognized falls back to
 * the raw name plus a best guess at its subject.
 */

import type { JsonValue } from "@/lib/api"

export type ToolDisplay = {
  /** Header label — the verb, in words. */
  title: string
  /** Second line: the argument that says which call this is. */
  meta?: string
}

/**
 * Verb phrases rather than bare nouns, because the row is a record of
 * something that happened. `targets` is the odd one out — it names no subject,
 * so its title has to carry the whole sentence.
 */
const TITLES: Record<string, string> = {
  targets: "List environments",
  exec: "Run command",
  start: "Start process",
  output: "Read process output",
  stdin: "Write to process",
  signal: "Signal process",
  read: "Read file",
  write: "Write file",
  edit: "Edit file",
  patch: "Apply patch",
  list: "List directory",
}

/** Keys worth showing for a tool this module has never heard of. */
const FALLBACK_KEYS = ["path", "command", "file", "pattern", "query", "url", "name"]

export function toolDisplay(name: string, input: unknown): ToolDisplay {
  return { title: TITLES[name] ?? name, meta: summarize(name, input) }
}

function summarize(name: string, input: unknown): string | undefined {
  // A call is drawn as soon as its block opens, before a single argument byte
  // has arrived, and `parseJsonLoosely` hands back `{ raw }` when what did
  // arrive was not valid JSON. Both are "no arguments yet", and the row simply
  // stands on its title until they land.
  const args = asObject(input)
  if (!args) return undefined

  // Every tool but `targets` requires one, so naming it on each row would put
  // the same token down the whole timeline for the ordinary single-environment
  // session — and at the front of the line, the part truncation spares. It is
  // the fallback subject instead: better than an empty second line when the
  // call has nothing else to identify it.
  return describe(name, args) ?? str(args.target)
}

function describe(name: string, args: Record<string, JsonValue>): string | undefined {
  switch (name) {
    case "targets":
      return undefined

    case "exec":
    case "start":
      return oneLine(str(args.command))

    case "output":
      return processLabel(args.process)

    case "stdin": {
      const handle = processLabel(args.process)
      const data = oneLine(str(args.data), 40)
      return handle && data ? `${handle} ← ${data}` : (handle ?? data)
    }

    case "signal": {
      const handle = processLabel(args.process)
      const signal = str(args.signal)
      return handle && signal ? `${handle} ← ${signal}` : (handle ?? signal)
    }

    case "read": {
      const path = shortPath(str(args.path))
      const offset = num(args.offset)
      const limit = num(args.limit)
      if (path && offset !== undefined && limit !== undefined) {
        return `${path}:${offset + 1}–${offset + limit}`
      }
      return path
    }

    case "write":
    case "edit":
      return shortPath(str(args.path))

    case "list": {
      const path = shortPath(str(args.path))
      const glob = str(args.glob)
      if (path && glob) return `${path}/${glob}`
      return path ?? glob
    }

    case "patch":
      return patchLabel(args.ops)

    default: {
      for (const key of FALLBACK_KEYS) {
        const value = oneLine(str(args[key]))
        if (value) return value
      }
      return undefined
    }
  }
}

/** "3 ops: add, update" — what the batch touched, without listing every path. */
function patchLabel(ops: JsonValue | undefined): string | undefined {
  if (!Array.isArray(ops) || ops.length === 0) return undefined
  const kinds = [...new Set(ops.map((op) => str(asObject(op)?.op)).filter(Boolean))]
  const count = `${ops.length} ${ops.length === 1 ? "op" : "ops"}`
  return kinds.length > 0 ? `${count}: ${kinds.join(", ")}` : count
}

function processLabel(process: JsonValue | undefined): string | undefined {
  const handle = num(process)
  return handle === undefined ? undefined : `#${handle}`
}

/**
 * The tail of a path, which is the readable part.
 *
 * Paths here are absolute inside the environment's root, and the meta line
 * clips its own overflow at the *end* — leaving it to CSS would hide the
 * filename and keep the leading directories nobody reads.
 */
function shortPath(path: string | undefined): string | undefined {
  if (!path) return undefined
  const segments = path.split("/").filter(Boolean)
  if (segments.length <= 2) return path
  return `…/${segments.slice(-2).join("/")}`
}

/** A command or a blob of stdin, flattened to something a single line can hold. */
function oneLine(text: string | undefined, max = 120): string | undefined {
  if (!text) return undefined
  const flat = text.replace(/\s+/g, " ").trim()
  if (!flat) return undefined
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat
}

function asObject(value: unknown): Record<string, JsonValue> | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined
  return value as Record<string, JsonValue>
}

function str(value: JsonValue | undefined): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined
}

function num(value: JsonValue | undefined): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined
}
