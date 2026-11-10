//! Following imports for linking (`docs/architecture.md`, step 3, "Linked view"): the
//! helper files a test file imports, read and parsed at one revision, so their checks can
//! be put in the tests that call them.
//!
//! ```text
//! test/refund.test.ts: import { expectOk } from './helpers'
//! → test/helpers.ts (tried in order: test/helpers.ts, test/helpers.tsx, …, test/helpers/index.ts, …)
//! ```
//!
//! Only files in test folders are followed (step 1's Test and Test support kinds): that's
//! where helpers live, and reading every source file a test imports would cost a git call
//! and a parse for nothing.

use std::collections::{HashMap, HashSet};

use assertrust_git::{Classifier, FileKind, Repo, Rev};
use assertrust_ir::{FileIr, Import, ItemKind};

/// How deep imports are followed from a test file: helpers importing helpers, re-exports.
const MAX_DEPTH: usize = 8;

/// How many helper files [`importers`] looks for importers of, at most, including the
/// ones that pass the changed file on.
const MAX_SEARCHES: usize = 20;

/// The extensions an import can leave out, in the order they're tried.
const EXTENSIONS: [&str; 8] = ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// The files [`importers`] searches.
const PATHSPECS: [&str; 8] = [
    "*.ts", "*.tsx", "*.mts", "*.cts", "*.js", "*.jsx", "*.mjs", "*.cjs",
];

/// The helper files at one revision, read as test files ask for them.
pub struct Modules<'a> {
    repo: &'a Repo,
    rev: &'a Rev,
    classifier: &'a Classifier,
    /// Where each import points, by the path it names (`test/helpers` for `./helpers` in
    /// `test/a.test.ts`).
    targets: HashMap<String, Target>,
    files: HashMap<String, FileIr>,
}

enum Target {
    /// Not a file in a test folder, or no file at all: not followed.
    None,
    File(String),
    /// A file that doesn't parse: `test/helpers.ts (1a2b3c4) doesn't parse (…)`.
    Broken(String),
}

impl<'a> Modules<'a> {
    pub fn new(repo: &'a Repo, rev: &'a Rev, classifier: &'a Classifier) -> Self {
        Self {
            repo,
            rev,
            classifier,
            targets: HashMap::new(),
            files: HashMap::new(),
        }
    }

    /// Reads the helper files `file` (at `path`) imports and calls, and the ones they
    /// import in turn.
    pub fn follow(&mut self, path: &str, file: &FileIr) -> Result<(), String> {
        self.follow_from(path, file, 0)
    }

    fn follow_from(&mut self, path: &str, file: &FileIr, depth: usize) -> Result<(), String> {
        if depth >= MAX_DEPTH {
            return Ok(());
        }
        for import in followed(file) {
            let Some(target) = import_path(path, &import.from) else {
                continue;
            };
            if self.targets.contains_key(&target) {
                continue;
            }
            let candidates: Vec<String> = candidates(&target)
                .into_iter()
                .filter(|candidate| in_test_folder(self.classifier, candidate))
                .collect();
            let found = if candidates.is_empty() {
                None
            } else {
                let contents = self
                    .repo
                    .read_files(self.rev, &candidates)
                    .map_err(|err| err.to_string())?;
                candidates
                    .into_iter()
                    .zip(contents)
                    .find_map(|(candidate, source)| Some((candidate, source?)))
            };
            let Some((found, source)) = found else {
                self.targets.insert(target, Target::None);
                continue;
            };
            if self.files.contains_key(&found) {
                self.targets.insert(target, Target::File(found));
                continue;
            }
            match assertrust_ir::parse_file(&found, &source) {
                Ok(ir) => {
                    self.targets.insert(target, Target::File(found.clone()));
                    self.follow_from(&found, &ir, depth + 1)?;
                    self.files.insert(found, ir);
                }
                Err(err) => {
                    let note = format!("{found} ({}) {err}", self.rev);
                    self.targets.insert(target, Target::Broken(note));
                }
            }
        }
        Ok(())
    }

    /// The helper files `file` reaches that don't parse, as `test/helpers.ts (1a2b3c4)
    /// doesn't parse (…)`. Linking without them would make their checks look removed (or
    /// added).
    pub fn problems(&self, path: &str, file: &FileIr) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut notes = Vec::new();
        self.collect_problems(path, file, &mut seen, &mut notes);
        notes
    }

    fn collect_problems<'s>(
        &'s self,
        path: &str,
        file: &FileIr,
        seen: &mut HashSet<&'s str>,
        notes: &mut Vec<String>,
    ) {
        for import in followed(file) {
            let Some(target) = import_path(path, &import.from) else {
                continue;
            };
            match self.targets.get(&target) {
                Some(Target::Broken(note)) if !notes.contains(note) => notes.push(note.clone()),
                Some(Target::File(found)) if seen.insert(found) => {
                    if let Some(ir) = self.files.get(found) {
                        self.collect_problems(found, ir, seen, notes);
                    }
                }
                _ => {}
            }
        }
    }
}

impl assertrust_ir::Modules for Modules<'_> {
    fn module(&self, from: &str, specifier: &str) -> Option<(&str, &FileIr)> {
        let Target::File(found) = self.targets.get(&import_path(from, specifier)?)? else {
            return None;
        };
        self.files
            .get_key_value(found)
            .map(|(path, ir)| (path.as_str(), ir))
    }
}

/// The test files at `rev` that call helpers from the file `path`, directly or through
/// other helper files that pass them on (`export * from './helpers'`), parsed.
///
/// A search for the file's name in imports finds the candidates (`/helpers'`), then each
/// one's imports are resolved to make sure.
pub fn importers(
    repo: &Repo,
    rev: &Rev,
    classifier: &Classifier,
    path: &str,
) -> Result<Vec<(String, FileIr)>, String> {
    let mut searched = vec![path.to_owned()];
    let mut seen: HashSet<String> = HashSet::from([path.to_owned()]);
    let mut tests = Vec::new();
    let mut next = 0;
    while next < searched.len() && next < MAX_SEARCHES {
        let target = searched[next].clone();
        next += 1;
        let Some(pattern) = import_pattern(&target) else {
            continue;
        };
        let paths: Vec<String> = repo
            .files_matching(rev, &pattern, &PATHSPECS)
            .map_err(|err| err.to_string())?
            .into_iter()
            .filter(|path| !seen.contains(path) && in_test_folder(classifier, path))
            .collect();
        let contents = repo
            .read_files(rev, &paths)
            .map_err(|err| err.to_string())?;
        for (path, source) in paths.into_iter().zip(contents) {
            let Some(source) = source else {
                continue;
            };
            let Ok(ir) = assertrust_ir::parse_file(&path, &source) else {
                continue;
            };
            let imports_target = followed(&ir).any(|import| {
                import_path(&path, &import.from)
                    .is_some_and(|named| candidates(&named).contains(&target))
            });
            if !imports_target {
                continue;
            }
            seen.insert(path.clone());
            if classifier.classify(&path) == FileKind::Test {
                tests.push((path, ir));
            } else {
                searched.push(path);
            }
        }
    }
    Ok(tests)
}

/// The imports of `file` worth following: the ones it calls, and the ones it passes on.
fn followed(file: &FileIr) -> impl Iterator<Item = &Import> {
    let mut called = HashSet::new();
    let bodies = file
        .tests
        .iter()
        .map(|test| &test.body)
        .chain(file.hooks.iter().map(|hook| &hook.body))
        .chain(file.helpers.iter().map(|helper| &helper.body));
    for body in bodies {
        for item in &body.items {
            if let ItemKind::Call(call) = &item.kind {
                called.insert(call.name.as_str());
            }
        }
    }
    file.imports
        .iter()
        .filter(move |import| import.reexport || called.contains(import.local.as_str()))
}

fn in_test_folder(classifier: &Classifier, path: &str) -> bool {
    matches!(
        classifier.classify(path),
        FileKind::Test | FileKind::TestSupport
    )
}

/// The path an import names, from the repo root, before extensions are tried: `./helpers`
/// in `test/a.test.ts` → `test/helpers`. `None` when it leaves the repo.
fn import_path(from: &str, specifier: &str) -> Option<String> {
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for part in specifier.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// The files an import path may point to, in the order they're tried: the path with each
/// extension, then an `index` file in it. A path with a JS extension may name its
/// TypeScript source (`./helpers.js` for `helpers.ts`), as ESM TypeScript writes it.
fn candidates(path: &str) -> Vec<String> {
    if let Some((stem, extension)) = path.rsplit_once('.')
        && !extension.contains('/')
        && EXTENSIONS.contains(&extension)
    {
        let sources: &[&str] = match extension {
            "js" => &["ts", "tsx"],
            "jsx" => &["tsx"],
            "mjs" => &["mts"],
            "cjs" => &["cts"],
            _ => &[],
        };
        let mut candidates = vec![path.to_owned()];
        candidates.extend(sources.iter().map(|source| format!("{stem}.{source}")));
        return candidates;
    }
    let index = if path.is_empty() {
        "index".to_owned()
    } else {
        format!("{path}/index")
    };
    let with_extensions = |base: &str| {
        EXTENSIONS
            .iter()
            .map(|extension| format!("{base}.{extension}"))
            .collect::<Vec<_>>()
    };
    let mut candidates = if path.is_empty() {
        Vec::new()
    } else {
        with_extensions(path)
    };
    candidates.extend(with_extensions(&index));
    candidates
}

/// A regular expression for import paths that may name the file `path`: its name (or its
/// folder's, for an `index` file) at the end of a quoted relative path. `/helpers'`,
/// `/helpers.js"` and `/helpers/index'` all match for `test/helpers.ts`.
fn import_pattern(path: &str) -> Option<String> {
    let (folder, file) = path.rsplit_once('/').unwrap_or(("", path));
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let name = if stem == "index" {
        folder.rsplit('/').next().filter(|name| !name.is_empty())?
    } else {
        stem
    };
    let mut escaped = String::new();
    for c in name.chars() {
        if "\\.^$|?*+()[]{}".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    Some(format!("/{escaped}(/index)?(\\.[cm]?[jt]sx?)?['\"`]"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_import_paths_from_the_importing_file() {
        assert_eq!(
            import_path("test/a.test.ts", "./helpers").as_deref(),
            Some("test/helpers")
        );
        assert_eq!(
            import_path("test/api/a.test.ts", "../support/http").as_deref(),
            Some("test/support/http")
        );
        assert_eq!(import_path("a.test.ts", "../outside"), None);
    }

    #[test]
    fn tries_extensions_then_an_index_file() {
        let tried = candidates("test/helpers");
        assert_eq!(tried[0], "test/helpers.ts");
        assert_eq!(tried[8], "test/helpers/index.ts");
        assert_eq!(tried.len(), 16);
        assert_eq!(
            candidates("test/helpers.js"),
            ["test/helpers.js", "test/helpers.ts", "test/helpers.tsx"]
        );
        // Not an extension: `helpers.mock` is a name.
        assert_eq!(candidates("test/helpers.mock")[0], "test/helpers.mock.ts");
    }

    #[test]
    fn searches_for_the_file_name_or_its_folder() {
        assert_eq!(
            import_pattern("test/helpers.ts").as_deref(),
            Some("/helpers(/index)?(\\.[cm]?[jt]sx?)?['\"`]")
        );
        assert_eq!(
            import_pattern("test/http/index.ts").as_deref(),
            Some("/http(/index)?(\\.[cm]?[jt]sx?)?['\"`]")
        );
        assert_eq!(
            import_pattern("test/a.b.ts").as_deref(),
            Some("/a\\.b(/index)?(\\.[cm]?[jt]sx?)?['\"`]")
        );
        assert_eq!(import_pattern("index.ts"), None);
    }
}
