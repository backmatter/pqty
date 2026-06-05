use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::cli::args::{Cli, Command};
use crate::cli::config::Config;
use crate::cli::protocol::Capabilities;
use crate::{
    BibliographyKind, ConsumerRequirements, ConvergenceStatus, LockFile, PqtyError,
    RegistryRequest, ResolvedEnvironment, byte_source, converge_trace, fail_on_unresolved_packages,
    hydrate_lock, install_locked_with_policy, load_convergence_index, load_tlpdb_index,
    normalized_consumer_requirements, print_explanation, print_tree, print_why, progress,
    read_lock, read_trace, require_runtime, resolve, scan_cli_project, stable_convergence_report,
    tlnet_base_from_tlpdb_url, validate_lock, validate_materialized_lock, validate_tlpdb_digest,
    write_lock,
};

pub(super) fn run(cli: Cli) -> Result<(), PqtyError> {
    progress::configure(cli.progress);
    let context = ExecutionContext {
        config: if cli.no_config {
            Config::default()
        } else {
            Config::load()?
        },
        project_root: cli.project_root,
        input_roots: cli.input_roots,
        offline: cli.offline,
        allow_insecure: cli.allow_insecure_registry,
    };
    match cli.command {
        Command::Capabilities {} => run_capabilities(),
        command @ Command::Scan { .. } => run_scan(&context, command),
        command @ Command::Explain { .. } => run_explain(&context, command),
        command @ Command::Tree { .. } => run_tree(&context, command),
        command @ Command::Why { .. } => run_why(&context, command),
        command @ Command::Resolve { .. } => run_resolve(&context, command),
        command @ Command::Install { .. } => run_install(&context, command),
        command @ Command::Lock { .. } => run_lock(&context, command),
        command @ Command::Env { .. } => run_env(command),
        command @ Command::CheckTrace { .. } => run_check_trace(command),
        command @ Command::Converge { .. } => run_converge(&context, command),
        command @ Command::Require { .. } => run_require(&context, command),
    }
}

struct ExecutionContext {
    config: Config,
    project_root: Option<PathBuf>,
    input_roots: Vec<PathBuf>,
    offline: bool,
    allow_insecure: bool,
}

impl ExecutionContext {
    fn scan(&self, root: PathBuf) -> Result<LockFile, PqtyError> {
        scan_cli_project(root, self.project_root.as_deref(), &self.input_roots)
    }
}

fn run_capabilities() -> Result<(), PqtyError> {
    let capabilities = serde_json::to_value(Capabilities::current())?;
    println!("{}", serde_json::to_string_pretty(&capabilities)?);
    Ok(())
}

fn run_scan(context: &ExecutionContext, command: Command) -> Result<(), PqtyError> {
    let Command::Scan { root } = command else {
        unreachable!("scan handler received another command");
    };
    let lock = context.scan(root)?;
    validate_lock(&lock)?;
    println!("{}", serde_json::to_string_pretty(&lock)?);
    Ok(())
}

fn run_explain(context: &ExecutionContext, command: Command) -> Result<(), PqtyError> {
    let Command::Explain { root } = command else {
        unreachable!("explain handler received another command");
    };
    print_explanation(&context.scan(root)?);
    Ok(())
}

fn run_tree(context: &ExecutionContext, command: Command) -> Result<(), PqtyError> {
    let Command::Tree {
        root,
        tlpdb,
        tlpdb_url,
        remote,
    } = command
    else {
        unreachable!("tree handler received another command");
    };
    let mut lock = context.scan(root)?;
    let registry = context.config.registry_request(
        tlpdb_url,
        remote,
        context.offline,
        context.allow_insecure,
    )?;
    resolve(&mut lock, &load_tlpdb_index(tlpdb, registry.as_ref())?);
    print_tree(&lock);
    Ok(())
}

fn run_why(context: &ExecutionContext, command: Command) -> Result<(), PqtyError> {
    let Command::Why {
        root,
        provider,
        tlpdb,
        tlpdb_url,
        remote,
    } = command
    else {
        unreachable!("why handler received another command");
    };
    let mut lock = context.scan(root)?;
    let registry = context.config.registry_request(
        tlpdb_url,
        remote,
        context.offline,
        context.allow_insecure,
    )?;
    resolve(&mut lock, &load_tlpdb_index(tlpdb, registry.as_ref())?);
    print_why(&lock, &provider);
    Ok(())
}

fn run_resolve(context: &ExecutionContext, command: Command) -> Result<(), PqtyError> {
    let Command::Resolve {
        root,
        tlpdb,
        tlpdb_url,
        remote,
        output,
    } = command
    else {
        unreachable!("resolve handler received another command");
    };
    let mut lock = context.scan(root)?;
    let registry = context.config.registry_request(
        tlpdb_url,
        remote,
        context.offline,
        context.allow_insecure,
    )?;
    resolve(&mut lock, &load_tlpdb_index(tlpdb, registry.as_ref())?);
    validate_lock(&lock)?;
    match output {
        Some(path) => {
            write_lock(&path, &lock)?;
            println!("wrote {}", path.display());
        }
        None => println!("{}", serde_json::to_string_pretty(&lock)?),
    }
    Ok(())
}

fn run_install(context: &ExecutionContext, command: Command) -> Result<(), PqtyError> {
    let Command::Install {
        lock,
        store,
        out,
        link,
    } = command
    else {
        unreachable!("install handler received another command");
    };
    let report = install_locked_with_policy(
        &read_lock(&lock)?,
        &context.config.store_dir(store),
        &out,
        link,
        context.offline,
        context.allow_insecure,
    )?;
    report.print_action("installed and verified");
    Ok(())
}

fn run_lock(context: &ExecutionContext, command: Command) -> Result<(), PqtyError> {
    let Command::Lock {
        root,
        tlpdb,
        tlpdb_url,
        remote,
        required_files,
        required_providers,
        tlpdb_sha256,
        store,
        output,
    } = command
    else {
        unreachable!("lock handler received another command");
    };
    let scanned = context.scan(root)?;
    let registry = context.config.registry_request(
        tlpdb_url,
        remote,
        context.offline,
        context.allow_insecure,
    )?;
    let consumer_requirements =
        normalized_consumer_requirements(&required_files, &required_providers)?;
    if let Some(lock) = refresh_existing_lock(
        &scanned,
        &output,
        tlpdb.as_deref(),
        registry.as_ref(),
        tlpdb_sha256.as_deref(),
        &consumer_requirements,
    ) {
        return publish_refreshed_lock(&lock, &output);
    }
    let mut lock = scanned;
    let index = load_tlpdb_index(tlpdb, registry.as_ref())?;
    if let Some(expected) = tlpdb_sha256.as_deref() {
        validate_tlpdb_digest(&index, expected)?;
    }
    resolve(&mut lock, &index);
    require_runtime(&mut lock, &index, &required_files, &required_providers)?;
    fail_on_unresolved_packages(&lock)?;
    let source = byte_source(registry.as_ref(), &index)?;
    hydrate_lock(&mut lock, &index, &source, &context.config.store_dir(store))?;
    ResolvedEnvironment::from_lock(&lock)?;
    write_lock(&output, &lock)?;
    println!("wrote {}", output.display());
    Ok(())
}

fn publish_refreshed_lock(lock: &LockFile, output: &Path) -> Result<(), PqtyError> {
    if serde_json::to_vec(&lock)? == serde_json::to_vec(&read_lock(output)?)? {
        println!("current {}", output.display());
    } else {
        write_lock(output, lock)?;
        println!("refreshed {}", output.display());
    }
    Ok(())
}

fn run_env(command: Command) -> Result<(), PqtyError> {
    let Command::Env { lock } = command else {
        unreachable!("env handler received another command");
    };
    let environment = ResolvedEnvironment::from_lock(&read_lock(&lock)?)?;
    println!("{}", serde_json::to_string_pretty(&environment)?);
    Ok(())
}

fn run_check_trace(command: Command) -> Result<(), PqtyError> {
    let Command::CheckTrace { lock, trace } = command else {
        unreachable!("check-trace handler received another command");
    };
    let environment = ResolvedEnvironment::from_lock(&read_lock(&lock)?)?;
    let report = environment.reconcile_trace(&read_trace(&trace)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if report.missing.is_empty() {
        Ok(())
    } else {
        Err(PqtyError::Usage(format!(
            "runtime trace contains {} unlocked package input(s)",
            report.missing.len()
        )))
    }
}

fn run_converge(context: &ExecutionContext, command: Command) -> Result<(), PqtyError> {
    let Command::Converge {
        lock,
        trace,
        tlpdb,
        tlpdb_url,
        store,
        output,
    } = command
    else {
        unreachable!("converge handler received another command");
    };
    let mut locked = read_lock(&lock)?;
    let trace = read_trace(&trace)?;
    if let Some(report) = stable_convergence_report(&locked, &trace)? {
        println!("{}", serde_json::to_string_pretty(&report)?);
        if let Some(output) = output {
            write_lock(&output, &locked)?;
        }
        return Ok(());
    }
    let (index, registry) = load_convergence_index(
        &locked,
        tlpdb,
        tlpdb_url,
        context.offline,
        context.allow_insecure,
    )?;
    let source = byte_source(registry.as_ref(), &index)?;
    let report = converge_trace(
        &mut locked,
        &trace,
        &index,
        &source,
        &context.config.store_dir(store),
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    match report.status {
        ConvergenceStatus::Stable => {
            if let Some(output) = output {
                write_lock(&output, &locked)?;
            }
            Ok(())
        }
        ConvergenceStatus::Changed => {
            write_lock(&output.unwrap_or(lock), &locked)?;
            Ok(())
        }
        ConvergenceStatus::Unresolved => Err(PqtyError::Usage(format!(
            "runtime trace contains {} package input(s) that cannot converge",
            report.unresolved.len()
        ))),
    }
}

fn run_require(context: &ExecutionContext, command: Command) -> Result<(), PqtyError> {
    let Command::Require {
        lock,
        files,
        providers,
        tlpdb,
        tlpdb_url,
        store,
        output,
    } = command
    else {
        unreachable!("require handler received another command");
    };
    if files.is_empty() && providers.is_empty() {
        return Err(PqtyError::Usage(
            "`require` needs at least one --file or --provider".to_string(),
        ));
    }
    let mut locked = read_lock(&lock)?;
    let (index, registry) = load_convergence_index(
        &locked,
        tlpdb,
        tlpdb_url,
        context.offline,
        context.allow_insecure,
    )?;
    let source = byte_source(registry.as_ref(), &index)?;
    require_runtime(&mut locked, &index, &files, &providers)?;
    hydrate_lock(
        &mut locked,
        &index,
        &source,
        &context.config.store_dir(store),
    )?;
    ResolvedEnvironment::from_lock(&locked)?;
    let output = output.unwrap_or(lock);
    write_lock(&output, &locked)?;
    println!("wrote {}", output.display());
    Ok(())
}

/// Reuse a hydrated closure when scanning found the same registry
/// requirements. Source digests and project-file records are refreshed, but no
/// registry metadata, package container, or store object is touched.
pub(super) fn refresh_existing_lock(
    scanned: &LockFile,
    output: &Path,
    tlpdb: Option<&Path>,
    registry_request: Option<&RegistryRequest>,
    expected_tlpdb_digest: Option<&str>,
    consumer_requirements: &ConsumerRequirements,
) -> Option<LockFile> {
    if tlpdb.is_some() || !output.is_file() {
        return None;
    }
    let Ok(existing) = read_lock(output) else {
        return None;
    };
    if validate_materialized_lock(&existing).is_err()
        || resolution_requirements(scanned) != resolution_requirements(&existing)
        || !registry_selection_matches(&existing, registry_request, expected_tlpdb_digest)
        || existing.consumer_requirements != *consumer_requirements
    {
        return None;
    }

    let mut refreshed = existing;
    refreshed.generated_with.clone_from(&scanned.generated_with);
    refreshed.root.clone_from(&scanned.root);
    refreshed.sources.clone_from(&scanned.sources);
    refreshed.document_class.clone_from(&scanned.document_class);
    refreshed.loaded_classes.clone_from(&scanned.loaded_classes);
    refreshed.packages.clone_from(&scanned.packages);
    refreshed.inputs.clone_from(&scanned.inputs);
    refreshed.bibliographies.clone_from(&scanned.bibliographies);
    refreshed.graphics.clone_from(&scanned.graphics);
    refreshed.unresolved.clone_from(&scanned.unresolved);
    Some(refreshed)
}

pub(super) fn registry_selection_matches(
    lock: &LockFile,
    registry_request: Option<&RegistryRequest>,
    expected_tlpdb_digest: Option<&str>,
) -> bool {
    let Some(request) = registry_request else {
        // A local TeX Live may have moved since this lock was produced. Reading
        // its metadata is the only sound way to decide, so keep the fast path
        // for explicitly pinned remote snapshots.
        return false;
    };
    let Some(base) = tlnet_base_from_tlpdb_url(&request.url) else {
        return false;
    };
    let Some(registry) = lock
        .registries
        .iter()
        .find(|registry| registry.id == "tlnet")
    else {
        return false;
    };
    if registry.url.trim_end_matches('/') != base.trim_end_matches('/') {
        return false;
    }
    if request
        .snapshot
        .as_ref()
        .is_some_and(|snapshot| registry.snapshot.as_ref() != Some(snapshot))
    {
        return false;
    }
    expected_tlpdb_digest.is_none_or(|expected| {
        let expected = expected.strip_prefix("sha256:").unwrap_or(expected);
        registry
            .metadata_digest
            .as_deref()
            .and_then(|digest| digest.strip_prefix("sha256:"))
            == Some(expected)
    })
}

/// Only these declarations affect package resolution. Locations, source
/// digests, local inputs, graphics, and bibliography database paths remain
/// important lock data, but changing them cannot change the provider closure.
fn resolution_requirements(lock: &LockFile) -> BTreeSet<(String, String, bool)> {
    let mut requirements = BTreeSet::new();
    if let Some(class) = &lock.document_class {
        requirements.insert((
            "class".to_string(),
            class.name.clone(),
            class.resolved_path.is_some(),
        ));
    }
    requirements.extend(lock.loaded_classes.iter().map(|class| {
        (
            "class".to_string(),
            class.name.clone(),
            class.resolved_path.is_some(),
        )
    }));
    requirements.extend(lock.packages.iter().map(|package| {
        (
            "package".to_string(),
            package.name.clone(),
            package.resolved_path.is_some(),
        )
    }));
    requirements.extend(
        lock.bibliographies
            .iter()
            .filter(|record| matches!(record.kind, BibliographyKind::BibliographyStyle))
            .map(|record| {
                (
                    "bibliography-style".to_string(),
                    record.name.clone(),
                    record.resolved_path.is_some(),
                )
            }),
    );
    requirements
}
