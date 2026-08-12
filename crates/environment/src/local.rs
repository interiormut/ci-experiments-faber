//! Direct execution on this machine — the `(local, direct)` mode.
//!
//! The mode where the root boundary is load-bearing and unbacked: nothing underneath enforces
//! the root, so escape is refused here or not at all. Two checks, because the
//! lexical one in [`RootedPath`] cannot see a symlink:
//!
//! 1. [`RootedPath::new`] refuses `..`, `~`, and relative paths lexically.
//! 2. [`LocalTarget::confine`] canonicalizes and re-checks before every
//!    filesystem operation, which is what catches a symlink inside the root
//!    pointing out of it.
//!
//! What it does *not* catch: a shell command. `sh -c 'cat ../../etc/passwd'`
//! runs, because the process is the user's own and the API is not a sandbox.
//! That is precisely the posture this target publishes —
//! [`Posture::Conventional`] — and publishing it is the honest half of the contract.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;

use crate::exec::{Chunk, Cursor, Exec, Exit, Outcome, ProcId, Signal, Stream};
use crate::fault::{Denial, Fault};
use crate::file::{
    Edit, Entry, EntryKind, LIST_CAP, Listing, Patch, PatchOp, Replace, Stat, Window,
};
use crate::manifest::{Capability, Manifest, Posture, Reachability, Scope};
use crate::path::{Root, RootedPath};
use crate::registry::Label;
use crate::store::{Blob, Blobs, Span};
use crate::target::Target;

/// Bytes of one stream kept in the blob store. Past this the capture is
/// flagged truncated (A5) and the whole of it goes to a spill file (A2).
pub const CAPTURE_CAP: usize = 256 * 1024;

/// Output this size or larger also lands at a path inside the target, because
/// a span cannot be `rg`'d and a path can.
pub const SPILL_THRESHOLD: usize = 64 * 1024;

/// Where spill files go, relative to the root.
///
    /// Lifecycle cleanup is unresolved and this implementation does not pretend otherwise:
    /// nothing deletes these. Faber owns no lifecycle, so a long run leaves
/// logs behind, under one predictable directory rather than scattered.
pub const SPILL_DIR: &str = "/.faber/logs";

/// Binaries probed at bind. Per-binary capability is manifest data, never tool
    /// presence — there is no `git` verb on [`Target`], only a `git` line in
/// the manifest.
const PROBED_TOOLS: &[&str] = &["git", "rg", "cargo", "node", "python3"];

/// A target that runs commands in this process's own machine and filesystem.
pub struct LocalTarget {
    manifest: Manifest,
    /// The canonicalized root, for the symlink re-check.
    real_root: PathBuf,
    blobs: Arc<dyn Blobs>,
    processes: Mutex<BTreeMap<ProcId, Process>>,
    next_id: AtomicU64,
    /// Distinguishes this binding's spill files from an earlier binding's over
    /// the same root. Call ids restart at 1 per bind, and nothing deletes a
    /// spill file — without this, a path recorded in an old transcript
    /// would later resolve to different bytes, which is the one property A2
    /// exists to provide.
    bind_nonce: u64,
}

/// A background process and everything read from it so far.
struct Process {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    pid: Option<u32>,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    outcome: Option<Outcome>,
}

impl LocalTarget {
    /// Probes and binds. The probe happens once, here, and the manifest it
    /// produces is frozen for the life of the binding.
    pub async fn bind(
        label: impl Into<Label>,
        root: Root,
        blobs: Arc<dyn Blobs>,
    ) -> Result<Self, Fault> {
        let label = label.into();
        let real_root = fs::canonicalize(root.as_str()).await.map_err(|error| {
            Fault::Denied(Denial::NotFound {
                path: format!("{root}: {error}"),
            })
        })?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());

        let mut tools = BTreeMap::new();
        for tool in PROBED_TOOLS {
            if let Some(version) = probe_tool(tool).await {
                tools.insert((*tool).to_owned(), version);
            }
        }

        // No `Capability::Pty`: background stdin here is a pipe, so a program
        // that opens `/dev/tty` or checks `isatty` will not prompt into it.
        // Advertising it would hand the agent a capability it can only
        // disprove by a write that goes nowhere.
        let capabilities = BTreeSet::from([
            Capability::Exec,
            Capability::Background,
            Capability::Stdin,
            Capability::Read,
            Capability::Write,
            Capability::Edit,
            Capability::Patch,
            Capability::List,
        ]);

        // A7: rc files can see an agent is driving and skip fancy prompts.
        let agent_env = BTreeMap::from([
            ("FABER_AGENT".to_owned(), "1".to_owned()),
            ("FABER_TARGET".to_owned(), label.0.clone()),
        ]);

        let manifest = Manifest {
            label,
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            shell,
            root,
            tools,
            capabilities,
            scope: Scope::Workspace,
            // Nothing probed the network, and asserting either way gets
            // believed.
            network: Reachability::Unknown,
            // Direct on a host: this API refuses escape and nothing under it
            // does.
            posture: Posture::Conventional,
            agent_env,
            // Commands run through `sh -c`, not a login shell. A7's other
            // half: say so rather than let an absent alias become a mystery.
            login_shell_sourced: false,
            probed_at: SystemTime::now(),
        };

        let bind_nonce = manifest
            .probed_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos() as u64);

        Ok(LocalTarget {
            manifest,
            real_root,
            blobs,
            processes: Mutex::new(BTreeMap::new()),
            next_id: AtomicU64::new(1),
            bind_nonce,
        })
    }

    /// Resolves a rooted path to a real one, refusing anything that leaves the
    /// root after symlink resolution.
    ///
    /// The path need not exist — the deepest existing ancestor is
    /// canonicalized and the remainder appended — so `write` to a new file is
    /// confined by the same check as `read` of an existing one.
    async fn confine(&self, path: &RootedPath) -> Result<PathBuf, Fault> {
        let mut existing = PathBuf::from(path.resolved());
        let mut trailing: Vec<std::ffi::OsString> = Vec::new();

        let real = loop {
            match fs::canonicalize(&existing).await {
                Ok(real) => break real,
                Err(_) => {
                    let Some(name) = existing.file_name().map(ToOwned::to_owned) else {
                        return Err(escape(
                            path,
                            "path does not resolve inside the target's root",
                        ));
                    };
                    trailing.push(name);
                    existing.pop();
                }
            }
        };

        let mut resolved = real;
        for name in trailing.iter().rev() {
            resolved.push(name);
        }

        if !resolved.starts_with(&self.real_root) {
            return Err(escape(
                path,
                "resolves outside the target's root via a symlink",
            ));
        }
        Ok(resolved)
    }

    /// The cwd for a call: the request's, or the root (per-call, never
    /// persistent).
    async fn resolve_cwd(&self, req: &Exec) -> Result<(RootedPath, PathBuf), Fault> {
        let rooted = req.cwd.clone().unwrap_or_else(|| self.root());
        let real = self.confine(&rooted).await?;
        if !real.is_dir() {
            return Err(Fault::Denied(Denial::NotFound {
                path: rooted.to_string(),
            }));
        }
        Ok((rooted, real))
    }

    fn command(&self, req: &Exec, cwd: &Path) -> Command {
        let mut command = Command::new(&self.manifest.shell);
        command
            .arg("-c")
            .arg(&req.command)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in &self.manifest.agent_env {
            command.env(key, value);
        }
        // Additive overlay on the probed base, per-call and last.
        for (key, value) in &req.env {
            command.env(key, value);
        }
        command
    }

    /// Stores captured bytes, spilling to a path inside the target when they
    /// are large enough that the agent will want to grep rather than read
    /// (A2), and flagging the capture when it was capped (A5).
    async fn capture(&self, bytes: Vec<u8>, stream: &str, id: u64) -> Stream {
        let full = bytes.len();
        let spill = if full >= SPILL_THRESHOLD {
            self.spill(&bytes, stream, id).await
        } else {
            None
        };

        let kept = full.min(CAPTURE_CAP);
        let span = Span {
            blob: self.blobs.put(&bytes[..kept]),
            len: kept as u64,
            truncated: kept < full,
        };

        let mut captured = Stream::new(span);
        if let Some(path) = spill {
            captured = captured.spilled_to(path);
        }
        captured
    }

    async fn spill(&self, bytes: &[u8], stream: &str, id: u64) -> Option<RootedPath> {
        let path = RootedPath::new(
            &self.manifest.root,
            format!("{SPILL_DIR}/{:x}-{id}.{stream}", self.bind_nonce),
        )
        .ok()?;
        let real = self.confine(&path).await.ok()?;
        if let Some(parent) = real.parent() {
            fs::create_dir_all(parent).await.ok()?;
        }
        match fs::write(&real, bytes).await {
            Ok(()) => Some(path),
            Err(error) => {
                tracing::warn!(%path, %error, "could not spill captured output");
                None
            }
        }
    }

    fn allocate(&self) -> ProcId {
        ProcId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    async fn read_file(&self, path: &RootedPath) -> Result<Vec<u8>, Fault> {
        let real = self.confine(path).await?;
        fs::read(&real).await.map_err(|error| missing(path, &error))
    }

    async fn write_file(&self, path: &RootedPath, body: &[u8]) -> Result<Stat, Fault> {
        let real = self.confine(path).await?;
        if let Some(parent) = real.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|error| Fault::Unreachable(error.to_string()))?;
        }
        fs::write(&real, body)
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))?;
        stat(path, &real).await
    }

    async fn apply_replace(&self, replace: &Replace) -> Result<Stat, Fault> {
        let bytes = self.read_file(&replace.path).await?;
        let text = String::from_utf8(bytes).map_err(|_| {
            Fault::Denied(Denial::Malformed {
                what: "edit".into(),
                reason: format!("`{}` is not valid utf-8", replace.path),
            })
        })?;

        let matches = text.matches(&replace.old).count();
        // Both refusals are deterministic, so they are denials rather than
        // failed writes: the agent must change the request, not retry it.
        if matches == 0 {
            return Err(Fault::Denied(Denial::EditRefused {
                path: replace.path.to_string(),
                reason: "the anchor text does not appear in the file".to_owned(),
            }));
        }
        if matches > 1 && !replace.all {
            return Err(Fault::Denied(Denial::EditRefused {
                path: replace.path.to_string(),
                reason: format!(
                    "the anchor text appears {matches} times; pass `all` to replace every occurrence"
                ),
            }));
        }

        let edited = if replace.all {
            text.replace(&replace.old, &replace.new)
        } else {
            text.replacen(&replace.old, &replace.new, 1)
        };
        self.write_file(&replace.path, edited.as_bytes()).await
    }

    /// Applies a patch set **sequentially, without atomicity**.
    ///
    /// Cross-file atomicity is left open because the transport may not
    /// support it. Nothing here provides it: a failing op leaves the earlier
    /// ones applied, and the returned [`Stat`] list says exactly which those
    /// were.
    async fn apply_patch(&self, patch: &Patch) -> Result<Vec<Stat>, Fault> {
        let mut stats = Vec::with_capacity(patch.ops.len());
        for op in &patch.ops {
            match op {
                PatchOp::Add { path, body } => stats.push(self.write_file(path, body).await?),
                PatchOp::Update(replace) => stats.push(self.apply_replace(replace).await?),
                PatchOp::Delete { path } => {
                    let real = self.confine(path).await?;
                    fs::remove_file(&real)
                        .await
                        .map_err(|error| missing(path, &error))?;
                }
                PatchOp::Move { from, to } => {
                    let source = self.confine(from).await?;
                    let destination = self.confine(to).await?;
                    if let Some(parent) = destination.parent() {
                        fs::create_dir_all(parent)
                            .await
                            .map_err(|error| Fault::Unreachable(error.to_string()))?;
                    }
                    fs::rename(&source, &destination)
                        .await
                        .map_err(|error| missing(from, &error))?;
                    stats.push(stat(to, &destination).await?);
                }
            }
        }
        Ok(stats)
    }
}

#[async_trait]
impl Target for LocalTarget {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    async fn exec(&self, req: Exec) -> Result<Exit, Fault> {
        self.manifest.require(Capability::Exec)?;
        let (cwd, real_cwd) = self.resolve_cwd(&req).await?;
        let id = self.allocate().0;

        let mut child = self
            .command(&req, &real_cwd)
            .spawn()
            .map_err(|error| Fault::Unreachable(format!("could not spawn a shell: {error}")))?;

        if let Some(body) = &req.stdin
            && let Some(mut pipe) = child.stdin.take()
        {
            let _ = pipe.write_all(body.as_bytes()).await;
            let _ = pipe.shutdown().await;
        }
        drop(child.stdin.take());

        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let reading_stdout = tokio::spawn(async move { drain(&mut stdout).await });
        let reading_stderr = tokio::spawn(async move { drain(&mut stderr).await });

        let started = Instant::now();
        let status = match tokio::time::timeout(req.timeout, child.wait()).await {
            Ok(Ok(status)) => Some(status),
            Ok(Err(error)) => return Err(Fault::Unreachable(error.to_string())),
            Err(_) => {
                // The command ran; it just did not stop. Kill it, keep what it
                // produced, and report the outcome — not a fault.
                let _ = child.start_kill();
                let _ = child.wait().await;
                None
            }
        };
        let wall = started.elapsed();

        let out = reading_stdout.await.unwrap_or_default();
        let err = reading_stderr.await.unwrap_or_default();

        Ok(Exit {
            target: self.manifest.label.clone(),
            cwd,
            outcome: status.map_or(Outcome::TimedOut, outcome_of),
            stdout: self.capture(out, "stdout", id).await,
            stderr: self.capture(err, "stderr", id).await,
            wall,
        })
    }

    async fn start(&self, req: Exec) -> Result<ProcId, Fault> {
        self.manifest.require(Capability::Background)?;
        let (_, real_cwd) = self.resolve_cwd(&req).await?;

        let mut child = self
            .command(&req, &real_cwd)
            .spawn()
            .map_err(|error| Fault::Unreachable(format!("could not spawn a shell: {error}")))?;

        let mut stdin = child.stdin.take();
        if let Some(body) = &req.stdin
            && let Some(pipe) = stdin.as_mut()
        {
            let _ = pipe.write_all(body.as_bytes()).await;
        }

        let pid = child.id();
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        pump(child.stdout.take(), Arc::clone(&stdout));
        pump(child.stderr.take(), Arc::clone(&stderr));

        let id = self.allocate();
        self.processes.lock().await.insert(
            id,
            Process {
                child: Some(child),
                stdin,
                pid,
                stdout,
                stderr,
                outcome: None,
            },
        );
        Ok(id)
    }

    async fn output(&self, id: ProcId, from: Cursor) -> Result<Chunk, Fault> {
        let mut processes = self.processes.lock().await;
        let process = processes
            .get_mut(&id)
            .ok_or(Fault::Denied(Denial::NoSuchProcess(id)))?;

        // Reap without blocking: a still-running process leaves `outcome`
        // `None`, which is what tells the reader to come back.
        if process.outcome.is_none()
            && let Some(child) = process.child.as_mut()
        {
            match child.try_wait() {
                Ok(Some(status)) => {
                    process.outcome = Some(outcome_of(status));
                    process.child = None;
                }
                Ok(None) => {}
                Err(error) => return Err(Fault::Unreachable(error.to_string())),
            }
        }

        let (stdout, next_stdout) = self.slice(&process.stdout, from.stdout).await;
        let (stderr, next_stderr) = self.slice(&process.stderr, from.stderr).await;

        Ok(Chunk {
            target: self.manifest.label.clone(),
            stdout,
            stderr,
            next: Cursor {
                stdout: next_stdout,
                stderr: next_stderr,
            },
            outcome: process.outcome,
        })
    }

    async fn stdin(&self, id: ProcId, body: &Blob) -> Result<(), Fault> {
        self.manifest.require(Capability::Stdin)?;
        let mut processes = self.processes.lock().await;
        let process = processes
            .get_mut(&id)
            .ok_or(Fault::Denied(Denial::NoSuchProcess(id)))?;

        let pipe = process
            .stdin
            .as_mut()
            .ok_or(Fault::Denied(Denial::Malformed {
                what: "stdin".into(),
                reason: "this process's stdin is already closed".to_owned(),
            }))?;

        pipe.write_all(body.as_bytes())
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))?;
        pipe.flush()
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))
    }

    async fn signal(&self, id: ProcId, signal: Signal) -> Result<(), Fault> {
        let mut processes = self.processes.lock().await;
        let process = processes
            .get_mut(&id)
            .ok_or(Fault::Denied(Denial::NoSuchProcess(id)))?;

        // A reaped handle is spent, and its pid is free for the OS to hand to
        // someone else — signalling it would hit an unrelated process. The
        // denial is also the honest answer: there is nothing left to signal.
        if process.outcome.is_some() || process.child.is_none() {
            return Err(Fault::Denied(Denial::NoSuchProcess(id)));
        }

        #[cfg(unix)]
        {
            let Some(pid) = process.pid else {
                return Err(Fault::Denied(Denial::NoSuchProcess(id)));
            };
            // Safety: `kill(2)` with a pid this process spawned and has not
            // reaped. `kill_on_drop` keeps the handle alive until we do.
            let sent = unsafe { libc::kill(pid as libc::pid_t, signal.number()) };
            if sent != 0 {
                return Err(Fault::Unreachable(
                    std::io::Error::last_os_error().to_string(),
                ));
            }
            Ok(())
        }

        #[cfg(not(unix))]
        {
            let _ = signal;
            match process.child.as_mut() {
                Some(child) if matches!(signal, Signal::Kill | Signal::Term) => child
                    .start_kill()
                    .map_err(|error| Fault::Unreachable(error.to_string())),
                Some(_) => Err(Fault::Denied(Denial::Malformed {
                    what: "signal".into(),
                    reason: "this platform can only terminate a process".to_owned(),
                })),
                None => Err(Fault::Denied(Denial::NoSuchProcess(id))),
            }
        }
    }

    async fn read(&self, path: &RootedPath, window: Option<Window>) -> Result<Span, Fault> {
        self.manifest.require(Capability::Read)?;
        let bytes = self.read_file(path).await?;

        let Some(window) = window else {
            let kept = bytes.len().min(CAPTURE_CAP);
            return Ok(Span {
                blob: self.blobs.put(&bytes[..kept]),
                len: kept as u64,
                truncated: kept < bytes.len(),
            });
        };

        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
            // A window past the end is not an empty file.
        if window.offset > 0 && window.offset as usize >= lines.len() {
            return Err(Fault::Denied(Denial::OutOfRange {
                path: path.to_string(),
                offset: window.offset,
                len: lines.len() as u64,
            }));
        }

        let start = window.offset as usize;
        let end = (start + window.limit as usize).min(lines.len());
        let selected = lines[start..end].join("\n");

        Ok(Span {
            blob: self.blobs.put(selected.as_bytes()),
            len: selected.len() as u64,
            truncated: end < lines.len(),
        })
    }

    async fn write(&self, path: &RootedPath, body: &Blob) -> Result<Stat, Fault> {
        self.manifest.require(Capability::Write)?;
        self.write_file(path, body.as_bytes()).await
    }

    async fn edit(&self, edit: &Edit) -> Result<Vec<Stat>, Fault> {
        match edit {
            Edit::Replace(replace) => {
                self.manifest.require(Capability::Edit)?;
                Ok(vec![self.apply_replace(replace).await?])
            }
            Edit::Patch(patch) => {
                self.manifest.require(Capability::Patch)?;
                self.apply_patch(patch).await
            }
        }
    }

    async fn list(&self, path: &RootedPath, glob: Option<&str>) -> Result<Listing, Fault> {
        self.manifest.require(Capability::List)?;
        if let Some(pattern) = glob {
            // A rejected pattern is never reported as an empty result.
            validate_glob(pattern)?;
        }

        let real = self.confine(path).await?;
        let mut dir = fs::read_dir(&real)
            .await
            .map_err(|error| missing(path, &error))?;

        let mut entries = Vec::new();
        let mut truncated = false;
        while let Some(entry) = dir
            .next_entry()
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))?
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(pattern) = glob
                && !glob_matches(pattern, &name)
            {
                continue;
            }
            if entries.len() >= LIST_CAP {
                // A5: cap *and* flag. A capped listing the model reads as
                // complete ends the search.
                truncated = true;
                break;
            }

            let kind = match entry.file_type().await {
                Ok(kind) if kind.is_dir() => EntryKind::Dir,
                Ok(kind) if kind.is_file() => EntryKind::File,
                Ok(kind) if kind.is_symlink() => EntryKind::Symlink,
                Ok(_) => EntryKind::Other,
                Err(_) => EntryKind::Other,
            };
            let size = entry.metadata().await.ok().map(|meta| meta.len());
            entries.push(Entry {
                path: path.join(&name)?,
                kind,
                size,
            });
        }

        entries.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
        Ok(Listing { entries, truncated })
    }
}

impl LocalTarget {
    /// Bytes accumulated past `from`, plus the cursor to resume at.
    async fn slice(&self, buffer: &Arc<Mutex<Vec<u8>>>, from: u64) -> (Span, u64) {
        let buffer = buffer.lock().await;
        let start = (from as usize).min(buffer.len());
        let end = (start + CAPTURE_CAP).min(buffer.len());
        let span = Span {
            blob: self.blobs.put(&buffer[start..end]),
            len: (end - start) as u64,
            truncated: end < buffer.len(),
        };
        (span, end as u64)
    }
}

fn escape(path: &RootedPath, reason: &'static str) -> Fault {
    Fault::Denied(Denial::PathEscape {
        path: path.to_string(),
        reason: reason.into(),
    })
}

/// At the filesystem: a missing path is a denial naming the path, and
/// anything else is the transport's problem rather than the request's.
fn missing(path: &RootedPath, error: &std::io::Error) -> Fault {
    if error.kind() == std::io::ErrorKind::NotFound {
        Fault::Denied(Denial::NotFound {
            path: path.to_string(),
        })
    } else {
        Fault::Unreachable(error.to_string())
    }
}

async fn stat(path: &RootedPath, real: &Path) -> Result<Stat, Fault> {
    let meta = fs::metadata(real)
        .await
        .map_err(|error| missing(path, &error))?;
    Ok(Stat {
        path: path.clone(),
        size: meta.len(),
        modified: meta.modified().ok(),
    })
}

fn outcome_of(status: std::process::ExitStatus) -> Outcome {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(number) = status.signal()
            && let Some(signal) = Signal::from_number(number)
        {
            return Outcome::Signaled { signal };
        }
    }
    Outcome::Completed {
        code: status.code().unwrap_or(-1),
    }
}

async fn drain<R: tokio::io::AsyncRead + Unpin>(reader: &mut Option<R>) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(reader) = reader.as_mut() {
        let _ = reader.read_to_end(&mut bytes).await;
    }
    bytes
}

/// Reads a background stream into a buffer for [`Target::output`] to slice.
///
/// Unbounded on purpose-for-now: a background process producing gigabytes
/// grows this buffer without limit. Bounding it means deciding whether
/// background output spills the way [`Target::exec`]'s does (A2), which is not
/// settled.
fn pump<R>(reader: Option<R>, into: Arc<Mutex<Vec<u8>>>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let Some(mut reader) = reader else { return };
    tokio::spawn(async move {
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(read) => into.lock().await.extend_from_slice(&chunk[..read]),
            }
        }
    });
}

/// Glob evaluation is Faber-side: host-side is fast and image-dependent, and
/// image-dependent is the risk.
///
/// Non-recursive, so `*` does not cross a `/` — there is nothing to cross,
/// since [`Target::list`] matches against one directory's names.
fn glob_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    matches_from(&pattern, &name)
}

fn matches_from(pattern: &[char], name: &[char]) -> bool {
    match pattern.first() {
        None => name.is_empty(),
        Some('*') => (0..=name.len()).any(|split| matches_from(&pattern[1..], &name[split..])),
        Some('?') => !name.is_empty() && matches_from(&pattern[1..], &name[1..]),
        Some('[') => {
            let Some(close) = pattern.iter().position(|c| *c == ']') else {
                return false;
            };
            let (negated, set) = match pattern.get(1) {
                Some('!') => (true, &pattern[2..close]),
                _ => (false, &pattern[1..close]),
            };
            let Some(first) = name.first() else {
                return false;
            };
            (set.contains(first) != negated) && matches_from(&pattern[close + 1..], &name[1..])
        }
        Some(literal) => name.first() == Some(literal) && matches_from(&pattern[1..], &name[1..]),
    }
}

fn validate_glob(pattern: &str) -> Result<(), Fault> {
    if pattern.is_empty() {
        return Err(Fault::Denied(Denial::BadPattern {
            pattern: pattern.to_owned(),
            reason: "an empty pattern matches nothing; omit the glob instead".to_owned(),
        }));
    }
    let mut open = false;
    for character in pattern.chars() {
        match character {
            '[' if open => {
                return Err(Fault::Denied(Denial::BadPattern {
                    pattern: pattern.to_owned(),
                    reason: "nested `[`".to_owned(),
                }));
            }
            '[' => open = true,
            ']' => open = false,
            '/' => {
                return Err(Fault::Denied(Denial::BadPattern {
                    pattern: pattern.to_owned(),
                    reason: "`list` is non-recursive; a glob matches names in one directory"
                        .to_owned(),
                }));
            }
            _ => {}
        }
    }
    if open {
        return Err(Fault::Denied(Denial::BadPattern {
            pattern: pattern.to_owned(),
            reason: "unterminated `[`".to_owned(),
        }));
    }
    Ok(())
}

async fn probe_tool(tool: &str) -> Option<String> {
    let output = Command::new(tool).arg("--version").output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.lines().next()?.trim().to_owned())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::store::MemoryBlobs;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A scratch root, and a target bound to it.
    pub(crate) fn scratch() -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("faber-env-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    /// A target that skips the probe, for tests that only need a `dyn Target`.
    pub(crate) fn stub_target(label: &str) -> LocalTarget {
        let dir = scratch();
        let root = Root::new(dir.to_string_lossy()).unwrap();
        LocalTarget {
            manifest: Manifest {
                label: label.into(),
                os: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                shell: "/bin/sh".to_owned(),
                root,
                tools: BTreeMap::new(),
                capabilities: BTreeSet::from([
                    Capability::Exec,
                    Capability::Background,
                    Capability::Stdin,
                    Capability::Read,
                    Capability::Write,
                    Capability::Edit,
                    Capability::Patch,
                    Capability::List,
                ]),
                scope: Scope::Workspace,
                network: Reachability::Unknown,
                posture: Posture::Conventional,
                agent_env: BTreeMap::new(),
                login_shell_sourced: false,
                probed_at: SystemTime::now(),
            },
            real_root: dir,
            blobs: Arc::new(MemoryBlobs::new()),
            processes: Mutex::new(BTreeMap::new()),
            next_id: AtomicU64::new(1),
            bind_nonce: 0,
        }
    }

    async fn target() -> (LocalTarget, Arc<MemoryBlobs>) {
        let blobs = Arc::new(MemoryBlobs::new());
        let dir = scratch();
        let root = Root::new(dir.to_string_lossy()).unwrap();
        let target = LocalTarget::bind("work", root, blobs.clone() as Arc<dyn Blobs>)
            .await
            .unwrap();
        (target, blobs)
    }

    fn text(blobs: &MemoryBlobs, span: &Span) -> String {
        String::from_utf8(blobs.get(&span.blob).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_a_result_and_not_a_fault() {
        let (target, blobs) = target().await;
        let exit = target
            .exec(Exec::new("echo out; echo err >&2; exit 3"))
            .await
            .unwrap();

        assert_eq!(exit.outcome, Outcome::Completed { code: 3 });
        assert_eq!(text(&blobs, &exit.stdout.span).trim(), "out");
        assert_eq!(text(&blobs, &exit.stderr.span).trim(), "err");
    }

    #[tokio::test]
    async fn every_exit_echoes_its_target_and_resolved_cwd() {
        let (target, _) = target().await;
        let exit = target.exec(Exec::new("true")).await.unwrap();

        assert_eq!(exit.target.as_str(), "work");
        assert_eq!(exit.cwd.as_str(), "/");
    }

    #[tokio::test]
    async fn a_timeout_is_an_outcome_rather_than_a_fault() {
        let (target, _) = target().await;
        let exit = target
            .exec(Exec::new("sleep 30").timeout(std::time::Duration::from_millis(150)))
            .await
            .unwrap();

        assert_eq!(exit.outcome, Outcome::TimedOut);
    }

    #[tokio::test]
    async fn cwd_does_not_carry_between_calls() {
        let (target, blobs) = target().await;
        target
            .write(&target.path("/sub/marker").unwrap(), &"x".into())
            .await
            .unwrap();

        target.exec(Exec::new("cd /sub")).await.unwrap();
        let exit = target.exec(Exec::new("pwd")).await.unwrap();

        assert_eq!(
            text(&blobs, &exit.stdout.span).trim(),
            target.root().resolved()
        );
    }

    #[tokio::test]
    async fn a_path_leaving_the_root_through_a_symlink_is_denied() {
        let (target, _) = target().await;
        let outside = scratch().join("secret");
        std::fs::write(&outside, "s3cret").unwrap();
        std::os::unix::fs::symlink(
            &outside,
            PathBuf::from(target.root().resolved()).join("link"),
        )
        .unwrap();

        let path = target.path("/link").unwrap(); // lexically fine
        let fault = target.read(&path, None).await.unwrap_err();

        assert!(matches!(fault, Fault::Denied(Denial::PathEscape { .. })));
    }

    #[tokio::test]
    async fn reading_a_missing_file_is_not_an_empty_read() {
        let (target, _) = target().await;
        let fault = target
            .read(&target.path("/nope.txt").unwrap(), None)
            .await
            .unwrap_err();

        assert!(matches!(fault, Fault::Denied(Denial::NotFound { .. })));
    }

    #[tokio::test]
    async fn a_window_past_the_end_is_out_of_range_and_a_window_inside_is_flagged() {
        let (target, blobs) = target().await;
        let path = target.path("/lines.txt").unwrap();
        target.write(&path, &"a\nb\nc\nd\n".into()).await.unwrap();

        let span = target.read(&path, Some(Window::new(1, 2))).await.unwrap();
        assert_eq!(text(&blobs, &span), "b\nc");
        assert!(span.truncated);

        let fault = target
            .read(&path, Some(Window::new(99, 2)))
            .await
            .unwrap_err();
        assert!(matches!(fault, Fault::Denied(Denial::OutOfRange { .. })));
    }

    #[tokio::test]
    async fn an_ambiguous_edit_is_refused_rather_than_guessed_at() {
        let (target, blobs) = target().await;
        let path = target.path("/dup.txt").unwrap();
        target.write(&path, &"x\nx\n".into()).await.unwrap();

        let one = Edit::Replace(Replace::new(path.clone(), "x", "y"));
        let fault = target.edit(&one).await.unwrap_err();
        assert!(matches!(fault, Fault::Denied(Denial::EditRefused { .. })));

        let all = Edit::Replace(Replace::new(path.clone(), "x", "y").all());
        target.edit(&all).await.unwrap();
        let span = target.read(&path, None).await.unwrap();
        assert_eq!(text(&blobs, &span), "y\ny\n");
    }

    #[tokio::test]
    async fn an_absent_anchor_is_a_denial_not_a_silent_no_op() {
        let (target, _) = target().await;
        let path = target.path("/file.txt").unwrap();
        target.write(&path, &"hello\n".into()).await.unwrap();

        let edit = Edit::Replace(Replace::new(path, "goodbye", "hi"));
        assert!(matches!(
            target.edit(&edit).await.unwrap_err(),
            Fault::Denied(Denial::EditRefused { .. })
        ));
    }

    #[tokio::test]
    async fn a_patch_set_can_add_edit_move_and_delete() {
        let (target, blobs) = target().await;
        let first = target.path("/a.txt").unwrap();
        let second = target.path("/b.txt").unwrap();
        let moved = target.path("/nested/c.txt").unwrap();

        target.write(&second, &"old\n".into()).await.unwrap();
        let patch = Patch::new(vec![
            PatchOp::Add {
                path: first.clone(),
                body: b"one\n".to_vec(),
            },
            PatchOp::Update(Replace::new(second.clone(), "old", "new")),
            PatchOp::Move {
                from: second.clone(),
                to: moved.clone(),
            },
        ]);

        let stats = target.edit(&Edit::Patch(patch)).await.unwrap();
        assert_eq!(stats.len(), 3);
        assert_eq!(
            text(&blobs, &target.read(&first, None).await.unwrap()),
            "one\n"
        );
        assert_eq!(
            text(&blobs, &target.read(&moved, None).await.unwrap()),
            "new\n"
        );
        assert!(matches!(
            target.read(&second, None).await.unwrap_err(),
            Fault::Denied(Denial::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn a_rejected_glob_is_never_reported_as_an_empty_listing() {
        let (target, _) = target().await;
        let root = target.root();

        let fault = target.list(&root, Some("[abc")).await.unwrap_err();
        assert!(matches!(fault, Fault::Denied(Denial::BadPattern { .. })));

        let listing = target.list(&root, Some("*.rs")).await.unwrap();
        assert!(listing.entries.is_empty());
        assert!(!listing.truncated);
    }

    #[tokio::test]
    async fn a_listing_matches_faber_side_and_stays_in_one_directory() {
        let (target, _) = target().await;
        target
            .write(&target.path("/keep.rs").unwrap(), &"".into())
            .await
            .unwrap();
        target
            .write(&target.path("/skip.md").unwrap(), &"".into())
            .await
            .unwrap();
        target
            .write(&target.path("/deep/also.rs").unwrap(), &"".into())
            .await
            .unwrap();

        let listing = target.list(&target.root(), Some("*.rs")).await.unwrap();
        let names: Vec<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(names, vec!["/keep.rs"]);
    }

    #[tokio::test]
    async fn listing_a_missing_directory_is_not_found() {
        let (target, _) = target().await;
        let fault = target
            .list(&target.path("/nowhere").unwrap(), None)
            .await
            .unwrap_err();
        assert!(matches!(fault, Fault::Denied(Denial::NotFound { .. })));
    }

    #[tokio::test]
    async fn a_background_process_reads_forward_from_a_cursor_and_takes_stdin() {
        let (target, blobs) = target().await;
        let id = target
            .start(Exec::new("while read line; do echo \"got $line\"; done"))
            .await
            .unwrap();

        target.stdin(id, &"one\n".into()).await.unwrap();
        let mut cursor = Cursor::START;
        let first = loop {
            let chunk = target.output(id, cursor).await.unwrap();
            cursor = chunk.next;
            if chunk.stdout.len > 0 {
                break text(&blobs, &chunk.stdout);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        assert_eq!(first.trim(), "got one");

        target.signal(id, Signal::Term).await.unwrap();
    }

    #[tokio::test]
    async fn a_spent_handle_is_not_signalled() {
        // The pid is free for the OS to reuse the moment the child is reaped,
        // so signalling a finished process would reach someone else's.
        let (target, _) = target().await;
        let id = target.start(Exec::new("true")).await.unwrap();

        loop {
            if target
                .output(id, Cursor::START)
                .await
                .unwrap()
                .outcome
                .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let fault = target.signal(id, Signal::Term).await.unwrap_err();
        assert!(matches!(fault, Fault::Denied(Denial::NoSuchProcess(_))));
    }

    #[tokio::test]
    async fn an_unknown_process_handle_is_denied_rather_than_unreachable() {
        let (target, _) = target().await;
        let fault = target.output(ProcId(999), Cursor::START).await.unwrap_err();
        assert!(matches!(fault, Fault::Denied(Denial::NoSuchProcess(_))));
    }

    #[tokio::test]
    async fn large_output_spills_to_a_path_inside_the_target() {
        let (target, _) = target().await;
        let exit = target
            .exec(Exec::new("head -c 200000 /dev/zero | tr '\\0' 'a'"))
            .await
            .unwrap();

        let spill = exit.stdout.spill.expect("large output spills (A2)");
        assert!(spill.as_str().starts_with(SPILL_DIR));
        let span = target.read(&spill, None).await.unwrap();
        assert_eq!(span.len, 200_000);
    }

    #[tokio::test]
    async fn a_missing_capability_is_denied_by_name() {
        let (mut target, _) = target().await;
        target.manifest.capabilities.remove(&Capability::Write);

        let fault = target
            .write(&target.path("/x").unwrap(), &"x".into())
            .await
            .unwrap_err();
        assert!(matches!(
            fault,
            Fault::Denied(Denial::MissingCapability(Capability::Write))
        ));
    }

    #[test]
    fn globs_match_faber_side_without_crossing_a_directory() {
        assert!(glob_matches("*.rs", "main.rs"));
        assert!(!glob_matches("*.rs", "main.md"));
        assert!(glob_matches("m?in.rs", "main.rs"));
        assert!(glob_matches("[mp]ain.rs", "main.rs"));
        assert!(!glob_matches("[!m]ain.rs", "main.rs"));
        assert!(validate_glob("src/*.rs").is_err());
    }
}
