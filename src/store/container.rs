use crate::store::DOWNLOAD_PROGRESS_INTERVAL;
use crate::store::archive::{container_expanded_limit, extract_required_container};
use crate::store::http::{
    DEFAULT_TLNET_CONTAINER_FALLBACKS, FetchFailure, HttpGetOutcome,
    get_with_safe_redirects_classified, response_header, transient_io, validate_remote_url,
};

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256, Sha512};

use crate::{
    DEFAULT_TLNET, MAX_CONTAINER_COMPRESSED_BYTES, PqtyError, progress, publish_file,
    temporary_file, validate_provider_identifier,
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
    // Three attempts per source, with a bounded 250ms/1s backoff. Retry only
    // classified transport failures; policy/integrity/local-I/O errors stop here.
    let backoff = [Duration::from_millis(250), Duration::from_secs(1)];
    let mut errors = Vec::new();
    let mut attempt = 0;
    for candidate in urls {
        for retry in 0..=backoff.len() {
            attempt += 1;
            progress::download_start(
                progress::DownloadCategory::Packages,
                provider,
                candidate,
                attempt,
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
                Err(failure) if !failure.retryable => return Err(failure.error),
                Err(failure) => errors.push(format!(
                    "{candidate} (attempt {attempt}): {}",
                    failure.error
                )),
            }
            if let Some(delay) = backoff.get(retry) {
                std::thread::sleep(*delay);
            }
        }
    }
    Err(PqtyError::Usage(format!(
        "container fetch failed for {provider} across {} checked source(s), after {attempt} attempt(s):\n  {}",
        urls.len(),
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
) -> Result<(), FetchFailure> {
    let started = Instant::now();
    let response = match get_with_safe_redirects_classified(url, allow_insecure, None, None)? {
        HttpGetOutcome::Response(response) => *response,
        HttpGetOutcome::NotModified => {
            return Err(PqtyError::Usage(format!(
                "fetch {url}: unexpected HTTP 304 without a cached container"
            ))
            .into());
        }
    };
    if response_header(&response, &ureq::http::header::CONTENT_LENGTH)
        .and_then(|length| length.parse::<u64>().ok())
        .is_some_and(|length| length > compressed_limit)
    {
        return Err(PqtyError::Usage(format!(
            "container {provider} response exceeds limit {compressed_limit}"
        ))
        .into());
    }
    let mut file = temporary_file(cache_path, "download")?;
    let mut reader = TransportReader {
        inner: response.into_body().into_reader(),
        retryable: false,
    };
    copy_container(
        &mut reader,
        &mut file,
        provider,
        expected_sha512,
        expected_size,
        true,
    )
    .map_err(|error| FetchFailure {
        error,
        retryable: reader.retryable,
    })?;
    publish_file(file, cache_path)?;
    progress::download_complete(
        progress::DownloadCategory::Packages,
        provider,
        expected_size,
        Some(expected_size),
        duration_millis(started.elapsed()),
    );
    Ok(())
}
// Only errors produced by reading the HTTP body can be transport retries.
// Disk writes/publication failures and checksum/size failures stay terminal.
struct TransportReader<R> {
    inner: R,
    retryable: bool,
}
impl<R: Read> Read for TransportReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buffer).inspect_err(|error| {
            self.retryable = transient_io(error);
        })
    }
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
    fn serve_sequence(
        responses: Vec<Vec<u8>>,
        cache: PathBuf,
    ) -> (String, thread::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let task = thread::spawn(move || {
            let count = responses.len();
            for response in responses {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = [0_u8; 4096];
                assert!(socket.read(&mut request).unwrap() > 0);
                // A prior interrupted attempt must have removed its staging file
                // and kept the existing cache bytes before the next GET starts.
                assert_eq!(fs::read(&cache).unwrap(), b"previous verified cache");
                assert_eq!(fs::read_dir(cache.parent().unwrap()).unwrap().count(), 1);
                socket.write_all(&response).unwrap();
            }
            // Leave the listener alive until all expected responses have been
            // consumed; an accidental extra attempt cannot be counted as success.
            count
        });
        (format!("http://{address}/container.tar.xz"), task)
    }

    #[test]
    fn transient_status_and_body_disconnect_retry_with_clean_staging() {
        let root = crate::tests::temporary_test_root("download-retry");
        fs::create_dir_all(&root).unwrap();
        let output = root.join("container.tar.xz");
        fs::write(&output, b"previous verified cache").unwrap();
        let bytes = b"complete verified container";
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len()
        );
        let (url, task) = serve_sequence(vec![
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            [header.as_bytes(), &bytes[..5]].concat(),
            [header.as_bytes(), bytes].concat(),
        ], output.clone());
        fetch_container_from_candidates(
            &[url],
            "fixture",
            &hex::encode(Sha512::digest(bytes)),
            bytes.len() as u64,
            bytes.len() as u64,
            true,
            &output,
        )
        .unwrap();
        assert_eq!(task.join().unwrap(), 3);
        assert_eq!(fs::read(&output).unwrap(), bytes);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn transient_retries_stop_after_three_attempts_without_replacing_cache() {
        let root = crate::tests::temporary_test_root("download-retry-limit");
        fs::create_dir_all(&root).unwrap();
        let output = root.join("container.tar.xz");
        fs::write(&output, b"previous verified cache").unwrap();
        let response =
            b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec();
        let (url, task) = serve_sequence(vec![response; 3], output.clone());
        let error = fetch_container_from_candidates(
            &[url],
            "fixture",
            &"00".repeat(64),
            10,
            10,
            true,
            &output,
        )
        .unwrap_err();
        assert!(error.to_string().contains("after 3 attempt(s)"));
        assert_eq!(task.join().unwrap(), 3);
        assert_eq!(fs::read(&output).unwrap(), b"previous verified cache");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn integrity_size_redirect_and_permanent_status_failures_are_terminal() {
        for response in [
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbad".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 999\r\nConnection: close\r\n\r\n".to_vec(),
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            b"HTTP/1.1 302 Found\r\nLocation: ftp://example.invalid/unsafe\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        ] {
            let root = crate::tests::temporary_test_root("download-terminal");
            fs::create_dir_all(&root).unwrap();
            let output = root.join("container.tar.xz");
            fs::write(&output, b"previous verified cache").unwrap();
            let (url, task) = serve_sequence(vec![response], output.clone());
            let failure = fetch_container_from_candidates(
                &[url], "fixture", &hex::encode(Sha512::digest(b"yes")), 3, 3, true, &output,
            ).unwrap_err();
            assert!(!failure.to_string().contains("container fetch failed"), "{failure}");
            assert_eq!(task.join().unwrap(), 1);
            assert_eq!(fs::read(&output).unwrap(), b"previous verified cache");
            assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
            fs::remove_dir_all(root).unwrap();
        }
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
