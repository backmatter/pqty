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
    canonical_or_original, resolved_from, scan_commands, split_names, validate_materialized_lock,
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
    let _store_guard = super::transaction::FileGuard::store(&store_dir)?;
    let mut report = MaterializeReport::new(store_dir.clone(), None);
    let mut expanded_bytes = 0_u64;
    let mut discovery = RuntimeFileIndex::default();

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
            for request in &entry.runtime_requests {
                if !materialized.files.iter().any(|file| {
                    file.tds_path == *request
                        || (!request.contains('/')
                            && Path::new(&file.tds_path)
                                .file_name()
                                .and_then(|name| name.to_str())
                                == Some(request))
                }) {
                    return Err(PqtyError::Usage(format!(
                        "provider {} is missing requested runtime file {request}",
                        entry.provider
                    )));
                }
            }
            entry.integrity = Some(materialized.integrity);
            entry.store_key = Some(materialized.store_key);
            entry.files = materialized
                .files
                .into_iter()
                .map(|file| crate::LockedFile {
                    tds_path: file.tds_path,
                    kind: file.kind,
                })
                .collect();
        }
        let discovered = discovery.discover(lock, index, &store_dir)?;
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

// A hydration operation only grows its immutable provider set. Keep verified
// manifest indexes and compact parsed requests for that operation, never source
// text, a process-wide cache, or a time-based cache policy.
#[derive(Default)]
struct RuntimeFileIndex {
    closure: BTreeMap<String, String>,
    by_name: BTreeMap<String, Vec<(String, String)>>,
    by_path: BTreeMap<String, (String, String)>,
    requests: BTreeMap<String, Vec<Vec<String>>>,
}

impl RuntimeFileIndex {
    fn discover(
        &mut self,
        lock: &LockFile,
        index: &impl PackageRegistry,
        store: &Path,
    ) -> Result<BTreeMap<String, RuntimeProviderDiscovery>, PqtyError> {
        self.extend(lock, store)?;
        let queue = initial_runtime_queue(lock, self);
        discover_from_runtime_queue(index, store, self, queue)
    }

    fn extend(&mut self, lock: &LockFile, store: &Path) -> Result<(), PqtyError> {
        for entry in &lock.closure {
            if let Some(identity) = self.closure.get(&entry.provider) {
                if entry.integrity.as_ref() != Some(identity) {
                    return Err(PqtyError::Usage(
                        "provider identity changed within hydration".into(),
                    ));
                }
                continue;
            }
            let manifest = load_store_manifest(store, entry)?;
            for file in manifest.files {
                if let Some(name) = Path::new(&file.tds_path)
                    .file_name()
                    .and_then(|name| name.to_str())
                {
                    self.by_name
                        .entry(name.to_owned())
                        .or_default()
                        .push((entry.provider.clone(), file.tds_path.clone()));
                }
                self.by_path
                    .insert(file.tds_path, (entry.provider.clone(), file.digest));
            }
            self.closure.insert(
                entry.provider.clone(),
                entry
                    .integrity
                    .clone()
                    .ok_or_else(|| PqtyError::Usage("provider is not hydrated".into()))?,
            );
        }
        Ok(())
    }

    fn parsed_requests(&mut self, digest: &str, store: &Path) -> Result<&[Vec<String>], PqtyError> {
        if !self.requests.contains_key(digest) {
            let digest_hex = stored_file_digest(digest)?;
            let path = store.join(&digest_hex[..2]).join(digest_hex);
            let bytes = super::http::read_file_bounded(&path, crate::MAX_ARCHIVE_ENTRY_BYTES)?;
            // Cache only normalized, deduplicated request candidates; source
            // bytes and parser structures are released before processing more files.
            let requests = parse_runtime_requests(&String::from_utf8_lossy(&bytes));
            self.requests.insert(digest.to_owned(), requests);
        }
        Ok(&self.requests[digest])
    }
}

fn parse_runtime_requests(text: &str) -> Vec<Vec<String>> {
    scan_commands(text)
        .into_iter()
        .flat_map(|(_, command)| {
            runtime_file_requests(&command)
                .into_iter()
                .filter_map(|(request, extensions)| {
                    let name = Path::new(request.trim()).file_name()?.to_str()?;
                    if name.contains(['\\', '#', '{', '}']) {
                        return None;
                    }
                    let mut candidates = vec![name.to_owned()];
                    if Path::new(name).extension().is_none() {
                        candidates.extend(
                            extensions
                                .iter()
                                .map(|extension| format!("{name}.{extension}")),
                        );
                    }
                    Some(candidates)
                })
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn initial_runtime_queue(lock: &LockFile, files: &RuntimeFileIndex) -> VecDeque<String> {
    let mut queue = VecDeque::new();
    for entry in &lock.closure {
        if !entry.runtime_requests.is_empty() {
            for request in &entry.runtime_requests {
                if request.contains('/') {
                    if files
                        .by_path
                        .get(request)
                        .is_some_and(|(owner, _)| owner == &entry.provider)
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
        }
        if entry.direct {
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
        } else if entry.runtime_requests.is_empty() {
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
    files: &mut RuntimeFileIndex,
    mut queue: VecDeque<String>,
) -> Result<BTreeMap<String, RuntimeProviderDiscovery>, PqtyError> {
    let mut visited = BTreeSet::new();
    let mut discovered: BTreeMap<String, RuntimeProviderDiscovery> = BTreeMap::new();
    while let Some(path) = queue.pop_front() {
        if !visited.insert(path.clone()) {
            continue;
        }
        let Some((owner, digest)) = files.by_path.get(&path).cloned() else {
            continue;
        };
        if !is_scannable_runtime_path(&path) {
            continue;
        }
        // Re-evaluate requests against the current file index on every round;
        // only reading and parsing immutable bytes is cached.
        files.parsed_requests(&digest, store_dir)?;
        for candidates in &files.requests[&digest] {
            let mut found_locked = false;
            for candidate in candidates {
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
                discovery.parents.insert(owner.clone());
                discovery.files.insert(runtime_request.clone());
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
        "input" => command
            .required
            .iter()
            .flat_map(|value| split_names(value))
            .filter(|name| {
                !name.contains(['/', '\\', '#', '{', '}'])
                    && crate::validate_tds_path(name, "literal runtime input").is_ok()
            })
            .map(|name| (name, &["tex"][..]))
            .collect(),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_cache_reuses_content_only_inside_its_operation() {
        let store = tempfile::tempdir().unwrap();
        let bytes = br"\RequirePackage{alpha,alpha}\input plain";
        let digest = format!(
            "sha256:{}",
            super::super::object::store_reader(store.path(), &mut &bytes[..], None)
                .unwrap()
                .digest
        );
        let mut cache = RuntimeFileIndex::default();
        let requests = cache
            .parsed_requests(&digest, store.path())
            .unwrap()
            .to_vec();
        assert_eq!(requests.len(), 2);
        let hex = stored_file_digest(&digest).unwrap();
        let path = store.path().join(&hex[..2]).join(hex);
        // Hits need no second read. Fresh operations still consult the store.
        crate::tests::make_test_tree_writable(store.path());
        fs::remove_file(path).unwrap();
        assert_eq!(
            cache.parsed_requests(&digest, store.path()).unwrap(),
            requests
        );
        assert!(
            RuntimeFileIndex::default()
                .parsed_requests(&digest, store.path())
                .is_err()
        );
    }
}
