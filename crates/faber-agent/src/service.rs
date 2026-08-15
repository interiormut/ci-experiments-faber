//! The systemd *user* unit `install` writes and starts.
//!
//! A user service, never a system one. The daemon's privileges were settled
//! the moment someone ran `install` under their own account, and a
//! `systemctl --user` unit is what keeps that the only scoping decision
//! rather than a second one layered on top: there is no root to install as
//! and therefore no privilege to drop.
//!
//! Everything here is best-effort by design. A machine without systemd, or
//! one reached over a bare `docker exec` with no user session bus, is still
//! a machine the daemon runs fine on — it just has to be started some other
//! way. Failing the whole install because supervision could not be arranged
//! would throw away a completed enrollment, which is the expensive half.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `$XDG_CONFIG_HOME/systemd/user`, falling back to `~/.config/systemd/user`
/// — where `systemctl --user` looks for units it did not install itself.
fn unit_dir() -> std::io::Result<PathBuf> {
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

fn unit_body(exe: &Path, env_file: &Path) -> String {
    // `Restart=always` overlaps with the daemon's own reconnect loop on
    // purpose: the loop covers a dropped connection, and this covers the
    // process itself dying, which the loop by definition cannot.
    //
    // No `After=` ordering: the usual `network-online.target` is a *system*
    // target a user manager cannot order against, so writing it would look
    // like a guarantee while doing nothing. The reconnect loop is the real
    // answer to starting before the network is up.
    //
    // `EnvironmentFile` with a leading `-` so a missing file is not a
    // startup failure — see `write_env_file` for why the file exists at all.
    format!(
        "[Unit]\n\
         Description=Faber agent\n\
         \n\
         [Service]\n\
         EnvironmentFile=-{env_file}\n\
         ExecStart={exe} run\n\
         Restart=always\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
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
pub fn install_unit(start: bool) -> std::io::Result<()> {
    // The absolute path of *this* binary, whatever put it there — the
    // installer script's `~/.local/bin`, a package, or a hand copy. Resolving
    // it here means the unit never encodes an assumption about which.
    let exe = std::env::current_exe()?.canonicalize()?;

    // Beside the credential and host key, not beside the unit: it is the
    // daemon's runtime configuration, and it is `0600` for the same reason
    // they are.
    let env_file = crate::config::config_dir()?.join("env");
    let carried = write_env_file(&env_file, &ambient_proxy_vars())?;
    if !carried.is_empty() {
        println!("carried {} into the service", carried.join(", "));
    }

    let dir = unit_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(UNIT_NAME);
    std::fs::write(&path, unit_body(&exe, &env_file))?;
    println!("wrote {}", path.display());

    if !start {
        println!("not started. to run it under systemd:");
        println!("  systemctl --user daemon-reload");
        println!("  systemctl --user enable --now {UNIT_NAME}");
        println!("  loginctl enable-linger \"$USER\"   # survive logout");
        return Ok(());
    }

    // Without lingering, a user manager is torn down at logout and takes the
    // daemon with it — which for a host nobody stays logged into is every
    // reboot. Attempted before the unit starts so the first start already
    // happens under a manager that will persist, and only warned about on
    // failure: it needs polkit authorization this process may not have, and
    // an unlingered daemon still works for as long as someone is logged in.
    if let Err(error) = run("loginctl", &["enable-linger"]) {
        eprintln!(
            "warning: could not enable lingering ({error}); the daemon will stop when this user logs out. \
             run `loginctl enable-linger \"$USER\"` as an administrator to fix that."
        );
    }

    if let Err(error) = run("systemctl", &["--user", "daemon-reload"])
        .and_then(|()| run("systemctl", &["--user", "enable", "--now", UNIT_NAME]))
    {
        eprintln!(
            "warning: the unit is written but could not be started ({error}). \
             enrollment succeeded — start the daemon with `systemctl --user enable --now {UNIT_NAME}`, \
             or run `faber-agent run` directly."
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
            Path::new("/home/someone/.local/bin/faber-agent"),
            Path::new("/home/someone/.config/faber-agent/env"),
        );
        assert!(body.contains("ExecStart=/home/someone/.local/bin/faber-agent run"));
        // Enabling a unit with no [Install] section silently does nothing,
        // which would leave a daemon that runs now and never again.
        assert!(body.contains("WantedBy=default.target"));
        // Without this the daemon loses the proxy that is the only way off
        // the kind of host it is installed on.
        assert!(body.contains("EnvironmentFile=-/home/someone/.config/faber-agent/env"));
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
