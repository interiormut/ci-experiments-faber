//! Per-user resource ceilings on a host faber operates.
//!
//! Everything else in this crate reaches *a user's* machine, and does it from
//! configuration passed in — never the process's own, because faber is a
//! multi-user service and `DOCKER_HOST`, `~/.ssh`, and a docker context all
//! describe the operator. This module is the deliberate exception, and the
//! distinction is worth stating rather than discovering: a **service host** is
//! faber's own machine. Writing a cgroup limit or an XFS project quota on it
//! is an operator-plane action on operator-owned hardware, not a user's
//! request executed with ambient credentials. Nothing here is reachable for a
//! host a user registered.
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
//! * **Storage** — one XFS project quota per user directory. Not
//!   `--storage-opt size=`, which caps a container's upper layer only, is
//!   ignored by volumes entirely, and gives N containers N × limit rather than
//!   the aggregate that was asked for.
//!
//! Both apply functions are idempotent and are meant to be called on every
//! launch. That is the repair path, and it is why there is no reconciler
//! daemon: a limit that drifted is rewritten by the next container the user
//! starts, and the database stays the single source of truth. For the same
//! reason the writes are *transient* (`systemctl set-property --runtime`) —
//! a persistent unit file is filesystem state that can disagree with the row
//! it came from.

use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

use crate::fault::{Denial, Fault};

/// How long any one machine-side command gets. These are local `execve`s
/// against systemd and xfs_quota; a second of them means something is wedged,
/// and a launch handler must not wait on it.
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

/// Writes one user's CPU, memory, and pid limits onto their slice.
///
/// Idempotent, and live: both `cpu.max` and `memory.max` can be rewritten
/// under running containers, so a quota change takes effect without
/// restarting anything. That also means a grant can shrink underneath a
/// running workload — a memory shrink below current usage triggers reclaim and
/// then an OOM kill *inside that user's slice*, which is the correct blast
/// radius.
pub async fn ensure_slice(subject: i32, limits: &Limits) -> Result<(), Fault> {
    let slice = slice_name(subject);

    let mut properties = Vec::new();
    properties.push(match limits.cpu_millis {
        // systemd takes a percentage of one CPU: 1500 millis is 150%. Written
        // to one decimal rather than divided by ten, because integer division
        // sends any grant under 10 millis to `0%` — which systemd reads as no
        // CPU at all, the opposite of the unlimited a missing grant means.
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
    // kill: a run that has silently become a hundred times slower looks like a
    // hung tool call, and nothing in the transcript says why.
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

    run("systemctl", &argv)
        .await
        .map_err(|error| Fault::Unreachable(format!("could not set limits on {slice}: {error}")))?;
    Ok(())
}

/// Creates one user's directories under a service host's data root and puts
/// their XFS project quota on them.
///
/// ```text
/// {root}/{subject}/
///   work/      → mounted into every container of that user
///   scratch/   → mounted at /scratch; the designated disposable path
/// ```
///
/// The split exists so reclaim guidance can name a path. An agent told only
/// that storage is reclaimable, and blocked on finishing a task, deletes
/// whatever is largest — which is regularly `.git`, a downloaded dataset, or a
/// build cache that took forty minutes to warm.
///
/// Idempotent: the directories are created if absent, and the project id and
/// limit are rewritten every time. Shrinking below current usage is allowed
/// and yields `EDQUOT` on new writes with existing data preserved.
pub async fn ensure_user_data(
    data_root: &Path,
    subject: i32,
    storage_bytes: Option<i64>,
) -> Result<UserPaths, Fault> {
    let paths = UserPaths::under(data_root, subject);

    for directory in [&paths.home, &paths.work, &paths.scratch] {
        tokio::fs::create_dir_all(directory)
            .await
            .map_err(|error| {
                Fault::Unreachable(format!("could not create {}: {error}", directory.display()))
            })?;
    }

    // The subject id is the host-side uid as well as the project id — one
    // integer, so a container's files on disk and a line in an audit log name
    // the same user without a translation table.
    let owner = format!("{subject}:{subject}");
    run("chown", &["-R".to_owned(), owner, path_arg(&paths.home)?]).await?;
    run("chmod", &["700".to_owned(), path_arg(&paths.home)?]).await?;

    let root = path_arg(data_root)?;
    // `project -s` walks the tree and stamps the project id on it, so files
    // already there are counted and new ones inherit it.
    run(
        "xfs_quota",
        &[
            "-x".to_owned(),
            "-c".to_owned(),
            format!("project -s -p {} {subject}", path_arg(&paths.home)?),
            root.clone(),
        ],
    )
    .await?;

    // bhard=0 is XFS's spelling of unlimited, which is what a null grant means.
    let hard = storage_bytes.filter(|bytes| *bytes > 0).unwrap_or(0);
    run(
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
pub async fn release_user_data(data_root: &Path, subject: i32) -> Result<(), Fault> {
    let paths = UserPaths::under(data_root, subject);
    let root = path_arg(data_root)?;

    run(
        "xfs_quota",
        &[
            "-x".to_owned(),
            "-c".to_owned(),
            format!("limit -p bhard=0 {subject}"),
            root,
        ],
    )
    .await?;

    tokio::fs::remove_dir_all(&paths.home)
        .await
        .map_err(|error| {
            Fault::Unreachable(format!(
                "could not remove {}: {error}",
                paths.home.display()
            ))
        })?;
    Ok(())
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
#[derive(Clone, Debug)]
pub struct Allowance {
    pub limits: Limits,
    pub container_max: Option<i32>,
    pub subject: i32,
    pub data_root: std::path::PathBuf,
    /// Already rendered, because a lapse time is a database fact and this
    /// crate has no clock opinion to add to it.
    pub expires_at: Option<String>,
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
            usage: usage(&self.data_root, self.subject).await,
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
/// on a field means the machine did not answer, not that usage is zero.
#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub memory_bytes: Option<u64>,
    pub pids: Option<u64>,
    pub storage_bytes: Option<u64>,
}

/// Reads one user's current footprint.
///
/// Best effort by construction: this exists so an agent that just saw a
/// confusing nonzero exit can cheaply check the hypothesis "I am out of
/// disk", and a report that omits one line is far better than a call that
/// fails because a controller was not enabled.
pub async fn usage(data_root: &Path, subject: i32) -> Usage {
    let slice = slice_name(subject);
    let cgroup = format!("{CGROUP_ROOT}/{TENANT_SLICE}/{slice}");

    Usage {
        memory_bytes: read_counter(&format!("{cgroup}/memory.current")).await,
        pids: read_counter(&format!("{cgroup}/pids.current")).await,
        storage_bytes: project_usage(data_root, subject).await,
    }
}

async fn read_counter(path: &str) -> Option<u64> {
    tokio::fs::read_to_string(path)
        .await
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Blocks used by one project, in bytes.
///
/// `xfs_quota` reports in 1K blocks and `-N` drops the header, leaving a line
/// whose second field is what is used. Parsed rather than derived from
/// `statvfs`, which reports the project quota only when the filesystem is
/// mounted in a particular way — a report that is quietly wrong under some
/// mount options is worse than one that is absent.
async fn project_usage(data_root: &Path, subject: i32) -> Option<u64> {
    let root = path_arg(data_root).ok()?;
    let output = run_capturing(
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
#[cfg(unix)]
pub fn capacity(path: &Path) -> Result<Capacity, Fault> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let raw = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Fault::Unreachable(format!("{} is not a usable path", path.display())))?;

    // SAFETY: `stats` is written by the call and only read once it reports
    // success; `raw` outlives the call.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::statvfs(raw.as_ptr(), &mut stats) } == 0;
    if !ok {
        return Err(Fault::Unreachable(format!(
            "could not stat the filesystem at {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }

    let block = stats.f_frsize as u64;
    Ok(Capacity {
        total_bytes: block * stats.f_blocks as u64,
        available_bytes: block * stats.f_bavail as u64,
    })
}

#[derive(Clone, Copy, Debug)]
pub struct Capacity {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// `MemAvailable` from `/proc/meminfo`, in bytes, and the machine's total.
///
/// The admission check built on this is a *floor* check — refuse new launches
/// when available memory is below an absolute reserve, regardless of the
/// requester's grant. A fit check (`free >= grant`) would be meaningless under
/// overcommit: it would defeat the overcommit it exists to manage. Safety does
/// not rest on either; `faber.slice` bounds the tenant tree in the kernel. The
/// check exists so the *first* failure is a clean refusal rather than an OOM
/// kill somewhere unrelated.
pub async fn memory() -> Result<Memory, Fault> {
    let text = tokio::fs::read_to_string("/proc/meminfo")
        .await
        .map_err(|error| Fault::Unreachable(format!("could not read /proc/meminfo: {error}")))?;

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

#[derive(Clone, Copy, Debug)]
pub struct Memory {
    pub total_bytes: u64,
    pub available_bytes: u64,
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

async fn run(program: &str, args: &[String]) -> Result<(), Fault> {
    run_capturing(program, args).await.map(|_| ())
}

/// One machine-side command, with its stderr carried into the failure.
///
/// The message matters more than usual here: "could not set limits" with the
/// controller-not-enabled line from systemd attached is a provisioning bug an
/// operator can fix in a minute, and the same failure without it is a support
/// ticket.
async fn run_capturing(program: &str, args: &[String]) -> Result<String, Fault> {
    let call = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output();

    let output = match tokio::time::timeout(COMMAND_TIMEOUT, call).await {
        Err(_) => {
            return Err(Fault::Unreachable(format!(
                "{program} did not finish within {COMMAND_TIMEOUT:?}"
            )));
        }
        Ok(Err(error)) => {
            return Err(Fault::Unreachable(format!(
                "could not run {program}: {error}"
            )));
        }
        Ok(Ok(output)) => output,
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Fault::Unreachable(format!(
            "{program} {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
}
