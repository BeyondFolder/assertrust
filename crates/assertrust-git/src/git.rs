//! Running git. Every call goes through here.
//!
//! - Only read-only commands, with `GIT_OPTIONAL_LOCKS=0` so git never takes the index lock.
//!   That makes it safe to run next to an editor or a coding agent that is using the repo.
//! - User-supplied revisions are resolved to SHAs first, behind `--end-of-options`, so a
//!   "revision" like `--output=file` can never be read as a git option.
//! - Output that lists paths uses `-z` (NUL-separated), so paths with spaces or unusual
//!   characters come through unchanged.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::diff::{self, RawChange};
use crate::error::Error;
use crate::model::Commit;

/// Separates the fields of one commit in `git log` output.
const FIELD: char = '\x1f';
/// Separates commits in `git log` output.
const RECORD: char = '\x1e';

pub(crate) struct Git {
    dir: PathBuf,
}

impl Git {
    pub(crate) fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    pub(crate) fn ensure_repo(&self) -> Result<(), Error> {
        if self.succeeds(&["rev-parse", "--git-dir"]) {
            Ok(())
        } else {
            Err(Error::new("not inside a git repository"))
        }
    }

    /// The full SHA of the commit `rev` points to.
    pub(crate) fn rev_parse(&self, rev: &str) -> Result<String, Error> {
        let spec = format!("{rev}^{{commit}}");
        let args = [
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &spec,
        ];
        match self.output(&args)? {
            output if output.status.success() => Ok(stdout(&output).trim().to_owned()),
            _ => Err(Error::new(format!("unknown revision `{rev}`"))),
        }
    }

    /// Where a symbolic ref points, shortened (e.g. `origin/main`), if it exists.
    pub(crate) fn symbolic_ref(&self, name: &str) -> Option<String> {
        let output = self
            .output(&["symbolic-ref", "--quiet", "--short", name])
            .ok()?;
        let target = stdout(&output).trim().to_owned();
        (output.status.success() && !target.is_empty()).then_some(target)
    }

    pub(crate) fn merge_base(&self, a: &str, b: &str) -> Result<String, Error> {
        match self.output(&["merge-base", a, b])? {
            output if output.status.success() => Ok(stdout(&output).trim().to_owned()),
            _ => Err(Error::new(format!(
                "{} and {} have no common ancestor",
                short(a),
                short(b)
            ))),
        }
    }

    /// `HEAD`'s SHA, or the empty tree in a repo with no commits yet.
    pub(crate) fn head_or_empty_tree(&self) -> Result<String, Error> {
        if self.succeeds(&["rev-parse", "--verify", "--quiet", "HEAD"]) {
            self.rev_parse("HEAD")
        } else {
            self.empty_tree()
        }
    }

    /// The SHA of the empty tree (computed, so it's right for SHA-1 and SHA-256 repos).
    pub(crate) fn empty_tree(&self) -> Result<String, Error> {
        Ok(self
            .run(&["hash-object", "-t", "tree", "--stdin"])?
            .trim()
            .to_owned())
    }

    /// The commits in `base..head`, oldest first, including merges.
    pub(crate) fn commits(&self, base: &str, head: &str) -> Result<Vec<Commit>, Error> {
        let range = format!("{base}..{head}");
        let format = format!("--format=%H{FIELD}%P{FIELD}%an{FIELD}%B{RECORD}");
        let output = self.run(&[
            "log",
            "--no-show-signature",
            "--no-color",
            "--reverse",
            &format,
            "--end-of-options",
            &range,
        ])?;
        Ok(output.split(RECORD).filter_map(parse_commit).collect())
    }

    /// What one commit changed compared with its first parent (everything, for a root commit).
    pub(crate) fn diff_commit(&self, sha: &str) -> Result<Vec<RawChange>, Error> {
        self.name_status(&[
            "diff-tree",
            "-r",
            "-z",
            "-M",
            "--name-status",
            "--no-commit-id",
            "--root",
            sha,
        ])
    }

    pub(crate) fn diff_trees(&self, before: &str, after: &str) -> Result<Vec<RawChange>, Error> {
        self.name_status(&[
            "diff-tree",
            "-r",
            "-z",
            "-M",
            "--name-status",
            before,
            after,
        ])
    }

    pub(crate) fn diff_staged(&self, before: &str) -> Result<Vec<RawChange>, Error> {
        self.name_status(&[
            "diff",
            "--cached",
            "--no-ext-diff",
            "--no-color",
            "-z",
            "-M",
            "--name-status",
            before,
            "--",
        ])
    }

    /// Tracked files that differ on disk. Untracked files come from [`Git::untracked`].
    pub(crate) fn diff_worktree(&self, before: &str) -> Result<Vec<RawChange>, Error> {
        self.name_status(&[
            "diff",
            "--no-ext-diff",
            "--no-color",
            "-z",
            "-M",
            "--name-status",
            before,
            "--",
        ])
    }

    /// New files on disk that git doesn't track yet (respecting `.gitignore`), as additions.
    pub(crate) fn untracked(&self) -> Result<Vec<RawChange>, Error> {
        let output = self.run(&["ls-files", "-z", "--others", "--exclude-standard"])?;
        Ok(output
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(|path| RawChange::added(path.to_owned()))
            .collect())
    }

    fn name_status(&self, args: &[&str]) -> Result<Vec<RawChange>, Error> {
        Ok(diff::parse_name_status(&self.run(args)?))
    }

    /// Runs git and returns stdout, or an error with git's message if it fails.
    fn run(&self, args: &[&str]) -> Result<String, Error> {
        let output = self.output(args)?;
        if output.status.success() {
            Ok(stdout(&output))
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(Error::new(format!(
                "`git {}` failed: {}",
                args.join(" "),
                stderr.trim()
            )))
        }
    }

    fn succeeds(&self, args: &[&str]) -> bool {
        self.output(args)
            .is_ok_and(|output| output.status.success())
    }

    fn output(&self, args: &[&str]) -> Result<Output, Error> {
        Command::new("git")
            .arg("--no-pager")
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .output()
            .map_err(|err| Error::new(format!("couldn't run git: {err}")))
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// Parses one `git log` record: SHA, parents, author, message.
fn parse_commit(record: &str) -> Option<Commit> {
    let record = record.trim_start_matches('\n');
    if record.is_empty() {
        return None;
    }
    let mut fields = record.splitn(4, FIELD);
    let sha = fields.next()?.to_owned();
    let parents = fields
        .next()?
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let author = fields.next()?.to_owned();
    let message = fields.next()?.trim_end().to_owned();
    Some(Commit {
        sha,
        parents,
        author,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_commit_record() {
        let record = "\nabc123\x1fp1 p2\x1fAda\x1fSubject\n\nBody line\n\n";
        let commit = parse_commit(record).unwrap();
        assert_eq!(commit.sha, "abc123");
        assert_eq!(commit.parents, ["p1", "p2"]);
        assert_eq!(commit.author, "Ada");
        assert_eq!(commit.message, "Subject\n\nBody line");
        assert!(commit.is_merge());
    }

    #[test]
    fn a_root_commit_has_no_parents() {
        let commit = parse_commit("abc\x1f\x1fAda\x1fFirst\n").unwrap();
        assert!(commit.parents.is_empty());
    }

    #[test]
    fn skips_empty_records() {
        assert_eq!(parse_commit("\n"), None);
    }
}
