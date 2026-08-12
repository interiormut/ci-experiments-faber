//! File verbs, first-class rather than shelled out.
//!
//! `cat`/`sed`/`find` behavior varies by image — busybox versus GNU is a real
//! violation waiting in every alpine container — and the edit/parse/repair
//! envelope is the harness's strongest justification for existing at all.
//!
//! Two shapes are deliberate:
//!
//! - The lean approach is kept: `write` and `edit` stay separate verbs. An edit is
//!   a write with a precondition; folding them costs the model a
//!   full-file-rewrite habit on large files.
//! - The wider tool-surface shape is applied: [`Edit`] is wider than the original
//!   `edit(path, Edit)`. A rename and a cross-file atomic apply cannot be
//!   expressed against a single path, and Codex-family projection is built on
//!   exactly that envelope. Whether the patch applies atomically
//!   depends on the transport; [`LocalTarget`](crate::LocalTarget) documents
//!   what it actually guarantees.
//!
//! Not here: content search. The design leans toward adding `grep` and away from
//! embeddings; until it is decided, `rg` through
//! [`exec`](crate::Target::exec) works and returns unstructured text.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::path::RootedPath;

/// A line window into a file. Absent means the whole file.
///
/// Lines, not bytes: the agent reads source, and an offset in bytes is not
/// something it can compute from a previous read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// 0-based first line.
    pub offset: u64,
    pub limit: u64,
}

impl Window {
    pub fn new(offset: u64, limit: u64) -> Self {
        Window { offset, limit }
    }
}

/// What a write or an edit left behind. Echoes the path (A1) so no unqualified
/// path enters the transcript.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stat {
    pub path: RootedPath,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// One directory entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub path: RootedPath,
    pub kind: EntryKind,
    /// `None` for anything without a meaningful size.
    pub size: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// A directory listing, capped and *flagged* when capped (A5).
///
/// Borrowed directly from `Glob`'s cap-plus-flag: a capped result the model
/// reads as complete is worse than no result, because it terminates the
/// search. An empty `entries` with `truncated: false` is a real negative
/// answer, and it stays distinct from a refused glob
/// ([`Denial::BadPattern`](crate::Denial::BadPattern)) and a missing directory
/// ([`Denial::NotFound`](crate::Denial::NotFound)).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

/// How many entries a listing returns before flagging truncation.
pub const LIST_CAP: usize = 1000;

/// An edit, at the widest shape the trait must express.
///
/// [`Edit::Replace`] is the Claude-family primitive and [`Edit::Patch`] the
/// Codex-family one; both project from here, which is the point — the
/// environment layer must not assume whether editing is one verb or three.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "edit", rename_all = "snake_case")]
pub enum Edit {
    Replace(Replace),
    Patch(Patch),
}

/// Exact string replacement, with the match count as a precondition.
///
/// A non-matching or ambiguous anchor is
/// [`Denial::EditRefused`](crate::Denial::EditRefused) — deterministic, so the
/// agent must change the request rather than retry it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Replace {
    pub path: RootedPath,
    pub old: String,
    pub new: String,
    /// Without this, more than one match is a refusal rather than a guess.
    #[serde(default)]
    pub all: bool,
}

impl Replace {
    pub fn new(path: RootedPath, old: impl Into<String>, new: impl Into<String>) -> Self {
        Replace {
            path,
            old: old.into(),
            new: new.into(),
            all: false,
        }
    }

    pub fn all(mut self) -> Self {
        self.all = true;
        self
    }
}

/// A set of operations over the target's files — create, edit, delete, rename
/// — as one request.
///
/// This is Codex's `apply_patch` envelope in typed form. Atomicity across ops
/// is a transport property, not a promise made here.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Patch {
    pub ops: Vec<PatchOp>,
}

impl Patch {
    pub fn new(ops: Vec<PatchOp>) -> Self {
        Patch { ops }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PatchOp {
    Add { path: RootedPath, body: Vec<u8> },
    Update(Replace),
    Delete { path: RootedPath },
    Move { from: RootedPath, to: RootedPath },
}
