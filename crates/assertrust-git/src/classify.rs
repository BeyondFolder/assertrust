//! Labelling changed files by kind (`docs/architecture.md`, step 1, "Which files").
//!
//! Checked in this order, first match wins:
//! 1. **Test:** `*.test.*` / `*.spec.*` code files, and code files under `__tests__/`
//! 2. **Gate:** GitHub workflows, vitest/jest/playwright config, `package.json`
//! 3. **Test support:** other code files under `test/`, `tests/`, `__tests__/`, `__mocks__/`
//!    or `e2e/`. Helpers are followed into these files and Test files only (step 2).
//! 4. **Source:** other code files
//! 5. **Other:** everything else

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::model::FileKind;

/// The order kinds are checked in, strongest first.
const ORDER: [FileKind; 5] = [
    FileKind::Test,
    FileKind::Gate,
    FileKind::TestSupport,
    FileKind::Source,
    FileKind::Other,
];

/// JS/TS extensions: the files assertrust parses as tests.
const TEST_EXTENSIONS: &str = "{ts,tsx,js,jsx,mts,cts,mjs,cjs}";

/// Extensions counted as source code.
const SOURCE_EXTENSIONS: &str =
    "{ts,tsx,js,jsx,mts,cts,mjs,cjs,vue,svelte,astro,py,rb,go,rs,java,kt,php,cs,swift}";

/// Decides the [`FileKind`] of a path. [`Classifier::default`] has the built-in patterns.
#[derive(Debug, Clone)]
pub struct Classifier {
    test: GlobSet,
    gate: GlobSet,
    test_support: GlobSet,
    source: GlobSet,
}

impl Classifier {
    /// A classifier with custom glob patterns per kind. Paths are relative to the repo root,
    /// `*` doesn't cross `/`, and `**/` also matches the root.
    pub fn new(test: &[String], gate: &[String], test_support: &[String]) -> Result<Self, String> {
        Ok(Self {
            test: glob_set(test)?,
            gate: glob_set(gate)?,
            test_support: glob_set(test_support)?,
            source: glob_set(&[format!("**/*.{SOURCE_EXTENSIONS}")])?,
        })
    }

    /// The kind of a changed file. A renamed file gets whichever of its two paths ranks
    /// higher in the order above: `a.test.ts` → `a.test.ts.disabled` is a test leaving
    /// the suite, not a new "other" file.
    pub(crate) fn classify_change(&self, before: Option<&str>, after: Option<&str>) -> FileKind {
        let kinds = [before, after]
            .into_iter()
            .flatten()
            .map(|path| self.classify(path));
        kinds
            .min_by_key(|kind| ORDER.iter().position(|k| k == kind))
            .unwrap_or(FileKind::Other)
    }

    pub fn classify(&self, path: &str) -> FileKind {
        if self.test.is_match(path) {
            FileKind::Test
        } else if self.gate.is_match(path) {
            FileKind::Gate
        } else if self.test_support.is_match(path) {
            FileKind::TestSupport
        } else if self.source.is_match(path) {
            FileKind::Source
        } else {
            FileKind::Other
        }
    }
}

impl Default for Classifier {
    fn default() -> Self {
        let (test, gate, test_support) = Self::default_patterns();
        Self::new(&test, &gate, &test_support).expect("built-in patterns are valid")
    }
}

impl Classifier {
    /// The built-in patterns: `(test, gate, test support)`.
    pub fn default_patterns() -> (Vec<String>, Vec<String>, Vec<String>) {
        let test = vec![
            format!("**/*.{{test,spec}}.{TEST_EXTENSIONS}"),
            format!("**/__tests__/**/*.{TEST_EXTENSIONS}"),
        ];
        let gate = vec![
            ".github/workflows/*.{yml,yaml}".to_owned(),
            "**/{vitest,jest,playwright}.config.*".to_owned(),
            "**/vitest.workspace.*".to_owned(),
            "**/package.json".to_owned(),
        ];
        let test_support = vec![format!(
            "**/{{test,tests,__tests__,__mocks__,e2e}}/**/*.{TEST_EXTENSIONS}"
        )];
        (test, gate, test_support)
    }
}

fn glob_set(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|err| format!("invalid pattern `{pattern}`: {err}"))?;
        builder.add(glob);
    }
    builder.build().map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn assert_kind(path: &str, kind: FileKind) {
        assert_eq!(Classifier::default().classify(path), kind, "{path}");
    }

    #[test]
    fn a_renamed_file_keeps_the_stronger_kind() {
        let classifier = Classifier::default();
        let kind = |before, after| classifier.classify_change(before, after);
        assert_eq!(
            kind(Some("test/a.test.ts"), Some("test/a.test.ts.disabled")),
            FileKind::Test
        );
        assert_eq!(
            kind(Some("src/helpers.ts"), Some("test/helpers.ts")),
            FileKind::TestSupport
        );
        assert_eq!(kind(None, Some("README.md")), FileKind::Other);
        assert_eq!(kind(Some("vitest.config.ts"), None), FileKind::Gate);
    }

    #[test]
    fn test_files() {
        assert_kind("test/api/forwarding.test.ts", FileKind::Test);
        assert_kind("src/email.spec.tsx", FileKind::Test);
        assert_kind("app.test.mjs", FileKind::Test);
        assert_kind("src/__tests__/parse.ts", FileKind::Test);
        assert_kind("e2e/dashboard.spec.ts", FileKind::Test);
    }

    #[test]
    fn gate_files() {
        assert_kind(".github/workflows/ci.yml", FileKind::Gate);
        assert_kind(".github/workflows/release.yaml", FileKind::Gate);
        assert_kind("vitest.config.ts", FileKind::Gate);
        assert_kind("packages/web/playwright.config.mjs", FileKind::Gate);
        assert_kind("jest.config.js", FileKind::Gate);
        assert_kind("package.json", FileKind::Gate);
        assert_kind("packages/api/package.json", FileKind::Gate);
    }

    #[test]
    fn test_support_files() {
        assert_kind("test/utils.ts", FileKind::TestSupport);
        assert_kind("tests/helpers/db.js", FileKind::TestSupport);
        assert_kind("src/__mocks__/fs.ts", FileKind::TestSupport);
        assert_kind("e2e/fixtures.ts", FileKind::TestSupport);
    }

    #[test]
    fn source_and_other_files() {
        assert_kind("server/api/forwarding/confirm.post.ts", FileKind::Source);
        assert_kind("app/components/Inbox.vue", FileKind::Source);
        assert_kind("README.md", FileKind::Other);
        assert_kind("test/__snapshots__/a.test.ts.snap", FileKind::Other);
        assert_kind("public/logo.png", FileKind::Other);
    }

    #[test]
    fn nested_workflow_dirs_are_not_gates() {
        // GitHub only runs workflows directly in .github/workflows/.
        assert_kind(".github/workflows/old/ci.yml", FileKind::Other);
    }

    #[test]
    fn rejects_an_invalid_pattern() {
        let err = Classifier::new(&["**/*.{ts".to_owned()], &[], &[]).unwrap_err();
        assert!(err.contains("invalid pattern"), "{err}");
    }
}
