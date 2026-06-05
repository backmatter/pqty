use crate::store::archive::decompress_xz_bounded;
use crate::store::container::{duration_millis, read_bounded_download};
use crate::store::http::{
    HttpGetOutcome, get_with_safe_redirects, read_file_bounded, response_header,
    validate_remote_url,
};
use crate::store::{MetadataCachePolicy, RegistryRequest};

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    LockFile, MAX_TLPDB_COMPRESSED_BYTES, MAX_TLPDB_EXPANDED_BYTES, PqtyError, RegistryKind,
    TlpdbIndex, atomic_write, locate_tlpdb, progress,
};

pub(super) fn default_cache_dir() -> PathBuf {
    default_cache_dir_from(|name| std::env::var_os(name), cfg!(target_os = "windows"))
}

fn default_cache_dir_from(
    mut variable: impl FnMut(&str) -> Option<OsString>,
    windows: bool,
) -> PathBuf {
    if let Some(cache) = variable("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(cache).join("pqty");
    }
    if windows && let Some(cache) = variable("LOCALAPPDATA").filter(|value| !value.is_empty()) {
        return PathBuf::from(cache).join("pqty");
    }
    if let Some(home) = variable("HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home).join(".cache/pqty");
    }
    if windows && let Some(home) = variable("USERPROFILE").filter(|value| !value.is_empty()) {
        return PathBuf::from(home).join(".cache/pqty");
    }
    PathBuf::from(".pqty-cache")
}

pub(crate) fn default_store_dir() -> PathBuf {
    default_cache_dir().join("store")
}

/// Resolve which tlpdb to use: an explicit path, a fetched URL, or the local
/// TeX Live. The `PackageRegistry` abstraction means remote is just "download
/// the file, then load it as before".
pub(crate) fn load_tlpdb_index(
    path: Option<PathBuf>,
    request: Option<&RegistryRequest>,
) -> Result<TlpdbIndex, PqtyError> {
    let (resolved, origin, snapshot) = match (path, request) {
        (Some(path), _) => (path, None, None),
        (None, Some(request)) => {
            let cached = fetch_tlpdb(
                &request.url,
                &default_cache_dir().join("tlpdb"),
                request.cache_policy,
                request.allow_insecure,
            )?;
            (
                cached,
                tlnet_base_from_tlpdb_url(&request.url),
                request.snapshot.clone(),
            )
        }
        (None, None) => (
            locate_tlpdb().ok_or_else(|| {
                PqtyError::Usage(
                    "could not locate texlive.tlpdb; pass --tlpdb <path> or --tlpdb-url <url>"
                        .to_string(),
                )
            })?,
            None,
            None,
        ),
    };
    let mut index = TlpdbIndex::load(&resolved)?;
    if origin.is_none() {
        index.retain_installed_runfiles();
    }
    index.origin = origin;
    index.snapshot_override = snapshot;
    Ok(index)
}

pub(crate) fn validate_tlpdb_digest(index: &TlpdbIndex, expected: &str) -> Result<(), PqtyError> {
    let expected = expected.strip_prefix("sha256:").unwrap_or(expected);
    let actual = index
        .metadata_digest()
        .strip_prefix("sha256:")
        .unwrap_or(index.metadata_digest());
    if expected == actual {
        Ok(())
    } else {
        Err(PqtyError::Usage(format!(
            "package registry metadata checksum mismatch\n  expected: sha256:{expected}\n  got:      sha256:{actual}"
        )))
    }
}

/// Reload the exact registry declared by a lock. Explicit CLI locations are
/// useful when the recorded local installation moved; absent overrides, the
/// lock remains the source of truth rather than `pqty.toml` or a rolling
/// default.
pub(crate) fn load_convergence_index(
    lock: &LockFile,
    path: Option<PathBuf>,
    url: Option<String>,
    offline: bool,
    allow_insecure: bool,
) -> Result<(TlpdbIndex, Option<RegistryRequest>), PqtyError> {
    let cache_policy = if offline {
        MetadataCachePolicy::Offline
    } else {
        MetadataCachePolicy::Immutable
    };
    let locked_registry = lock.registries.first();
    if let Some(path) = path {
        let mut index = load_tlpdb_index(Some(path), None)?;
        if let [registry] = lock.registries.as_slice()
            && registry.kind == RegistryKind::Tlnet
            && registry.url.contains("://")
            && !registry.url.starts_with("file://")
        {
            index.origin = Some(registry.url.clone());
            let url = format!(
                "{}/tlpkg/texlive.tlpdb.xz",
                registry.url.trim_end_matches('/')
            );
            index.snapshot_override.clone_from(&registry.snapshot);
            return Ok((
                index,
                Some(RegistryRequest {
                    url,
                    snapshot: registry.snapshot.clone(),
                    cache_policy,
                    allow_insecure,
                }),
            ));
        }
        return Ok((index, None));
    }
    if let Some(url) = url {
        let request = RegistryRequest {
            url,
            snapshot: locked_registry.and_then(|registry| registry.snapshot.clone()),
            cache_policy,
            allow_insecure,
        };
        request.validate()?;
        let index = load_tlpdb_index(None, Some(&request))?;
        return Ok((index, Some(request)));
    }

    let [registry] = lock.registries.as_slice() else {
        return Err(PqtyError::Usage(
            "automatic convergence currently requires exactly one registry in the lock; pass --tlpdb or --tlpdb-url"
                .to_string(),
        ));
    };
    if let Some(root) = registry.url.strip_prefix("file://") {
        let path = Path::new(root).join("tlpkg/texlive.tlpdb");
        let index = load_tlpdb_index(Some(path), None)?;
        return Ok((index, None));
    }
    if !registry.url.contains("://") {
        return Err(PqtyError::Usage(format!(
            "cannot reload registry location {}; pass --tlpdb or --tlpdb-url",
            registry.url
        )));
    }
    let url = format!(
        "{}/tlpkg/texlive.tlpdb.xz",
        registry.url.trim_end_matches('/')
    );
    let request = RegistryRequest {
        url,
        snapshot: registry.snapshot.clone(),
        cache_policy,
        allow_insecure,
    };
    request.validate()?;
    let index = load_tlpdb_index(None, Some(&request))?;
    Ok((index, Some(request)))
}

/// Derive the tlnet base (containers live at `<base>/archive/<pkg>.tar.xz`) from
/// a tlpdb URL like `<base>/tlpkg/texlive.tlpdb[.xz]`.
pub(crate) fn tlnet_base_from_tlpdb_url(url: &str) -> Option<String> {
    for suffix in ["/tlpkg/texlive.tlpdb.xz", "/tlpkg/texlive.tlpdb"] {
        if let Some(base) = url.strip_suffix(suffix) {
            return Some(base.to_string());
        }
    }
    None
}

const REGISTRY_CACHE_SCHEMA: &str = "pqty.registry-cache/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CachedRegistryMetadata {
    pub(super) schema: String,
    pub(super) url: String,
    pub(super) metadata_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) last_modified: Option<String>,
}

pub(super) fn registry_cache_paths(url: &str, cache_dir: &Path) -> (PathBuf, PathBuf) {
    let key = hex::encode(Sha256::digest(url.as_bytes()));
    (
        cache_dir.join(format!("{key}.tlpdb")),
        cache_dir.join(format!("{key}.json")),
    )
}

pub(super) fn read_registry_cache_metadata(
    path: &Path,
) -> Result<Option<CachedRegistryMetadata>, PqtyError> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = read_file_bounded(path, 64 * 1024)?;
    let metadata: CachedRegistryMetadata = serde_json::from_slice(&bytes)?;
    if metadata.schema != REGISTRY_CACHE_SCHEMA {
        return Err(PqtyError::Usage(format!(
            "unsupported registry cache metadata {} in {}",
            metadata.schema,
            path.display()
        )));
    }
    Ok(Some(metadata))
}

fn verify_cached_tlpdb(
    url: &str,
    cache_path: &Path,
    metadata_path: &Path,
) -> Result<CachedRegistryMetadata, PqtyError> {
    let bytes = read_file_bounded(cache_path, MAX_TLPDB_EXPANDED_BYTES)?;
    let actual = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    if let Some(metadata) = read_registry_cache_metadata(metadata_path)? {
        if metadata.url != url || metadata.metadata_digest != actual {
            return Err(PqtyError::Usage(format!(
                "cached Registry Snapshot metadata is inconsistent at {}\n  recorded: {}\n  actual:   {actual}",
                cache_path.display(),
                metadata.metadata_digest
            )));
        }
        return Ok(metadata);
    }

    let metadata = CachedRegistryMetadata {
        schema: REGISTRY_CACHE_SCHEMA.to_string(),
        url: url.to_string(),
        metadata_digest: actual,
        etag: None,
        last_modified: None,
    };
    let mut encoded = serde_json::to_vec_pretty(&metadata)?;
    encoded.push(b'\n');
    atomic_write(metadata_path, &encoded)?;
    Ok(metadata)
}

pub(super) fn publish_tlpdb_cache(
    url: &str,
    cache_path: &Path,
    metadata_path: &Path,
    bytes: &[u8],
    etag: Option<String>,
    last_modified: Option<String>,
) -> Result<(), PqtyError> {
    atomic_write(cache_path, bytes)?;
    let metadata = CachedRegistryMetadata {
        schema: REGISTRY_CACHE_SCHEMA.to_string(),
        url: url.to_string(),
        metadata_digest: format!("sha256:{}", hex::encode(Sha256::digest(bytes))),
        etag,
        last_modified,
    };
    let mut encoded = serde_json::to_vec_pretty(&metadata)?;
    encoded.push(b'\n');
    atomic_write(metadata_path, &encoded)
}

/// Download and cache a Registry Snapshot's tlpdb.
///
/// Rolling metadata is conditionally revalidated, dated metadata is immutable,
/// and offline access requires an existing verified cache entry.
pub(crate) fn fetch_tlpdb(
    url: &str,
    cache_dir: &Path,
    policy: MetadataCachePolicy,
    allow_insecure: bool,
) -> Result<PathBuf, PqtyError> {
    validate_remote_url(url, allow_insecure)?;
    let (cache_path, metadata_path) = registry_cache_paths(url, cache_dir);
    let cached = cache_path.is_file();
    if cached && policy != MetadataCachePolicy::Revalidate {
        verify_cached_tlpdb(url, &cache_path, &metadata_path)?;
        return Ok(cache_path);
    }
    if policy == MetadataCachePolicy::Offline {
        return Err(PqtyError::Usage(format!(
            "offline mode requires cached Registry Snapshot metadata for {url}"
        )));
    }
    fs::create_dir_all(cache_dir).map_err(|source| PqtyError::Io {
        path: cache_dir.to_path_buf(),
        source,
    })?;

    let cached_metadata = read_registry_cache_metadata(&metadata_path)?;
    let etag = cached
        .then(|| {
            cached_metadata
                .as_ref()
                .and_then(|metadata| metadata.etag.as_deref())
        })
        .flatten();
    let last_modified = cached
        .then(|| {
            cached_metadata
                .as_ref()
                .and_then(|metadata| metadata.last_modified.as_deref())
        })
        .flatten();
    let response = match get_with_safe_redirects(url, allow_insecure, etag, last_modified)? {
        HttpGetOutcome::NotModified => {
            verify_cached_tlpdb(url, &cache_path, &metadata_path)?;
            return Ok(cache_path);
        }
        HttpGetOutcome::Response(response) => *response,
    };
    let content_length = response_header(&response, &ureq::http::header::CONTENT_LENGTH)
        .and_then(|length| length.parse::<u64>().ok());
    if content_length.is_some_and(|length| length > MAX_TLPDB_COMPRESSED_BYTES) {
        return Err(PqtyError::Usage(format!(
            "registry response declares {} bytes; limit is {MAX_TLPDB_COMPRESSED_BYTES}",
            content_length.expect("checked present length")
        )));
    }
    progress::download_plan(
        progress::DownloadCategory::Registry,
        1,
        0,
        content_length,
        content_length.map(|_| 0),
    );
    progress::download_start(
        progress::DownloadCategory::Registry,
        "texlive.tlpdb",
        url,
        1,
        content_length,
    );
    let etag = response_header(&response, &ureq::http::header::ETAG).map(ToString::to_string);
    let last_modified =
        response_header(&response, &ureq::http::header::LAST_MODIFIED).map(ToString::to_string);
    let started = Instant::now();
    let compressed = read_bounded_download(
        response.into_body().into_reader(),
        MAX_TLPDB_COMPRESSED_BYTES,
        url,
        progress::DownloadCategory::Registry,
        "texlive.tlpdb",
        content_length,
        started,
    )?;
    progress::download_complete(
        progress::DownloadCategory::Registry,
        "texlive.tlpdb",
        compressed.len() as u64,
        content_length,
        duration_millis(started.elapsed()),
    );

    let bytes = if Path::new(url)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xz"))
    {
        decompress_xz_bounded(&compressed, MAX_TLPDB_EXPANDED_BYTES, url)?
    } else {
        compressed
    };

    publish_tlpdb_cache(
        url,
        &cache_path,
        &metadata_path,
        &bytes,
        etag,
        last_modified,
    )?;
    Ok(cache_path)
}

pub(super) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut divisor = 1_u64;
    let mut unit = 0_usize;
    while bytes / divisor >= 1024 && unit < UNITS.len() - 1 {
        divisor *= 1024;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        let rounded_tenths =
            (u128::from(bytes) * 10 + u128::from(divisor / 2)) / u128::from(divisor);
        format!(
            "{}.{} {}",
            rounded_tenths / 10,
            rounded_tenths % 10,
            UNITS[unit]
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::default_cache_dir_from;

    #[test]
    fn windows_cache_uses_local_app_data_without_home() {
        let variables = BTreeMap::from([(
            "LOCALAPPDATA",
            OsString::from(r"C:\Users\Ada\AppData\Local"),
        )]);

        assert_eq!(
            default_cache_dir_from(|name| variables.get(name).cloned(), true),
            PathBuf::from(r"C:\Users\Ada\AppData\Local").join("pqty")
        );
    }

    #[test]
    fn explicit_xdg_cache_wins_on_every_platform() {
        let variables = BTreeMap::from([("XDG_CACHE_HOME", OsString::from("/managed/texe"))]);

        assert_eq!(
            default_cache_dir_from(|name| variables.get(name).cloned(), true),
            PathBuf::from("/managed/texe/pqty")
        );
    }
}
