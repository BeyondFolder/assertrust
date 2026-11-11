//! Small helpers shared by the commands that print text.

use std::fmt::Write as _;

use assertrust_git::ModeKind;

/// Appends `text` and a newline.
pub(crate) fn line(out: &mut String, text: &str) {
    writeln!(out, "{text}").expect("writing to a String can't fail");
}

/// `1 test`, `2 tests`.
pub(crate) fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// What a mode is called in output: `per-commit`, `pr-style`, `staged`, `worktree`.
pub(crate) fn mode_name(kind: ModeKind) -> &'static str {
    match kind {
        ModeKind::PerCommit => "per-commit",
        ModeKind::PrStyle => "pr-style",
        ModeKind::Staged => "staged",
        ModeKind::WorkTree => "worktree",
    }
}
