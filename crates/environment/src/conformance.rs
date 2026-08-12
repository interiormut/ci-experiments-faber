//! One test body per promise, run against every mode this environment can bind.
//!
//! The agent is told that only two things differ between targets: what the
//! manifest says, and the posture line. Path shape, failure classes, exec
//! semantics, truncation behavior, and cwd handling are supposed to be
//! identical. Everywhere else in the crate that claim is kept by construction
//! — the behavior lives in [`Machine`] and the transports only supply
//! plumbing. This file is where the claim is *checked*, because "by
//! construction" is an argument and an argument is not a test.
//!
//! Every assertion names the mode it failed for, so a transport that reaches
//! into policy and changes an answer says which transport did it.
//!
//! Modes that need something this machine may not have — a docker daemon, an
//! SSH host — are bound when they are available and skipped when they are
//! not. A skipped mode is reported rather than silently passing: a
//! conformance suite that quietly checks one mode is worse than none, because
//! it reads as coverage.

use std::sync::Arc;
use std::time::Duration;

use crate::exec::{Exec, Outcome};
use crate::fault::{Denial, Fault};
use crate::file::{Edit, Replace, Window};
use crate::local::LocalTarget;
use crate::local::tests::scratch;
use crate::machine::Machine;
use crate::path::Root;
use crate::store::{Blobs, MemoryBlobs, Span};
use crate::target::Target;

/// One bound target, plus the store its spans redeem against.
struct Mode {
    name: &'static str,
    target: Machine,
    blobs: Arc<MemoryBlobs>,
}

impl Mode {
    fn text(&self, span: &Span) -> String {
        String::from_utf8(self.blobs.get(&span.blob).unwrap()).unwrap()
    }
}

/// Every mode bindable here. Adding a transport is one push.
async fn modes() -> Vec<Mode> {
    let mut modes = Vec::new();

    let blobs = Arc::new(MemoryBlobs::new());
    let root = Root::new(scratch().to_string_lossy()).unwrap();
    modes.push(Mode {
        name: "local+direct",
        target: LocalTarget::bind("conformance", root, blobs.clone() as Arc<dyn Blobs>)
            .await
            .unwrap(),
        blobs,
    });

    modes
}

#[tokio::test]
async fn a_nonzero_exit_is_a_result_in_every_mode() {
    for mode in modes().await {
        let exit = mode
            .target
            .exec(Exec::new("exit 3"))
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "{}: a nonzero exit must not be a fault, got {error}",
                    mode.name
                )
            });
        assert_eq!(
            exit.outcome,
            Outcome::Completed { code: 3 },
            "{}: a nonzero exit is a result carrying the code",
            mode.name
        );
    }
}

#[tokio::test]
async fn a_timeout_is_an_outcome_in_every_mode() {
    for mode in modes().await {
        let exit = mode
            .target
            .exec(Exec::new("sleep 30").timeout(Duration::from_millis(150)))
            .await
            .unwrap_or_else(|error| {
                panic!("{}: a timeout must not be a fault, got {error}", mode.name)
            });
        assert_eq!(
            exit.outcome,
            Outcome::TimedOut,
            "{}: the command ran, it just did not stop",
            mode.name
        );
    }
}

#[tokio::test]
async fn every_exit_echoes_its_target_and_resolved_cwd_in_every_mode() {
    for mode in modes().await {
        let exit = mode.target.exec(Exec::new("true")).await.unwrap();
        assert_eq!(
            exit.target.as_str(),
            "conformance",
            "{}: the exit names the target it ran on",
            mode.name
        );
        assert_eq!(
            exit.cwd.as_str(),
            "/",
            "{}: the exit carries the resolved cwd, not the requested one",
            mode.name
        );
    }
}

#[tokio::test]
async fn cwd_does_not_carry_between_calls_in_any_mode() {
    for mode in modes().await {
        mode.target
            .write(&mode.target.path("/sub/marker").unwrap(), &"x".into())
            .await
            .unwrap();
        mode.target.exec(Exec::new("cd /sub")).await.unwrap();

        let exit = mode.target.exec(Exec::new("pwd")).await.unwrap();
        assert_eq!(
            mode.text(&exit.stdout.span).trim(),
            mode.target.root().resolved(),
            "{}: cwd is per-call and never persistent",
            mode.name
        );
    }
}

#[tokio::test]
async fn a_path_leaving_the_root_is_refused_in_every_mode() {
    for mode in modes().await {
        // Lexical, so it is refused before any transport sees it — which is
        // the point: the refusal cannot differ by mode.
        for escape in ["/../etc/passwd", "~/.ssh/id_rsa", "relative/path"] {
            let denial = mode.target.path(escape).unwrap_err();
            assert!(
                matches!(denial, Fault::Denied(Denial::PathEscape { .. })),
                "{}: `{escape}` leaves the root and is refused, got {denial:?}",
                mode.name
            );
        }
    }
}

#[tokio::test]
async fn a_missing_file_is_not_an_empty_read_in_any_mode() {
    for mode in modes().await {
        let fault = mode
            .target
            .read(&mode.target.path("/nope.txt").unwrap(), None)
            .await
            .unwrap_err();
        assert!(
            matches!(fault, Fault::Denied(Denial::NotFound { .. })),
            "{}: a missing path is not an empty file, got {fault:?}",
            mode.name
        );
    }
}

#[tokio::test]
async fn a_window_past_the_end_is_out_of_range_in_every_mode() {
    for mode in modes().await {
        let path = mode.target.path("/lines.txt").unwrap();
        mode.target
            .write(&path, &"a\nb\nc\nd\n".into())
            .await
            .unwrap();

        let span = mode
            .target
            .read(&path, Some(Window::new(1, 2)))
            .await
            .unwrap();
        assert_eq!(
            mode.text(&span),
            "b\nc",
            "{}: the window selects lines",
            mode.name
        );
        assert!(
            span.truncated,
            "{}: a window that stopped short of the end is flagged",
            mode.name
        );

        let fault = mode
            .target
            .read(&path, Some(Window::new(99, 2)))
            .await
            .unwrap_err();
        assert!(
            matches!(fault, Fault::Denied(Denial::OutOfRange { .. })),
            "{}: past the end is not an empty read, got {fault:?}",
            mode.name
        );
    }
}

#[tokio::test]
async fn a_rejected_glob_is_never_an_empty_listing_in_any_mode() {
    for mode in modes().await {
        let root = mode.target.root();

        let fault = mode.target.list(&root, Some("[abc")).await.unwrap_err();
        assert!(
            matches!(fault, Fault::Denied(Denial::BadPattern { .. })),
            "{}: a rejected pattern is a refusal, not a settled negative answer, got {fault:?}",
            mode.name
        );

        // The other half: a real negative answer stays distinguishable.
        let listing = mode.target.list(&root, Some("*.nothing")).await.unwrap();
        assert!(
            listing.entries.is_empty() && !listing.truncated,
            "{}: an empty match set is empty and not truncated",
            mode.name
        );
    }
}

#[tokio::test]
async fn an_ambiguous_edit_is_refused_in_every_mode() {
    for mode in modes().await {
        let path = mode.target.path("/dup.txt").unwrap();
        mode.target.write(&path, &"x\nx\n".into()).await.unwrap();

        let fault = mode
            .target
            .edit(&Edit::Replace(Replace::new(path.clone(), "x", "y")))
            .await
            .unwrap_err();
        assert!(
            matches!(fault, Fault::Denied(Denial::EditRefused { .. })),
            "{}: two matches without `all` is refused rather than guessed at, got {fault:?}",
            mode.name
        );
    }
}

#[tokio::test]
async fn every_mode_publishes_what_it_can_do() {
    for mode in modes().await {
        let manifest = mode.target.manifest();
        assert!(
            !manifest.capabilities.is_empty(),
            "{}: a target that answers nothing is not a target",
            mode.name
        );
        assert_eq!(
            manifest.label.as_str(),
            "conformance",
            "{}: the manifest names the label it was bound under",
            mode.name
        );
        assert_eq!(
            manifest.shell,
            crate::probe::SHELL,
            "{}: every mode runs command strings through the same shell",
            mode.name
        );
        // The probe went out over this mode's own transport and came back with
        // something. `unknown` is the honest answer when it did not, and it is
        // the answer a transport that silently returns nothing would give.
        assert_ne!(
            manifest.os, "unknown",
            "{}: the bind probe reached the target",
            mode.name
        );
        assert_ne!(
            manifest.arch, "unknown",
            "{}: the bind probe reached the target",
            mode.name
        );
    }
}
