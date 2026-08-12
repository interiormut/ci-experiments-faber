//! The standard environment tool surface.
//!
//! `crates/environment` deliberately holds no JSON schema, no tool name, and
//! no rendering policy: the tools are a *projection* of its contract, the
//! projection is per model family, and the whole point of the split is that
//! adding a family costs a formatting decision rather than a second
//! implementation. This module is the first such projection, and it ships as
//! the general-purpose one — a harness gets a working environment surface by
//! being granted it, not by writing tools of its own.
//!
//! What the projection must preserve, and does:
//!
//! - A nonzero exit code and a timeout are **results**. `is_error` is true for
//!   a fault and for nothing else. There is no exit-code allowlist here and
//!   there is no reason for one: the two are distinct in the type, so nothing
//!   has to be classified back apart by a table of command names.
//! - **Invalid input never comes back as an empty result.** A missing
//!   parameter, a parameter of the wrong type, and a genuinely empty answer are
//!   three different things and never share a rendering.
//! - **The target is required and never defaulted** — see [`schema`].
//! - **The agent cannot bind.** There is no bind, unbind, or discover verb, and
//!   `targets` enumerates what is bound rather than what exists on the machine.
//!   Binding is the user's, and it reaches the run as data.
//!
//! The registry is read-only here on purpose. Bindings are established by the
//! consumer before the run and appended as events; nothing the model calls can
//! add one, so this side never needs to mutate it.

pub mod render;
pub mod schema;

use std::sync::Arc;
use std::time::Duration;

use environment::{
    Blob, Blobs, Cursor, Edit, Exec, Label, Patch, PatchOp, ProcId, Registry, Replace, RootedPath,
    Signal, Target, Window,
};
use futures_util::FutureExt;
use serde_json::Value;

use crate::mapping::ToolResult;
use crate::state::ToolInvoker;

/// One projection of the environment contract, bound to a set of targets.
///
/// Holds the same blob store the targets were built with, because a `Span` is
/// a handle and the model reads text: output that never reaches the prefix
/// still has to be redeemable by whoever renders the result.
pub struct Surface {
    registry: Arc<Registry>,
    blobs: Arc<dyn Blobs>,
}

impl Surface {
    pub fn new(registry: Arc<Registry>, blobs: Arc<dyn Blobs>) -> Self {
        Surface { registry, blobs }
    }

    /// The tool definitions. A constant — see [`schema::definitions`].
    pub fn definitions() -> Vec<llm::ToolDef> {
        schema::definitions()
    }

    /// The dispatcher, in the shape [`Grant`](crate::Grant) takes.
    pub fn invoker(self: Arc<Self>) -> ToolInvoker {
        Arc::new(move |name, input| {
            let surface = Arc::clone(&self);
            async move { Ok(surface.invoke(&name, input).await) }.boxed()
        })
    }

    /// Runs one call and renders whatever came back.
    ///
    /// Infallible by construction: everything a target can answer, including
    /// both failure classes, is a tool *result* the model reads and acts on.
    /// The dispatch-failed channel is reserved for an unknown tool name, which
    /// is a fact about the grant rather than about the environment.
    async fn invoke(&self, name: &str, input: Value) -> ToolResult {
        match self.route(name, input).await {
            Ok(content) => ToolResult {
                content,
                is_error: false,
            },
            Err(content) => ToolResult {
                content,
                is_error: true,
            },
        }
    }

    async fn route(&self, name: &str, input: Value) -> Result<String, String> {
        if name == "targets" {
            return Ok(render::manifests(&self.registry.manifests()));
        }

        let label = Label::from(string(&input, "target")?);
        let target = self
            .registry
            .target(&label)
            .map_err(|fault| render::fault(&fault))?;

        match name {
            "exec" => self.exec(target.as_ref(), &input).await,
            "start" => self.start(target.as_ref(), &input).await,
            "output" => self.output(target.as_ref(), &input).await,
            "stdin" => self.stdin(target.as_ref(), &input).await,
            "signal" => self.signal(target.as_ref(), &input).await,
            "read" => self.read(target.as_ref(), &input).await,
            "write" => self.write(target.as_ref(), &input).await,
            "edit" => self.edit(target.as_ref(), &input).await,
            "patch" => self.patch(target.as_ref(), &input).await,
            "list" => self.list(target.as_ref(), &input).await,
            other => Err(format!(
                "`{other}` is not one of this surface's tools; `targets` lists what is bound"
            )),
        }
    }

    // -----------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------

    async fn exec(&self, target: &dyn Target, input: &Value) -> Result<String, String> {
        let request = self.request(target, input)?;
        let exit = target.exec(request).await.map_err(fault)?;
        Ok(render::exit(&exit, self.blobs.as_ref()))
    }

    async fn start(&self, target: &dyn Target, input: &Value) -> Result<String, String> {
        let request = self.request(target, input)?;
        let id = target.start(request).await.map_err(fault)?;
        Ok(format!(
            "{}: started, process {}. Read it with `output`, using `process: {}`.",
            target.manifest().label,
            id,
            id.0
        ))
    }

    /// Assembles an [`Exec`] from a call. Shared by `exec` and `start`, which
    /// differ in what they do with it and in nothing else.
    fn request(&self, target: &dyn Target, input: &Value) -> Result<Exec, String> {
        let mut request = Exec::new(string(input, "command")?);

        if let Some(cwd) = optional_string(input, "cwd")? {
            request.cwd = Some(path(target, &cwd)?);
        }
        if let Some(timeout) = optional_u64(input, "timeout_ms")? {
            request.timeout = Duration::from_millis(timeout);
        }
        if let Some(stdin) = optional_string(input, "stdin")? {
            request.stdin = Some(Blob::from(stdin.as_bytes().to_vec()));
        }
        if let Some(env) = input.get("env").filter(|value| !value.is_null()) {
            let Some(pairs) = env.as_object() else {
                return Err("`env` must be an object of string values".to_owned());
            };
            for (key, value) in pairs {
                let Some(value) = value.as_str() else {
                    return Err(format!("`env.{key}` must be a string"));
                };
                request.env.push((key.clone(), value.to_owned()));
            }
        }
        Ok(request)
    }

    async fn output(&self, target: &dyn Target, input: &Value) -> Result<String, String> {
        let id = ProcId(u64(input, "process")?);
        let cursor = Cursor {
            stdout: optional_u64(input, "from_stdout")?.unwrap_or(0),
            stderr: optional_u64(input, "from_stderr")?.unwrap_or(0),
        };
        let chunk = target.output(id, cursor).await.map_err(fault)?;
        Ok(render::chunk(&chunk, self.blobs.as_ref()))
    }

    async fn stdin(&self, target: &dyn Target, input: &Value) -> Result<String, String> {
        let id = ProcId(u64(input, "process")?);
        let data = string(input, "data")?;
        target
            .stdin(id, &Blob::from(data.as_bytes().to_vec()))
            .await
            .map_err(fault)?;
        Ok(format!(
            "{}: wrote {} bytes to process {}'s stdin",
            target.manifest().label,
            data.len(),
            id.0
        ))
    }

    async fn signal(&self, target: &dyn Target, input: &Value) -> Result<String, String> {
        let id = ProcId(u64(input, "process")?);
        let name = string(input, "signal")?;
        let signal = match name.as_str() {
            "int" => Signal::Int,
            "term" => Signal::Term,
            "kill" => Signal::Kill,
            "hup" => Signal::Hup,
            "quit" => Signal::Quit,
            "usr1" => Signal::Usr1,
            "usr2" => Signal::Usr2,
            other => {
                return Err(format!(
                    "`{other}` is not a signal this surface sends; \
                     use int, term, kill, hup, quit, usr1, or usr2"
                ));
            }
        };
        target.signal(id, signal).await.map_err(fault)?;
        Ok(format!(
            "{}: sent {signal} to process {}",
            target.manifest().label,
            id.0
        ))
    }

    // -----------------------------------------------------------------------
    // Files
    // -----------------------------------------------------------------------

    async fn read(&self, target: &dyn Target, input: &Value) -> Result<String, String> {
        let where_ = path(target, &string(input, "path")?)?;

        // Half a window is a malformed request and is refused as one. Guessing
        // the other half — a default limit, or "from here to the end" — would
        // answer a question that was not asked, and the model would read the
        // answer as though it had been.
        let offset = optional_u64(input, "offset")?;
        let limit = optional_u64(input, "limit")?;
        let window = match (offset, limit) {
            (None, None) => None,
            (Some(offset), Some(limit)) => Some(Window::new(offset, limit)),
            _ => {
                return Err(
                    "`offset` and `limit` go together: pass both to read a window of \
                     lines, or neither to read the whole file"
                        .to_owned(),
                );
            }
        };

        let span = target.read(&where_, window).await.map_err(fault)?;
        Ok(render::read(
            target.manifest().label.as_str(),
            where_.as_str(),
            &span,
            self.blobs.as_ref(),
        ))
    }

    async fn write(&self, target: &dyn Target, input: &Value) -> Result<String, String> {
        let where_ = path(target, &string(input, "path")?)?;
        let content = string(input, "content")?;
        let stat = target
            .write(&where_, &Blob::from(content.into_bytes()))
            .await
            .map_err(fault)?;
        Ok(render::stat(target.manifest().label.as_str(), &stat))
    }

    async fn edit(&self, target: &dyn Target, input: &Value) -> Result<String, String> {
        let replace = self.replace(target, input)?;
        let stats = target.edit(&Edit::Replace(replace)).await.map_err(fault)?;
        Ok(render::stats(target.manifest().label.as_str(), &stats))
    }

    fn replace(&self, target: &dyn Target, input: &Value) -> Result<Replace, String> {
        Ok(Replace {
            path: path(target, &string(input, "path")?)?,
            old: string(input, "old")?,
            new: string(input, "new")?,
            all: optional_bool(input, "all")?.unwrap_or(false),
        })
    }

    async fn patch(&self, target: &dyn Target, input: &Value) -> Result<String, String> {
        let Some(ops) = input.get("ops").and_then(Value::as_array) else {
            return Err("`ops` is required and must be an array of operations".to_owned());
        };
        if ops.is_empty() {
            return Err("`ops` is empty; a patch with no operations changes nothing".to_owned());
        }

        let mut parsed = Vec::with_capacity(ops.len());
        for (index, op) in ops.iter().enumerate() {
            // The index is in the message because a patch is one call: without
            // it, "path is required" names none of the several paths.
            parsed.push(
                self.patch_op(target, op)
                    .map_err(|error| format!("ops[{index}]: {error}"))?,
            );
        }

        let stats = target
            .edit(&Edit::Patch(Patch::new(parsed)))
            .await
            .map_err(fault)?;
        Ok(render::stats(target.manifest().label.as_str(), &stats))
    }

    fn patch_op(&self, target: &dyn Target, op: &Value) -> Result<PatchOp, String> {
        match string(op, "op")?.as_str() {
            "add" => Ok(PatchOp::Add {
                path: path(target, &string(op, "path")?)?,
                body: string(op, "content")?.into_bytes(),
            }),
            "update" => Ok(PatchOp::Update(self.replace(target, op)?)),
            "delete" => Ok(PatchOp::Delete {
                path: path(target, &string(op, "path")?)?,
            }),
            "move" => Ok(PatchOp::Move {
                from: path(target, &string(op, "from")?)?,
                to: path(target, &string(op, "to")?)?,
            }),
            other => Err(format!(
                "`{other}` is not an operation; use add, update, delete, or move"
            )),
        }
    }

    async fn list(&self, target: &dyn Target, input: &Value) -> Result<String, String> {
        let where_ = path(target, &string(input, "path")?)?;
        let glob = optional_string(input, "glob")?;
        let listing = target.list(&where_, glob.as_deref()).await.map_err(fault)?;
        Ok(render::listing(
            target.manifest().label.as_str(),
            where_.as_str(),
            &listing,
        ))
    }
}

/// Resolves an agent-supplied path against the target's root.
fn path(target: &dyn Target, supplied: &str) -> Result<RootedPath, String> {
    target.path(supplied).map_err(|fault| render::fault(&fault))
}

fn fault(error: environment::Fault) -> String {
    render::fault(&error)
}

// ---------------------------------------------------------------------------
// Reading a call's arguments
// ---------------------------------------------------------------------------
//
// Every one of these names the parameter and says what was wrong with it. A
// tool that answers a malformed call with an empty result teaches the model
// that the environment had nothing to say, which is a settled negative answer
// to a question it never actually asked.

fn string(input: &Value, key: &str) -> Result<String, String> {
    match input.get(key) {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(Value::Null) | None => Err(format!("`{key}` is required")),
        Some(other) => Err(format!("`{key}` must be a string; got {}", kind_of(other))),
    }
}

fn optional_string(input: &Value, key: &str) -> Result<Option<String>, String> {
    match input.get(key) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(other) => Err(format!("`{key}` must be a string; got {}", kind_of(other))),
    }
}

fn u64(input: &Value, key: &str) -> Result<u64, String> {
    match optional_u64(input, key)? {
        Some(value) => Ok(value),
        None => Err(format!("`{key}` is required")),
    }
}

fn optional_u64(input: &Value, key: &str) -> Result<Option<u64>, String> {
    match input.get(key) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::Number(number)) => number.as_u64().map(Some).ok_or_else(|| {
            format!("`{key}` must be a whole number that is not negative; got {number}")
        }),
        Some(other) => Err(format!(
            "`{key}` must be a whole number; got {}",
            kind_of(other)
        )),
    }
}

fn optional_bool(input: &Value, key: &str) -> Result<Option<bool>, String> {
    match input.get(key) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(format!(
            "`{key}` must be true or false; got {}",
            kind_of(other)
        )),
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use environment::{LocalTarget, MemoryBlobs, Root};
    use serde_json::json;

    use super::*;

    fn empty_surface() -> Surface {
        Surface::new(Arc::new(Registry::new()), Arc::new(MemoryBlobs::new()))
    }

    /// A surface with one real local target rooted at a fresh directory.
    async fn bound_surface(root: &std::path::Path) -> Surface {
        let blobs: Arc<dyn Blobs> = Arc::new(MemoryBlobs::new());
        let machine = LocalTarget::bind(
            "build",
            Root::new(root.to_str().expect("a utf-8 temp path")).expect("a root"),
            Arc::clone(&blobs),
        )
        .await
        .expect("binding a local target");

        let mut registry = Registry::new();
        registry
            .bind("build", Arc::new(machine))
            .expect("a fresh label");
        Surface::new(Arc::new(registry), blobs)
    }

    /// A directory of this test's own, removed when the test ends.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("faber-tools-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("a temp directory");
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn a_call_against_a_label_nothing_bound_is_not_bound() {
        let surface = empty_surface();
        let result = surface
            .invoke("exec", json!({ "target": "build", "command": "true" }))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not_bound"));
    }

    #[tokio::test]
    async fn a_missing_target_is_a_named_validation_error_rather_than_an_empty_answer() {
        let surface = empty_surface();
        let result = surface.invoke("exec", json!({ "command": "true" })).await;
        assert!(result.is_error);
        assert!(result.content.contains("`target` is required"));
        assert!(!result.content.is_empty());
    }

    #[tokio::test]
    async fn a_parameter_of_the_wrong_type_says_which_one_and_what_it_got() {
        let dir = TempDir::new("wrong-type");
        let surface = bound_surface(&dir.0).await;
        let result = surface
            .invoke("exec", json!({ "target": "build", "command": 7 }))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("`command` must be a string"));
        assert!(result.content.contains("a number"));
    }

    #[tokio::test]
    async fn half_a_window_is_refused_rather_than_completed_by_guesswork() {
        let dir = TempDir::new("half-window");
        let surface = bound_surface(&dir.0).await;
        let result = surface
            .invoke(
                "read",
                json!({ "target": "build", "path": "/a", "offset": 40 }),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("go together"));
    }

    #[tokio::test]
    async fn a_command_that_fails_is_a_result_and_not_an_error() {
        let dir = TempDir::new("nonzero");
        let surface = bound_surface(&dir.0).await;
        let result = surface
            .invoke(
                "exec",
                json!({ "target": "build", "command": "echo out; echo err >&2; exit 3" }),
            )
            .await;

        // The whole of the no-allowlist position, in one assertion: the
        // command ran, so this is something the model reads and acts on.
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("exit 3"));
        // And the resolved target and cwd are echoed rather than implied.
        assert!(result.content.contains("build:/"));
        assert!(result.content.contains("out"));
        assert!(result.content.contains("err"));
    }

    #[tokio::test]
    async fn a_path_that_leaves_the_root_is_denied_with_the_class_named() {
        let dir = TempDir::new("escape");
        let surface = bound_surface(&dir.0).await;
        let result = surface
            .invoke(
                "read",
                json!({ "target": "build", "path": "/../etc/passwd" }),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("path_escape"));
    }

    #[tokio::test]
    async fn a_written_file_reads_back_through_the_same_surface() {
        let dir = TempDir::new("roundtrip");
        let surface = bound_surface(&dir.0).await;

        let written = surface
            .invoke(
                "write",
                json!({ "target": "build", "path": "/notes.txt", "content": "one\ntwo\n" }),
            )
            .await;
        assert!(!written.is_error, "{}", written.content);
        assert!(written.content.contains("build:/notes.txt"));

        let read = surface
            .invoke("read", json!({ "target": "build", "path": "/notes.txt" }))
            .await;
        assert!(!read.is_error, "{}", read.content);
        assert!(read.content.contains("two"));

        let edited = surface
            .invoke(
                "edit",
                json!({ "target": "build", "path": "/notes.txt", "old": "two", "new": "three" }),
            )
            .await;
        assert!(!edited.is_error, "{}", edited.content);

        let listed = surface
            .invoke(
                "list",
                json!({ "target": "build", "path": "/", "glob": "*.txt" }),
            )
            .await;
        assert!(!listed.is_error, "{}", listed.content);
        assert!(listed.content.contains("notes.txt"));
    }

    #[tokio::test]
    async fn a_rejected_glob_is_never_reported_as_an_empty_directory() {
        let dir = TempDir::new("bad-glob");
        let surface = bound_surface(&dir.0).await;
        let result = surface
            .invoke(
                "list",
                json!({ "target": "build", "path": "/", "glob": "src/*" }),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("bad_pattern"));
        assert!(!result.content.contains("empty"));
    }

    #[tokio::test]
    async fn an_ambiguous_edit_is_refused_rather_than_guessed_at() {
        let dir = TempDir::new("ambiguous");
        let surface = bound_surface(&dir.0).await;
        surface
            .invoke(
                "write",
                json!({ "target": "build", "path": "/twice.txt", "content": "a\na\n" }),
            )
            .await;

        let refused = surface
            .invoke(
                "edit",
                json!({ "target": "build", "path": "/twice.txt", "old": "a", "new": "b" }),
            )
            .await;
        assert!(refused.is_error);
        assert!(refused.content.contains("edit_refused"));

        let deliberate = surface
            .invoke(
                "edit",
                json!({
                    "target": "build", "path": "/twice.txt",
                    "old": "a", "new": "b", "all": true
                }),
            )
            .await;
        assert!(!deliberate.is_error, "{}", deliberate.content);
    }

    #[tokio::test]
    async fn targets_answers_on_an_empty_registry_without_pretending_it_can_bind_one() {
        let surface = empty_surface();
        let result = surface.invoke("targets", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("No environments are bound"));
        // The surface must not offer a way out of this that the agent does not
        // have: binding is the user's, and there is no verb for it here.
        assert!(
            !Surface::definitions()
                .iter()
                .any(|tool| { matches!(tool.name.as_str(), "bind" | "unbind" | "discover") })
        );
    }

    #[tokio::test]
    async fn targets_publishes_the_manifest_the_model_has_to_reason_about() {
        let dir = TempDir::new("manifest");
        let surface = bound_surface(&dir.0).await;
        let result = surface.invoke("targets", json!({})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("build"));
        assert!(result.content.contains("root"));
        // A local target holds its root by convention alone, and the model is
        // told so rather than left to find out by walking out of it.
        assert!(result.content.contains("held by this API alone"));
    }
}
