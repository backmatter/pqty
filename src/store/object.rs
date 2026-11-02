use crate::store::container::container_cache_path;
use crate::store::http::read_file_bounded;
use crate::store::{MaterializeReport, PackageByteSource, visit_provider_files};

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    LockFile, MAX_CLOSURE_EXPANDED_BYTES, MAX_CLOSURE_RUNFILES, MAX_CONTAINER_EXPANDED_BYTES,
    MAX_PROVIDER_RUNFILES, PackageRegistry, PqtyError, ResolvedPackage, progress, resource_kind,
    temporary_file, unique_sibling,
};

/// Fetch and verify one provider's runfiles into the content-addressable store.
pub(super) struct ProviderMaterialization {
    pub(super) integrity: String,
    pub(super) store_key: String,
    pub(super) files: Vec<PreparedFile>,
    pub(super) expanded_bytes: u64,
}

// Metadata produced while store_reader verifies the bytes. Keeping it here
// avoids rereading content just to project another public package artifact.
pub(super) struct PreparedFile {
    pub(super) tds_path: String,
    pub(super) kind: crate::ResourceKind,
    pub(super) digest: String,
    pub(super) size: u64,
}

pub(super) const STORE_PACKAGE_SCHEMA: &str = "pqty.store-package/v1";

/// The per-file integrity index belongs to the shared store, not every
/// project's lock. The lock pins this whole manifest with one package digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StoredPackageManifest {
    pub(super) schema: String,
    pub(super) files: Vec<StoredFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StoredFile {
    pub(super) tds_path: String,
    pub(super) digest: String,
}

pub(super) fn provider_manifest_digest(files: &[StoredFile]) -> Result<[u8; 32], PqtyError> {
    let mut manifest = files
        .iter()
        .map(|file| {
            let digest = file.digest.strip_prefix("sha256:").ok_or_else(|| {
                PqtyError::Usage(format!("unsupported stored file digest: {}", file.digest))
            })?;
            Ok((file.tds_path.as_str(), digest))
        })
        .collect::<Result<Vec<_>, PqtyError>>()?;
    manifest.sort_unstable();

    let mut hasher = Sha256::new();
    for (path, digest) in manifest {
        hasher.update(path.as_bytes());
        hasher.update(b":");
        hasher.update(digest.as_bytes());
        hasher.update(b"\n");
    }
    Ok(hasher.finalize().into())
}

pub(super) fn materialize_provider(
    index: &impl PackageRegistry,
    provider: &str,
    container_checksum: Option<&str>,
    container_size: Option<u64>,
    source: &PackageByteSource,
    store_dir: &Path,
    report: &mut MaterializeReport,
) -> Result<ProviderMaterialization, PqtyError> {
    let runfiles = index
        .package(provider)
        .map(|meta| meta.runfiles.clone())
        .unwrap_or_default();
    let mut locked_files = Vec::new();
    let mut stored_files = Vec::new();
    let mut expanded_bytes = 0_u64;
    visit_provider_files(
        source,
        provider,
        container_checksum,
        container_size,
        &runfiles,
        &mut |relative, reader| {
            let object = store_reader(store_dir, reader, None)?;
            expanded_bytes += object.size;
            if expanded_bytes > MAX_CONTAINER_EXPANDED_BYTES {
                return Err(PqtyError::Usage(format!(
                    "provider {provider} exceeds the {MAX_CONTAINER_EXPANDED_BYTES}-byte expanded-content limit"
                )));
            }
            if object.created {
                report.bytes_stored += object.size;
            }
            let tds_path = relative
                .strip_prefix("texmf-dist/")
                .unwrap_or(relative)
                .to_owned();
            locked_files.push(PreparedFile {
                kind: resource_kind(&tds_path),
                tds_path: tds_path.clone(),
                digest: format!("sha256:{}", object.digest),
                size: object.size,
            });
            stored_files.push(StoredFile {
                tds_path,
                digest: format!("sha256:{}", object.digest),
            });
            report.files += 1;
            Ok(())
        },
    )?;
    locked_files.sort_by(|left, right| left.tds_path.cmp(&right.tds_path));
    stored_files.sort_by(|left, right| left.tds_path.cmp(&right.tds_path));
    // Provider integrity covers the sorted path/file-hash manifest.
    let provider_digest = provider_manifest_digest(&stored_files)?;
    let provider_hex = hex::encode(provider_digest);
    let integrity = format!("sha256-{}", BASE64.encode(provider_digest));
    let store_key = format!("sha256:{provider_hex}");
    write_store_manifest(
        store_dir,
        &provider_hex,
        &StoredPackageManifest {
            schema: STORE_PACKAGE_SCHEMA.to_string(),
            files: stored_files,
        },
    )?;
    report.providers += 1;
    Ok(ProviderMaterialization {
        integrity,
        store_key,
        files: locked_files,
        expanded_bytes,
    })
}

pub(super) fn validate_closure_resource_limits(
    lock: &LockFile,
    index: &impl PackageRegistry,
) -> Result<(), PqtyError> {
    let mut runfiles = 0_usize;
    let mut compressed_bytes = 0_u64;
    for entry in &lock.closure {
        let Some(package) = index.package(&entry.provider) else {
            continue;
        };
        if package.runfiles.len() > MAX_PROVIDER_RUNFILES {
            return Err(PqtyError::Usage(format!(
                "provider {} exceeds the {MAX_PROVIDER_RUNFILES}-runfile limit",
                entry.provider
            )));
        }
        runfiles = runfiles
            .checked_add(package.runfiles.len())
            .ok_or_else(|| PqtyError::Usage("closure runfile count overflow".to_string()))?;
        if runfiles > MAX_CLOSURE_RUNFILES {
            return Err(PqtyError::Usage(format!(
                "provider closure exceeds the {MAX_CLOSURE_RUNFILES}-runfile limit"
            )));
        }
        compressed_bytes = compressed_bytes
            .checked_add(package.container_size.unwrap_or_default())
            .ok_or_else(|| PqtyError::Usage("closure container size overflow".to_string()))?;
        if compressed_bytes > MAX_CLOSURE_EXPANDED_BYTES {
            return Err(PqtyError::Usage(format!(
                "provider closure declares more than {MAX_CLOSURE_EXPANDED_BYTES} bytes of containers"
            )));
        }
    }
    Ok(())
}

pub(super) fn emit_hydration_download_plan(
    source: &PackageByteSource,
    entries: &[&ResolvedPackage],
) -> Result<(), PqtyError> {
    let PackageByteSource::Remote { cache_dir, .. } = source else {
        return Ok(());
    };
    let mut bytes_total = 0_u64;
    let mut bytes_cached = 0_u64;
    let mut items_cached = 0_usize;
    for entry in entries {
        let checksum = entry.source.container_checksum.as_deref().ok_or_else(|| {
            PqtyError::Usage(format!(
                "remote container {} has no locked SHA-512 checksum and size",
                entry.provider
            ))
        })?;
        let size = entry.source.container_size.ok_or_else(|| {
            PqtyError::Usage(format!(
                "remote container {} has no locked SHA-512 checksum and size",
                entry.provider
            ))
        })?;
        bytes_total = bytes_total
            .checked_add(size)
            .ok_or_else(|| PqtyError::Usage("download plan byte count overflow".to_string()))?;
        if container_cache_path(cache_dir, &entry.source.locator, checksum).is_file() {
            items_cached += 1;
            bytes_cached = bytes_cached
                .checked_add(size)
                .ok_or_else(|| PqtyError::Usage("download plan byte count overflow".to_string()))?;
        }
    }
    if !entries.is_empty() {
        progress::download_plan(
            progress::DownloadCategory::Packages,
            entries.len(),
            items_cached,
            Some(bytes_total),
            Some(bytes_cached),
        );
    }
    Ok(())
}

pub(super) fn write_store_manifest(
    store_dir: &Path,
    package_digest: &str,
    manifest: &StoredPackageManifest,
) -> Result<(), PqtyError> {
    let path = store_manifest_path(store_dir, package_digest);
    let expected = provider_manifest_digest(&manifest.files)?;
    if path.is_file() {
        return verify_manifest_winner(&path, expected);
    }
    let parent = path
        .parent()
        .ok_or_else(|| PqtyError::Usage("store manifest has no parent directory".to_string()))?;
    fs::create_dir_all(parent).map_err(|source| PqtyError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let mut bytes = serde_json::to_vec(manifest)?;
    bytes.push(b'\n');
    let mut file = temporary_file(&path, "manifest")?;
    file.write_all(&bytes).map_err(|source| PqtyError::Io {
        path: file.path().to_owned(),
        source,
    })?;
    file.as_file().sync_all().map_err(|source| PqtyError::Io {
        path: file.path().to_owned(),
        source,
    })?;
    if publish_if_absent(file, &path)? {
        make_read_only(&path)
    } else {
        verify_manifest_winner(&path, expected)
    }
}

fn verify_manifest_winner(path: &Path, expected: [u8; 32]) -> Result<(), PqtyError> {
    let existing = read_store_manifest(path)?;
    if provider_manifest_digest(&existing.files)? != expected {
        return Err(PqtyError::Usage(format!(
            "conflicting package manifest in content-addressed store: {}",
            path.display()
        )));
    }
    make_read_only(path)
}

fn publish_if_absent(temp: tempfile::NamedTempFile, destination: &Path) -> Result<bool, PqtyError> {
    let created = match fs::hard_link(temp.path(), destination) {
        Ok(()) => true,
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => false,
        Err(source) => {
            return Err(PqtyError::Io {
                path: destination.to_owned(),
                source,
            });
        }
    };
    let path = temp.path().to_owned();
    // Remove the temporary link before making the destination read-only;
    // Windows otherwise also makes the temporary link difficult to delete.
    temp.close()
        .map_err(|source| PqtyError::Io { path, source })?;
    if created
        && let Some(parent) = destination.parent()
        && let Ok(directory) = fs::File::open(parent)
    {
        let _ = directory.sync_all();
    }
    Ok(created)
}

fn make_read_only(path: &Path) -> Result<(), PqtyError> {
    let mut permissions = fs::metadata(path)
        .map_err(|source| PqtyError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    if !permissions.readonly() {
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).map_err(|source| PqtyError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

pub(super) fn store_manifest_path(store_dir: &Path, package_digest: &str) -> PathBuf {
    store_dir
        .join("manifests")
        .join(format!("{package_digest}.json"))
}

pub(super) fn read_store_manifest(path: &Path) -> Result<StoredPackageManifest, PqtyError> {
    let bytes = read_file_bounded(path, 64 * 1024 * 1024)?;
    let manifest: StoredPackageManifest = serde_json::from_slice(&bytes)?;
    if manifest.schema != STORE_PACKAGE_SCHEMA {
        return Err(PqtyError::Usage(format!(
            "unsupported package store manifest {} in {}",
            manifest.schema,
            path.display()
        )));
    }
    Ok(manifest)
}

pub(super) fn package_digest(entry: &ResolvedPackage) -> Result<String, PqtyError> {
    let integrity = entry
        .integrity
        .as_deref()
        .ok_or_else(|| PqtyError::Usage(format!("provider {} has no integrity", entry.provider)))?;
    let encoded = integrity.strip_prefix("sha256-").ok_or_else(|| {
        PqtyError::Usage(format!(
            "provider {} has unsupported integrity {integrity}",
            entry.provider
        ))
    })?;
    let digest = BASE64.decode(encoded).map_err(|_| {
        PqtyError::Usage(format!(
            "provider {} has invalid integrity {integrity}",
            entry.provider
        ))
    })?;
    if digest.len() != 32 {
        return Err(PqtyError::Usage(format!(
            "provider {} has invalid integrity {integrity}",
            entry.provider
        )));
    }
    Ok(hex::encode(digest))
}

pub(super) fn load_store_manifest(
    store_dir: &Path,
    entry: &ResolvedPackage,
) -> Result<StoredPackageManifest, PqtyError> {
    let expected = package_digest(entry)?;
    let path = store_manifest_path(store_dir, &expected);
    let manifest = read_store_manifest(&path)?;
    let actual = hex::encode(provider_manifest_digest(&manifest.files)?);
    if actual != expected {
        return Err(PqtyError::Usage(format!(
            "package manifest checksum mismatch for {} in {}\n  expected: {expected}\n  got:      {actual}",
            entry.provider,
            path.display()
        )));
    }

    let locked_paths = entry
        .files
        .iter()
        .map(|file| file.tds_path.as_str())
        .collect::<BTreeSet<_>>();
    let stored_paths = manifest
        .files
        .iter()
        .map(|file| file.tds_path.as_str())
        .collect::<BTreeSet<_>>();
    if locked_paths.len() != entry.files.len()
        || stored_paths.len() != manifest.files.len()
        || locked_paths != stored_paths
    {
        return Err(PqtyError::Usage(format!(
            "package store manifest for {} does not match the locked file index",
            entry.provider
        )));
    }
    Ok(manifest)
}

/// Identity and publication result of one streamed immutable object.
pub(super) struct StoredObject {
    pub(super) digest: String,
    pub(super) size: u64,
    pub(super) created: bool,
}

/// Publish immutable, content-addressed bytes using standard-library-managed buffering.
/// Provider manifests are published separately after complete validation.
pub(super) fn store_reader(
    store_dir: &Path,
    reader: &mut dyn std::io::Read,
    expected: Option<&str>,
) -> Result<StoredObject, PqtyError> {
    fs::create_dir_all(store_dir).map_err(|source| PqtyError::Io {
        path: store_dir.to_owned(),
        source,
    })?;
    let mut file = temporary_file(&store_dir.join("incoming"), "object")?;
    let (digest, size) = super::stream::copy_hashed(
        reader,
        &mut file,
        crate::MAX_ARCHIVE_ENTRY_BYTES,
        &store_dir.join("incoming"),
    )?;
    if expected.is_some_and(|expected| expected != digest) {
        return Err(PqtyError::Usage(
            "refusing to publish store object with mismatched key".into(),
        ));
    }
    let path = store_dir.join(&digest[..2]).join(&digest);
    if path.exists() {
        if super::stream::digest_file(&path, crate::MAX_ARCHIVE_ENTRY_BYTES)?.0 == digest {
            make_read_only(&path)?;
            return Ok(StoredObject {
                digest,
                size,
                created: false,
            });
        }
        let evidence = quarantine_store_entry(store_dir, &path, "object")?;
        eprintln!(
            "pqty: quarantined corrupt store object as {}",
            evidence.display()
        );
    }
    let parent = path.parent().expect("object path has parent");
    fs::create_dir_all(parent).map_err(|source| PqtyError::Io {
        path: parent.to_owned(),
        source,
    })?;
    file.as_file().sync_all().map_err(|source| PqtyError::Io {
        path: file.path().to_owned(),
        source,
    })?;
    let created = publish_if_absent(file, &path)?;
    if !created && super::stream::digest_file(&path, crate::MAX_ARCHIVE_ENTRY_BYTES)?.0 != digest {
        return Err(PqtyError::Usage(format!(
            "conflicting content-addressed store object {}",
            path.display()
        )));
    }
    make_read_only(&path)?;
    Ok(StoredObject {
        digest,
        size,
        created,
    })
}

#[cfg(test)]
pub(super) fn ensure_store_object(
    store_dir: &Path,
    store_path: &Path,
    bytes: &[u8],
    expected: &str,
) -> Result<bool, PqtyError> {
    if store_path != store_dir.join(&expected[..2]).join(expected) {
        return Err(PqtyError::Usage(
            "store path does not match digest".to_owned(),
        ));
    }
    Ok(store_reader(store_dir, &mut &*bytes, Some(expected))?.created)
}

pub(super) fn quarantine_store_entry(
    store_dir: &Path,
    path: &Path,
    label: &str,
) -> Result<PathBuf, PqtyError> {
    let quarantine = store_dir.join("quarantine");
    fs::create_dir_all(&quarantine).map_err(|source| PqtyError::Io {
        path: quarantine.clone(),
        source,
    })?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| PqtyError::Usage("store entry has no portable filename".to_string()))?;
    let destination = unique_sibling(&quarantine.join(name), label)?;
    fs::rename(path, &destination).map_err(|source| PqtyError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(destination)
}
