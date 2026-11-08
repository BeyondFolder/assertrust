//! The input layer of assertrust (`docs/architecture.md`, step 1).
//!
//! It turns "check this range / these staged changes / these edits" into before/after
//! comparisons, plus the commits that produced them, with every changed file labelled by
//! kind (test, gate, source, …). It shells out to `git` and knows nothing about tests or
//! rules, so other tools can reuse it.
//!
//! ```no_run
//! use assertrust_git::{analyze, Classifier, Mode};
//!
//! let analysis = analyze(".".as_ref(), &Mode::Range("main...HEAD".into()), &Classifier::default())?;
//! for step in &analysis.steps {
//!     println!("{} touches {} files", step.commit.short_sha(), step.comparison.files.len());
//! }
//! # Ok::<(), assertrust_git::Error>(())
//! ```

mod classify;
mod diff;
mod error;
mod git;
mod mode;
mod model;

use std::path::Path;

pub use crate::classify::Classifier;
pub use crate::error::Error;
pub use crate::mode::Mode;
pub use crate::model::{
    Analysis, ChangeStatus, Commit, Comparison, FileChange, FileKind, ModeKind, Rev, Step,
};

use crate::git::Git;
use crate::mode::Resolved;

/// Works out what `mode` compares in the repo at `dir`.
pub fn analyze(dir: &Path, mode: &Mode, classifier: &Classifier) -> Result<Analysis, Error> {
    let git = Git::open(dir)?;

    match mode::resolve(&git, mode)? {
        Resolved::PerCommit { base, head, label } => {
            let (steps, skipped_merges) = steps(&git, &base, &head, classifier)?;
            Ok(Analysis {
                kind: ModeKind::PerCommit,
                label,
                net: None,
                steps,
                skipped_merges,
            })
        }
        Resolved::PrStyle { base, head, label } => {
            let merge_base = git.merge_base(&base, &head)?;
            let files = diff::classify(git.diff_trees(&merge_base, &head)?, classifier);
            let net = Comparison {
                before: Rev::Commit(merge_base),
                after: Rev::Commit(head.clone()),
                files,
            };
            let (steps, skipped_merges) = steps(&git, &base, &head, classifier)?;
            Ok(Analysis {
                kind: ModeKind::PrStyle,
                label,
                net: Some(net),
                steps,
                skipped_merges,
            })
        }
        Resolved::Staged => {
            let before = git.head_or_empty_tree()?;
            let files = diff::classify(git.diff_staged(&before)?, classifier);
            Ok(uncommitted(ModeKind::Staged, before, Rev::Index, files))
        }
        Resolved::WorkTree => {
            let before = git.head_or_empty_tree()?;
            let files = diff::classify(git.diff_worktree(&before)?, classifier);
            Ok(uncommitted(
                ModeKind::WorkTree,
                before,
                Rev::WorkTree,
                files,
            ))
        }
    }
}

/// One comparison per commit in `base..head` (the commits `head` adds), oldest first.
/// Merge commits are skipped: the commits they bring in are walked one by one instead.
/// What a merge changes on its own (a conflict resolution) is only covered by PR-style's
/// net comparison; see "Merge commits" in `docs/architecture.md`, step 1.
fn steps(
    git: &Git,
    base: &str,
    head: &str,
    classifier: &Classifier,
) -> Result<(Vec<Step>, Vec<Commit>), Error> {
    let mut steps = Vec::new();
    let mut skipped_merges = Vec::new();
    for commit in git.commits(base, head)? {
        if commit.is_merge() {
            skipped_merges.push(commit);
            continue;
        }
        let before = match commit.parents.first() {
            Some(parent) => parent.clone(),
            None => git.empty_tree()?,
        };
        let files = diff::classify(git.diff_commit(&commit.sha)?, classifier);
        steps.push(Step {
            comparison: Comparison {
                before: Rev::Commit(before),
                after: Rev::Commit(commit.sha.clone()),
                files,
            },
            commit,
        });
    }
    Ok((steps, skipped_merges))
}

fn uncommitted(kind: ModeKind, before: String, after: Rev, files: Vec<FileChange>) -> Analysis {
    Analysis {
        kind,
        label: String::new(),
        net: Some(Comparison {
            before: Rev::Commit(before),
            after,
            files,
        }),
        steps: Vec::new(),
        skipped_merges: Vec::new(),
    }
}
