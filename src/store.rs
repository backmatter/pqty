#[cfg(test)]
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{
    MAX_ARCHIVE_ENTRY_BYTES, MAX_PROVIDER_RUNFILES, PqtyError, TlpdbIndex,
    validate_provider_identifier, validate_tds_path,
};

const DOWNLOAD_PROGRESS_INTERVAL: Duration = Duration::from_millis(500);

// Package content is stored once by SHA-256. Consumer TEXMF trees are
// published from that store using the requested installation mode.

/// Counts and locations produced by hydration or TEXMF materialization.
#[derive(Debug, Clone)]
pub struct MaterializeReport {
    providers: usize,
    files: usize,
    bytes_stored: u64,
    missing: usize,
    store: PathBuf,
    out: Option<PathBuf>,
}

impl MaterializeReport {
    fn new(store: PathBuf, out: Option<PathBuf>) -> Self {
        Self {
            providers: 0,
            files: 0,
            bytes_stored: 0,
            missing: 0,
            store,
            out,
        }
    }

    pub(crate) fn print_action(&self, action: &str) {
        println!(
            "{action} {} providers, {} files ({} new in store)",
            self.providers,
            self.files,
            human_bytes(self.bytes_stored),
        );
        println!("store: {}", self.store.display());
        if let Some(out) = &self.out {
            println!("texmf: {}", out.display());
        }
        if self.missing > 0 {
            println!(
                "warning: {} missing from the byte source",
                count_noun(self.missing, "runfile", "runfiles")
            );
        }
    }

    #[must_use]
    /// Number of providers processed by the operation.
    pub fn providers(&self) -> usize {
        self.providers
    }

    #[must_use]
    /// Number of runtime files processed by the operation.
    pub fn files(&self) -> usize {
        self.files
    }

    #[must_use]
    /// Number of previously absent bytes written to the content store.
    pub fn bytes_stored(&self) -> u64 {
        self.bytes_stored
    }

    #[must_use]
    /// Number of requested runfiles missing from the selected byte source.
    pub fn missing(&self) -> usize {
        self.missing
    }

    #[must_use]
    /// Content-addressed store used by the operation.
    pub fn store(&self) -> &Path {
        &self.store
    }

    #[must_use]
    /// Materialized TEXMF destination, when the operation published one.
    pub fn out(&self) -> Option<&Path> {
        self.out.as_deref()
    }
}

fn count_noun(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

/// Where package bytes come from: a local TeX Live tree, or fetched tlnet
/// containers. Both yield the same `(relative_path, bytes)` set, so the store /
/// integrity logic is identical regardless of source.
#[derive(Debug, Clone)]
pub enum PackageByteSource {
    /// Read package files from an installed TeX Live tree.
    Local {
        /// Directory containing the installed `texmf-dist` tree.
        texmf_root: PathBuf,
    },
    /// Read package files from verified tlnet containers.
    Remote {
        /// tlnet root containing `archive/` and `tlpkg/`.
        base_url: String,
        /// Directory used to cache downloaded containers.
        cache_dir: PathBuf,
        /// Whether network requests are forbidden.
        offline: bool,
        /// Whether an explicitly configured HTTP source is permitted.
        allow_insecure: bool,
    },
}

impl PackageByteSource {
    /// Read runfiles from an installed TeX Live root (the directory containing
    /// `texmf-dist`).
    pub fn local_texlive(texmf_root: impl Into<PathBuf>) -> Self {
        Self::Local {
            texmf_root: texmf_root.into(),
        }
    }

    /// Fetch verified package containers from a tlnet base URL. The URL is the
    /// directory containing `archive/` and `tlpkg/`.
    pub fn tlnet(
        base_url: impl Into<String>,
        cache_dir: impl Into<PathBuf>,
        offline: bool,
    ) -> Self {
        Self::tlnet_with_transport(base_url, cache_dir, offline, false)
    }

    fn tlnet_with_transport(
        base_url: impl Into<String>,
        cache_dir: impl Into<PathBuf>,
        offline: bool,
        allow_insecure: bool,
    ) -> Self {
        Self::Remote {
            base_url: base_url.into(),
            cache_dir: cache_dir.into(),
            offline,
            allow_insecure,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetadataCachePolicy {
    Revalidate,
    Immutable,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegistryRequest {
    pub(crate) url: String,
    pub(crate) snapshot: Option<String>,
    pub(crate) cache_policy: MetadataCachePolicy,
    pub(crate) allow_insecure: bool,
}

impl RegistryRequest {
    pub(crate) fn validate(&self) -> Result<(), PqtyError> {
        validate_remote_url(&self.url, self.allow_insecure)?;
        if tlnet_base_from_tlpdb_url(&self.url).is_none() {
            return Err(PqtyError::Usage(format!(
                "unsupported tlnet metadata URL (expected .../tlpkg/texlive.tlpdb[.xz]): {}",
                self.url
            )));
        }
        Ok(())
    }
}

/// Resolve the byte source: remote when a tlnet URL is given, else the local
/// TeX Live tree the tlpdb came from.
pub(crate) fn byte_source(
    registry: Option<&RegistryRequest>,
    index: &TlpdbIndex,
) -> Result<PackageByteSource, PqtyError> {
    if let Some(registry) = registry {
        registry.validate()?;
        let base = tlnet_base_from_tlpdb_url(&registry.url).ok_or_else(|| {
            PqtyError::Usage(
                "could not derive tlnet base from --tlpdb-url (expected \
                 .../tlpkg/texlive.tlpdb[.xz])"
                    .to_string(),
            )
        })?;
        Ok(PackageByteSource::tlnet_with_transport(
            base,
            default_cache_dir().join("containers"),
            registry.cache_policy == MetadataCachePolicy::Offline,
            registry.allow_insecure,
        ))
    } else {
        let texmf_root = index.texmf_root.clone().ok_or_else(|| {
            PqtyError::Usage(
                "tlpdb location has no TEXMF root; pass --tlpdb-url to fetch remotely".to_string(),
            )
        })?;
        Ok(PackageByteSource::local_texlive(texmf_root))
    }
}

/// Normalize a tlpdb runfile into its installed-source path
/// (`texmf-dist/...`) and portable TDS path. Relocated tlnet packages use a
/// `RELOC/` prefix and store bare TDS paths in their containers. Non-relocated
/// packages and installed tlpdbs use `texmf-dist/`, which some containers retain.
pub(crate) fn normalize_runfile(runfile: &str) -> (String, String) {
    let bare = runfile
        .strip_prefix("RELOC/")
        .or_else(|| runfile.strip_prefix("texmf-dist/"))
        .unwrap_or(runfile);
    (format!("texmf-dist/{bare}"), bare.to_string())
}

#[cfg(test)]
pub(crate) fn take_container_runfile(
    files: &mut BTreeMap<String, Vec<u8>>,
    runfile: &str,
) -> Option<Vec<u8>> {
    let (placement, bare) = normalize_runfile(runfile);
    files
        .remove(&bare)
        .or_else(|| files.remove(&placement))
        .or_else(|| files.remove(runfile))
}

/// Collect a provider's runfiles as `(placement_path, bytes)` from the byte source.
fn provider_bytes(
    source: &PackageByteSource,
    provider: &str,
    container_checksum: Option<&str>,
    container_size: Option<u64>,
    runfiles: &[String],
) -> Result<Vec<(String, Vec<u8>)>, PqtyError> {
    validate_provider_identifier(provider, "provider")?;
    if runfiles.len() > MAX_PROVIDER_RUNFILES {
        return Err(PqtyError::Usage(format!(
            "provider {provider} exceeds the {MAX_PROVIDER_RUNFILES}-runfile limit"
        )));
    }
    for runfile in runfiles {
        validate_tds_path(runfile, "registry runfile")?;
    }
    let mut out = Vec::new();
    match source {
        PackageByteSource::Local { texmf_root } => {
            for runfile in runfiles {
                let (placement, _bare) = normalize_runfile(runfile);
                let path = texmf_root.join(&placement);
                match read_file_bounded(&path, MAX_ARCHIVE_ENTRY_BYTES) {
                    Ok(bytes) => out.push((placement, bytes)),
                    Err(PqtyError::Io { source, .. })
                        if source.kind() == io::ErrorKind::NotFound =>
                    {
                        // Distribution packages can legitimately omit
                        // documentation, generated aggregates, or other
                        // optional files still listed by their installed
                        // tlpdb. A local Exact Lock records the bytes that are
                        // actually present. Validation below still rejects a
                        // direct class/package/style request whose owning file
                        // is absent.
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        PackageByteSource::Remote {
            base_url,
            cache_dir,
            offline,
            allow_insecure,
        } => {
            let files = fetch_container_runfiles(&ContainerRequest {
                base_url,
                cache_dir,
                provider,
                expected_sha512: container_checksum,
                expected_size: container_size,
                offline: *offline,
                allow_insecure: *allow_insecure,
                runfiles,
            })?;
            out.extend(files);
        }
    }
    Ok(out)
}

mod archive;
mod container;
mod http;
mod hydrate;
mod install;
mod object;
mod registry_cache;

use container::{ContainerRequest, fetch_container_runfiles};
use http::{read_file_bounded, validate_remote_url};
use registry_cache::{default_cache_dir, human_bytes};

#[cfg(test)]
pub(crate) use archive::BoundedWriter;
pub(crate) use http::read_bounded;
pub(crate) use hydrate::add_trace_providers;
pub use hydrate::hydrate_lock;
pub(crate) use install::install_locked_with_policy;
pub use install::{install_locked, materialize_from_store};
pub(crate) use registry_cache::{
    default_store_dir, fetch_tlpdb, load_convergence_index, load_tlpdb_index,
    tlnet_base_from_tlpdb_url, validate_tlpdb_digest,
};

#[cfg(test)]
mod tests {
    use crate::store::archive::{decompress_xz_bounded, extract_required_tar};
    use crate::store::container::container_urls;
    use crate::store::http::{
        DEFAULT_TLNET_CONTAINER_FALLBACKS, safe_redirect_target, validate_remote_url,
    };
    use crate::store::object::{
        STORE_PACKAGE_SCHEMA, StoredFile, StoredPackageManifest, ensure_store_object,
        provider_manifest_digest, read_store_manifest, store_manifest_path, write_store_manifest,
    };
    use crate::store::registry_cache::{
        publish_tlpdb_cache, read_registry_cache_metadata, registry_cache_paths,
    };
    use crate::store::{
        MetadataCachePolicy, PackageByteSource, RegistryRequest, fetch_tlpdb, provider_bytes,
    };
    use crate::tests::temporary_test_root;
    use crate::{DEFAULT_TLNET, MAX_ARCHIVE_ENTRY_BYTES};
    use sha2::{Digest as _, Sha256};
    use std::fs;
    use std::io::Cursor;
    use std::path::Path;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[cfg(unix)]
    fn make_test_writable(path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let metadata = fs::metadata(path).unwrap();
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o200);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(windows)]
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "this Windows-only test helper must clear FILE_ATTRIBUTE_READONLY during cleanup"
    )]
    fn make_test_writable(path: &Path) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn tar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(bytes.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, path, Cursor::new(*bytes))
                .unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn tar_bytes_with_directory(directory: &str, path: &str, bytes: &[u8]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut directory_header = tar::Header::new_gnu();
        directory_header.set_path(directory).unwrap();
        directory_header.set_mode(0o755);
        directory_header.set_size(0);
        directory_header.set_entry_type(tar::EntryType::Directory);
        directory_header.set_cksum();
        builder.append(&directory_header, Cursor::new([])).unwrap();

        let mut file_header = tar::Header::new_gnu();
        file_header.set_path(path).unwrap();
        file_header.set_mode(0o644);
        file_header.set_size(bytes.len() as u64);
        file_header.set_entry_type(tar::EntryType::Regular);
        file_header.set_cksum();
        builder
            .append_data(&mut file_header, path, Cursor::new(bytes))
            .unwrap();
        builder.into_inner().unwrap()
    }

    fn declared_size_tar(path: &str, size: u64) -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        header.set_path(path).unwrap();
        header.set_mode(0o644);
        header.set_size(size);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        let mut bytes = header.as_bytes().to_vec();
        bytes.extend_from_slice(&[0_u8; 1024]);
        bytes
    }

    fn raw_path_tar(path: &[u8]) -> Vec<u8> {
        assert!(path.len() < 100);
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.as_mut_bytes()[..100].fill(0);
        header.as_mut_bytes()[..path.len()].copy_from_slice(path);
        header.set_cksum();
        let mut bytes = header.as_bytes().to_vec();
        bytes.extend_from_slice(&[0_u8; 1024]);
        bytes
    }

    #[test]
    fn immutable_and_offline_registry_cache_reuse_verifies_content() {
        let body = b"name 00texlive.config\ncategory TLCore\ndepend release/2026\n";
        let root = temporary_test_root("registry-cache");
        let url = "https://example.invalid/tlpkg/texlive.tlpdb";
        fs::create_dir_all(&root).unwrap();
        let (cache_path, metadata_path) = registry_cache_paths(url, &root);
        publish_tlpdb_cache(
            url,
            &cache_path,
            &metadata_path,
            body,
            Some("\"snapshot-a\"".to_string()),
            None,
        )
        .unwrap();

        assert_eq!(
            fetch_tlpdb(url, &root, MetadataCachePolicy::Immutable, false).unwrap(),
            cache_path
        );
        assert_eq!(
            fetch_tlpdb(url, &root, MetadataCachePolicy::Offline, false).unwrap(),
            cache_path
        );
        let metadata = read_registry_cache_metadata(&metadata_path)
            .unwrap()
            .unwrap();
        assert_eq!(metadata.etag.as_deref(), Some("\"snapshot-a\""));

        fs::write(&cache_path, b"corrupt").unwrap();
        assert!(
            fetch_tlpdb(url, &root, MetadataCachePolicy::Offline, false)
                .unwrap_err()
                .to_string()
                .contains("inconsistent")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn offline_registry_cache_miss_is_clear() {
        let root = std::env::temp_dir().join(format!(
            "pqty-registry-cache-miss-test-{}",
            std::process::id()
        ));
        let error = fetch_tlpdb(
            "https://example.invalid/tlpkg/texlive.tlpdb",
            &root,
            MetadataCachePolicy::Offline,
            false,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("requires cached Registry Snapshot")
        );
    }

    #[test]
    fn archive_extraction_retains_only_required_entries() {
        let tar = tar_bytes(&[
            ("tex/latex/wanted/wanted.sty", b"wanted"),
            ("tex/latex/unwanted/unwanted.sty", b"unwanted"),
        ]);
        let files = extract_required_tar(
            Cursor::new(tar),
            "wanted",
            &["RELOC/tex/latex/wanted/wanted.sty".to_string()],
        )
        .unwrap();
        assert_eq!(
            files,
            vec![(
                "texmf-dist/tex/latex/wanted/wanted.sty".to_string(),
                b"wanted".to_vec()
            )]
        );
    }

    #[test]
    fn archive_extraction_accepts_portable_directory_entries() {
        let tar = tar_bytes_with_directory(
            "fonts/enc/dvips/example/",
            "fonts/enc/dvips/example/example.enc",
            b"encoding",
        );
        let files = extract_required_tar(
            Cursor::new(tar),
            "example",
            &["fonts/enc/dvips/example/example.enc".to_string()],
        )
        .unwrap();
        assert_eq!(
            files,
            vec![(
                "texmf-dist/fonts/enc/dvips/example/example.enc".to_string(),
                b"encoding".to_vec()
            )]
        );
    }

    #[test]
    fn local_registry_materialization_records_only_installed_runfiles() {
        let root = temporary_test_root("local-subset");
        if root.exists() {
            fs::remove_dir_all(&root).expect("remove stale test root");
        }
        let installed = root.join("texmf-dist/tex/latex/example/example.sty");
        fs::create_dir_all(installed.parent().expect("installed file parent"))
            .expect("installed file directory");
        fs::write(&installed, b"installed").expect("installed runfile");
        let source = PackageByteSource::local_texlive(&root);

        let files = provider_bytes(
            &source,
            "example",
            None,
            None,
            &[
                "RELOC/tex/latex/example/example.sty".to_string(),
                "RELOC/doc/latex/example/optional.pdf".to_string(),
            ],
        )
        .expect("local provider bytes");

        assert_eq!(
            files,
            vec![(
                "texmf-dist/tex/latex/example/example.sty".to_string(),
                b"installed".to_vec()
            )]
        );
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn archive_extraction_rejects_oversized_selected_entries_before_reading() {
        let tar = declared_size_tar("tex/latex/huge/huge.sty", MAX_ARCHIVE_ENTRY_BYTES + 1);
        let error = extract_required_tar(
            Cursor::new(tar),
            "huge",
            &["RELOC/tex/latex/huge/huge.sty".to_string()],
        )
        .unwrap_err();
        assert!(error.to_string().contains("per-entry limit"));
    }

    #[test]
    fn archive_extraction_validates_even_unselected_paths() {
        let tar = raw_path_tar(b"../escape");
        let error = extract_required_tar(
            Cursor::new(tar),
            "safe",
            &["RELOC/tex/latex/safe/safe.sty".to_string()],
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("Portable Path") || error.to_string().contains("archive")
        );
    }

    #[test]
    fn remote_transport_is_https_by_default_and_has_no_credentials() {
        assert!(validate_remote_url("https://example.invalid/tlnet", false).is_ok());
        assert!(validate_remote_url("http://example.invalid/tlnet", false).is_err());
        assert!(validate_remote_url("http://example.invalid/tlnet", true).is_ok());
        assert!(validate_remote_url("https://user@example.invalid/tlnet", false).is_err());
        assert!(validate_remote_url("https://example.invalid/tlnet#fragment", false).is_err());
        assert!(
            RegistryRequest {
                url: "https://example.invalid/metadata.json".to_string(),
                snapshot: None,
                cache_policy: MetadataCachePolicy::Immutable,
                allow_insecure: false,
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn redirect_targets_are_resolved_and_revalidated() {
        let current = url::Url::parse("https://mirror.example/tlnet/tlpkg/index").unwrap();
        let relative = safe_redirect_target(&current, "../archive/package.tar.xz", false).unwrap();
        assert_eq!(
            relative.as_str(),
            "https://mirror.example/tlnet/archive/package.tar.xz"
        );
        assert!(
            safe_redirect_target(
                &current,
                "http://mirror.example/archive/package.tar.xz",
                true
            )
            .is_err()
        );
        assert!(
            safe_redirect_target(
                &current,
                "https://user:secret@mirror.example/archive/package.tar.xz",
                false
            )
            .is_err()
        );
        assert!(safe_redirect_target(&current, "file:///tmp/package.tar.xz", true).is_err());
    }

    #[test]
    fn default_tlnet_containers_have_independent_checked_fallbacks() {
        let urls = container_urls(DEFAULT_TLNET, "fandol");
        assert_eq!(urls.len(), 1 + DEFAULT_TLNET_CONTAINER_FALLBACKS.len());
        assert!(
            urls.iter().all(|url| {
                url.starts_with("https://") && url.ends_with("/archive/fandol.tar.xz")
            })
        );
        assert_eq!(
            container_urls("https://registry.example/tlnet/", "fandol"),
            vec!["https://registry.example/tlnet/archive/fandol.tar.xz"]
        );
    }

    #[test]
    fn truncated_xz_fixture_never_reaches_archive_processing() {
        let error = decompress_xz_bounded(
            include_bytes!("../tests/fixtures/adversarial/truncated.xz"),
            1024,
            "truncated fixture",
        )
        .unwrap_err();
        assert!(error.to_string().contains("xz decompress"));
    }

    #[test]
    fn concurrent_store_publication_accepts_identical_winner() {
        let root = std::env::temp_dir().join(format!(
            "pqty-store-publication-test-{}",
            std::process::id()
        ));
        let content = Arc::new(b"shared immutable content".to_vec());
        let digest = hex::encode(Sha256::digest(content.as_slice()));
        let path = root.join(&digest[..2]).join(&digest);
        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let root = root.clone();
            let path = path.clone();
            let digest = digest.clone();
            let content = Arc::clone(&content);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                ensure_store_object(&root, &path, &content, &digest)
            }));
        }
        let winners = workers
            .into_iter()
            .map(|worker| worker.join().unwrap().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        assert_eq!(fs::read(&path).unwrap(), content.as_slice());
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
        assert!(fs::read_dir(path.parent().unwrap()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("pqty-object")
        }));
        make_test_writable(&path);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_manifest_publication_rejects_conflicts_without_overwrite() {
        let root = std::env::temp_dir().join(format!(
            "pqty-manifest-publication-test-{}",
            std::process::id()
        ));
        let first = StoredPackageManifest {
            schema: STORE_PACKAGE_SCHEMA.to_string(),
            files: vec![StoredFile {
                tds_path: "tex/latex/foo/foo.sty".to_string(),
                digest: format!("sha256:{}", "11".repeat(32)),
            }],
        };
        let package_digest = hex::encode(provider_manifest_digest(&first.files).unwrap());
        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let root = root.clone();
            let package_digest = package_digest.clone();
            let manifest = first.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                write_store_manifest(&root, &package_digest, &manifest)
            }));
        }
        for worker in workers {
            worker.join().unwrap().unwrap();
        }

        let conflicting = StoredPackageManifest {
            schema: STORE_PACKAGE_SCHEMA.to_string(),
            files: vec![StoredFile {
                tds_path: "tex/latex/bar/bar.sty".to_string(),
                digest: format!("sha256:{}", "22".repeat(32)),
            }],
        };
        assert!(
            write_store_manifest(&root, &package_digest, &conflicting)
                .unwrap_err()
                .to_string()
                .contains("conflicting")
        );
        let path = store_manifest_path(&root, &package_digest);
        assert_eq!(
            read_store_manifest(&path).unwrap().files[0].tds_path,
            "tex/latex/foo/foo.sty"
        );
        make_test_writable(&path);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn property_manifest_hash_is_order_invariant_and_path_sensitive() {
        let files = vec![
            StoredFile {
                tds_path: "tex/latex/a/a.sty".to_string(),
                digest: format!("sha256:{}", "11".repeat(32)),
            },
            StoredFile {
                tds_path: "tex/latex/b/b.sty".to_string(),
                digest: format!("sha256:{}", "22".repeat(32)),
            },
            StoredFile {
                tds_path: "fonts/tfm/c.tfm".to_string(),
                digest: format!("sha256:{}", "33".repeat(32)),
            },
        ];
        let expected = provider_manifest_digest(&files).unwrap();
        for rotation in 0..files.len() {
            let mut generated = files.clone();
            generated.rotate_left(rotation);
            assert_eq!(provider_manifest_digest(&generated).unwrap(), expected);
            generated.reverse();
            assert_eq!(provider_manifest_digest(&generated).unwrap(), expected);
        }

        let mut changed = files;
        changed[0].tds_path = "tex/latex/a/other.sty".to_string();
        assert_ne!(provider_manifest_digest(&changed).unwrap(), expected);
    }
}
