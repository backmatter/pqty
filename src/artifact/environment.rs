use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::artifact::model::{
    EnvironmentFile, EnvironmentPackage, EnvironmentRequirements, InputTrace, LockFile,
    ResolvedEnvironment, ResourceKind, TraceMatch, TraceReport, TraceScope,
};
use crate::artifact::validation::{
    validate_materialized_lock, validate_trace, validate_trace_report,
};
use crate::{ENVIRONMENT_SCHEMA, PqtyError, TRACE_REPORT_SCHEMA};

pub(crate) fn font_filename_is_case_folded(kind: Option<&ResourceKind>) -> bool {
    matches!(
        kind,
        Some(ResourceKind::OpenTypeFont | ResourceKind::TrueTypeFont)
    )
}

fn is_outline_font_file(kind: &ResourceKind) -> bool {
    matches!(
        kind,
        ResourceKind::OpenTypeFont | ResourceKind::TrueTypeFont
    )
}
impl ResolvedEnvironment {
    /// Build the renderer-facing environment described by a fully materialized
    /// lock.
    ///
    /// # Errors
    ///
    /// Returns an error when the lock schema, closure, integrity metadata, or
    /// file ownership is incomplete or inconsistent.
    pub fn from_lock(lock: &LockFile) -> Result<Self, PqtyError> {
        validate_materialized_lock(lock)?;

        let mut engines = lock
            .environment
            .engines
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut font_maps = BTreeSet::new();
        let mut packages = Vec::with_capacity(lock.closure.len());
        let mut files = Vec::new();
        let mut owners: BTreeMap<String, String> = BTreeMap::new();

        for entry in &lock.closure {
            engines.extend(entry.engines.iter().cloned());
            font_maps.extend(entry.font_maps.iter().cloned());
            let integrity = entry.integrity.clone().ok_or_else(|| {
                PqtyError::Usage(format!(
                    "provider {} has no integrity; create the lock with `pqty lock`",
                    entry.provider
                ))
            })?;
            packages.push(EnvironmentPackage {
                provider: entry.provider.clone(),
                version: entry.version.clone(),
                source: entry.source.clone(),
                integrity,
                direct: entry.direct,
                dependencies: entry.dependencies.clone(),
            });

            for file in &entry.files {
                if let Some(previous) = owners.insert(file.tds_path.clone(), entry.provider.clone())
                {
                    return Err(PqtyError::Usage(format!(
                        "package file collision at {}: {} and {}",
                        file.tds_path, previous, entry.provider
                    )));
                }
                let request_name = Path::new(&file.tds_path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        PqtyError::Usage(format!("invalid locked package path: {}", file.tds_path))
                    })?
                    .to_string();
                files.push(EnvironmentFile {
                    request_name,
                    tds_path: file.tds_path.clone(),
                    owner: entry.provider.clone(),
                    kind: file.kind.clone(),
                });
            }
        }

        packages.sort_by(|left, right| left.provider.cmp(&right.provider));
        files.sort_by(|left, right| {
            (&left.tds_path, &left.owner).cmp(&(&right.tds_path, &right.owner))
        });
        for font_map in &font_maps {
            let matches = files
                .iter()
                .filter(|file| file.request_name == *font_map)
                .count();
            if matches != 1 {
                return Err(PqtyError::Usage(format!(
                    "font-map declaration {font_map} resolves to {matches} locked files; expected exactly one"
                )));
            }
        }

        let mut environment = Self {
            schema: ENVIRONMENT_SCHEMA.to_string(),
            fingerprint: String::new(),
            lock_schema: lock.schema.clone(),
            registries: lock.registries.clone(),
            requirements: EnvironmentRequirements {
                engines: engines.into_iter().collect(),
                external_tools: Vec::new(),
            },
            font_maps: font_maps.into_iter().collect(),
            packages,
            files,
        };
        let bytes = serde_json::to_vec(&environment)?;
        environment.fingerprint = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
        Ok(environment)
    }

    /// Compare an engine-neutral trace with this exact environment.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported trace schema or an environment
    /// fingerprint that does not match this environment.
    pub fn reconcile_trace(&self, trace: &InputTrace) -> Result<TraceReport, PqtyError> {
        validate_trace(trace)?;
        if let Some(fingerprint) = trace.environment_fingerprint.as_deref()
            && fingerprint != self.fingerprint
        {
            return Err(PqtyError::Usage(format!(
                "trace was produced by environment {fingerprint}, not {}",
                self.fingerprint
            )));
        }
        let report = self.reconcile_trace_inputs(trace);
        validate_trace_report(&report)?;
        Ok(report)
    }

    pub(crate) fn reconcile_trace_inputs(&self, trace: &InputTrace) -> TraceReport {
        let mut by_path = BTreeMap::new();
        let mut by_request: BTreeMap<&str, Vec<&EnvironmentFile>> = BTreeMap::new();
        let mut by_folded_font_path = BTreeMap::new();
        let mut by_folded_font_request: BTreeMap<String, Vec<&EnvironmentFile>> = BTreeMap::new();
        let mut by_folded_font_stem: BTreeMap<String, Vec<&EnvironmentFile>> = BTreeMap::new();
        for file in &self.files {
            by_path.insert(file.tds_path.as_str(), file);
            by_request
                .entry(file.request_name.as_str())
                .or_default()
                .push(file);
            if is_outline_font_file(&file.kind) {
                by_folded_font_path.insert(file.tds_path.to_ascii_lowercase(), file);
                by_folded_font_request
                    .entry(file.request_name.to_ascii_lowercase())
                    .or_default()
                    .push(file);
                if let Some(stem) = Path::new(&file.request_name)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                {
                    by_folded_font_stem
                        .entry(stem.to_ascii_lowercase())
                        .or_default()
                        .push(file);
                }
            }
        }

        let mut matched = Vec::new();
        let mut missing = Vec::new();
        let mut ignored = Vec::new();
        for input in &trace.inputs {
            if input.scope != TraceScope::Package {
                ignored.push(input.clone());
                continue;
            }
            let candidate = match input.resolved.as_deref() {
                Some(path) => {
                    let path = normalize_trace_path(path);
                    by_path.get(path.as_str()).copied().or_else(|| {
                        font_filename_is_case_folded(input.kind.as_ref())
                            .then(|| by_folded_font_path.get(&path.to_ascii_lowercase()).copied())
                            .flatten()
                    })
                }
                None => by_request
                    .get(input.requested.as_str())
                    .and_then(|candidates| (candidates.len() == 1).then_some(candidates[0]))
                    .or_else(|| {
                        font_filename_is_case_folded(input.kind.as_ref())
                            .then(|| {
                                by_folded_font_request
                                    .get(&input.requested.to_ascii_lowercase())
                                    .and_then(|candidates| {
                                        (candidates.len() == 1).then_some(candidates[0])
                                    })
                            })
                            .flatten()
                    })
                    .or_else(|| {
                        matches!(input.kind.as_ref(), Some(ResourceKind::FontFamily))
                            .then(|| {
                                by_folded_font_stem
                                    .get(&input.requested.to_ascii_lowercase())
                                    .and_then(|candidates| {
                                        (candidates.len() == 1).then_some(candidates[0])
                                    })
                            })
                            .flatten()
                    }),
            };
            match candidate {
                Some(file) => matched.push(TraceMatch {
                    requested: input.requested.clone(),
                    tds_path: file.tds_path.clone(),
                    owner: file.owner.clone(),
                }),
                None => missing.push(input.clone()),
            }
        }
        TraceReport {
            schema: TRACE_REPORT_SCHEMA.to_string(),
            environment_fingerprint: self.fingerprint.clone(),
            matched,
            missing,
            ignored,
        }
    }
}

pub(crate) fn normalize_trace_path(path: &str) -> String {
    let path = path
        .trim_start_matches("./")
        .trim_start_matches('/')
        .replace('\\', "/");
    path.strip_prefix("texmf-dist/")
        .unwrap_or(&path)
        .to_string()
}

pub(crate) fn resource_kind(path: &str) -> ResourceKind {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "tex" | "sty" | "cls" | "ltx" | "def" | "cfg" | "clo" | "fd" => ResourceKind::Tex,
        "bib" | "bst" | "bbx" | "cbx" => ResourceKind::Bibliography,
        "tfm" => ResourceKind::FontMetric,
        "vf" => ResourceKind::VirtualFont,
        "pfb" | "pfa" => ResourceKind::Type1Font,
        "otf" => ResourceKind::OpenTypeFont,
        "ttf" | "ttc" => ResourceKind::TrueTypeFont,
        "map" => ResourceKind::Map,
        "enc" => ResourceKind::Encoding,
        "pat" | "hyp" => ResourceKind::Hyphenation,
        "fmt" => ResourceKind::Format,
        "exe" | "dll" | "so" | "dylib" => ResourceKind::Binary,
        _ => ResourceKind::Data,
    }
}
