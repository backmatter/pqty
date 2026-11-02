//! Snapshot-bound font-map metadata. Backend/profile selection stays with hosts.
use crate::{
    LockFile, MAX_CLOSURE_RUNFILES, PackageRegistry, PqtyError, Registry, ResolvedEnvironment,
    TlpdbIndex, normalize_runfile, validate_materialized_lock, validate_tds_path,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Schema for typed font-map declarations belonging to an exact environment.
pub const FONT_MAPS_SCHEMA: &str = "pqty.font-maps/v1";

/// TeX Live's distinct map registration classes, uninterpreted by pqty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FontMapKind {
    /// Outline-font map registration.
    Map,
    /// Mixed bitmap/outline registration.
    MixedMap,
    /// CJK backend map registration.
    KanjiMap,
}

/// A declared map filename or template, with its type preserved.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontMapDeclaration {
    /// Registry registration class.
    pub kind: FontMapKind,
    /// Original registry filename or template, such as `ptex-@jaEmbed@.map`.
    pub name: String,
    /// Sorted unique template parameter names, without the surrounding `@`.
    pub parameters: Vec<String>,
}

/// One declaration and its concrete locked file, when no profile is needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedFontMap {
    /// Provider declaring this map, which need not own its concrete file.
    pub provider: String,
    /// Uninterpreted type and template metadata.
    pub declaration: FontMapDeclaration,
    /// Exact locked TDS path for concrete names; null for profile templates.
    pub tds_path: Option<String>,
}

/// Complete typed map metadata for one exact package environment and registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontMapManifest {
    /// Artifact schema, currently [`FONT_MAPS_SCHEMA`].
    pub schema: String,
    /// Registry identity including its metadata digest.
    pub registry: Registry,
    /// Existing v1 environment fingerprint, unchanged by this projection.
    pub environment_fingerprint: String,
    /// Declarations ordered by provider, type, and name; duplicates removed.
    pub maps: Vec<ResolvedFontMap>,
}

impl FontMapManifest {
    /// Validate complete metadata, ordering, and file ownership against a lock
    /// and its original registry, without reading package bytes.
    ///
    /// # Errors
    /// Returns an error for changed snapshots, missing files, or altered metadata.
    pub fn validate(&self, lock: &LockFile, index: &TlpdbIndex) -> Result<(), PqtyError> {
        if self != &index.font_map_manifest(lock)? {
            return Err(invalid(
                "font-map manifest differs from its locked registry",
            ));
        }
        Ok(())
    }
}

impl TlpdbIndex {
    /// Project all map declarations belonging to the lock's provider closure.
    /// Templates remain unresolved metadata. Concrete names must have one
    /// registry path and that file must belong to the exact lock.
    ///
    /// # Errors
    /// Returns an error for an invalid lock, changed metadata, or ambiguous,
    /// missing, or unlocked concrete map files.
    pub fn font_map_manifest(&self, lock: &LockFile) -> Result<FontMapManifest, PqtyError> {
        validate_materialized_lock(lock)?;
        let registry = self.registry();
        if !lock.registries.contains(&registry) {
            return Err(invalid(
                "font maps require the lock's exact registry snapshot",
            ));
        }
        let environment = ResolvedEnvironment::from_lock(lock)?;
        let locked_files = environment
            .files
            .iter()
            .map(|file| (file.tds_path.as_str(), file.owner.as_str()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut maps = Vec::new();
        for provider in &lock.closure {
            let package = self
                .package(&provider.provider)
                .ok_or_else(|| invalid("font-map provider is absent from registry"))?;
            let declarations = package
                .font_map_declarations
                .iter()
                .collect::<BTreeSet<_>>();
            for declaration in declarations {
                let tds_path = if declaration.parameters.is_empty() {
                    let candidates = self
                        .providers_of_file(&declaration.name)
                        .into_iter()
                        .flat_map(|owner| {
                            self.package(owner).into_iter().flat_map(move |package| {
                                package.runfiles.iter().filter_map(move |file| {
                                    let path = normalize_runfile(file).1;
                                    (path.rsplit('/').next() == Some(declaration.name.as_str()))
                                        .then_some((owner, path))
                                })
                            })
                        })
                        .collect::<BTreeSet<_>>();
                    let mut candidates = candidates.into_iter();
                    let (owner, path) = candidates.next().ok_or_else(|| {
                        invalid(format!("font map is missing: {}", declaration.name))
                    })?;
                    if candidates.next().is_some() {
                        return Err(invalid(format!(
                            "font map has ambiguous ownership: {}",
                            declaration.name
                        )));
                    }
                    if locked_files.get(path.as_str()).copied() != Some(owner) {
                        return Err(invalid(format!(
                            "font map is outside the exact lock: {path}"
                        )));
                    }
                    Some(path)
                } else {
                    None
                };
                if maps.len() >= MAX_CLOSURE_RUNFILES {
                    return Err(invalid(
                        "font-map manifest exceeds the declaration count limit",
                    ));
                }
                maps.push(ResolvedFontMap {
                    provider: provider.provider.clone(),
                    declaration: declaration.clone(),
                    tds_path,
                });
            }
        }
        maps.sort_by(|a, b| (&a.provider, &a.declaration).cmp(&(&b.provider, &b.declaration)));
        Ok(FontMapManifest {
            schema: FONT_MAPS_SCHEMA.into(),
            registry,
            environment_fingerprint: environment.fingerprint,
            maps,
        })
    }
}

pub(super) fn parse_font_map(action: &str) -> Result<Option<FontMapDeclaration>, PqtyError> {
    let mut fields = action.split_whitespace();
    let kind = match fields.next() {
        Some("addMap") => FontMapKind::Map,
        Some("addMixedMap") => FontMapKind::MixedMap,
        Some("addKanjiMap") => FontMapKind::KanjiMap,
        _ => return Ok(None),
    };
    let name = fields
        .next()
        .ok_or_else(|| invalid("font-map action has no filename"))?;
    validate_tds_path(name, "font map")?;
    if fields.next().is_some()
        || name.contains('/')
        || !std::path::Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("map"))
    {
        return Err(invalid(
            "font-map action must name a .map basename or template",
        ));
    }
    let mut rest = name;
    let mut parameters = BTreeSet::new();
    while let Some((_, tail)) = rest.split_once('@') {
        let (parameter, tail) = tail
            .split_once('@')
            .ok_or_else(|| invalid("unclosed font-map template parameter"))?;
        if parameter.is_empty()
            || !parameter
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            return Err(invalid("invalid font-map template parameter"));
        }
        parameters.insert(parameter.to_owned());
        rest = tail;
    }
    Ok(Some(FontMapDeclaration {
        kind,
        name: name.into(),
        parameters: parameters.into_iter().collect(),
    }))
}

fn invalid(message: impl Into<String>) -> PqtyError {
    PqtyError::Usage(message.into())
}
