//! The systemd unit `install` writes and starts — a *user* unit by default,
//! a system one when asked for.
//!
//! The daemon's privileges are settled the moment someone runs `install`,
//! and the unit's scope is the recording of that decision rather than a
//! second decision layered on top. A user install has no root to install as
//! and therefore no privilege to drop; a system install has exactly what
//! provisioning gave the machine, and the daemon neither asks for elevation
//! nor has a protocol for requesting it.
//!
//! That fixed-at-install privilege is load-bearing rather than incidental.
//! Faber writes cgroup limits and XFS project quotas on the hosts it
//! operates, and it writes them through this daemon; a daemon installed
//! under a user's own account is *physically incapable* of either, so "a
//! host a user registered has no limits written on it" is a property of the
//! operating system rather than of a code path.
//!
//! Everything here is best-effort by design. A machine without systemd, or
//! one reached over a bare `docker exec` with no user session bus, is still
//! a machine the daemon runs fine on — it just has to be started some other
//! way. Failing the whole install because supervision could not be arranged
//! would throw away a completed enrollment, which is the expensive half.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Scope;

/// Where the system manager reads units an administrator installed. Not
/// `/lib/systemd/system`, which belongs to the package manager.
const SYSTEM_UNIT_DIR: &str = "/etc/systemd/system";

/// `$XDG_CONFIG_HOME/systemd/user`, falling back to `~/.config/systemd/user`
/// — where `systemctl --user` looks for units it did not install itself.
fn unit_dir(scope: Scope) -> std::io::Result<PathBuf> {
    if scope == Scope::System {
        return Ok(PathBuf::from(SYSTEM_UNIT_DIR));
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("systemd").join("user"));
    }
    let home = std::env::var("HOME").map_err(|_| {
        std::io::Error::other("neither XDG_CONFIG_HOME nor HOME is set; cannot place the unit")
    })?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("systemd")
        .join("user"))
}

const UNIT_NAME: &str = "faber-agent.service";

fn unit_body(scope: Scope, exe: &Path, env_file: &Path) -> String {
    // `Restart=always` overlaps with the daemon's own reconnect loop on
    // purpose: the loop covers a dropped connection, and this covers the
    // process itself dying, which the loop by definition cannot.
    //
    // Ordering only in the system unit: `network-online.target` is a system
    // target, so a user manager cannot order against it and writing it there
    // would look like a guarantee while doing nothing. The reconnect loop is
    // the real answer to starting before the network is up either way; the
    // ordering just spares a system daemon one guaranteed failed dial at
    // boot.
    //
    // `run --system` rather than a bare `run`: the scope is not detected at
    // runtime, so the unit has to say which daemon it is starting — and the
    // config it reads lives in a different place because of it.
    //
    // `EnvironmentFile` with a leading `-` so a missing file is not a
    // startup failure — see `write_env_file` for why the file exists at all.
    //
    // `RuntimeDirectory` only in the system unit, and it is not decoration:
    // faber writes a host's tenant limits through this daemon, and the
    // transport leaves one pid file per command so a timed-out command can
    // still be signalled. `/run/faber-agent` is where they go — tmpfs,
    // root-only, created and removed by systemd — because nothing deletes
    // them individually and a per-launch file on the tenant filesystem would
    // grow without bound inside the reserve faber keeps free.
    let (ordering, run, runtime_dir, wanted_by) = match scope {
        Scope::User => ("", "run", "", "default.target"),
        Scope::System => (
            "Wants=network-online.target\nAfter=network-online.target\n",
            "run --system",
            "RuntimeDirectory=faber-agent\nRuntimeDirectoryMode=0700\n",
            "multi-user.target",
        ),
    };
    format!(
        "[Unit]\n\
         Description=Faber agent\n\
         {ordering}\
         \n\
         [Service]\n\
         EnvironmentFile=-{env_file}\n\
         ExecStart={exe} {run}\n\
         {runtime_dir}\
         Restart=always\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy={wanted_by}\n",
        env_file = env_file.display(),
        exe = exe.display()
    )
}

/// Proxy variables systemd would otherwise drop on the floor.
const PROXY_VARS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
];

/// Carries the installing shell's proxy settings into the service.
///
/// A systemd user manager builds its environment at login — or, under
/// lingering, at boot — and reads no shell profile, so an `HTTPS_PROXY`
/// exported in the terminal that ran `install` is simply absent when the
/// daemon later runs. That is the worst possible place to lose it: the
/// hosts this daemon exists for are the ones whose only route out is a
/// proxy, `install` itself succeeds because it ran in the shell that had
/// the variable, and the loss surfaces only as a connect failure afterwards.
///
/// Written as a file rather than `Environment=` lines because a proxy URL
/// can carry credentials, and this file can be `0600` where a unit file
/// read by the user manager conventionally is not.
/// Reads the proxy variables out of this process's environment, for
/// [`write_env_file`] to record. Split from the writing so the writer takes
/// its input as an argument and stays testable without a test having to
/// mutate the environment out from under its neighbours.
fn ambient_proxy_vars() -> Vec<(String, String)> {
    PROXY_VARS
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| ((*name).to_owned(), value))
        })
        .collect()
}

fn write_env_file(path: &Path, vars: &[(String, String)]) -> std::io::Result<Vec<String>> {
    let mut set = Vec::new();
    let mut body = String::from(
        "# Written by `faber-agent install` from the environment it ran in.\n\
         # systemd user services inherit no shell profile, so anything the\n\
         # daemon needs at runtime has to be stated here.\n",
    );
    for (name, value) in vars {
        // A newline would end the assignment and let the rest of the value
        // be read as another directive.
        if !value.trim().is_empty() && !value.contains('\n') {
            body.push_str(&format!("{name}={value}\n"));
            set.push(name.clone());
        }
    }
    std::fs::write(path, body)?;
    set_owner_only(path)?;
    Ok(set)
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Writes the unit and, unless `start` is false, enables and starts it.
///
/// Reports what happened rather than returning a status the caller has to
/// interpret: the only consumer is `install`, and the only thing it does
/// with the outcome is tell the operator what is now running.
pub fn install_unit(scope: Scope, start: bool) -> std::io::Result<()> {
    // The absolute path of *this* binary, whatever put it there — the
    // installer script's `~/.local/bin`, a package, or a hand copy. Resolving
    // it here means the unit never encodes an assumption about which.
    let exe = std::env::current_exe()?.canonicalize()?;

    // Beside the credential and host key, not beside the unit: it is the
    // daemon's runtime configuration, and it is `0600` for the same reason
    // they are.
    let env_file = crate::config::config_dir(scope)?.join("env");
    let carried = write_env_file(&env_file, &ambient_proxy_vars())?;
    if !carried.is_empty() {
        println!("carried {} into the service", carried.join(", "));
    }

    let dir = unit_dir(scope)?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(UNIT_NAME);
    std::fs::write(&path, unit_body(scope, &exe, &env_file))?;
    println!("wrote {}", path.display());

    // Every `systemctl` below, and nothing else about the two scopes
    // differs at this point.
    let scoped: &[&str] = match scope {
        Scope::User => &["--user"],
        Scope::System => &[],
    };
    let spelled = match scope {
        Scope::User => "systemctl --user",
        Scope::System => "systemctl",
    };

    if !start {
        println!("not started. to run it under systemd:");
        println!("  {spelled} daemon-reload");
        println!("  {spelled} enable --now {UNIT_NAME}");
        if scope == Scope::User {
            println!("  loginctl enable-linger \"$USER\"   # survive logout");
        }
        return Ok(());
    }

    // Without lingering, a user manager is torn down at logout and takes the
    // daemon with it — which for a host nobody stays logged into is every
    // reboot. Attempted before the unit starts so the first start already
    // happens under a manager that will persist, and only warned about on
    // failure: it needs polkit authorization this process may not have, and
    // an unlingered daemon still works for as long as someone is logged in.
    // A system unit has no session to outlive.
    if scope == Scope::User
        && let Err(error) = run("loginctl", &["enable-linger"])
    {
        eprintln!(
            "warning: could not enable lingering ({error}); the daemon will stop when this user logs out. \
             run `loginctl enable-linger \"$USER\"` as an administrator to fix that."
        );
    }

    let reload: Vec<&str> = [scoped, &["daemon-reload"]].concat();
    let enable: Vec<&str> = [scoped, &["enable", "--now", UNIT_NAME]].concat();
    if let Err(error) = run("systemctl", &reload).and_then(|()| run("systemctl", &enable)) {
        let bare = match scope {
            Scope::User => "faber-agent run",
            Scope::System => "faber-agent run --system",
        };
        eprintln!(
            "warning: the unit is written but could not be started ({error}). \
             enrollment succeeded — start the daemon with `{spelled} enable --now {UNIT_NAME}`, \
             or run `{bare}` directly."
        );
        return Ok(());
    }

    println!("started {UNIT_NAME}");
    Ok(())
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("{program}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    Err(format!(
        "{program} {} failed{}",
        args.join(" "),
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_runs_the_binary_it_was_written_from() {
        let body = unit_body(
            Scope::User,
            Path::new("/home/someone/.local/bin/faber-agent"),
            Path::new("/home/someone/.config/faber-agent/env"),
        );
        assert!(body.contains("ExecStart=/home/someone/.local/bin/faber-agent run\n"));
        // Enabling a unit with no [Install] section silently does nothing,
        // which would leave a daemon that runs now and never again.
        assert!(body.contains("WantedBy=default.target"));
        // Without this the daemon loses the proxy that is the only way off
        // the kind of host it is installed on.
        assert!(body.contains("EnvironmentFile=-/home/someone/.config/faber-agent/env"));
        // A user manager cannot order against a system target, so claiming
        // to would be a guarantee that does nothing.
        assert!(!body.contains("network-online.target"));
    }

    #[test]
    fn a_system_unit_starts_the_daemon_in_the_scope_it_was_installed_for() {
        let body = unit_body(
            Scope::System,
            Path::new("/usr/local/bin/faber-agent"),
            Path::new("/etc/faber-agent/env"),
        );
        // The scope is never detected at runtime: the unit states it, and
        // the daemon reads its identity from the directory that scope names.
        assert!(body.contains("ExecStart=/usr/local/bin/faber-agent run --system"));
        assert!(body.contains("WantedBy=multi-user.target"));
        assert!(body.contains("After=network-online.target"));
        // No `User=`: a system install's whole point is the authority
        // provisioning gave it, and dropping to an account here would take
        // away the cgroup and quota writes it exists to make.
        assert!(!body.contains("User="));
        // Where the tenancy transport's pid files go. Without this they
        // would land on the tenant filesystem and accumulate per launch.
        assert!(body.contains("RuntimeDirectory=faber-agent"));
    }

    #[test]
    fn the_env_file_carries_the_proxy_and_nothing_injected() {
        let dir = std::env::temp_dir().join(format!("faber-agent-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("env");

        let carried = write_env_file(
            &path,
            &[
                (
                    "HTTPS_PROXY".to_owned(),
                    "http://proxy.internal:3128".to_owned(),
                ),
                (
                    "NO_PROXY".to_owned(),
                    "localhost\nExecStart=/bin/sh".to_owned(),
                ),
            ],
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();

        assert!(body.contains("HTTPS_PROXY=http://proxy.internal:3128"));
        assert!(carried.contains(&"HTTPS_PROXY".to_owned()));
        // A value with a newline in it could otherwise smuggle a second
        // assignment into the file.
        assert!(!body.contains("ExecStart"));
        assert!(!carried.contains(&"NO_PROXY".to_owned()));

        std::fs::remove_dir_all(&dir).ok();
    }
}
