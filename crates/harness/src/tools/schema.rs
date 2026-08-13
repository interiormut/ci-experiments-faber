//! The tool definitions, as bytes in the prefix.
//!
//! Two properties this file exists to hold still.
//!
//! **The schema is a constant.** It is not built from the bindings, not
//! filtered by what any target can do, and not rebuilt when a target is bound
//! mid-session. Tool definitions sit near the top of the request and every
//! byte after them is cached against them, so a schema that varied with the
//! binding set would invalidate the whole prefix each time a user added an
//! environment — and would make binding mid-run impossible rather than merely
//! expensive. Capability lives in the manifest, which is data at the end of
//! the context, and a call against a target that cannot answer it is denied
//! with the capability named.
//!
//! **`target` is required on every call and never defaulted.** It is a plain
//! string, not an enum of bound labels — an enum would put the bindings back
//! into the schema through the side door. A defaulted target means a forgotten
//! parameter silently routes to a machine the model was not thinking about,
//! and the failures that produces are the destructive ones: an `rm -rf`, a
//! migration, a service restart on the wrong host. An omitted required
//! parameter is a validation error the model repairs in one turn.

use serde_json::{Value, json};

/// The label of the target a call runs against. Repeated on every tool
/// because every tool needs it and none of them may infer it.
fn target_property() -> Value {
    json!({
        "type": "string",
        "description": "The label of the bound environment to run against. \
                        Required. There is no default and no current target: \
                        call `targets` to see what is bound.",
    })
}

fn tool(name: &str, description: &str, mut properties: Value, required: &[&str]) -> llm::ToolDef {
    if let Value::Object(fields) = &mut properties {
        fields.insert("target".to_owned(), target_property());
    }
    let mut required: Vec<&str> = required.to_vec();
    required.insert(0, "target");

    llm::ToolDef {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        }),
    }
}

/// The whole surface, in a fixed order.
///
/// Order is load-bearing: a reordered tool array hashes differently and reads
/// as a changed prefix, so this list is appended to and never rearranged.
pub fn definitions() -> Vec<llm::ToolDef> {
    vec![
        targets(),
        exec(),
        start(),
        output(),
        stdin(),
        signal(),
        read(),
        write(),
        edit(),
        patch(),
        list(),
    ]
}

/// The only tool with no `target`, because it is the one that answers what the
/// targets are.
fn targets() -> llm::ToolDef {
    llm::ToolDef {
        name: "targets".to_owned(),
        description: "List the bound environments and what each one is: os, arch, shell, \
                      root, which verbs it answers, which tools were found on it, whether \
                      it can reach the network, and whether its root is enforced by a \
                      container or only by this API. Enumerates what is bound — never \
                      what exists on the machine."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        }),
    }
}

fn exec() -> llm::ToolDef {
    tool(
        "exec",
        "Run a shell command in an environment and wait for it to finish.\n\n\
         A nonzero exit code is a result, not a failure: the command ran and told you \
         something. Only a refused or unreachable environment is an error. A command that \
         hits its timeout is also a result — it ran, it did not stop, and what it printed \
         first is returned.\n\n\
         `cwd` is per call and never persists. `cd` inside the command string affects that \
         command only; the next call starts from `cwd` or the root again. Large output is \
         also written to a file inside the environment, and the result names the path so \
         you can grep it rather than re-read it.",
        json!({
            "command": {
                "type": "string",
                "description": "The command line, run through the environment's shell.",
            },
            "cwd": {
                "type": "string",
                "description": "Absolute virtual path inside the environment's root. Do not \
                                 prefix it with the root shown by `targets` (for example, use \
                                 `/src/main.rs`, not `/workspace/src/main.rs`). Defaults to the \
                                 root. Applies to this call only.",
            },
            "timeout_ms": {
                "type": "integer",
                "description": "How long to wait before killing the command and returning \
                                what it produced. Defaults to 120000.",
                "minimum": 1,
            },
            "env": {
                "type": "object",
                "description": "Extra environment variables for this call, layered over \
                                the environment's own.",
                "additionalProperties": { "type": "string" },
            },
            "stdin": {
                "type": "string",
                "description": "Fed to the command's stdin, which is then closed.",
            },
        }),
        &["command"],
    )
}

fn start() -> llm::ToolDef {
    tool(
        "start",
        "Start a command without waiting for it, and return a handle. Read what it has \
         produced with `output`, write to it with `stdin`, and stop it with `signal`. \
         Handles live as long as the connection to the environment does; if the machine \
         goes away the handle goes with it and the work has to be restarted.",
        json!({
            "command": { "type": "string" },
            "cwd": {
                "type": "string",
                "description": "Absolute virtual path inside the environment's root. Do not \
                                 prefix it with the root shown by `targets` (for example, use \
                                 `/src/main.rs`, not `/workspace/src/main.rs`). Defaults to the \
                                 root.",
            },
            "env": {
                "type": "object",
                "additionalProperties": { "type": "string" },
            },
            "stdin": {
                "type": "string",
                "description": "Written to stdin before the process is handed back. \
                                Unlike `exec`, stdin stays open.",
            },
        }),
        &["command"],
    )
}

fn output() -> llm::ToolDef {
    tool(
        "output",
        "Read what a started process has produced since a cursor, and get back the cursor \
         to resume from. A process that is still running says so; once it has ended, the \
         result carries how it ended and further reads only drain what is left.",
        json!({
            "process": {
                "type": "integer",
                "description": "The handle `start` returned.",
                "minimum": 0,
            },
            "from_stdout": {
                "type": "integer",
                "description": "Byte offset to resume stdout from. Defaults to 0.",
                "minimum": 0,
            },
            "from_stderr": {
                "type": "integer",
                "description": "Byte offset to resume stderr from. Defaults to 0.",
                "minimum": 0,
            },
        }),
        &["process"],
    )
}

fn stdin() -> llm::ToolDef {
    tool(
        "stdin",
        "Write to a started process's stdin — answer a prompt, drive a REPL. Not every \
         environment offers this, and one that does not says so in `targets` rather than \
         swallowing the write.",
        json!({
            "process": { "type": "integer", "minimum": 0 },
            "data": {
                "type": "string",
                "description": "Written as-is. Include the trailing newline yourself if \
                                the process is waiting for one.",
            },
        }),
        &["process", "data"],
    )
}

fn signal() -> llm::ToolDef {
    tool(
        "signal",
        "Send a signal to a started process. Process lifecycle only — there is no verb \
         here that starts, stops, or removes an environment itself.",
        json!({
            "process": { "type": "integer", "minimum": 0 },
            "signal": {
                "type": "string",
                "enum": ["int", "term", "kill", "hup", "quit", "usr1", "usr2"],
            },
        }),
        &["process", "signal"],
    )
}

fn read() -> llm::ToolDef {
    tool(
        "read",
        "Read a file, or a window of lines from one. A missing file, a window past the end \
         of the file, and a file that is genuinely empty are three different answers and \
         never look alike. A result that was cut short says so.",
        json!({
            "path": {
                "type": "string",
                "description": "Absolute virtual path inside the environment's root. Do not \
                                prefix it with the root shown by `targets` (for example, use \
                                `/src/main.rs`, not `/workspace/src/main.rs`).",
            },
            "offset": {
                "type": "integer",
                "description": "0-based first line. Requires `limit`.",
                "minimum": 0,
            },
            "limit": {
                "type": "integer",
                "description": "How many lines to read from `offset`.",
                "minimum": 1,
            },
        }),
        &["path"],
    )
}

fn write() -> llm::ToolDef {
    tool(
        "write",
        "Write a file whole, creating it if it does not exist and replacing it if it does. \
         To change part of a file, use `edit` — rewriting a large file to change one line \
         is slower, loses anything you had not read, and is harder to review.",
        json!({
            "path": {
                "type": "string",
                "description": "Absolute virtual path inside the environment's root. Do not \
                                prefix it with the root shown by `targets` (for example, use \
                                `/src/main.rs`, not `/workspace/src/main.rs`).",
            },
            "content": { "type": "string" },
        }),
        &["path", "content"],
    )
}

fn edit() -> llm::ToolDef {
    tool(
        "edit",
        "Replace exact text in a file. The anchor must appear exactly once, or the edit is \
         refused rather than guessed at — pass `all` to replace every occurrence \
         deliberately. A refusal is about the request, so change the anchor rather than \
         retrying it.",
        json!({
            "path": {
                "type": "string",
                "description": "Absolute virtual path inside the environment's root. Do not \
                                prefix it with the root shown by `targets` (for example, use \
                                `/src/main.rs`, not `/workspace/src/main.rs`).",
            },
            "old": {
                "type": "string",
                "description": "Text to find. Must match exactly, whitespace included.",
            },
            "new": { "type": "string" },
            "all": {
                "type": "boolean",
                "description": "Replace every occurrence. Without it, more than one match \
                                is a refusal.",
            },
        }),
        &["path", "old", "new"],
    )
}

fn patch() -> llm::ToolDef {
    tool(
        "patch",
        "Apply a set of file operations — create, replace-in-file, delete, rename — as one \
         request. Operations run in order and are NOT atomic: if one fails, the ones before \
         it stay applied, and the result says exactly which those were.",
        json!({
            "ops": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "properties": {
                        "op": {
                            "type": "string",
                            "enum": ["add", "update", "delete", "move"],
                        },
                        "path": {
                            "type": "string",
                            "description": "Absolute virtual path inside the environment's root; \
                                            do not prefix it with the root shown by `targets`. \
                                            For add, update, and delete.",
                        },
                        "content": { "type": "string", "description": "For add." },
                        "old": { "type": "string", "description": "For update." },
                        "new": { "type": "string", "description": "For update." },
                        "all": { "type": "boolean", "description": "For update." },
                        "from": {
                            "type": "string",
                            "description": "Absolute virtual source path; do not prefix it with \
                                            the root shown by `targets`. For move.",
                        },
                        "to": {
                            "type": "string",
                            "description": "Absolute virtual destination path; do not prefix it \
                                            with the root shown by `targets`. For move.",
                        },
                    },
                    "required": ["op"],
                    "additionalProperties": false,
                },
            },
        }),
        &["ops"],
    )
}

fn list() -> llm::ToolDef {
    tool(
        "list",
        "List one directory, optionally filtered by a glob over the entry names. \
         Non-recursive: a glob matches names in that directory and cannot contain `/`. \
         A pattern the matcher rejects is reported as a rejected pattern, never as an \
         empty directory. A listing that hit the cap says so — narrow it rather than \
         reading it as complete.",
        json!({
            "path": {
                "type": "string",
                "description": "Absolute virtual path inside the environment's root. Do not \
                                prefix it with the root shown by `targets` (for example, use \
                                `/src/main.rs`, not `/workspace/src/main.rs`).",
            },
            "glob": {
                "type": "string",
                "description": "Shell-style: `*`, `?`, `[abc]`, `[!abc]`.",
            },
        }),
        &["path"],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_but_targets_requires_a_target_it_cannot_default() {
        for tool in definitions() {
            let required = tool.input_schema["required"].as_array().cloned();
            if tool.name == "targets" {
                assert!(required.is_none(), "`targets` takes no parameters");
                continue;
            }
            let required = required.expect("every other tool has required parameters");
            assert!(
                required.iter().any(|name| name == "target"),
                "`{}` must require a target",
                tool.name
            );
            let property = &tool.input_schema["properties"]["target"];
            // A plain string, never an enum: an enum of bound labels would put
            // the binding set back into the prefix, which is exactly what
            // makes mid-session binding cheap to avoid.
            assert_eq!(property["type"], "string");
            assert!(property.get("enum").is_none(), "`{}`", tool.name);
        }
    }

    #[test]
    fn the_schema_does_not_depend_on_what_is_bound() {
        // Nothing to pass in — the definitions are a constant. This test is
        // the statement of that, and it fails to compile the day someone
        // gives `definitions` a parameter.
        assert_eq!(definitions().len(), definitions().len());
        assert!(definitions().iter().any(|tool| tool.name == "exec"));
    }
}
