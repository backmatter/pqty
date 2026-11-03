//! Project-independent package publication for lazy clients.
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use std::{collections::BTreeSet, fs, io::Write, path::Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::object::{StoredFile, materialize_provider, provider_manifest_digest};
use super::{MaterializeReport, PackageByteSource, file_export::copy_object};
use crate::{
    MAX_ARCHIVE_ENTRY_BYTES, MAX_CONTAINER_EXPANDED_BYTES, MAX_PROVIDER_RUNFILES, ManifestFile,
    PackageRegistry, PqtyError, Registry, atomic_write, normalize_runfile, publish_file,
    resolved_from, resource_kind, temporary_file, usable_parent, validate_provider_identifier,
    validate_tds_path,
};

/// Schema for a verified provider prepared without any project source.
pub const PACKAGE_SCHEMA: &str = "pqty.package/v1";

/// Verified metadata for one provider, suitable for an immutable HTTP catalog.
/// Files contain digests and sizes, never private store paths or application URLs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    /// Artifact schema, currently [`PACKAGE_SCHEMA`].
    pub schema: String,
    /// SHA-256 of the canonical metadata tuple documented in package-serving.md.
    pub fingerprint: String,
    /// Exact registry snapshot used to resolve this provider.
    pub registry: Registry,
    /// Registry provider name.
    pub provider: String,
    /// Snapshot-bound provider revision.
    pub version: String,
    /// Expected checksum of the remote compressed container, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_checksum: Option<String>,
    /// Expected compressed container length, paired with its checksum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_size: Option<u64>,
    /// Declared runtime data dependencies; not downloaded speculatively.
    pub dependencies: Vec<String>,
    /// Concrete font-map fragment names, retaining existing v1 semantics.
    pub font_maps: Vec<String>,
    /// Provider integrity over the sorted path/content-hash manifest.
    pub integrity: String,
    /// Complete file metadata, sorted by normalized TDS path.
    pub files: Vec<PackageFile>,
}

/// One provider-owned file. Ownership is stored once in its package manifest,
/// without repeating provider names or the project lock's path index.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageFile {
    /// Exact normalized path relative to the package TEXMF root.
    pub tds_path: String,
    /// Engine-neutral classification of this file.
    pub kind: crate::ResourceKind,
    /// SHA-256 content digest in lowercase `sha256:<hex>` form.
    pub digest: String,
    /// Verified uncompressed byte count.
    pub size: u64,
}

impl PackageManifest {
    /// Validate all identities, paths, kinds, sizes and content-manifest hashes.
    /// This checks metadata; received bytes must also be checked against their
    /// digest and size. Hosts pin the registry/fingerprint before trusting a feed.
    ///
    /// # Errors
    /// Rejects malformed, incomplete, oversized or altered manifests.
    pub fn validate(&self) -> Result<(), PqtyError> {
        validate_provider_identifier(&self.provider, "provider")?;
        validate_provider_identifier(&self.registry.id, "registry")?;
        if self.schema != PACKAGE_SCHEMA
            || self.files.len() > MAX_PROVIDER_RUNFILES
            || self.registry.url.is_empty()
            || self.version.is_empty()
            || self
                .registry
                .metadata_digest
                .as_deref()
                .is_none_or(|digest| super::install::stored_file_digest(digest).is_err())
            || self.container_checksum.is_some() != self.container_size.is_some()
            || self
                .container_size
                .is_some_and(|size| size == 0 || size > crate::MAX_CONTAINER_COMPRESSED_BYTES)
            || self.container_checksum.as_deref().is_some_and(|digest| {
                digest.len() != 128
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        {
            return Err(invalid("invalid package manifest identity or file count"));
        }
        for dependency in &self.dependencies {
            validate_provider_identifier(dependency, "dependency")?;
        }
        let mut total = 0_u64;
        let mut stored = Vec::with_capacity(self.files.len());
        let mut previous: Option<&str> = None;
        for file in &self.files {
            validate_tds_path(&file.tds_path, "package file")?;
            super::install::stored_file_digest(&file.digest)?;
            if file.kind != resource_kind(&file.tds_path)
                || previous.is_some_and(|path| path >= file.tds_path.as_str())
                || file.size > MAX_ARCHIVE_ENTRY_BYTES
                || file.size > MAX_CONTAINER_EXPANDED_BYTES.saturating_sub(total)
            {
                return Err(invalid("invalid package file metadata or order"));
            }
            previous = Some(&file.tds_path);
            total += file.size;
            stored.push(StoredFile {
                tds_path: file.tds_path.clone(),
                digest: file.digest.clone(),
            });
        }
        if self.integrity
            != format!(
                "sha256-{}",
                BASE64.encode(provider_manifest_digest(&stored)?)
            )
            || self.fingerprint != self.compute_fingerprint()?
        {
            return Err(invalid(
                "package manifest fingerprint or integrity mismatch",
            ));
        }
        Ok(())
    }

    /// Bind a validated manifest to the selected registry and its full provider
    /// metadata. This also rejects crossing snapshots or truncating the file set.
    ///
    /// # Errors
    /// Rejects mismatched registry, provider metadata or declared runtime files.
    pub fn validate_against(&self, index: &impl PackageRegistry) -> Result<(), PqtyError> {
        self.validate()?;
        let expected = resolved_from(index, &self.provider, &index.registry().id);
        let metadata = index
            .package(&self.provider)
            .ok_or_else(|| invalid("unknown package provider"))?;
        let paths = metadata
            .runfiles
            .iter()
            .map(|path| normalize_runfile(path).1)
            .collect::<BTreeSet<_>>();
        if self.registry != index.registry()
            || self.version != expected.version
            || self.container_checksum != metadata.container_checksum
            || self.container_size != metadata.container_size
            || self.dependencies != expected.dependencies
            || self.font_maps != expected.font_maps
            || self
                .files
                .iter()
                .map(|file| file.tds_path.clone())
                .collect::<BTreeSet<_>>()
                != paths
        {
            return Err(invalid(
                "package manifest differs from the selected registry",
            ));
        }
        Ok(())
    }

    fn compute_fingerprint(&self) -> Result<String, PqtyError> {
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
        let mut writer = HashWriter(Sha256::new());
        serde_json::to_writer(
            &mut writer,
            &(
                &self.schema,
                &self.registry,
                &self.provider,
                &self.version,
                &self.container_checksum,
                &self.container_size,
                &self.dependencies,
                &self.font_maps,
                &self.integrity,
                &self.files,
            ),
        )?;
        Ok(format!("sha256:{}", hex::encode(writer.0.finalize())))
    }
}

/// Find the unique provider for an exact TDS path or literal filename.
/// This does not fetch bytes or consult project sources.
///
/// # Errors
/// Rejects unsafe names, missing files and ambiguous ownership.
pub fn provider_for_file(index: &impl PackageRegistry, request: &str) -> Result<String, PqtyError> {
    validate_tds_path(request, "requested package file")?;
    let providers = if request.contains('/') {
        index.providers_of_path(request)
    } else {
        index.providers_of_file(request)
    };
    match providers.as_slice() {
        [provider] => Ok((*provider).into()),
        [] => Err(invalid(format!("no package provides {request}"))),
        _ => Err(invalid(format!(
            "ambiguous package file {request}: {}",
            providers.join(", ")
        ))),
    }
}

/// Prepare one provider in the shared store, without scanning a project or
/// installing a TEXMF tree. Checksummed remote providers reuse their verified
/// cached manifest; their files are verified again when exported. Concurrent
/// preparers coordinate through the existing OS-owned store lock.
///
/// # Errors
/// Returns resolution, integrity, resource-limit, storage or download errors.
pub fn prepare_package(
    index: &impl PackageRegistry,
    provider: &str,
    source: &PackageByteSource,
    store: &Path,
) -> Result<PackageManifest, PqtyError> {
    validate_provider_identifier(provider, "provider")?;
    let metadata = index
        .package(provider)
        .filter(|package| package.belongs_to_package_layer())
        .ok_or_else(|| invalid(format!("unknown runtime provider {provider}")))?;
    let _guard = super::transaction::FileGuard::store(store)?;
    let registry = index.registry();
    let key = hex::encode(Sha256::digest(serde_json::to_vec(&(&registry, provider))?));
    let cached = store.join("prepared").join(format!("{key}.json"));
    // A remote checksum fixes content independently of ambient local files.
    if matches!(source, PackageByteSource::Remote { .. })
        && metadata.container_checksum.is_some()
        && let Some(manifest) = read_cached(&cached)?
    {
        manifest.validate_against(index)?;
        if manifest.files.iter().all(|file| {
            file.digest
                .strip_prefix("sha256:")
                .is_some_and(|digest| store.join(&digest[..2]).join(digest).is_file())
        }) {
            return Ok(manifest);
        }
    }
    let mut report = MaterializeReport::new(store.to_owned(), None);
    let materialized = materialize_provider(
        index,
        provider,
        metadata.container_checksum.as_deref(),
        metadata.container_size,
        source,
        store,
        &mut report,
    )?;
    if report.missing > 0 {
        return Err(invalid("provider is missing registry-declared files"));
    }
    let expected = resolved_from(index, provider, &registry.id);
    let files = materialized
        .files
        .into_iter()
        .map(|file| PackageFile {
            kind: file.kind,
            tds_path: file.tds_path,
            digest: file.digest,
            size: file.size,
        })
        .collect();
    let mut manifest = PackageManifest {
        schema: PACKAGE_SCHEMA.into(),
        fingerprint: String::new(),
        registry,
        provider: provider.into(),
        version: expected.version,
        container_checksum: metadata.container_checksum.clone(),
        container_size: metadata.container_size,
        dependencies: expected.dependencies,
        font_maps: expected.font_maps,
        integrity: materialized.integrity,
        files,
    };
    manifest.fingerprint = manifest.compute_fingerprint()?;
    manifest.validate_against(index)?;
    fs::create_dir_all(usable_parent(&cached)).map_err(|source| PqtyError::Io {
        path: cached.clone(),
        source,
    })?;
    atomic_write(&cached, &serde_json::to_vec(&manifest)?)?;
    Ok(manifest)
}

fn read_cached(path: &Path) -> Result<Option<PackageManifest>, PqtyError> {
    match fs::File::open(path) {
        Ok(file) => {
            let bytes = super::http::read_bounded(
                file,
                crate::MAX_TLPDB_EXPANDED_BYTES,
                "cached package manifest",
            )?;
            Ok(Some(serde_json::from_slice(&bytes)?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(PqtyError::Io {
            path: path.into(),
            source,
        }),
    }
}

/// Copy one package file offline to a host-owned writer. Bytes are verified
/// before they reach the writer. The host commits its response only on success.
///
/// # Errors
/// Rejects invalid manifests, unknown paths, corrupt bytes and I/O failures.
pub fn copy_package_file(
    manifest: &PackageManifest,
    store: &Path,
    path: &str,
    writer: &mut impl Write,
) -> Result<ManifestFile, PqtyError> {
    copy_file(manifest, store, path, writer, true)
}

fn copy_file(
    manifest: &PackageManifest,
    store: &Path,
    path: &str,
    writer: &mut impl Write,
    preverify: bool,
) -> Result<ManifestFile, PqtyError> {
    manifest.validate()?;
    let file = manifest
        .files
        .iter()
        .find(|file| file.tds_path == path)
        .ok_or_else(|| invalid(format!("file is absent from the package manifest: {path}")))?;
    if preverify && copy_object(store, &file.digest, &mut std::io::sink(), false)? != file.size {
        return Err(invalid("package file size mismatch"));
    }
    if copy_object(store, &file.digest, writer, false)? != file.size {
        return Err(invalid("package file size mismatch"));
    }
    Ok(ManifestFile {
        tds_path: file.tds_path.clone(),
        owner: manifest.provider.clone(),
        kind: file.kind.clone(),
        digest: file.digest.clone(),
        size: file.size,
    })
}

/// Atomically export one package file for immutable hosting. This performs no
/// network I/O and never publishes incomplete or unverified output.
///
/// # Errors
/// Rejects store/output overlap and errors from [`copy_package_file`].
pub fn export_package_file(
    manifest: &PackageManifest,
    store: &Path,
    path: &str,
    output: &Path,
) -> Result<ManifestFile, PqtyError> {
    let parent = fs::canonicalize(usable_parent(output)).map_err(|source| PqtyError::Io {
        path: output.into(),
        source,
    })?;
    let root = fs::canonicalize(store).map_err(|source| PqtyError::Io {
        path: store.into(),
        source,
    })?;
    if parent.starts_with(root) {
        return Err(invalid("package export output must be outside the store"));
    }
    let mut file = temporary_file(output, "package-file")?;
    let metadata = copy_file(manifest, store, path, &mut file, false)?;
    publish_file(file, output)?;
    Ok(metadata)
}

fn invalid(message: impl Into<String>) -> PqtyError {
    PqtyError::Usage(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TlpdbIndex;
    use std::{
        io::{Cursor, Read as _},
        net::TcpListener,
        thread,
        time::Duration,
    };

    fn archive() -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (path, bytes) in [
            ("tex/latex/demo/demo.sty", b"% demo package\n".as_slice()),
            ("tex/latex/demo/extra.def", b"% unopened file\n".as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(bytes.len() as u64);
            header.set_cksum();
            tar.append_data(&mut header, path, bytes).unwrap();
        }
        let tar = tar.into_inner().unwrap();
        let mut compressed = Vec::new();
        lzma_rs::xz_compress(&mut Cursor::new(tar), &mut compressed).unwrap();
        compressed
    }

    fn registry(bytes: &[u8], origin: &str) -> TlpdbIndex {
        use sha2::Sha512;
        let text = format!(
            "name 00texlive.config\ncategory TLCore\ndepend release/2026\n\nname demo\ncategory Package\nrevision 17\ncontainerchecksum {}\ncontainersize {}\nrunfiles size=1\n RELOC/tex/latex/demo/demo.sty\n RELOC/tex/latex/demo/extra.def\n",
            hex::encode(Sha512::digest(bytes)),
            bytes.len()
        );
        let mut index = TlpdbIndex::parse(&text, Path::new("tlpkg/texlive.tlpdb"));
        index.origin = Some(origin.into());
        index.snapshot_override = Some("2026-07-26".into());
        index
    }

    fn server(bytes: Vec<u8>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0_u8; 4096];
            let count = socket.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..count]).contains("/archive/demo.tar.xz"));
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            socket.write_all(&bytes).unwrap();
        });
        (url, task)
    }

    #[test]
    fn prepared_packages_reuse_server_disk_cache_without_project_source_or_network() {
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("store");
        let bytes = archive();
        let (url, task) = server(bytes.clone());
        let index = registry(&bytes, &url);
        let cache = root.path().join("containers");
        let mut source = PackageByteSource::Remote {
            base_url: url,
            cache_dir: cache.clone(),
            offline: false,
            allow_insecure: true,
        };
        assert_eq!(provider_for_file(&index, "demo.sty").unwrap(), "demo");
        let manifest = prepare_package(&index, "demo", &source, &store).unwrap();
        task.join().unwrap();
        assert_eq!(manifest.files.len(), 2);
        assert!(!root.path().join("main.tex").exists());
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../schemas/pqty.package.schema.json")).unwrap();
        let value = serde_json::to_value(&manifest).unwrap();
        jsonschema::draft202012::meta::validate(&schema).unwrap();
        jsonschema::draft202012::validate(&schema, &value).unwrap();
        // Drop the compressed download cache entirely. A fresh preparation
        // still succeeds offline from the persisted verified package record.
        fs::remove_dir_all(cache).unwrap();
        if let PackageByteSource::Remote { offline, .. } = &mut source {
            *offline = true;
        }
        let reloaded: PackageManifest =
            serde_json::from_slice(&serde_json::to_vec(&manifest).unwrap()).unwrap();
        let cached = prepare_package(&index, "demo", &source, &store).unwrap();
        assert_eq!(reloaded.fingerprint, cached.fingerprint);
        let output = root.path().join("public.sty");
        export_package_file(&cached, &store, "tex/latex/demo/demo.sty", &output).unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"% demo package\n");
        assert!(!root.path().join("extra.def").exists());
        assert!(!root.path().join("texmf").exists());
        let mut sink = Vec::new();
        assert!(copy_package_file(&cached, &store, "../demo.sty", &mut sink).is_err());
        assert!(sink.is_empty(), "{sink:?}");
        let mut wrong_snapshot = registry(&bytes, "https://other.invalid/snapshot");
        wrong_snapshot.snapshot_override = Some("2026-07-27".into());
        assert!(cached.validate_against(&wrong_snapshot).is_err());
        crate::tests::make_test_tree_writable(root.path());
    }

    #[test]
    fn package_export_rejects_corruption_and_wrong_sizes_before_raw_output() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("distribution");
        let bytes = archive();
        let index = registry(&bytes, "https://example.invalid/pinned");
        fs::create_dir_all(source.join("texmf-dist/tex/latex/demo")).unwrap();
        fs::write(
            source.join("texmf-dist/tex/latex/demo/demo.sty"),
            b"% demo package\n",
        )
        .unwrap();
        fs::write(
            source.join("texmf-dist/tex/latex/demo/extra.def"),
            b"% unopened file\n",
        )
        .unwrap();
        let store = root.path().join("store");
        let package = prepare_package(
            &index,
            "demo",
            &PackageByteSource::local_texlive(source),
            &store,
        )
        .unwrap();
        let mut altered = package.clone();
        altered.files[0].size += 1;
        altered.fingerprint = altered.compute_fingerprint().unwrap();
        let mut raw = Vec::new();
        assert!(copy_package_file(&altered, &store, &altered.files[0].tds_path, &mut raw).is_err());
        assert!(raw.is_empty(), "{raw:?}");
        let output = root.path().join("published.sty");
        fs::write(&output, b"previous output").unwrap();
        let digest = package.files[0].digest.strip_prefix("sha256:").unwrap();
        crate::tests::make_test_tree_writable(&store);
        fs::write(store.join(&digest[..2]).join(digest), b"corruption").unwrap();
        assert!(
            export_package_file(&package, &store, &package.files[0].tds_path, &output).is_err()
        );
        assert_eq!(fs::read(&output).unwrap(), b"previous output");
        assert!(copy_package_file(&package, &store, &package.files[0].tds_path, &mut raw).is_err());
        assert!(raw.is_empty(), "{raw:?}");
        crate::tests::make_test_tree_writable(root.path());
    }

    #[test]
    fn filename_resolution_rejects_unsafe_missing_and_ambiguous_ownership() {
        let text = "name demo\ncategory Package\nrevision 1\nrunfiles size=1\n RELOC/tex/latex/demo/same.sty\n\nname other\ncategory Package\nrevision 1\nrunfiles size=1\n RELOC/tex/latex/other/same.sty\n";
        let index = TlpdbIndex::parse(text, Path::new("tlpkg/texlive.tlpdb"));
        assert!(
            provider_for_file(&index, "same.sty")
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        assert_eq!(
            provider_for_file(&index, "tex/latex/demo/same.sty").unwrap(),
            "demo"
        );
        for path in ["../same.sty", "/same.sty", "missing.sty"] {
            assert!(provider_for_file(&index, path).is_err());
        }
    }
}
