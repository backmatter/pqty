//! OS-owned installation locks and recoverable directory publication.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

use super::install::{ensure_replaceable_destination, remove_materialized_tree};
use crate::{PqtyError, usable_parent};

const OWNER: &[u8] = b"pqty.install-transaction/v1\n";

pub(super) struct FileGuard(fs::File);

impl FileGuard {
    pub fn acquire(path: &Path) -> Result<Self, PqtyError> {
        if let Ok(metadata) = path.symlink_metadata()
            && (!metadata.is_file() || metadata.file_type().is_symlink())
        {
            return Err(PqtyError::Usage(format!(
                "invalid pqty lock file: {}",
                path.display()
            )));
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| error(path, source))?;
        // The kernel sleeps while another process owns this lock, and releases
        // it if the owner exits. Never remove the file: waiters share its inode.
        file.lock().map_err(|source| error(path, source))?;
        Ok(Self(file))
    }

    pub fn store(store: &Path) -> Result<Self, PqtyError> {
        fs::create_dir_all(store).map_err(|source| error(store, source))?;
        Self::acquire(&store.join(".pqty-store.lock"))
    }
}

impl Drop for FileGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub(super) struct Installation {
    _guard: FileGuard,
    root: PathBuf,
    pub destination: PathBuf,
    pub stage: PathBuf,
    previous: PathBuf,
}

impl Installation {
    pub fn acquire(destination: &Path) -> Result<Self, PqtyError> {
        let parent = usable_parent(destination)
            .canonicalize()
            .map_err(|source| error(destination, source))?;
        let name = destination.file_name().ok_or_else(|| {
            PqtyError::Usage(format!(
                "TEXMF output must name a concrete directory: {}",
                destination.display()
            ))
        })?;
        let mut control_name = std::ffi::OsString::from(".");
        control_name.push(name);
        control_name.push(".pqty-install");
        let root = parent.join(control_name);
        match fs::create_dir(&root) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(error(&root, source)),
        }
        let metadata = root
            .symlink_metadata()
            .map_err(|source| error(&root, source))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(PqtyError::Usage(format!(
                "invalid pqty transaction directory: {}",
                root.display()
            )));
        }
        let guard = FileGuard::acquire(&root.join("lock"))?;
        let owner = root.join("owner");
        match fs::read(&owner) {
            Ok(bytes) if bytes == OWNER => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                // A process can exit between creating the directory and writing
                // its owner record. Only an otherwise empty directory is safe.
                for entry in fs::read_dir(&root).map_err(|source| error(&root, source))? {
                    let entry = entry.map_err(|source| error(&root, source))?;
                    if entry.file_name() != "lock" {
                        return Err(PqtyError::Usage(format!(
                            "transaction directory not owned by pqty: {}",
                            root.display()
                        )));
                    }
                }
                fs::write(&owner, OWNER).map_err(|source| error(&owner, source))?;
                fs::File::open(&owner)
                    .and_then(|file| file.sync_all())
                    .map_err(|source| error(&owner, source))?;
                sync_directory(&root)?;
            }
            Ok(_) => {
                return Err(PqtyError::Usage(format!(
                    "invalid pqty transaction owner: {}",
                    owner.display()
                )));
            }
            Err(source) => return Err(error(&owner, source)),
        }
        let installation = Self {
            _guard: guard,
            destination: parent.join(name),
            stage: root.join("stage"),
            previous: root.join("previous"),
            root,
        };
        installation.recover()?;
        Ok(installation)
    }

    fn recover(&self) -> Result<(), PqtyError> {
        ensure_replaceable_destination(&self.destination)?;
        if self
            .previous
            .try_exists()
            .map_err(|source| error(&self.previous, source))?
        {
            if self
                .destination
                .try_exists()
                .map_err(|source| error(&self.destination, source))?
            {
                // Cleanup may have been interrupted after removing the backup's
                // marker. A complete active destination wins over that debris.
                Self::remove(&self.previous)?;
            } else {
                ensure_replaceable_destination(&self.previous)?;
                fs::rename(&self.previous, &self.destination)
                    .map_err(|source| error(&self.destination, source))?;
                self.sync()?;
            }
        }
        // An interrupted assembly was never visible to consumers. It may not
        // have its completion marker yet, but lives in our owned control dir.
        if self.stage.symlink_metadata().is_ok() {
            Self::remove(&self.stage)?;
        }
        Ok(())
    }

    pub fn publish(&self) -> Result<(), PqtyError> {
        ensure_replaceable_destination(&self.destination)?;
        ensure_replaceable_destination(&self.stage)?;
        let existing = self
            .destination
            .try_exists()
            .map_err(|source| error(&self.destination, source))?;
        if existing {
            fs::rename(&self.destination, &self.previous)
                .map_err(|source| error(&self.destination, source))?;
            if let Err(error) = self.sync() {
                fs::rename(&self.previous, &self.destination).map_err(|source| {
                    crate::PqtyError::Io {
                        path: self.destination.clone(),
                        source,
                    }
                })?;
                return Err(error);
            }
        }
        if let Err(source) = fs::rename(&self.stage, &self.destination) {
            // Preserve the old tree even if restoration fails. The next lock
            // owner will retry recovery before doing any other work.
            if existing {
                fs::rename(&self.previous, &self.destination)
                    .map_err(|source| error(&self.destination, source))?;
                self.sync()?;
            }
            return Err(error(&self.destination, source));
        }
        self.sync()?;
        if existing {
            Self::remove(&self.previous)?;
        }
        Ok(())
    }

    fn remove(path: &Path) -> Result<(), PqtyError> {
        let metadata = path
            .symlink_metadata()
            .map_err(|source| error(path, source))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(PqtyError::Usage(format!(
                "invalid pqty transaction tree: {}",
                path.display()
            )));
        }
        remove_materialized_tree(path).map_err(|source| error(path, source))
    }

    fn sync(&self) -> Result<(), PqtyError> {
        sync_directory(&self.root)?;
        sync_directory(usable_parent(&self.destination))
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), PqtyError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| error(path, source))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // Match the fallible Unix directory-sync interface.
fn sync_directory(_path: &Path) -> Result<(), PqtyError> {
    Ok(())
}

fn error(path: &Path, source: io::Error) -> PqtyError {
    PqtyError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_lock_waits_and_is_reusable_after_owner_exits() {
        let root = crate::tests::temporary_test_root("store-lock");
        let held = FileGuard::store(&root).unwrap();
        let path = root.join(".pqty-store.lock");
        let contender = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(matches!(
            contender.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        let (ready, waiting) = std::sync::mpsc::channel();
        let next = root.clone();
        let worker = std::thread::spawn(move || {
            let _guard = FileGuard::store(&next).unwrap();
            ready.send(()).unwrap();
        });
        assert!(
            waiting
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err()
        );
        drop(held);
        waiting
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        worker.join().unwrap();
        assert!(path.is_file(), "the lock inode survives its owner");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn destination_lock_prevents_competing_installers() {
        let root = crate::tests::temporary_test_root("target-lock");
        fs::create_dir_all(&root).unwrap();
        let held = Installation::acquire(&root.join("texmf")).unwrap();
        let lock = root.join(".texmf.pqty-install/lock");
        let contender = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock)
            .unwrap();
        assert!(matches!(
            contender.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        drop(held);
        assert!(contender.try_lock().is_ok());
        contender.unlock().unwrap();
        drop(contender);
        fs::remove_dir_all(root).unwrap();
    }
}
