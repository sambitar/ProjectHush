//! Test-only helpers.
//!
//! One place, because the alternative — every test module building its own
//! directory under the system temp dir and never removing it — leaves a vault
//! behind on every run. A test vault holds an `identity.secret`, so those
//! leftovers are private keys sitting in a shared temp directory, and they
//! accumulate for as long as the machine lives.

use std::path::{Path, PathBuf};

/// A directory under the system temp dir that is deleted when it goes out of
/// scope, including when the test panics.
///
/// Derefs to the path it was created at, so it stands in for the `PathBuf` these
/// tests used to build by hand: pass `&dir` where a path is wanted.
pub(crate) struct ScratchDir {
    path: PathBuf,
    _guard: tempfile::TempDir,
}

impl ScratchDir {
    /// An empty directory whose name starts with `name`, unique per call.
    pub(crate) fn new(name: &str) -> Self {
        let guard = tempfile::Builder::new()
            .prefix(&format!("hush-{name}-"))
            .tempdir()
            .expect("a scratch directory under the system temp dir");
        let path = guard.path().to_path_buf();
        Self {
            path,
            _guard: guard,
        }
    }
}

impl std::ops::Deref for ScratchDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for ScratchDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}
