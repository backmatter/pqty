use crate::store::http::read_bounded;
use crate::store::normalize_runfile;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha512};

use crate::{
    MAX_ARCHIVE_ENTRY_BYTES, MAX_CONTAINER_EXPANDED_BYTES, MAX_PROVIDER_RUNFILES,
    MIN_CONTAINER_EXPANDED_BYTES, PqtyError, unique_sibling, validate_tds_path,
};

pub(crate) struct BoundedWriter {
    bytes: Vec<u8>,
    limit: u64,
    description: String,
}

impl BoundedWriter {
    pub(crate) fn new(limit: u64, description: impl Into<String>) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            description: description.into(),
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = (self.bytes.len() as u64).saturating_add(bytes.len() as u64);
        if next > self.limit {
            return Err(io::Error::other(format!(
                "{} exceeded the {}-byte decompression limit",
                self.description, self.limit
            )));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct BoundedFileWriter {
    file: fs::File,
    written: u64,
    limit: u64,
    description: String,
}

impl Write for BoundedFileWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self.written.saturating_add(bytes.len() as u64);
        if next > self.limit {
            return Err(io::Error::other(format!(
                "{} exceeded the {}-byte decompression limit",
                self.description, self.limit
            )));
        }
        let written = self.file.write(bytes)?;
        self.written = self.written.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

pub(super) fn decompress_xz_bounded(
    compressed: &[u8],
    limit: u64,
    description: &str,
) -> Result<Vec<u8>, PqtyError> {
    let mut output = BoundedWriter::new(limit, description);
    lzma_rs::xz_decompress(&mut std::io::BufReader::new(compressed), &mut output)
        .map_err(|error| PqtyError::Usage(format!("xz decompress {description}: {error:?}")))?;
    Ok(output.into_inner())
}

pub(super) fn container_expanded_limit(compressed_size: u64) -> u64 {
    compressed_size
        .saturating_mul(64)
        .clamp(MIN_CONTAINER_EXPANDED_BYTES, MAX_CONTAINER_EXPANDED_BYTES)
}

/// Fetch a package container, verify its sha512 before extraction, and return
/// only the requested regular-file entries.
pub(super) fn validate_container_bytes(
    provider: &str,
    bytes: &[u8],
    expected_sha512: &str,
    expected_size: u64,
) -> Result<(), PqtyError> {
    if bytes.len() as u64 != expected_size {
        return Err(PqtyError::Usage(format!(
            "container size mismatch for {provider}\n  expected: {expected_size}\n  got:      {}",
            bytes.len()
        )));
    }
    let got = hex::encode(Sha512::digest(bytes));
    if got != expected_sha512 {
        return Err(PqtyError::Usage(format!(
            "container checksum mismatch for {provider}\n  expected: {expected_sha512}\n  got:      {got}"
        )));
    }
    Ok(())
}

pub(super) fn extract_required_container(
    compressed: &[u8],
    cache_path: &Path,
    provider: &str,
    runfiles: &[String],
    expanded_limit: u64,
) -> Result<Vec<(String, Vec<u8>)>, PqtyError> {
    if runfiles.len() > MAX_PROVIDER_RUNFILES {
        return Err(PqtyError::Usage(format!(
            "provider {provider} requests more than {MAX_PROVIDER_RUNFILES} archive entries"
        )));
    }
    let expanded = unique_sibling(cache_path, "expanded")?;
    let expansion = (|| -> Result<(), PqtyError> {
        let file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&expanded)
            .map_err(|source| PqtyError::Io {
                path: expanded.clone(),
                source,
            })?;
        let mut output = BoundedFileWriter {
            file,
            written: 0,
            limit: expanded_limit,
            description: provider.to_string(),
        };
        lzma_rs::xz_decompress(&mut std::io::BufReader::new(compressed), &mut output).map_err(
            |error| PqtyError::Usage(format!("xz decompress container {provider}: {error:?}")),
        )?;
        output.file.sync_all().map_err(|source| PqtyError::Io {
            path: expanded.clone(),
            source,
        })
    })();
    if let Err(error) = expansion {
        let _ = fs::remove_file(&expanded);
        return Err(error);
    }

    let extraction = (|| -> Result<Vec<(String, Vec<u8>)>, PqtyError> {
        let file = fs::File::open(&expanded).map_err(|source| PqtyError::Io {
            path: expanded.clone(),
            source,
        })?;
        extract_required_tar(file, provider, runfiles)
    })();
    let _ = fs::remove_file(&expanded);
    extraction
}

pub(super) fn extract_required_tar(
    reader: impl Read,
    provider: &str,
    runfiles: &[String],
) -> Result<Vec<(String, Vec<u8>)>, PqtyError> {
    let container = PathBuf::from("<container>");
    let mut aliases = BTreeMap::new();
    let mut required = BTreeSet::new();
    for runfile in runfiles {
        validate_tds_path(runfile, "registry runfile")?;
        let (placement, bare) = normalize_runfile(runfile);
        if !required.insert(placement.clone()) {
            return Err(PqtyError::Usage(format!(
                "provider {provider} repeats required runfile {runfile}"
            )));
        }
        for alias in [runfile.clone(), bare, placement.clone()] {
            if let Some(previous) = aliases.insert(alias, placement.clone())
                && previous != placement
            {
                return Err(PqtyError::Usage(format!(
                    "provider {provider} has colliding runfile aliases"
                )));
            }
        }
    }

    let mut archive = tar::Archive::new(reader);
    let mut selected = BTreeMap::new();
    let mut selected_bytes = 0_u64;
    let entries = archive.entries().map_err(|source| PqtyError::Io {
        path: container.clone(),
        source,
    })?;
    for entry in entries {
        let mut entry = entry.map_err(|source| PqtyError::Io {
            path: container.clone(),
            source,
        })?;
        let path = entry.path().map_err(|source| PqtyError::Io {
            path: container.clone(),
            source,
        })?;
        let path = path
            .to_str()
            .ok_or_else(|| {
                PqtyError::Usage("container contains a path that is not valid UTF-8".to_string())
            })?
            .to_string();
        let entry_type = entry.header().entry_type();
        validate_container_entry_path(&path, entry_type)?;
        if entry_type.is_dir() {
            continue;
        }
        let Some(placement) = aliases.get(&path) else {
            continue;
        };
        if !entry_type.is_file() {
            return Err(PqtyError::Usage(format!(
                "required container entry {path} for {provider} is not a regular file"
            )));
        }
        let declared_size = entry.header().size().map_err(|source| PqtyError::Io {
            path: container.clone(),
            source,
        })?;
        if declared_size > MAX_ARCHIVE_ENTRY_BYTES {
            return Err(PqtyError::Usage(format!(
                "container entry {path} declares {declared_size} bytes; per-entry limit is {MAX_ARCHIVE_ENTRY_BYTES}"
            )));
        }
        selected_bytes = selected_bytes
            .checked_add(declared_size)
            .ok_or_else(|| PqtyError::Usage("selected archive size overflow".to_string()))?;
        if selected_bytes > MAX_CONTAINER_EXPANDED_BYTES {
            return Err(PqtyError::Usage(format!(
                "selected runfiles for {provider} exceed the {MAX_CONTAINER_EXPANDED_BYTES}-byte provider limit"
            )));
        }
        let bytes = read_bounded(
            &mut entry,
            MAX_ARCHIVE_ENTRY_BYTES,
            &format!("container entry {path}"),
        )?;
        if selected.insert(placement.clone(), bytes).is_some() {
            return Err(PqtyError::Usage(format!(
                "container for {provider} repeats required entry {path}"
            )));
        }
    }
    let selected_paths = selected.keys().cloned().collect::<BTreeSet<_>>();
    let missing = required
        .difference(&selected_paths)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(PqtyError::Usage(format!(
            "container {provider} is missing required runfile(s): {}",
            missing.join(", ")
        )));
    }
    Ok(selected.into_iter().collect())
}

fn validate_container_entry_path(path: &str, entry_type: tar::EntryType) -> Result<(), PqtyError> {
    let path = if entry_type.is_dir() {
        path.strip_suffix('/').unwrap_or(path)
    } else {
        path
    };
    validate_tds_path(path, "container entry path")
}
