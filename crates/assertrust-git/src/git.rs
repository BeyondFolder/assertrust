//! Running git. Every call goes through here.
//!
//! - Git runs from the repo's top-level directory, so paths are always relative to the repo
//!   root, wherever assertrust was started.
//! - Only read-only commands, with `GIT_OPTIONAL_LOCKS=0` so git never takes the index lock.
//!   That makes it safe to run next to an editor or a coding agent that is using the repo.
//!   The one write goes to a throwaway copy of the index in a temp dir (see
//!   [`Git::diff_worktree`]).
//! - User-supplied revisions are resolved to SHAs first, behind `--end-of-options`, so a
//!   "revision" like `--output=file` can never be read as a git option.
//! - Output that lists paths uses `-z` (NUL-separated), so paths with spaces or unusual
//!   characters come through unchanged.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::diff::{self, RawChange};
use crate::error::Error;
use crate::model::{Commit, short};

/// Separates the fields of one commit in `git log` output.
const FIELD: char = '\x1f';
/// Separates commits in `git log` output.
const RECORD: char = '\x1e';

pub(crate) struct Git {
    /// The repo's top-level directory (or the starting directory in a bare repo).
    dir: PathBuf,
    /// Where assertrust was started, relative to `dir`, e.g. `test/` (empty at the root).
    prefix: String,
}

/// Extra environment and stdin for one git call.
#[derive(Default)]
struct Extra<'a> {
    env: &'a [(&'a str, &'a Path)],
    stdin: Option<&'a [u8]>,
}

impl Git {
    /// The repo containing `dir`. Git then runs from the repo's top-level directory.
    pub(crate) fn open(dir: &Path) -> Result<Self, Error> {
        let git = Self {
            dir: dir.to_path_buf(),
            prefix: String::new(),
        };
        if let Ok(output) = git.run(&["rev-parse", "--show-toplevel", "--show-prefix"]) {
            let mut lines = output.split('\n');
            let top = lines.next().unwrap_or_default();
            let prefix = lines.next().unwrap_or_default();
            return Ok(Self {
                dir: PathBuf::from(top),
                prefix: prefix.to_owned(),
            });
        }
        // A bare repo (or inside `.git`): no working tree, but ranges still work.
        if git.succeeds(&["rev-parse", "--git-dir"]) {
            Ok(git)
        } else {
            Err(Error::new("not inside a git repository"))
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn prefix(&self) -> &str {
        &self.prefix
    }

    /// A blob's raw contents, e.g. `<sha>:test/a.test.ts` or `:0:test/a.test.ts` (the
    /// index). `None` if there's no such file there.
    pub(crate) fn read_blob(&self, spec: &str) -> Result<Option<Vec<u8>>, Error> {
        let args = ["cat-file", "blob", spec];
        let output = self.output(&args)?;
        if output.status.success() {
            return Ok(Some(output.stdout));
        }
        if !self.succeeds(&["cat-file", "-e", spec]) {
            return Ok(None);
        }
        Err(failed(&args, &output))
    }

    /// Several blobs in one call (`git cat-file --batch`), in order: `None` for one that
    /// doesn't exist or isn't a file (a directory).
    pub(crate) fn read_blobs(&self, specs: &[String]) -> Result<Vec<Option<Vec<u8>>>, Error> {
        // A name with a line break can't go on a line of its own; no import names one.
        let sent: Vec<&String> = specs.iter().filter(|spec| !spec.contains('\n')).collect();
        let mut input = String::new();
        for spec in &sent {
            input.push_str(spec);
            input.push('\n');
        }
        let args = ["cat-file", "--batch"];
        let output = self.output_with(
            &args,
            Extra {
                env: &[],
                stdin: Some(input.as_bytes()),
            },
        )?;
        if !output.status.success() {
            return Err(failed(&args, &output));
        }
        let malformed = || Error::new("unexpected output from `git cat-file --batch`");
        let mut rest = output.stdout.as_slice();
        let mut found = Vec::with_capacity(sent.len());
        for _ in &sent {
            let end = rest
                .iter()
                .position(|&b| b == b'\n')
                .ok_or_else(malformed)?;
            let header = String::from_utf8_lossy(&rest[..end]).into_owned();
            rest = &rest[end + 1..];
            // `<name> missing` (or `ambiguous`), else `<oid> <type> <size>` and the contents.
            let mut fields = header.rsplitn(3, ' ');
            let (Some(size), Some(kind), Some(_)) = (fields.next(), fields.next(), fields.next())
            else {
                found.push(None);
                continue;
            };
            let Ok(size) = size.parse::<usize>() else {
                found.push(None);
                continue;
            };
            let contents = rest.get(..size).ok_or_else(malformed)?;
            rest = rest.get(size + 1..).ok_or_else(malformed)?;
            found.push((kind == "blob").then(|| contents.to_vec()));
        }
        let mut found = found.into_iter();
        Ok(specs
            .iter()
            .map(|spec| {
                if spec.contains('\n') {
                    None
                } else {
                    found.next().flatten()
                }
            })
            .collect())
    }

    /// The files whose contents match the extended regular expression `pattern`, among
    /// `pathspecs` (`git grep`). `tree` is a commit, `--cached` (the index) or `--untracked`
    /// (the files on disk). Paths come back relative to the repo root.
    pub(crate) fn grep_files(
        &self,
        tree: &str,
        pattern: &str,
        pathspecs: &[&str],
    ) -> Result<Vec<String>, Error> {
        let mut args = vec![
            "grep",
            "--no-color",
            "--no-recurse-submodules",
            "-I",
            "-l",
            "-z",
            "-E",
            "-e",
            pattern,
            tree,
            "--",
        ];
        args.extend_from_slice(pathspecs);
        let output = self.output(&args)?;
        // Exit code 1 with no output means nothing matched.
        if !output.status.success() {
            if output.status.code() == Some(1) && output.stderr.is_empty() {
                return Ok(Vec::new());
            }
            return Err(failed(&args, &output));
        }
        // From a commit, each path comes as `<sha>:<path>`.
        let prefix = format!("{tree}:");
        Ok(stdout(&output)
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(|path| path.strip_prefix(&prefix).unwrap_or(path).to_owned())
            .collect())
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
        let args = ["merge-base", a, b];
        let output = self.output(&args)?;
        if output.status.success() {
            return Ok(stdout(&output).trim().to_owned());
        }
        // Exit code 1 with no output means "no merge base"; anything else is a real error.
        if output.status.code() != Some(1) || !output.stderr.is_empty() {
            return Err(failed(&args, &output));
        }
        let mut message = format!("{} and {} have no common ancestor", short(a), short(b));
        if self.run(&["rev-parse", "--is-shallow-repository"])?.trim() == "true" {
            message.push_str(
                ". This clone is shallow, so the history that joins them may be missing: \
                 fetch the full history (`git fetch --unshallow`, or `fetch-depth: 0` in \
                 actions/checkout)",
            );
        }
        Err(Error::new(message))
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

    /// The commits on `head`'s first-parent line, oldest first: those after `base`, or the
    /// last `limit` of them. On a main branch that's one commit per merge or push.
    pub(crate) fn first_parent(
        &self,
        base: Option<&str>,
        head: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Commit>, Error> {
        let range = match base {
            Some(base) => format!("{base}..{head}"),
            None => head.to_owned(),
        };
        let format = format!("--format=%H{FIELD}%P{FIELD}%an{FIELD}%B{RECORD}");
        let max = limit.map(|limit| format!("--max-count={limit}"));
        let mut args = vec![
            "log",
            "--no-show-signature",
            "--no-color",
            "--first-parent",
            // Applied after --max-count, so the newest commits are the ones kept.
            "--reverse",
            &format,
        ];
        args.extend(max.as_deref());
        args.extend(["--end-of-options", &range]);
        let output = self.run(&args)?;
        Ok(output.split(RECORD).filter_map(parse_commit).collect())
    }

    /// The tree of merging `theirs` into `ours`, as `git merge` would make it, without
    /// touching the work tree: `None` when it conflicts. Needs git 2.38 or newer.
    pub(crate) fn merge_tree(&self, ours: &str, theirs: &str) -> Result<Option<String>, Error> {
        let args = ["merge-tree", "--write-tree", "--no-messages", ours, theirs];
        let output = self.output(&args)?;
        match output.status.code() {
            Some(0) => Ok(Some(
                stdout(&output)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            )),
            Some(1) => Ok(None),
            _ => Err(failed(&args, &output)),
        }
    }

    /// The URL of the `origin` remote, if there is one.
    pub(crate) fn origin_url(&self) -> Option<String> {
        let output = self.output(&["remote", "get-url", "origin"]).ok()?;
        let url = stdout(&output).trim().to_owned();
        (output.status.success() && !url.is_empty()).then_some(url)
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

    /// `before` → the files on disk, including untracked files (respecting `.gitignore`).
    ///
    /// Untracked files are added as "intent to add" to a throwaway copy of the index, then
    /// one `git diff` compares `before` with the disk. Doing it in one diff means a file
    /// moved without `git add` still counts as a rename, and a file removed from the index
    /// but still on disk isn't listed twice. The real index is never touched.
    pub(crate) fn diff_worktree(&self, before: &str) -> Result<Vec<RawChange>, Error> {
        let temp = tempfile::tempdir()
            .map_err(|err| Error::new(format!("couldn't create a temp dir: {err}")))?;
        let index = temp.path().join("index");
        let real_index = self.dir.join(
            self.run(&["rev-parse", "--git-path", "index"])?
                .trim_end_matches('\n'),
        );
        if real_index.exists() {
            fs::copy(&real_index, &index)
                .map_err(|err| Error::new(format!("couldn't copy the git index: {err}")))?;
        }
        let env = [("GIT_INDEX_FILE", index.as_path())];

        let untracked = self.run(&["ls-files", "-z", "--others", "--exclude-standard"])?;
        if !untracked.is_empty() {
            let args = [
                "add",
                "--intent-to-add",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ];
            // Literal pathspecs, so a file named like `[id].vue` isn't read as a glob.
            let literal = [
                ("GIT_INDEX_FILE", index.as_path()),
                ("GIT_LITERAL_PATHSPECS", Path::new("1")),
            ];
            let output = self.output_with(
                &args,
                Extra {
                    env: &literal,
                    stdin: Some(untracked.as_bytes()),
                },
            )?;
            if !output.status.success() {
                return Err(failed(&args, &output));
            }
        }

        let args = [
            "diff",
            "--no-ext-diff",
            "--no-color",
            "-z",
            "-M",
            "--name-status",
            before,
            "--",
        ];
        let output = self.output_with(
            &args,
            Extra {
                env: &env,
                stdin: None,
            },
        )?;
        if !output.status.success() {
            return Err(failed(&args, &output));
        }
        Ok(diff::parse_name_status(&stdout(&output)))
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
            Err(failed(args, &output))
        }
    }

    fn succeeds(&self, args: &[&str]) -> bool {
        self.output(args)
            .is_ok_and(|output| output.status.success())
    }

    fn output(&self, args: &[&str]) -> Result<Output, Error> {
        self.output_with(args, Extra::default())
    }

    fn output_with(&self, args: &[&str], extra: Extra<'_>) -> Result<Output, Error> {
        let mut command = Command::new("git");
        command
            .arg("--no-pager")
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .envs(extra.env.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if extra.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let spawn_error = |err| Error::new(format!("couldn't run git: {err}"));
        let mut child = command.spawn().map_err(spawn_error)?;
        let pipe = child.stdin.take();
        // Feed stdin from another thread while reading the output, so neither side can
        // block the other when there's a lot of either.
        std::thread::scope(|scope| {
            if let (Some(input), Some(mut pipe)) = (extra.stdin, pipe) {
                // A write error means git exited early; its exit status says why.
                scope.spawn(move || pipe.write_all(input));
            }
            child.wait_with_output().map_err(spawn_error)
        })
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The error for a git command that failed, with git's own message.
fn failed(args: &[&str], output: &Output) -> Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    Error::new(format!(
        "`git {}` failed: {}",
        args.join(" "),
        stderr.trim()
    ))
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
