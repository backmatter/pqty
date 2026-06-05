use crate::{
    BTreeMap, BTreeSet, CONVERGENCE_REPORT_SCHEMA, ConvergenceReport, ConvergenceStatus,
    ConvergenceUnresolvedReason, InputTrace, LockFile, ObservedInput, PackageByteSource,
    PackageRegistry, Path, PqtyError, ResolvedEnvironment, ResourceKind, UnresolvedTraceInput,
    add_trace_providers, font_filename_is_case_folded, hydrate_lock, normalize_trace_path,
};

/// Extend an exact lock with package inputs observed at runtime.
///
/// The operation is transactional: `lock` is changed only when every missing
/// package input resolves against the same registry metadata as the existing
/// lock, all added providers hydrate successfully, and the resulting
/// environment satisfies the complete trace. Callers materialize the updated
/// lock and run their renderer again until this reports `stable`.
///
/// # Errors
///
/// Returns an error when the trace is stale, registry data cannot be resolved,
/// provider bytes cannot be verified, or the candidate environment is invalid.
pub fn converge_trace(
    lock: &mut LockFile,
    trace: &InputTrace,
    index: &impl PackageRegistry,
    source: &PackageByteSource,
    store_dir: &Path,
) -> Result<ConvergenceReport, PqtyError> {
    if let Some(report) = stable_convergence_report(lock, trace)? {
        return Ok(report);
    }
    let previous_environment = ResolvedEnvironment::from_lock(lock)?;
    let initial = previous_environment.reconcile_trace(trace)?;

    validate_convergence_registry(lock, index)?;
    let locked_providers = lock
        .closure
        .iter()
        .map(|entry| entry.provider.as_str())
        .collect::<BTreeSet<_>>();
    let (providers, unresolved) =
        resolve_missing_inputs(index, &initial.missing, &locked_providers);

    if !unresolved.is_empty() {
        return Ok(ConvergenceReport {
            schema: CONVERGENCE_REPORT_SCHEMA.to_string(),
            status: ConvergenceStatus::Unresolved,
            previous_environment_fingerprint: previous_environment.fingerprint.clone(),
            environment_fingerprint: previous_environment.fingerprint,
            added_providers: Vec::new(),
            matched: initial.matched,
            unresolved,
            ignored: initial.ignored,
        });
    }

    let previous_providers = locked_providers
        .into_iter()
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    let mut candidate_lock = lock.clone();
    add_trace_providers(&mut candidate_lock, index, providers);
    hydrate_lock(&mut candidate_lock, index, source, store_dir)?;

    let environment = ResolvedEnvironment::from_lock(&candidate_lock)?;
    // The trace identifies the previous environment (validated above). Check
    // its observations against the candidate environment to prove that the
    // proposed additions actually satisfy it.
    let reconciled = environment.reconcile_trace_inputs(trace);
    if !reconciled.missing.is_empty() {
        let unresolved = reconciled
            .missing
            .into_iter()
            .map(|input| provider_did_not_satisfy(index, input))
            .collect();
        return Ok(ConvergenceReport {
            schema: CONVERGENCE_REPORT_SCHEMA.to_string(),
            status: ConvergenceStatus::Unresolved,
            previous_environment_fingerprint: previous_environment.fingerprint.clone(),
            environment_fingerprint: previous_environment.fingerprint,
            added_providers: Vec::new(),
            matched: initial.matched,
            unresolved,
            ignored: initial.ignored,
        });
    }

    let added_providers = candidate_lock
        .closure
        .iter()
        .map(|entry| entry.provider.clone())
        .filter(|provider| !previous_providers.contains(provider))
        .collect();
    let report = ConvergenceReport {
        schema: CONVERGENCE_REPORT_SCHEMA.to_string(),
        status: ConvergenceStatus::Changed,
        previous_environment_fingerprint: previous_environment.fingerprint,
        environment_fingerprint: environment.fingerprint,
        added_providers,
        matched: reconciled.matched,
        unresolved: Vec::new(),
        ignored: reconciled.ignored,
    };
    *lock = candidate_lock;
    Ok(report)
}

fn resolve_missing_inputs(
    index: &impl PackageRegistry,
    missing: &[ObservedInput],
    locked_providers: &BTreeSet<&str>,
) -> (
    BTreeMap<String, BTreeSet<String>>,
    Vec<UnresolvedTraceInput>,
) {
    let mut providers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut unresolved = Vec::new();
    for input in missing {
        let Some((candidate, request, candidates)) = trace_provider_candidates(index, input) else {
            unresolved.push(UnresolvedTraceInput {
                input: input.clone(),
                reason: ConvergenceUnresolvedReason::InvalidRequest,
                candidate: None,
                provider: None,
                candidates: Vec::new(),
            });
            continue;
        };
        let provider = match candidates.as_slice() {
            [provider] => *provider,
            [] => {
                unresolved.push(UnresolvedTraceInput {
                    input: input.clone(),
                    reason: ConvergenceUnresolvedReason::NoProvider,
                    candidate: Some(candidate),
                    provider: None,
                    candidates: Vec::new(),
                });
                continue;
            }
            providers => {
                unresolved.push(UnresolvedTraceInput {
                    input: input.clone(),
                    reason: ConvergenceUnresolvedReason::AmbiguousProvider,
                    candidate: Some(candidate),
                    provider: None,
                    candidates: providers
                        .iter()
                        .map(|provider| (*provider).to_string())
                        .collect(),
                });
                continue;
            }
        };
        if locked_providers.contains(provider) {
            unresolved.push(UnresolvedTraceInput {
                input: input.clone(),
                reason: ConvergenceUnresolvedReason::ProviderAlreadyLocked,
                candidate: Some(candidate),
                provider: Some(provider.to_string()),
                candidates: Vec::new(),
            });
            continue;
        }
        providers
            .entry(provider.to_string())
            .or_default()
            .insert(request);
    }
    (providers, unresolved)
}

fn provider_did_not_satisfy(
    index: &impl PackageRegistry,
    input: ObservedInput,
) -> UnresolvedTraceInput {
    let lookup = trace_provider_candidates(index, &input);
    let candidate = lookup.as_ref().map(|(candidate, _, _)| candidate.clone());
    let candidates = lookup
        .as_ref()
        .map(|(_, _, providers)| {
            providers
                .iter()
                .map(|provider| (*provider).to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let provider = (candidates.len() == 1).then(|| candidates[0].clone());
    UnresolvedTraceInput {
        input,
        reason: ConvergenceUnresolvedReason::ProviderDidNotSatisfy,
        candidate,
        provider,
        candidates,
    }
}

pub(crate) fn stable_convergence_report(
    lock: &LockFile,
    trace: &InputTrace,
) -> Result<Option<ConvergenceReport>, PqtyError> {
    let environment = ResolvedEnvironment::from_lock(lock)?;
    let reconciled = environment.reconcile_trace(trace)?;
    if !reconciled.missing.is_empty() {
        return Ok(None);
    }
    Ok(Some(ConvergenceReport {
        schema: CONVERGENCE_REPORT_SCHEMA.to_string(),
        status: ConvergenceStatus::Stable,
        previous_environment_fingerprint: environment.fingerprint.clone(),
        environment_fingerprint: environment.fingerprint,
        added_providers: Vec::new(),
        matched: reconciled.matched,
        unresolved: Vec::new(),
        ignored: reconciled.ignored,
    }))
}

pub(crate) fn trace_provider_candidates<'a>(
    index: &'a impl PackageRegistry,
    input: &ObservedInput,
) -> Option<(String, String, Vec<&'a str>)> {
    if input.resolved.is_none() && matches!(input.kind.as_ref(), Some(ResourceKind::FontFamily)) {
        let matches = index.font_file_candidates(&input.requested);
        let candidate = match matches.as_slice() {
            [(filename, _)] => (*filename).to_string(),
            _ => input.requested.clone(),
        };
        let request = Path::new(&candidate)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty() && *name != "." && *name != "..")?
            .to_string();
        let providers = matches.into_iter().map(|(_, provider)| provider).collect();
        return Some((candidate, request, providers));
    }
    let (mut candidate, mut providers) = if let Some(path) = input.resolved.as_deref() {
        let normalized = normalize_trace_path(path);
        let providers = index.providers_of_path(&normalized);
        (normalized, providers)
    } else {
        let request = input.requested.replace('\\', "/");
        let providers = index.providers_of_file(&request);
        (request, providers)
    };
    if providers.is_empty() && font_filename_is_case_folded(input.kind.as_ref()) {
        let folded = candidate.to_ascii_lowercase();
        providers = if input.resolved.is_some() {
            index.providers_of_path(&folded)
        } else {
            index.providers_of_file(&folded)
        };
        if !providers.is_empty() {
            candidate = folded;
        }
    }
    let raw = candidate.as_str();
    if raw.is_empty() || raw.contains(['#', '{', '}']) {
        return None;
    }
    let request = Path::new(raw)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .map(ToString::to_string)?;
    Some((candidate, request, providers))
}

fn validate_convergence_registry(
    lock: &LockFile,
    index: &impl PackageRegistry,
) -> Result<(), PqtyError> {
    let current = index.registry();
    let locked = lock
        .registries
        .iter()
        .find(|registry| registry.id == current.id)
        .ok_or_else(|| {
            PqtyError::Usage(format!(
                "registry {} is not present in the lock; convergence cannot change registries",
                current.id
            ))
        })?;
    let locked_digest = locked.metadata_digest.as_deref().ok_or_else(|| {
        PqtyError::Usage(
            "lock has no registry metadata digest; recreate it with `pqty lock` before converging"
                .to_string(),
        )
    })?;
    let current_digest = current
        .metadata_digest
        .as_deref()
        .ok_or_else(|| PqtyError::Usage("selected registry has no metadata digest".to_string()))?;
    if locked.kind != current.kind
        || locked.snapshot != current.snapshot
        || locked_digest != current_digest
    {
        return Err(PqtyError::Usage(format!(
            "selected registry metadata does not match the locked snapshot\n  locked:  {locked_digest}\n  selected: {current_digest}"
        )));
    }
    Ok(())
}
