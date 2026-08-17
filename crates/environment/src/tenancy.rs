//! Per-user resource ceilings on a host faber operates.
//!
//! Everything else in this crate reaches *a user's* machine, and does it from
//! configuration passed in — never the process's own, because faber is a
//! multi-user service and `DOCKER_HOST`, `~/.ssh`, and a docker context all
//! describe the operator. This module used to be the deliberate exception: a
//! service host was faber's own machine, so writing a cgroup limit or an XFS
//! project quota was a local `execve` on operator-owned hardware.
//!
//! It is not an exception any more. A service host is reached the same way
//! every other machine is — a [`Spawn`] for the four commands, and a
//! [`Reads`] for the three values a syscall returns — and both come from the
//! caller. What replaced the exception is stronger than it was: the agent
//! that runs a host's containers is the same connection that writes its
//! limits, so the two cannot be pointed at different machines, and an
//! agent's privilege is fixed when someone installs it. A daemon installed
//! under a user's own account is *physically incapable* of writing a cgroup
//! limit or a project quota, so "nothing here is reachable for a host a user
//! registered" is now a property of the operating system rather than of a
//! code path.
//!
//! Two mechanisms, chosen because the kernel already aggregates for them:
//!
//! * **CPU and RAM** — one systemd slice per (host, user), with every one of
//!   that user's containers launched under it via `--cgroup-parent`. The
//!   dash-hierarchy naming puts `faber-500001.slice` under `faber.slice`
//!   automatically, so a machine-wide tenant cap comes for free and the
//!   per-user aggregate across an arbitrary number of containers is enforced
//!   with no accounting code. Summing per-container limits in userspace would
//!   reimplement what the parent slice does in the kernel and then drift from
//!   it.
//! * **Storage** — one XFS project quota per user directory. The *aggregate*
//!   is only ever this: `--storage-opt size=` caps a container's upper layer
//!   only, is ignored by volumes entirely, and gives N containers N × limit
//!   rather than the sum that was asked for. It is not a substitute and never
//!   was. It is, however, no longer optional either — the read-only root
//!   filesystem that used to keep the writable layer out of play went away
//!   with the pinned uid, so the layer cap is now the only thing bounding a
//!   write path the project quota cannot see. Two mechanisms bounding two
//!   different things, and the count limit is what makes the second aggregate
//!   at all.
//!
//! Both apply functions are idempotent and are meant to be called on every
//! launch. That is the repair path, and it is why there is no reconciler
//! daemon: a limit that drifted is rewritten by the next container the user
//! starts, and the database stays the single source of truth. For the same
//! reason the writes are *transient* (`systemctl set-property --runtime`) —
//! a persistent unit file is filesystem state that can disagree with the row
//! it came from.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncReadExt;

use crate::fault::{Denial, Fault};
use crate::spawn::{Run, Spawn};

/// How long any one machine-side command gets.
///
/// These are no longer local `execve`s: each one crosses the agent link, and
/// the round trip is why the reads that feed an admission decision happen
/// before the admission lock rather than inside it. Twenty seconds is still
/// far longer than `systemctl set-property` or `xfs_quota` needs, and a
/// launch handler must not wait on a wedged one for longer.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

/// The tenant tree every per-user slice nests under. Created at provisioning
/// with the machine-wide cap, never by this code — if it is missing, the
/// per-user slices still work and the machine-wide bound is simply absent,
/// which is a provisioning failure and not something to paper over at launch.
pub const TENANT_SLICE: &str = "faber.slice";

/// Where the unified hierarchy is mounted. Fixed by cgroup v2; a service host
/// that has it elsewhere fails the provisioning check rather than being
/// probed for here.
const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// The fraction of a memory grant at which reclaim starts, so a user meets
/// backpressure before the OOM killer meets them.
const MEMORY_HIGH_RATIO: f64 = 0.85;

/// Where the transport leaves its own bookkeeping on a service host — one pid
/// file per command, so a timed-out command can be signalled.
///
/// Three constraints pick this path, and only one of them is obvious. It has
/// to be writable by the daemon, which is root, and *not* by anyone else: a
/// world-writable directory lets a local user pre-create the parent as a
/// symlink and have root write through it. It has to be somewhere that gets
/// cleaned up, because these commands run on every launch and nothing here
/// deletes what it wrote — under `user_data_root` the files would accumulate
/// forever inside the very reserve the admission check is protecting. `/run`
/// is tmpfs, root-owned, and empty after a reboot; the system unit declares
/// `RuntimeDirectory=faber-agent`, so systemd creates and removes it.
const WORK_ROOT: &str = "/run/faber-agent";

/// What one user is allowed on one host. `None` is unlimited — everywhere and
/// always, never "inherit".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// 1000 = one core. A hard ceiling, not a weight: `cpu.weight` stays at
    /// the default for every tenant, because deriving it from the grant would
    /// make an otherwise-idle box favour large-grant users and undo the
    /// predictability a hard cap was chosen for.
    pub cpu_millis: Option<i32>,
    pub memory_bytes: Option<i64>,
    pub storage_bytes: Option<i64>,
    /// The fork-bomb guard. One line, and the only thing standing between a
    /// runaway build script and every other tenant's ability to fork.
    pub pids_max: Option<i32>,
}

/// The slice one user's containers run under on this machine.
///
/// `faber-<subject>.slice` — systemd reads the dashes as a hierarchy, so this
/// is a child of [`TENANT_SLICE`] without anything having to say so.
pub fn slice_name(subject: i32) -> String {
    format!("faber-{subject}.slice")
}

// ---------------------------------------------------------------------------
// Reaching the machine
// ---------------------------------------------------------------------------

/// The three values that are a syscall on the machine and cannot be one from
/// here.
///
/// Split out from [`Spawn`] rather than expressed as commands because each of
/// these feeds a decision on a number: `statvfs` fields survive a struct, and
/// they do not reliably survive `stat -f` across coreutils versions. The
/// admission check is the one that matters — it is the single place faber
/// refuses to overcommit, and it should not be refusing on a parse.
#[async_trait]
pub trait Reads: Send + Sync {
    async fn capacity(&self, path: &str) -> Result<Capacity, Fault>;
    async fn memory(&self) -> Result<Memory, Fault>;
    /// Whole numbers out of files, one per path, in order. `None` for a path
    /// that could not be read — a controller that is not enabled says
    /// nothing, and saying zero for it would report a tenant as idle.
    async fn counters(&self, paths: &[String]) -> Result<Vec<Option<u64>>, Fault>;
}

/// One service host's machine, as everything in this module reaches it.
///
/// Carried rather than looked up: this crate knows nothing about hosts,
/// transports, or the database, and the pair of handles arrives already
/// resolved. The invariant that makes the whole design safe is upstream of
/// here and structural — the docker socket and these commands are served
/// over one connection, so limits cannot be written on one machine while
/// containers run on another.
#[derive(Clone)]
pub struct Tenancy {
    spawn: Arc<dyn Spawn>,
    reads: Arc<dyn Reads>,
}

impl std::fmt::Debug for Tenancy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Neither half has anything printable — a transport handle and a
        // connection — and the useful fact is that it exists.
        f.write_str("Tenancy")
    }
}

impl Tenancy {
    pub fn new(spawn: Arc<dyn Spawn>, reads: Arc<dyn Reads>) -> Self {
        Tenancy { spawn, reads }
    }

    /// A service host that is this process's own machine.
    ///
    /// Still reachable, and still what a development checkout has, but no
    /// longer what a deployment looks like: an API in a container is not the
    /// machine, `systemctl` and `xfs_quota` are absent from the image, and
    /// `/sys/fs/cgroup` resolves to the container's own subtree.
    pub fn local() -> Self {
        Tenancy::new(Arc::new(crate::local::LocalSpawn), Arc::new(LocalReads))
    }

    /// A service host reached through the daemon it runs.
    ///
    /// The same session the host's docker socket is forwarded over, which is
    /// what makes "the agent that runs a host's containers is the agent that
    /// writes its limits" structural rather than a rule someone has to keep.
    ///
    pub fn over_agent(session: Arc<crate::ssh::SshSession>) -> Self {
        Tenancy::new(
            Arc::new(crate::ssh::SshSpawn::new(Arc::clone(&session), WORK_ROOT)),
            Arc::new(crate::ssh::probe::AgentReads::new(session)),
        )
    }

    /// Writes one user's CPU, memory, and pid limits onto their slice.
    ///
    /// Idempotent, and live: both `cpu.max` and `memory.max` can be rewritten
    /// under running containers, so a quota change takes effect without
    /// restarting anything. That also means a grant can shrink underneath a
    /// running workload — a memory shrink below current usage triggers reclaim
    /// and then an OOM kill *inside that user's slice*, which is the correct
    /// blast radius.
    pub async fn ensure_slice(&self, subject: i32, limits: &Limits) -> Result<(), Fault> {
        let slice = slice_name(subject);

        let mut properties = Vec::new();
        properties.push(match limits.cpu_millis {
            // systemd takes a percentage of one CPU: 1500 millis is 150%.
            // Written to one decimal rather than divided by ten, because
            // integer division sends any grant under 10 millis to `0%` —
            // which systemd reads as no CPU at all, the opposite of the
            // unlimited a missing grant means.
            Some(millis) if millis > 0 => format!("CPUQuota={}.{}%", millis / 10, millis % 10),
            // An empty value is systemd's spelling of "no quota".
            _ => "CPUQuota=".to_owned(),
        });
        properties.push(match limits.memory_bytes {
            Some(bytes) if bytes > 0 => format!("MemoryMax={bytes}"),
            _ => "MemoryMax=infinity".to_owned(),
        });
        properties.push(match limits.memory_bytes {
            Some(bytes) if bytes > 0 => {
                format!("MemoryHigh={}", (bytes as f64 * MEMORY_HIGH_RATIO) as i64)
            }
            _ => "MemoryHigh=infinity".to_owned(),
        });
        // Swap-induced latency inside an agent container is worse than a clean
        // kill: a run that has silently become a hundred times slower looks
        // like a hung tool call, and nothing in the transcript says why.
        properties.push("MemorySwapMax=0".to_owned());
        properties.push(match limits.pids_max {
            Some(max) if max > 0 => format!("TasksMax={max}"),
            _ => "TasksMax=infinity".to_owned(),
        });

        let mut argv = vec![
            "set-property".to_owned(),
            "--runtime".to_owned(),
            slice.clone(),
        ];
        argv.extend(properties);

        self.run("systemctl", &argv).await.map_err(|error| {
            Fault::Unreachable(format!("could not set limits on {slice}: {error}"))
        })?;
        Ok(())
    }

    /// Creates one user's directories under a service host's data root and
    /// puts their XFS project quota on them.
    ///
    /// ```text
    /// {root}/{subject}/
    ///   work/      → mounted into every container of that user
    ///   scratch/   → mounted at /scratch; the designated disposable path
    /// ```
    ///
    /// The split exists so reclaim guidance can name a path. An agent told
    /// only that storage is reclaimable, and blocked on finishing a task,
    /// deletes whatever is largest — which is regularly `.git`, a downloaded
    /// dataset, or a build cache that took forty minutes to warm.
    ///
    /// `container_root_uid` is what the daemon's `--userns-remap` maps
    /// container uid 0 to, and it is who the tree is given to — so root inside
    /// a tenant's container owns the workspace it is handed. It is passed in
    /// rather than read off the machine for the reason at the top of this
    /// module: what faber does to a host comes from configuration, never from
    /// the machine's own idea of itself.
    ///
    /// A tenant who wants a non-root process to write here does it themselves,
    /// with `chmod` inside their container. Owning the tree is what makes that
    /// possible and is the whole of what faber owes them; faber cannot guess
    /// which uids an image drops privileges to.
    ///
    /// Idempotent: the directories are created if absent, and the project id
    /// and limit are rewritten every time. Shrinking below current usage is
    /// allowed and yields `EDQUOT` on new writes with existing data preserved.
    pub async fn ensure_user_data(
        &self,
        data_root: &Path,
        subject: i32,
        storage_bytes: Option<i64>,
        container_root_uid: u32,
    ) -> Result<UserPaths, Fault> {
        let paths = UserPaths::under(data_root, subject);

        // `mkdir -p` rather than the `Files` half of the transport: these
        // three are one command where sftp is three round trips and a
        // create-if-absent dance, and the next two commands are already
        // going the same way.
        self.run(
            "mkdir",
            &[
                "-p".to_owned(),
                path_arg(&paths.work)?,
                path_arg(&paths.scratch)?,
            ],
        )
        .await?;

        // Given to the uid the container's root maps to, not to `subject`.
        // The tenant is root inside their container and has to be able to
        // write their own workspace; a tree owned by anyone else refuses them
        // at the mount boundary, which is the same wall the read-only root and
        // the pinned uid used to be.
        //
        // What this gives up is that `subject` no longer names the owner on
        // disk. It cannot: the remap is daemon-wide, so this uid is the same
        // for every tenant, and correlating a file to a user now goes through
        // the path and the project id rather than through `stat`. The project
        // id is the one that matters and is unaffected — XFS accounts by
        // directory tree, so a write under any uid still bills this tenant.
        let owner = format!("{container_root_uid}:{container_root_uid}");
        self.run("chown", &["-R".to_owned(), owner, path_arg(&paths.home)?])
            .await?;
        // Still 700. The mode is no longer what separates tenants — the remap
        // makes every tenant's container-root one uid, so mode bits between
        // them stopped meaning anything — but the owner is now the container's
        // root, so 700 is exactly "the tenant, and nobody else on the machine".
        self.run("chmod", &["700".to_owned(), path_arg(&paths.home)?])
            .await?;

        let root = path_arg(data_root)?;
        // `project -s` walks the tree and stamps the project id on it, so
        // files already there are counted and new ones inherit it.
        self.run(
            "xfs_quota",
            &[
                "-x".to_owned(),
                "-c".to_owned(),
                format!("project -s -p {} {subject}", path_arg(&paths.home)?),
                root.clone(),
            ],
        )
        .await?;

        // bhard=0 is XFS's spelling of unlimited, which is what a null grant
        // means.
        let hard = storage_bytes.filter(|bytes| *bytes > 0).unwrap_or(0);
        self.run(
            "xfs_quota",
            &[
                "-x".to_owned(),
                "-c".to_owned(),
                format!("limit -p bhard={hard} {subject}"),
                root,
            ],
        )
        .await?;

        Ok(paths)
    }

    /// Removes a user's project quota and directory when their reservation is
    /// returned.
    ///
    /// Separate from container teardown on purpose: slices and quotas are
    /// per-user, not per-container, and the next launch reuses them. Only
    /// releasing the whole `host_user` gets here.
    pub async fn release_user_data(&self, data_root: &Path, subject: i32) -> Result<(), Fault> {
        let paths = UserPaths::under(data_root, subject);
        let root = path_arg(data_root)?;

        self.run(
            "xfs_quota",
            &[
                "-x".to_owned(),
                "-c".to_owned(),
                format!("limit -p bhard=0 {subject}"),
                root,
            ],
        )
        .await?;

        self.run("rm", &["-rf".to_owned(), path_arg(&paths.home)?])
            .await?;
        Ok(())
    }

    /// Reads one user's current footprint.
    ///
    /// Best effort *per field*, and honest about the difference between the
    /// two ways a field can be missing. A controller that is not enabled and
    /// a project with no usage are both answers from the machine, and they
    /// arrive as `None` and `Some(0)`. A machine that did not answer at all
    /// is neither, and sets [`Usage::unreachable`] — over a network that case
    /// exists, where locally it did not, and rendering it as "unknown" would
    /// tell a tenant their usage is unmeasurable when the host is simply
    /// gone.
    pub async fn usage(&self, data_root: &Path, subject: i32) -> Usage {
        let slice = slice_name(subject);
        let cgroup = format!("{CGROUP_ROOT}/{TENANT_SLICE}/{slice}");
        let paths = vec![
            format!("{cgroup}/memory.current"),
            format!("{cgroup}/pids.current"),
        ];

        let counters = match self.reads.counters(&paths).await {
            Ok(values) => values,
            Err(error) => {
                return Usage {
                    unreachable: Some(error.to_string()),
                    ..Default::default()
                };
            }
        };

        Usage {
            memory_bytes: counters.first().copied().flatten(),
            pids: counters.get(1).copied().flatten(),
            storage_bytes: self.project_usage(data_root, subject).await,
            unreachable: None,
        }
    }

    /// Blocks used by one project, in bytes.
    ///
    /// `xfs_quota` reports in 1K blocks and `-N` drops the header, leaving a
    /// line whose second field is what is used. Parsed rather than derived
    /// from `statvfs`, which reports the project quota only when the
    /// filesystem is mounted in a particular way — a report that is quietly
    /// wrong under some mount options is worse than one that is absent.
    async fn project_usage(&self, data_root: &Path, subject: i32) -> Option<u64> {
        let root = path_arg(data_root).ok()?;
        let output = self
            .run_capturing(
                "xfs_quota",
                &[
                    "-x".to_owned(),
                    "-c".to_owned(),
                    format!("quota -p -N -b -v {subject}"),
                    root,
                ],
            )
            .await
            .ok()?;

        output
            .lines()
            .find(|line| !line.trim().is_empty())
            .and_then(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
            .map(|blocks| blocks * 1024)
    }

    /// Bytes free on the filesystem holding a path, and its total size.
    ///
    /// Used for the storage reservation check, which is the one place faber
    /// refuses to overcommit: `ENOSPC` is global and cannot be repaired by the
    /// user who caused it, while CPU and RAM merely degrade under contention.
    pub async fn capacity(&self, path: &Path) -> Result<Capacity, Fault> {
        self.reads.capacity(&path_arg(path)?).await
    }

    /// `MemAvailable` from `/proc/meminfo`, in bytes, and the machine's total.
    ///
    /// The admission check built on this is a *floor* check — refuse new
    /// launches when available memory is below an absolute reserve, regardless
    /// of the requester's grant. A fit check (`free >= grant`) would be
    /// meaningless under overcommit: it would defeat the overcommit it exists
    /// to manage. Safety does not rest on either; `faber.slice` bounds the
    /// tenant tree in the kernel. The check exists so the *first* failure is a
    /// clean refusal rather than an OOM kill somewhere unrelated.
    pub async fn memory(&self) -> Result<Memory, Fault> {
        self.reads.memory().await
    }

    async fn run(&self, program: &str, args: &[String]) -> Result<(), Fault> {
        self.run_capturing(program, args).await.map(|_| ())
    }

    /// One machine-side command, with its stderr carried into the failure.
    ///
    /// The message matters more than usual here: "could not set limits" with
    /// the controller-not-enabled line from systemd attached is a provisioning
    /// bug an operator can fix in a minute, and the same failure without it is
    /// a support ticket. Which is why stderr is drained rather than dropped —
    /// a transport that reports only an exit code would lose exactly the half
    /// that makes these failures actionable.
    async fn run_capturing(&self, program: &str, args: &[String]) -> Result<String, Fault> {
        let mut argv = vec![program.to_owned()];
        argv.extend_from_slice(args);

        let mut proc = self
            .spawn
            .spawn(Run {
                argv,
                // Nothing here reads a relative path, and `/` is the one
                // directory every machine has.
                cwd: "/".to_owned(),
                env: Vec::new(),
                pty: false,
            })
            .await?;

        // Closed rather than left open: none of these commands reads input,
        // and a shell rc file that prompts would otherwise wait on one.
        drop(proc.stdin());
        let mut stdout = proc.stdout();
        let mut stderr = proc.stderr();
        let draining = tokio::spawn(async move {
            let mut out = Vec::new();
            let mut err = Vec::new();
            if let Some(pipe) = stdout.as_mut() {
                let _ = pipe.read_to_end(&mut out).await;
            }
            if let Some(pipe) = stderr.as_mut() {
                let _ = pipe.read_to_end(&mut err).await;
            }
            (out, err)
        });

        let outcome = match tokio::time::timeout(COMMAND_TIMEOUT, proc.wait()).await {
            Err(_) => {
                let _ = proc.signal(crate::exec::Signal::Kill).await;
                return Err(Fault::Unreachable(format!(
                    "{program} did not finish within {COMMAND_TIMEOUT:?}"
                )));
            }
            Ok(Err(fault)) => return Err(fault),
            Ok(Ok(outcome)) => outcome,
        };

        let (out, err) = draining.await.unwrap_or_default();
        let code = match outcome {
            crate::exec::Outcome::Completed { code } => code,
            other => {
                return Err(Fault::Unreachable(format!(
                    "{program} did not complete: {other:?}"
                )));
            }
        };

        // A channel that closed without reporting a status leaves `-1` and no
        // stderr, and rendering that as "failed: " with nothing after the
        // colon says the machine refused when in fact it never answered.
        // Named here so the two stay distinguishable in a log.
        if code == -1 && err.is_empty() {
            return Err(Fault::Unreachable(format!(
                "the link closed before {program} reported a status"
            )));
        }

        if code != 0 {
            return Err(Fault::Unreachable(format!(
                "{program} {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&err).trim()
            )));
        }

        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

/// Where one user's data lives on a service host.
#[derive(Clone, Debug)]
pub struct UserPaths {
    pub home: std::path::PathBuf,
    pub work: std::path::PathBuf,
    /// The designated disposable path, and the only path reclaim guidance ever
    /// names.
    pub scratch: std::path::PathBuf,
}

impl UserPaths {
    pub fn under(data_root: &Path, subject: i32) -> Self {
        let home = data_root.join(subject.to_string());
        UserPaths {
            work: home.join("work"),
            scratch: home.join("scratch"),
            home,
        }
    }
}

// ---------------------------------------------------------------------------
// What a bound target is allowed, and what it is using
// ---------------------------------------------------------------------------

/// One target's share of a shared machine, attached at bind.
///
/// Carried rather than looked up, because this crate knows nothing about
/// grants, users, or the database — the numbers arrive already resolved and
/// everything here does with them is read the machine and render.
///
/// The [`Tenancy`] rides along for one specific reason: a bound target's own
/// transport reaches *into the container*, and reading a cgroup counter or a
/// project quota through it would report the container's view of itself
/// rather than the host's view of the tenant. The reads have to go to the
/// machine, so the handle to the machine comes with the allowance.
#[derive(Clone, Debug)]
pub struct Allowance {
    pub limits: Limits,
    pub container_max: Option<i32>,
    pub subject: i32,
    pub data_root: std::path::PathBuf,
    /// Already rendered, because a lapse time is a database fact and this
    /// crate has no clock opinion to add to it.
    pub expires_at: Option<String>,
    pub tenancy: Tenancy,
}

/// An [`Allowance`] with the machine read for what is actually in use.
#[derive(Clone, Debug)]
pub struct AllowanceReport {
    pub limits: Limits,
    pub container_max: Option<i32>,
    pub usage: Usage,
    /// The one path reclaim guidance names.
    pub scratch_path: std::path::PathBuf,
    pub work_path: std::path::PathBuf,
    pub expires_at: Option<String>,
}

impl Allowance {
    /// Reads the machine and pairs what it says with the grant.
    pub async fn measure(&self) -> AllowanceReport {
        let paths = UserPaths::under(&self.data_root, self.subject);
        AllowanceReport {
            limits: self.limits,
            container_max: self.container_max,
            usage: self.tenancy.usage(&self.data_root, self.subject).await,
            scratch_path: paths.scratch,
            work_path: paths.work,
            expires_at: self.expires_at.clone(),
        }
    }
}

impl AllowanceReport {
    /// The report as the agent reads it.
    ///
    /// Every line is either a number the agent can compare a hypothesis
    /// against or the path it may act on. Nothing here is advice.
    pub fn render(&self) -> String {
        fn granted(value: Option<i64>) -> String {
            match value {
                None => "unlimited".to_owned(),
                Some(value) => gibibytes(value as f64),
            }
        }
        fn measured(value: Option<u64>) -> String {
            match value {
                None => "unknown".to_owned(),
                Some(value) => gibibytes(value as f64),
            }
        }
        fn gibibytes(value: f64) -> String {
            format!("{:.1} GiB", value / 1024.0 / 1024.0 / 1024.0)
        }

        let mut lines = vec![
            format!(
                "  storage: {} of {} used",
                measured(self.usage.storage_bytes),
                granted(self.limits.storage_bytes)
            ),
            format!(
                "  memory: {} of {} in use",
                measured(self.usage.memory_bytes),
                granted(self.limits.memory_bytes)
            ),
            match self.limits.cpu_millis {
                None => "  cpu: unlimited".to_owned(),
                Some(millis) => format!("  cpu: {:.2} cores", f64::from(millis) / 1000.0),
            },
        ];

        // Said once, plainly, rather than left to be inferred from three
        // `unknown`s: an agent that cannot tell "nothing is measurable" from
        // "the host is not answering" will keep testing hypotheses against
        // numbers that are not there.
        if let Some(reason) = &self.usage.unreachable {
            lines.push(format!("  (usage could not be read: {reason})"));
        }

        if let Some(max) = self.container_max {
            lines.push(format!("  containers: up to {max}"));
        }
        // Named, never merely offered. An agent told that space is
        // reclaimable and blocked on finishing deletes whatever is largest,
        // which is regularly `.git` or a build cache that took forty minutes
        // to warm.
        lines.push(format!(
            "  free space under {} only; everything else is durable",
            self.scratch_path.display()
        ));

        if let Some(expires) = &self.expires_at {
            lines.push(format!(
                "  these limits are temporary and lapse at {expires}"
            ));
        }

        lines.join("\n")
    }
}

// ---------------------------------------------------------------------------
// Live reads
// ---------------------------------------------------------------------------

/// What a user is currently using, read from the machine every time.
///
/// Nothing here is cached or stored: per-user aggregates are a property of the
/// parent cgroup and the project quota, and a copy in the database would be
/// the same cached-liveness mistake the host schema already refuses. `None`
/// on a field means the machine did not answer *that field*, not that usage is
/// zero; `unreachable` means the machine did not answer at all.
#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub memory_bytes: Option<u64>,
    pub pids: Option<u64>,
    pub storage_bytes: Option<u64>,
    /// Why nothing could be read, when nothing could be. Best effort became a
    /// different thing when the machine moved to the far end of a link: a
    /// swallowed error used to mean "a controller is off" and can now also
    /// mean "the host is gone", and those want different reactions.
    pub unreachable: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct Capacity {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Memory {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// Machine reads on the machine this process is running on.
///
/// What a development checkout has, and what a service host deployed beside
/// the API used to have. Kept because `local` service host rows are still
/// legal and because an operator report that degrades in a checkout is better
/// than one that fails there.
pub struct LocalReads;

#[async_trait]
impl Reads for LocalReads {
    #[cfg(unix)]
    async fn capacity(&self, path: &str) -> Result<Capacity, Fault> {
        use std::ffi::CString;

        let raw = CString::new(path)
            .map_err(|_| Fault::Unreachable(format!("{path} is not a usable path")))?;

        // SAFETY: `stats` is written by the call and only read once it reports
        // success; `raw` outlives the call.
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(raw.as_ptr(), &mut stats) } != 0 {
            return Err(Fault::Unreachable(format!(
                "could not stat the filesystem at {path}: {}",
                std::io::Error::last_os_error()
            )));
        }

        let block = stats.f_frsize as u64;
        Ok(Capacity {
            total_bytes: block * stats.f_blocks as u64,
            available_bytes: block * stats.f_bavail as u64,
        })
    }

    #[cfg(not(unix))]
    async fn capacity(&self, _path: &str) -> Result<Capacity, Fault> {
        Err(Fault::Unreachable("this machine has no statvfs".to_owned()))
    }

    async fn memory(&self) -> Result<Memory, Fault> {
        let text = tokio::fs::read_to_string("/proc/meminfo")
            .await
            .map_err(|error| {
                Fault::Unreachable(format!("could not read /proc/meminfo: {error}"))
            })?;

        let field = |name: &str| -> Option<u64> {
            text.lines()
                .find(|line| line.starts_with(name))?
                .split_whitespace()
                .nth(1)?
                .parse::<u64>()
                .ok()
                .map(|kilobytes| kilobytes * 1024)
        };

        Ok(Memory {
            total_bytes: field("MemTotal:").unwrap_or(0),
            available_bytes: field("MemAvailable:").ok_or_else(|| {
                Fault::Unreachable("/proc/meminfo did not report MemAvailable".to_owned())
            })?,
        })
    }

    async fn counters(&self, paths: &[String]) -> Result<Vec<Option<u64>>, Fault> {
        let mut values = Vec::with_capacity(paths.len());
        for path in paths {
            values.push(
                tokio::fs::read_to_string(path)
                    .await
                    .ok()
                    .and_then(|body| body.trim().parse().ok()),
            );
        }
        Ok(values)
    }
}

// ---------------------------------------------------------------------------
// Running the tools
// ---------------------------------------------------------------------------

fn path_arg(path: &Path) -> Result<String, Fault> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        Fault::Denied(Denial::Malformed {
            what: "path".into(),
            reason: format!("{} is not valid UTF-8", path.display()),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slice_name_nests_under_the_tenant_tree() {
        // The dash is the hierarchy: systemd puts `faber-500001.slice` under
        // `faber.slice` without anything declaring the relationship, which is
        // what gives the machine-wide tenant cap for free.
        assert_eq!(slice_name(500_001), "faber-500001.slice");
    }

    #[test]
    fn a_user_directory_carries_work_and_scratch() {
        let paths = UserPaths::under(Path::new("/srv/faber"), 500_001);
        assert_eq!(paths.home, Path::new("/srv/faber/500001"));
        assert_eq!(paths.work, Path::new("/srv/faber/500001/work"));
        // Named, not implied: reclaim guidance points here and nowhere else.
        assert_eq!(paths.scratch, Path::new("/srv/faber/500001/scratch"));
    }

    #[tokio::test]
    async fn an_unreachable_machine_is_not_reported_as_an_idle_tenant() {
        struct Gone;

        #[async_trait]
        impl Reads for Gone {
            async fn capacity(&self, _path: &str) -> Result<Capacity, Fault> {
                Err(Fault::Unreachable("no agent is connected".to_owned()))
            }
            async fn memory(&self) -> Result<Memory, Fault> {
                Err(Fault::Unreachable("no agent is connected".to_owned()))
            }
            async fn counters(&self, _paths: &[String]) -> Result<Vec<Option<u64>>, Fault> {
                Err(Fault::Unreachable("no agent is connected".to_owned()))
            }
        }

        let tenancy = Tenancy::new(Arc::new(crate::local::LocalSpawn), Arc::new(Gone));
        let usage = tenancy.usage(Path::new("/srv/faber"), 500_001).await;

        // The distinction the old best-effort read could not express: this
        // is not a tenant using nothing, and the report has to be able to
        // say so.
        assert_eq!(usage.memory_bytes, None);
        assert!(usage.unreachable.is_some());

        let report = AllowanceReport {
            limits: Limits::default(),
            container_max: None,
            usage,
            scratch_path: "/srv/faber/500001/scratch".into(),
            work_path: "/srv/faber/500001/work".into(),
            expires_at: None,
        };
        assert!(report.render().contains("usage could not be read"));
    }
}
