//! Convert kpathsea `.fls` recorder output into an engine-neutral
//! `pqty.trace/v1` artifact.
//!
//! The adapter reads `INPUT` records, resolves relative paths against recorder
//! `PWD` records, and classifies every input through caller-supplied
//! [`RootMapping`] values. The most specific matching root wins. Unmapped
//! inputs and equally specific roots with conflicting scopes fail closed.
//! `OUTPUT` records are ignored; a generated file that is later read appears
//! as an `INPUT` and can be classified under an output root.
//!
//! Paths are normalized lexically rather than canonicalized through the
//! filesystem. This preserves the identity of a mounted package root whose
//! files may be symlinks into pqty's content-addressed store.
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//! use pqty_fls::{RootMapping, TraceScope, adapt_fls};
//!
//! # fn main() -> Result<(), pqty_fls::AdapterError> {
//! let working_directory = Path::new("/work/paper");
//! let roots = [
//!     RootMapping::new("/work/paper", TraceScope::Project, working_directory)?,
//!     RootMapping::new("/work/pqty-texmf", TraceScope::Package, working_directory)?,
//!     RootMapping::new("/work/paper/build", TraceScope::Output, working_directory)?,
//! ];
//! let recorder = "\
//! PWD /work/paper
//! INPUT ./main.tex
//! INPUT /work/pqty-texmf/tex/latex/foo/foo.sty
//! ";
//! let trace = adapt_fls(
//!     recorder,
//!     working_directory,
//!     &roots,
//!     Some("my-renderer/1.0".to_string()),
//!     None,
//! )?;
//! assert_eq!(trace.inputs.len(), 2);
//! # Ok(())
//! # }
//! ```
//!
//! Artifact Protocol v1 is the stable process-integration boundary. The Rust
//! API follows this crate's SemVer compatibility; while the major version is
//! zero, minor releases may contain breaking Rust API changes.

#![deny(missing_docs)]

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

/// Schema identifier emitted by [`adapt_fls`].
pub const TRACE_SCHEMA: &str = "pqty.trace/v1";

/// Engine-neutral input trace produced from a TeX recorder file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InputTrace {
    /// Artifact schema identifier; always [`TRACE_SCHEMA`].
    pub schema: String,
    /// Optional adapter name and version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    /// Fingerprint of the package environment mounted for the recorded run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_fingerprint: Option<String>,
    /// Deterministically ordered, deduplicated recorder inputs.
    pub inputs: Vec<ObservedInput>,
}

/// One input file observed by the TeX recorder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservedInput {
    /// Basename requested or opened by the engine.
    pub requested: String,
    /// Path relative to the matching classification root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    /// Ownership boundary assigned by the matching root.
    pub scope: TraceScope,
    /// Resource category inferred from the filename extension.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ResourceKind>,
}

/// Ownership boundary assigned to a recorder input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TraceScope {
    /// File owned by the locked package environment.
    Package,
    /// File owned by project source.
    Project,
    /// File supplied by the TeX engine or its configuration.
    Engine,
    /// Generated build output subsequently read by the engine.
    Output,
}

/// Engine-neutral category inferred for a recorder input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceKind {
    /// TeX source, package, class, or configuration file.
    Tex,
    /// Bibliography database or style file.
    Bibliography,
    /// TeX font metric file.
    FontMetric,
    /// Virtual font file.
    VirtualFont,
    /// Type 1 font program.
    Type1Font,
    /// OpenType font program.
    OpenTypeFont,
    /// TrueType font program or collection.
    TrueTypeFont,
    /// Font map fragment.
    Map,
    /// Font encoding file.
    Encoding,
    /// Hyphenation pattern file.
    Hyphenation,
    /// Precompiled TeX format.
    Format,
    /// Native executable or shared library.
    Binary,
    /// Any resource not covered by a more specific category.
    Data,
}

/// A native filesystem root and the ownership scope assigned beneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootMapping {
    /// Absolute, lexically normalized classification root.
    pub root: PathBuf,
    /// Scope assigned to matching recorder inputs.
    pub scope: TraceScope,
}

impl RootMapping {
    /// Create a root mapping, resolving relative roots against `relative_to`.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError::Usage`] when the resulting root is not absolute.
    pub fn new(
        root: impl AsRef<Path>,
        scope: TraceScope,
        relative_to: impl AsRef<Path>,
    ) -> Result<Self, AdapterError> {
        let root = absolute_lexical(root.as_ref(), relative_to.as_ref());
        if !root.is_absolute() {
            return Err(AdapterError::Usage(format!(
                "classification root is not absolute: {}",
                root.display()
            )));
        }
        Ok(Self { root, scope })
    }
}

/// Error returned while configuring or converting a recorder trace.
#[derive(Debug)]
pub enum AdapterError {
    /// A filesystem or stream operation failed.
    Io {
        /// Path associated with the failed operation.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// A root, fingerprint, or recorder record violated the adapter contract.
    Usage(String),
    /// Trace or environment JSON could not be encoded or decoded.
    Json(serde_json::Error),
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Usage(message) => formatter.write_str(message),
            Self::Json(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for AdapterError {}

impl From<serde_json::Error> for AdapterError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Convert kpathsea recorder contents into a deterministic pqty input trace.
///
/// Paths are normalized lexically rather than canonicalized through the
/// filesystem. A pqty TEXMF commonly contains symlinks into its content store;
/// following those symlinks would lose the mounted package-root identity.
pub fn adapt_fls(
    contents: &str,
    fallback_working_directory: &Path,
    roots: &[RootMapping],
    producer: Option<String>,
    environment_fingerprint: Option<String>,
) -> Result<InputTrace, AdapterError> {
    if roots.is_empty() {
        return Err(AdapterError::Usage(
            "at least one classification root is required".to_string(),
        ));
    }
    if let Some(fingerprint) = environment_fingerprint.as_deref() {
        validate_fingerprint(fingerprint)?;
    }

    let fallback_working_directory = absolute_lexical(
        fallback_working_directory,
        &std::env::current_dir().map_err(|source| AdapterError::Io {
            path: PathBuf::from("."),
            source,
        })?,
    );
    let mut working_directory = fallback_working_directory;
    let mut inputs: BTreeMap<(u8, String, String), ObservedInput> = BTreeMap::new();

    for (index, raw_line) in contents.lines().enumerate() {
        let line_number = index + 1;
        if let Some(path) = raw_line.strip_prefix("PWD ") {
            if path.is_empty() {
                return Err(AdapterError::Usage(format!(
                    "empty PWD record at recorder line {line_number}"
                )));
            }
            working_directory = absolute_lexical(Path::new(path), &working_directory);
            continue;
        }
        let Some(path) = raw_line.strip_prefix("INPUT ") else {
            continue;
        };
        if path.is_empty() {
            return Err(AdapterError::Usage(format!(
                "empty INPUT record at recorder line {line_number}"
            )));
        }

        let absolute = absolute_lexical(Path::new(path), &working_directory);
        let classification = classify(&absolute, roots).map_err(|error| {
            AdapterError::Usage(format!(
                "{error} at recorder line {line_number}: {}",
                absolute.display()
            ))
        })?;
        let requested = absolute
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                AdapterError::Usage(format!(
                    "recorder input has no filename at line {line_number}: {}",
                    absolute.display()
                ))
            })?
            .to_string();
        let mut resolved = slash_path(&classification.relative)?;
        if classification.scope == TraceScope::Package {
            resolved = resolved
                .strip_prefix("texmf-dist/")
                .unwrap_or(&resolved)
                .to_string();
        }
        if resolved.is_empty() {
            return Err(AdapterError::Usage(format!(
                "classification root names a file, not a directory: {}",
                classification.root.display()
            )));
        }
        let input = ObservedInput {
            requested: requested.clone(),
            resolved: Some(resolved.clone()),
            scope: classification.scope,
            kind: Some(resource_kind(&resolved)),
        };
        inputs
            .entry((scope_rank(classification.scope), resolved, requested))
            .or_insert(input);
    }

    Ok(InputTrace {
        schema: TRACE_SCHEMA.to_string(),
        producer,
        environment_fingerprint,
        inputs: inputs.into_values().collect(),
    })
}

#[derive(Debug)]
struct Classification<'a> {
    root: &'a Path,
    relative: PathBuf,
    scope: TraceScope,
}

fn classify<'a>(path: &Path, roots: &'a [RootMapping]) -> Result<Classification<'a>, AdapterError> {
    let mut matches = roots
        .iter()
        .filter_map(|mapping| {
            path.strip_prefix(&mapping.root)
                .ok()
                .map(|relative| (mapping, relative.to_path_buf()))
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Err(AdapterError::Usage(
            "recorder input is outside every declared root".to_string(),
        ));
    }
    matches.sort_by(|(left, _), (right, _)| {
        right
            .root
            .components()
            .count()
            .cmp(&left.root.components().count())
            .then_with(|| left.root.cmp(&right.root))
    });
    let specificity = matches[0].0.root.components().count();
    let most_specific = matches
        .iter()
        .take_while(|(mapping, _)| mapping.root.components().count() == specificity)
        .collect::<Vec<_>>();
    let scope = most_specific[0].0.scope;
    if most_specific
        .iter()
        .any(|(mapping, _)| mapping.scope != scope)
    {
        let roots = most_specific
            .iter()
            .map(|(mapping, _)| format!("{}={}", scope_name(mapping.scope), mapping.root.display()))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(AdapterError::Usage(format!(
            "recorder input matches equally specific roots with different scopes ({roots})"
        )));
    }
    Ok(Classification {
        root: &most_specific[0].0.root,
        relative: most_specific[0].1.clone(),
        scope,
    })
}

fn validate_fingerprint(fingerprint: &str) -> Result<(), AdapterError> {
    let digest = fingerprint.strip_prefix("sha256:").unwrap_or_default();
    if digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(AdapterError::Usage(format!(
            "invalid environment fingerprint: {fingerprint}"
        )))
    }
}

fn absolute_lexical(path: &Path, relative_to: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        relative_to.join(path)
    };
    lexical_normalize(&joined)
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn slash_path(path: &Path) -> Result<String, AdapterError> {
    path.to_str()
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| {
            AdapterError::Usage(format!(
                "classified input cannot be represented as a Portable Path: {}",
                path.display()
            ))
        })
}

fn scope_rank(scope: TraceScope) -> u8 {
    match scope {
        TraceScope::Package => 0,
        TraceScope::Project => 1,
        TraceScope::Engine => 2,
        TraceScope::Output => 3,
    }
}

fn scope_name(scope: TraceScope) -> &'static str {
    match scope {
        TraceScope::Package => "package",
        TraceScope::Project => "project",
        TraceScope::Engine => "engine",
        TraceScope::Output => "output",
    }
}

fn resource_kind(path: &str) -> ResourceKind {
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

#[cfg(test)]
mod tests {
    use crate::{RootMapping, TRACE_SCHEMA, TraceScope, adapt_fls, slash_path};
    #[cfg(windows)]
    use std::path::Path;
    use std::path::PathBuf;

    fn fixture_root(path: &str) -> PathBuf {
        std::env::current_dir()
            .unwrap()
            .join("adapter-platform-fixture")
            .join(path)
    }

    fn roots() -> Vec<RootMapping> {
        vec![
            RootMapping::new(
                fixture_root("project"),
                TraceScope::Project,
                fixture_root("fallback"),
            )
            .unwrap(),
            RootMapping::new(
                fixture_root("project/build"),
                TraceScope::Output,
                fixture_root("fallback"),
            )
            .unwrap(),
            RootMapping::new(
                fixture_root("pqty"),
                TraceScope::Package,
                fixture_root("fallback"),
            )
            .unwrap(),
            RootMapping::new(
                fixture_root("system/texmf"),
                TraceScope::Package,
                fixture_root("fallback"),
            )
            .unwrap(),
            RootMapping::new(
                fixture_root("system/texmf/fonts"),
                TraceScope::Engine,
                fixture_root("fallback"),
            )
            .unwrap(),
            RootMapping::new(
                fixture_root("system/config"),
                TraceScope::Engine,
                fixture_root("fallback"),
            )
            .unwrap(),
        ]
    }

    #[test]
    fn converts_inputs_with_longest_root_classification_and_deduplication() {
        let project = fixture_root("project");
        let recorder = format!(
            "PWD {}\n\
             INPUT ./main.tex\n\
             INPUT {}\n\
             INPUT {}\n\
             INPUT {}\n\
             INPUT {}\n\
             INPUT {}\n\
             INPUT {}\n\
             OUTPUT {}\n",
            project.display(),
            fixture_root("project/build/main.aux").display(),
            fixture_root("pqty/tex/latex/foo/foo.sty").display(),
            fixture_root("pqty/tex/latex/foo/foo.sty").display(),
            fixture_root("system/texmf/tex/latex/bar/bar.sty").display(),
            fixture_root("system/texmf/fonts/tfm/public/cm/cmr10.tfm").display(),
            fixture_root("system/config/web2c/texmf.cnf").display(),
            fixture_root("project/build/main.pdf").display(),
        );
        let trace = adapt_fls(
            &recorder,
            &fixture_root("fallback"),
            &roots(),
            Some("pqty-fls/test".to_string()),
            Some(format!("sha256:{}", "01".repeat(32))),
        )
        .unwrap();

        assert_eq!(trace.schema, TRACE_SCHEMA);
        assert_eq!(trace.inputs.len(), 6);
        assert_eq!(trace.inputs[0].scope, TraceScope::Package);
        assert_eq!(
            trace.inputs[0].resolved.as_deref(),
            Some("tex/latex/bar/bar.sty")
        );
        assert_eq!(trace.inputs[1].scope, TraceScope::Package);
        assert_eq!(
            trace.inputs[1].resolved.as_deref(),
            Some("tex/latex/foo/foo.sty")
        );
        assert_eq!(trace.inputs[2].scope, TraceScope::Project);
        assert_eq!(trace.inputs[2].resolved.as_deref(), Some("main.tex"));
        assert_eq!(trace.inputs[3].scope, TraceScope::Engine);
        assert_eq!(trace.inputs[4].scope, TraceScope::Engine);
        assert_eq!(trace.inputs[5].scope, TraceScope::Output);
    }

    #[test]
    fn strips_a_texmf_dist_prefix_for_package_roots() {
        let texlive = fixture_root("texlive");
        let roots = vec![
            RootMapping::new(&texlive, TraceScope::Package, fixture_root("fallback")).unwrap(),
        ];
        let trace = adapt_fls(
            &format!(
                "INPUT {}\n",
                texlive.join("texmf-dist/tex/latex/foo/foo.sty").display()
            ),
            &fixture_root("fallback"),
            &roots,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            trace.inputs[0].resolved.as_deref(),
            Some("tex/latex/foo/foo.sty")
        );
    }

    #[test]
    fn golden_v1_trace_is_a_real_adapter_output() {
        let package = fixture_root("pqty");
        let roots = vec![
            RootMapping::new(&package, TraceScope::Package, fixture_root("fallback")).unwrap(),
        ];
        let trace = adapt_fls(
            &format!(
                "INPUT {}\n",
                package.join("tex/latex/foo/foo.sty").display()
            ),
            &fixture_root("fallback"),
            &roots,
            Some("pqty-fls 0.1.0".to_string()),
            Some(
                "sha256:95ea8e9b9a2b0b5478a49763e115d9adb5f0a66d2f340f235bb28b08bbeb434a"
                    .to_string(),
            ),
        )
        .unwrap();
        let golden: serde_json::Value =
            serde_json::from_str(include_str!("../tests/golden/v1/trace.json")).unwrap();
        assert_eq!(serde_json::to_value(trace).unwrap(), golden);
    }

    #[test]
    fn rejects_unmapped_inputs() {
        let unknown = fixture_root("unknown/foo.sty");
        let error = adapt_fls(
            &format!("INPUT {}\n", unknown.display()),
            &fixture_root("project"),
            &roots(),
            None,
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("outside every declared root"));
        assert!(error.to_string().contains("recorder line 1"));
    }

    #[test]
    fn rejects_equally_specific_conflicting_roots() {
        let same = fixture_root("same");
        let roots = vec![
            RootMapping::new(&same, TraceScope::Package, fixture_root("fallback")).unwrap(),
            RootMapping::new(&same, TraceScope::Engine, fixture_root("fallback")).unwrap(),
        ];
        let error = adapt_fls(
            &format!("INPUT {}\n", same.join("foo.sty").display()),
            &fixture_root("fallback"),
            &roots,
            None,
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("different scopes"));
    }

    #[test]
    fn rejects_invalid_environment_fingerprints() {
        let error = adapt_fls(
            "",
            &fixture_root("fallback"),
            &roots(),
            None,
            Some("latest".to_string()),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid environment fingerprint")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_portable_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let path = PathBuf::from(OsString::from_vec(vec![b'f', 0x80]));
        assert!(
            slash_path(&path)
                .unwrap_err()
                .to_string()
                .contains("Portable Path")
        );
    }

    #[cfg(windows)]
    #[test]
    fn rejects_non_utf8_portable_paths() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        let path = PathBuf::from(OsString::from_wide(&[0xD800]));
        assert!(
            slash_path(&path)
                .unwrap_err()
                .to_string()
                .contains("Portable Path")
        );
    }

    #[cfg(windows)]
    #[test]
    fn converts_drive_letter_roots_to_portable_trace_paths() {
        let roots = vec![
            RootMapping::new(
                Path::new(r"C:\work\project"),
                TraceScope::Project,
                Path::new(r"C:\work"),
            )
            .unwrap(),
            RootMapping::new(
                Path::new(r"C:\pqty\texmf"),
                TraceScope::Package,
                Path::new(r"C:\work"),
            )
            .unwrap(),
        ];
        let trace = adapt_fls(
            "PWD C:\\work\\project\nINPUT C:\\pqty\\texmf\\tex\\latex\\foo\\foo.sty\n",
            Path::new(r"C:\work\project"),
            &roots,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            trace.inputs[0].resolved.as_deref(),
            Some("tex/latex/foo/foo.sty")
        );
        assert!(!trace.inputs[0].resolved.as_deref().unwrap().contains(':'));
        assert!(!trace.inputs[0].resolved.as_deref().unwrap().contains('\\'));
    }
}
