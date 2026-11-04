use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::{HTTP_CONNECT_TIMEOUT, HTTP_READ_TIMEOUT, HTTP_REQUEST_TIMEOUT, PqtyError};

/// The dated TeX Live archive admits standard download-client identities
/// without a browser challenge. Keep pqty identifiable while retaining the
/// compatibility product that the archive expects from automated downloads.
const HTTP_USER_AGENT: &str = concat!(
    "Wget/1.21.4 (compatible; pqty/",
    env!("CARGO_PKG_VERSION"),
    "; +https://github.com/backmatter/pqty)"
);

fn http_agent() -> &'static ureq::Agent {
    // The Agent owns bounded connection pooling. URL/redirect policy remains
    // request-local; no credentials or cookie storage are configured here.
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .user_agent(HTTP_USER_AGENT)
            .max_redirects(0)
            .timeout_global(Some(HTTP_REQUEST_TIMEOUT))
            .timeout_connect(Some(HTTP_CONNECT_TIMEOUT))
            .timeout_recv_body(Some(HTTP_READ_TIMEOUT))
            .build()
            .into()
    })
}

const MAX_HTTP_REDIRECTS: usize = 5;
pub(super) const DEFAULT_TLNET_CONTAINER_FALLBACKS: &[&str] = &[
    "https://ftp.fau.de/ctan/systems/texlive/tlnet",
    "https://ctan.math.illinois.edu/systems/texlive/tlnet",
];

pub(super) fn validate_remote_url(url: &str, allow_insecure: bool) -> Result<(), PqtyError> {
    if url.is_empty()
        || url.chars().any(char::is_control)
        || url.contains(['\\', '#'])
        || url.contains(char::is_whitespace)
    {
        return Err(PqtyError::Usage(format!(
            "registry URL is malformed or unsafe: {url}"
        )));
    }
    let parsed = url::Url::parse(url)
        .map_err(|error| PqtyError::Usage(format!("registry URL is malformed: {url}: {error}")))?;
    match parsed.scheme() {
        "https" => {}
        "http" if allow_insecure => {}
        "http" => {
            return Err(PqtyError::Usage(format!(
                "registry URL requires HTTPS; pass --allow-insecure-registry to use {url}"
            )));
        }
        _ => {
            return Err(PqtyError::Usage(format!(
                "remote registry URL must use HTTPS: {url}"
            )));
        }
    }
    if parsed.host_str().is_none() || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(PqtyError::Usage(format!(
            "registry URL has an empty host or embedded credentials: {url}"
        )));
    }
    if parsed.fragment().is_some() {
        return Err(PqtyError::Usage(format!(
            "registry URL must not contain a fragment: {url}"
        )));
    }
    Ok(())
}

pub(super) enum HttpGetOutcome {
    NotModified,
    Response(Box<ureq::http::Response<ureq::Body>>),
}

pub(super) fn response_header<'a>(
    response: &'a ureq::http::Response<ureq::Body>,
    name: &ureq::http::HeaderName,
) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

pub(super) fn safe_redirect_target(
    current: &url::Url,
    location: &str,
    allow_insecure: bool,
) -> Result<url::Url, PqtyError> {
    let next = current.join(location).map_err(|error| {
        PqtyError::Usage(format!(
            "fetch {current}: unsafe redirect target {location}: {error}"
        ))
    })?;
    validate_remote_url(next.as_str(), allow_insecure)?;
    if current.scheme() == "https" && next.scheme() != "https" {
        return Err(PqtyError::Usage(format!(
            "fetch {current}: refusing HTTPS downgrade redirect to {next}"
        )));
    }
    Ok(next)
}

pub(super) fn get_with_safe_redirects(
    url: &str,
    allow_insecure: bool,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<HttpGetOutcome, PqtyError> {
    get_with_safe_redirects_classified(url, allow_insecure, etag, last_modified)
        .map_err(|failure| failure.error)
}

#[derive(Debug)]
pub(super) struct FetchFailure {
    pub(super) error: PqtyError,
    pub(super) retryable: bool,
}

impl From<PqtyError> for FetchFailure {
    fn from(error: PqtyError) -> Self {
        Self {
            error,
            retryable: false,
        }
    }
}

impl std::fmt::Display for FetchFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

pub(super) fn transient_io(error: &std::io::Error) -> bool {
    if let Some(error) = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<ureq::Error>())
    {
        return transient_http(error);
    }
    matches!(
        error.kind(),
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::NotConnected
    )
}

fn transient_http(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::Io(error) => transient_io(error),
        ureq::Error::Timeout(_) | ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => true,
        ureq::Error::StatusCode(status) => matches!(*status, 408 | 429 | 500 | 502 | 503 | 504),
        _ => false, // TLS, URI, protocol, redirect and policy failures are terminal.
    }
}

pub(super) fn get_with_safe_redirects_classified(
    url: &str,
    allow_insecure: bool,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<HttpGetOutcome, FetchFailure> {
    let mut retryable = false;
    get_with_safe_redirects_inner(url, allow_insecure, etag, last_modified, &mut retryable)
        .map_err(|error| FetchFailure { error, retryable })
}

fn get_with_safe_redirects_inner(
    url: &str,
    allow_insecure: bool,
    etag: Option<&str>,
    last_modified: Option<&str>,
    retryable: &mut bool,
) -> Result<HttpGetOutcome, PqtyError> {
    validate_remote_url(url, allow_insecure)?;
    let agent = http_agent();
    let mut current = url::Url::parse(url)
        .map_err(|error| PqtyError::Usage(format!("registry URL is malformed: {url}: {error}")))?;

    for redirect_count in 0..=MAX_HTTP_REDIRECTS {
        let mut request = agent.get(current.as_str());
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
        }
        if let Some(last_modified) = last_modified {
            request = request.header("If-Modified-Since", last_modified);
        }
        let response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(status))
                if status == ureq::http::StatusCode::NOT_MODIFIED =>
            {
                return Ok(HttpGetOutcome::NotModified);
            }
            Err(ureq::Error::StatusCode(status)) => {
                *retryable = matches!(status, 408 | 429 | 500 | 502 | 503 | 504);
                return Err(PqtyError::Usage(format!(
                    "fetch {current}: HTTP status {status}"
                )));
            }
            Err(error) => {
                *retryable = transient_http(&error);
                return Err(PqtyError::Usage(format!("fetch {current}: {error}")));
            }
        };

        match response.status().as_u16() {
            304 => return Ok(HttpGetOutcome::NotModified),
            301 | 302 | 303 | 307 | 308 => {
                if redirect_count == MAX_HTTP_REDIRECTS {
                    return Err(PqtyError::Usage(format!(
                        "fetch {url}: exceeded {MAX_HTTP_REDIRECTS} redirects"
                    )));
                }
                let location = response_header(&response, &ureq::http::header::LOCATION)
                    .ok_or_else(|| {
                        PqtyError::Usage(format!(
                            "fetch {current}: redirect response has no valid Location header"
                        ))
                    })?;
                current = safe_redirect_target(&current, location, allow_insecure)?;
            }
            status if (300..400).contains(&status) => {
                return Err(PqtyError::Usage(format!(
                    "fetch {current}: unsupported redirect status {status}"
                )));
            }
            _ => return Ok(HttpGetOutcome::Response(Box::new(response))),
        }
    }

    unreachable!("bounded redirect loop always returns")
}

pub(crate) fn read_bounded(
    reader: impl Read,
    limit: u64,
    description: &str,
) -> Result<Vec<u8>, PqtyError> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| PqtyError::Io {
            path: PathBuf::from(description),
            source,
        })?;
    if bytes.len() as u64 > limit {
        return Err(PqtyError::Usage(format!(
            "{description} exceeded the {limit}-byte limit"
        )));
    }
    Ok(bytes)
}

pub(super) fn read_file_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, PqtyError> {
    let metadata = fs::metadata(path).map_err(|source| PqtyError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > limit {
        return Err(PqtyError::Usage(format!(
            "cached artifact {} is {} bytes; limit is {limit}",
            path.display(),
            metadata.len()
        )));
    }
    let file = fs::File::open(path).map_err(|source| PqtyError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    read_bounded(file, limit, &path.display().to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn retry_classification_uses_typed_transport_errors_only() {
        use std::io::{Error, ErrorKind};
        assert!(super::transient_io(&Error::new(
            ErrorKind::UnexpectedEof,
            "peer disconnected"
        )));
        assert!(super::transient_io(&Error::new(
            ErrorKind::TimedOut,
            "deadline"
        )));
        assert!(!super::transient_io(&Error::new(
            ErrorKind::InvalidData,
            "Peer disconnected"
        )));
        assert!(!super::transient_http(&ureq::Error::Tls(
            "invalid certificate"
        )));
        assert!(!super::transient_http(&ureq::Error::StatusCode(403)));
        assert!(!super::transient_http(&ureq::Error::StatusCode(404)));
        assert!(super::transient_http(&ureq::Error::StatusCode(503)));
        let local_failure = super::FetchFailure::from(crate::PqtyError::Io {
            path: "disk".into(),
            source: Error::new(ErrorKind::TimedOut, "local write"),
        });
        assert!(!local_failure.retryable);
    }

    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
    };

    fn read_request(stream: &mut BufReader<std::net::TcpStream>) -> bool {
        let mut line = String::new();
        loop {
            line.clear();
            if stream.read_line(&mut line).unwrap() == 0 {
                return false;
            }
            if line == "\r\n" {
                return true;
            }
        }
    }

    #[test]
    fn separate_fetches_reuse_a_connection_after_the_body_is_consumed() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/package", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut stream = BufReader::new(socket);
            let mut connections = 1;
            for _ in 0..2 {
                if !read_request(&mut stream) {
                    let (socket, _) = listener.accept().unwrap();
                    socket
                        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                        .unwrap();
                    stream = BufReader::new(socket);
                    connections += 1;
                    assert!(read_request(&mut stream));
                }
                stream
                    .get_mut()
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .unwrap();
            }
            connections
        });
        for _ in 0..2 {
            let HttpGetOutcome::Response(response) =
                get_with_safe_redirects(&url, true, None, None).unwrap()
            else {
                panic!("expected response")
            };
            let mut body = Vec::new();
            response
                .into_body()
                .into_reader()
                .read_to_end(&mut body)
                .unwrap();
            assert_eq!(body, b"ok");
        }
        assert_eq!(server.join().unwrap(), 1);
    }
}
