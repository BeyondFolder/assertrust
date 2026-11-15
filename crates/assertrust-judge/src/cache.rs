//! Verdicts kept on disk, so the same change always gets the same verdict and CI never
//! flips: models don't answer the same way twice, and the current Claude models reject the
//! sampling settings that would make them.
//!
//! One JSON file per verdict, named by its prompt's key ([`Prompt::key`]):
//!
//! ```text
//! .assertrust/cache/judge/3f9a…e1.json   {"reason": "…", "verdict": "fine"}
//! .assertrust/cache/judge/.gitignore     *
//! ```
//!
//! [`Prompt::key`]: crate::Prompt::key

use std::fs;
use std::io;
use std::path::PathBuf;

use serde_json::Value;

use crate::{Judgment, Verdict};

pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The verdict kept under `key`, if any. An unreadable one counts as none.
    pub fn get(&self, key: &str) -> Option<Judgment> {
        let text = fs::read_to_string(self.path(key)).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        Some(Judgment {
            verdict: Verdict::parse(value["verdict"].as_str()?)?,
            reason: value["reason"].as_str()?.to_owned(),
        })
    }

    /// Keeps `judgment` under `key`. The directory gets a `.gitignore`, so git never shows
    /// it.
    pub fn put(&self, key: &str, judgment: &Judgment) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let ignore = self.dir.join(".gitignore");
        if !ignore.exists() {
            fs::write(ignore, "*\n")?;
        }
        // Written aside and renamed, so a reader never sees half a file.
        let path = self.path(key);
        let partial = path.with_extension("partial");
        fs::write(&partial, format!("{}\n", judgment.to_json()))?;
        fs::rename(partial, path)
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_verdicts() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().join("judge"));
        assert_eq!(cache.get("abc"), None);
        let judgment = Judgment {
            verdict: Verdict::Suspicious,
            reason: "changed to match".to_owned(),
        };
        cache.put("abc", &judgment).unwrap();
        assert_eq!(cache.get("abc"), Some(judgment));
        assert_eq!(
            fs::read_to_string(dir.path().join("judge/.gitignore")).unwrap(),
            "*\n"
        );
        // A broken file is as good as none.
        fs::write(dir.path().join("judge/def.json"), "{").unwrap();
        assert_eq!(cache.get("def"), None);
    }
}
