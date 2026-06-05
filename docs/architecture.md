# Architecture

pqty owns the package layer between LaTeX source and a renderer:

```text
project source
    -> scan
    -> resolve against one TeX Live snapshot
    -> hydrate verified provider files
    -> pqty.lock/v1
    -> TEXMF tree + pqty.env/v1
    -> renderer or build system
    -> optional trace and convergence
```

Engine binaries, formats, options, compilation passes, bibliography and index
tools, sandboxing, system fonts, and PDF output remain outside pqty. A complete
reproducible build combines pqty's environment identity with those inputs.

## Artifact lifecycle

`pqty.lock/v1` has three explicit stages:

1. `scanned` records project discoveries and unresolved resources.
2. `resolved` adds registry provenance and a provider closure.
3. `exact` adds verified provider integrity, store identities, and runtime-file
   indexes.

Runtime convergence may extend an exact lock but does not add another schema
stage. A lock is converged only for the execution path represented by its
stable trace.

All artifacts reject unknown fields. Project and TDS paths are relative UTF-8
portable paths using `/`.

`pqty.env/v1` is the renderer-facing projection of an exact lock. Its
fingerprint identifies the serialized package environment, not project source
or engine state.

`pqty.trace/v1` records observed inputs as `package`, `project`, `engine`, or
`output`. pqty reconciles only package inputs. An optional environment
fingerprint prevents stale or crossed traces.

Detailed contracts:

- [lock format](lockfile-schema.md)
- [environment format](environment-schema.md)
- [trace format](trace-schema.md)
- [runtime convergence](convergence.md)
- [progress stream](progress-schema.md)

## Source and registry model

The scanner reads bytes through `SourceTree`, allowing filesystem projects,
editor snapshots, and virtual trees to use the same implementation. It follows
explicit LaTeX declarations and confined local `.cls` and `.sty` files. It is
not a TeX interpreter; runtime-selected inputs require convergence.

The registry implementation reads TeX Live `texlive.tlpdb`. Resolution selects
one internally consistent distribution snapshot rather than solving
independent package version ranges. It:

1. maps each requested resource to its provider;
2. rejects missing or ambiguous ownership;
3. expands package dependency edges;
4. scans selected runtime files for additional explicit loads;
5. records why each provider entered the closure.

`latest` revalidates rolling metadata when resolution is required. Dated
snapshots are immutable in the cache. Offline mode accepts cached dated data or
an existing exact lock and rejects `latest`.

## Store and installation

Provider files are stored by SHA-256. A provider manifest maps normalized TDS
paths to objects, and provider integrity covers the sorted path/content
manifest.

Published objects and manifests are immutable. Concurrent identical writers
accept the same result; conflicting content fails. Corrupt content is
quarantined before online recovery from the source recorded in the lock.

Installation verifies referenced objects, stages a normal TEXMF tree, and then
replaces the destination. It accepts new or empty destinations and trees with
a valid pqty ownership marker. Store/output overlap and unrelated non-empty
directories are rejected. The caller coordinates one writer and a quiescent
destination.

## Trust boundary

Registry records, archives, locks, and traces are untrusted. Identifiers,
paths, sizes, digests, ownership, dependency edges, redirects, reads, and
archive expansion are validated or bounded before publication. HTTPS and
hashes establish transport and content consistency, not publisher
authentication.

Lock and trace JSON inputs have a 64 MiB read ceiling.

## Runtime convergence

Static scanning creates the initial package plan. A trace records what one
renderer execution used. Convergence:

1. verifies the trace's environment fingerprint;
2. plans missing package inputs against the lock's exact registry metadata;
3. adds provider closures to a cloned lock;
4. hydrates and validates the complete candidate;
5. updates the lock only if reconciliation succeeds.

The result is `stable`, `changed`, or `unresolved`. After `changed`, the caller
installs the new tree and renders again. The caller must bound this loop.

Engine-specific log parsing stays outside the core. `pqty-fls` is the
reference adapter for kpathsea `.fls` files.

## Code map

| Location | Responsibility |
| --- | --- |
| `src/artifact/` | Artifact models, validation, environment projection, and I/O |
| `src/path.rs` | Portable project and TDS path rules |
| `src/source/` | Source trees, scanning, and local resolution |
| `src/registry/` | TeX Live parsing, ownership, and provider resolution |
| `src/store/` | HTTP/cache bounds, archives, objects, and installation |
| `src/convergence.rs` | Transactional trace-driven lock extension |
| `src/cli/` | Configuration, protocol output, and command orchestration |
| `adapters/fls/` | Kpathsea recorder conversion |
| `schemas/` | Machine-readable protocol schemas |
| `tests/golden/` | Frozen compatibility artifacts |
