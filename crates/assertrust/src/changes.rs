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

use assertrust_git::{Analysis, FileChange, ModeKind, Repo};

use crate::cli::ChangesArgs;
use crate::config::Config;
use crate::output::{line, mode_name};

pub fn run(args: &ChangesArgs) -> Result<(), String> {
    let mode = args.mode.to_input()?;
    let dir = std::env::current_dir().map_err(|err| format!("can't read current dir: {err}"))?;
    let repo = Repo::open(&dir).map_err(|err| err.to_string())?;
    // The file patterns in assertrust.toml decide which files are tests, as in `check`.
    let classifier = Config::for_check(&repo, &mode)?.0.classifier;
    let analysis = repo
        .analyze(&mode, &classifier)
        .map_err(|err| err.to_string())?;
    crate::print_stdout(&render(&analysis))
}

fn render(analysis: &Analysis) -> String {
    let mut out = String::new();
    let mode = mode_name(analysis.kind);
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
