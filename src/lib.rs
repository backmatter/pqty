//! Discover, lock, verify, and materialize the TeX Live package layer required
//! by a LaTeX project.
//!
//! pqty deliberately stops at the package boundary. It does not run a TeX
//! engine, schedule compilation passes, invoke bibliography tools, or produce
//! a PDF. A renderer can use the exact lock and TEXMF tree produced here with
//! its own engine and build policy.
//!
//! # Typical lifecycle
//!
//! 1. Scan project source with [`scan_project_at`] or [`scan_source`].
//! 2. Resolve declarations against a [`PackageRegistry`] with [`resolve`].
//! 3. Verify provider bytes and create an exact lock with [`hydrate_lock`].
//! 4. Publish a TEXMF tree with [`materialize_from_store`] or reproduce an
//!    existing exact lock with [`install_locked`].
//! 5. Project the lock into a renderer-facing [`ResolvedEnvironment`].
//! 6. Optionally reconcile [`InputTrace`] observations or extend the lock with
//!    [`converge_trace`].
//!
//! The command-line interface wraps these primitives with registry selection,
//! cache policy, offline behavior, and atomic artifact writes.
//!
//! # Example
//!
//! ```no_run
//! use pqty::{ResolvedEnvironment, scan_project_at, validate_lock};
//!
//! # fn main() -> Result<(), pqty::PqtyError> {
//! let scanned = scan_project_at(".", "paper/main.tex")?;
//! validate_lock(&scanned)?;
//! println!("discovered {} package declarations", scanned.packages.len());
//!
//! // After resolution and hydration, an exact lock can be projected for a
//! // renderer:
//! if scanned.stage == pqty::LockStage::Exact {
//!     let environment = ResolvedEnvironment::from_lock(&scanned)?;
//!     println!("{}", environment.fingerprint);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Compatibility
//!
//! Artifact Protocol v1—the JSON schemas named by [`LOCK_SCHEMA`],
//! [`ENVIRONMENT_SCHEMA`], [`TRACE_SCHEMA`], and the related constants—is the
//! stable process-integration boundary. Unknown fields are rejected and
//! artifact inputs should be validated before use. The Rust API follows the
//! crate's `SemVer` compatibility; while the major version is zero, minor
//! releases may include breaking Rust API changes.

#![deny(missing_docs)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

/// Schema identifier for serialized [`LockFile`] artifacts.
pub const LOCK_SCHEMA: &str = "pqty.lock/v1";
/// Schema identifier for serialized [`ResolvedEnvironment`] artifacts.
pub const ENVIRONMENT_SCHEMA: &str = "pqty.env/v1";
/// Schema identifier for serialized [`InputTrace`] artifacts.
pub const TRACE_SCHEMA: &str = "pqty.trace/v1";
/// Schema identifier for serialized [`TraceReport`] artifacts.
pub const TRACE_REPORT_SCHEMA: &str = "pqty.trace-report/v1";
/// Schema identifier for serialized [`ConvergenceReport`] artifacts.
pub const CONVERGENCE_REPORT_SCHEMA: &str = "pqty.convergence-report/v1";
/// Schema identifier for JSON Lines progress events emitted by the CLI.
pub const PROGRESS_SCHEMA: &str = "pqty.progress/v1";

/// Default tlnet registry (tracks "latest").
const DEFAULT_TLNET: &str = "https://mirror.ctan.org/systems/texlive/tlnet";
/// Historic TeX Live network snapshots, indexed by UTC calendar date.
const TEXLIVE_ARCHIVE_ROOT: &str = "https://texlive.info/tlnet-archive";
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(60);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_CONTAINER_COMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
const MIN_CONTAINER_EXPANDED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CONTAINER_EXPANDED_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ARCHIVE_ENTRY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CLOSURE_EXPANDED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_PROVIDER_RUNFILES: usize = 100_000;
const MAX_CLOSURE_RUNFILES: usize = 1_000_000;
const MAX_TLPDB_PACKAGES: usize = 250_000;
const MAX_TLPDB_LINE_BYTES: usize = 1024 * 1024;
const MAX_TLPDB_COMPRESSED_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TLPDB_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;
static OPERATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

mod error;
pub use error::PqtyError;

mod artifact;
#[cfg(test)]
use artifact::validation::validate_portable_path;
pub use artifact::{
    BibliographyKind, BibliographyRecord, ClassRecord, ConsumerRequirements, ConvergenceReport,
    ConvergenceStatus, ConvergenceUnresolvedReason, Environment, EnvironmentFile,
    EnvironmentPackage, EnvironmentRequirements, GraphicRecord, InputKind, InputRecord, InputTrace,
    LinkMode, Location, LockFile, LockStage, LockedFile, ObservedInput, PackageRecord,
    PackageSource, PackageSourceKind, Registry, RegistryKind, ResolvedEnvironment, ResolvedPackage,
    ResourceKind, SourceRecord, TraceMatch, TraceReport, TraceScope, UnresolvedRecord,
    UnresolvedTraceInput, read_lock, read_trace, validate_lock, validate_materialized_lock,
    write_lock,
};
pub(crate) use artifact::{
    atomic_write, closure_by_provider, fail_on_unresolved_packages, font_filename_is_case_folded,
    invalid_artifact_text, normalize_trace_path, resource_kind, unique_sibling, usable_parent,
    validate_provider_identifier, validate_tds_path,
};

mod path;
pub use path::{SourceTree, VirtualPath};

mod progress;

mod source;
#[cfg(test)]
pub(crate) use source::clean_relative_name;
#[cfg(test)]
use source::digest_bytes;
pub use source::{
    FileSystemSourceTree, MemorySourceTree, scan_project, scan_project_at,
    scan_project_at_with_roots, scan_source,
};
pub(crate) use source::{
    ParsedCommand, canonical_or_original, print_explanation, print_tree, print_why,
    scan_cli_project, scan_commands, split_names,
};

mod registry;
pub use registry::{IndexPackage, PackageRegistry, TlpdbIndex, require_runtime, resolve};
pub(crate) use registry::{locate_tlpdb, normalized_consumer_requirements, resolved_from};

mod store;
#[cfg(test)]
use store::{BoundedWriter, take_container_runfile};
pub use store::{
    MaterializeReport, PackageByteSource, hydrate_lock, install_locked, materialize_from_store,
};
pub(crate) use store::{
    MetadataCachePolicy, RegistryRequest, add_trace_providers, byte_source, default_store_dir,
    fetch_tlpdb, install_locked_with_policy, load_convergence_index, load_tlpdb_index,
    normalize_runfile, read_bounded, tlnet_base_from_tlpdb_url, validate_tlpdb_digest,
};

mod convergence;
pub use convergence::converge_trace;
pub(crate) use convergence::stable_convergence_report;
#[cfg(test)]
use convergence::trace_provider_candidates;

mod cli;

/// Run the `pqty` command-line interface using the process arguments.
///
/// Errors are printed to standard error and terminate the process with a
/// nonzero exit status.
pub fn main_entry() {
    cli::main_entry();
}

#[cfg(test)]
mod tests;
