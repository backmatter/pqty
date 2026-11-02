//! Scoped staging in the destination filesystem, with explicit publication.
use crate::{PqtyError, usable_parent};
use std::{fs, path::Path};

fn prefix(path: &Path, label: &str) -> Result<String, PqtyError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            PqtyError::Usage(format!(
                "path must name a concrete target: {}",
                path.display()
            ))
        })?;
    Ok(format!(".{name}.pqty-{label}-"))
}

pub(crate) fn temporary_file(
    path: &Path,
    label: &str,
) -> Result<tempfile::NamedTempFile, PqtyError> {
    // Retain ordinary OpenOptions permissions/umask, including readable shared
    // store objects. tempfile owns naming, collision handling, and cleanup.
    tempfile::Builder::new()
        .prefix(&prefix(path, label)?)
        .make_in(usable_parent(path), |path| {
            fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(path)
        })
        .map_err(|source| PqtyError::Io {
            path: path.to_owned(),
            source,
        })
}

pub(crate) fn temporary_directory(
    path: &Path,
    label: &str,
) -> Result<tempfile::TempDir, PqtyError> {
    tempfile::Builder::new()
        .prefix(&prefix(path, label)?)
        .tempdir_in(usable_parent(path))
        .map_err(|source| PqtyError::Io {
            path: path.to_owned(),
            source,
        })
}

pub(crate) fn publish_file(file: tempfile::NamedTempFile, output: &Path) -> Result<(), PqtyError> {
    file.as_file().sync_all().map_err(|source| PqtyError::Io {
        path: file.path().to_owned(),
        source,
    })?;
    file.persist(output).map_err(|error| PqtyError::Io {
        path: output.to_owned(),
        source: error.error,
    })?;
    // Match artifact publication's existing best-effort directory durability.
    if let Ok(directory) = fs::File::open(usable_parent(output)) {
        let _ = directory.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn scoped_files_and_directories_cleanup_on_errors_and_unwinding() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("artifact");
        let result = std::panic::catch_unwind(|| {
            let mut file = temporary_file(&target, "test").unwrap();
            file.write_all(b"private bytes").unwrap();
            let directory = temporary_directory(&target, "test").unwrap();
            fs::write(directory.path().join("nested"), b"staged").unwrap();
            panic!("test scope unwinding");
        });
        assert!(result.is_err());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        // A failed atomic publication also drops the file carried by PersistError.
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep"), b"original").unwrap();
        let mut file = temporary_file(&target, "test").unwrap();
        file.write_all(b"new bytes").unwrap();
        assert!(publish_file(file, &target).is_err());
        assert_eq!(fs::read(target.join("keep")).unwrap(), b"original");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn publication_replaces_only_the_destination_and_keeps_ordinary_permissions() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("artifact");
        fs::write(&target, b"previous").unwrap();
        let mut file = temporary_file(&target, "test").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let expected = fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(
                file.as_file().metadata().unwrap().permissions().mode(),
                expected
            );
        }
        file.write_all(b"verified replacement").unwrap();
        publish_file(file, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"verified replacement");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
