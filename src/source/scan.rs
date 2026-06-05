use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::source::parser::{ParsedCommand, scan_commands, split_names};
use crate::source::resolve::{
    canonical_or_original, digest_bytes, enqueue_source_file, normalize_project_path,
    read_source_file, resolve_tex_input, resolve_with_extensions,
};
use crate::source::tree::FileSystemSourceTree;
use crate::{
    BibliographyKind, BibliographyRecord, ClassRecord, ConsumerRequirements, Environment,
    GraphicRecord, InputKind, InputRecord, LOCK_SCHEMA, Location, LockFile, LockStage,
    PackageRecord, PqtyError, SourceRecord, SourceTree, UnresolvedRecord, VirtualPath,
};

#[derive(Debug, Clone)]
struct ScanContext {
    root: VirtualPath,
    visited: BTreeSet<VirtualPath>,
    sources: BTreeMap<String, SourceRecord>,
    document_class: Option<ClassRecord>,
    loaded_classes: BTreeMap<String, ClassRecord>,
    packages: BTreeMap<String, PackageRecord>,
    inputs: Vec<InputRecord>,
    bibliographies: Vec<BibliographyRecord>,
    graphics: Vec<GraphicRecord>,
    unresolved: Vec<UnresolvedRecord>,
}

impl ScanContext {
    fn new(root: VirtualPath) -> Self {
        Self {
            root,
            visited: BTreeSet::new(),
            sources: BTreeMap::new(),
            document_class: None,
            loaded_classes: BTreeMap::new(),
            packages: BTreeMap::new(),
            inputs: Vec::new(),
            bibliographies: Vec::new(),
            graphics: Vec::new(),
            unresolved: Vec::new(),
        }
    }

    fn into_lock(self) -> LockFile {
        LockFile {
            schema: LOCK_SCHEMA.to_string(),
            stage: LockStage::Scanned,
            generated_with: concat!("pqty ", env!("CARGO_PKG_VERSION")).to_string(),
            environment: Environment::default(),
            registries: Vec::new(),
            consumer_requirements: ConsumerRequirements::default(),
            root: self.root.as_str().to_string(),
            sources: self.sources.into_values().collect(),
            document_class: self.document_class,
            loaded_classes: self.loaded_classes.into_values().collect(),
            packages: self.packages.into_values().collect(),
            inputs: self.inputs,
            bibliographies: self.bibliographies,
            graphics: self.graphics,
            closure: Vec::new(),
            unresolved: self.unresolved,
        }
    }

    fn source_location(path: &VirtualPath, line: usize) -> Location {
        Location {
            path: path.as_str().to_string(),
            line,
        }
    }

    fn push_unresolved(&mut self, kind: &str, name: &str, source: Location) {
        self.unresolved.push(UnresolvedRecord {
            kind: kind.to_string(),
            name: name.to_string(),
            source,
            candidates: Vec::new(),
        });
    }
}

/// Scan a standalone root file using its parent as the project root.
///
/// # Errors
///
/// Returns an error when the root cannot be read or the source graph contains
/// an invalid project path.
pub fn scan_project(root: impl AsRef<Path>) -> Result<LockFile, PqtyError> {
    let root = root.as_ref();
    if !root.is_file() {
        return Err(PqtyError::Usage(format!(
            "root TeX file is missing: {}",
            root.display()
        )));
    }
    let root = canonical_or_original(root);
    let project_root = root
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let source = FileSystemSourceTree::new(project_root)?;
    let entrypoint = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| PqtyError::Usage("root TeX file has no file name".to_string()))?;
    scan_source(&source, VirtualPath::new(entrypoint)?)
}

pub(crate) fn scan_cli_project(
    root: PathBuf,
    project_root: Option<&Path>,
    search_roots: &[PathBuf],
) -> Result<LockFile, PqtyError> {
    match project_root {
        Some(project_root) => scan_project_at_with_roots(project_root, &root, search_roots),
        None if search_roots.is_empty() => scan_project(root),
        None => {
            let absolute = canonical_or_original(&root);
            let project_root = absolute
                .parent()
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
            let entry = absolute
                .file_name()
                .map(PathBuf::from)
                .ok_or_else(|| PqtyError::Usage("root TeX file has no file name".to_string()))?;
            scan_project_at_with_roots(project_root, entry, search_roots)
        }
    }
}

/// Scan a filesystem project while preserving paths relative to an explicit
/// project root. Build systems should use this entry point so a nested root
/// such as `paper/main.tex` remains nested in the lock.
///
/// # Errors
///
/// Returns an error when the project root or entry cannot be read, when the
/// entry is outside the project, or when a source path is invalid.
pub fn scan_project_at(
    project_root: impl AsRef<Path>,
    root: impl AsRef<Path>,
) -> Result<LockFile, PqtyError> {
    scan_project_at_with_roots(project_root, root, &[])
}

/// Scan a filesystem project with additional confined input roots.
///
/// # Errors
///
/// Returns an error for the same conditions as [`scan_project_at`], or when an
/// input root is absent or escapes the project.
pub fn scan_project_at_with_roots(
    project_root: impl AsRef<Path>,
    root: impl AsRef<Path>,
    search_roots: &[PathBuf],
) -> Result<LockFile, PqtyError> {
    let source = FileSystemSourceTree::with_search_roots(project_root.as_ref(), search_roots)?;
    let requested = root.as_ref();
    let absolute = if requested.is_absolute() {
        canonical_or_original(requested)
    } else {
        canonical_or_original(&source.root().join(requested))
    };
    if !absolute.is_file() {
        return Err(PqtyError::Usage(format!(
            "root TeX file is missing: {}",
            absolute.display()
        )));
    }
    let relative = absolute.strip_prefix(source.root()).map_err(|_| {
        PqtyError::Usage(format!(
            "root TeX file is outside project root {}\n  root: {}",
            source.root().display(),
            absolute.display()
        ))
    })?;
    let entrypoint = normalize_project_path(relative).ok_or_else(|| {
        PqtyError::Usage(format!(
            "root TeX file is not a confined project path: {}",
            relative.display()
        ))
    })?;
    scan_source(&source, entrypoint)
}

/// Scan a source tree from a normalized virtual root.
///
/// # Errors
///
/// Returns an error when a source cannot be read or a discovered project path
/// is invalid.
pub fn scan_source(source: &impl SourceTree, root: VirtualPath) -> Result<LockFile, PqtyError> {
    let mut context = ScanContext::new(root);
    let mut queue = VecDeque::from([(context.root.clone(), None)]);
    while let Some((path, prefetched)) = queue.pop_front() {
        scan_file(source, &mut context, &path, prefetched, &mut queue)?;
    }
    Ok(context.into_lock())
}

fn scan_file(
    source: &impl SourceTree,
    context: &mut ScanContext,
    path: &VirtualPath,
    prefetched: Option<Vec<u8>>,
    queue: &mut VecDeque<(VirtualPath, Option<Vec<u8>>)>,
) -> Result<(), PqtyError> {
    if !context.visited.insert(path.clone()) {
        return Ok(());
    }
    let bytes = match prefetched {
        Some(bytes) => bytes,
        None => read_source_file(source, path)?.ok_or_else(|| {
            PqtyError::Usage(format!("source file is missing: {}", path.as_str()))
        })?,
    };
    let text = String::from_utf8(bytes).map_err(|error| {
        PqtyError::Usage(format!(
            "source file is not UTF-8: {}: {error}",
            path.as_str()
        ))
    })?;
    record_source_bytes(context, path, text.as_bytes());

    for (line_number, command) in scan_commands(&text) {
        handle_command(source, context, path, line_number, &command, queue)?;
    }

    Ok(())
}

fn record_source_bytes(context: &mut ScanContext, path: &VirtualPath, bytes: &[u8]) {
    let relative = path.as_str().to_string();
    context.sources.insert(
        relative.clone(),
        SourceRecord {
            path: relative,
            digest: digest_bytes(bytes),
        },
    );
}

fn handle_command(
    tree: &impl SourceTree,
    context: &mut ScanContext,
    current_path: &VirtualPath,
    line: usize,
    command: &ParsedCommand,
    queue: &mut VecDeque<(VirtualPath, Option<Vec<u8>>)>,
) -> Result<(), PqtyError> {
    let source = ScanContext::source_location(current_path, line);
    match command.name.as_str() {
        "documentclass" => {
            handle_document_class(tree, context, current_path, &source, command, queue)?;
        }
        "LoadClass" => {
            handle_loaded_classes(tree, context, current_path, &source, command, queue)?;
        }
        "usepackage" | "RequirePackage" => {
            handle_packages(tree, context, current_path, &source, command, queue)?;
        }
        "input" | "include" => {
            handle_inputs(tree, context, current_path, &source, command, queue)?;
        }
        "InputIfFileExists" => {
            handle_input_if_exists(tree, current_path, command, queue)?;
        }
        "includeonly" => handle_include_only(context, &source, command),
        "addbibresource" => handle_bibliography(
            tree,
            context,
            current_path,
            &source,
            command,
            &BibliographyKind::AddBibResource,
            true,
        )?,
        "bibliography" => handle_bibliography(
            tree,
            context,
            current_path,
            &source,
            command,
            &BibliographyKind::Bibliography,
            true,
        )?,
        "bibliographystyle" => handle_bibliography(
            tree,
            context,
            current_path,
            &source,
            command,
            &BibliographyKind::BibliographyStyle,
            false,
        )?,
        "includegraphics" => {
            handle_graphics(tree, context, current_path, &source, command)?;
        }
        _ => {}
    }
    Ok(())
}

fn handle_document_class(
    tree: &impl SourceTree,
    context: &mut ScanContext,
    current_path: &VirtualPath,
    source: &Location,
    command: &ParsedCommand,
    queue: &mut VecDeque<(VirtualPath, Option<Vec<u8>>)>,
) -> Result<(), PqtyError> {
    if let Some(name) = command
        .required
        .first()
        .and_then(|value| split_names(value).first().cloned())
    {
        let resolved = resolve_with_extensions(tree, current_path, &name, &["cls"])?;
        context.document_class = Some(ClassRecord {
            name,
            options: command.options.clone(),
            resolved_path: enqueue_source_file(resolved, queue),
            source: source.clone(),
        });
    }
    Ok(())
}

fn handle_loaded_classes(
    tree: &impl SourceTree,
    context: &mut ScanContext,
    current_path: &VirtualPath,
    source: &Location,
    command: &ParsedCommand,
    queue: &mut VecDeque<(VirtualPath, Option<Vec<u8>>)>,
) -> Result<(), PqtyError> {
    for name in command.required.iter().flat_map(|value| split_names(value)) {
        let resolved = resolve_with_extensions(tree, current_path, &name, &["cls"])?;
        let resolved_path = enqueue_source_file(resolved, queue);
        context
            .loaded_classes
            .entry(name.clone())
            .or_insert(ClassRecord {
                name,
                options: command.options.clone(),
                resolved_path,
                source: source.clone(),
            });
    }
    Ok(())
}

fn handle_packages(
    tree: &impl SourceTree,
    context: &mut ScanContext,
    current_path: &VirtualPath,
    source: &Location,
    command: &ParsedCommand,
    queue: &mut VecDeque<(VirtualPath, Option<Vec<u8>>)>,
) -> Result<(), PqtyError> {
    for name in command.required.iter().flat_map(|value| split_names(value)) {
        let resolved = resolve_with_extensions(tree, current_path, &name, &["sty"])?;
        let resolved_path = enqueue_source_file(resolved, queue);
        context
            .packages
            .entry(name.clone())
            .or_insert(PackageRecord {
                name,
                options: command.options.clone(),
                command: command.name.clone(),
                resolved_path,
                source: source.clone(),
            });
    }
    Ok(())
}

fn handle_inputs(
    tree: &impl SourceTree,
    context: &mut ScanContext,
    current_path: &VirtualPath,
    source: &Location,
    command: &ParsedCommand,
    queue: &mut VecDeque<(VirtualPath, Option<Vec<u8>>)>,
) -> Result<(), PqtyError> {
    let kind = if command.name == "include" {
        InputKind::Include
    } else {
        InputKind::Input
    };
    for name in command.required.iter().flat_map(|value| split_names(value)) {
        let resolved = resolve_tex_input(tree, current_path, &name)?;
        let resolved_path = if let Some(file) = resolved {
            let path = file.path.as_str().to_string();
            queue.push_back((file.path, Some(file.bytes)));
            Some(path)
        } else {
            context.push_unresolved("input", &name, source.clone());
            None
        };
        context.inputs.push(InputRecord {
            kind: kind.clone(),
            name,
            resolved_path,
            source: source.clone(),
        });
    }
    Ok(())
}

fn handle_input_if_exists(
    tree: &impl SourceTree,
    current_path: &VirtualPath,
    command: &ParsedCommand,
    queue: &mut VecDeque<(VirtualPath, Option<Vec<u8>>)>,
) -> Result<(), PqtyError> {
    if let Some(name) = command.required.first().map(|name| name.trim())
        && let Some(file) = resolve_with_extensions(tree, current_path, name, &[])?
    {
        queue.push_back((file.path, Some(file.bytes)));
    }
    Ok(())
}

fn handle_include_only(context: &mut ScanContext, source: &Location, command: &ParsedCommand) {
    for name in command.required.iter().flat_map(|value| split_names(value)) {
        context.inputs.push(InputRecord {
            kind: InputKind::IncludeOnly,
            name,
            resolved_path: None,
            source: source.clone(),
        });
    }
}

fn handle_bibliography(
    tree: &impl SourceTree,
    context: &mut ScanContext,
    current_path: &VirtualPath,
    source: &Location,
    command: &ParsedCommand,
    kind: &BibliographyKind,
    unresolved_is_error: bool,
) -> Result<(), PqtyError> {
    let extensions = match kind {
        BibliographyKind::BibliographyStyle => &["bst"][..],
        BibliographyKind::AddBibResource | BibliographyKind::Bibliography => &["bib"][..],
    };
    for name in command.required.iter().flat_map(|value| split_names(value)) {
        let resolved = resolve_with_extensions(tree, current_path, &name, extensions)?;
        if unresolved_is_error && resolved.is_none() {
            context.push_unresolved("bibliography", &name, source.clone());
        }
        if let Some(file) = &resolved {
            record_source_bytes(context, &file.path, &file.bytes);
        }
        context.bibliographies.push(BibliographyRecord {
            kind: kind.clone(),
            name,
            resolved_path: resolved.as_ref().map(|file| file.path.as_str().to_string()),
            source: source.clone(),
        });
    }
    Ok(())
}

fn handle_graphics(
    tree: &impl SourceTree,
    context: &mut ScanContext,
    current_path: &VirtualPath,
    source: &Location,
    command: &ParsedCommand,
) -> Result<(), PqtyError> {
    for name in command.required.iter().flat_map(|value| split_names(value)) {
        let resolved = resolve_with_extensions(
            tree,
            current_path,
            &name,
            &["pdf", "png", "jpg", "jpeg", "mps"],
        )?;
        if resolved.is_none() {
            context.push_unresolved("graphic", &name, source.clone());
        }
        if let Some(file) = &resolved {
            record_source_bytes(context, &file.path, &file.bytes);
        }
        context.graphics.push(GraphicRecord {
            name,
            resolved_path: resolved.as_ref().map(|file| file.path.as_str().to_string()),
            source: source.clone(),
        });
    }
    Ok(())
}
