use crate::store::DOWNLOAD_PROGRESS_INTERVAL;
use crate::store::archive::{
    container_expanded_limit, extract_required_container, validate_container_bytes,
};
use crate::store::http::{
    DEFAULT_TLNET_CONTAINER_FALLBACKS, HttpGetOutcome, get_with_safe_redirects, read_file_bounded,
    response_header, validate_remote_url,
};

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256};

use crate::{
    DEFAULT_TLNET, MAX_CONTAINER_COMPRESSED_BYTES, PqtyError, atomic_write, progress,
    validate_provider_identifier,
};

pub(super) struct ContainerRequest<'a> {
    pub(super) base_url: &'a str,
    pub(super) cache_dir: &'a Path,
    pub(super) provider: &'a str,
    pub(super) expected_sha512: Option<&'a str>,
    pub(super) expected_size: Option<u64>,
    pub(super) offline: bool,
    pub(super) allow_insecure: bool,
    pub(super) runfiles: &'a [String],
}

pub(super) fn fetch_container_runfiles(
    request: &ContainerRequest<'_>,
) -> Result<Vec<(String, Vec<u8>)>, PqtyError> {
    let base_url = request.base_url;
    let cache_dir = request.cache_dir;
    let provider = request.provider;
    let expected_sha512 = request.expected_sha512;
    let expected_size = request.expected_size;
    let offline = request.offline;
    let allow_insecure = request.allow_insecure;
    let runfiles = request.runfiles;
    validate_provider_identifier(provider, "container provider")?;
    validate_remote_url(base_url, allow_insecure)?;
    if expected_sha512.is_none() {
        return Err(PqtyError::Usage(format!(
            "remote container {provider} has no locked SHA-512 checksum and size"
        )));
    }
    if expected_sha512.is_some() != expected_size.is_some() {
        return Err(PqtyError::Usage(format!(
            "container {provider} must declare checksum and size together"
        )));
    }
    if let Some(checksum) = expected_sha512
        && (checksum.len() != 128
            || !checksum
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(PqtyError::Usage(format!(
            "container {provider} has a malformed SHA-512 checksum"
        )));
    }
    let urls = container_urls(base_url, provider);
    let url = &urls[0];
    let cache_key = expected_sha512.map_or_else(
        || hex::encode(Sha256::digest(url.as_bytes())),
        ToString::to_string,
    );
    let cache_path = container_cache_path(cache_dir, provider, &cache_key);

    let compressed_limit = expected_size.unwrap_or(MAX_CONTAINER_COMPRESSED_BYTES);
    if compressed_limit > MAX_CONTAINER_COMPRESSED_BYTES {
        return Err(PqtyError::Usage(format!(
            "container {provider} declares {compressed_limit} bytes; maximum supported size is {MAX_CONTAINER_COMPRESSED_BYTES}"
        )));
    }
    let (compressed, downloaded) = if cache_path.is_file() {
        (read_file_bounded(&cache_path, compressed_limit)?, false)
    } else if offline {
        return Err(PqtyError::Usage(format!(
            "offline mode requires cached container content for provider {provider}"
        )));
    } else {
        fs::create_dir_all(cache_dir).map_err(|source| PqtyError::Io {
            path: cache_dir.to_path_buf(),
            source,
        })?;
        let expected_sha512 = expected_sha512.expect("validated checksum");
        let expected_size = expected_size.expect("validated size");
        (
            fetch_container_from_candidates(
                &urls,
                provider,
                expected_sha512,
                expected_size,
                compressed_limit,
                allow_insecure,
            )?,
            true,
        )
    };

    validate_container_bytes(
        provider,
        &compressed,
        expected_sha512.expect("validated checksum"),
        expected_size.expect("validated size"),
    )?;
    if downloaded {
        atomic_write(&cache_path, &compressed)?;
    }

    extract_required_container(
        &compressed,
        &cache_path,
        provider,
        runfiles,
        container_expanded_limit(compressed.len() as u64),
    )
}

fn fetch_container_from_candidates(
    urls: &[String],
    provider: &str,
    expected_sha512: &str,
    expected_size: u64,
    compressed_limit: u64,
    allow_insecure: bool,
) -> Result<Vec<u8>, PqtyError> {
    let mut errors = Vec::new();
    for (attempt, candidate) in urls.iter().enumerate() {
        progress::download_start(
            progress::DownloadCategory::Packages,
            provider,
            candidate,
            attempt + 1,
            Some(expected_size),
        );
        match fetch_verified_container(
            candidate,
            provider,
            expected_sha512,
            expected_size,
            compressed_limit,
            allow_insecure,
        ) {
            Ok(bytes) => return Ok(bytes),
            Err(error) => errors.push(format!("{candidate}: {error}")),
        }
    }
    Err(PqtyError::Usage(format!(
        "container fetch failed for {provider} across {} checked source(s):\n  {}",
        errors.len(),
        errors.join("\n  ")
    )))
}

pub(super) fn container_urls(base_url: &str, provider: &str) -> Vec<String> {
    let base = base_url.trim_end_matches('/');
    let mut bases = vec![base];
    if base == DEFAULT_TLNET {
        bases.extend(DEFAULT_TLNET_CONTAINER_FALLBACKS);
    }
    bases
        .into_iter()
        .map(|base| format!("{base}/archive/{provider}.tar.xz"))
        .collect()
}

fn fetch_verified_container(
    url: &str,
    provider: &str,
    expected_sha512: &str,
    expected_size: u64,
    compressed_limit: u64,
    allow_insecure: bool,
) -> Result<Vec<u8>, PqtyError> {
    let started = Instant::now();
    let response = match get_with_safe_redirects(url, allow_insecure, None, None)? {
        HttpGetOutcome::Response(response) => *response,
        HttpGetOutcome::NotModified => {
            return Err(PqtyError::Usage(format!(
                "fetch {url}: unexpected HTTP 304 without a cached container"
            )));
        }
    };
    if let Some(length) = response_header(&response, &ureq::http::header::CONTENT_LENGTH)
        .and_then(|length| length.parse::<u64>().ok())
        && length > compressed_limit
    {
        return Err(PqtyError::Usage(format!(
            "container {provider} response declares {length} bytes; limit is {compressed_limit}"
        )));
    }
    let bytes = read_bounded_download(
        response.into_body().into_reader(),
        compressed_limit,
        url,
        progress::DownloadCategory::Packages,
        provider,
        Some(expected_size),
        started,
    )?;
    validate_container_bytes(provider, &bytes, expected_sha512, expected_size)?;
    progress::download_complete(
        progress::DownloadCategory::Packages,
        provider,
        bytes.len() as u64,
        Some(expected_size),
        duration_millis(started.elapsed()),
    );
    Ok(bytes)
}

pub(super) fn container_cache_path(cache_dir: &Path, provider: &str, cache_key: &str) -> PathBuf {
    cache_dir.join(format!(
        "{provider}.{}.tar.xz",
        &cache_key[..cache_key.len().min(16)]
    ))
}

pub(super) fn read_bounded_download(
    mut reader: impl Read,
    limit: u64,
    description: &str,
    category: progress::DownloadCategory,
    item: &str,
    bytes_total: Option<u64>,
    started: Instant,
) -> Result<Vec<u8>, PqtyError> {
    let capacity = bytes_total
        .and_then(|bytes| usize::try_from(bytes).ok())
        .unwrap_or_default();
    let mut bytes = Vec::with_capacity(capacity);
    let mut buffer = [0_u8; 16 * 1024];
    let mut last_emitted = Instant::now();

    loop {
        let remaining = limit.saturating_add(1).saturating_sub(bytes.len() as u64);
        if remaining == 0 {
            break;
        }
        let read_limit = usize::try_from(remaining.min(buffer.len() as u64))
            .expect("read limit is bounded by the fixed buffer");
        let read = reader
            .read(&mut buffer[..read_limit])
            .map_err(|source| PqtyError::Io {
                path: PathBuf::from(description),
                source,
            })?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() as u64 > limit {
            return Err(PqtyError::Usage(format!(
                "{description} exceeded the {limit}-byte limit"
            )));
        }
        if last_emitted.elapsed() >= DOWNLOAD_PROGRESS_INTERVAL {
            progress::download_progress(
                category,
                item,
                bytes.len() as u64,
                bytes_total,
                duration_millis(started.elapsed()),
            );
            last_emitted = Instant::now();
        }
    }
    Ok(bytes)
}

pub(super) fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
