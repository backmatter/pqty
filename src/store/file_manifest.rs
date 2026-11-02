//! Public, offline projection of verified store bytes for lazy file consumers.
use std::{collections::BTreeMap, fs::File, io::Write, path::Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    install::stored_file_digest,
    object::{StoredFile, load_store_manifest, package_digest, provider_manifest_digest},
};
use crate::{
    LockFile, MAX_ARCHIVE_ENTRY_BYTES, MAX_CLOSURE_EXPANDED_BYTES, MAX_CLOSURE_RUNFILES, PqtyError,
    ResolvedEnvironment, ResourceKind,
};

/// Schema identifying a verified public package-file index.
pub const FILE_MANIFEST_SCHEMA: &str = "pqty.files/v1";

/// One immutable package file, independent of store layout and transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestFile {
    /// Exact normalized path relative to the package TEXMF root.
    pub tds_path: String,
    /// Locked provider owning this path.
    pub owner: String,
    /// Engine-neutral file classification from the lock.
    pub kind: ResourceKind,
    /// Content digest in lowercase `sha256:<hex>` form.
    pub digest: String,
    /// Verified uncompressed byte count.
    pub size: u64,
}

/// Snapshot of package file identities for lazy loading and content caches.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileManifest {
    /// Artifact schema, currently [`FILE_MANIFEST_SCHEMA`].
    pub schema: String,
    /// SHA-256 of compact JSON `[schema, environment_fingerprint, files]`.
    pub fingerprint: String,
    /// Fingerprint of the exact package environment represented by this index.
    pub environment_fingerprint: String,
    /// Every locked file, sorted by normalized TDS path.
    pub files: Vec<ManifestFile>,
}

impl FileManifest {
    /// Check schema, completeness, ownership, kinds, digests, and fingerprint
    /// against an exact lock, without reading package bytes or using a registry.
    ///
    /// Size bounds are checked here. Actual sizes and content digests must also
    /// be checked when a consumer receives bytes from its chosen transport.
    ///
    /// # Errors
    /// Returns an error for incomplete, altered, or incompatible artifacts.
    pub fn validate(&self, lock: &LockFile) -> Result<(), PqtyError> {
        if self.schema != FILE_MANIFEST_SCHEMA || self.files.len() > MAX_CLOSURE_RUNFILES {
            return Err(invalid("unsupported or oversized package-file manifest"));
        }
        let environment = ResolvedEnvironment::from_lock(lock)?;
        if self.environment_fingerprint != environment.fingerprint
            || self.files.len() != environment.files.len()
        {
            return Err(invalid(
                "package-file manifest differs from its locked environment",
            ));
        }
        let mut providers = BTreeMap::<&str, Vec<StoredFile>>::new();
        let mut total = 0;
        for (file, expected) in self.files.iter().zip(&environment.files) {
            if file.tds_path != expected.tds_path
                || file.owner != expected.owner
                || file.kind != expected.kind
            {
                return Err(invalid(
                    "package-file manifest path, ownership, kind, or order differs from its lock",
                ));
            }
            stored_file_digest(&file.digest)?;
            account_size(&mut total, file.size)?;
            providers.entry(&file.owner).or_default().push(StoredFile {
                tds_path: file.tds_path.clone(),
                digest: file.digest.clone(),
            });
        }
        for provider in &lock.closure {
            let files = providers
                .remove(provider.provider.as_str())
                .unwrap_or_default();
            if hex::encode(provider_manifest_digest(&files)?) != package_digest(provider)? {
                return Err(invalid(
                    "package-file manifest digests differ from locked provider integrity",
                ));
            }
        }
        if self.fingerprint != self.compute_fingerprint()? {
            return Err(invalid("package-file manifest fingerprint mismatch"));
        }
        Ok(())
    }

    fn compute_fingerprint(&self) -> Result<String, PqtyError> {
        let mut writer = HashWriter(Sha256::new());
        serde_json::to_writer(
            &mut writer,
            &(&self.schema, &self.environment_fingerprint, &self.files),
        )?;
        Ok(format!("sha256:{}", hex::encode(writer.0.finalize())))
    }
}

struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Export all file hashes and sizes from an exact lock and verified store.
///
/// This operation is read-only and offline. Shared content objects are hashed
/// once per export with a bounded buffer. It does not expose private paths,
/// construct URLs, fetch packages, or materialize a second TEXMF tree.
///
/// # Errors
/// Returns an error for an invalid lock, missing or corrupt store content,
/// unsupported sizes, or inconsistent provider metadata.
pub fn export_file_manifest(lock: &LockFile, store: &Path) -> Result<FileManifest, PqtyError> {
    let environment = ResolvedEnvironment::from_lock(lock)?;
    if environment.files.len() > MAX_CLOSURE_RUNFILES {
        return Err(invalid(
            "package-file manifest exceeds the file count limit",
        ));
    }
    let mut objects = BTreeMap::new();
    let mut files = BTreeMap::new();
    let mut total = 0;
    for provider in &lock.closure {
        let manifest = load_store_manifest(store, provider)?;
        for file in manifest.files {
            let digest = stored_file_digest(&file.digest)?;
            let size = if let Some(size) = objects.get(digest) {
                *size
            } else {
                let size = verify_object(&store.join(&digest[..2]).join(digest), digest)?;
                objects.insert(digest.to_string(), size);
                size
            };
            account_size(&mut total, size)?;
            files.insert(file.tds_path, (file.digest, size));
        }
    }
    let mut manifest = FileManifest {
        schema: FILE_MANIFEST_SCHEMA.to_string(),
        fingerprint: String::new(),
        environment_fingerprint: environment.fingerprint,
        files: environment
            .files
            .into_iter()
            .map(|file| {
                let (digest, size) = files
                    .remove(&file.tds_path)
                    .ok_or_else(|| invalid("verified store is missing a locked file"))?;
                Ok(ManifestFile {
                    tds_path: file.tds_path,
                    owner: file.owner,
                    kind: file.kind,
                    digest,
                    size,
                })
            })
            .collect::<Result<_, PqtyError>>()?,
    };
    manifest.fingerprint = manifest.compute_fingerprint()?;
    Ok(manifest)
}

fn verify_object(path: &Path, expected: &str) -> Result<u64, PqtyError> {
    if !std::fs::symlink_metadata(path)
        .map_err(|source| PqtyError::Io {
            path: path.to_owned(),
            source,
        })?
        .is_file()
    {
        return Err(invalid("package store object is not a regular file"));
    }
    let mut file = File::open(path).map_err(|source| PqtyError::Io {
        path: path.to_owned(),
        source,
    })?;
    if !file
        .metadata()
        .map_err(|source| PqtyError::Io {
            path: path.to_owned(),
            source,
        })?
        .is_file()
    {
        return Err(invalid("package store object is not a regular file"));
    }
    let (actual, total) = super::stream::copy_hashed(
        &mut file,
        &mut std::io::sink(),
        MAX_ARCHIVE_ENTRY_BYTES,
        path,
    )?;
    if actual != expected {
        return Err(invalid(format!(
            "corrupt content-addressed store object {}",
            path.display()
        )));
    }
    Ok(total)
}

fn account_size(total: &mut u64, size: u64) -> Result<(), PqtyError> {
    if size > MAX_ARCHIVE_ENTRY_BYTES || size > MAX_CLOSURE_EXPANDED_BYTES.saturating_sub(*total) {
        return Err(invalid(
            "package-file manifest exceeds supported byte limits",
        ));
    }
    *total += size;
    Ok(())
}

fn invalid(message: impl Into<String>) -> PqtyError {
    PqtyError::Usage(message.into())
}
