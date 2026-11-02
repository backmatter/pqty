//! Literal package inputs supplement project-owned source discovery.
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::Path,
};

use super::model::is_texmf_runfile;
use crate::{
    InputKind, InputRecord, LockFile, PackageRegistry, ResolvedPackage, UnresolvedRecord,
    normalize_runfile, resolved_from, validate_tds_path,
};

pub(crate) fn has_registry_inputs(lock: &LockFile) -> bool {
    lock.inputs
        .iter()
        .any(|input| literal_input(input).is_some())
}

fn literal_input(input: &InputRecord) -> Option<&str> {
    let name = input.name.trim();
    (matches!(input.kind, InputKind::Input)
        && input.resolved_path.is_none()
        && !name.contains(['\\', '#', '{', '}'])
        && validate_tds_path(name, "literal TeX input").is_ok())
    .then_some(name)
}

pub(super) fn resolve_inputs(
    lock: &mut LockFile,
    index: &impl PackageRegistry,
    registry_id: &str,
    closure: &mut BTreeMap<String, ResolvedPackage>,
    queue: &mut VecDeque<String>,
) {
    for input in &lock.inputs {
        let Some(name) = literal_input(input) else {
            continue;
        };
        let mut candidates = input_files(index, name);
        if candidates.is_empty() && Path::new(name).extension().is_none() {
            candidates = input_files(index, &format!("{name}.tex"));
        }
        if candidates.is_empty() {
            continue;
        }
        lock.unresolved.retain(|record| {
            !(record.kind == "input"
                && record.name == input.name
                && record.source.path == input.source.path
                && record.source.line == input.source.line)
        });
        if let [(provider, path)] = candidates.as_slice() {
            let entry = closure
                .entry(provider.clone())
                .or_insert_with(|| resolved_from(index, provider, registry_id));
            entry.direct = true;
            if !entry.runtime_requests.contains(path) {
                entry.runtime_requests.push(path.clone());
            }
            if !queue.contains(provider) {
                queue.push_back(provider.clone());
            }
        } else {
            lock.unresolved.push(UnresolvedRecord {
                kind: "package".into(),
                name: input.name.clone(),
                source: input.source.clone(),
                candidates: candidates
                    .into_iter()
                    .map(|(provider, _)| provider)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            });
        }
    }
}

fn input_files(index: &impl PackageRegistry, request: &str) -> Vec<(String, String)> {
    let request = normalize_runfile(request).1;
    let providers = if request.contains('/') {
        index.providers_of_path(&request)
    } else {
        index.providers_of_file(&request)
    };
    let mut files = Vec::new();
    for provider in providers {
        if let Some(package) = index.package(provider) {
            for file in &package.runfiles {
                if !is_texmf_runfile(file) {
                    continue;
                }
                let path = normalize_runfile(file).1;
                if (request.contains('/') && path == request)
                    || (!request.contains('/') && path.rsplit('/').next() == Some(request.as_str()))
                {
                    files.push((provider.to_string(), path));
                }
            }
        }
    }
    files
}
