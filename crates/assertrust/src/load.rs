//! Reads and parses the test files a comparison touches, for matching (step 4).

use assertrust_git::{Comparison, FileKind, Repo, Rev};
use assertrust_ir::{FileIr, ParseError};
use assertrust_match::{FilePair, Side};

/// The test files of one comparison, parsed on both sides.
pub struct Loaded {
    pub files: Vec<LoadedFile>,
    /// Files that were skipped, e.g. "test/a.test.ts (1a2b3c4): skipped, doesn't parse (…)".
    pub notes: Vec<String>,
}

pub struct LoadedFile {
    pub before: Option<Parsed>,
    pub after: Option<Parsed>,
}

pub struct Parsed {
    pub path: String,
    pub ir: FileIr,
}

impl Loaded {
    pub fn file_pairs(&self) -> Vec<FilePair<'_>> {
        self.files
            .iter()
            .map(|file| FilePair {
                before: file.before.as_ref().map(Parsed::side),
                after: file.after.as_ref().map(Parsed::side),
            })
            .collect()
    }
}

impl Parsed {
    fn side(&self) -> Side<'_> {
        Side {
            path: &self.path,
            ir: &self.ir,
        }
    }
}

pub fn load(repo: &Repo, comparison: &Comparison) -> Result<Loaded, String> {
    let mut loaded = Loaded {
        files: Vec::new(),
        notes: Vec::new(),
    };
    for file in &comparison.files {
        if file.kind != FileKind::Test {
            continue;
        }
        let before = read(repo, &comparison.before, file.path_before.as_deref())?;
        let after = read(repo, &comparison.after, file.path_after.as_deref())?;
        // A side that can't be read makes the whole file unreadable: comparing against
        // nothing would make every test in it look deleted (or added).
        let skipped: Vec<String> = [before.skipped(), after.skipped()]
            .into_iter()
            .flatten()
            .collect();
        if !skipped.is_empty() {
            loaded.notes.extend(skipped);
            continue;
        }
        let (before, after) = (before.parsed(), after.parsed());
        if before.is_some() || after.is_some() {
            loaded.files.push(LoadedFile { before, after });
        }
    }
    Ok(loaded)
}

/// One side of a file.
enum Read {
    /// The file doesn't exist on this side (added or deleted).
    Missing,
    Parsed(Parsed),
    /// It exists but can't be read, with a note saying why.
    Skipped(String),
}

impl Read {
    fn skipped(&self) -> Option<String> {
        match self {
            Self::Skipped(note) => Some(note.clone()),
            _ => None,
        }
    }

    fn parsed(self) -> Option<Parsed> {
        match self {
            Self::Parsed(parsed) => Some(parsed),
            _ => None,
        }
    }
}

fn read(repo: &Repo, rev: &Rev, path: Option<&str>) -> Result<Read, String> {
    let Some(path) = path else {
        return Ok(Read::Missing);
    };
    let Some(source) = repo.read_file(rev, path).map_err(|err| err.to_string())? else {
        return Ok(Read::Missing);
    };
    Ok(match assertrust_ir::parse_file(path, &source) {
        Ok(ir) => Read::Parsed(Parsed {
            path: path.to_owned(),
            ir,
        }),
        Err(ParseError::UnsupportedFile) => {
            Read::Skipped(format!("{path}: skipped, not a JS or TS file"))
        }
        Err(err) => Read::Skipped(format!("{path} ({rev}): skipped, {err}")),
    })
}
