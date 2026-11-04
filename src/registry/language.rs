use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    LockFile, PackageRegistry, PqtyError, Registry, TlpdbIndex, normalize_runfile, require_runtime,
    validate_tds_path,
};

/// Schema identifying resolved, snapshot-bound language declarations.
pub const LANGUAGES_SCHEMA: &str = "pqty.languages/v1";

/// An uninterpreted extension to a TeX Live `AddHyphen` declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageAttribute {
    /// Registry field name, such as `luaspecial`.
    pub name: String,
    /// Registry value. pqty never executes or interprets it.
    pub value: String,
}

/// Engine-neutral hyphenation metadata retained from `AddHyphen`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HyphenationDeclaration {
    /// Canonical, case-sensitive language name.
    pub name: String,
    /// Case-sensitive language synonyms, sorted without duplicates.
    pub synonyms: Vec<String>,
    /// Pattern loader filename; a normalized TDS path in a language manifest.
    pub loader: String,
    /// Pattern data filenames, in declaration order; exact TDS paths after resolution.
    pub patterns: Vec<String>,
    /// Exception data filenames, in declaration order; exact TDS paths after resolution.
    pub exceptions: Vec<String>,
    /// Declared minimum characters before a hyphenation point.
    pub left_hyphen_min: u8,
    /// Declared minimum characters after a hyphenation point.
    pub right_hyphen_min: u8,
    /// Additional registry metadata for the consuming engine to interpret.
    pub attributes: Vec<LanguageAttribute>,
}

/// A declaration and the registry provider that published it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedLanguage {
    /// Provider declaring the language (file owners may include dependencies).
    pub provider: String,
    /// Metadata with every declared file resolved to an exact TDS path.
    pub declaration: HyphenationDeclaration,
}

/// Language metadata resolved against one exact registry snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageManifest {
    /// Artifact schema, currently [`LANGUAGES_SCHEMA`].
    pub schema: String,
    /// Registry provenance, including its exact metadata digest.
    pub registry: Registry,
    /// Unique requested declarations, sorted by canonical name and provider.
    pub languages: Vec<ResolvedLanguage>,
}

impl HyphenationDeclaration {
    pub(crate) fn validate(&self) -> Result<(), PqtyError> {
        validate_language_name(&self.name)?;
        let mut names = BTreeSet::new();
        for synonym in &self.synonyms {
            validate_language_name(synonym)?;
            if !names.insert(synonym) {
                return Err(invalid("repeated language synonym"));
            }
        }
        validate_tds_path(&self.loader, "language loader")?;
        for paths in [&self.patterns, &self.exceptions] {
            let mut unique = BTreeSet::new();
            for path in paths {
                validate_tds_path(path, "language data")?;
                if !unique.insert(path) {
                    return Err(invalid("repeated language data path"));
                }
            }
        }
        let mut attributes = BTreeSet::new();
        for attribute in &self.attributes {
            if attribute.name.is_empty()
                || !attribute
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || !attributes.insert(&attribute.name)
                || attribute.value.chars().any(char::is_control)
            {
                return Err(invalid("invalid language attribute"));
            }
        }
        Ok(())
    }
}

// Project only declarations backed by the already locked runtime files. No
// registry reads or downloads are needed when an exact lock is replayed offline.
pub(crate) fn locked_languages(lock: &LockFile) -> Result<Vec<ResolvedLanguage>, PqtyError> {
    let files = lock
        .closure
        .iter()
        .flat_map(|entry| &entry.files)
        .collect::<Vec<_>>();
    let resolve = |request: &str| -> Result<Option<String>, PqtyError> {
        let request = normalize_runfile(request).1;
        let matches = files
            .iter()
            .filter(|file| {
                if request.contains('/') {
                    file.tds_path == request
                } else {
                    file.tds_path.rsplit('/').next() == Some(request.as_str())
                }
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => Ok(None),
            [file] => Ok(Some(file.tds_path.clone())),
            _ => Err(invalid(format!(
                "locked language file is ambiguous: {request}"
            ))),
        }
    };
    let mut languages = BTreeMap::new();
    let mut names = BTreeSet::new();
    for entry in &lock.closure {
        for declaration in &entry.hyphenation {
            declaration.validate()?;
            let mut declaration = declaration.clone();
            let requests = std::iter::once(&declaration.loader)
                .chain(&declaration.patterns)
                .chain(&declaration.exceptions);
            if requests
                .clone()
                .any(|path| matches!(resolve(path), Ok(None)))
            {
                // A declaration alone does not authorize adding another package.
                // Only complete installed languages are enabled by the consumer.
                continue;
            }
            declaration.loader =
                resolve(&declaration.loader)?.ok_or_else(|| invalid("missing language loader"))?;
            declaration.patterns = declaration
                .patterns
                .iter()
                .map(|path| resolve(path)?.ok_or_else(|| invalid("missing language pattern")))
                .collect::<Result<_, _>>()?;
            declaration.exceptions = declaration
                .exceptions
                .iter()
                .map(|path| resolve(path)?.ok_or_else(|| invalid("missing language exception")))
                .collect::<Result<_, _>>()?;
            for name in std::iter::once(&declaration.name).chain(
                declaration
                    .synonyms
                    .iter()
                    .filter(|synonym| **synonym != declaration.name),
            ) {
                if !names.insert(name.clone()) {
                    return Err(invalid(format!(
                        "locked language name is ambiguous: {name}"
                    )));
                }
            }
            languages.insert(
                (declaration.name.clone(), entry.provider.clone()),
                ResolvedLanguage {
                    provider: entry.provider.clone(),
                    declaration,
                },
            );
        }
    }
    Ok(languages.into_values().collect())
}

impl LanguageManifest {
    /// Verify every declaration, path, and registry record against an index.
    ///
    /// # Errors
    /// Returns an error for an unsupported schema, changed metadata, ambiguous
    /// declarations, incomplete files, or a different registry snapshot.
    pub fn validate(&self, index: &TlpdbIndex) -> Result<(), PqtyError> {
        if self.schema != LANGUAGES_SCHEMA {
            return Err(invalid("unsupported language manifest schema"));
        }
        let names = self
            .languages
            .iter()
            .map(|language| language.declaration.name.clone())
            .collect::<Vec<_>>();
        if *self != index.resolve_languages(&names)? {
            return Err(invalid(
                "language manifest differs from its registry snapshot",
            ));
        }
        Ok(())
    }

    /// Exact loader, pattern, and exception paths required by these languages.
    #[must_use]
    pub fn runtime_files(&self) -> Vec<String> {
        self.languages
            .iter()
            .flat_map(|language| {
                let declaration = &language.declaration;
                std::iter::once(&declaration.loader)
                    .chain(declaration.patterns.iter())
                    .chain(declaration.exceptions.iter())
                    .cloned()
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

impl TlpdbIndex {
    /// Resolve explicit language names or synonyms without guessing Babel options.
    ///
    /// Empty requests produce an empty manifest. This checks declared files and
    /// ownership, not whether an engine can activate the language's patterns.
    ///
    /// # Errors
    /// Returns an error for absent or ambiguous names, files, or file owners.
    pub fn resolve_languages(&self, names: &[String]) -> Result<LanguageManifest, PqtyError> {
        let mut declarations = BTreeMap::<&str, Vec<_>>::new();
        if !names.is_empty() {
            for (provider, package) in &self.packages {
                for declaration in &package.hyphenation {
                    for name in std::iter::once(&declaration.name).chain(
                        declaration
                            .synonyms
                            .iter()
                            .filter(|name| **name != declaration.name),
                    ) {
                        declarations
                            .entry(name)
                            .or_default()
                            .push((provider, declaration));
                    }
                }
            }
        }
        let mut selected = BTreeMap::new();
        for name in names {
            validate_language_name(name)?;
            let candidates = declarations
                .get(name.as_str())
                .map_or(&[][..], Vec::as_slice);
            let (provider, declaration) = match candidates {
                [candidate] => *candidate,
                [] => {
                    return Err(invalid(format!(
                        "no AddHyphen declaration for required language {name}"
                    )));
                }
                _ => return Err(invalid(format!("required language {name} is ambiguous"))),
            };
            let mut declaration = declaration.clone();
            declaration.loader = self.language_file(&declaration.loader)?;
            declaration.patterns = declaration
                .patterns
                .iter()
                .map(|file| self.language_file(file))
                .collect::<Result<_, _>>()?;
            declaration.exceptions = declaration
                .exceptions
                .iter()
                .map(|file| self.language_file(file))
                .collect::<Result<_, _>>()?;
            selected.insert(
                (declaration.name.clone(), provider.clone()),
                ResolvedLanguage {
                    provider: provider.clone(),
                    declaration,
                },
            );
        }
        Ok(LanguageManifest {
            schema: LANGUAGES_SCHEMA.to_string(),
            registry: self.registry(),
            languages: selected.into_values().collect(),
        })
    }

    fn language_file(&self, request: &str) -> Result<String, PqtyError> {
        let request = normalize_runfile(request).1;
        let providers = if request.contains('/') {
            self.providers_of_path(&request)
        } else {
            self.providers_of_file(&request)
        };
        let paths = providers
            .iter()
            .flat_map(|provider| {
                self.packages[*provider]
                    .runfiles
                    .iter()
                    .map(|path| normalize_runfile(path).1)
            })
            .filter(|path| {
                if request.contains('/') {
                    *path == request
                } else {
                    path.rsplit('/').next() == Some(request.as_str())
                }
            })
            .collect::<Vec<_>>();
        match paths.as_slice() {
            [path] => Ok(path.clone()),
            [] => Err(invalid(format!(
                "declared language file is absent from the selected registry: {request}"
            ))),
            _ => Err(invalid(format!(
                "declared language file has ambiguous ownership or paths: {request}"
            ))),
        }
    }
}

/// Add complete explicit language requirements using the existing v1 lock contract.
///
/// Declaration providers and exact loader/pattern/exception paths are persisted
/// as consumer requirements. Hydrate the resulting lock to verify package bytes.
///
/// # Errors
/// Returns an error without changing the lock if names, files, or the registry
/// snapshot cannot be resolved consistently.
pub fn require_languages(
    lock: &mut LockFile,
    index: &TlpdbIndex,
    names: &[String],
) -> Result<LanguageManifest, PqtyError> {
    let manifest = index.resolve_languages(names)?;
    let providers = manifest
        .languages
        .iter()
        .map(|language| language.provider.clone())
        .collect::<Vec<_>>();
    let mut updated = lock.clone();
    require_runtime(&mut updated, index, &manifest.runtime_files(), &providers)?;
    *lock = updated;
    Ok(manifest)
}

fn invalid(message: impl Into<String>) -> PqtyError {
    PqtyError::Usage(message.into())
}

fn validate_language_name(name: &str) -> Result<(), PqtyError> {
    if name.is_empty()
        || name.len() > 255
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(invalid(format!("invalid language name: {name:?}")));
    }
    Ok(())
}

pub(super) fn parse_add_hyphen(action: &str) -> Result<Option<HyphenationDeclaration>, PqtyError> {
    let Some(rest) = action
        .trim_start()
        .strip_prefix("AddHyphen")
        .filter(|rest| {
            rest.is_empty() || rest.starts_with(char::is_whitespace) || rest.starts_with(r"\t")
        })
    else {
        return Ok(None);
    };
    let mut fields = BTreeMap::new();
    let mut token = String::new();
    let mut quote = None;
    let mut chars = rest.chars().chain(std::iter::once(' ')).peekable();
    while let Some(mut ch) = chars.next() {
        // TeX Live metadata also uses literal backslash-t separators. Preserve
        // those bytes inside quoted extension values.
        if quote.is_none() && ch == '\\' && chars.peek() == Some(&'t') {
            chars.next();
            ch = ' ';
        }
        if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            } else {
                token.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if token.is_empty() {
                continue;
            }
            let (key, value) = token
                .split_once('=')
                .ok_or_else(|| invalid("malformed AddHyphen key=value field"))?;
            if key.is_empty()
                || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || value.chars().any(char::is_control)
                || fields.insert(key.to_string(), value.to_string()).is_some()
            {
                return Err(invalid("invalid or repeated AddHyphen field"));
            }
            token.clear();
        } else {
            token.push(ch);
        }
    }
    if quote.is_some() {
        return Err(invalid("unterminated AddHyphen quoted value"));
    }
    let name = fields
        .remove("name")
        .ok_or_else(|| invalid("AddHyphen is missing name"))?;
    validate_language_name(&name)?;
    let synonyms = fields
        .remove("synonyms")
        .filter(|s| !s.is_empty())
        .map(|s| s.split(',').map(str::to_string).collect::<BTreeSet<_>>())
        .unwrap_or_default();
    for synonym in &synonyms {
        validate_language_name(synonym)?;
    }
    let loader = fields
        .remove("file")
        .ok_or_else(|| invalid("AddHyphen is missing file"))?;
    validate_tds_path(&loader, "AddHyphen loader")?;
    let patterns = optional_files(&mut fields, "file_patterns")?;
    let exceptions = optional_files(&mut fields, "file_exceptions")?;
    let left_hyphen_min = minimum(&mut fields, "lefthyphenmin")?;
    let right_hyphen_min = minimum(&mut fields, "righthyphenmin")?;
    Ok(Some(HyphenationDeclaration {
        name,
        synonyms: synonyms.into_iter().collect(),
        loader,
        patterns,
        exceptions,
        left_hyphen_min,
        right_hyphen_min,
        attributes: fields
            .into_iter()
            .map(|(name, value)| LanguageAttribute { name, value })
            .collect(),
    }))
}

fn optional_files(
    fields: &mut BTreeMap<String, String>,
    key: &str,
) -> Result<Vec<String>, PqtyError> {
    let mut files = Vec::new();
    let Some(value) = fields.remove(key).filter(|value| !value.is_empty()) else {
        return Ok(files);
    };
    let mut unique = BTreeSet::new();
    for file in value.split(',') {
        validate_tds_path(file, "AddHyphen data file")?;
        if files.len() >= crate::MAX_PROVIDER_RUNFILES || !unique.insert(file) {
            return Err(invalid("oversized or repeated AddHyphen data file list"));
        }
        files.push(file.to_string());
    }
    Ok(files)
}

fn minimum(fields: &mut BTreeMap<String, String>, key: &str) -> Result<u8, PqtyError> {
    fields
        .remove(key)
        .ok_or_else(|| invalid(format!("AddHyphen is missing {key}")))?
        .parse()
        .map_err(|_| invalid(format!("invalid AddHyphen {key}")))
}
