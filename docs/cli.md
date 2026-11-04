# CLI and configuration reference

`registry --runtime-files --remote YYYY-MM-DD --tlpdb-sha256 sha256:...` exports
the pinned registry digest and runtime basename/exact-TDS-path ownership as
`pqty.registry/v1`. Hosts keep this index server-side to answer absent or
ambiguous requests without starting package preparation. The ordinary installed
`registry --tlpdb FILE` response is unchanged.

This page documents every command and option in the standalone `pqty` binary.
For a managed source-to-PDF workflow that includes pqty, use
[texe](https://github.com/backmatter/texe).

Run `pqty <command> --help` to check the interface of the version you have
installed. Machine integrations should negotiate Artifact Protocol support
with `capabilities` rather than parsing human-readable output.

## Global options

Global options may be written before or after the command.

| Option | Meaning |
| --- | --- |
| `--project-root <PATH>` | Root used for project-relative paths in artifacts. Without it, the entry `.tex` file's parent is the root. |
| `--input-root <PATH>` | Add a project-relative, project-confined source search directory. May be repeated. |
| `--no-config` | Ignore `pqty.toml` and use only explicit options and platform defaults. Recommended for process integrations. |
| `--offline` | Forbid network access. Cached dated registry metadata and package containers may still be used. |
| `--allow-insecure-registry` | Allow an explicitly selected `http://` registry URL. HTTPS-to-HTTP redirects remain forbidden. |
| `--progress human\|json\|off` | Select progress on stderr. The default is `human`; integrations can negotiate and use `json`. |
| `-h`, `--help` | Print help. |
| `-V`, `--version` | Print the installed version. |

Additional input roots are searched after the source file's directory and the
project root. A file found through one remains confined to the project and is
included in the source digest set.

## Selecting a package registry

Commands that resolve packages accept one of these choices:

| Option | Meaning |
| --- | --- |
| `--remote latest` | Use the rolling TeX Live registry. It is revalidated when resolution is needed. |
| `--remote YYYY-MM-DD` | Use the immutable TeX Live archive for that calendar date. This is the recommended reproducible choice. |
| `--tlpdb <PATH>` | Use a local `texlive.tlpdb`. |
| `--tlpdb-url <URL>` | Fetch a custom `texlive.tlpdb` or `texlive.tlpdb.xz` URL. This is an expert override. |

If no choice or configured default is present, pqty looks for a local TeX Live
package database. Use only one registry selection per invocation.

`--offline --remote latest` is invalid because `latest` is mutable. Offline
dated resolution requires the corresponding verified metadata and package
containers to be cached. A complete unchanged exact lock can be installed
without registry access.

Custom registry URLs must use HTTPS unless
`--allow-insecure-registry` is also present. This exception permits transport
over HTTP; it does not authenticate the registry publisher.

## Inspection commands

### `capabilities`

```text
pqty capabilities
```

Prints a machine-readable description of the schemas and CLI protocols
supported by the binary, including the JSON Lines progress protocol.

`capabilities` advertises language requirements, verified file manifests,
font-map metadata, individual file export, and standalone package preparation
in the single `pqty.capabilities/v1` contract. An explicit
`--schema pqty.capabilities/v1` selects the same response. Other schema versions
are rejected.

### `package` and `package-file`

```sh
pqty --no-config package --file foo.sty --remote 2026-07-26 \
  --tlpdb-sha256 <pinned-metadata-sha256> --store /cache/pqty -o foo.package.json
pqty --no-config --offline package-file --manifest foo.package.json \
  --store /cache/pqty --path tex/latex/foo/foo.sty -o /public/objects/<sha256>
```

`package` prepares one provider and emits `pqty.package/v1` without reading a
project or creating a TEXMF tree. `--provider NAME` selects a provider directly;
`--file` accepts an exact TDS path or an unambiguous literal basename. Unknown,
unsafe and ambiguous requests fail. Dependencies are retained as metadata;
this command does not install every dependency speculatively.

Checksummed remote providers reuse the server's prepared manifest and shared
objects across invocations. Missing objects are recovered from the pinned
container; offline misses fail. Local mutable source trees are read again.
`package-file` exports just one verified file atomically, works offline, and
prints no file bytes or protocol JSON on stdout. Servers publish that output
under its content digest with immutable cache headers. See the
[server and browser package guide](package-serving.md).

### `scan`

```text
pqty scan <ROOT>
```

Scans a root `.tex` file and prints a `pqty.lock/v1` artifact at the `scanned`
stage. It does not consult a registry or download packages.

### `explain`

```text
pqty explain <ROOT>
```

Prints a human-readable account of source declarations, locally resolved
files, and unresolved references.

### `tree`

```text
pqty tree <ROOT> [REGISTRY OPTION]
```

Resolves the scanned requirements and prints the provider dependency closure
as a human-readable tree. It does not hydrate package content.

### `why`

```text
pqty why <ROOT> <PROVIDER> [REGISTRY OPTION]
```

Prints the dependency chains that selected a TeX Live provider, for example
`pqty why main.tex graphics --remote 2026-07-26`.

### `resolve`

```text
pqty resolve <ROOT> [REGISTRY OPTION] [-o|--output <PATH>]
```

Resolves the project to a `pqty.lock/v1` artifact at the `resolved` stage,
without downloading package content.

- Without `--output`, the artifact is printed to stdout.
- With `-o` or `--output`, it is written atomically to that path.

## Exact-environment commands

### `lock`

```text
pqty lock <ROOT> [REGISTRY OPTION] [OPTIONS]
```

Resolves the full closure, fetches and verifies its package files, stores
them, and writes an exact lock.

| Option | Meaning |
| --- | --- |
| `--require-file <TDS_PATH>` | Add a runtime file requirement not visible in the source. May be repeated. |
| `--require-provider <NAME>` | Add a TeX Live provider requirement. May be repeated. |
| `--require-language <NAME>` | Add complete explicit `AddHyphen` language inputs. May be repeated. |
| `--tlpdb-sha256 <DIGEST>` | Require the SHA-256 digest of the decompressed registry metadata. The optional `sha256:` prefix is accepted. |
| `--store <PATH>` | Override the content-addressed store directory. |
| `-o`, `--output <PATH>` | Lock output path. The default is `pqty.lock`. |

`lock` can refresh source records in an unchanged exact lock without fetching
its already verified package closure again.

### `install`

```text
pqty install [OPTIONS]
```

Reproduces an exact lock as a TEXMF tree and verifies every installed file.

| Option | Meaning |
| --- | --- |
| `--lock <PATH>` | Exact lock to install. The default is `pqty.lock`. |
| `--store <PATH>` | Override the content-addressed store directory. |
| `-d`, `--out <PATH>` | Destination TEXMF tree. The default is `pqty-texmf`. |
| `--link copy\|experimental-symlink\|experimental-hardlink` | Placement mode. The supported default is `copy`. |

The destination must be new, empty, or an existing tree carrying pqty's
ownership marker. Store/output overlap is rejected. OS locks coordinate writers
to the same destination or shared store. Valid unchanged trees are audited against
the lock and retained, including when the shared cache is empty. A sibling
`.<destination>.pqty-install` directory holds the persistent lock and recoverable
staging/backup trees. Interrupted installations recover on the next invocation.
Changed trees use two directory renames, so consumers must coordinate builds or
tolerate transient missing paths during replacement.

### `env`

```text
pqty env [--lock <PATH>] [-o, --output <PATH>]
```

Prints the deterministic `pqty.env/v1` projection of an exact lock. The
default lock path is `pqty.lock`. The environment fingerprint is suitable for
renderer cache keys.

Use `--output pqty.env.json` to write the environment atomically instead of
printing it. The parent directory must exist. A failed read, validation, or
write leaves an existing output file intact. Successful file output is silent;
without `--output`, stdout remains the same versioned JSON document.

### `require`

```text
pqty require [--lock <PATH>] (--file <TDS_PATH>|--provider <NAME>|--language <NAME>)... [OPTIONS]
```

Adds explicit runtime files or providers to an exact lock, hydrates the
expanded closure, and writes the updated lock.

| Option | Meaning |
| --- | --- |
| `--lock <PATH>` | Exact lock to extend. The default is `pqty.lock`. |
| `--file <TDS_PATH>` | Runtime filename or normalized TDS path. May be repeated. |
| `--provider <NAME>` | TeX Live provider. May be repeated. |
| `--language <NAME>` | Canonical `AddHyphen` name or synonym. May be repeated. |
| `--tlpdb <PATH>` | Exact local registry metadata corresponding to the lock. |
| `--tlpdb-url <URL>` | Exact registry URL corresponding to the lock. |
| `--store <PATH>` | Override the content-addressed store directory. |
| `-o`, `--output <PATH>` | Write to another path; otherwise update `--lock`. |

At least one `--file`, `--provider`, or `--language` is required. The selected registry must
match the snapshot and metadata digest recorded by the lock.

### `languages` and `files`

```sh
pqty languages --lock pqty.lock --language polish --output languages.json
pqty --offline files --lock pqty.lock --store .pqty/store --output files.json
```

`languages` resolves named declarations and exact loader/pattern/exception
paths. Without a lock, use `--tlpdb` or `--tlpdb-url` to query a registry.
`files` exports a complete hash and size index after verifying the exact store
contents. Both print JSON to stdout unless `--output` is supplied; failed
exports preserve existing output files. See the
[language and lazy-input contracts](lazy-inputs.md) for all fields, validation,
offline behavior, and the boundary between packages and engine activation.

### `file`

```sh
pqty --offline file --lock pqty.lock --store .pqty/store \
  --path tex/latex/base/article.cls --output article.cls
```

Copies only the requested exact normalized TDS path from the verified store.
`--lock` defaults to `pqty.lock`; `--store` uses the ordinary configured cache
when omitted. `--path` and `--output` are required. Output is raw file bytes,
atomically replaced after verification; stdout is empty. Basename guessing,
network recovery, registry lookup, and TEXMF installation are not performed.
Outputs inside the store are rejected. A failed export preserves existing output.

### `bundle` and `import`

```sh
pqty bundle --lock pqty.lock --output packages.tar.gz
pqty import --bundle packages.tar.gz --lock pqty.lock
```

`bundle` writes an exact lock and its verified store objects to one gzip tar
archive, replaced atomically. `import` validates the whole archive before it
publishes objects and package manifests into the store (`--store` or the
configured cache), then **replaces the `--lock` file** with the lock carried
by the bundle. An existing lock at that path is not compared or merged.

A bundle is not signed. `import` only checks it against itself: every object
must match its listed digest and every package manifest must match the bundle's
own lock. This catches damage and malformed archives, not a substituted bundle.
Import bundles only from sources you trust, or compare the imported lock with
one you already trust.

### `font-maps`

```sh
pqty --offline font-maps --lock pqty.lock --output font-maps.json
```

Exports `pqty.font-maps/v1` declarations for the exact lock's closure. The
command reloads the lock's pinned registry and verifies its metadata identity;
`--tlpdb` or `--tlpdb-url` may supply that same registry explicitly. Offline
remote use needs the matching cached snapshot. `--lock` defaults to `pqty.lock`;
without `--output`, artifact JSON goes to stdout.

Map, MixedMap, and KanjiMap types remain distinct. Concrete names resolve to
locked TDS paths; templates retain parameter names and a null path. pqty does
not choose a CJK profile, expand templates, generate aggregate maps, or run
registry actions. See the [metadata contract](lazy-inputs.md#typed-font-map-metadata).

## Runtime-observation commands

### `check-trace`

```text
pqty check-trace [--lock <PATH>] --trace <PATH>
```

Reconciles a `pqty.trace/v1` artifact with an exact lock and prints a
`pqty.trace-report/v1` artifact.

- `--lock <PATH>` defaults to `pqty.lock`.
- `--trace <PATH>` is required.

The report is printed even when package-scope inputs are missing; that result
then exits unsuccessfully.

### `converge`

```text
pqty converge [--lock <PATH>] --trace <PATH> [OPTIONS]
```

Finds providers for package inputs observed at runtime, hydrates them, and
prints a `pqty.convergence-report/v1` artifact with status `stable`, `changed`,
or `unresolved`.

| Option | Meaning |
| --- | --- |
| `--lock <PATH>` | Exact lock to reconcile. The default is `pqty.lock`. |
| `--trace <PATH>` | Runtime trace to reconcile. Required. |
| `--tlpdb <PATH>` | Exact local registry metadata corresponding to the lock. |
| `--tlpdb-url <URL>` | Exact registry URL corresponding to the lock. |
| `--store <PATH>` | Override the content-addressed store directory. |
| `-o`, `--output <PATH>` | Write a changed lock to another path; otherwise update `--lock`. |

If the trace is already stable, `--output` writes an unchanged copy when
requested. An `unresolved` report is printed and then exits unsuccessfully.
Convergence always uses the registry recorded in the lock rather than an
ambient rolling registry.

A renderer should bound its render-and-converge loop:

```text
render -> trace -> converge -> install -> rerender
```

Stop only at `stable`; stop with an error at `unresolved`.

## Configuration file

Unless `--no-config` is present, pqty reads `pqty.toml` from the process's
current working directory. It does not search parent directories. Explicit
CLI options take precedence.

```toml
[registry]
remote = "2026-07-26"
# Instead of remote:
# url = "https://example.test/tlnet/tlpkg/texlive.tlpdb.xz"

[store]
path = "/srv/pqty/store"
```

Supported keys are:

- `registry.remote`: `latest` or a valid `YYYY-MM-DD` date;
- `registry.url`: a custom registry URL;
- `store.path`: the content-addressed store directory.

Set either `registry.remote` or `registry.url`; if both are present,
`registry.url` takes precedence. Unknown keys, malformed TOML, and invalid
values are errors. Without `store.path` or `--store`, pqty uses the platform
user cache: `$XDG_CACHE_HOME/pqty/store` when set,
`%LOCALAPPDATA%\pqty\store` on Windows, or `$HOME/.cache/pqty/store` on Unix.
It falls back to `.pqty-cache/store` only when no user cache location can be
determined.

## What the scanner recognizes

Project-owned files take precedence for literal `\input` requests. Remaining
literal names can resolve to unique registry files; ambiguous names fail during
locking. `\include` remains project-owned, and computed names require runtime
observations. Locks with non-project literal inputs revisit the selected registry
when resolving again, while exact-lock installation remains offline-capable.

Text bytes in comments and document bodies need not be UTF-8. Source digests
cover the original bytes, and dependency arguments and paths must remain UTF-8.

The project scanner follows local source files and records a digest for each
one. It recognizes:

- classes from `\documentclass` and `\LoadClass`;
- packages from `\usepackage` and `\RequirePackage`;
- source inputs from `\input`, `\include`, `\includeonly`, and
  `\InputIfFileExists`;
- bibliography resources and styles from `\addbibresource`,
  `\bibliography`, and `\bibliographystyle`;
- graphics from `\includegraphics`.

Local `.cls` and `.sty` files are followed and scanned as project source.
Graphics are resolved with `.pdf`, `.png`, `.jpg`, `.jpeg`, and `.mps`
extensions. Bibliography resources and styles are resolved as `.bib` and
`.bst`.

The parser skips TeX comments, `\verb`, and common verbatim environments. It
continues through unknown commands so that a recognized command nested inside
one can still be found.

During lock hydration, pqty also scans package-owned `.tex`, `.sty`, `.cls`,
`.def`, `.cfg`, `.clo`, `.ltx`, and `.mkii` files. This pass follows package
and class loads plus `\IfFileExists` and `\InputIfFileExists` requests to add
statically discoverable runtime providers.

The scanner is conservative, not a TeX interpreter. It cannot reliably infer
filenames assembled by macros, engine decisions, or data-dependent execution.
Use explicit `require` options or an engine trace and `converge` for those
dependencies.

## Output and exit behavior

Artifact-producing commands write JSON to stdout. Human inspection commands
also use stdout. Progress and diagnostics use stderr, so JSON consumers can
keep the streams separate.

The conventional exit statuses are:

- `0`: success;
- `1`: an operational, validation, reconciliation, or convergence failure;
- `2`: invalid CLI syntax or option parsing.

Lock and trace JSON inputs are limited to 64 MiB each.

The separate
[`pqty-fls` adapter](https://github.com/backmatter/pqty/blob/main/adapters/fls/README.md)
converts recorder output from kpathsea-based engines into `pqty.trace/v1`; its
guide documents the adapter CLI, root classification, and portable paths.

### Complete immutable package mirror

`pqty mirror --remote YYYY-MM-DD --tlpdb-sha256 sha256:HEX --store STORE --output ROOT`
prepares and publishes all runtime providers serially. Registry and offline
selection follow `package`; local `--tlpdb` or explicit `--tlpdb-url` are also
supported. See [publication layout and completion semantics](package-serving.md#complete-publication-before-serving).
Capabilities v1 advertises this command as `package_mirror: true`.

Package-container downloads make at most three attempts per checked source for
transient connection/body-read failures, timeouts, and HTTP 408, 429, 500, 502,
503 or 504. Retries wait 250 ms then one second and start with fresh staging.
Progress attempt numbers count actual attempts, including source fallbacks.
Checksum/size mismatches, TLS/URL/redirect policy failures, permanent HTTP errors
and local filesystem failures stop immediately. Offline mode never retries a
network fetch; existing verified cache content is preserved on failure.
