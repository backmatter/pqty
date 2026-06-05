# Rust API overview

The `pqty` crate exposes the same scanning, resolution, storage, environment,
and trace-convergence building blocks used by the CLI. Use it when pqty is
part of a Rust process and the host needs direct control over source or
registry abstractions.

Artifact Protocol v1 is the stable integration boundary. The Rust API follows
the crate's SemVer compatibility: while the major version is zero, minor
releases may contain breaking API changes. Non-Rust tools, and integrations
that need a process boundary, should invoke the CLI and exchange the documented
JSON artifacts.

The generated item-by-item API reference is available on
[docs.rs](https://docs.rs/pqty). The sections below are a map of the public
surface rather than a replacement for those signatures.

## Scan a project

Add `pqty` to the Rust application's dependencies, then scan a filesystem
project:

```rust
use pqty::{scan_project_at, validate_lock};

fn main() -> Result<(), pqty::PqtyError> {
    let scanned = scan_project_at(".", "paper/main.tex")?;
    validate_lock(&scanned)?;

    println!("found {} package declarations", scanned.packages.len());
    Ok(())
}
```

`scan_project` uses the entry file's parent as the project root.
`scan_project_at` preserves paths relative to an explicit root, and
`scan_project_at_with_roots` adds confined project-owned search directories.

For an editor buffer, archive, or other non-filesystem source:

- implement `SourceTree`, which reads bytes by a confined `VirtualPath`; or
- populate `MemorySourceTree` and call `scan_source`.

## API map

### Paths and source discovery

- `VirtualPath` validates normalized project-relative UTF-8 paths.
- `SourceTree` is the minimal source-snapshot abstraction.
- `FileSystemSourceTree` and `MemorySourceTree` are the supplied
  implementations.
- `scan_project`, `scan_project_at`, `scan_project_at_with_roots`, and
  `scan_source` produce a `LockFile` at the `Scanned` stage.

Scanner coverage and its intentional limits are documented in the
[CLI reference](cli.md#what-the-scanner-recognizes).

### Registry and resolution

- `PackageRegistry` abstracts package ownership and dependency metadata.
- `TlpdbIndex` implements it for TeX Live `texlive.tlpdb` metadata.
- `TlpdbIndex::load` reads local metadata.
- `TlpdbIndex::load_url` fetches and caches supported HTTPS tlnet metadata.
- `resolve` changes a scanned lock to the `Resolved` stage.
- `require_runtime` adds explicit runtime files or providers without encoding
  why a host tool needs them.

The CLI contains additional policy for dated snapshots, strict offline use,
configuration merging, and checksum pins. Prefer the CLI when the host wants
those policies as one supported operation.

### Package storage and TEXMF trees

- `PackageByteSource` selects an installed TeX Live tree or verified tlnet
  containers as the byte source.
- `hydrate_lock` fetches or copies package files into the content-addressed
  store and changes a resolved lock to the `Exact` stage.
- `materialize_from_store` publishes an already hydrated lock as a
  transactional TEXMF tree.
- `install_locked` verifies the store, recovers missing locked objects when
  possible, and publishes the TEXMF tree.
- `MaterializeReport` reports provider, file, byte, store, and output counts.
- `LinkMode::Copy` is the supported mode; the symlink and hardlink variants are
  experimental.

### Artifacts and validation

- `LockFile`, `ResolvedEnvironment`, `InputTrace`, `TraceReport`, and
  `ConvergenceReport` are the principal protocol models.
- `read_lock`, `write_lock`, and `read_trace` perform bounded artifact I/O.
- `validate_lock` accepts any valid lock stage.
- `validate_materialized_lock` requires a complete exact lock.
- `ResolvedEnvironment::from_lock` creates `pqty.env/v1` and its deterministic
  environment fingerprint.
- `ResolvedEnvironment::reconcile_trace` compares an input trace with an exact
  environment.

The normative fields and invariants are described by the
[machine-readable schemas](../schemas/) and their linked format guides:

- [lock](lockfile-schema.md)
- [environment](environment-schema.md)
- [trace and trace report](trace-schema.md)
- [convergence report](convergence.md)

### Runtime convergence

`converge_trace` transactionally attempts to extend an exact lock with
providers for missing package-scope inputs. It returns `Stable`, `Changed`, or
`Unresolved`. On `Changed`, publish the updated TEXMF tree and render again;
the host must bound this loop.

## `pqty-fls` library

The separate `pqty-fls` crate provides a small Rust API for converting
kpathsea `.fls` recorder data:

- `RootMapping` assigns a native path root to project, package, engine, or
  output scope;
- `adapt_fls` returns a deterministic `InputTrace`;
- its trace model serializes directly to `pqty.trace/v1`.

See the
[adapter guide](https://github.com/backmatter/pqty/blob/main/adapters/fls/README.md)
and
[generated API reference](https://docs.rs/pqty-fls).
