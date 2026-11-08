//! Parsing `git diff --name-status -z` output into file changes.

use crate::classify::Classifier;
use crate::model::{ChangeStatus, FileChange};

/// A changed path before it's classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawChange {
    pub(crate) status: ChangeStatus,
    pub(crate) before: Option<String>,
    pub(crate) after: Option<String>,
}

impl RawChange {
    pub(crate) fn added(path: String) -> Self {
        Self {
            status: ChangeStatus::Added,
            before: None,
            after: Some(path),
        }
    }
}

/// Parses NUL-separated name-status output: a status, then one path (or two for a rename
/// or copy). Statuses are `A`, `M`, `D`, `T` (type change), `U` (unmerged), `R<score>`
/// and `C<score>`. A copy counts as an added file.
pub(crate) fn parse_name_status(output: &str) -> Vec<RawChange> {
    let mut tokens = output.split('\0').filter(|token| !token.is_empty());
    let mut changes = Vec::new();
    while let (Some(status), Some(path)) = (tokens.next(), tokens.next()) {
        let path = path.to_owned();
        let change = match status.chars().next() {
            Some('A') => RawChange::added(path),
            Some('D') => RawChange {
                status: ChangeStatus::Deleted,
                before: Some(path),
                after: None,
            },
            Some('R') => match tokens.next() {
                Some(new) => RawChange {
                    status: ChangeStatus::Renamed,
                    before: Some(path),
                    after: Some(new.to_owned()),
                },
                None => break,
            },
            Some('C') => match tokens.next() {
                Some(new) => RawChange::added(new.to_owned()),
                None => break,
            },
            _ => RawChange {
                status: ChangeStatus::Modified,
                before: Some(path.clone()),
                after: Some(path),
            },
        };
        changes.push(change);
    }
    changes
}

/// Labels each change with its file kind (for a rename, the stronger of its two paths; see
/// [`Classifier::classify_change`]).
pub(crate) fn classify(changes: Vec<RawChange>, classifier: &Classifier) -> Vec<FileChange> {
    changes
        .into_iter()
        .map(|change| {
            let kind =
                classifier.classify_change(change.before.as_deref(), change.after.as_deref());
            FileChange {
                kind,
                status: change.status,
                path_before: change.before,
                path_after: change.after,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(status: ChangeStatus, before: Option<&str>, after: Option<&str>) -> RawChange {
        RawChange {
            status,
            before: before.map(str::to_owned),
            after: after.map(str::to_owned),
        }
    }

    #[test]
    fn parses_every_status() {
        let output = "A\0new.ts\0M\0changed.ts\0D\0gone.ts\0T\0link.ts\0\
                      R092\0old name.ts\0new name.ts\0C100\0src.ts\0copy.ts\0";
        assert_eq!(
            parse_name_status(output),
            [
                change(ChangeStatus::Added, None, Some("new.ts")),
                change(
                    ChangeStatus::Modified,
                    Some("changed.ts"),
                    Some("changed.ts")
                ),
                change(ChangeStatus::Deleted, Some("gone.ts"), None),
                change(ChangeStatus::Modified, Some("link.ts"), Some("link.ts")),
                change(
                    ChangeStatus::Renamed,
                    Some("old name.ts"),
                    Some("new name.ts")
                ),
                change(ChangeStatus::Added, None, Some("copy.ts")),
            ]
        );
    }

    #[test]
    fn empty_output_means_no_changes() {
        assert!(parse_name_status("").is_empty());
    }
}
