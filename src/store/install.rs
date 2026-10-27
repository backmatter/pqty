use crate::store::container::container_cache_path;
use crate::store::http::read_file_bounded;
use crate::store::object::{
    STORE_PACKAGE_SCHEMA, StoredFile, StoredPackageManifest, ensure_store_object,
    load_store_manifest, package_digest, provider_manifest_digest, quarantine_store_entry,
    store_manifest_path, write_store_manifest,
};
use crate::store::transaction::{FileGuard, Installation};
use crate::store::{
    ContainerRequest, MaterializeReport, default_cache_dir, fetch_container_runfiles,
};

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    LinkMode, LockFile, LockedFile, MAX_ARCHIVE_ENTRY_BYTES, PqtyError, ResolvedEnvironment,
    ResolvedPackage, canonical_or_original, progress, usable_parent, validate_materialized_lock,
    validate_tds_path,
};

const MATERIALIZED_MARKER: &str = ".pqty-materialized.json";
const MATERIALIZED_MARKER_SCHEMA: &str = "pqty.materialized-tree/v1";
const MAX_MATERIALIZED_MARKER_BYTES: u64 = 16 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MaterializedTreeMarker {
    schema: String,
    generated_with: String,
    environment_fingerprint: String,
    #[serde(default)]
    link_mode: Option<String>,
}

/// Reproduce an exact lock without re-resolving it. Existing content-addressed
/// objects are checked against the package manifest in the store when a new
/// tree is needed; an existing copied tree is checked directly against the lock.
/// Missing objects are recovered from the registry recorded in the lock and the whole
/// provider is verified against its one locked digest.
/// Reproduce a locked TEXMF tree without resolving against current registry
/// metadata.
///
/// # Errors
///
/// Returns an error when locked objects cannot be fetched, fail their digests,
/// or cannot be placed transactionally in the output tree.
pub fn install_locked(
    lock: &LockFile,
    store_dir: &Path,
    out_dir: &Path,
    mode: LinkMode,
) -> Result<MaterializeReport, PqtyError> {
    install_locked_with_policy(lock, store_dir, out_dir, mode, false, false)
}

pub(crate) fn install_locked_with_policy(
    lock: &LockFile,
    store_dir: &Path,
    out_dir: &Path,
    mode: LinkMode,
    offline: bool,
    allow_insecure: bool,
) -> Result<MaterializeReport, PqtyError> {
    validate_materialized_lock(lock)?;
    fs::create_dir_all(store_dir).map_err(|source| PqtyError::Io {
        path: store_dir.to_path_buf(),
        source,
    })?;
    let store_dir = canonical_or_original(store_dir);
    fs::create_dir_all(usable_parent(out_dir)).map_err(|source| PqtyError::Io {
        path: out_dir.to_path_buf(),
        source,
    })?;
    ensure_distinct_store_and_output(&store_dir, out_dir)?;
    let installation = Installation::acquire(out_dir)?;

    let mut report = MaterializeReport::new(store_dir.clone(), Some(out_dir.to_path_buf()));
    if installed_tree_matches(&installation.destination, lock, mode, &store_dir)? {
        report.providers = lock.closure.len();
        report.files = lock.closure.iter().map(|entry| entry.files.len()).sum();
        return Ok(report);
    }
    let _store_guard = FileGuard::store(&store_dir)?;
    let mut inspected = Vec::with_capacity(lock.closure.len());
    for entry in &lock.closure {
        let state = inspect_stored_provider(&store_dir, entry)?;
        inspected.push((entry, state));
    }
    let recovery = inspected
        .iter()
        .filter_map(|(entry, state)| {
            (!matches!(state, StoredProviderState::Ready)).then_some(*entry)
        })
        .collect::<Vec<_>>();
    emit_locked_download_plan(lock, &recovery)?;

    for (entry, state) in inspected {
        match state {
            StoredProviderState::Ready => {}
            StoredProviderState::Missing => {
                recover_locked_provider(
                    lock,
                    entry,
                    &store_dir,
                    &mut report,
                    offline,
                    allow_insecure,
                )?;
            }
            StoredProviderState::Corrupt { evidence, reason } => {
                if offline {
                    return Err(PqtyError::Usage(format!(
                        "offline mode preserved corrupt store evidence for {} at {}; recovery requires its recorded source ({reason})",
                        entry.provider,
                        evidence.display()
                    )));
                }
                recover_locked_provider(
                    lock,
                    entry,
                    &store_dir,
                    &mut report,
                    false,
                    allow_insecure,
                )?;
            }
        }
        report.providers += 1;
        report.files += entry.files.len();
    }

    materialize_locked(lock, &store_dir, &installation, mode, true)?;
    report.out = Some(out_dir.to_path_buf());
    Ok(report)
}

fn emit_locked_download_plan(
    lock: &LockFile,
    entries: &[&ResolvedPackage],
) -> Result<(), PqtyError> {
    let cache_dir = default_cache_dir().join("containers");
    let mut bytes_total = 0_u64;
    let mut bytes_cached = 0_u64;
    let mut items_total = 0_usize;
    let mut items_cached = 0_usize;
    for entry in entries {
        let registry_id = entry.source.registry.as_deref().ok_or_else(|| {
            PqtyError::Usage(format!(
                "provider {} has no source registry",
                entry.provider
            ))
        })?;
        let registry = lock
            .registries
            .iter()
            .find(|registry| registry.id == registry_id)
            .ok_or_else(|| {
                PqtyError::Usage(format!(
                    "provider {} references unknown registry {}",
                    entry.provider, registry_id
                ))
            })?;
        if !registry.url.starts_with("http://") && !registry.url.starts_with("https://") {
            continue;
        }
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
        items_total += 1;
        bytes_total = bytes_total
            .checked_add(size)
            .ok_or_else(|| PqtyError::Usage("download plan byte count overflow".to_string()))?;
        if container_cache_path(&cache_dir, &entry.source.locator, checksum).is_file() {
            items_cached += 1;
            bytes_cached = bytes_cached
                .checked_add(size)
                .ok_or_else(|| PqtyError::Usage("download plan byte count overflow".to_string()))?;
        }
    }
    if items_total > 0 {
        progress::download_plan(
            progress::DownloadCategory::Packages,
            items_total,
            items_cached,
            Some(bytes_total),
            Some(bytes_cached),
        );
    }
    Ok(())
}

fn recover_locked_provider(
    lock: &LockFile,
    entry: &ResolvedPackage,
    store_dir: &Path,
    report: &mut MaterializeReport,
    offline: bool,
    allow_insecure: bool,
) -> Result<(), PqtyError> {
    let required = entry.files.iter().collect::<Vec<_>>();
    let bytes = locked_provider_bytes(lock, entry, &required, offline, allow_insecure)?;
    store_locked_provider(store_dir, entry, &bytes, report)
}

enum StoredProviderState {
    Ready,
    Missing,
    Corrupt { evidence: PathBuf, reason: String },
}

pub(super) fn stored_file_digest(digest: &str) -> Result<&str, PqtyError> {
    digest
        .strip_prefix("sha256:")
        .filter(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| PqtyError::Usage(format!("invalid stored file digest: {digest}")))
}

fn file_digest(path: &Path) -> Result<String, PqtyError> {
    let mut file = fs::File::open(path).map_err(|source| PqtyError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 16384];
    loop {
        let count = file.read(&mut buffer).map_err(|source| PqtyError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn verify_file_digest(path: &Path, expected: &str) -> Result<(), PqtyError> {
    let got = file_digest(path)?;
    if got == expected {
        Ok(())
    } else {
        Err(PqtyError::Usage(format!(
            "corrupt content-addressed store object {}\n  expected: {expected}\n  got:      {got}",
            path.display()
        )))
    }
}

fn inspect_stored_provider(
    store_dir: &Path,
    entry: &ResolvedPackage,
) -> Result<StoredProviderState, PqtyError> {
    let path = store_manifest_path(store_dir, &package_digest(entry)?);
    if !path.is_file() {
        return Ok(StoredProviderState::Missing);
    }
    let manifest = match load_store_manifest(store_dir, entry) {
        Ok(manifest) => manifest,
        Err(error) => {
            let reason = error.to_string();
            let evidence = quarantine_store_entry(store_dir, &path, "manifest")?;
            return Ok(StoredProviderState::Corrupt { evidence, reason });
        }
    };
    for file in &manifest.files {
        let digest = stored_file_digest(&file.digest)?;
        let path = store_dir.join(&digest[..2]).join(digest);
        if !path.is_file() {
            return Ok(StoredProviderState::Missing);
        }
        if let Err(error) = verify_file_digest(&path, digest) {
            let reason = error.to_string();
            let evidence = quarantine_store_entry(store_dir, &path, "object")?;
            return Ok(StoredProviderState::Corrupt { evidence, reason });
        }
    }
    Ok(StoredProviderState::Ready)
}

fn store_locked_provider(
    store_dir: &Path,
    entry: &ResolvedPackage,
    bytes: &BTreeMap<String, Vec<u8>>,
    report: &mut MaterializeReport,
) -> Result<(), PqtyError> {
    let locked_paths = entry
        .files
        .iter()
        .map(|file| file.tds_path.as_str())
        .collect::<BTreeSet<_>>();
    let fetched_paths = bytes.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if locked_paths != fetched_paths {
        return Err(PqtyError::Usage(format!(
            "fetched provider {} does not match its locked file index",
            entry.provider
        )));
    }

    let mut files = Vec::with_capacity(bytes.len());
    let mut content_digests = Vec::with_capacity(bytes.len());
    for (path, content) in bytes {
        let digest = hex::encode(Sha256::digest(content));
        files.push(StoredFile {
            tds_path: path.clone(),
            digest: format!("sha256:{digest}"),
        });
        content_digests.push((content, digest));
    }
    files.sort_by(|left, right| left.tds_path.cmp(&right.tds_path));
    let actual = hex::encode(provider_manifest_digest(&files)?);
    let expected = package_digest(entry)?;
    if actual != expected {
        return Err(PqtyError::Usage(format!(
            "package checksum mismatch for {}\n  expected: {expected}\n  got:      {actual}",
            entry.provider
        )));
    }
    for (content, digest) in content_digests {
        let store_path = store_dir.join(&digest[..2]).join(&digest);
        if ensure_store_object(store_dir, &store_path, content, &digest)? {
            report.bytes_stored += content.len() as u64;
        }
    }
    write_store_manifest(
        store_dir,
        &expected,
        &StoredPackageManifest {
            schema: STORE_PACKAGE_SCHEMA.to_string(),
            files,
        },
    )
}

fn locked_provider_bytes(
    lock: &LockFile,
    entry: &ResolvedPackage,
    required: &[&LockedFile],
    offline: bool,
    allow_insecure: bool,
) -> Result<BTreeMap<String, Vec<u8>>, PqtyError> {
    let registry_id = entry.source.registry.as_deref().ok_or_else(|| {
        PqtyError::Usage(format!(
            "provider {} has no source registry",
            entry.provider
        ))
    })?;
    let registry = lock
        .registries
        .iter()
        .find(|registry| registry.id == registry_id)
        .ok_or_else(|| {
            PqtyError::Usage(format!(
                "provider {} references unknown registry {}",
                entry.provider, registry_id
            ))
        })?;
    if registry.url.starts_with("http://") || registry.url.starts_with("https://") {
        let runfiles = required
            .iter()
            .map(|file| file.tds_path.clone())
            .collect::<Vec<_>>();
        let cache_dir = default_cache_dir().join("containers");
        let files = fetch_container_runfiles(&ContainerRequest {
            base_url: registry.url.trim_end_matches('/'),
            cache_dir: &cache_dir,
            provider: &entry.source.locator,
            expected_sha512: entry.source.container_checksum.as_deref(),
            expected_size: entry.source.container_size,
            offline,
            allow_insecure,
            runfiles: &runfiles,
        })?;
        let mut result = BTreeMap::new();
        for (placement, bytes) in files {
            let tds_path = placement
                .strip_prefix("texmf-dist/")
                .unwrap_or(&placement)
                .to_string();
            result.insert(tds_path, bytes);
        }
        return Ok(result);
    }

    let root = registry.url.strip_prefix("file://").ok_or_else(|| {
        PqtyError::Usage(format!(
            "unsupported registry URL for {}: {}",
            entry.provider, registry.url
        ))
    })?;
    let root = Path::new(root);
    let mut result = BTreeMap::new();
    for file in required {
        validate_tds_path(&file.tds_path, "locked TDS path")?;
        let installed_path = root.join("texmf-dist").join(&file.tds_path);
        let bytes = read_file_bounded(&installed_path, MAX_ARCHIVE_ENTRY_BYTES)?;
        result.insert(file.tds_path.clone(), bytes);
    }
    Ok(result)
}

/// Publish a lock's standard TDS paths into a complete TEXMF tree. The tree is
/// assembled beside the destination and renamed into place only after every
/// locked store object has been checked.
/// Materialize an already hydrated lock from the shared object store.
///
/// # Errors
///
/// Returns an error when a store object is missing or corrupt, or when the
/// output tree cannot be replaced transactionally.
pub fn materialize_from_store(
    lock: &LockFile,
    store_dir: &Path,
    out_dir: &Path,
    mode: LinkMode,
) -> Result<(), PqtyError> {
    validate_materialized_lock(lock)?;
    let parent = usable_parent(out_dir);
    fs::create_dir_all(parent).map_err(|source| PqtyError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let store_dir = canonical_or_original(store_dir);
    ensure_distinct_store_and_output(&store_dir, out_dir)?;
    let installation = Installation::acquire(out_dir)?;
    if installed_tree_matches(&installation.destination, lock, mode, &store_dir)? {
        return Ok(());
    }
    let _store_guard = FileGuard::store(&store_dir)?;
    materialize_locked(lock, &store_dir, &installation, mode, false)
}

fn materialize_locked(
    lock: &LockFile,
    store_dir: &Path,
    installation: &Installation,
    mode: LinkMode,
    store_verified: bool,
) -> Result<(), PqtyError> {
    let environment = ResolvedEnvironment::from_lock(lock)?;
    // Check each store object once, then use this same placement plan for
    // auditing the installed tree and, only if necessary, rebuilding it.
    let mut files = BTreeMap::new();
    for entry in &lock.closure {
        let manifest = load_store_manifest(store_dir, entry)?;
        for file in manifest.files {
            if file.tds_path == MATERIALIZED_MARKER {
                return Err(PqtyError::Usage(format!(
                    "locked TDS path uses pqty's reserved ownership marker: {MATERIALIZED_MARKER}"
                )));
            }
            let digest = stored_file_digest(&file.digest)?;
            let object = store_dir.join(&digest[..2]).join(digest);
            if !store_verified {
                verify_file_digest(&object, digest)?;
            }
            files.insert(PathBuf::from(file.tds_path), (object, digest.to_string()));
        }
    }
    let staging = &installation.stage;
    fs::create_dir(staging).map_err(|source| PqtyError::Io {
        path: staging.clone(),
        source,
    })?;
    let assemble = (|| {
        for (path, (object, _)) in &files {
            place(object, &staging.join(path), mode)?;
        }
        write_materialized_marker(staging, &environment.fingerprint, mode)?;
        installation.publish()
    })();
    if assemble.is_err() && staging.exists() {
        let _ = remove_materialized_tree(staging);
    }
    assemble
}

fn installed_tree_matches(
    root: &Path,
    lock: &LockFile,
    mode: LinkMode,
    store_dir: &Path,
) -> Result<bool, PqtyError> {
    let marker = fs::read(root.join(MATERIALIZED_MARKER))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<MaterializedTreeMarker>(&bytes).ok());
    let Some(marker) = marker else {
        return Ok(false);
    };
    if marker.link_mode.as_deref() != Some(mode_name(mode))
        || marker.environment_fingerprint != ResolvedEnvironment::from_lock(lock)?.fingerprint
    {
        return Ok(false);
    }
    let expected: BTreeSet<_> = lock
        .closure
        .iter()
        .flat_map(|entry| entry.files.iter().map(|file| Path::new(&file.tds_path)))
        .collect();
    let mut pending = vec![root.to_path_buf()];
    let mut digests = BTreeMap::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|source| PqtyError::Io {
            path: directory.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| PqtyError::Io {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let relative = path.strip_prefix(root).expect("TEXMF descendant");
            let kind = entry.file_type().map_err(|source| PqtyError::Io {
                path: path.clone(),
                source,
            })?;
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            if !expected.contains(relative) {
                if relative == Path::new(MATERIALIZED_MARKER) || relative == Path::new("ls-R") {
                    continue;
                }
                return Ok(false);
            }
            if !(kind.is_file() || kind.is_symlink()) {
                return Ok(false);
            }
            let Ok(digest) = file_digest(&path) else {
                return Ok(false);
            };
            if kind.is_symlink()
                && (!matches!(mode, LinkMode::Symlink)
                    || fs::read_link(&path).ok()
                        != Some(store_dir.join(&digest[..2]).join(&digest)))
            {
                return Ok(false);
            }
            digests.insert(relative.to_path_buf(), format!("sha256:{digest}"));
        }
    }
    if digests.len() != expected.len() {
        return Ok(false);
    }
    // Reconstruct each provider manifest from the actual installed bytes. Its
    // digest is already pinned in the exact lock, so auditing a copied tree
    // needs neither shared-cache files nor a registry request.
    for entry in &lock.closure {
        let files = entry
            .files
            .iter()
            .map(|file| StoredFile {
                tds_path: file.tds_path.clone(),
                digest: digests[Path::new(&file.tds_path)].clone(),
            })
            .collect::<Vec<_>>();
        if hex::encode(provider_manifest_digest(&files)?) != package_digest(entry)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn mode_name(mode: LinkMode) -> &'static str {
    match mode {
        LinkMode::Copy => "copy",
        LinkMode::Symlink => "experimental-symlink",
        LinkMode::Hardlink => "experimental-hardlink",
    }
}

fn ensure_distinct_store_and_output(store_dir: &Path, out_dir: &Path) -> Result<(), PqtyError> {
    let store = store_dir.canonicalize().map_err(|source| PqtyError::Io {
        path: store_dir.to_path_buf(),
        source,
    })?;
    let output = canonical_destination(out_dir)?;
    if store == output || store.starts_with(&output) || output.starts_with(&store) {
        return Err(PqtyError::Usage(format!(
            "content store and TEXMF output must not overlap: {} and {}",
            store.display(),
            output.display()
        )));
    }
    Ok(())
}

fn canonical_destination(path: &Path) -> Result<PathBuf, PqtyError> {
    let name = path.file_name().ok_or_else(|| {
        PqtyError::Usage(format!(
            "TEXMF output must name a concrete directory: {}",
            path.display()
        ))
    })?;
    let parent = usable_parent(path);
    let parent = parent.canonicalize().map_err(|source| PqtyError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    Ok(parent.join(name))
}

pub(super) fn ensure_replaceable_destination(destination: &Path) -> Result<(), PqtyError> {
    let metadata = match destination.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(PqtyError::Io {
                path: destination.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(PqtyError::Usage(format!(
            "refusing to replace non-directory TEXMF target: {}",
            destination.display()
        )));
    }
    let mut entries = fs::read_dir(destination).map_err(|source| PqtyError::Io {
        path: destination.to_path_buf(),
        source,
    })?;
    if entries.next().is_none() {
        return Ok(());
    }

    let marker_path = destination.join(MATERIALIZED_MARKER);
    let marker_metadata = marker_path.symlink_metadata().map_err(|_| {
        PqtyError::Usage(format!(
            "refusing to replace non-empty TEXMF target not owned by pqty: {}",
            destination.display()
        ))
    })?;
    if !marker_metadata.is_file()
        || marker_metadata.file_type().is_symlink()
        || marker_metadata.len() > MAX_MATERIALIZED_MARKER_BYTES
    {
        return Err(PqtyError::Usage(format!(
            "refusing to replace TEXMF target with an invalid pqty ownership marker: {}",
            destination.display()
        )));
    }
    let mut bytes = Vec::new();
    fs::File::open(&marker_path)
        .and_then(|file| {
            file.take(MAX_MATERIALIZED_MARKER_BYTES.saturating_add(1))
                .read_to_end(&mut bytes)
        })
        .map_err(|source| PqtyError::Io {
            path: marker_path,
            source,
        })?;
    let marker: MaterializedTreeMarker = serde_json::from_slice(&bytes).map_err(|_| {
        PqtyError::Usage(format!(
            "refusing to replace TEXMF target with an invalid pqty ownership marker: {}",
            destination.display()
        ))
    })?;
    if marker.schema != MATERIALIZED_MARKER_SCHEMA || !valid_sha256(&marker.environment_fingerprint)
    {
        return Err(PqtyError::Usage(format!(
            "refusing to replace TEXMF target with an unsupported pqty ownership marker: {}",
            destination.display()
        )));
    }
    Ok(())
}

fn write_materialized_marker(
    root: &Path,
    fingerprint: &str,
    mode: LinkMode,
) -> Result<(), PqtyError> {
    let marker = MaterializedTreeMarker {
        schema: MATERIALIZED_MARKER_SCHEMA.to_string(),
        generated_with: format!("pqty/{}", env!("CARGO_PKG_VERSION")),
        environment_fingerprint: fingerprint.to_string(),
        link_mode: Some(mode_name(mode).to_string()),
    };
    let mut bytes = serde_json::to_vec_pretty(&marker)?;
    bytes.push(b'\n');
    let path = root.join(MATERIALIZED_MARKER);
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|source| PqtyError::Io {
            path: path.clone(),
            source,
        })?;
    file.write_all(&bytes).map_err(|source| PqtyError::Io {
        path: path.clone(),
        source,
    })?;
    file.sync_all().map_err(|source| PqtyError::Io {
        path: path.clone(),
        source,
    })?;
    let mut permissions = file
        .metadata()
        .map_err(|source| PqtyError::Io {
            path: path.clone(),
            source,
        })?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).map_err(|source| PqtyError::Io { path, source })
}

fn valid_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Create a file symlink (platform-specific; unsupported targets fall back to a
/// copy via the caller).
#[cfg(unix)]
fn symlink_file(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn symlink_file(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(src, dst)
}

#[cfg(not(any(unix, windows)))]
fn symlink_file(src: &Path, dst: &Path) -> io::Result<()> {
    fs::copy(src, dst).map(|_| ())
}

fn copy_store_file(src: &Path, dst: &Path) -> io::Result<()> {
    fs::copy(src, dst)?;
    let mut permissions = fs::metadata(dst)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(dst, permissions)
}

pub(super) fn remove_materialized_tree(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    make_tree_removable(path)?;
    fs::remove_dir_all(path)
}

#[cfg(windows)]
#[allow(
    clippy::permissions_set_readonly_false,
    reason = "this Windows-only helper must clear FILE_ATTRIBUTE_READONLY before removal"
)]
fn make_tree_removable(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        for entry in fs::read_dir(path)? {
            make_tree_removable(&entry?.path())?;
        }
    }
    let mut permissions = metadata.permissions();
    if permissions.readonly() {
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

/// Place a stored file into the TEXMF tree, replacing any existing entry.
/// Symlinks fall back to a copy when unsupported (e.g. Windows without the
/// privilege), so the tree is always populated.
pub(super) fn place(store_path: &Path, out_path: &Path, mode: LinkMode) -> Result<(), PqtyError> {
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).map_err(|source| PqtyError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    if out_path.symlink_metadata().is_ok() {
        let _ = fs::remove_file(out_path);
    }
    let result =
        match mode {
            LinkMode::Copy => copy_store_file(store_path, out_path),
            LinkMode::Symlink => symlink_file(store_path, out_path)
                .or_else(|_| copy_store_file(store_path, out_path)),
            LinkMode::Hardlink => fs::hard_link(store_path, out_path)
                .or_else(|_| copy_store_file(store_path, out_path)),
        };
    result.map_err(|source| PqtyError::Io {
        path: out_path.to_path_buf(),
        source,
    })
}
