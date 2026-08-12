//! One rooted namespace.
//!
//! Every path in every signature is absolute *within the target's normalized
//! root*. No host paths, no `~`, no cwd-relative ambiguity between calls.
//! Escape is refused here, at the API boundary, in all modes — including
//! direct, where nothing underneath enforces it — because a surface that
//! differs by mode teaches mode-specific habits, and direct mode is where
//! those habits are most expensive.
//!
//! Paths are POSIX-shaped strings, not [`std::path::PathBuf`]. The target is
//! frequently another machine, and `Path`'s separator and prefix rules are
//! those of the *host* process. Normalizing an agent-supplied `/src/main.rs`
//! through host semantics is the silent-until-the-target-changes failure
//! rejects.
//!
//! **Refusal here is lexical.** A symlink inside the root pointing outside it
//! defeats a lexical check, and that hole is real in direct mode specifically.
//! Closing it is per-transport — see [`LocalTarget`](crate::LocalTarget),
//! which re-checks after resolution — because only the transport knows what
//! resolution means on the far side.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::fault::Denial;

/// A target's normalized agent-visible root (`host_container.root_path`).
///
/// Operator configuration, never agent input: the agent addresses a bound
/// label and paths under it, and the root itself never appears in a signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root(String);

impl Root {
    /// Normalizes and validates an absolute target-side path.
    ///
    /// The root is stored without its trailing slash, so `/` normalizes to the
    /// empty string and joining is unconditional concatenation.
    pub fn new(path: impl AsRef<str>) -> Result<Self, Denial> {
        let path = path.as_ref();
        let (normalized, escaped) = normalize(path)?;
        if escaped {
            return Err(Denial::PathEscape {
                path: path.to_owned(),
                reason: "root path traverses above `/`".into(),
            });
        }
        Ok(Root(normalized))
    }

    /// The root as it appears on the target, always with a leading slash.
    pub fn as_str(&self) -> &str {
        if self.0.is_empty() { "/" } else { &self.0 }
    }
}

impl fmt::Display for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An absolute path inside a target's root, checked at construction.
///
/// Carries both halves deliberately. `virtual_path` is what the agent said and
/// what every result echoes — `/src/main.rs` denotes a different file per
/// target, so a bare path in the transcript is ambiguous and a
/// `target:path` pair is not. `resolved` is what the transport uses and is
/// never shown to the agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootedPath {
    root: Root,
    virtual_path: String,
    resolved: String,
}

impl RootedPath {
    /// Resolves an agent-supplied path against `root`, refusing escape.
    ///
    /// Refused: anything not starting with `/` (cwd-relative, and cwd is
    /// per-call so there is nothing to be relative to), `~` in any position
    /// (there is no home directory in this namespace), embedded NULs, and any
    /// `..` sequence that walks above the root after lexical normalization.
    pub fn new(root: &Root, path: impl AsRef<str>) -> Result<Self, Denial> {
        let path = path.as_ref();

        if path.contains('\0') {
            return Err(Denial::Malformed {
                what: "path".into(),
                reason: "contains a NUL byte".to_owned(),
            });
        }
        if path.starts_with('~') {
            return Err(Denial::PathEscape {
                path: path.to_owned(),
                reason: "`~` names a host home directory, which is outside the target's root"
                    .into(),
            });
        }
        if !path.starts_with('/') {
            return Err(Denial::PathEscape {
                path: path.to_owned(),
                reason: "paths are absolute within the target's root; \
                         cwd is per-call and never persistent"
                    .into(),
            });
        }

        let (virtual_path, escaped) = normalize(path)?;
        if escaped {
            return Err(Denial::PathEscape {
                path: path.to_owned(),
                reason: "`..` traverses above the target's root".into(),
            });
        }

        Ok(RootedPath {
            resolved: format!("{}{}", root.0, virtual_path),
            root: root.clone(),
            virtual_path,
        })
    }

    /// The root this path was resolved against.
    pub fn root(&self) -> &Root {
        &self.root
    }

    /// The path as the agent wrote it, normalized. Always leading-slashed.
    pub fn as_str(&self) -> &str {
        if self.virtual_path.is_empty() {
            "/"
        } else {
            &self.virtual_path
        }
    }

    /// The path on the target. Transport-facing; never rendered to the agent.
    pub fn resolved(&self) -> &str {
        if self.resolved.is_empty() {
            "/"
        } else {
            &self.resolved
        }
    }

    /// Resolves `child` relative to this path, subject to the same refusals.
    pub fn join(&self, child: &str) -> Result<Self, Denial> {
        RootedPath::new(&self.root, format!("{}/{child}", self.as_str()))
    }

    /// This target's root as a path, the default `cwd` for [`Exec`](crate::Exec).
    pub fn root_of(root: &Root) -> Self {
        RootedPath {
            root: root.clone(),
            virtual_path: String::new(),
            resolved: root.0.clone(),
        }
    }
}

impl fmt::Display for RootedPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lexically normalizes an absolute POSIX path.
///
/// Returns the normalized path (empty for `/`, otherwise leading-slashed with
/// no trailing slash) and whether a `..` walked above the top. Purely
/// lexical: no filesystem is consulted, because the filesystem in question is
/// usually not this process's.
fn normalize(path: &str) -> Result<(String, bool), Denial> {
    if !path.starts_with('/') {
        return Err(Denial::Malformed {
            what: "path".into(),
            reason: format!("`{path}` is not absolute"),
        });
    }

    let mut segments: Vec<&str> = Vec::new();
    let mut escaped = false;

    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    escaped = true;
                }
            }
            other => segments.push(other),
        }
    }

    let normalized = if segments.is_empty() {
        String::new()
    } else {
        format!("/{}", segments.join("/"))
    };

    Ok((normalized, escaped))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> Root {
        Root::new("/work/repo/").unwrap()
    }

    #[test]
    fn a_root_keeps_no_trailing_slash() {
        assert_eq!(root().as_str(), "/work/repo");
        assert_eq!(Root::new("/").unwrap().as_str(), "/");
    }

    #[test]
    fn a_path_resolves_under_the_root_without_the_agent_seeing_it() {
        let path = RootedPath::new(&root(), "/src/main.rs").unwrap();
        assert_eq!(path.as_str(), "/src/main.rs");
        assert_eq!(path.resolved(), "/work/repo/src/main.rs");
    }

    #[test]
    fn a_path_escaping_the_root_is_denied() {
        let denial = RootedPath::new(&root(), "/src/../../etc/passwd").unwrap_err();
        assert!(matches!(denial, Denial::PathEscape { .. }));
    }

    #[test]
    fn interior_dot_dot_that_stays_inside_the_root_is_allowed() {
        let path = RootedPath::new(&root(), "/src/../Cargo.toml").unwrap();
        assert_eq!(path.as_str(), "/Cargo.toml");
    }

    #[test]
    fn a_host_absolute_path_lands_inside_the_root_rather_than_on_the_host() {
        // `/etc/passwd` is the target's `/etc/passwd`, which under
        // a root is a file that almost certainly does not exist. There is no
        // spelling of a host path in this namespace.
        let path = RootedPath::new(&root(), "/etc/passwd").unwrap();
        assert_eq!(path.resolved(), "/work/repo/etc/passwd");
    }

    #[test]
    fn a_tilde_path_is_denied() {
        let denial = RootedPath::new(&root(), "~/.ssh/id_rsa").unwrap_err();
        assert!(matches!(denial, Denial::PathEscape { .. }));
    }

    #[test]
    fn a_relative_path_is_denied_rather_than_guessed_at() {
        let denial = RootedPath::new(&root(), "src/main.rs").unwrap_err();
        assert!(matches!(denial, Denial::PathEscape { .. }));
    }

    #[test]
    fn a_join_is_subject_to_the_same_refusal() {
        let src = RootedPath::new(&root(), "/src").unwrap();
        assert_eq!(src.join("main.rs").unwrap().as_str(), "/src/main.rs");
        assert!(src.join("../../etc").is_err());
    }
}
