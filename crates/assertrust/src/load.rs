//! Reads and parses the test files a comparison touches, for matching (step 4) and the
//! rules (step 5).

use assertrust_git::{Classifier, Comparison, FileKind, Repo, Rev};
use assertrust_ir::{FileIr, Gate, ParseError};
use assertrust_match::{FilePair, Side};
use assertrust_rules::{GateFile, GateSide, TestFile};

/// The test files of one comparison, parsed on both sides, and its CI and test config
/// files (gates).
pub struct Loaded {
    pub files: Vec<LoadedFile>,
    pub gates: Vec<LoadedGate>,
    /// Files that were skipped, e.g. "test/a.test.ts (1a2b3c4): skipped, doesn't parse (…)".
    pub notes: Vec<String>,
}

pub struct LoadedFile {
    pub before: Option<Parsed>,
    pub after: Option<Parsed>,
    /// The new name, when a test file was renamed to a name that isn't a test file name, so
    /// the test runner no longer picks it up. `after` is then `None`.
    pub renamed_out: Option<String>,
}

pub struct Parsed {
    pub path: String,
    pub ir: FileIr,
}

pub struct LoadedGate {
    pub before: Option<ParsedGate>,
    pub after: Option<ParsedGate>,
}

pub struct ParsedGate {
    pub path: String,
    pub gate: Gate,
}

impl ParsedGate {
    fn side(&self) -> GateSide<'_> {
        GateSide {
            path: &self.path,
            gate: &self.gate,
        }
    }
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

    pub fn gate_files(&self) -> Vec<GateFile<'_>> {
        self.gates
            .iter()
            .map(|gate| GateFile {
                before: gate.before.as_ref().map(ParsedGate::side),
                after: gate.after.as_ref().map(ParsedGate::side),
            })
            .collect()
    }

    pub fn test_files(&self) -> Vec<TestFile<'_>> {
        self.files
            .iter()
            .map(|file| TestFile {
                before: file.before.as_ref().map(Parsed::side),
                after: file.after.as_ref().map(Parsed::side),
                renamed_out: file.renamed_out.as_deref(),
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

pub fn load(
    repo: &Repo,
    comparison: &Comparison,
    classifier: &Classifier,
) -> Result<Loaded, String> {
    let mut loaded = Loaded {
        files: Vec::new(),
        gates: Vec::new(),
        notes: Vec::new(),
    };
    for file in &comparison.files {
        if file.kind == FileKind::Gate {
            let before = read_gate(repo, &comparison.before, file.path_before.as_deref())?;
            let after = read_gate(repo, &comparison.after, file.path_after.as_deref())?;
            match (before, after) {
                (Ok(before), Ok(after)) if before.is_some() || after.is_some() => {
                    loaded.gates.push(LoadedGate { before, after });
                }
                (Err(note), _) | (_, Err(note)) => loaded.notes.push(note),
                _ => {}
            }
            continue;
        }
        if file.kind != FileKind::Test {
            continue;
        }
        // A test file renamed to a name that isn't a test file name (`a.test.ts` →
        // `a.test.ts.disabled`) stops running: on that side it's as good as gone.
        let is_test = |path: &&str| classifier.classify(path) == FileKind::Test;
        let path_before = file.path_before.as_deref().filter(is_test);
        let path_after = file.path_after.as_deref().filter(is_test);
        let renamed_out = path_after
            .is_none()
            .then(|| file.path_after.clone())
            .flatten();
        let before = read(repo, &comparison.before, path_before)?;
        let after = read(repo, &comparison.after, path_after)?;
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
            loaded.files.push(LoadedFile {
                before,
                after,
                renamed_out,
            });
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

/// One side of a gate file: `Ok(None)` when it doesn't exist there, `Err` with a note when
/// it can't be read.
fn read_gate(
    repo: &Repo,
    rev: &Rev,
    path: Option<&str>,
) -> Result<Result<Option<ParsedGate>, String>, String> {
    let Some(path) = path else {
        return Ok(Ok(None));
    };
    let Some(source) = repo.read_file(rev, path).map_err(|err| err.to_string())? else {
        return Ok(Ok(None));
    };
    Ok(match assertrust_ir::parse_gate(path, &source) {
        Ok(gate) => Ok(Some(ParsedGate {
            path: path.to_owned(),
            gate,
        })),
        Err(err) => Err(format!("{path} ({rev}): skipped, {err}")),
    })
}
