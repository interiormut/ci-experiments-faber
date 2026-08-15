//! The CRUD surface, and the one file it keeps in step.
//!
//! The in-memory entry set is authoritative and the file is its projection.
//! The alternative — reading the entries back out of the rendered document on
//! startup — was rejected: recovering a target from `http://web:8080` means
//! guessing whether `web` is a container name or the operator's host address,
//! and the guess is wrong exactly when the two coincide. Faber's API owns the
//! durable copy in its database and hands it back with [`Traefik::replace`]
//! at startup, so a second, lossy source of truth buys nothing.
//!
//! Every mutation rewrites the whole document. The write is
//! temporary-file-then-rename, which matters twice: Traefik watches this path
//! and must never observe a half-written file, and a failed write must not
//! leave the entry set and the file disagreeing. The temporary file is
//! `.<name>.tmp` in the same directory — same filesystem, so the rename is
//! atomic; dot-prefixed and `.tmp`-suffixed, so a Traefik pointed at the
//! *directory* skips it instead of loading a second copy of every router.
//!
//! The mutex is held across the render and the write. Two concurrent updates
//! could otherwise both succeed while the file ends up matching the loser:
//! the file's content is part of the mutation, not a consequence of it.
//!
//! No reload call exists because none is needed. The file provider watches
//! its target and applies changes within a couple of seconds; there is no
//! signal to send and no Traefik API to call.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::domain::Domain;
use crate::entry::{Entry, Target};
use crate::error::{Error, Result};
use crate::render;

/// One instance per Traefik. Cheap to share behind an `Arc`; all methods
/// take `&self`.
pub struct Traefik {
    config: Config,
    entries: Mutex<BTreeMap<Domain, Entry>>,
}

impl Traefik {
    /// Build a manager. Touches no file — call [`Traefik::replace`] with the
    /// durable entry set (possibly empty) to publish the initial document.
    pub fn new(config: Config) -> Self {
        Traefik {
            config,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Add an entry. Fails with [`Error::AlreadyExists`] if the domain is
    /// taken — two routers matching one host is a coin toss, so the conflict
    /// is reported rather than resolved.
    pub async fn create(&self, entry: Entry) -> Result<Entry> {
        let mut entries = self.entries.lock().await;
        if entries.contains_key(&entry.domain) {
            return Err(Error::AlreadyExists {
                domain: entry.domain,
            });
        }
        let mut next = entries.clone();
        next.insert(entry.domain.clone(), entry.clone());
        self.publish(&next).await?;
        *entries = next;
        Ok(entry)
    }

    /// Repoint an existing domain. Fails with [`Error::NotFound`] if there is
    /// nothing to repoint; use [`Traefik::put`] to not care.
    pub async fn update(&self, domain: &Domain, target: Target) -> Result<Entry> {
        let mut entries = self.entries.lock().await;
        if !entries.contains_key(domain) {
            return Err(Error::NotFound {
                domain: domain.clone(),
            });
        }
        let entry = Entry::new(domain.clone(), target);
        let mut next = entries.clone();
        next.insert(domain.clone(), entry.clone());
        self.publish(&next).await?;
        *entries = next;
        Ok(entry)
    }

    /// Create or repoint, whichever applies.
    pub async fn put(&self, entry: Entry) -> Result<Entry> {
        let mut entries = self.entries.lock().await;
        let mut next = entries.clone();
        next.insert(entry.domain.clone(), entry.clone());
        self.publish(&next).await?;
        *entries = next;
        Ok(entry)
    }

    pub async fn get(&self, domain: &Domain) -> Option<Entry> {
        self.entries.lock().await.get(domain).cloned()
    }

    /// Every entry, ordered by domain.
    pub async fn list(&self) -> Vec<Entry> {
        self.entries.lock().await.values().cloned().collect()
    }

    /// Remove an entry. Returns whether there was one, so a caller replaying
    /// a deletion is not forced to treat "already gone" as a failure.
    pub async fn delete(&self, domain: &Domain) -> Result<bool> {
        let mut entries = self.entries.lock().await;
        if !entries.contains_key(domain) {
            return Ok(false);
        }
        let mut next = entries.clone();
        next.remove(domain);
        self.publish(&next).await?;
        *entries = next;
        Ok(true)
    }

    /// Replace the entire entry set, and with it the file.
    ///
    /// This is the startup path: hand over what the database says, and
    /// whatever a previous process left behind is gone. A duplicated domain
    /// in `entries` is [`Error::AlreadyExists`] rather than a silent
    /// last-one-wins.
    pub async fn replace(&self, entries: impl IntoIterator<Item = Entry>) -> Result<()> {
        let mut next = BTreeMap::new();
        for entry in entries {
            if let Some(existing) = next.insert(entry.domain.clone(), entry) {
                return Err(Error::AlreadyExists {
                    domain: existing.domain,
                });
            }
        }

        let mut held = self.entries.lock().await;
        self.publish(&next).await?;
        *held = next;
        Ok(())
    }

    /// Render and write. The caller commits its in-memory change only after
    /// this returns `Ok`, so a filesystem failure leaves both halves at the
    /// previous state.
    async fn publish(&self, entries: &BTreeMap<Domain, Entry>) -> Result<()> {
        let document = render::render(&self.config, entries.values())?;
        let path = self.config.file.clone();
        let temp = temp_path(&path);

        // The failing path is named, not the destination: a read-only
        // directory fails on the temporary file, and reporting the target
        // instead sends whoever reads the error to the wrong `ls`.
        let io = |at: &Path| {
            let at = at.to_path_buf();
            move |source| Error::Io {
                path: at.clone(),
                source,
            }
        };

        let mut file = tokio::fs::File::create(&temp).await.map_err(io(&temp))?;
        file.write_all(document.as_bytes())
            .await
            .map_err(io(&temp))?;
        // Rename publishes the bytes; without the sync the rename can land
        // ahead of them across a crash and Traefik picks up an empty file.
        file.sync_all().await.map_err(io(&temp))?;
        drop(file);
        tokio::fs::rename(&temp, &path).await.map_err(io(&path))?;

        tracing::debug!(path = %path.display(), entries = entries.len(), "published traefik dynamic configuration");
        Ok(())
    }
}

/// `/etc/traefik/dynamic/faber.yml` → `/etc/traefik/dynamic/.faber.yml.tmp`.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "faber".to_owned());
    path.with_file_name(format!(".{name}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_file_is_hidden_and_unparseable_by_traefik() {
        let temp = temp_path(&PathBuf::from("/etc/traefik/dynamic/faber.yml"));
        assert_eq!(
            temp,
            PathBuf::from("/etc/traefik/dynamic/.faber.yml.tmp"),
            "must share the directory for the rename to be atomic"
        );
        let name = temp.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with('.'));
        assert!(name.ends_with(".tmp"));
    }
}
