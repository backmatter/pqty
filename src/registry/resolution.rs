use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

use crate::registry::model::{IndexPackage, PackageRegistry};
use crate::registry::tlpdb::TlpdbIndex;
use crate::{
    BibliographyKind, ConsumerRequirements, Location, LockFile, LockStage, PackageSource,
    PackageSourceKind, PqtyError, ResolvedPackage, UnresolvedRecord, normalize_runfile,
    validate_provider_identifier, validate_tds_path,
};

pub(crate) fn resolved_from(
    index: &impl PackageRegistry,
    provider: &str,
    registry_id: &str,
) -> ResolvedPackage {
    let meta = index.package(provider);
    let (version, dependencies) = meta.map_or_else(
        || ("tlrev:unknown".to_string(), Vec::new()),
        |meta| {
            (
                meta.version.clone(),
                meta.depends
                    .iter()
                    .filter(|dependency| {
                        index
                            .package(dependency)
                            .is_some_and(IndexPackage::belongs_to_package_layer)
                    })
                    .cloned()
                    .collect(),
            )
        },
    );
    let (container_checksum, container_size) = meta.map_or((None, None), |meta| {
        (meta.container_checksum.clone(), meta.container_size)
    });
    ResolvedPackage {
        provider: provider.to_string(),
        version,
        source: PackageSource {
            registry: Some(registry_id.to_string()),
            kind: PackageSourceKind::Registry,
            locator: provider.to_string(),
            container_checksum,
            container_size,
        },
        satisfies: Vec::new(),
        integrity: None,
        dependencies,
        direct: false,
        requested_by: Vec::new(),
        store_key: None,
        engines: Vec::new(),
        font_maps: meta.map_or_else(Vec::new, |meta| meta.font_maps.clone()),
        runtime_requests: Vec::new(),
        files: Vec::new(),
    }
}

/// Resolve every non-local class, package, and bibliography-style request
/// against a package registry.
///
/// The function replaces the lock's registry provenance and provider closure,
/// then advances it to [`LockStage::Resolved`]. Requests with no unique
/// provider are retained in [`LockFile::unresolved`] rather than causing an
/// immediate error; call [`crate::validate_lock`] or use the higher-level CLI
/// before treating the result as complete.
pub fn resolve(lock: &mut LockFile, index: &impl PackageRegistry) {
    // The scanned manifest, paired with the file extensions each request can match.
    let mut requests: Vec<(String, Vec<&str>, Location)> = Vec::new();
    lock.unresolved.retain(|item| item.kind != "package");
    if let Some(class) = &lock.document_class
        && class.resolved_path.is_none()
    {
        requests.push((class.name.clone(), vec!["cls"], class.source.clone()));
    }
    for class in lock
        .loaded_classes
        .iter()
        .filter(|class| class.resolved_path.is_none())
    {
        requests.push((class.name.clone(), vec!["cls"], class.source.clone()));
    }
    for package in lock
        .packages
        .iter()
        .filter(|package| package.resolved_path.is_none())
    {
        requests.push((package.name.clone(), vec!["sty"], package.source.clone()));
    }
    // Bibliography styles are dependencies too: `\bibliographystyle{plain}`
    // needs plain.bst in the closure so a consumer needs no system .bst.
    for bib in &lock.bibliographies {
        if matches!(bib.kind, BibliographyKind::BibliographyStyle) && bib.resolved_path.is_none() {
            requests.push((bib.name.clone(), vec!["bst"], bib.source.clone()));
        }
    }

    let registry_id = index.registry().id;
    let mut closure: BTreeMap<String, ResolvedPackage> = BTreeMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    for (name, extensions, source) in &requests {
        let providers = index.providers_of(name, extensions);
        match providers.as_slice() {
            [provider] => {
                let entry = closure
                    .entry((*provider).to_string())
                    .or_insert_with(|| resolved_from(index, provider, &registry_id));
                entry.direct = true;
                if !entry.satisfies.contains(name) {
                    entry.satisfies.push(name.clone());
                }
                if !queue.contains(&(*provider).to_string()) {
                    queue.push_back((*provider).to_string());
                }
            }
            providers => lock.unresolved.push(UnresolvedRecord {
                kind: "package".to_string(),
                name: name.clone(),
                source: source.clone(),
                candidates: providers
                    .iter()
                    .map(|provider| (*provider).to_string())
                    .collect(),
            }),
        }
    }

    // Expand tlpdb `depend` edges into the transitive closure.
    while let Some(provider) = queue.pop_front() {
        let deps = closure
            .get(&provider)
            .map(|entry| entry.dependencies.clone())
            .unwrap_or_default();
        for dep in deps {
            // Only expand deps that are real packages (skip arch-/collection-only
            // pseudo-deps); the edge is still recorded in `dependencies`.
            if index
                .package(&dep)
                .is_none_or(|package| !package.belongs_to_package_layer())
            {
                continue;
            }
            if let Some(entry) = closure.get_mut(&dep) {
                if !entry.requested_by.contains(&provider) {
                    entry.requested_by.push(provider.clone());
                }
            } else {
                let mut entry = resolved_from(index, &dep, &registry_id);
                entry.requested_by.push(provider.clone());
                closure.insert(dep.clone(), entry);
                queue.push_back(dep);
            }
        }
    }

    lock.registries = vec![index.registry()];
    lock.closure = closure.into_values().collect();
    lock.stage = LockStage::Resolved;
}

/// Add generic runtime file/provider requirements to an already resolved lock.
///
/// This is deliberately engine-neutral: a caller supplies registry filenames
/// or provider names, while pqty owns resolution, provenance, and hydration.
/// Add explicitly requested providers or runtime files to an existing lock.
///
/// # Errors
///
/// Returns an error when a provider is unknown or a file request has no unique
/// owner. Exact TDS paths may be used to disambiguate shared basenames.
pub fn require_runtime(
    lock: &mut LockFile,
    index: &TlpdbIndex,
    files: &[String],
    providers: &[String],
) -> Result<(), PqtyError> {
    let registry = index.registry();
    validate_runtime_registry(lock, &registry)?;
    let mut closure = lock
        .closure
        .clone()
        .into_iter()
        .map(|entry| (entry.provider.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut queue = VecDeque::new();

    add_required_providers(index, &registry.id, providers, &mut closure, &mut queue)?;
    add_required_files(index, &registry.id, files, &mut closure, &mut queue)?;
    expand_runtime_dependencies(index, &registry.id, &mut closure, &mut queue);
    normalize_runtime_closure(&mut closure);

    lock.registries = vec![registry];
    lock.closure = closure.into_values().collect();
    merge_consumer_requirements(lock, files, providers)?;
    lock.stage = LockStage::Resolved;
    Ok(())
}

fn validate_runtime_registry(lock: &LockFile, registry: &crate::Registry) -> Result<(), PqtyError> {
    if !lock.registries.is_empty()
        && lock
            .registries
            .iter()
            .all(|locked| locked.metadata_digest != registry.metadata_digest)
    {
        return Err(PqtyError::Usage(
            "runtime requirements cannot change the lock's registry snapshot".to_string(),
        ));
    }
    Ok(())
}

fn add_required_providers(
    index: &TlpdbIndex,
    registry_id: &str,
    providers: &[String],
    closure: &mut BTreeMap<String, ResolvedPackage>,
    queue: &mut VecDeque<String>,
) -> Result<(), PqtyError> {
    for provider in providers {
        let provider = provider.trim();
        validate_provider_identifier(provider, "required provider")?;
        if index.package(provider).is_none() {
            return Err(PqtyError::Usage(format!(
                "required provider is absent from the selected registry: {provider}"
            )));
        }
        let entry = closure
            .entry(provider.to_string())
            .or_insert_with(|| resolved_from(index, provider, registry_id));
        entry.direct = true;
        if !queue.contains(&provider.to_string()) {
            queue.push_back(provider.to_string());
        }
    }
    Ok(())
}

fn add_required_files(
    index: &TlpdbIndex,
    registry_id: &str,
    files: &[String],
    closure: &mut BTreeMap<String, ResolvedPackage>,
    queue: &mut VecDeque<String>,
) -> Result<(), PqtyError> {
    for file in files {
        let file = file.trim();
        validate_tds_path(file, "required runtime file")?;
        let normalized = normalize_runfile(file).1;
        let candidates = if normalized.contains('/') {
            index.providers_of_path(&normalized)
        } else {
            index.providers_of_file(&normalized)
        };
        let provider = match candidates.as_slice() {
            [provider] => *provider,
            [] => {
                return Err(PqtyError::Usage(format!(
                    "no provider in the selected registry ships required runtime file {file}"
                )));
            }
            providers => {
                return Err(PqtyError::Usage(format!(
                    "required runtime file {file} is ambiguous; use its exact TDS path\n  providers: {}",
                    providers.join(", ")
                )));
            }
        };
        let entry = closure
            .entry(provider.to_string())
            .or_insert_with(|| resolved_from(index, provider, registry_id));
        entry.direct = true;
        let request = if normalized.contains('/') {
            normalized.clone()
        } else {
            Path::new(&normalized)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&normalized)
                .to_string()
        };
        if !entry.runtime_requests.contains(&request) {
            entry.runtime_requests.push(request);
        }
        if !queue.contains(&provider.to_string()) {
            queue.push_back(provider.to_string());
        }
    }
    Ok(())
}

fn expand_runtime_dependencies(
    index: &TlpdbIndex,
    registry_id: &str,
    closure: &mut BTreeMap<String, ResolvedPackage>,
    queue: &mut VecDeque<String>,
) {
    while let Some(provider) = queue.pop_front() {
        let dependencies = closure
            .get(&provider)
            .map(|entry| entry.dependencies.clone())
            .unwrap_or_default();
        for dependency in dependencies {
            if index
                .package(&dependency)
                .is_none_or(|package| !package.belongs_to_package_layer())
            {
                continue;
            }
            if let Some(entry) = closure.get_mut(&dependency) {
                if !entry.requested_by.contains(&provider) {
                    entry.requested_by.push(provider.clone());
                }
            } else {
                let mut entry = resolved_from(index, &dependency, registry_id);
                entry.requested_by.push(provider.clone());
                closure.insert(dependency.clone(), entry);
                queue.push_back(dependency);
            }
        }
    }
}

fn normalize_runtime_closure(closure: &mut BTreeMap<String, ResolvedPackage>) {
    for entry in closure.values_mut() {
        entry.requested_by.sort();
        entry.requested_by.dedup();
        entry.runtime_requests.sort();
        entry.runtime_requests.dedup();
    }
}

fn merge_consumer_requirements(
    lock: &mut LockFile,
    files: &[String],
    providers: &[String],
) -> Result<(), PqtyError> {
    let requirements = normalized_consumer_requirements(files, providers)?;
    for provider in requirements.providers {
        if !lock
            .consumer_requirements
            .providers
            .iter()
            .any(|existing| existing == &provider)
        {
            lock.consumer_requirements.providers.push(provider);
        }
    }
    for file in requirements.files {
        if !lock
            .consumer_requirements
            .files
            .iter()
            .any(|existing| existing == &file)
        {
            lock.consumer_requirements.files.push(file);
        }
    }
    lock.consumer_requirements.providers.sort();
    lock.consumer_requirements.files.sort();
    Ok(())
}

pub(crate) fn normalized_consumer_requirements(
    files: &[String],
    providers: &[String],
) -> Result<ConsumerRequirements, PqtyError> {
    let mut requirements = ConsumerRequirements::default();
    for provider in providers {
        let provider = provider.trim();
        validate_provider_identifier(provider, "required provider")?;
        requirements.providers.push(provider.to_string());
    }
    for file in files {
        let file = file.trim();
        validate_tds_path(file, "required runtime file")?;
        requirements.files.push(normalize_runfile(file).1);
    }
    requirements.providers.sort();
    requirements.providers.dedup();
    requirements.files.sort();
    requirements.files.dedup();
    Ok(requirements)
}
