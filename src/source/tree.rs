use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::source::resolve::{canonical_or_original, normalize_project_path};
use crate::{PqtyError, SourceTree, VirtualPath};

/// A [`SourceTree`] backed by a confined directory on the local filesystem.
///
/// Symlinks are canonicalized before reads and ignored when they resolve
/// outside the project root.
#[derive(Debug, Clone)]
pub struct FileSystemSourceTree {
    root: PathBuf,
    search_roots: Vec<PathBuf>,
}

impl FileSystemSourceTree {
    /// Open a filesystem-backed source tree rooted at a project directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the project root does not exist or cannot be
    /// canonicalized.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, PqtyError> {
        let root = canonical_or_original(&root.into());
        if !root.is_dir() {
            return Err(PqtyError::Usage(format!(
                "source tree root is missing: {}",
                root.display()
            )));
        }
        Ok(Self {
            root,
            search_roots: Vec::new(),
        })
    }

    /// Open a source tree with additional confined, recursively searched
    /// project-owned input roots.
    ///
    /// # Errors
    ///
    /// Returns an error when the project root or any input root is absent,
    /// unreadable, or outside the project.
    pub fn with_search_roots(
        root: impl Into<PathBuf>,
        search_roots: &[PathBuf],
    ) -> Result<Self, PqtyError> {
        let mut source = Self::new(root)?;
        let mut roots = Vec::new();
        let mut seen = BTreeSet::new();
        for requested in search_roots {
            let relative = normalize_project_path(requested).ok_or_else(|| {
                PqtyError::Usage(format!(
                    "input root is not a confined project path: {}",
                    requested.display()
                ))
            })?;
            let absolute = source.root.join(relative.as_str());
            let canonical = absolute.canonicalize().map_err(|error| PqtyError::Io {
                path: absolute.clone(),
                source: error,
            })?;
            if !canonical.starts_with(&source.root) || !canonical.is_dir() {
                return Err(PqtyError::Usage(format!(
                    "input root is not a project directory: {}",
                    requested.display()
                )));
            }
            if seen.insert(canonical.clone()) {
                roots.push(canonical);
            }
        }
        source.search_roots = roots;
        Ok(source)
    }

    #[must_use]
    /// Return the canonical project root used to confine source reads.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, path: &VirtualPath) -> Option<PathBuf> {
        let candidate = self.root.join(path.as_str());
        let canonical = candidate.canonicalize().ok()?;
        canonical.starts_with(&self.root).then_some(canonical)
    }
}

impl SourceTree for FileSystemSourceTree {
    fn read(&self, path: &VirtualPath) -> io::Result<Option<Vec<u8>>> {
        let Some(path) = self.resolve(path) else {
            return Ok(None);
        };
        if !path.is_file() {
            return Ok(None);
        }
        fs::read(path).map(Some)
    }

    fn search(&self, path: &VirtualPath) -> io::Result<Option<(VirtualPath, Vec<u8>)>> {
        for root in &self.search_roots {
            let direct = root.join(path.as_str());
            if let Some(found) = self.read_search_candidate(&direct)? {
                return Ok(Some(found));
            }
            let mut pending = vec![root.clone()];
            let mut matches = Vec::new();
            while let Some(directory) = pending.pop() {
                let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
                entries.sort_by_key(fs::DirEntry::file_name);
                for entry in entries {
                    let file_type = entry.file_type()?;
                    let candidate = entry.path();
                    if file_type.is_dir() {
                        pending.push(candidate);
                    } else if file_type.is_file()
                        && candidate
                            .strip_prefix(root)
                            .is_ok_and(|relative| relative.ends_with(path.as_str()))
                    {
                        matches.push(candidate);
                    }
                }
            }
            matches.sort();
            for candidate in matches {
                if let Some(found) = self.read_search_candidate(&candidate)? {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }
}

impl FileSystemSourceTree {
    fn read_search_candidate(
        &self,
        candidate: &Path,
    ) -> io::Result<Option<(VirtualPath, Vec<u8>)>> {
        let canonical = match candidate.canonicalize() {
            Ok(path) if path.starts_with(&self.root) && path.is_file() => path,
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let Some(relative) = canonical
            .strip_prefix(&self.root)
            .ok()
            .and_then(normalize_project_path)
        else {
            return Ok(None);
        };
        fs::read(canonical).map(|bytes| Some((relative, bytes)))
    }
}

/// An in-memory [`SourceTree`] for editor snapshots, archives, and tests.
#[derive(Debug, Clone, Default)]
pub struct MemorySourceTree {
    files: BTreeMap<VirtualPath, Vec<u8>>,
}

impl MemorySourceTree {
    #[must_use]
    /// Create a source tree from normalized virtual paths and their bytes.
    pub fn new(files: BTreeMap<VirtualPath, Vec<u8>>) -> Self {
        Self { files }
    }

    /// Insert or replace one virtual source file.
    pub fn insert(&mut self, path: VirtualPath, bytes: impl Into<Vec<u8>>) {
        self.files.insert(path, bytes.into());
    }
}

impl SourceTree for MemorySourceTree {
    fn read(&self, path: &VirtualPath) -> io::Result<Option<Vec<u8>>> {
        Ok(self.files.get(path).cloned())
    }
}
