use std::fs;
use std::io::{Read as _, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use crate::artifact::model::{InputTrace, LockFile};
use crate::artifact::validation::{validate_lock, validate_trace};
use crate::{OPERATION_SEQUENCE, PqtyError};

const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

/// Read and minimally validate a pqty lock from disk.
///
/// # Errors
///
/// Returns an error when the file cannot be read, decoded, or uses an
/// unsupported schema.
pub fn read_lock(path: &Path) -> Result<LockFile, PqtyError> {
    let text = read_text_bounded(path, MAX_ARTIFACT_BYTES)?;
    let lock: LockFile = serde_json::from_str(&text)?;
    validate_lock(&lock)?;
    Ok(lock)
}

/// Read and minimally validate an engine-neutral input trace.
///
/// # Errors
///
/// Returns an error when the file cannot be read, decoded, or uses an
/// unsupported schema.
pub fn read_trace(path: &Path) -> Result<InputTrace, PqtyError> {
    let text = read_text_bounded(path, MAX_ARTIFACT_BYTES)?;
    let trace: InputTrace = serde_json::from_str(&text)?;
    validate_trace(&trace)?;
    Ok(trace)
}

fn read_text_bounded(path: &Path, limit: u64) -> Result<String, PqtyError> {
    let file = fs::File::open(path).map_err(|source| PqtyError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| PqtyError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > limit {
        return Err(PqtyError::Usage(format!(
            "artifact {} exceeds the supported {} MiB limit",
            path.display(),
            limit / (1024 * 1024)
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| PqtyError::Usage(format!("artifact {} is not valid UTF-8", path.display())))
}

/// Atomically serialize a lock to disk.
///
/// # Errors
///
/// Returns an error when validation, serialization, or the atomic file
/// replacement fails.
pub fn write_lock(path: &Path, lock: &LockFile) -> Result<(), PqtyError> {
    validate_lock(lock)?;
    let mut bytes = serde_json::to_vec_pretty(lock)?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), PqtyError> {
    let parent = usable_parent(path);
    let temp = unique_sibling(path, "write")?;
    let result = (|| -> Result<(), PqtyError> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(|source| PqtyError::Io {
                path: temp.clone(),
                source,
            })?;
        file.write_all(bytes).map_err(|source| PqtyError::Io {
            path: temp.clone(),
            source,
        })?;
        file.sync_all().map_err(|source| PqtyError::Io {
            path: temp.clone(),
            source,
        })?;
        fs::rename(&temp, path).map_err(|source| PqtyError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub(crate) fn usable_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub(crate) fn unique_sibling(path: &Path, label: &str) -> Result<PathBuf, PqtyError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            PqtyError::Usage(format!(
                "path must name a concrete target: {}",
                path.display()
            ))
        })?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let sequence = OPERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(usable_parent(path).join(format!(
        ".{name}.pqty-{label}-{}.{}.{sequence}",
        std::process::id(),
        nonce
    )))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use crate::artifact::io::read_text_bounded;

    fn temporary_directory() -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("pqty-artifact-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).expect("temporary directory");
        path
    }

    #[test]
    fn bounded_artifact_reader_rejects_oversized_and_non_utf8_input() {
        let directory = temporary_directory();
        let path = directory.join("artifact.json");
        fs::write(&path, b"12345").expect("oversized artifact");
        let error = read_text_bounded(&path, 4).expect_err("size limit");
        assert!(error.to_string().contains("exceeds"));

        fs::write(&path, [0xff]).expect("non-UTF-8 artifact");
        let error = read_text_bounded(&path, 4).expect_err("UTF-8");
        assert!(error.to_string().contains("UTF-8"));
        fs::remove_dir_all(directory).expect("cleanup");
    }
}
