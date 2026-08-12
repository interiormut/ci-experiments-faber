//! Bound labels.
//!
//! **S1: the target is a required parameter on every call and is never
//! defaulted.** Ambient selection (S2) stops the call from recording where it
//! ran and turns every path-taking tool into a place the resolution can be
//! forgotten — Codex had to patch `view_image` for exactly this. Namespaced
//! per-target tools (S3) make the tool list a function of the bindings, which
//! is not merely worse ergonomically: it forecloses mid-session binding, since
//! new tools rewrite the schema block near the top of the prefix.
//!
//! With S1, binding a target mid-session touches **zero prefix bytes**. The
//! schema already has a `target` parameter of type string; a new binding is an
//! appended event carrying a label and a manifest, at the end of the context.
//!
//! **The binding set is append-only within a run; the tool schema is
//! immutable within a run.** These are two different objects and the old design conflated
//! them. Unbinding removes nothing: it tombstones, and later calls against the
//! label answer [`Denial::NotBound`].
//!
//! Bind events are transcript, not configuration. A run resumed from its
//! exchange log replays its bindings in order and arrives at the same target
//! set; a binding stored only in a config table is invisible to replay. This
//! type is the in-memory projection of that log, not its home.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::fault::{Denial, Fault};
use crate::manifest::Manifest;
use crate::target::Target;

/// A user-chosen name for a bound target — `"build"`, not a uuid.
///
/// Binding is consumer-side, so the run holds the mapping and the agent
/// cannot name what is not bound. Re-exposing host and container identity
/// would hand back a decision the user already made at bind time.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Label(pub String);

impl Label {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Label {
    fn from(label: &str) -> Self {
        Label(label.to_owned())
    }
}

impl From<String> for Label {
    fn from(label: String) -> Self {
        Label(label)
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One entry in the append-only binding log.
#[derive(Clone)]
pub struct Binding {
    pub label: Label,
    pub target: Arc<dyn Target>,
    pub bound_at: SystemTime,
    /// Set by [`Registry::unbind`]. The entry is never removed.
    pub tombstoned_at: Option<SystemTime>,
}

impl Binding {
    pub fn live(&self) -> bool {
        self.tombstoned_at.is_none()
    }

    pub fn manifest(&self) -> &Manifest {
        self.target.manifest()
    }
}

impl fmt::Debug for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Binding")
            .field("label", &self.label)
            .field("bound_at", &self.bound_at)
            .field("tombstoned_at", &self.tombstoned_at)
            .finish_non_exhaustive()
    }
}

/// The run's label → target mapping.
#[derive(Default)]
pub struct Registry {
    bindings: Vec<Binding>,
    index: HashMap<Label, usize>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a binding.
    ///
    /// A label is claimed once and stays claimed. Reusing one — live or
    /// tombstoned — is refused, because the transcript records calls as
    /// `label:path` and a label that meant two machines makes the earlier half
    /// of the transcript quietly wrong.
    pub fn bind(
        &mut self,
        label: impl Into<Label>,
        target: Arc<dyn Target>,
    ) -> Result<&Binding, Denial> {
        let label = label.into();

        if label.0.is_empty() {
            return Err(Denial::Malformed {
                what: "label".into(),
                reason: "a target label cannot be empty".to_owned(),
            });
        }
        if let Some(existing) = self.index.get(&label) {
            let existing = &self.bindings[*existing];
            return Err(Denial::Malformed {
                what: "label".into(),
                reason: if existing.live() {
                    format!("`{label}` is already bound")
                } else {
                    format!("`{label}` was bound and unbound; labels are not reused")
                },
            });
        }

        self.index.insert(label.clone(), self.bindings.len());
        self.bindings.push(Binding {
            label,
            target,
            bound_at: SystemTime::now(),
            tombstoned_at: None,
        });
        Ok(self.bindings.last().expect("just pushed"))
    }

    /// Tombstones a binding. Nothing is removed.
    pub fn unbind(&mut self, label: &Label) -> Result<(), Denial> {
        let index = *self.index.get(label).ok_or_else(|| Denial::NotBound {
            label: label.0.clone(),
        })?;
        let binding = &mut self.bindings[index];
        if binding.tombstoned_at.is_none() {
            binding.tombstoned_at = Some(SystemTime::now());
        }
        Ok(())
    }

    /// Resolves a label to the target behind it.
    ///
    /// Never defaulted: a defaulted target means a forgotten parameter
    /// silently routes to a machine the model was not thinking about, and the
    /// failures that produces — `rm -rf`, a migration, a service restart — are
    /// exactly the destructive ones. An omitted required parameter is a
    /// validation error the model corrects in one turn.
    pub fn target(&self, label: &Label) -> Result<Arc<dyn Target>, Fault> {
        let binding = self
            .index
            .get(label)
            .map(|index| &self.bindings[*index])
            .filter(|binding| binding.live())
            .ok_or_else(|| {
                Fault::Denied(Denial::NotBound {
                    label: label.0.clone(),
                })
            })?;
        Ok(Arc::clone(&binding.target))
    }

    /// Currently bound labels. Enumerates what is bound, never what exists on
    /// the host.
    pub fn live(&self) -> impl Iterator<Item = &Binding> {
        self.bindings.iter().filter(|binding| binding.live())
    }

    /// The whole log, tombstones included — this is what replays.
    pub fn log(&self) -> &[Binding] {
        &self.bindings
    }

    /// Every live target's manifest.
    ///
    /// The agent sees all of them at once, on purpose: differences between
    /// targets are the thing it most needs to reason about — one has `cargo`,
    /// the other has network.
    pub fn manifests(&self) -> Vec<&Manifest> {
        self.live().map(Binding::manifest).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::local::tests::stub_target;

    #[test]
    fn a_call_against_a_tombstoned_label_is_not_bound() {
        let mut registry = Registry::new();
        registry
            .bind("build", Arc::new(stub_target("build")))
            .unwrap();
        assert!(registry.target(&"build".into()).is_ok());

        registry.unbind(&"build".into()).unwrap();

        let fault = registry.target(&"build".into()).err().unwrap();
        assert!(matches!(fault, Fault::Denied(Denial::NotBound { .. })));
        // Tombstoned, not removed.
        assert_eq!(registry.log().len(), 1);
        assert_eq!(registry.live().count(), 0);
    }

    #[test]
    fn an_unbound_label_is_not_bound_rather_than_missing() {
        let registry = Registry::new();
        let fault = registry.target(&"nowhere".into()).err().unwrap();
        assert!(matches!(fault, Fault::Denied(Denial::NotBound { .. })));
    }

    #[test]
    fn a_label_is_never_reused() {
        let mut registry = Registry::new();
        registry
            .bind("build", Arc::new(stub_target("build")))
            .unwrap();
        assert!(
            registry
                .bind("build", Arc::new(stub_target("build")))
                .is_err()
        );

        registry.unbind(&"build".into()).unwrap();
        assert!(
            registry
                .bind("build", Arc::new(stub_target("build")))
                .is_err()
        );
    }
}
