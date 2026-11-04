# Language requirements and lazy package inputs

pqty resolves package data. The caller still chooses the engine, builds its
formats, activates language patterns, and checks the rendered result. These
contracts let a caller prepare complete language inputs and publish immutable
files without depending on pqty's private store layout.

## Negotiation and compatibility

```sh
pqty --no-config capabilities
```

The single `pqty.capabilities/v1` contract advertises `languages_schema`,
`file_manifest_schema`, `font_maps_schema`, `package_schema`,
`language_requirements` and `file_export`, alongside the existing protocol
fields. Explicit `--schema pqty.capabilities/v1` is equivalent. Intermediate
capabilities v2/v3 selectors are removed in 0.1.4. Existing lock, environment, trace and bundle formats remain v1; their
frozen fixture bytes are unchanged.

The Rust API adds `IndexPackage.hyphenation` and
`IndexPackage.font_map_declarations`; callers constructing that public struct
directly must initialize both fields.

## Explicit language requirements

```sh
pqty --no-config lock main.tex --remote 2026-07-26 --require-language polish
pqty --no-config require --lock pqty.lock --language french
pqty --no-config languages --lock pqty.lock --language polish -o languages.json
```

Names and synonyms are case-sensitive TeX Live `execute AddHyphen` identifiers.
pqty retains canonical names, synonyms, loaders, optional pattern and exception
files, left and right minima, and additional registry attributes. It parses
quoted attribute values and both whitespace and literal `\t` separators.
Attributes such as `luaspecial` remain data: the consuming engine decides whether
the declaration is suitable and never receives an instruction to execute a
registry action from pqty.

Every declared file must resolve to exactly one normalized TDS path and owner.
Missing names, files, duplicate declarations, ambiguous aliases, and ambiguous
basenames fail. Patterns and exceptions may belong to a different provider;
pqty includes those owners and the declaring provider's package dependencies.
Pattern and exception fields may contain comma-separated file lists (as used
for Serbian); every member is required and its declaration order is retained.
An empty optional field means no declared input, rather than a missing file.
Unsafe paths, duplicate fields, malformed quotes, and invalid minima fail while
loading the registry. Minima are retained as unsigned bytes; engine-specific
limits remain the caller's responsibility.

Requirements are lowered to existing `consumer_requirements.files` and
`consumer_requirements.providers` records in `pqty.lock/v1`. Exact TDS paths
preserve the selected inputs; aliases produce identical requirements. Hydration
uses the existing package checksum and file verification path. `require` and
`languages --lock` reject registry metadata that differs from the lock's pinned
snapshot. Failure leaves existing artifact files intact.

The lock persists the exact files and providers, rather than language labels or
minima. A named request therefore needs the pinned registry metadata again, even
when an exact lock exists. Installing that exact lock and exporting its file
manifest remain offline operations with no registry requirement. If the caller
needs language metadata offline, retain the snapshot-bound language manifest too.

`languages` can also query a registry before a lock exists:

```sh
pqty --no-config languages --tlpdb /texlive/tlpkg/texlive.tlpdb --language polish
```

Without `--lock`, it accepts `--tlpdb` or `--tlpdb-url` and the ordinary registry
selection defaults. A local installed registry is restricted to files present
on disk, as with other CLI resolution operations.

### `pqty.languages/v1`

The [closed JSON Schema](../schemas/pqty.languages.schema.json) is normative for
fields. `registry` contains the exact metadata digest; `languages` is sorted by
canonical name and provider and contains one `ResolvedLanguage` per declaration.
Each entry has `provider` and `declaration`. The declaration contains `name`,
sorted `synonyms`, `loader`, arrays `patterns` and `exceptions` (possibly empty),
`left_hyphen_min`, `right_hyphen_min`, and sorted `attributes` (`name`, `value`).
All resolved file references are exact TDS paths. `LanguageManifest::validate`
checks the complete artifact against its registry index.

This metadata is not proof that an engine has activated the requested language.
Source scanning does not infer arbitrary Babel options or Polyglossia settings.
A host supplies explicit language policy or interprets engine observations;
silent fallback to another language needs an engine-level behavioral check.

## Verified files for lazy consumers

```sh
pqty --no-config --offline files --lock pqty.lock --store .pqty/store -o files.json
```

`files` reads an exact lock and its verified store. It checks provider manifests,
the complete locked file set, file ownership, and every content digest before
publishing output. Missing, corrupt, or unsupported content fails without
fetching or repairing anything. `--output` is atomic and stdout contains only
artifact JSON. No second TEXMF tree is created. Shared objects are hashed once
per export using standard I/O streaming. Existing file and closure byte limits
apply (128 MiB per file and 8 GiB of expanded closure bytes).

### `pqty.files/v1`

The [closed JSON Schema](../schemas/pqty.files.schema.json) defines:

- `schema`: `pqty.files/v1`;
- `environment_fingerprint`: the existing `pqty.env/v1` fingerprint;
- `files`: the complete index sorted by normalized `tds_path`, with `owner`,
  engine-neutral `kind`, lowercase `sha256:<hex>` content `digest`, and byte `size`;
- `fingerprint`: lowercase `sha256:<hex>` of compact JSON serialization of
  `[schema, environment_fingerprint, files]`, using the field order above for
  each file (`tds_path`, `owner`, `kind`, `digest`, `size`).

`FileManifest::validate(&lock)` checks schema, fingerprint, exact file set and
order, ownership, kinds, size bounds, and provider digests against the exact lock.
It verifies no file bytes itself. File sizes are measured during export, not
independently authenticated by the old lock; consumers must check both digest
and size when accepting transported bytes. A fingerprint is a cache identity,
not publisher authentication.

The manifest contains neither store paths nor URLs. A host can obtain bytes
through the existing public TEXMF installation or portable bundle interfaces,
publish them using its own transport, and key its cache by content digest.
The public `copy_verified_file` and `export_file` APIs below also provide direct
access to a selected file, without store paths, a full bundle, or a TEXMF tree.
The host owns URLs, authorization, HTTP caching, browser storage and eviction,
and worker delivery. Compilation can reuse cached package bytes locally;
this export does not add a compilation RPC or project synchronization step.

## Rust APIs

```rust,no_run
use pqty::{TlpdbIndex, require_languages, export_file_manifest};

# fn example(mut lock: pqty::LockFile, store: &std::path::Path) -> Result<(), pqty::PqtyError> {
let index = TlpdbIndex::load(std::path::Path::new("/texlive/tlpkg/texlive.tlpdb"))?;
let languages = require_languages(&mut lock, &index, &["polish".into()])?;
languages.validate(&index)?;
// Hydrate the extended lock with hydrate_lock before exporting file bytes.
# let _ = (lock, store);
# Ok(())
# }
# fn files(lock: &pqty::LockFile, store: &std::path::Path) -> Result<(), pqty::PqtyError> {
let files = export_file_manifest(&lock, store)?;
files.validate(&lock)?;
# Ok(())
# }
```

`TlpdbIndex::resolve_languages` queries without mutating a lock. Empty requests
produce an empty manifest. `require_languages` is transactional on resolution
failure and returns the language manifest; its successful result is resolved
and needs normal hydration before exact installation. None of these APIs runs
an engine or guesses that a project's dependency set is complete on unobserved
execution paths.

## Single-file bytes and bounded ingestion

```sh
pqty --no-config --offline file --lock pqty.lock --store .pqty/store \
  --path tex/latex/base/article.cls --output article.cls
```

`export_file(&lock, store, tds_path, output)` atomically publishes the requested
file and returns its `ManifestFile` metadata. It validates the exact lock, the
selected provider manifest, and the selected object's digest before export.
Existing output survives errors; paths inside the store are rejected. Other
package files and upstream registry metadata need not be available. No recovery
or network requests occur.

`copy_verified_file(&lock, store, tds_path, &mut writer)` supports a host-owned
stream or staging destination. It verifies the complete file before writing and
checks its digest and size again during copying. The caller must discard partial
output on any error and commit it only after success. As with other store APIs,
objects must remain immutable while being consumed. The standard library manages
transfer buffering; neither users nor API callers choose chunk sizes. File and
provider metadata still scale with the locked index.

Initial hydration and exact-lock recovery process each selected file through a
standard I/O hash/copy stream. Remote containers download to a temporary file
and are SHA-512/size verified before caching and decompression. Bounded decompression
stages a tar file on disk, then processes selected entries individually; the
selected provider payload is not collected in memory. Existing file, archive,
expansion, and closure limits remain enforced. Decoder working memory and the
registry/source scanner have separate memory costs. This is an implementation
bound on byte copying, not a measured process RSS or speed claim.

Incomplete remote archives or failed provider integrity checks never publish a
package manifest or become exact. Valid content-addressed objects written before
a later provider failure can remain in the store. A later closure failure can
also leave complete verified providers, as can an interrupted installation.
Portable bundle imports also publish individual staged objects with bounded
copying; bundle and v1 provider identities are unchanged.

## Typed font-map metadata

```sh
pqty --no-config capabilities
pqty --no-config --offline font-maps --lock pqty.lock -o font-maps.json
```

Capabilities v1 includes `font_maps_schema` (`pqty.font-maps/v1`) and
`file_export` (`true`). The existing v1 `font_maps` list retains its original
concrete Map/MixedMap filename semantics.

[`pqty.font-maps/v1`](../schemas/pqty.font-maps.schema.json) contains `schema`,
`registry`, `environment_fingerprint`, and `maps`. It projects every declaration
from the exact lock's provider closure, sorted by provider, registration kind
(Map, MixedMap, KanjiMap), and name, with duplicate declarations removed.
Each entry contains:

- `provider`: the declaring registry provider;
- `declaration.kind`: `map`, `mixed-map`, or `kanji-map`;
- `declaration.name`: the original registry basename or template;
- `declaration.parameters`: sorted unique names inside paired `@` markers;
- `tds_path`: the concrete locked file path, or null for a profile template.

Concrete declarations must resolve to one registry path and owner and be present
in the lock; cross-provider ownership is allowed. Missing or ambiguous files
fail explicitly. Templates such as `ptex-@jaEmbed@@jaVariant@.map` preserve both
parameters without pretending the template is an available file. The host
selects a profile/backend and requests its concrete files through existing
runtime requirements. pqty neither expands templates nor executes registry
commands. Availability of a KanjiMap does not imply support in an engine.

`TlpdbIndex::font_map_manifest(&lock)` creates this projection and
`FontMapManifest::validate(&lock, &index)` verifies its complete metadata and
ordering against the original snapshot. It does not read file contents. Retain
the manifest when the consuming host needs metadata without the registry.

### Literal package graphics and short verbatim

Source discovery honors literal `\MakeShortVerb` / `\DeleteShortVerb`
definitions, including the starred form, so inline documentation examples are
not package dependencies. Delimiters are tracked within each scanned source;
the scanner does not expand macros or execute TeX.

Project-local graphics take precedence. For unresolved literal graphics, registry
resolution considers `pdf`, `png`, `jpg`, `jpeg`, then `mps` when no extension is
specified. Multiple formats from one provider are alternatives; distinct providers
or multiple paths for the preferred format remain ambiguous. Missing project
images and dynamic names stay unresolved. Registry-owned graphics are recorded
as runtime requests, not as project-owned source paths.
