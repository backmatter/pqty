use crate::store::DOWNLOAD_PROGRESS_INTERVAL;
use crate::store::archive::{container_expanded_limit, extract_required_container};
use crate::store::http::{
    DEFAULT_TLNET_CONTAINER_FALLBACKS, HttpGetOutcome, get_with_safe_redirects, response_header,
    validate_remote_url,
};

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256, Sha512};

use crate::{
    DEFAULT_TLNET, MAX_CONTAINER_COMPRESSED_BYTES, PqtyError, progress, unique_sibling,
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
    visitor: &mut super::FileVisitor<'_>,
) -> Result<(), PqtyError> {
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
    let checksum = expected_sha512.expect("validated checksum");
    let size = expected_size.expect("validated size");
    if !cache_path.is_file() {
        if offline {
            return Err(PqtyError::Usage(format!(
                "offline mode requires cached container content for provider {provider}"
            )));
        }
        fs::create_dir_all(cache_dir).map_err(|source| PqtyError::Io {
            path: cache_dir.to_owned(),
            source,
        })?;
        fetch_container_from_candidates(
            &urls,
            provider,
            checksum,
            size,
            compressed_limit,
            allow_insecure,
            &cache_path,
        )?;
    }
    // Verify the open handle before decompression, including cached containers.
    let mut compressed = fs::File::open(&cache_path).map_err(|source| PqtyError::Io {
        path: cache_path.clone(),
        source,
    })?;
    copy_container(
        &mut compressed,
        &mut std::io::sink(),
        provider,
        checksum,
        size,
        false,
    )?;
    compressed
        .seek(SeekFrom::Start(0))
        .map_err(|source| PqtyError::Io {
            path: cache_path.clone(),
            source,
        })?;
    extract_required_container(
        std::io::BufReader::new(compressed),
        &cache_path,
        provider,
        runfiles,
        container_expanded_limit(size),
        visitor,
    )
}

#[allow(clippy::too_many_arguments)]
fn fetch_container_from_candidates(
    urls: &[String],
    provider: &str,
    expected_sha512: &str,
    expected_size: u64,
    compressed_limit: u64,
    allow_insecure: bool,
    cache_path: &Path,
) -> Result<(), PqtyError> {
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
            cache_path,
        ) {
            Ok(()) => return Ok(()),
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

#[allow(clippy::too_many_arguments)]
fn fetch_verified_container(
    url: &str,
    provider: &str,
    expected_sha512: &str,
    expected_size: u64,
    compressed_limit: u64,
    allow_insecure: bool,
    cache_path: &Path,
) -> Result<(), PqtyError> {
    let started = Instant::now();
    let response = match get_with_safe_redirects(url, allow_insecure, None, None)? {
        HttpGetOutcome::Response(response) => *response,
        HttpGetOutcome::NotModified => {
            return Err(PqtyError::Usage(format!(
                "fetch {url}: unexpected HTTP 304 without a cached container"
            )));
        }
    };
    if response_header(&response, &ureq::http::header::CONTENT_LENGTH)
        .and_then(|length| length.parse::<u64>().ok())
        .is_some_and(|length| length > compressed_limit)
    {
        return Err(PqtyError::Usage(format!(
            "container {provider} response exceeds limit {compressed_limit}"
        )));
    }
    let temporary = unique_sibling(cache_path, "download")?;
    let result = (|| {
        let mut file = fs::File::options()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| PqtyError::Io {
                path: temporary.clone(),
                source,
            })?;
        copy_container(
            &mut response.into_body().into_reader(),
            &mut file,
            provider,
            expected_sha512,
            expected_size,
            true,
        )?;
        file.sync_all().map_err(|source| PqtyError::Io {
            path: temporary.clone(),
            source,
        })?;
        drop(file);
        fs::rename(&temporary, cache_path).map_err(|source| PqtyError::Io {
            path: cache_path.to_owned(),
            source,
        })?;
        progress::download_complete(
            progress::DownloadCategory::Packages,
            provider,
            expected_size,
            Some(expected_size),
            duration_millis(started.elapsed()),
        );
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result
}

fn copy_container(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    provider: &str,
    checksum: &str,
    expected_size: u64,
    report_progress: bool,
) -> Result<(), PqtyError> {
    let started = Instant::now();
    let mut last = Instant::now();
    let mut hash = Sha512::new();
    let size = super::stream::copy_observed(
        reader,
        writer,
        expected_size,
        Path::new(provider),
        |bytes, size| {
            hash.update(bytes);
            if report_progress && last.elapsed() >= DOWNLOAD_PROGRESS_INTERVAL {
                progress::download_progress(
                    progress::DownloadCategory::Packages,
                    provider,
                    size,
                    Some(expected_size),
                    duration_millis(started.elapsed()),
                );
                last = Instant::now();
            }
        },
    )?;
    if size != expected_size {
        return Err(PqtyError::Usage(format!(
            "container size mismatch for {provider}: expected {expected_size}, got {size}"
        )));
    }
    if hex::encode(hash.finalize()) != checksum {
        return Err(PqtyError::Usage(format!(
            "container checksum mismatch for {provider}"
        )));
    }
    Ok(())
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
        .and_then(|bytes| usize::try_from(bytes.min(limit)).ok())
        .unwrap_or_default();
    let mut bytes = Vec::with_capacity(capacity);
    let mut last_emitted = Instant::now();
    super::stream::copy_observed(
        &mut reader,
        &mut bytes,
        limit,
        Path::new(description),
        |_, size| {
            if last_emitted.elapsed() >= DOWNLOAD_PROGRESS_INTERVAL {
                progress::download_progress(
                    category,
                    item,
                    size,
                    bytes_total,
                    duration_millis(started.elapsed()),
                );
                last_emitted = Instant::now();
            }
        },
    )?;
    Ok(bytes)
}

pub(super) fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread};
    fn serve(bytes: Vec<u8>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let task = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).unwrap();
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            socket.write_all(&bytes).unwrap();
        });
        (format!("http://{address}/container.tar.xz"), task)
    }
    #[test]
    fn downloads_publish_only_complete_verified_containers_and_preserve_previous_cache() {
        let root = crate::tests::temporary_test_root("download-stream");
        fs::create_dir_all(&root).unwrap();
        let output = root.join("container.tar.xz");
        let bytes = b"verified synthetic container bytes";
        let checksum = hex::encode(Sha512::digest(bytes));
        let size = bytes.len() as u64;
        let (url, task) = serve(bytes.to_vec());
        fetch_verified_container(&url, "fixture", &checksum, size, size, true, &output).unwrap();
        task.join().unwrap();
        assert_eq!(fs::read(&output).unwrap(), bytes);
        let broken = vec![b'x'; bytes.len()];
        let (url, task) = serve(broken);
        assert!(
            fetch_verified_container(&url, "fixture", &checksum, size, size, true, &output)
                .unwrap_err()
                .to_string()
                .contains("checksum mismatch")
        );
        task.join().unwrap();
        assert_eq!(fs::read(&output).unwrap(), bytes);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
