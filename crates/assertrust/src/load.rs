//! Reads and parses the test files a comparison touches, links them to the helpers they
//! call (step 3), for matching (step 4) and the rules (step 5).
//!
//! A test file that didn't change is loaded too when a helper file it calls changed:
//!
//! ```text
//! changed:   test/helpers.ts      function expectOk(res) { expect(res.status).toBe(200) }  (check removed)
//! unchanged: test/refund.test.ts  it('refunds', () => { expectOk(res) })
//! → test/refund.test.ts is loaded on both sides, each linked to its side's helpers
//! ```

use std::collections::HashSet;

use assertrust_git::{Classifier, Comparison, FileChange, FileKind, Repo, Rev};
use assertrust_ir::{FileIr, Gate, Item, ItemKind, ParseError, Via};
use assertrust_match::{FilePair, Side};
use assertrust_rules::{GateFile, GateSide, TestFile};

use crate::modules::Modules;

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

/// `carry` holds the helper files read for the previous comparison's "after" side: when
/// that's this one's "before" (the commits of a range, one by one), they aren't read again.
pub fn load<'a>(
    repo: &'a Repo,
    comparison: &'a Comparison,
    classifier: &'a Classifier,
    carry: &mut Option<Modules<'a>>,
) -> Result<Loaded, String> {
    let mut loaded = Loaded {
        files: Vec::new(),
        gates: Vec::new(),
        notes: Vec::new(),
    };
    let before = match carry.take() {
        Some(previous) if previous.rev() == &comparison.before => previous,
        _ => Modules::new(repo, &comparison.before, classifier),
    };
    // Only the files the change touched read differently after it.
    let changed: HashSet<String> = comparison
        .files
        .iter()
        .flat_map(|file| [file.path_before.clone(), file.path_after.clone()])
        .flatten()
        .collect();
    let after = before.successor(&comparison.after, &changed);
    let mut modules = (before, after);
    // Helper files whose helpers changed: the tests that call them are checked too. The
    // changed helper files are read in one go per side.
    let mut changed_helpers: Vec<&FileChange> = Vec::new();
    let support = |path: &Option<String>, file: &FileChange| {
        (file.kind == FileKind::TestSupport)
            .then(|| path.clone())
            .flatten()
    };
    modules.0.read(
        comparison
            .files
            .iter()
            .filter_map(|file| support(&file.path_before, file))
            .collect(),
    )?;
    modules.1.read(
        comparison
            .files
            .iter()
            .filter_map(|file| support(&file.path_after, file))
            .collect(),
    )?;
    for file in &comparison.files {
        if file.kind == FileKind::TestSupport {
            let before = read_helpers(&modules.0, file.path_before.as_deref());
            let after = read_helpers(&modules.1, file.path_after.as_deref());
            if checks_changed(&before, &after, &mut modules)? {
                changed_helpers.push(file);
            }
            continue;
        }
        if file.kind == FileKind::Gate {
            // A workflow renamed or moved where it doesn't run (`ci.yml` → `ci.yml.disabled`)
            // is as good as gone on that side, like a test file renamed out.
            let is_gate = |path: &&str| classifier.classify(path) == FileKind::Gate;
            let path_before = file.path_before.as_deref().filter(is_gate);
            let path_after = file.path_after.as_deref().filter(is_gate);
            let before = read_gate(repo, &comparison.before, path_before)?;
            let after = read_gate(repo, &comparison.after, path_after)?;
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
        // A file with no tests under `__tests__/` holds helpers.
        if checks_changed(&before, &after, &mut modules)? {
            changed_helpers.push(file);
        }
        // A side whose helpers can't all be read makes the file unreadable too.
        let Some((before, after)) = link(
            before.parsed(),
            after.parsed(),
            &mut modules,
            &mut loaded.notes,
        )?
        else {
            continue;
        };
        if before.is_some() || after.is_some() {
            loaded.files.push(LoadedFile {
                before,
                after,
                renamed_out,
            });
        }
    }
    load_importers(comparison, &changed_helpers, &mut modules, &mut loaded)?;
    *carry = Some(modules.1);
    Ok(loaded)
}

/// Links both sides of a test file to the helpers they call. A helper followed on one side
/// that's in a file outside test folders on the other (moved to `src/`) is followed there
/// too. `None` (with a note) when a helper file it calls doesn't parse: comparing without
/// its checks would make them look removed (or added).
#[allow(clippy::type_complexity)]
fn link(
    before: Option<Parsed>,
    after: Option<Parsed>,
    modules: &mut (Modules, Modules),
    notes: &mut Vec<String>,
) -> Result<Option<(Option<Parsed>, Option<Parsed>)>, String> {
    if let Some(before) = &before {
        modules.0.follow(&before.path, &before.ir)?;
    }
    if let Some(after) = &after {
        modules.1.follow(&after.path, &after.ir)?;
    }
    if let (Some(before), Some(after)) = (&before, &after) {
        let was = modules.0.followed_names(&before.path, &before.ir);
        let now = modules.1.followed_names(&after.path, &after.ir);
        for name in was.difference(&now) {
            modules.1.follow_outside(&after.path, &after.ir, name)?;
        }
        for name in now.difference(&was) {
            modules.0.follow_outside(&before.path, &before.ir, name)?;
        }
    }
    let mut problems = Vec::new();
    for (parsed, modules) in [(&before, &modules.0), (&after, &modules.1)] {
        if let Some(parsed) = parsed {
            problems.extend(
                modules
                    .problems(&parsed.path, &parsed.ir)
                    .into_iter()
                    .map(|problem| format!("{}: skipped, its helper file {problem}", parsed.path)),
            );
        }
    }
    if !problems.is_empty() {
        notes.extend(problems);
        return Ok(None);
    }
    let linked = |parsed: Option<Parsed>, modules: &Modules| {
        parsed.map(|parsed| Parsed {
            ir: assertrust_ir::link(&parsed.path, &parsed.ir, modules),
            path: parsed.path,
        })
    };
    Ok(Some((
        linked(before, &modules.0),
        linked(after, &modules.1),
    )))
}

/// Adds the test files that didn't change but call helpers from a helper file that did.
/// Each is linked on both sides, and kept only when that changes what its tests check.
fn load_importers(
    comparison: &Comparison,
    changed_helpers: &[&FileChange],
    modules: &mut (Modules, Modules),
    loaded: &mut Loaded,
) -> Result<(), String> {
    let mut done: HashSet<String> = comparison
        .files
        .iter()
        .flat_map(|file| [file.path_before.clone(), file.path_after.clone()])
        .flatten()
        .collect();
    // Searched where each file still exists: after the change, or before it for one
    // deleted. All at once, so each step away from them is one search.
    let after: Vec<String> = changed_helpers
        .iter()
        .filter_map(|file| file.path_after.clone())
        .collect();
    let before: Vec<String> = changed_helpers
        .iter()
        .filter(|file| file.path_after.is_none())
        .filter_map(|file| file.path_before.clone())
        .collect();
    let mut importers = Vec::new();
    if !after.is_empty() {
        let (found, note) = modules.1.importers(&after)?;
        importers.extend(found);
        loaded.notes.extend(note);
    }
    if !before.is_empty() {
        let (found, note) = modules.0.importers(&before)?;
        importers.extend(found);
        loaded.notes.extend(note);
    }
    {
        for (path, ir) in importers {
            if !done.insert(path.clone()) {
                continue;
            }
            // It didn't change, so it reads the same on both sides.
            let parsed = || Parsed {
                path: path.clone(),
                ir: ir.clone(),
            };
            let Some((Some(before), Some(after))) =
                link(Some(parsed()), Some(parsed()), modules, &mut loaded.notes)?
            else {
                continue;
            };
            if same_checks(&before.ir, &after.ir) {
                continue;
            }
            loaded.files.push(LoadedFile {
                before: Some(before),
                after: Some(after),
                renamed_out: None,
            });
        }
    }
    Ok(())
}

/// Whether a changed helper file may have changed what tests check: its helpers changed,
/// and they check something on one side, themselves or through helper files they follow. A
/// file of fixtures or factories with no checks doesn't send the search for importers off.
fn checks_changed(
    before: &Read,
    after: &Read,
    modules: &mut (Modules, Modules),
) -> Result<bool, String> {
    if !helpers_changed(before, after) {
        return Ok(false);
    }
    let checks = |side: &Read, modules: &mut Modules| match side {
        Read::Missing => Ok(false),
        Read::Skipped(_) => Ok(true),
        Read::Parsed(parsed) => {
            modules.follow(&parsed.path, &parsed.ir)?;
            Ok::<_, String>(modules.has_checks(&parsed.path, &parsed.ir))
        }
    };
    Ok(checks(before, &mut modules.0)? || checks(after, &mut modules.1)?)
}

/// Whether a changed file holds helpers (and no tests) and they changed, beyond moving
/// lines. A side that doesn't parse counts as changed: the tests that call it then say
/// they were skipped.
fn helpers_changed(before: &Read, after: &Read) -> bool {
    fn ir<'r>(side: &'r Read, empty: &'r FileIr) -> Option<&'r FileIr> {
        match side {
            Read::Missing => Some(empty),
            Read::Parsed(parsed) => Some(&parsed.ir),
            Read::Skipped(_) => None,
        }
    }
    let empty = FileIr::default();
    let (Some(before), Some(after)) = (ir(before, &empty), ir(after, &empty)) else {
        return true;
    };
    if !before.tests.is_empty() || !after.tests.is_empty() {
        return false;
    }
    before.imports != after.imports
        || before.helpers.len() != after.helpers.len()
        || before.helpers.iter().zip(&after.helpers).any(|(a, b)| {
            a.name != b.name
                || a.suites != b.suites
                || a.params != b.params
                || a.is_async != b.is_async
                || a.default_export != b.default_export
                || !same_items(&a.body.items, &b.body.items)
        })
}

/// Whether two versions of the same file check the same things in every test, ignoring
/// lines.
fn same_checks(before: &FileIr, after: &FileIr) -> bool {
    before.tests.len() == after.tests.len()
        && before
            .tests
            .iter()
            .zip(&after.tests)
            .all(|(a, b)| same_items(&a.body.items, &b.body.items))
}

fn same_items(before: &[Item], after: &[Item]) -> bool {
    let same_via = |a: &Via, b: &Via| {
        (&a.name, &a.call, &a.context, a.awaited, &a.args, &a.path)
            == (&b.name, &b.call, &b.context, b.awaited, &b.args, &b.path)
    };
    // A call's helper may have moved lines; whether it's followed, and from where, counts.
    let same_kind = |a: &ItemKind, b: &ItemKind| match (a, b) {
        (ItemKind::Call(a), ItemKind::Call(b)) => {
            (
                &a.text,
                a.awaited,
                a.defined.as_ref().map(|defined| &defined.path),
            ) == (
                &b.text,
                b.awaited,
                b.defined.as_ref().map(|defined| &defined.path),
            )
        }
        _ => a == b,
    };
    before.len() == after.len()
        && before.iter().zip(after).all(|(a, b)| {
            same_kind(&a.kind, &b.kind)
                && a.context == b.context
                && a.via.len() == b.via.len()
                && a.via.iter().zip(&b.via).all(|(a, b)| same_via(a, b))
        })
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

/// One side of a helper file (read already), to see whether its helpers changed. One that
/// isn't UTF-8 text holds no helpers: it isn't followed either.
fn read_helpers(modules: &Modules, path: Option<&str>) -> Read {
    let Some(source) = path.and_then(|path| modules.text(path)) else {
        return Read::Missing;
    };
    let path = path.expect("has a text");
    match assertrust_ir::parse_file(path, source) {
        Ok(ir) => Read::Parsed(Parsed {
            path: path.to_owned(),
            ir,
        }),
        Err(err) => Read::Skipped(format!("{path} ({}): skipped, {err}", modules.rev())),
    }
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
