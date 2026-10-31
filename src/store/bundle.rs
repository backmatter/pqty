//! Portable package bytes. No caller needs to know pqty's private store layout.
#![allow(clippy::large_stack_arrays)]
use super::install::stored_file_digest;
use super::object::{
    StoredPackageManifest, ensure_store_object, load_store_manifest, package_digest,
    provider_manifest_digest, write_store_manifest,
};
use crate::{LockFile, PqtyError, unique_sibling, validate_materialized_lock};
use flate2::{Compression, bufread::GzDecoder, write::GzEncoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, Read, Write},
    path::Path,
};

/// Schema identifying a portable, verified package bundle.
pub const BUNDLE_SCHEMA: &str = "pqty.bundle/v1";
const MAX_TOTAL: u64 = 2 * 1024 * 1024 * 1024;
const MAX_FILE: u64 = 128 * 1024 * 1024;
const MAX_HEADER: u64 = 64 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    schema: String,
    lock: LockFile,
    manifests: BTreeMap<String, StoredPackageManifest>,
}
fn invalid(message: impl Into<String>) -> PqtyError {
    PqtyError::Usage(message.into())
}
fn io(path: &Path, source: std::io::Error) -> PqtyError {
    PqtyError::Io {
        path: path.to_owned(),
        source,
    }
}
fn append<W: Write, R: Read>(
    archive: &mut tar::Builder<W>,
    path: &str,
    size: u64,
    reader: R,
) -> Result<(), PqtyError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(size);
    header.set_mode(0o600);
    header.set_mtime(0);
    header.set_cksum();
    archive
        .append_data(&mut header, path, reader)
        .map_err(|e| invalid(format!("Could not write package bundle: {e}")))
}
fn digest_file(path: &Path) -> Result<String, PqtyError> {
    let mut file = File::open(path).map_err(|e| io(path, e))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut total = 0;
    loop {
        let size = file.read(&mut buffer).map_err(|e| io(path, e))?;
        if size == 0 {
            break;
        }
        total += size as u64;
        if total > MAX_FILE {
            return Err(invalid("Package bundle file exceeds its size limit"));
        }
        hash.update(&buffer[..size]);
    }
    Ok(hex::encode(hash.finalize()))
}
fn validate(bundle: &Bundle) -> Result<BTreeSet<String>, PqtyError> {
    if bundle.schema != BUNDLE_SCHEMA {
        return Err(invalid("Unsupported package bundle schema"));
    }
    validate_materialized_lock(&bundle.lock)?;
    if bundle.manifests.len() != bundle.lock.closure.len() {
        return Err(invalid("Package bundle provider set differs from its lock"));
    }
    let mut objects = BTreeSet::new();
    for provider in &bundle.lock.closure {
        let manifest = bundle
            .manifests
            .get(&provider.provider)
            .ok_or_else(|| invalid("Package bundle is missing a provider"))?;
        if manifest.schema != super::object::STORE_PACKAGE_SCHEMA
            || hex::encode(provider_manifest_digest(&manifest.files)?) != package_digest(provider)?
        {
            return Err(invalid(
                "Package bundle provider integrity differs from its lock",
            ));
        }
        let locked = provider
            .files
            .iter()
            .map(|f| f.tds_path.as_str())
            .collect::<BTreeSet<_>>();
        let actual = manifest
            .files
            .iter()
            .map(|f| f.tds_path.as_str())
            .collect::<BTreeSet<_>>();
        if locked != actual || actual.len() != manifest.files.len() {
            return Err(invalid("Package bundle file index differs from its lock"));
        }
        for file in &manifest.files {
            objects.insert(stored_file_digest(&file.digest)?.to_owned());
        }
    }
    Ok(objects)
}

/// Export an exact lock and verified immutable package bytes to an atomic archive.
///
/// # Errors
///
/// Returns an error if a locked package is missing or fails verification, or if
/// the archive cannot be written.
pub fn export_bundle(lock: &LockFile, store: &Path, output: &Path) -> Result<(), PqtyError> {
    let mut manifests = BTreeMap::new();
    for provider in &lock.closure {
        manifests.insert(
            provider.provider.clone(),
            load_store_manifest(store, provider)?,
        );
    }
    let bundle = Bundle {
        schema: BUNDLE_SCHEMA.to_owned(),
        lock: lock.clone(),
        manifests,
    };
    let objects = validate(&bundle)?;
    let bytes = serde_json::to_vec(&bundle)?;
    if bytes.len() as u64 > MAX_HEADER {
        return Err(invalid("Package bundle metadata exceeds its size limit"));
    }
    let temporary = unique_sibling(output, "bundle")?;
    let result = (|| {
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| io(&temporary, e))?;
        let encoder = GzEncoder::new(file, Compression::fast());
        let mut archive = tar::Builder::new(encoder);
        append(
            &mut archive,
            "bundle.json",
            bytes.len() as u64,
            bytes.as_slice(),
        )?;
        let mut total = bytes.len() as u64;
        for digest in objects {
            let path = store.join(&digest[..2]).join(&digest);
            if digest_file(&path)? != digest {
                return Err(invalid("Cannot export a corrupt package store object"));
            }
            let file = File::open(&path).map_err(|e| io(&path, e))?;
            let size = file.metadata().map_err(|e| io(&path, e))?.len();
            total = total
                .checked_add(size)
                .ok_or_else(|| invalid("Package bundle is too large"))?;
            if total > MAX_TOTAL {
                return Err(invalid("Package bundle exceeds its size limit"));
            }
            append(&mut archive, &format!("objects/{digest}"), size, file)?;
        }
        let encoder = archive.into_inner().map_err(|e| io(&temporary, e))?;
        let file = encoder.finish().map_err(|e| io(&temporary, e))?;
        file.sync_all().map_err(|e| io(&temporary, e))?;
        fs::rename(&temporary, output).map_err(|e| io(output, e))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Validate a bounded archive before publishing any package manifest into the store.
///
/// # Errors
///
/// Returns an error if the archive is malformed, exceeds its size limits, or holds
/// bytes that do not match its lock.
pub fn import_bundle(input: &Path, store: &Path) -> Result<LockFile, PqtyError> {
    fs::create_dir_all(store).map_err(|e| io(store, e))?;
    let staging = unique_sibling(&store.join("bundle"), "import")?;
    fs::create_dir(&staging).map_err(|e| io(&staging, e))?;
    let result = (|| {
        let file = File::open(input).map_err(|e| io(input, e))?;
        let mut archive = tar::Archive::new(GzDecoder::new(BufReader::new(file)));
        // This format has fixed short names. Reject raw extension headers
        // before tar can expand their bodies into unbounded path metadata.
        let mut entries = archive.entries().map_err(|e| io(input, e))?.raw(true);
        let mut first = entries
            .next()
            .ok_or_else(|| invalid("Empty package bundle"))?
            .map_err(|e| io(input, e))?;
        if !first.header().entry_type().is_file()
            || first.path().map_err(|e| io(input, e))?.as_ref() != Path::new("bundle.json")
            || first.size() > MAX_HEADER
        {
            return Err(invalid("Invalid package bundle metadata entry"));
        }
        let mut metadata = Vec::new();
        first.read_to_end(&mut metadata).map_err(|e| io(input, e))?;
        let bundle: Bundle = serde_json::from_slice(&metadata)?;
        let expected = validate(&bundle)?;
        let mut seen = BTreeSet::new();
        let mut total = metadata.len() as u64;
        for entry in entries {
            let mut entry = entry.map_err(|e| io(input, e))?;
            let path = entry.path().map_err(|e| io(input, e))?.into_owned();
            let digest = path
                .to_str()
                .and_then(|p| p.strip_prefix("objects/"))
                .filter(|d| expected.contains(*d))
                .ok_or_else(|| invalid("Unlisted package bundle object"))?
                .to_owned();
            if !entry.header().entry_type().is_file()
                || !seen.insert(digest.clone())
                || entry.size() > MAX_FILE
            {
                return Err(invalid("Invalid package bundle object entry"));
            }
            total = total
                .checked_add(entry.size())
                .ok_or_else(|| invalid("Package bundle is too large"))?;
            if total > MAX_TOTAL {
                return Err(invalid("Package bundle exceeds its size limit"));
            }
            let target = staging.join(&digest);
            let mut file = File::options()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| io(&target, e))?;
            std::io::copy(&mut entry, &mut file).map_err(|e| io(&target, e))?;
            file.sync_all().map_err(|e| io(&target, e))?;
            if digest_file(&target)? != digest {
                return Err(invalid("Package bundle content checksum mismatch"));
            }
        }
        let mut decoder = archive.into_inner();
        let mut tail = Vec::new();
        Read::by_ref(&mut decoder)
            .take(65537)
            .read_to_end(&mut tail)
            .map_err(|e| io(input, e))?;
        if tail.len() > 65536 || tail.iter().any(|byte| *byte != 0) {
            return Err(invalid("Unexpected trailing package bundle data"));
        }
        let mut compressed = decoder.into_inner();
        let mut extra = [0_u8; 1];
        if compressed.read(&mut extra).map_err(|e| io(input, e))? != 0 {
            return Err(invalid("Unexpected compressed data after package bundle"));
        }
        if seen != expected {
            return Err(invalid("Incomplete package bundle"));
        }
        // Only verified content is published. Extra immutable objects are harmless
        // if a later manifest write fails; no source or active TEXMF tree is touched.
        for digest in &expected {
            let path = staging.join(digest);
            let bytes = fs::read(&path).map_err(|e| io(&path, e))?;
            ensure_store_object(
                store,
                &store.join(&digest[..2]).join(digest),
                &bytes,
                digest,
            )?;
        }
        for provider in &bundle.lock.closure {
            write_store_manifest(
                store,
                &package_digest(provider)?,
                &bundle.manifests[&provider.provider],
            )?;
        }
        Ok(bundle.lock)
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}
