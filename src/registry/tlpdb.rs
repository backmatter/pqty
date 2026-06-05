use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::registry::model::{IndexPackage, PackageRegistry};
use crate::{
    MAX_CLOSURE_RUNFILES, MAX_CONTAINER_COMPRESSED_BYTES, MAX_PROVIDER_RUNFILES,
    MAX_TLPDB_EXPANDED_BYTES, MAX_TLPDB_LINE_BYTES, MAX_TLPDB_PACKAGES, MetadataCachePolicy,
    PqtyError, Registry, RegistryKind, fetch_tlpdb, invalid_artifact_text, normalize_runfile,
    read_bounded, tlnet_base_from_tlpdb_url, validate_provider_identifier, validate_tds_path,
};

/// Validated, queryable index of TeX Live `texlive.tlpdb` metadata.
///
/// The index maps requested basenames and normalized TDS paths to providers,
/// retains package dependency and container metadata, and records the exact
/// metadata digest used for lock provenance.
pub struct TlpdbIndex {
    /// provider name -> metadata
    packages: BTreeMap<String, IndexPackage>,
    /// installed file basename (e.g. "graphicx.sty") -> provider candidates.
    by_file: BTreeMap<String, Vec<String>>,
    /// normalized TDS path -> provider candidates.
    by_path: BTreeMap<String, Vec<String>>,
    /// TEXMF root, derived from the tlpdb location (for the registry url).
    pub(crate) texmf_root: Option<PathBuf>,
    /// TeX Live release (snapshot anchor), if declared in 00texlive.config.
    release: Option<String>,
    /// Published Registry Snapshot selector supplied by the Consumer.
    pub(crate) snapshot_override: Option<String>,
    /// Exact identity of the provider metadata, including file ownership and
    /// dependency edges.
    metadata_digest: String,
    /// tlnet base URL when loaded remotely (for the registry record + fetches).
    pub(crate) origin: Option<String>,
}

#[derive(Default)]
struct TlpdbRecord {
    name: Option<String>,
    revision: Option<String>,
    category: Option<String>,
    container_checksum: Option<String>,
    container_size: Option<u64>,
    depends: Vec<String>,
    font_maps: Vec<String>,
    files: Vec<String>,
    in_runfiles: bool,
    saw_content: bool,
}

#[derive(Default)]
struct TlpdbContents {
    packages: BTreeMap<String, IndexPackage>,
    by_file: BTreeMap<String, Vec<String>>,
    by_path: BTreeMap<String, Vec<String>>,
    release: Option<String>,
}

fn finish_tlpdb_record(
    record: &mut TlpdbRecord,
    contents: &mut TlpdbContents,
) -> Result<(), PqtyError> {
    if !record.saw_content {
        return Ok(());
    }
    let current = record.name.take().ok_or_else(|| {
        PqtyError::Usage("tlpdb contains a non-empty record without a provider name".to_string())
    })?;
    validate_tlpdb_record(record, &current, &contents.packages)?;
    update_release(record, &current, &mut contents.release)?;
    index_runfiles(
        record,
        &current,
        &mut contents.by_file,
        &mut contents.by_path,
    );
    insert_package(record, current, &mut contents.packages);
    *record = TlpdbRecord::default();
    Ok(())
}

fn validate_tlpdb_record(
    record: &TlpdbRecord,
    current: &str,
    packages: &BTreeMap<String, IndexPackage>,
) -> Result<(), PqtyError> {
    validate_provider_identifier(current, "tlpdb provider")?;
    if packages.len() >= MAX_TLPDB_PACKAGES {
        return Err(PqtyError::Usage(format!(
            "tlpdb contains more than {MAX_TLPDB_PACKAGES} provider records"
        )));
    }
    if packages.contains_key(current) {
        return Err(PqtyError::Usage(format!(
            "tlpdb contains duplicate provider record {current}"
        )));
    }
    if record.files.len() > MAX_PROVIDER_RUNFILES {
        return Err(PqtyError::Usage(format!(
            "tlpdb provider {current} contains more than {MAX_PROVIDER_RUNFILES} runfiles"
        )));
    }
    if record.container_checksum.is_some() != record.container_size.is_some() {
        return Err(PqtyError::Usage(format!(
            "tlpdb provider {current} must declare container checksum and size together"
        )));
    }
    if let Some(checksum) = record.container_checksum.as_deref()
        && (checksum.len() != 128
            || !checksum
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(PqtyError::Usage(format!(
            "tlpdb provider {current} has malformed SHA-512 container checksum"
        )));
    }
    if let Some(size) = record.container_size
        && (size == 0 || size > MAX_CONTAINER_COMPRESSED_BYTES)
    {
        return Err(PqtyError::Usage(format!(
            "tlpdb provider {current} declares unsupported container size {size}"
        )));
    }
    for (label, value) in [
        ("revision", record.revision.as_deref()),
        ("category", record.category.as_deref()),
    ] {
        if value.is_some_and(invalid_artifact_text) {
            return Err(PqtyError::Usage(format!(
                "tlpdb provider {current} has invalid {label}"
            )));
        }
    }
    for dependency in &record.depends {
        if invalid_artifact_text(dependency) || dependency.len() > 4096 {
            return Err(PqtyError::Usage(format!(
                "tlpdb provider {current} has invalid dependency"
            )));
        }
    }
    for map in &record.font_maps {
        validate_tds_path(map, "tlpdb font map")?;
    }
    let mut unique_files = BTreeSet::new();
    for file in &record.files {
        if !unique_files.insert(file.as_str()) {
            return Err(PqtyError::Usage(format!(
                "tlpdb provider {current} repeats runfile {file}"
            )));
        }
    }
    Ok(())
}

fn update_release(
    record: &TlpdbRecord,
    current: &str,
    release: &mut Option<String>,
) -> Result<(), PqtyError> {
    if current == "00texlive.config" {
        for dependency in &record.depends {
            if let Some(revision) = dependency.strip_prefix("release/") {
                if revision.is_empty() || !revision.bytes().all(|byte| byte.is_ascii_alphanumeric())
                {
                    return Err(PqtyError::Usage(format!(
                        "tlpdb declares invalid TeX Live release {revision}"
                    )));
                }
                *release = Some(revision.to_string());
            }
        }
    }
    Ok(())
}

fn index_runfiles(
    record: &TlpdbRecord,
    current: &str,
    by_file: &mut BTreeMap<String, Vec<String>>,
    by_path: &mut BTreeMap<String, Vec<String>>,
) {
    for file in &record.files {
        let (_, normalized) = normalize_runfile(file);
        by_path
            .entry(normalized)
            .or_default()
            .push(current.to_string());
        if let Some(base) = Path::new(file).file_name().and_then(|name| name.to_str()) {
            by_file
                .entry(base.to_string())
                .or_default()
                .push(current.to_string());
        }
    }
}

fn insert_package(
    record: &mut TlpdbRecord,
    current: String,
    packages: &mut BTreeMap<String, IndexPackage>,
) {
    let version = record.revision.take().map_or_else(
        || "tlrev:unknown".to_string(),
        |revision| format!("tlrev:{revision}"),
    );
    packages.insert(
        current,
        IndexPackage {
            version,
            category: record
                .category
                .take()
                .unwrap_or_else(|| "Package".to_string()),
            depends: std::mem::take(&mut record.depends),
            font_maps: std::mem::take(&mut record.font_maps),
            runfiles: std::mem::take(&mut record.files),
            container_checksum: record.container_checksum.take(),
            container_size: record.container_size.take(),
        },
    );
}

fn parse_tlpdb_contents(text: &str) -> Result<TlpdbContents, PqtyError> {
    let mut contents = TlpdbContents::default();
    let mut record = TlpdbRecord::default();
    let mut total_runfiles = 0_usize;

    // A trailing empty line flushes the final block.
    for (line_index, line) in text.lines().chain(std::iter::once("")).enumerate() {
        let line_number = line_index + 1;
        if line.len() > MAX_TLPDB_LINE_BYTES {
            return Err(PqtyError::Usage(format!(
                "tlpdb line {line_number} exceeds the {MAX_TLPDB_LINE_BYTES}-byte limit"
            )));
        }
        if line.is_empty() {
            finish_tlpdb_record(&mut record, &mut contents)?;
            continue;
        }
        record.saw_content = true;
        if line.starts_with(' ') {
            parse_runfile_line(line, &mut record, &mut total_runfiles)?;
        } else {
            parse_record_field(line, line_number, &mut record)?;
        }
    }
    Ok(contents)
}

fn parse_runfile_line(
    line: &str,
    record: &mut TlpdbRecord,
    total_runfiles: &mut usize,
) -> Result<(), PqtyError> {
    if !record.in_runfiles {
        return Ok(());
    }
    let Some(file) = line.split_whitespace().next() else {
        return Ok(());
    };
    validate_tds_path(file, "tlpdb runfile")?;
    *total_runfiles = total_runfiles
        .checked_add(1)
        .ok_or_else(|| PqtyError::Usage("tlpdb runfile count overflow".to_string()))?;
    if *total_runfiles > MAX_CLOSURE_RUNFILES {
        return Err(PqtyError::Usage(format!(
            "tlpdb contains more than {MAX_CLOSURE_RUNFILES} runfiles"
        )));
    }
    record.files.push(file.to_string());
    Ok(())
}

fn parse_record_field(
    line: &str,
    line_number: usize,
    record: &mut TlpdbRecord,
) -> Result<(), PqtyError> {
    let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
    record.in_runfiles = key == "runfiles";
    match key {
        "name" if record.name.is_some() => {
            return Err(PqtyError::Usage(format!(
                "tlpdb record has duplicate name at line {line_number}"
            )));
        }
        "name" => record.name = Some(rest.to_string()),
        "revision" => record.revision = Some(rest.to_string()),
        "category" => record.category = Some(rest.to_string()),
        "depend" => record.depends.push(rest.to_string()),
        "execute" => {
            if let Some(font_map) = parse_font_map_action(rest) {
                record.font_maps.push(font_map);
            }
        }
        "containerchecksum" => record.container_checksum = Some(rest.to_string()),
        "containersize" => {
            record.container_size = Some(rest.parse::<u64>().map_err(|_| {
                PqtyError::Usage(format!(
                    "tlpdb has malformed container size at line {line_number}: {rest}"
                ))
            })?);
        }
        _ => {}
    }
    Ok(())
}

fn normalize_provider_indexes(
    packages: &BTreeMap<String, IndexPackage>,
    by_file: &mut BTreeMap<String, Vec<String>>,
    by_path: &mut BTreeMap<String, Vec<String>>,
) {
    let stable_providers = packages
        .iter()
        .filter(|(provider, package)| {
            !provider.ends_with("-dev")
                && !package
                    .runfiles
                    .iter()
                    .any(|path| path.contains("/latex-dev/"))
        })
        .map(|(provider, _)| provider.as_str())
        .collect::<BTreeSet<_>>();
    for providers in by_file.values_mut().chain(by_path.values_mut()) {
        providers.sort();
        providers.dedup();
        let has_stable = providers
            .iter()
            .any(|provider| stable_providers.contains(provider.as_str()));
        if has_stable {
            providers.retain(|provider| stable_providers.contains(provider.as_str()));
        }
    }
}

impl TlpdbIndex {
    /// Load a TeX Live package database from disk.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be read.
    pub fn load(path: &Path) -> Result<Self, PqtyError> {
        let file = fs::File::open(path).map_err(|source| PqtyError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let bytes = read_bounded(file, MAX_TLPDB_EXPANDED_BYTES, &path.display().to_string())?;
        let text = String::from_utf8(bytes).map_err(|_| {
            PqtyError::Usage(format!(
                "package registry metadata is not valid UTF-8: {}",
                path.display()
            ))
        })?;
        Self::try_parse(&text, path)
    }

    /// Fetch and cache a tlnet package database, preserving its remote origin
    /// in the registry record emitted into locks.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported URL, a failed or oversized
    /// response, decompression failure, or a cache write failure.
    pub fn load_url(url: &str, cache_dir: &Path) -> Result<Self, PqtyError> {
        let path = fetch_tlpdb(url, cache_dir, MetadataCachePolicy::Revalidate, false)?;
        let mut index = Self::load(&path)?;
        index.origin = tlnet_base_from_tlpdb_url(url);
        if index.origin.is_none() {
            return Err(PqtyError::Usage(format!(
                "unsupported tlnet package database URL: {url}"
            )));
        }
        Ok(index)
    }

    #[cfg(test)]
    pub(crate) fn parse(text: &str, source: &Path) -> Self {
        Self::try_parse(text, source).expect("valid tlpdb fixture")
    }

    pub(crate) fn try_parse(text: &str, source: &Path) -> Result<Self, PqtyError> {
        let metadata_digest = format!("sha256:{}", hex::encode(Sha256::digest(text.as_bytes())));
        let mut contents = parse_tlpdb_contents(text)?;
        normalize_provider_indexes(
            &contents.packages,
            &mut contents.by_file,
            &mut contents.by_path,
        );
        let texmf_root = source
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        Ok(Self {
            packages: contents.packages,
            by_file: contents.by_file,
            by_path: contents.by_path,
            texmf_root,
            release: contents.release,
            snapshot_override: None,
            metadata_digest,
            origin: None,
        })
    }

    /// Providers that ship a file by its exact basename.
    pub fn providers_of_file(&self, filename: &str) -> Vec<&str> {
        self.by_file
            .get(filename)
            .into_iter()
            .flatten()
            .filter(|provider| {
                self.packages
                    .get(provider.as_str())
                    .is_some_and(IndexPackage::provides_package_resource)
            })
            .map(String::as_str)
            .collect()
    }

    /// Installed distribution databases can describe a larger upstream
    /// collection than the OS package actually placed on disk. Restrict a
    /// local index to its observable files so resolution never selects an
    /// absent provider and the registry identity records that installed
    /// ownership subset.
    pub(crate) fn retain_installed_runfiles(&mut self) {
        let Some(texmf_root) = self.texmf_root.as_ref() else {
            return;
        };
        for package in self.packages.values_mut() {
            package.runfiles.retain(|runfile| {
                let (placement, _) = normalize_runfile(runfile);
                texmf_root.join(placement).is_file()
            });
            package.font_maps.retain(|map_name| {
                package.runfiles.iter().any(|runfile| {
                    Path::new(runfile)
                        .file_name()
                        .and_then(|name| name.to_str())
                        == Some(map_name)
                })
            });
        }

        self.by_file.clear();
        self.by_path.clear();
        let mut digest = Sha256::new();
        digest.update(self.metadata_digest.as_bytes());
        for (provider, package) in &self.packages {
            digest.update(provider.as_bytes());
            for runfile in &package.runfiles {
                let (_, normalized) = normalize_runfile(runfile);
                digest.update(normalized.as_bytes());
                self.by_path
                    .entry(normalized)
                    .or_default()
                    .push(provider.clone());
                if let Some(base) = Path::new(runfile)
                    .file_name()
                    .and_then(|name| name.to_str())
                {
                    self.by_file
                        .entry(base.to_string())
                        .or_default()
                        .push(provider.clone());
                }
            }
        }
        let stable_providers = self
            .packages
            .iter()
            .filter(|(provider, package)| {
                !provider.ends_with("-dev")
                    && !package
                        .runfiles
                        .iter()
                        .any(|path| path.contains("/latex-dev/"))
            })
            .map(|(provider, _)| provider.as_str())
            .collect::<BTreeSet<_>>();
        for providers in self.by_file.values_mut().chain(self.by_path.values_mut()) {
            providers.sort();
            providers.dedup();
            let has_stable = providers
                .iter()
                .any(|provider| stable_providers.contains(provider.as_str()));
            if has_stable {
                providers.retain(|provider| stable_providers.contains(provider.as_str()));
            }
        }
        self.metadata_digest = format!("sha256:{}", hex::encode(digest.finalize()));
    }

    /// Unique provider that ships a file by its exact basename.
    #[must_use]
    pub fn provider_of_file(&self, filename: &str) -> Option<&str> {
        let providers = self.providers_of_file(filename);
        (providers.len() == 1)
            .then(|| providers.first().copied())
            .flatten()
    }

    /// Providers that ship an exact normalized TDS path.
    pub fn providers_of_path(&self, path: &str) -> Vec<&str> {
        let path = path
            .strip_prefix("RELOC/")
            .or_else(|| path.strip_prefix("texmf-dist/"))
            .unwrap_or(path);
        self.by_path
            .get(path)
            .into_iter()
            .flatten()
            .filter(|provider| {
                self.packages
                    .get(provider.as_str())
                    .is_some_and(IndexPackage::provides_package_resource)
            })
            .map(String::as_str)
            .collect()
    }

    /// Unique provider that ships an exact normalized TDS path.
    #[must_use]
    pub fn provider_of_path(&self, path: &str) -> Option<&str> {
        let providers = self.providers_of_path(path);
        (providers.len() == 1)
            .then(|| providers.first().copied())
            .flatten()
    }

    /// SHA-256 identity of the parsed package database.
    #[must_use]
    pub fn metadata_digest(&self) -> &str {
        &self.metadata_digest
    }
}

impl PackageRegistry for TlpdbIndex {
    fn providers_of(&self, stem: &str, extensions: &[&str]) -> Vec<&str> {
        let mut providers = BTreeSet::new();
        for extension in extensions {
            if let Some(candidates) = self.by_file.get(&format!("{stem}.{extension}")) {
                providers.extend(candidates.iter().filter_map(|provider| {
                    self.packages
                        .get(provider)
                        .is_some_and(IndexPackage::provides_package_resource)
                        .then_some(provider.as_str())
                }));
            }
        }
        providers.into_iter().collect()
    }

    fn providers_of_file(&self, filename: &str) -> Vec<&str> {
        TlpdbIndex::providers_of_file(self, filename)
    }

    fn font_file_candidates<'a>(&'a self, stem: &str) -> Vec<(&'a str, &'a str)> {
        let mut candidates = Vec::new();
        for (filename, providers) in &self.by_file {
            let path = Path::new(filename);
            let Some(candidate_stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if candidate_stem.eq_ignore_ascii_case(stem)
                && matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "otf" | "ttf" | "ttc"
                )
            {
                candidates.extend(
                    providers
                        .iter()
                        .filter(|provider| {
                            self.packages
                                .get(provider.as_str())
                                .is_some_and(IndexPackage::provides_package_resource)
                        })
                        .map(|provider| (filename.as_str(), provider.as_str())),
                );
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        candidates
    }

    fn providers_of_path(&self, path: &str) -> Vec<&str> {
        TlpdbIndex::providers_of_path(self, path)
    }

    fn package(&self, provider: &str) -> Option<&IndexPackage> {
        self.packages.get(provider)
    }

    fn registry(&self) -> Registry {
        match &self.origin {
            Some(base) => Registry {
                id: "tlnet".to_string(),
                kind: RegistryKind::Tlnet,
                url: base.clone(),
                snapshot: self
                    .snapshot_override
                    .clone()
                    .or_else(|| self.release.clone()),
                metadata_digest: Some(self.metadata_digest.clone()),
            },
            None => Registry {
                id: "texlive-local".to_string(),
                kind: RegistryKind::Tlnet,
                url: self.texmf_root.as_ref().map_or_else(
                    || "texlive-local".to_string(),
                    |root| format!("file://{}", root.display()),
                ),
                snapshot: self
                    .snapshot_override
                    .clone()
                    .or_else(|| self.release.clone()),
                metadata_digest: Some(self.metadata_digest.clone()),
            },
        }
    }
}

fn parse_font_map_action(action: &str) -> Option<String> {
    let mut fields = action.split_whitespace();
    let command = fields.next()?;
    if !matches!(command, "addMap" | "addMixedMap") {
        return None;
    }
    let map = fields.next()?;
    (Path::new(map)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("map")))
    .then(|| map.to_string())
}

pub(crate) fn locate_tlpdb() -> Option<PathBuf> {
    if let Ok(output) = std::process::Command::new("kpsewhich")
        .arg("-var-value=TEXMFROOT")
        .output()
        && output.status.success()
    {
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !root.is_empty() {
            let candidate = Path::new(&root).join("tlpkg/texlive.tlpdb");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for candidate in [
        "/usr/share/tlpkg/texlive.tlpdb",
        "/usr/share/texlive/tlpkg/texlive.tlpdb",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

// ---------------------------------------------------------------------------
