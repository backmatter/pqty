//! Verified per-file access without a TEXMF tree or portable bundle.
use super::{install::stored_file_digest, object::load_store_manifest, stream::copy_hashed};
use crate::{
    LockFile, MAX_ARCHIVE_ENTRY_BYTES, ManifestFile, PqtyError, unique_sibling, usable_parent,
    validate_materialized_lock, validate_tds_path,
};
use std::{
    fs::{self, File},
    io::{Seek, SeekFrom, Write},
    path::Path,
};

/// Copy one exact TDS path from a verified store to a caller-owned writer.
///
/// No network, registry, or other package bytes are needed. The selected provider
/// manifest and file are verified before the writer receives data. Copying uses
/// a 64 KiB buffer and verifies the copied bytes again. The caller must discard
/// partial output on any error and publish it only after success. Store objects
/// must remain immutable, as with all pqty store consumers.
///
/// # Errors
/// Returns an error for an unknown path, invalid lock, missing or corrupt file,
/// unsupported object type, or reader/writer failure.
pub fn copy_verified_file(
    lock: &LockFile,
    store: &Path,
    tds_path: &str,
    writer: &mut impl Write,
) -> Result<ManifestFile, PqtyError> {
    copy_file(lock, store, tds_path, writer, true)
}

fn copy_file(
    lock: &LockFile,
    store: &Path,
    tds_path: &str,
    writer: &mut impl Write,
    verify_before_copy: bool,
) -> Result<ManifestFile, PqtyError> {
    validate_materialized_lock(lock)?;
    validate_tds_path(tds_path, "exported TDS path")?;
    let (provider, locked) = lock
        .closure
        .iter()
        .find_map(|provider| {
            provider
                .files
                .iter()
                .find(|file| file.tds_path == tds_path)
                .map(|file| (provider, file))
        })
        .ok_or_else(|| {
            PqtyError::Usage(format!("file is absent from the exact lock: {tds_path}"))
        })?;
    let manifest = load_store_manifest(store, provider)?;
    let stored = manifest
        .files
        .iter()
        .find(|file| file.tds_path == tds_path)
        .ok_or_else(|| {
            PqtyError::Usage("provider manifest is missing the requested file".into())
        })?;
    let digest = stored_file_digest(&stored.digest)?;
    let path = store.join(&digest[..2]).join(digest);
    if !fs::symlink_metadata(&path)
        .map_err(|source| PqtyError::Io {
            path: path.clone(),
            source,
        })?
        .is_file()
    {
        return Err(PqtyError::Usage(
            "package store object is not a regular file".into(),
        ));
    }
    let mut file = File::open(&path).map_err(|source| PqtyError::Io {
        path: path.clone(),
        source,
    })?;
    if !file
        .metadata()
        .map_err(|source| PqtyError::Io {
            path: path.clone(),
            source,
        })?
        .is_file()
    {
        return Err(PqtyError::Usage(
            "package store object is not a regular file".into(),
        ));
    }
    let verified = if verify_before_copy {
        let (actual, size) = copy_hashed(
            &mut file,
            &mut std::io::sink(),
            MAX_ARCHIVE_ENTRY_BYTES,
            &path,
        )?;
        if actual != digest {
            return Err(PqtyError::Usage(
                "corrupt content-addressed store object".into(),
            ));
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|source| PqtyError::Io {
                path: path.clone(),
                source,
            })?;
        Some((actual, size))
    } else {
        None
    };
    let (actual, size) = copy_hashed(&mut file, writer, MAX_ARCHIVE_ENTRY_BYTES, &path)?;
    if actual != digest || verified.is_some_and(|expected| expected != (actual, size)) {
        return Err(PqtyError::Usage(
            "corrupt or changed content-addressed store object".into(),
        ));
    }
    Ok(ManifestFile {
        tds_path: tds_path.into(),
        owner: provider.provider.clone(),
        kind: locked.kind.clone(),
        digest: stored.digest.clone(),
        size,
    })
}

/// Atomically export one verified locked file, leaving existing output intact
/// on failure. No TEXMF tree is created and no network access is performed.
///
/// # Errors
/// Returns errors from [`copy_verified_file`] or output publication. Outputs
/// inside the package store are rejected to preserve store ownership.
pub fn export_file(
    lock: &LockFile,
    store: &Path,
    tds_path: &str,
    output: &Path,
) -> Result<ManifestFile, PqtyError> {
    let parent = usable_parent(output);
    let resolved_parent = fs::canonicalize(parent).map_err(|source| PqtyError::Io {
        path: parent.into(),
        source,
    })?;
    let resolved_store = fs::canonicalize(store).map_err(|source| PqtyError::Io {
        path: store.into(),
        source,
    })?;
    if resolved_parent.starts_with(&resolved_store) {
        return Err(PqtyError::Usage(
            "file export output must be outside the package store".into(),
        ));
    }
    let temporary = unique_sibling(output, "file")?;
    let result = (|| {
        let mut file = File::options()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| PqtyError::Io {
                path: temporary.clone(),
                source,
            })?;
        // Private output is published only after the streamed digest is checked;
        // it needs no preliminary content pass.
        let metadata = copy_file(lock, store, tds_path, &mut file, false)?;
        file.sync_all().map_err(|source| PqtyError::Io {
            path: temporary.clone(),
            source,
        })?;
        drop(file);
        fs::rename(&temporary, output).map_err(|source| PqtyError::Io {
            path: output.into(),
            source,
        })?;
        Ok(metadata)
    })();
    let _ = fs::remove_file(&temporary);
    result
}
