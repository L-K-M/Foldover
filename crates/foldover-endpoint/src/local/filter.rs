//! Exclusion rules, gitignore-style. The same filter applies to both
//! folders of a pair, so an excluded entry is absent on both sides and is
//! forgotten rather than deleted.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use foldover_types::SyncPath;
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::{LocalEndpoint, ScanError, at};

/// A root's own rules, honored on both sides.
const IGNORE_FILE: &[u8] = b".foldoverignore";
const IGNORE_FILE_LIMIT: u64 = 64 * 1024;

pub struct Filter {
    rules: Gitignore,
    size_limit: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
#[error("Filter rule {rule:?} is not valid: {message}")]
pub struct InvalidFilter {
    rule: String,
    message: String,
}

impl Filter {
    pub fn none() -> Self {
        Self {
            rules: Gitignore::empty(),
            size_limit: None,
        }
    }

    /// Later rules override earlier ones; `!` re-includes, as in gitignore.
    pub fn new<'a>(rules: impl IntoIterator<Item = &'a str>) -> Result<Self, InvalidFilter> {
        let mut builder = GitignoreBuilder::new("");
        for rule in rules {
            builder
                .add_line(None, rule)
                .map_err(|error| InvalidFilter {
                    rule: rule.to_owned(),
                    message: error.to_string(),
                })?;
        }
        let rules = builder.build().map_err(|error| InvalidFilter {
            rule: String::new(),
            message: error.to_string(),
        })?;
        Ok(Self {
            rules,
            size_limit: None,
        })
    }

    /// Also skip files larger than `bytes`. Sizes differ between folders,
    /// so callers must skip a path skipped in either folder in both.
    pub fn with_size_limit(self, bytes: Option<u64>) -> Self {
        Self {
            size_limit: bytes,
            ..self
        }
    }

    pub(super) fn too_large(&self, size: u64) -> bool {
        self.size_limit.is_some_and(|limit| size > limit)
    }

    pub(super) fn excludes(&self, path: &SyncPath, directory: bool) -> bool {
        self.rules
            .matched(Path::new(OsStr::from_bytes(path.as_bytes())), directory)
            .is_ignore()
    }
}

impl LocalEndpoint {
    /// The rules in the root's `.foldoverignore`, one per line; none if absent.
    pub fn ignore_rules(&self) -> Result<Vec<String>, ScanError> {
        let path = SyncPath::try_from(IGNORE_FILE).expect("the ignore file path is valid");
        let Some(content) = at(
            &Some(path.clone()),
            self.root.read_private_file(&path, IGNORE_FILE_LIMIT),
        )?
        else {
            return Ok(Vec::new());
        };
        let text = String::from_utf8(content).map_err(|_| ScanError::InvalidIgnoreFile)?;
        Ok(text.lines().map(str::to_owned).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(bytes: &[u8]) -> SyncPath {
        SyncPath::try_from(bytes).unwrap()
    }

    #[test]
    fn rules_follow_gitignore_semantics() {
        let filter =
            Filter::new(["*.tmp", "build/", "/top", "!keep.tmp", "# comment", ""]).unwrap();
        assert!(filter.excludes(&path(b"a.tmp"), false));
        assert!(filter.excludes(&path(b"nested/a.tmp"), false));
        assert!(!filter.excludes(&path(b"keep.tmp"), false));
        assert!(filter.excludes(&path(b"build"), true));
        assert!(!filter.excludes(&path(b"build"), false));
        assert!(filter.excludes(&path(b"top"), false));
        assert!(!filter.excludes(&path(b"nested/top"), false));
        assert!(!filter.excludes(&path(b"a.txt"), false));
        assert!(!Filter::none().excludes(&path(b"a.tmp"), false));
        assert!(Filter::new(["a{"]).is_err());
    }

    /// The app skips one path with an anchored rule that escapes
    /// pattern characters and trailing spaces.
    #[test]
    fn escaped_anchored_rules_match_exactly_one_path() {
        let filter = Filter::new([r"/dir/odd \*\[x\]\{y\}\?.txt\ "]).unwrap();
        assert!(filter.excludes(&path(b"dir/odd *[x]{y}?.txt "), false));
        assert!(!filter.excludes(&path(b"dir/odd a[x]{y}b.txt "), false));
        assert!(!filter.excludes(&path(b"dir/odd *[x]{y}?.txt"), false));
        assert!(!filter.excludes(&path(b"other/dir/odd *[x]{y}?.txt "), false));
    }

    #[test]
    fn ignore_files_are_read_as_lines_and_must_be_text() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        assert!(endpoint.ignore_rules().unwrap().is_empty());
        std::fs::write(directory.path().join(".foldoverignore"), b"*.tmp\nbuild/\n").unwrap();
        assert_eq!(endpoint.ignore_rules().unwrap(), ["*.tmp", "build/"]);
        std::fs::write(directory.path().join(".foldoverignore"), b"\xff").unwrap();
        assert!(matches!(
            endpoint.ignore_rules(),
            Err(ScanError::InvalidIgnoreFile)
        ));
    }
}
