//! Complete, immutable package publication outside the request path.
use std::{collections::BTreeSet, fs, path::Path};

use super::{
    file_export::copy_object, package::prepare_package, stream::digest_file, transaction::FileGuard,
};
use crate::{PackageByteSource, PqtyError, TlpdbIndex, atomic_write, publish_file, temporary_file};

/// Prepare and publish every runtime provider in one registry snapshot.
///
/// Providers are processed serially through the existing verified store. Public
/// objects use full SHA-256 filenames; provider metadata retains `pqty.package/v1`.
/// `catalogs/<metadata-digest>.json` is published last, in `pqty.registry/v1`
/// format, so its presence records a completely published snapshot. Interrupted
/// runs can resume from verified objects without downloading them again.
///
/// The caller owns storage capacity and the serving layer. No server, project,
/// engine, installation tree, or background process is created.
///
/// # Errors
/// Rejects store/output overlap, missing or corrupt inputs, and publication errors.
pub fn mirror_packages(
    index: &TlpdbIndex,
    source: &PackageByteSource,
    store: &Path,
    output: &Path,
) -> Result<(), PqtyError> {
    mkdir(store)?;
    mkdir(output)?;
    let _guard = FileGuard::acquire(&output.join(".pqty-mirror.lock"))?;
    let public = output.join("public");
    mkdir(&public)?;
    let canonical_store = canonical(store)?;
    let canonical_public = canonical(&public)?;
    if canonical_public.starts_with(&canonical_store)
        || canonical_store.starts_with(&canonical_public)
    {
        return Err(PqtyError::Usage(
            "mirror public objects must be outside the private store".into(),
        ));
    }
    let digest = index.metadata_digest();
    let identity = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| PqtyError::Usage("missing registry digest".into()))?;
    let providers_root = output.join("providers").join(identity);
    let catalogs = output.join("catalogs");
    mkdir(&providers_root)?;
    mkdir(&catalogs)?;
    if canonical(&providers_root)?.starts_with(&canonical_store)
        || canonical(&catalogs)?.starts_with(&canonical_store)
    {
        return Err(PqtyError::Usage(
            "mirror metadata must be outside the private store".into(),
        ));
    }
    let owners = index.runtime_file_owners();
    let providers: BTreeSet<_> = owners.values().flatten().collect();
    let mut published = BTreeSet::new();
    for provider in providers {
        let package = prepare_package(index, provider, source, store)?;
        for file in &package.files {
            if !published.insert(file.digest.clone()) {
                continue;
            }
            let hex = file
                .digest
                .strip_prefix("sha256:")
                .ok_or_else(|| PqtyError::Usage("invalid package file digest".into()))?;
            let path = public.join(hex);
            match fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    if !metadata.is_file() {
                        return Err(PqtyError::Usage(
                            "mirror object is not a regular file".into(),
                        ));
                    }
                    if metadata.len() == file.size {
                        let (actual, size) = digest_file(&path, crate::MAX_ARCHIVE_ENTRY_BYTES)?;
                        if actual == hex && size == file.size {
                            continue;
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(PqtyError::Io { path, source }),
            }
            let mut pending = temporary_file(&path, "mirror-object")?;
            if copy_object(store, &file.digest, &mut pending, false)? != file.size {
                return Err(PqtyError::Usage("mirror object size mismatch".into()));
            }
            publish_file(pending, &path)?;
        }
        atomic_write(
            &providers_root.join(format!("{provider}.json")),
            &serde_json::to_vec(&package)?,
        )?;
    }
    let catalog = serde_json::json!({"schema":"pqty.registry/v1", "metadata_digest":digest, "file_owners":owners});
    atomic_write(
        &catalogs.join(format!("{identity}.json")),
        &serde_json::to_vec(&catalog)?,
    )
}

fn mkdir(path: &Path) -> Result<(), PqtyError> {
    fs::create_dir_all(path).map_err(|source| PqtyError::Io {
        path: path.into(),
        source,
    })
}
fn canonical(path: &Path) -> Result<std::path::PathBuf, PqtyError> {
    fs::canonicalize(path).map_err(|source| PqtyError::Io {
        path: path.into(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest as _, Sha256};

    #[test]
    fn complete_publication_repairs_objects_and_keeps_incomplete_snapshots_unpublished() {
        let root = tempfile::tempdir().unwrap();
        let distribution = root.path().join("distribution");
        let inputs = distribution.join("texmf-dist/tex/latex/demo");
        mkdir(&inputs).unwrap();
        fs::write(inputs.join("a.sty"), b"same bytes").unwrap();
        fs::write(inputs.join("b.sty"), b"same bytes").unwrap();
        let database = root.path().join("texlive.tlpdb");
        fs::write(&database, "name demo\ncategory Package\nrevision 1\nrunfiles size=1\n RELOC/tex/latex/demo/a.sty\n RELOC/tex/latex/demo/b.sty\n\nname second\ncategory Package\nrevision 1\nrunfiles size=1\n RELOC/tex/latex/demo/c.sty\n").unwrap();
        let index = TlpdbIndex::load(&database).unwrap();
        let store = root.path().join("store");
        let output = root.path().join("mirror");
        let source = PackageByteSource::local_texlive(&distribution);
        let catalog = output.join("catalogs").join(format!(
            "{}.json",
            index.metadata_digest().strip_prefix("sha256:").unwrap()
        ));
        assert!(mirror_packages(&index, &source, &store, &output).is_err());
        assert!(!catalog.exists());
        fs::write(inputs.join("c.sty"), b"other bytes").unwrap();
        mirror_packages(&index, &source, &store, &output).unwrap();
        assert!(catalog.exists());
        assert_eq!(fs::read_dir(output.join("public")).unwrap().count(), 2);
        let path = output
            .join("public")
            .join(hex::encode(Sha256::digest(b"same bytes")));
        fs::write(&path, b"bad").unwrap();
        mirror_packages(&index, &source, &store, &output).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"same bytes");
        assert!(mirror_packages(&index, &source, &store, &store).is_err());
        crate::tests::make_test_tree_writable(root.path());
    }
}
