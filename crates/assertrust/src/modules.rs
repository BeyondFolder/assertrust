//! Following imports for linking (`docs/architecture.md`, step 2, "Helper functions"): the
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
//! and a parse for nothing. Imports that aren't relative go through the nearest
//! `tsconfig.json`'s `paths` and `baseUrl`.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use assertrust_git::{Classifier, FileKind, Repo, Rev};
use assertrust_ir::{FileIr, Import, ItemKind, TsConfig};

/// How deep imports are followed from a test file: helpers importing helpers, re-exports.
/// Also how deep `extends` is followed between tsconfig files.
const MAX_DEPTH: usize = 8;

/// How many helper files [`Modules::importers`] looks for importers of, at most, including
/// the ones that pass the changed file on.
const MAX_SEARCHES: usize = 100;

/// The extensions an import can leave out, in the order they're tried.
const EXTENSIONS: [&str; 8] = ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// The files [`Modules::importers`] searches.
const PATHSPECS: [&str; 8] = [
    "*.ts", "*.tsx", "*.mts", "*.cts", "*.js", "*.jsx", "*.mjs", "*.cjs",
];

/// The test files that call a helper file's helpers, parsed, and a note when the search
/// stopped early.
pub type Importers = (Vec<(String, FileIr)>, Option<String>);

/// The helper files at one revision, read as test files ask for them.
pub struct Modules<'a> {
    repo: &'a Repo,
    rev: &'a Rev,
    classifier: &'a Classifier,
    /// Where each import points, by the importing file and the import path as written.
    imports: HashMap<(String, String), Target>,
    /// Where a list of candidate files points: the first one that exists.
    found: HashMap<Vec<String>, Target>,
    /// Files read, by path: `None` when missing or not UTF-8 text.
    texts: HashMap<String, Option<String>>,
    files: HashMap<String, Rc<FileIr>>,
    /// Files whose imports have been followed at this revision.
    followed: HashSet<String>,
    /// The aliases for files in each folder, from the nearest `tsconfig.json`.
    aliases: HashMap<String, Rc<Aliases>>,
    /// tsconfig files read, by path: `None` when missing or not JSON.
    configs: HashMap<String, Option<TsConfig>>,
}

#[derive(Clone)]
enum Target {
    /// Not a file in a test folder, or no file at all: not followed.
    None,
    File(String),
    /// A file that doesn't parse: `test/helpers.ts (1a2b3c4) doesn't parse (…)`.
    Broken(String),
}

/// What a `tsconfig.json` says about imports that aren't relative, with paths from the repo
/// root.
#[derive(Default)]
struct Aliases {
    /// `paths`: each pattern (`@/*`) with what it stands for (`src/*`).
    paths: Vec<(String, Vec<String>)>,
    base_url: Option<String>,
}

impl<'a> Modules<'a> {
    pub fn new(repo: &'a Repo, rev: &'a Rev, classifier: &'a Classifier) -> Self {
        Self {
            repo,
            rev,
            classifier,
            imports: HashMap::new(),
            found: HashMap::new(),
            texts: HashMap::new(),
            files: HashMap::new(),
            followed: HashSet::new(),
            aliases: HashMap::new(),
            configs: HashMap::new(),
        }
    }

    /// The helper files read, with their paths.
    pub fn files(&self) -> impl Iterator<Item = (&str, &FileIr)> {
        self.files.iter().map(|(path, ir)| (path.as_str(), &**ir))
    }

    /// The revision it reads.
    pub fn rev(&self) -> &Rev {
        self.rev
    }

    /// The files at `rev`, starting from what was read here: apart from the files a change
    /// touched (`changed`), they read the same. The other side of a comparison, or the
    /// next commit's.
    pub fn successor(&self, rev: &'a Rev, changed: &HashSet<String>) -> Self {
        let unchanged = |path: &&String| !changed.contains(*path);
        Self {
            repo: self.repo,
            rev,
            classifier: self.classifier,
            imports: HashMap::new(),
            found: HashMap::new(),
            texts: self
                .texts
                .iter()
                .filter(|(path, _)| unchanged(path))
                .map(|(path, text)| (path.clone(), text.clone()))
                .collect(),
            files: self
                .files
                .iter()
                .filter(|(path, _)| unchanged(path))
                .map(|(path, ir)| (path.clone(), Rc::clone(ir)))
                .collect(),
            followed: HashSet::new(),
            aliases: HashMap::new(),
            configs: self
                .configs
                .iter()
                .filter(|(path, _)| unchanged(path))
                .map(|(path, config)| (path.clone(), config.clone()))
                .collect(),
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
        // Where each new import may point; all of them are read in one go.
        let mut pending: Vec<((String, String), Vec<String>)> = Vec::new();
        for import in followed(file) {
            let key = (path.to_owned(), import.from.clone());
            if self.imports.contains_key(&key) || pending.iter().any(|(other, _)| *other == key) {
                continue;
            }
            let candidates: Vec<String> = self
                .candidates(path, &import.from)?
                .into_iter()
                .filter(|candidate| in_test_folder(self.classifier, candidate))
                .collect();
            pending.push((key, candidates));
        }
        let unread: Vec<String> = pending
            .iter()
            .filter(|(_, candidates)| !self.found.contains_key(candidates))
            .flat_map(|(_, candidates)| candidates.iter().cloned())
            .collect();
        self.read(unread)?;
        let mut new = Vec::new();
        for (key, candidates) in pending {
            let target = self.pick(candidates);
            if let Target::File(found) = &target
                && self.followed.insert(found.clone())
            {
                new.push(found.clone());
            }
            self.imports.insert(key, target);
        }
        for found in new {
            let ir = Rc::clone(&self.files[&found]);
            self.follow_from(&found, &ir, depth + 1)?;
        }
        Ok(())
    }

    /// A file's text at this revision, once [`Modules::read`] has read it: `None` when it's
    /// missing or isn't UTF-8 text.
    pub fn text(&self, path: &str) -> Option<&str> {
        self.texts.get(path)?.as_deref()
    }

    /// Reads the files not read yet, with one git call.
    pub fn read(&mut self, paths: Vec<String>) -> Result<(), String> {
        let mut unread: Vec<String> = paths
            .into_iter()
            .filter(|path| !self.texts.contains_key(path))
            .collect();
        unread.sort();
        unread.dedup();
        if unread.is_empty() {
            return Ok(());
        }
        let contents = self
            .repo
            .read_files(self.rev, &unread)
            .map_err(|err| err.to_string())?;
        for (path, bytes) in unread.into_iter().zip(contents) {
            // A file that isn't text isn't a helper file.
            let text = bytes.and_then(|bytes| String::from_utf8(bytes).ok());
            self.texts.insert(path, text);
        }
        Ok(())
    }

    /// The first of `candidates` that exists (they've been read), parsed.
    fn pick(&mut self, candidates: Vec<String>) -> Target {
        if let Some(target) = self.found.get(&candidates) {
            return target.clone();
        }
        let first = candidates.iter().find_map(|candidate| {
            let text = self.texts.get(candidate)?.as_deref()?;
            Some((candidate.clone(), text))
        });
        let target = match first {
            None => Target::None,
            Some((found, _)) if self.files.contains_key(&found) => Target::File(found),
            Some((found, source)) => match assertrust_ir::parse_file(&found, source) {
                Ok(ir) => {
                    self.files.insert(found.clone(), Rc::new(ir));
                    Target::File(found)
                }
                Err(err) => Target::Broken(format!("{found} ({}) {err}", self.rev)),
            },
        };
        self.found.insert(candidates, target.clone());
        target
    }

    /// Follows the import of `local` in `file` (at `path`) even into a file outside test
    /// folders: the helper moved there (`test/helpers.ts` → `src/testing.ts`) and was
    /// followed on the other side of the change. Says whether it now leads to a file.
    pub fn follow_outside(
        &mut self,
        path: &str,
        file: &FileIr,
        local: &str,
    ) -> Result<bool, String> {
        let Some(import) = file
            .imports
            .iter()
            .find(|import| !import.reexport && import.local == local)
        else {
            return Ok(false);
        };
        let key = (path.to_owned(), import.from.clone());
        if matches!(self.imports.get(&key), Some(Target::File(_))) {
            return Ok(false);
        }
        let candidates = self.candidates(path, &import.from)?;
        self.read(candidates.clone())?;
        let target = self.pick(candidates);
        let Target::File(found) = &target else {
            return Ok(false);
        };
        if self.followed.insert(found.clone()) {
            let ir = Rc::clone(&self.files[found]);
            self.follow_from(found, &ir, 1)?;
        }
        self.imports.insert(key, target);
        Ok(true)
    }

    /// The names `file` (at `path`) imports, calls, and could follow.
    pub fn followed_names(&self, path: &str, file: &FileIr) -> HashSet<String> {
        followed(file)
            .filter(|import| {
                !import.reexport
                    && matches!(
                        self.imports.get(&(path.to_owned(), import.from.clone())),
                        Some(Target::File(_))
                    )
            })
            .map(|import| import.local.clone())
            .collect()
    }

    /// The files an import may point to, in the order they're tried: a relative path from
    /// the importing file, else the aliases and `baseUrl` of the nearest `tsconfig.json`.
    fn candidates(&mut self, from: &str, specifier: &str) -> Result<Vec<String>, String> {
        if specifier.starts_with('.') {
            return Ok(import_path(from, specifier)
                .map(|path| candidates(&path))
                .unwrap_or_default());
        }
        let aliases = self.aliases(parent(from))?;
        Ok(aliases
            .resolve(specifier)
            .iter()
            .flat_map(|path| candidates(path))
            .collect())
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
            match self.imports.get(&(path.to_owned(), import.from.clone())) {
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

    /// Whether a helper in `file` (at `path`) checks anything, itself or through the helper
    /// files it follows. One that doesn't parse counts, so the tests that call it say so.
    pub fn has_checks(&self, path: &str, file: &FileIr) -> bool {
        let mut seen = HashSet::new();
        self.has_checks_in(path, file, &mut seen)
    }

    fn has_checks_in<'s>(&'s self, path: &str, file: &FileIr, seen: &mut HashSet<&'s str>) -> bool {
        let checks = file.helpers.iter().any(|helper| {
            helper.body.items.iter().any(|item| {
                matches!(
                    item.kind,
                    ItemKind::Assertion(_) | ItemKind::AssertionCount(_)
                )
            })
        });
        checks
            || followed(file).any(|import| {
                match self.imports.get(&(path.to_owned(), import.from.clone())) {
                    Some(Target::Broken(_)) => true,
                    Some(Target::File(found)) if seen.insert(found) => self
                        .files
                        .get(found)
                        .is_some_and(|ir| self.has_checks_in(found, ir, seen)),
                    _ => false,
                }
            })
    }

    /// The aliases for files in `folder`: from the nearest `tsconfig.json` above it. Each
    /// folder on the way gets the answer too, so a sibling folder reads less.
    fn aliases(&mut self, folder: &str) -> Result<Rc<Aliases>, String> {
        // Up from `folder` to the first one already known.
        let mut unknown = Vec::new();
        let mut current = folder;
        let known = loop {
            if let Some(aliases) = self.aliases.get(current) {
                break Rc::clone(aliases);
            }
            unknown.push(current.to_owned());
            if current.is_empty() {
                break Rc::new(Aliases::default());
            }
            current = parent(current);
        };
        if unknown.is_empty() {
            return Ok(known);
        }
        let paths: Vec<String> = unknown
            .iter()
            .map(|folder| join(folder, "tsconfig.json"))
            .collect();
        self.read(paths.clone())?;
        let nearest = paths
            .into_iter()
            .enumerate()
            .find(|(_, path)| matches!(self.texts.get(path), Some(Some(_))));
        let (found, below) = match nearest {
            Some((index, path)) => (Rc::new(self.effective(&path, 0)?), index + 1),
            None => (Rc::clone(&known), unknown.len()),
        };
        // The folders up to the tsconfig use it; the ones above it, what was known.
        for (index, folder) in unknown.into_iter().enumerate() {
            let aliases = if index < below { &found } else { &known };
            self.aliases.insert(folder, Rc::clone(aliases));
        }
        Ok(found)
    }

    /// The tsconfig at `path` with what it `extends` (files in the repo, not packages).
    fn effective(&mut self, path: &str, depth: usize) -> Result<Aliases, String> {
        let Some(config) = self.config(path)? else {
            return Ok(Aliases::default());
        };
        let folder = parent(path);
        let mut aliases = Aliases::default();
        if depth < MAX_DEPTH {
            for base in &config.extends {
                if !base.starts_with('.') {
                    continue;
                }
                let Some(base) = import_path(path, base) else {
                    continue;
                };
                // `./base` may leave out `.json`.
                let candidates = if base.ends_with(".json") {
                    vec![base]
                } else {
                    vec![base.clone(), format!("{base}.json")]
                };
                for candidate in candidates {
                    if self.config(&candidate)?.is_some() {
                        let inherited = self.effective(&candidate, depth + 1)?;
                        if inherited.base_url.is_some() {
                            aliases.base_url = inherited.base_url;
                        }
                        if !inherited.paths.is_empty() {
                            aliases.paths = inherited.paths;
                        }
                        break;
                    }
                }
            }
        }
        if let Some(base_url) = &config.base_url {
            aliases.base_url = Some(join(folder, base_url));
        }
        if let Some(paths) = &config.paths {
            // Relative to `baseUrl` when there is one, else to the file that sets them.
            let root = aliases
                .base_url
                .clone()
                .unwrap_or_else(|| folder.to_owned());
            aliases.paths = paths
                .iter()
                .map(|(pattern, targets)| {
                    let targets = targets.iter().map(|target| join(&root, target)).collect();
                    (pattern.clone(), targets)
                })
                .collect();
        }
        Ok(aliases)
    }

    /// The tsconfig file at `path`, read once.
    fn config(&mut self, path: &str) -> Result<Option<TsConfig>, String> {
        if let Some(config) = self.configs.get(path) {
            return Ok(config.clone());
        }
        self.read(vec![path.to_owned()])?;
        let config = self.texts[path]
            .as_deref()
            .and_then(assertrust_ir::parse_tsconfig);
        self.configs.insert(path.to_owned(), config.clone());
        Ok(config)
    }

    /// The test files that call helpers from the files `paths`, directly or through other
    /// helper files that pass them on or call them (`export * from './helpers'`), parsed.
    /// With a note when there were too many helper files to look through.
    ///
    /// One search per step away from `paths` finds the candidates (their names in import
    /// paths: `/helpers'`), then each one's imports are resolved to make sure.
    pub fn importers(&mut self, paths: &[String]) -> Result<Importers, String> {
        let mut seen: HashSet<String> = paths.iter().cloned().collect();
        let mut targets: Vec<String> = paths.to_vec();
        let mut tests = Vec::new();
        let mut searched = 0;
        for _ in 0..MAX_DEPTH {
            if targets.is_empty() {
                break;
            }
            if searched + targets.len() > MAX_SEARCHES {
                break;
            }
            searched += targets.len();
            let patterns: Vec<String> = targets.iter().filter_map(|t| import_pattern(t)).collect();
            if patterns.is_empty() {
                break;
            }
            let found: Vec<String> = self
                .repo
                .files_matching(self.rev, &patterns.join("|"), &PATHSPECS)
                .map_err(|err| err.to_string())?
                .into_iter()
                .filter(|path| !seen.contains(path) && in_test_folder(self.classifier, path))
                .collect();
            let contents = self
                .repo
                .read_files(self.rev, &found)
                .map_err(|err| err.to_string())?;
            let mut next = Vec::new();
            for (path, bytes) in found.into_iter().zip(contents) {
                let Some(source) = bytes.and_then(|bytes| String::from_utf8(bytes).ok()) else {
                    continue;
                };
                let Ok(ir) = assertrust_ir::parse_file(&path, &source) else {
                    continue;
                };
                let mut imports_target = false;
                for import in followed(&ir) {
                    let candidates = self.candidates(&path, &import.from)?;
                    if targets.iter().any(|target| candidates.contains(target)) {
                        imports_target = true;
                        break;
                    }
                }
                if !imports_target {
                    continue;
                }
                seen.insert(path.clone());
                if self.classifier.classify(&path) == FileKind::Test {
                    tests.push((path, ir));
                } else {
                    next.push(path);
                }
            }
            targets = next;
        }
        let note = (!targets.is_empty()).then(|| {
            format!(
                "{} ({}): stopped looking for tests that call its helpers, too many helper files pass them on",
                paths.join(", "),
                self.rev
            )
        });
        Ok((tests, note))
    }
}

impl assertrust_ir::Modules for Modules<'_> {
    fn module(&self, from: &str, specifier: &str) -> Option<(&str, &FileIr)> {
        let key = (from.to_owned(), specifier.to_owned());
        let Target::File(found) = self.imports.get(&key)? else {
            return None;
        };
        self.files
            .get_key_value(found)
            .map(|(path, ir)| (path.as_str(), &**ir))
    }
}

impl Aliases {
    /// The paths an import that isn't relative may name, before extensions are tried: what
    /// the best `paths` pattern stands for (an exact match, else the longest prefix), then
    /// the import under `baseUrl`.
    fn resolve(&self, specifier: &str) -> Vec<String> {
        let mut best: Option<(usize, &[String], &str)> = None;
        for (pattern, targets) in &self.paths {
            let matched = match pattern.split_once('*') {
                None => (pattern == specifier).then_some((usize::MAX, "")),
                Some((prefix, suffix)) => (specifier.len() >= prefix.len() + suffix.len()
                    && specifier.starts_with(prefix)
                    && specifier.ends_with(suffix))
                .then(|| {
                    (
                        prefix.len(),
                        &specifier[prefix.len()..specifier.len() - suffix.len()],
                    )
                }),
            };
            if let Some((rank, star)) = matched
                && best.is_none_or(|(best, _, _)| rank > best)
            {
                best = Some((rank, targets, star));
            }
        }
        let mut paths: Vec<String> = best
            .map(|(_, targets, star)| {
                targets
                    .iter()
                    .map(|target| target.replacen('*', star, 1))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(base_url) = &self.base_url {
            paths.push(join(base_url, specifier));
        }
        paths
    }
}

/// The imports of `file` worth following: the ones it calls, calls methods on, builds objects
/// from or extends (their methods may be helpers), and the ones it passes on.
fn followed(file: &FileIr) -> impl Iterator<Item = &Import> {
    let mut used: HashSet<&str> = file.constructed.iter().map(String::as_str).collect();
    let bodies = file
        .tests
        .iter()
        .map(|test| &test.body)
        .chain(file.hooks.iter().map(|hook| &hook.body))
        .chain(file.helpers.iter().map(|helper| &helper.body));
    for body in bodies {
        for item in &body.items {
            if let ItemKind::Call(call) = &item.kind {
                match &call.receiver {
                    None => used.insert(call.name.as_str()),
                    // `api.expectOk(…)`, `app.album.expectOpen(…)`: the name it starts from.
                    Some(receiver) => used.insert(receiver.split('.').next().unwrap_or_default()),
                };
            }
        }
    }
    file.imports
        .iter()
        .filter(move |import| import.reexport || used.contains(import.local.as_str()))
}

fn in_test_folder(classifier: &Classifier, path: &str) -> bool {
    matches!(
        classifier.classify(path),
        FileKind::Test | FileKind::TestSupport
    )
}

/// The folder a repo path is in: `test` for `test/a.test.ts`, empty at the root.
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

/// `relative` from the folder `folder`, as a path from the repo root, with `.` and `..`
/// worked out. A `..` above the root is dropped.
fn join(folder: &str, relative: &str) -> String {
    let mut parts: Vec<&str> = folder.split('/').filter(|part| !part.is_empty()).collect();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
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
/// folder's, for an `index` file) at the end of a quoted path. `/helpers'`, `/helpers.js"`,
/// `@/test/helpers'` and `/helpers/index'` all match for `test/helpers.ts`. An `index` file
/// can also be imported by dots alone: `'.'`, `'..'`, `'../index'`.
fn import_pattern(path: &str) -> Option<String> {
    const EXTENSION: &str = "(\\.[cm]?[jt]sx?)?";
    let (folder, file) = path.rsplit_once('/').unwrap_or(("", path));
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    if stem != "index" {
        return Some(format!("/{}(/index)?{EXTENSION}['\"`]", escape(stem)));
    }
    let name = folder.rsplit('/').next().filter(|name| !name.is_empty())?;
    Some(format!(
        "/{}(/index)?{EXTENSION}['\"`]|['\"`]\\.\\.?(/\\.\\.)*(/index{EXTENSION})?/?['\"`]",
        escape(name)
    ))
}

/// `name` with the characters a regular expression gives a meaning to escaped.
fn escape(name: &str) -> String {
    let mut escaped = String::new();
    for c in name.chars() {
        if "\\.^$|?*+()[]{}".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
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
    fn aliases_pick_the_best_pattern_then_base_url() {
        let aliases = Aliases {
            paths: vec![
                ("@/*".to_owned(), vec!["src/*".to_owned()]),
                ("@/test/*".to_owned(), vec!["test/*".to_owned()]),
                ("#shared".to_owned(), vec!["shared/index.ts".to_owned()]),
            ],
            base_url: Some(String::new()),
        };
        assert_eq!(
            aliases.resolve("@/test/helpers"),
            ["test/helpers", "@/test/helpers"]
        );
        assert_eq!(aliases.resolve("@/api"), ["src/api", "@/api"]);
        assert_eq!(aliases.resolve("#shared"), ["shared/index.ts", "#shared"]);
        assert_eq!(aliases.resolve("vitest"), ["vitest"]);
        assert!(Aliases::default().resolve("vitest").is_empty());
    }

    #[test]
    fn joins_paths_from_a_folder() {
        assert_eq!(join("", "./src/*"), "src/*");
        assert_eq!(join("packages/a", "../../test/*"), "test/*");
        assert_eq!(join("packages/a", "."), "packages/a");
    }

    #[test]
    fn searches_for_the_file_name_or_its_folder() {
        assert_eq!(
            import_pattern("test/helpers.ts").as_deref(),
            Some("/helpers(/index)?(\\.[cm]?[jt]sx?)?['\"`]")
        );
        assert_eq!(
            import_pattern("test/a.b.ts").as_deref(),
            Some("/a\\.b(/index)?(\\.[cm]?[jt]sx?)?['\"`]")
        );
        assert_eq!(import_pattern("index.ts"), None);
    }

    #[test]
    fn an_index_file_is_found_by_dots_too() {
        let pattern = regex_lite(import_pattern("test/http/index.ts").unwrap());
        for import in [
            "'./http'",
            "\"../http/index.js\"",
            "'.'",
            "'..'",
            "'../index'",
            "'../../index.ts'",
            "'./'",
        ] {
            assert!(pattern(import), "{import}");
        }
        for import in ["'./https'", "'vitest'", "'./other'"] {
            assert!(!pattern(import), "{import}");
        }
    }

    /// Runs `pattern` with `git grep -E` on one line, as the search does.
    fn regex_lite(pattern: String) -> impl Fn(&str) -> bool {
        move |line: &str| {
            let dir = tempfile::tempdir().unwrap();
            let git = |args: &[&str]| {
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(dir.path())
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .output()
                    .unwrap()
            };
            git(&["init", "-q"]);
            std::fs::write(dir.path().join("a.ts"), format!("import x from {line}\n")).unwrap();
            git(&[
                "grep",
                "--no-index",
                "-q",
                "-E",
                "-e",
                &pattern,
                "--",
                "a.ts",
            ])
            .status
            .success()
        }
    }
}
