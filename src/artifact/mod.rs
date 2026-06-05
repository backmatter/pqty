mod environment;
mod io;
mod model;
pub(crate) mod validation;

pub use io::{read_lock, read_trace, write_lock};
pub use model::{
    BibliographyKind, BibliographyRecord, ClassRecord, ConsumerRequirements, ConvergenceReport,
    ConvergenceStatus, ConvergenceUnresolvedReason, Environment, EnvironmentFile,
    EnvironmentPackage, EnvironmentRequirements, GraphicRecord, InputKind, InputRecord, InputTrace,
    LinkMode, Location, LockFile, LockStage, LockedFile, ObservedInput, PackageRecord,
    PackageSource, PackageSourceKind, Registry, RegistryKind, ResolvedEnvironment, ResolvedPackage,
    ResourceKind, SourceRecord, TraceMatch, TraceReport, TraceScope, UnresolvedRecord,
    UnresolvedTraceInput,
};
pub use validation::{validate_lock, validate_materialized_lock};

pub(crate) use environment::{font_filename_is_case_folded, normalize_trace_path, resource_kind};
pub(crate) use io::{atomic_write, unique_sibling, usable_parent};
pub(crate) use validation::{
    closure_by_provider, fail_on_unresolved_packages, invalid_artifact_text,
    validate_provider_identifier, validate_tds_path,
};
