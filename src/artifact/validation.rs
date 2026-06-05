use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::artifact::model::{
    BibliographyKind, ConsumerRequirements, InputTrace, Location, LockFile, LockStage,
    ResolvedPackage, TraceReport,
};
use crate::{
    LOCK_SCHEMA, MAX_CONTAINER_COMPRESSED_BYTES, PqtyError, TRACE_REPORT_SCHEMA, TRACE_SCHEMA,
};

pub(crate) fn fail_on_unresolved_packages(lock: &LockFile) -> Result<(), PqtyError> {
    let unresolved = lock
        .unresolved
        .iter()
        .filter(|item| item.kind == "package")
        .map(|item| {
            if item.candidates.is_empty() {
                item.name.clone()
            } else {
                format!(
                    "{} (ambiguous providers: {})",
                    item.name,
                    item.candidates.join(", ")
                )
            }
        })
        .collect::<Vec<_>>();
    if unresolved.is_empty() {
        Ok(())
    } else {
        Err(PqtyError::Usage(format!(
            "cannot lock unresolved package request(s): {}",
            unresolved.join(", ")
        )))
    }
}

/// Validate the common and stage-specific invariants of a lock artifact.
///
/// # Errors
///
/// Returns an error when the schema, stage, Portable Paths, registry
/// provenance, provider graph, or exact integrity records are inconsistent.
pub fn validate_lock(lock: &LockFile) -> Result<(), PqtyError> {
    let source_paths = validate_lock_header(lock)?;
    validate_lock_references(lock, &source_paths)?;
    validate_consumer_requirements(&lock.consumer_requirements)?;

    if lock.stage == LockStage::Scanned {
        return validate_scanned_lock(lock);
    }

    let registry_ids = validate_registries(lock)?;
    let providers = validate_provider_records(lock, &registry_ids)?;
    validate_provider_graph(lock, &providers)?;
    validate_resolved_consumer_requirements(lock)?;
    validate_resolution_requests(lock)?;
    if lock.stage == LockStage::Resolved {
        return Ok(());
    }

    validate_exact_contents(lock)
}

fn validate_lock_header(lock: &LockFile) -> Result<BTreeSet<&str>, PqtyError> {
    if lock.schema != LOCK_SCHEMA {
        return Err(PqtyError::Usage(format!(
            "unsupported lock schema {}; expected {LOCK_SCHEMA}",
            lock.schema
        )));
    }
    if lock.generated_with.is_empty() {
        return Err(PqtyError::Usage(
            "lock generated_with cannot be empty".to_string(),
        ));
    }
    validate_portable_path(&lock.root, "lock root")?;
    let mut source_paths = BTreeSet::new();
    for source in &lock.sources {
        validate_portable_path(&source.path, "source path")?;
        validate_sha256_colon(&source.digest, "source digest")?;
        if !source_paths.insert(source.path.as_str()) {
            return Err(PqtyError::Usage(format!(
                "lock contains duplicate source path {}",
                source.path
            )));
        }
    }
    if !source_paths.contains(lock.root.as_str()) {
        return Err(PqtyError::Usage(
            "lock sources do not contain the declared root".to_string(),
        ));
    }
    Ok(source_paths)
}

fn validate_lock_references(
    lock: &LockFile,
    source_paths: &BTreeSet<&str>,
) -> Result<(), PqtyError> {
    for class in lock.document_class.iter().chain(lock.loaded_classes.iter()) {
        validate_location(&class.source, source_paths)?;
        if let Some(path) = class.resolved_path.as_deref() {
            validate_local_source(path, "local class path", source_paths)?;
        }
    }
    for package in &lock.packages {
        validate_location(&package.source, source_paths)?;
        if let Some(path) = package.resolved_path.as_deref() {
            validate_local_source(path, "local package path", source_paths)?;
        }
    }
    for input in &lock.inputs {
        validate_location(&input.source, source_paths)?;
        if let Some(path) = input.resolved_path.as_deref() {
            validate_local_source(path, "local input path", source_paths)?;
        }
    }
    for bibliography in &lock.bibliographies {
        validate_location(&bibliography.source, source_paths)?;
        if let Some(path) = bibliography.resolved_path.as_deref() {
            validate_local_source(path, "local bibliography path", source_paths)?;
        }
    }
    for graphic in &lock.graphics {
        validate_location(&graphic.source, source_paths)?;
        if let Some(path) = graphic.resolved_path.as_deref() {
            validate_local_source(path, "local graphic path", source_paths)?;
        }
    }
    for unresolved in &lock.unresolved {
        validate_location(&unresolved.source, source_paths)?;
        validate_unique_nonempty(&unresolved.candidates, "unresolved provider candidates")?;
    }
    Ok(())
}

fn validate_scanned_lock(lock: &LockFile) -> Result<(), PqtyError> {
    if !lock.environment.is_empty()
        || !lock.registries.is_empty()
        || !lock.closure.is_empty()
        || !lock.consumer_requirements.is_empty()
    {
        return Err(PqtyError::Usage(
            "scanned lock must not contain a resolution environment, registries, Consumer requirements, or provider closure"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_registries(lock: &LockFile) -> Result<BTreeSet<&str>, PqtyError> {
    if lock.registries.is_empty() {
        return Err(PqtyError::Usage(
            "resolved and exact locks must record registry provenance".to_string(),
        ));
    }
    let mut registry_ids = BTreeSet::new();
    for registry in &lock.registries {
        if invalid_artifact_text(&registry.id)
            || invalid_artifact_text(&registry.url)
            || registry
                .snapshot
                .as_deref()
                .is_some_and(invalid_artifact_text)
        {
            return Err(PqtyError::Usage(
                "registry id, URL, and optional snapshot must be non-empty and free of control characters"
                    .to_string(),
            ));
        }
        if !registry_ids.insert(registry.id.as_str()) {
            return Err(PqtyError::Usage(format!(
                "duplicate registry id in lock: {}",
                registry.id
            )));
        }
        let digest = registry.metadata_digest.as_deref().ok_or_else(|| {
            PqtyError::Usage(format!("registry {} has no metadata digest", registry.id))
        })?;
        validate_sha256_colon(digest, "registry metadata digest")?;
    }
    Ok(registry_ids)
}

fn validate_provider_records<'a>(
    lock: &'a LockFile,
    registry_ids: &BTreeSet<&str>,
) -> Result<BTreeSet<&'a str>, PqtyError> {
    let mut providers = BTreeSet::new();
    for entry in &lock.closure {
        validate_provider_identifier(&entry.provider, "provider")?;
        if invalid_artifact_text(&entry.version) {
            return Err(PqtyError::Usage(
                "provider version must be non-empty and free of control characters".to_string(),
            ));
        }
        validate_provider_identifier(&entry.source.locator, "registry source locator")?;
        if !providers.insert(entry.provider.as_str()) {
            return Err(PqtyError::Usage(format!(
                "duplicate provider in lock closure: {}",
                entry.provider
            )));
        }
        let registry = entry.source.registry.as_deref().ok_or_else(|| {
            PqtyError::Usage(format!(
                "registry provider {} has no source registry",
                entry.provider
            ))
        })?;
        if !registry_ids.contains(registry) {
            return Err(PqtyError::Usage(format!(
                "provider {} references unknown registry {}",
                entry.provider, registry
            )));
        }
        if let Some(checksum) = entry.source.container_checksum.as_deref()
            && (checksum.len() != 128
                || !checksum
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        {
            return Err(PqtyError::Usage(format!(
                "provider {} has invalid SHA-512 container checksum",
                entry.provider
            )));
        }
        if entry.source.container_checksum.is_some() != entry.source.container_size.is_some() {
            return Err(PqtyError::Usage(format!(
                "provider {} must record container checksum and size together",
                entry.provider
            )));
        }
        if entry.source.container_size == Some(0) {
            return Err(PqtyError::Usage(format!(
                "provider {} has a zero container size",
                entry.provider
            )));
        }
        if entry
            .source
            .container_size
            .is_some_and(|size| size > MAX_CONTAINER_COMPRESSED_BYTES)
        {
            return Err(PqtyError::Usage(format!(
                "provider {} declares a container larger than the supported limit",
                entry.provider
            )));
        }
        validate_unique_nonempty(&entry.satisfies, "provider satisfies")?;
        validate_unique_nonempty(&entry.dependencies, "provider dependencies")?;
        validate_unique_nonempty(&entry.requested_by, "provider requested_by")?;
        validate_unique_nonempty(&entry.engines, "provider engines")?;
        validate_unique_nonempty(&entry.font_maps, "provider font maps")?;
        validate_unique_nonempty(&entry.runtime_requests, "provider runtime requests")?;
        for request in &entry.runtime_requests {
            validate_portable_path(request, "provider runtime request")?;
        }
    }
    Ok(providers)
}

fn validate_provider_graph(lock: &LockFile, providers: &BTreeSet<&str>) -> Result<(), PqtyError> {
    for entry in &lock.closure {
        for dependency in &entry.dependencies {
            if dependency == &entry.provider || !providers.contains(dependency.as_str()) {
                return Err(PqtyError::Usage(format!(
                    "provider {} has invalid dependency edge to {}",
                    entry.provider, dependency
                )));
            }
            let Some(dependent) = lock
                .closure
                .iter()
                .find(|candidate| candidate.provider == *dependency)
            else {
                return Err(PqtyError::Usage(format!(
                    "provider {} has an absent dependency {}",
                    entry.provider, dependency
                )));
            };
            if !dependent.requested_by.contains(&entry.provider) {
                return Err(PqtyError::Usage(format!(
                    "dependency edge {} -> {} is missing its requested_by inverse",
                    entry.provider, dependency
                )));
            }
        }
        for parent in &entry.requested_by {
            if !providers.contains(parent.as_str()) {
                return Err(PqtyError::Usage(format!(
                    "provider {} was requested by absent provider {}",
                    entry.provider, parent
                )));
            }
        }
        if !entry.direct && entry.requested_by.is_empty() {
            return Err(PqtyError::Usage(format!(
                "transitive provider {} has no requesting provider",
                entry.provider
            )));
        }
    }
    Ok(())
}

fn validate_consumer_requirements(requirements: &ConsumerRequirements) -> Result<(), PqtyError> {
    validate_unique_nonempty(&requirements.providers, "Consumer requirement providers")?;
    validate_unique_nonempty(&requirements.files, "Consumer requirement files")?;
    for provider in &requirements.providers {
        validate_provider_identifier(provider, "Consumer requirement provider")?;
    }
    for file in &requirements.files {
        if file.starts_with("RELOC/") || file.starts_with("texmf-dist/") {
            return Err(PqtyError::Usage(format!(
                "Consumer requirement file is not a normalized TDS path: {file}"
            )));
        }
        validate_tds_path(file, "Consumer requirement file")?;
    }
    Ok(())
}

fn validate_resolved_consumer_requirements(lock: &LockFile) -> Result<(), PqtyError> {
    for provider in &lock.consumer_requirements.providers {
        let Some(entry) = lock
            .closure
            .iter()
            .find(|entry| entry.provider == *provider)
        else {
            return Err(PqtyError::Usage(format!(
                "Consumer requirement provider {provider} is absent from the closure"
            )));
        };
        if !entry.direct {
            return Err(PqtyError::Usage(format!(
                "Consumer requirement provider {provider} is not direct"
            )));
        }
    }
    for file in &lock.consumer_requirements.files {
        let owners = lock
            .closure
            .iter()
            .filter(|entry| entry.runtime_requests.iter().any(|request| request == file))
            .count();
        if owners != 1 {
            return Err(PqtyError::Usage(format!(
                "Consumer requirement file {file} must have exactly one recorded owner"
            )));
        }
    }
    Ok(())
}

fn validate_exact_contents(lock: &LockFile) -> Result<(), PqtyError> {
    fail_on_unresolved_packages(lock)?;
    let mut owners: BTreeMap<&str, &str> = BTreeMap::new();
    for entry in &lock.closure {
        let integrity = entry.integrity.as_deref().ok_or_else(|| {
            PqtyError::Usage(format!(
                "provider {} is not materialized; recreate the lock with `pqty lock`",
                entry.provider
            ))
        })?;
        let Some(encoded_integrity) = integrity.strip_prefix("sha256-") else {
            return Err(PqtyError::Usage(format!(
                "provider {} has unsupported integrity {}",
                entry.provider, integrity
            )));
        };
        let Ok(integrity_digest) = BASE64.decode(encoded_integrity) else {
            return Err(PqtyError::Usage(format!(
                "provider {} has invalid integrity {}",
                entry.provider, integrity
            )));
        };
        if integrity_digest.len() != 32 {
            return Err(PqtyError::Usage(format!(
                "provider {} has invalid integrity {}",
                entry.provider, integrity
            )));
        }
        let store_key = entry.store_key.as_deref().ok_or_else(|| {
            PqtyError::Usage(format!(
                "provider {} has no content-addressable store key",
                entry.provider
            ))
        })?;
        validate_sha256_colon(store_key, "provider store key")?;
        if store_key.strip_prefix("sha256:") != Some(hex::encode(integrity_digest).as_str()) {
            return Err(PqtyError::Usage(format!(
                "provider {} integrity does not match its store key",
                entry.provider
            )));
        }
        if entry.files.is_empty() {
            return Err(PqtyError::Usage(format!(
                "provider {} has an empty locked file index",
                entry.provider
            )));
        }
        let mut paths = BTreeSet::new();
        for file in &entry.files {
            if !paths.insert(file.tds_path.as_str()) {
                return Err(PqtyError::Usage(format!(
                    "provider {} contains duplicate path {}",
                    entry.provider, file.tds_path
                )));
            }
            validate_tds_path(&file.tds_path, "locked TDS path")?;
            if file.tds_path.starts_with("texmf-dist/") || file.tds_path.starts_with("RELOC/") {
                return Err(PqtyError::Usage(format!(
                    "provider {} contains non-canonical TDS path {}",
                    entry.provider, file.tds_path
                )));
            }
            if let Some(previous) = owners.insert(&file.tds_path, &entry.provider) {
                return Err(PqtyError::Usage(format!(
                    "locked TDS path {} is owned by both {} and {}",
                    file.tds_path, previous, entry.provider
                )));
            }
        }
    }

    for (kind, name, extension) in non_local_requests(lock) {
        let request = format!("{name}.{extension}");
        let owner = lock.closure.iter().find(|entry| {
            entry.satisfies.iter().any(|satisfied| satisfied == name)
                && entry.files.iter().any(|file| {
                    file.tds_path == request
                        || file
                            .tds_path
                            .strip_suffix(&request)
                            .is_some_and(|prefix| prefix.ends_with('/'))
                })
        });
        if owner.is_none() {
            return Err(PqtyError::Usage(format!(
                "exact lock does not trace non-local {kind} request {name} to a provider file"
            )));
        }
    }
    Ok(())
}

/// Validate materialized integrity metadata and provider file indexes.
///
/// # Errors
///
/// Returns an error unless the lock is a valid Exact Lock.
pub fn validate_materialized_lock(lock: &LockFile) -> Result<(), PqtyError> {
    if lock.stage != LockStage::Exact {
        return Err(PqtyError::Usage(format!(
            "operation requires an exact lock; found {:?} stage",
            lock.stage
        )));
    }
    validate_lock(lock)
}

fn validate_resolution_requests(lock: &LockFile) -> Result<(), PqtyError> {
    let satisfied = lock
        .closure
        .iter()
        .flat_map(|entry| entry.satisfies.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    for (kind, name, _) in non_local_requests(lock) {
        let unresolved = lock
            .unresolved
            .iter()
            .any(|record| record.kind == "package" && record.name == name);
        if !satisfied.contains(name) && !unresolved {
            return Err(PqtyError::Usage(format!(
                "{:?} lock does not account for non-local {kind} request {name}",
                lock.stage
            )));
        }
    }
    Ok(())
}

fn non_local_requests(lock: &LockFile) -> Vec<(&'static str, &str, &'static str)> {
    let mut requests = Vec::new();
    if let Some(class) = &lock.document_class
        && class.resolved_path.is_none()
    {
        requests.push(("class", class.name.as_str(), "cls"));
    }
    requests.extend(
        lock.loaded_classes
            .iter()
            .filter(|class| class.resolved_path.is_none())
            .map(|class| ("class", class.name.as_str(), "cls")),
    );
    requests.extend(
        lock.packages
            .iter()
            .filter(|package| package.resolved_path.is_none())
            .map(|package| ("package", package.name.as_str(), "sty")),
    );
    requests.extend(
        lock.bibliographies
            .iter()
            .filter(|record| {
                matches!(record.kind, BibliographyKind::BibliographyStyle)
                    && record.resolved_path.is_none()
            })
            .map(|record| ("bibliography-style", record.name.as_str(), "bst")),
    );
    requests
}

pub(crate) fn validate_trace(trace: &InputTrace) -> Result<(), PqtyError> {
    if trace.schema != TRACE_SCHEMA {
        return Err(PqtyError::Usage(format!(
            "unsupported trace schema {}; expected {TRACE_SCHEMA}",
            trace.schema
        )));
    }
    if trace.producer.as_deref() == Some("") {
        return Err(PqtyError::Usage(
            "trace producer cannot be empty".to_string(),
        ));
    }
    if let Some(fingerprint) = trace.environment_fingerprint.as_deref() {
        validate_sha256_colon(fingerprint, "trace environment fingerprint")?;
    }
    for input in &trace.inputs {
        if input.requested.is_empty() || input.requested.chars().any(char::is_control) {
            return Err(PqtyError::Usage(
                "trace requested name cannot be empty or contain control characters".to_string(),
            ));
        }
        if let Some(path) = input.resolved.as_deref() {
            validate_portable_path(path, "trace resolved path")?;
        }
    }
    Ok(())
}

pub(crate) fn validate_trace_report(report: &TraceReport) -> Result<(), PqtyError> {
    if report.schema != TRACE_REPORT_SCHEMA {
        return Err(PqtyError::Usage(format!(
            "unsupported trace report schema {}; expected {TRACE_REPORT_SCHEMA}",
            report.schema
        )));
    }
    validate_sha256_colon(
        &report.environment_fingerprint,
        "trace report environment fingerprint",
    )?;
    for matched in &report.matched {
        if invalid_artifact_text(&matched.requested) {
            return Err(PqtyError::Usage(
                "trace report match has an empty or invalid request".to_string(),
            ));
        }
        validate_tds_path(&matched.tds_path, "trace report matched TDS path")?;
        validate_provider_identifier(&matched.owner, "trace report matched owner")?;
    }
    Ok(())
}

fn validate_location(location: &Location, source_paths: &BTreeSet<&str>) -> Result<(), PqtyError> {
    validate_portable_path(&location.path, "source location")?;
    if !source_paths.contains(location.path.as_str()) {
        return Err(PqtyError::Usage(format!(
            "source location {} is absent from lock sources",
            location.path
        )));
    }
    if location.line == 0 {
        return Err(PqtyError::Usage(
            "source location line must be at least one".to_string(),
        ));
    }
    Ok(())
}

fn validate_local_source(
    path: &str,
    label: &str,
    source_paths: &BTreeSet<&str>,
) -> Result<(), PqtyError> {
    validate_portable_path(path, label)?;
    if !source_paths.contains(path) {
        return Err(PqtyError::Usage(format!(
            "{label} is absent from lock sources: {path}"
        )));
    }
    Ok(())
}

fn validate_unique_nonempty(values: &[String], label: &str) -> Result<(), PqtyError> {
    let mut unique = BTreeSet::new();
    for value in values {
        if invalid_artifact_text(value) || !unique.insert(value.as_str()) {
            return Err(PqtyError::Usage(format!(
                "{label} must contain unique, non-empty values without control characters"
            )));
        }
    }
    Ok(())
}

pub(crate) fn invalid_artifact_text(value: &str) -> bool {
    value.is_empty() || value.chars().any(char::is_control)
}

fn validate_sha256_colon(value: &str, label: &str) -> Result<(), PqtyError> {
    let digest = value.strip_prefix("sha256:").unwrap_or_default();
    if digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(PqtyError::Usage(format!("invalid {label}: {value}")))
    }
}

pub(crate) fn validate_portable_path(value: &str, label: &str) -> Result<(), PqtyError> {
    let bytes = value.as_bytes();
    let drive_letter = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let invalid = value.is_empty()
        || value.starts_with('/')
        || value.starts_with("//")
        || drive_letter
        || value.contains('\\')
        || value.chars().any(char::is_control)
        || value
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."));
    if invalid {
        Err(PqtyError::Usage(format!(
            "{label} is not a Portable Path: {value}"
        )))
    } else {
        Ok(())
    }
}

pub(crate) fn validate_provider_identifier(value: &str, label: &str) -> Result<(), PqtyError> {
    let valid = !matches!(value, "" | "." | "..")
        && value.len() <= 255
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'));
    if valid {
        Ok(())
    } else {
        Err(PqtyError::Usage(format!(
            "{label} is not a safe provider identifier: {value}"
        )))
    }
}

pub(crate) fn validate_tds_path(value: &str, label: &str) -> Result<(), PqtyError> {
    let normalized = value
        .strip_prefix("RELOC/")
        .or_else(|| value.strip_prefix("texmf-dist/"))
        .unwrap_or(value);
    validate_portable_path(normalized, label)?;
    if value.len() > 4096 || normalized.contains(':') {
        return Err(PqtyError::Usage(format!(
            "{label} is not a safe TDS path: {value}"
        )));
    }
    Ok(())
}
pub(crate) fn closure_by_provider(lock: &LockFile) -> BTreeMap<&str, &ResolvedPackage> {
    lock.closure
        .iter()
        .map(|entry| (entry.provider.as_str(), entry))
        .collect()
}
