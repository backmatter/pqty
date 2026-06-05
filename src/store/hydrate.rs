use crate::store::install::stored_file_digest;
use crate::store::object::{
    emit_hydration_download_plan, load_store_manifest, materialize_provider,
    validate_closure_resource_limits,
};
use crate::store::{MaterializeReport, PackageByteSource};

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::Path;

use crate::{
    LockFile, LockStage, MAX_CLOSURE_EXPANDED_BYTES, PackageRegistry, ParsedCommand, PqtyError,
    ResolvedPackage, canonical_or_original, resolved_from, scan_commands, split_names,
    validate_materialized_lock,
};

/// Resolve package bytes into the shared store and fill all integrity/file
/// records without creating a consumer-specific renderer tree. Remote
/// containers must contain every declared runfile; local distribution
/// snapshots record the subset actually installed.
/// Fetch or copy every provider file into the content-addressed store, attach
/// its path index and one package integrity digest to the lock, and keep the
/// per-file digest manifest in the shared store.
///
/// # Errors
///
/// Returns an error when provider bytes are unavailable, exceed configured
/// limits, fail integrity checks, or cannot be stored.
pub fn hydrate_lock(
    lock: &mut LockFile,
    index: &impl PackageRegistry,
    source: &PackageByteSource,
    store_dir: &Path,
) -> Result<MaterializeReport, PqtyError> {
    fs::create_dir_all(store_dir).map_err(|source| PqtyError::Io {
        path: store_dir.to_path_buf(),
        source,
    })?;
    let store_dir = canonical_or_original(store_dir);
    let mut report = MaterializeReport::new(store_dir.clone(), None);
    let mut expanded_bytes = 0_u64;

    loop {
        validate_closure_resource_limits(lock, index)?;
        {
            let pending = lock
                .closure
                .iter()
                .filter(|entry| entry.integrity.is_none())
                .collect::<Vec<_>>();
            emit_hydration_download_plan(source, &pending)?;
        }
        for entry in lock
            .closure
            .iter_mut()
            .filter(|entry| entry.integrity.is_none())
        {
            let materialized = materialize_provider(
                index,
                &entry.provider,
                entry.source.container_checksum.as_deref(),
                entry.source.container_size,
                source,
                &store_dir,
                &mut report,
            )?;
            expanded_bytes = expanded_bytes
                .checked_add(materialized.expanded_bytes)
                .ok_or_else(|| PqtyError::Usage("closure byte count overflow".to_string()))?;
            if expanded_bytes > MAX_CLOSURE_EXPANDED_BYTES {
                return Err(PqtyError::Usage(format!(
                    "provider closure exceeds the {MAX_CLOSURE_EXPANDED_BYTES}-byte expanded-content limit"
                )));
            }
            entry.integrity = Some(materialized.integrity);
            entry.store_key = Some(materialized.store_key);
            entry.files = materialized.files;
        }
        let discovered = discover_runtime_providers(lock, index, &store_dir)?;
        if add_runtime_providers(lock, index, discovered) == 0 {
            break;
        }
    }
    if report.missing > 0 {
        return Err(PqtyError::Usage(format!(
            "cannot create an exact package environment: {} registry runfile(s) are missing",
            report.missing
        )));
    }
    lock.stage = LockStage::Exact;
    if let Err(error) = validate_materialized_lock(lock) {
        lock.stage = LockStage::Resolved;
        return Err(error);
    }
    Ok(report)
}

fn discover_runtime_providers(
    lock: &LockFile,
    index: &impl PackageRegistry,
    store_dir: &Path,
) -> Result<BTreeMap<String, RuntimeProviderDiscovery>, PqtyError> {
    let files = build_runtime_file_index(lock, store_dir)?;
    let queue = initial_runtime_queue(lock, &files);
    discover_from_runtime_queue(index, store_dir, &files, queue)
}

struct RuntimeFileIndex<'a> {
    closure: BTreeMap<&'a str, &'a ResolvedPackage>,
    by_name: BTreeMap<String, Vec<(String, String)>>,
    by_path: BTreeMap<String, (&'a ResolvedPackage, String)>,
}

fn build_runtime_file_index<'a>(
    lock: &'a LockFile,
    store_dir: &Path,
) -> Result<RuntimeFileIndex<'a>, PqtyError> {
    let closure = lock
        .closure
        .iter()
        .map(|entry| (entry.provider.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut by_name: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut by_path: BTreeMap<String, (&ResolvedPackage, String)> = BTreeMap::new();
    for entry in &lock.closure {
        let manifest = load_store_manifest(store_dir, entry)?;
        let digests = manifest
            .files
            .into_iter()
            .map(|file| (file.tds_path, file.digest))
            .collect::<BTreeMap<_, _>>();
        for file in &entry.files {
            if let Some(name) = Path::new(&file.tds_path)
                .file_name()
                .and_then(|name| name.to_str())
            {
                by_name
                    .entry(name.to_string())
                    .or_default()
                    .push((entry.provider.clone(), file.tds_path.clone()));
            }
            let digest = digests.get(&file.tds_path).ok_or_else(|| {
                PqtyError::Usage(format!(
                    "package store manifest for {} omits {}",
                    entry.provider, file.tds_path
                ))
            })?;
            by_path.insert(file.tds_path.clone(), (entry, digest.clone()));
        }
    }
    Ok(RuntimeFileIndex {
        closure,
        by_name,
        by_path,
    })
}

fn initial_runtime_queue(lock: &LockFile, files: &RuntimeFileIndex<'_>) -> VecDeque<String> {
    let mut queue = VecDeque::new();
    for entry in &lock.closure {
        if !entry.runtime_requests.is_empty() {
            for request in &entry.runtime_requests {
                if request.contains('/') {
                    if files
                        .by_path
                        .get(request)
                        .is_some_and(|(owner, _)| owner.provider == entry.provider)
                    {
                        queue.push_back(request.clone());
                    }
                } else if let Some(paths) = files.by_name.get(request) {
                    queue.extend(
                        paths
                            .iter()
                            .filter(|(owner, _)| owner == &entry.provider)
                            .map(|(_, path)| path.clone()),
                    );
                }
            }
        } else if entry.direct {
            for request in &entry.satisfies {
                for extension in ["sty", "cls", "bst"] {
                    let name = format!("{request}.{extension}");
                    if let Some(paths) = files.by_name.get(&name) {
                        queue.extend(
                            paths
                                .iter()
                                .filter(|(owner, _)| owner == &entry.provider)
                                .map(|(_, path)| path.clone()),
                        );
                    }
                }
            }
        } else {
            queue.extend(
                entry
                    .files
                    .iter()
                    .filter(|file| is_scannable_runtime_path(&file.tds_path))
                    .map(|file| file.tds_path.clone()),
            );
        }
    }
    queue
}

fn discover_from_runtime_queue(
    index: &impl PackageRegistry,
    store_dir: &Path,
    files: &RuntimeFileIndex<'_>,
    mut queue: VecDeque<String>,
) -> Result<BTreeMap<String, RuntimeProviderDiscovery>, PqtyError> {
    let mut visited = BTreeSet::new();
    let mut discovered: BTreeMap<String, RuntimeProviderDiscovery> = BTreeMap::new();
    while let Some(path) = queue.pop_front() {
        if !visited.insert(path.clone()) {
            continue;
        }
        let Some((owner, digest)) = files.by_path.get(&path) else {
            continue;
        };
        if !is_scannable_runtime_path(&path) {
            continue;
        }
        let digest = stored_file_digest(digest)?;
        let bytes = fs::read(store_dir.join(&digest[..2]).join(digest)).map_err(|source| {
            PqtyError::Io {
                path: store_dir.join(&digest[..2]).join(digest),
                source,
            }
        })?;
        let text = String::from_utf8_lossy(&bytes);
        for (_, command) in scan_commands(&text) {
            for (request, extensions) in runtime_file_requests(&command) {
                let mut candidates = Vec::new();
                let request_path = Path::new(request.trim());
                let Some(name) = request_path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if name.contains(['\\', '#', '{', '}']) {
                    continue;
                }
                candidates.push(name.to_string());
                if Path::new(name).extension().is_none() {
                    candidates.extend(
                        extensions
                            .iter()
                            .map(|extension| format!("{name}.{extension}")),
                    );
                }

                let mut found_locked = false;
                for candidate in &candidates {
                    if let Some(paths) = files.by_name.get(candidate) {
                        queue.extend(paths.iter().map(|(_, path)| path.clone()));
                        found_locked = true;
                    }
                }
                if found_locked {
                    continue;
                }
                if let Some((runtime_request, provider)) = candidates.iter().find_map(|candidate| {
                    index
                        .provider_of_file(candidate)
                        .map(|provider| (candidate, provider))
                }) && !files.closure.contains_key(provider)
                {
                    let discovery = discovered.entry(provider.to_string()).or_default();
                    discovery.parents.insert(owner.provider.clone());
                    discovery.files.insert(runtime_request.clone());
                }
            }
        }
    }
    Ok(discovered)
}

#[derive(Default)]
struct RuntimeProviderDiscovery {
    parents: BTreeSet<String>,
    files: BTreeSet<String>,
}

fn runtime_file_requests(command: &ParsedCommand) -> Vec<(String, &'static [&'static str])> {
    match command.name.as_str() {
        "usepackage" | "RequirePackage" => command
            .required
            .iter()
            .flat_map(|value| split_names(value))
            .map(|name| (name, &["sty"][..]))
            .collect(),
        "documentclass" | "LoadClass" => command
            .required
            .iter()
            .flat_map(|value| split_names(value))
            .map(|name| (name, &["cls"][..]))
            .collect(),
        "IfFileExists" | "InputIfFileExists" => command
            .required
            .first()
            .map(|name| vec![(name.clone(), &[][..])])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn is_scannable_runtime_path(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str()),
        Some("tex" | "sty" | "cls" | "def" | "cfg" | "clo" | "ltx" | "mkii")
    )
}

fn add_runtime_providers(
    lock: &mut LockFile,
    index: &impl PackageRegistry,
    discovered: BTreeMap<String, RuntimeProviderDiscovery>,
) -> usize {
    let registry_id = index.registry().id;
    let mut closure = std::mem::take(&mut lock.closure)
        .into_iter()
        .map(|entry| (entry.provider.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let before = closure.len();
    let mut queue = VecDeque::new();

    for (provider, discovery) in discovered {
        if let Some(entry) = closure.get_mut(&provider) {
            for parent in discovery.parents {
                if !entry.requested_by.contains(&parent) {
                    entry.requested_by.push(parent);
                }
            }
            for file in discovery.files {
                if !entry.runtime_requests.contains(&file) {
                    entry.runtime_requests.push(file);
                }
            }
        } else if index.package(&provider).is_some() {
            let mut entry = resolved_from(index, &provider, &registry_id);
            entry.requested_by.extend(discovery.parents);
            entry.runtime_requests.extend(discovery.files);
            eprintln!(
                "pqty: adding {} for runtime file {}",
                provider,
                entry.runtime_requests.join(", ")
            );
            closure.insert(provider.clone(), entry);
            queue.push_back(provider);
        }
    }

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
                let mut entry = resolved_from(index, &dependency, &registry_id);
                entry.requested_by.push(provider.clone());
                closure.insert(dependency.clone(), entry);
                queue.push_back(dependency);
            }
        }
    }

    let added = closure.len() - before;
    lock.closure = closure.into_values().collect();
    if added > 0 {
        lock.stage = LockStage::Resolved;
    }
    added
}

pub(crate) fn add_trace_providers(
    lock: &mut LockFile,
    index: &impl PackageRegistry,
    providers: BTreeMap<String, BTreeSet<String>>,
) {
    let registry_id = index.registry().id;
    let mut closure = std::mem::take(&mut lock.closure)
        .into_iter()
        .map(|entry| (entry.provider.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut queue = VecDeque::new();

    for (provider, requests) in providers {
        let mut entry = resolved_from(index, &provider, &registry_id);
        entry.direct = true;
        entry.runtime_requests.extend(requests);
        closure.insert(provider.clone(), entry);
        queue.push_back(provider);
    }

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
                    entry.requested_by.sort();
                }
            } else {
                let mut entry = resolved_from(index, &dependency, &registry_id);
                entry.requested_by.push(provider.clone());
                closure.insert(dependency.clone(), entry);
                queue.push_back(dependency);
            }
        }
    }

    lock.closure = closure.into_values().collect();
    lock.stage = LockStage::Resolved;
}
