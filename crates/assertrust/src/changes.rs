//! `assertrust changes`: shows which commits and files a check would look at, without
//! checking anything. It prints what the input layer (step 1) sees, for debugging.
//!
//! ```text
//! pr-style: main...feature
//! compare 1a2b3c4 → 9f8e7d6
//!   M test         test/a.test.ts
//! commits (1, 1 merge skipped):
//!   9f8e7d6 Loosen the test
//!     M test         test/a.test.ts
//! ```

use std::fmt::Write as _;

use assertrust_git::{Analysis, Classifier, FileChange, ModeKind};

use crate::cli::ChangesArgs;

pub fn run(args: &ChangesArgs) -> Result<(), String> {
    let mode = args.mode.to_input()?;
    let dir = std::env::current_dir().map_err(|err| format!("can't read current dir: {err}"))?;
    let analysis = assertrust_git::analyze(&dir, &mode, &Classifier::default())
        .map_err(|err| err.to_string())?;
    print!("{}", render(&analysis));
    Ok(())
}

fn render(analysis: &Analysis) -> String {
    let mut out = String::new();
    let mode = match analysis.kind {
        ModeKind::PerCommit => "per-commit",
        ModeKind::PrStyle => "pr-style",
        ModeKind::Staged => "staged",
        ModeKind::WorkTree => "worktree",
    };
    if analysis.label.is_empty() {
        line(&mut out, mode);
    } else {
        line(&mut out, &format!("{mode}: {}", analysis.label));
    }

    if let Some(net) = &analysis.net {
        line(&mut out, &format!("compare {} → {}", net.before, net.after));
        files(&mut out, &net.files, "  ");
    }

    if matches!(analysis.kind, ModeKind::PerCommit | ModeKind::PrStyle) {
        let merges = match analysis.skipped_merges.len() {
            0 => String::new(),
            1 => ", 1 merge skipped".to_owned(),
            n => format!(", {n} merges skipped"),
        };
        line(
            &mut out,
            &format!("commits ({}{merges}):", analysis.steps.len()),
        );
        for step in &analysis.steps {
            let commit = &step.commit;
            line(
                &mut out,
                &format!("  {} {}", commit.short_sha(), commit.subject()),
            );
            files(&mut out, &step.comparison.files, "    ");
        }
    }

    if analysis.is_empty() {
        line(&mut out, "nothing to check");
    }
    out
}

fn files(out: &mut String, files: &[FileChange], indent: &str) {
    for file in files {
        let path = match (&file.path_before, &file.path_after) {
            (Some(before), Some(after)) if before != after => format!("{before} → {after}"),
            _ => file.path().to_owned(),
        };
        line(
            out,
            &format!(
                "{indent}{} {:<12} {path}",
                file.status.letter(),
                file.kind.as_str()
            ),
        );
    }
}

fn line(out: &mut String, text: &str) {
    writeln!(out, "{text}").expect("writing to a String can't fail");
}
