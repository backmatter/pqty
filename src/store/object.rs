use crate::store::container::container_cache_path;
use crate::store::http::read_file_bounded;
use crate::store::{MaterializeReport, PackageByteSource, provider_bytes};

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    LockFile, LockedFile, MAX_CLOSURE_EXPANDED_BYTES, MAX_CLOSURE_RUNFILES,
    MAX_CONTAINER_EXPANDED_BYTES, MAX_PROVIDER_RUNFILES, PackageRegistry, PqtyError,
    ResolvedPackage, progress, resource_kind, unique_sibling,
};

/// Fetch and verify one provider's runfiles into the content-addressable store.
pub(super) struct ProviderMaterialization {
    pub(super) integrity: String,
    pub(super) store_key: String,
    pub(super) files: Vec<LockedFile>,
    pub(super) expanded_bytes: u64,
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
    let files = provider_bytes(
        source,
        provider,
        container_checksum,
        container_size,
        &runfiles,
    )?;
    let expanded_bytes = files.iter().try_fold(0_u64, |total, (_, bytes)| {
        total
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| PqtyError::Usage("provider byte count overflow".to_string()))
    })?;
    if expanded_bytes > MAX_CONTAINER_EXPANDED_BYTES {
        return Err(PqtyError::Usage(format!(
            "provider {provider} exceeds the {MAX_CONTAINER_EXPANDED_BYTES}-byte expanded-content limit"
        )));
    }

    let mut locked_files = Vec::new();
    let mut stored_files = Vec::new();
    for (relative, bytes) in &files {
        let hex_digest = hex::encode(Sha256::digest(bytes));
        let store_path = store_dir.join(&hex_digest[..2]).join(&hex_digest);
        if ensure_store_object(store_dir, &store_path, bytes, &hex_digest)? {
            report.bytes_stored += bytes.len() as u64;
        }
        let tds_path = relative
            .strip_prefix("texmf-dist/")
            .unwrap_or(relative)
            .to_string();
        locked_files.push(LockedFile {
            kind: resource_kind(&tds_path),
            tds_path: tds_path.clone(),
        });
        stored_files.push(StoredFile {
            tds_path,
            digest: format!("sha256:{hex_digest}"),
        });
        report.files += 1;
    }

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
    let temp = unique_sibling(&path, "manifest")?;
    let result = (|| -> Result<(), PqtyError> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(|source| PqtyError::Io {
                path: temp.clone(),
                source,
            })?;
        file.write_all(&bytes).map_err(|source| PqtyError::Io {
            path: temp.clone(),
            source,
        })?;
        file.sync_all().map_err(|source| PqtyError::Io {
            path: temp.clone(),
            source,
        })?;
        let won = publish_if_absent(&temp, &path)?;
        if won {
            make_read_only(&path)?;
            Ok(())
        } else {
            verify_manifest_winner(&path, expected)
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
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

fn publish_if_absent(temp: &Path, destination: &Path) -> Result<bool, PqtyError> {
    match fs::hard_link(temp, destination) {
        Ok(()) => {
            fs::remove_file(temp).map_err(|source| PqtyError::Io {
                path: temp.to_path_buf(),
                source,
            })?;
            if let Some(parent) = destination.parent()
                && let Ok(directory) = fs::File::open(parent)
            {
                let _ = directory.sync_all();
            }
            Ok(true)
        }
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(temp).map_err(|source| PqtyError::Io {
                path: temp.to_path_buf(),
                source,
            })?;
            Ok(false)
        }
        Err(source) => Err(PqtyError::Io {
            path: destination.to_path_buf(),
            source,
        }),
    }
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

/// Ensure a content-addressed store object exists and actually matches its key.
/// New objects are written to a sibling temporary file and atomically promoted.
/// Returns whether new bytes were stored.
pub(super) fn ensure_store_object(
    store_dir: &Path,
    store_path: &Path,
    bytes: &[u8],
    expected_hex: &str,
) -> Result<bool, PqtyError> {
    let input_digest = hex::encode(Sha256::digest(bytes));
    if input_digest != expected_hex {
        return Err(PqtyError::Usage(format!(
            "refusing to publish store object with mismatched key\n  expected: {expected_hex}\n  got:      {input_digest}"
        )));
    }
    if store_path.exists() {
        let existing = fs::read(store_path).map_err(|source| PqtyError::Io {
            path: store_path.to_path_buf(),
            source,
        })?;
        let got = hex::encode(Sha256::digest(&existing));
        if got == expected_hex {
            make_read_only(store_path)?;
            return Ok(false);
        }
        let evidence = quarantine_store_entry(store_dir, store_path, "object")?;
        eprintln!(
            "pqty: quarantined corrupt store object as {}",
            evidence.display()
        );
    }

    let parent = store_path
        .parent()
        .ok_or_else(|| PqtyError::Usage("store object has no parent directory".to_string()))?;
    fs::create_dir_all(parent).map_err(|source| PqtyError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let temp = unique_sibling(store_path, "object")?;
    let write_result = (|| -> Result<bool, PqtyError> {
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
        if publish_if_absent(&temp, store_path)? {
            make_read_only(store_path)?;
            Ok(true)
        } else {
            let existing = fs::read(store_path).map_err(|source| PqtyError::Io {
                path: store_path.to_path_buf(),
                source,
            })?;
            let got = hex::encode(Sha256::digest(existing));
            if got != expected_hex {
                return Err(PqtyError::Usage(format!(
                    "conflicting content-addressed store object {}\n  expected: {expected_hex}\n  got:      {got}",
                    store_path.display()
                )));
            }
            make_read_only(store_path)?;
            Ok(false)
        }
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_result
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
