//! The input layer of assertrust (`docs/architecture.md`, step 1).
//!
//! It turns "check this range / these staged changes / these edits" into before/after
//! comparisons, plus the commits that produced them, with every changed file labelled by
//! kind (test, gate, source, …). It shells out to `git` and knows nothing about tests or
//! rules, so other tools can reuse it.
//!
//! ```no_run
//! use assertrust_git::{Classifier, Mode, Repo};
//!
//! let repo = Repo::open(".".as_ref())?;
//! let analysis = repo.analyze(&Mode::Range("main...HEAD".into()), &Classifier::default())?;
//! for step in &analysis.steps {
//!     println!("{} touches {} files", step.commit.short_sha(), step.comparison.files.len());
//!     for file in &step.comparison.files {
//!         let contents = repo.read_file(&step.comparison.after, file.path())?;
//!     }
//! }
//! # Ok::<(), assertrust_git::Error>(())
//! ```

mod classify;
mod diff;
mod error;
mod git;
mod mode;
mod model;

use std::fs;
use std::io;
use std::path::{Component, Path};

pub use crate::classify::Classifier;
pub use crate::error::Error;
pub use crate::mode::Mode;
pub use crate::model::{
    Analysis, ChangeStatus, Commit, Comparison, FileChange, FileKind, ModeKind, Rev, Step,
};

use crate::git::Git;
use crate::mode::Resolved;

/// A git repository, found from a directory inside it.
pub struct Repo {
    git: Git,
}

impl Repo {
    /// The repo containing `dir`.
    pub fn open(dir: &Path) -> Result<Self, Error> {
        Ok(Self {
            git: Git::open(dir)?,
        })
    }

    /// Works out what `mode` compares.
    pub fn analyze(&self, mode: &Mode, classifier: &Classifier) -> Result<Analysis, Error> {
        analyze(&self.git, mode, classifier)
    }

    /// The full SHA of the commit `rev` points to (a branch, tag, `HEAD~1`, …).
    pub fn resolve(&self, rev: &str) -> Result<String, Error> {
        self.git.rev_parse(rev)
    }

    /// A file's contents on one side of a comparison. `path` is relative to the repo root.
    /// `None` if the file doesn't exist there.
    pub fn read_file(&self, rev: &Rev, path: &str) -> Result<Option<String>, Error> {
        let bytes = match rev {
            Rev::Commit(sha) => self.git.read_blob(&format!("{sha}:{path}"))?,
            Rev::Index => self.git.read_blob(&format!(":0:{path}"))?,
            Rev::WorkTree => match fs::read(self.git.root().join(path)) {
                Ok(bytes) => Some(bytes),
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(err) => return Err(Error::new(format!("couldn't read `{path}`: {err}"))),
            },
        };
        bytes
            .map(|bytes| {
                String::from_utf8(bytes)
                    .map_err(|_| Error::new(format!("`{path}` isn't UTF-8 text")))
            })
            .transpose()
    }

    /// Several files' raw contents on one side of a comparison, in order, with one git call:
    /// `None` for a file that doesn't exist there (or is a directory). Bytes, so a file
    /// that isn't UTF-8 text is the caller's call to skip.
    pub fn read_files(&self, rev: &Rev, paths: &[String]) -> Result<Vec<Option<Vec<u8>>>, Error> {
        match rev {
            Rev::Commit(sha) => {
                let specs: Vec<String> = paths.iter().map(|path| format!("{sha}:{path}")).collect();
                self.git.read_blobs(&specs)
            }
            Rev::Index => {
                let specs: Vec<String> = paths.iter().map(|path| format!(":0:{path}")).collect();
                self.git.read_blobs(&specs)
            }
            Rev::WorkTree => paths
                .iter()
                .map(|path| {
                    let full = self.git.root().join(path);
                    if !full.is_file() {
                        return Ok(None);
                    }
                    fs::read(&full)
                        .map(Some)
                        .map_err(|err| Error::new(format!("couldn't read `{path}`: {err}")))
                })
                .collect(),
        }
    }

    /// The files on one side of a comparison whose contents match the extended regular
    /// expression `pattern`, among `pathspecs` (git pathspecs, like `*.ts`). On disk, files
    /// git ignores are left out.
    pub fn files_matching(
        &self,
        rev: &Rev,
        pattern: &str,
        pathspecs: &[&str],
    ) -> Result<Vec<String>, Error> {
        let tree = match rev {
            Rev::Commit(sha) => sha.as_str(),
            Rev::Index => "--cached",
            Rev::WorkTree => "--untracked",
        };
        self.git.grep_files(tree, pattern, pathspecs)
    }

    /// A path typed by the user (relative to the directory the repo was opened from, or
    /// absolute) as a path relative to the repo root, with `/` separators.
    pub fn path_in_repo(&self, path: &Path) -> Result<String, Error> {
        let outside = || Error::new(format!("`{}` is outside the repository", path.display()));
        let relative = if path.is_absolute() {
            match path.strip_prefix(self.git.root()) {
                Ok(relative) => relative.to_path_buf(),
                Err(_) => fs::canonicalize(path)
                    .ok()
                    .and_then(|real| {
                        real.strip_prefix(self.git.root())
                            .ok()
                            .map(Path::to_path_buf)
                    })
                    .ok_or_else(outside)?,
            }
        } else {
            Path::new(self.git.prefix()).join(path)
        };
        let mut parts: Vec<String> = Vec::new();
        for component in relative.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
                Component::CurDir => {}
                Component::ParentDir => {
                    parts.pop().ok_or_else(outside)?;
                }
                Component::RootDir | Component::Prefix(_) => return Err(outside()),
            }
        }
        Ok(parts.join("/"))
    }
}

fn analyze(git: &Git, mode: &Mode, classifier: &Classifier) -> Result<Analysis, Error> {
    match mode::resolve(git, mode)? {
        Resolved::PerCommit { base, head, label } => {
            let (steps, skipped_merges) = steps(git, &base, &head, classifier)?;
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
            let (steps, skipped_merges) = steps(git, &base, &head, classifier)?;
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
