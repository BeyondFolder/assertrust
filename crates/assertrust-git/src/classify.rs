//! Labelling changed files by kind (`docs/architecture.md`, step 1, "Which files").
//!
//! Checked in this order, first match wins:
//! 1. **Test:** `*.test.*` / `*.spec.*` code files, and code files under `__tests__/`
//! 2. **Gate:** GitHub workflows, vitest/jest/playwright config, `package.json`
//! 3. **Test support:** other code files under `test/`, `tests/`, `__tests__/`, `__mocks__/`
//!    or `e2e/`. Files imported by tests from elsewhere are added when helpers are linked
//!    (step 2).
//! 4. **Source:** other code files
//! 5. **Other:** everything else

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::model::FileKind;

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
        let test = [
            format!("**/*.{{test,spec}}.{TEST_EXTENSIONS}"),
            format!("**/__tests__/**/*.{TEST_EXTENSIONS}"),
        ];
        let gate = [
            ".github/workflows/*.{yml,yaml}".to_owned(),
            "**/{vitest,jest,playwright}.config.*".to_owned(),
            "**/vitest.workspace.*".to_owned(),
            "**/package.json".to_owned(),
        ];
        let test_support = [format!(
            "**/{{test,tests,__tests__,__mocks__,e2e}}/**/*.{TEST_EXTENSIONS}"
        )];
        Self::new(&test, &gate, &test_support).expect("built-in patterns are valid")
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
