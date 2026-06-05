use serde::{Deserialize, Serialize};

/// Placement strategy used when publishing files into a TEXMF tree.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum LinkMode {
    /// Copy file content into the TEXMF tree (supported mode).
    Copy,
    /// Experimentally symlink each file to the immutable store.
    #[value(name = "experimental-symlink")]
    Symlink,
    /// Experimentally hardlink each file to the immutable store.
    #[value(name = "experimental-hardlink")]
    Hardlink,
}

/// Versioned dependency lock for one LaTeX project.
///
/// The [`stage`](Self::stage) determines which resolution and integrity fields
/// must be populated. Use [`crate::validate_lock`] before consuming a value
/// received from another process.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockFile {
    /// Artifact schema identifier; currently [`crate::LOCK_SCHEMA`].
    pub schema: String,
    /// Explicit completeness state of this lock.
    pub stage: LockStage,
    /// Provenance of the producing tool (npm/Cargo/uv all stamp this).
    pub generated_with: String,
    /// Declared resolution environment. Empty until a resolver runs.
    #[serde(default, skip_serializing_if = "Environment::is_empty")]
    pub environment: Environment,
    /// Pinned package registries/snapshots. Empty until a resolver runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registries: Vec<Registry>,
    /// Generic provider and runtime-file requirements declared by a Consumer.
    /// These are resolution inputs, not inferred explanations, and therefore
    /// participate in Exact Lock reuse.
    #[serde(default, skip_serializing_if = "ConsumerRequirements::is_empty")]
    pub consumer_requirements: ConsumerRequirements,
    /// Project-relative path of the root TeX source file.
    pub root: String,
    /// Every project-owned source file read during scanning.
    pub sources: Vec<SourceRecord>,
    /// Document class declared by `\documentclass`, when present.
    pub document_class: Option<ClassRecord>,
    /// Additional classes loaded by project-owned source.
    pub loaded_classes: Vec<ClassRecord>,
    /// Package declarations found in project-owned source.
    pub packages: Vec<PackageRecord>,
    /// Project source inputs declared with `\input`, `\include`, or
    /// `\includeonly`.
    pub inputs: Vec<InputRecord>,
    /// Bibliography databases and styles declared by project source.
    pub bibliographies: Vec<BibliographyRecord>,
    /// Graphics referenced by project source.
    pub graphics: Vec<GraphicRecord>,
    /// Flat resolved dependency closure (Cargo/uv model): every provider needed,
    /// direct and transitive. Empty until a resolver runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closure: Vec<ResolvedPackage>,
    /// Source or package requests that could not be resolved unambiguously.
    pub unresolved: Vec<UnresolvedRecord>,
}

/// Runtime providers and files explicitly required by a consumer.
///
/// These requirements are independent of static source discovery and
/// participate in exact-lock reuse checks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerRequirements {
    /// Registry provider names that must appear in the closure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    /// Runtime basenames or normalized TDS paths that must appear in the
    /// closure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

impl ConsumerRequirements {
    pub(crate) fn is_empty(&self) -> bool {
        self.providers.is_empty() && self.files.is_empty()
    }
}

/// Completeness state of a [`LockFile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LockStage {
    /// Project source was scanned, but no package registry was consulted.
    Scanned,
    /// Package providers and dependency edges were resolved.
    Resolved,
    /// Every provider file was verified and stored by content hash.
    Exact,
}

/// Digest record for one project-owned source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecord {
    /// Normalized path relative to the project root.
    pub path: String,
    /// SHA-256 digest prefixed with `sha256:`.
    pub digest: String,
}

/// A document class declaration discovered in project source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassRecord {
    /// Class name without the `.cls` suffix.
    pub name: String,
    /// Options supplied to the class declaration.
    pub options: Vec<String>,
    /// Project-owned `.cls` path. Absent when the class must come from a
    /// package registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<String>,
    /// Source location of the declaration.
    pub source: Location,
}

/// A package declaration discovered in project source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRecord {
    /// Package name without the `.sty` suffix.
    pub name: String,
    /// Options supplied to the package declaration.
    pub options: Vec<String>,
    /// TeX command that loaded the package, such as `usepackage`.
    pub command: String,
    /// Project-owned `.sty` path. Absent when the package must come from a
    /// package registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<String>,
    /// Source location of the declaration.
    pub source: Location,
}

/// A project source input declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputRecord {
    /// Form of input command used by the source.
    pub kind: InputKind,
    /// Filename as written in the command.
    pub name: String,
    /// Normalized project-relative path when the input was found locally.
    pub resolved_path: Option<String>,
    /// Source location of the declaration.
    pub source: Location,
}

/// TeX command used to declare a source input.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputKind {
    /// An `\input` declaration.
    Input,
    /// An `\include` declaration.
    Include,
    /// An `\includeonly` declaration.
    IncludeOnly,
}

/// A bibliography database or style declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BibliographyRecord {
    /// Form of bibliography declaration.
    pub kind: BibliographyKind,
    /// Resource name as written in the command.
    pub name: String,
    /// Normalized project-relative path when found locally.
    pub resolved_path: Option<String>,
    /// Source location of the declaration.
    pub source: Location,
}

/// TeX command used to declare a bibliography resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BibliographyKind {
    /// An `\addbibresource` declaration.
    AddBibResource,
    /// A `\bibliography` database declaration.
    Bibliography,
    /// A `\bibliographystyle` declaration.
    BibliographyStyle,
}

/// A graphics resource declaration discovered in project source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphicRecord {
    /// Graphics resource name as written in `\includegraphics`.
    pub name: String,
    /// Normalized project-relative path when the graphic was found locally.
    pub resolved_path: Option<String>,
    /// Source location of the declaration.
    pub source: Location,
}

/// A source or registry request that did not resolve uniquely.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedRecord {
    /// Resource category, such as `package` or `input`.
    pub kind: String,
    /// Unresolved name as requested by the source.
    pub name: String,
    /// Source location that introduced the request.
    pub source: Location,
    /// Sorted provider candidates when resolution was ambiguous.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
}

/// Project source location associated with a discovered declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    /// Normalized path relative to the project root.
    pub path: String,
    /// One-based source line number.
    pub line: usize,
}

// ---------------------------------------------------------------------------
// Resolution model (pqty.lock/v1).
//
// Borrowed deliberately from modern language package managers, keeping only
// what fits LaTeX:
//   * integrity hashes + recorded dependency edges   (npm, Cargo, uv)
//   * explicit typed package sources                 (Cargo, uv)
//   * direct-vs-transitive distinction               (npm, Cargo)
//   * content-addressable shared store keys           (pnpm)
//   * declared environment / markers                  (uv requires-python)
//
// Deliberately NOT borrowed: SemVer range solving / MVS. TeX Live is an
// internally-consistent distribution, so pqty pins ONE snapshot (nixpkgs/distro
// model) instead of solving per-package version ranges.
// ---------------------------------------------------------------------------

/// Resolution environment the closure must satisfy (e.g. which TeX engines).
/// Mirrors uv's `requires-python` / resolution markers: the lock is pinned
/// against a declared environment, not "whatever happens to be installed".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    /// Engine identifiers the resolved closure is known to require.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub engines: Vec<String>,
}

impl Environment {
    pub(crate) fn is_empty(&self) -> bool {
        self.engines.is_empty()
    }
}

/// A package registry and the exact metadata used for provider resolution.
///
/// Dated remote registries record their immutable selector in
/// [`snapshot`](Self::snapshot). Local installations may instead use their
/// declared TeX Live release or leave the optional selector absent; the
/// metadata digest remains mandatory for resolved and exact locks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    /// Stable id referenced by `PackageSource.registry`, e.g. "tlnet".
    pub id: String,
    /// Registry backend and package format.
    pub kind: RegistryKind,
    /// Registry origin, either a tlnet base URL or a local `file://` root.
    pub url: String,
    /// Immutable date selector or TeX Live release, when one is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    /// SHA-256 identity of the registry metadata used for provider lookup.
    /// Convergence requires this to prevent mixing packages from two states of
    /// a rolling registry or locally updated TeX Live installation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_digest: Option<String>,
}

/// Package-registry backend represented by a [`Registry`] record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryKind {
    /// TeX Live network repository (pre-built runfiles, ready to drop in).
    Tlnet,
}

/// A package after resolution. Carries identity, provenance, integrity, and the
/// recorded dependency edges so the closure can be verified and rebuilt offline
/// (Cargo and uv keep the graph in the lock for exactly this reason).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedPackage {
    /// tlpdb package/bundle that provides the requested `.sty`/class. Many-to-one:
    /// `graphicx` resolves to provider `graphics`.
    pub provider: String,
    /// TL revision or catalogue version: informational + for audit. Derived from
    /// the pinned snapshot, not independently range-solved.
    pub version: String,
    /// Registry source from which the provider's bytes can be recovered.
    pub source: PackageSource,
    /// Requested `.sty`/`.cls` stems this provider satisfies directly (the link
    /// back to the scanned manifest). Empty for transitive-only providers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub satisfies: Vec<String>,
    /// Subresource-integrity-style digest of the resolved artifact, e.g.
    /// `sha256-<base64>` (npm integrity / Cargo checksum / uv hash). Filled at
    /// materialize time, when actual bytes are fetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity: Option<String>,
    /// Provider names this package depends on (tlpdb `depend` edges).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    /// True when this provider is a root requirement from project source or a
    /// runtime trace; false when pulled transitively.
    pub direct: bool,
    /// For transitive packages: the providers that pulled this one in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_by: Vec<String>,
    /// Content-addressable key in the shared pqty store (pnpm-style dedup across
    /// projects via a global store + linking). Filled at materialize time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_key: Option<String>,
    /// Engine constraints when the package is engine-specific
    /// (e.g. fontspec → xetex/luatex).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub engines: Vec<String>,
    /// Font-map fragments declared by the package registry (`addMap` and
    /// `addMixedMap`). Consumers combine these into their engine configuration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub font_maps: Vec<String>,
    /// Concrete runtime filenames whose static loads or engine-neutral traces
    /// caused this provider to enter the closure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_requests: Vec<String>,
    /// Exact runtime files owned by this provider. Filled together with
    /// `integrity`; consumers can use this engine-neutral index instead of
    /// depending on a materialized filesystem layout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<LockedFile>,
}

/// Where the resolved bytes come from. Artifact Protocol v1 supports verified
/// TeX Live registry packages only; future source types require a new protocol
/// version with complete resolution and installation semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSource {
    /// References `Registry.id` for registry sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    /// Source implementation used to retrieve the package.
    pub kind: PackageSourceKind,
    /// Path within the registry, URL, or git revision, depending on `kind`.
    pub locator: String,
    /// sha512 of the downloadable `.tar.xz` container (tlpdb `containerchecksum`),
    /// verified before extraction when fetching remotely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_checksum: Option<String>,
    /// Size in bytes of the container (tlpdb `containersize`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_size: Option<u64>,
}

/// Supported source implementation for package bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageSourceKind {
    /// Package supplied by the registry referenced in [`PackageSource::registry`].
    Registry,
}

/// One runtime file included in an exact provider record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedFile {
    /// Canonical path relative to the TEXMF root.
    pub tds_path: String,
    /// Resource category derived from the file extension.
    pub kind: ResourceKind,
}

/// Engine-neutral category assigned to a package or trace resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// A font-loader family/style name whose concrete OTF or TTF extension is
    /// not known until it is matched against registry metadata.
    FontFamily,
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

/// Renderer-facing projection of a fully verified exact lock.
///
/// The fingerprint covers the complete serialized package environment and is
/// suitable for renderer cache keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedEnvironment {
    /// Artifact schema identifier; currently [`crate::ENVIRONMENT_SCHEMA`].
    pub schema: String,
    /// Deterministic SHA-256 identity of the complete environment.
    pub fingerprint: String,
    /// Lock schema from which this environment was projected.
    pub lock_schema: String,
    /// Registry provenance inherited from the exact lock.
    pub registries: Vec<Registry>,
    /// Engine and tool requirements known to the package layer.
    pub requirements: EnvironmentRequirements,
    /// Registry-declared font-map fragments required by the package closure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub font_maps: Vec<String>,
    /// Providers in the exact package closure.
    pub packages: Vec<EnvironmentPackage>,
    /// Complete package-owned runtime-file index.
    pub files: Vec<EnvironmentFile>,
}

/// Engine and external-tool constraints attached to an environment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentRequirements {
    /// Engine identifiers required by the selected provider closure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub engines: Vec<String>,
    /// Non-engine runtime tools explicitly required by the environment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_tools: Vec<String>,
}

/// Renderer-facing identity and dependency metadata for one provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentPackage {
    /// Registry provider name.
    pub provider: String,
    /// Registry revision or version string.
    pub version: String,
    /// Recoverable package-byte source.
    pub source: PackageSource,
    /// Integrity digest over the provider's sorted path/content manifest.
    pub integrity: String,
    /// Whether project or consumer requirements selected this provider
    /// directly.
    pub direct: bool,
    /// Direct provider dependency names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
}

/// Renderer-facing ownership record for one package runtime file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentFile {
    /// Basename normally requested by TeX, e.g. `graphicx.sty`.
    pub request_name: String,
    /// Canonical path relative to the TEXMF root.
    pub tds_path: String,
    /// Provider that owns this TDS path.
    pub owner: String,
    /// Engine-neutral resource category.
    pub kind: ResourceKind,
}

/// Engine-neutral record of files observed during one renderer run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputTrace {
    /// Artifact schema identifier; currently [`crate::TRACE_SCHEMA`].
    pub schema: String,
    /// Optional adapter name and version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    /// Environment mounted for the recorded run. Optional for simple adapters;
    /// when present, pqty rejects stale or concurrently crossed traces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_fingerprint: Option<String>,
    /// Inputs observed by the renderer.
    pub inputs: Vec<ObservedInput>,
}

/// One file request observed during a renderer run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedInput {
    /// Basename or family name requested by the renderer.
    pub requested: String,
    /// Normalized package-relative path when the adapter can provide it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    /// Ownership boundary assigned by the renderer adapter.
    pub scope: TraceScope,
    /// Optional engine-neutral resource category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ResourceKind>,
}

/// Ownership boundary assigned to an observed renderer input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TraceScope {
    /// File expected to come from the locked package environment.
    Package,
    /// File owned by project source.
    Project,
    /// File supplied by the engine, format, or engine configuration.
    Engine,
    /// Generated build output subsequently read by the renderer.
    Output,
}

/// Result of reconciling a trace against one exact environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceReport {
    /// Artifact schema identifier; currently [`crate::TRACE_REPORT_SCHEMA`].
    pub schema: String,
    /// Fingerprint of the environment used for reconciliation.
    pub environment_fingerprint: String,
    /// Package inputs matched to exact locked files.
    pub matched: Vec<TraceMatch>,
    /// Package inputs not present unambiguously in the environment.
    pub missing: Vec<ObservedInput>,
    /// Project, engine, and output inputs outside package reconciliation.
    pub ignored: Vec<ObservedInput>,
}

/// Successful match between an observed request and a locked package file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceMatch {
    /// Name requested by the renderer.
    pub requested: String,
    /// Exact normalized TDS path in the environment.
    pub tds_path: String,
    /// Provider that owns the matched path.
    pub owner: String,
}

/// Outcome of a trace-driven convergence attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConvergenceStatus {
    /// Every package input was already satisfied; the lock is unchanged.
    Stable,
    /// New verified providers were added to the lock.
    Changed,
    /// At least one package input could not be resolved safely.
    Unresolved,
}

/// Report emitted after attempting to converge an exact lock with a trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConvergenceReport {
    /// Artifact schema identifier; currently
    /// [`crate::CONVERGENCE_REPORT_SCHEMA`].
    pub schema: String,
    /// Overall convergence outcome.
    pub status: ConvergenceStatus,
    /// Fingerprint of the environment that produced the input trace.
    pub previous_environment_fingerprint: String,
    /// Fingerprint after convergence; unchanged unless providers were added.
    pub environment_fingerprint: String,
    /// Sorted provider names newly added to the exact lock.
    pub added_providers: Vec<String>,
    /// Package inputs matched by the final candidate environment.
    pub matched: Vec<TraceMatch>,
    /// Package inputs that prevented convergence.
    pub unresolved: Vec<UnresolvedTraceInput>,
    /// Project, engine, and output inputs outside package convergence.
    pub ignored: Vec<ObservedInput>,
}

/// Package trace input that could not be resolved during convergence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedTraceInput {
    /// Original observed input fields.
    #[serde(flatten)]
    pub input: ObservedInput,
    /// Reason convergence could not safely add a provider.
    pub reason: ConvergenceUnresolvedReason,
    /// Normalized filename or TDS path used for provider lookup, when valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<String>,
    /// Single provider considered during reconciliation, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Sorted provider candidates when ownership was ambiguous.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
}

/// Reason a package trace input could not converge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConvergenceUnresolvedReason {
    /// Requested name or path violated portable artifact rules.
    InvalidRequest,
    /// No provider in the locked registry owns the requested resource.
    NoProvider,
    /// Multiple providers own the request and no exact path disambiguated it.
    AmbiguousProvider,
    /// The selected provider was already locked but did not match the input.
    ProviderAlreadyLocked,
    /// Adding the selected provider still did not satisfy the observed input.
    ProviderDidNotSatisfy,
}
