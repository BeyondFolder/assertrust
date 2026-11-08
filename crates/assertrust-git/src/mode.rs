//! Turning the user's choice of what to compare into concrete revisions.

use crate::error::Error;
use crate::git::Git;

/// What to compare (`docs/architecture.md`, step 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// `BASE..HEAD` checks each commit on its own. `BASE...HEAD` checks it PR-style: what
    /// `HEAD` adds since it split off from `BASE`, plus its commits. Same meaning as in
    /// `git diff`. An empty side means `HEAD`.
    Range(String),
    /// `HEAD` → the git index.
    Staged,
    /// `HEAD` → the files on disk, including untracked files.
    WorkTree,
    /// No flag given: the current branch compared with the default branch, PR-style.
    Default,
}

/// A mode with its revisions resolved to full SHAs.
pub(crate) enum Resolved {
    PerCommit {
        base: String,
        head: String,
        label: String,
    },
    PrStyle {
        base: String,
        head: String,
        label: String,
    },
    Staged,
    WorkTree,
}

pub(crate) fn resolve(git: &Git, mode: &Mode) -> Result<Resolved, Error> {
    match mode {
        Mode::Range(range) => {
            let (base, head, pr_style) = parse_range(range)?;
            let base = git.rev_parse(base)?;
            let head = git.rev_parse(head)?;
            let label = range.clone();
            Ok(if pr_style {
                Resolved::PrStyle { base, head, label }
            } else {
                Resolved::PerCommit { base, head, label }
            })
        }
        Mode::Default => {
            let branch = default_branch(git)?;
            Ok(Resolved::PrStyle {
                base: git.rev_parse(&branch)?,
                head: git.rev_parse("HEAD")?,
                label: format!("{branch}...HEAD (default)"),
            })
        }
        Mode::Staged => Ok(Resolved::Staged),
        Mode::WorkTree => Ok(Resolved::WorkTree),
    }
}

/// Splits `A..B` / `A...B` into `(A, B, is_three_dot)`.
fn parse_range(range: &str) -> Result<(&str, &str, bool), Error> {
    let (base, head, pr_style) = if let Some((base, head)) = range.split_once("...") {
        (base, head, true)
    } else if let Some((base, head)) = range.split_once("..") {
        (base, head, false)
    } else {
        return Err(Error::new(format!(
            "expected BASE..HEAD or BASE...HEAD, got `{range}`"
        )));
    };
    Ok((or_head(base), or_head(head), pr_style))
}

fn or_head(side: &str) -> &str {
    if side.is_empty() { "HEAD" } else { side }
}

/// The branch PRs merge into: what `origin/HEAD` points to, else a local `main` or `master`.
/// `origin/HEAD` is skipped if it points to a branch that no longer exists (the remote's
/// default branch was renamed, then pruned).
fn default_branch(git: &Git) -> Result<String, Error> {
    if let Some(remote_head) = git.symbolic_ref("refs/remotes/origin/HEAD")
        && git.rev_parse(&remote_head).is_ok()
    {
        return Ok(remote_head);
    }
    for branch in ["main", "master"] {
        if git.rev_parse(&format!("refs/heads/{branch}")).is_ok() {
            return Ok(branch.to_owned());
        }
    }
    Err(Error::new(
        "couldn't find the default branch (no origin/HEAD, main or master); pass --range",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_dots_mean_each_commit() {
        assert_eq!(
            parse_range("main..feature").unwrap(),
            ("main", "feature", false)
        );
    }

    #[test]
    fn three_dots_mean_pr_style() {
        assert_eq!(
            parse_range("main...feature").unwrap(),
            ("main", "feature", true)
        );
    }

    #[test]
    fn an_empty_side_means_head() {
        assert_eq!(parse_range("main..").unwrap(), ("main", "HEAD", false));
        assert_eq!(
            parse_range("...feature").unwrap(),
            ("HEAD", "feature", true)
        );
    }

    #[test]
    fn a_single_revision_is_not_a_range() {
        let err = parse_range("main").unwrap_err();
        assert_eq!(
            err.to_string(),
            "expected BASE..HEAD or BASE...HEAD, got `main`"
        );
    }
}
