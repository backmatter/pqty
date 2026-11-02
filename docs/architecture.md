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

TeX Live categories do not distinguish all package data from engine support.
Providers in `Package`, `TLCore`, and `ConTeXt` contribute declared TEXMF data
and its dependencies. Binary-only, collection, and installation providers stay
outside that closure. Ownership lookup excludes distribution-root installer
helpers, including helpers inside otherwise valid data packages.

Literal `\input` requests use project-owned files first. A unique registry file
can supply a remaining literal request; ambiguous requests stay unresolved.
Missing project includes and computed names are not guessed. Package hydration
also follows bare literal inputs between package files. Named input changes
participate in closure reuse decisions; non-project literal inputs require
registry resolution again. Existing exact locks remain installable as captured.

Within one hydration operation, verified provider indexes are extended only for
new providers. Normalized, deduplicated runtime requests are cached by file
content digest, while their resolution is repeated against the growing package
set. The cache retains neither source text nor parser structures and is discarded
at the end of the operation; subsequent operations consult their current inputs.

Source hashing uses original bytes. Scanning tolerates legacy text in comments
and document bodies while requiring valid UTF-8 dependency arguments and paths;
it does not reinterpret or rewrite source encoding for the engine.

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

Remote downloads share a lazily initialized HTTP agent, allowing the library to
reuse connections across metadata and package fetches. Pool behavior uses the
library defaults. HTTPS policy, redirect validation, conditional request headers,
and timeouts remain enforced for each request.

Package ingestion copies and hashes one file at a time with bounded buffers,
including exact-lock recovery and portable bundle object publication. Remote
compressed containers are verified on disk before bounded decompression and
per-entry processing. A provider manifest is published only after its complete
required file set and integrity checks succeed; failed providers may leave valid
unreferenced immutable objects.

Temporary files are owned by `tempfile` in the destination filesystem. This
handles naming, collisions, and cleanup on ordinary errors or stack unwinding.
Artifact/cache replacement remains explicit and atomic; content-addressed store
publication still refuses to overwrite a competing writer. Temporary links are
removed before making published objects read-only, including on Windows. Store
permissions continue to follow ordinary file creation and the process umask.
Bundle import staging directories have the same scoped cleanup. Persistent
transaction recovery directories and quarantined evidence retain their lifetimes.

Installation first audits a matching destination against the exact lock. It
hashes installed files with a bounded buffer and reconstructs the pinned provider
manifests, so valid copied trees survive a cleared shared cache. Requested link
mode changes, missing or altered files, and extra package files require replacement.
The generated `ls-R` database may remain alongside package files.

OS locks serialize shared-store hydration/recovery and each destination's writers.
Waiters sleep in the kernel; process exit releases the lock. The lock files remain
in place so waiting processes always use the same inode. Installation stages a
normal TEXMF tree in a sibling `.<destination>.pqty-install` directory, then moves
the old destination to its backup and publishes the staged tree. The next writer
restores the backup if publication was interrupted, or finishes backup cleanup if
the new tree is already active. Incomplete staging trees are discarded under the
same lock. A valid ownership record protects the transaction directory.

New or empty destinations and trees with a valid pqty ownership marker are
accepted. Store/output overlap and unrelated non-empty directories are rejected.
Publication uses two renames and does not promise a whole-tree atomic snapshot
to arbitrary readers. Consumers coordinate builds or tolerate transient missing
paths. Cross-platform power-loss durability is not guaranteed.

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

Explicit language requirements lower registry `AddHyphen` declarations to
existing exact file/provider requirements. The separate `pqty.languages/v1`
artifact retains snapshot-bound language metadata; `pqty.files/v1` projects
verified store bytes to a public hash/size index for lazy consumers. Neither
changes existing v1 lock or environment formats. See the
[language and lazy-input guide](lazy-inputs.md) for the engine and transport
boundaries and the single v1 capabilities contract.

Standalone package preparation for immutable hosting and the browser disk-cache
adapter are described in [package serving](package-serving.md). Server-side
resolution is shared across projects; compilation and source synchronization
remain outside pqty. Clients receive file metadata for selected providers and
fetch individual files, rather than the distribution registry or containers.

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

Typed font-map declarations retain registry Map/MixedMap/KanjiMap classes and
unresolved profile templates in a separate snapshot-bound artifact. Existing
v1 map lists and environment fingerprints retain their semantics. Hosts own
profile selection, aggregate generation, and backend behavior.
