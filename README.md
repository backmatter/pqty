# pqty

`pqty` discovers the TeX Live packages used by a LaTeX project, locks their
exact files, and installs those files into a package tree that a TeX renderer
can use reproducibly.

Use [texe](https://github.com/backmatter/texe) to build a paper and produce a
PDF. Its release bundles pqty and pqty-fls alongside the managed TeX engine.
Use pqty directly to integrate package management into a renderer, editor, or
CI service.

## Install standalone pqty

For standalone use, download the archive for your platform from
[GitHub Releases](https://github.com/backmatter/pqty/releases):

1. Download the `.tar.gz` or `.zip` archive and its adjacent `.sha256` file.
2. Verify the archive against that SHA-256 file.
3. Extract it and place its directory, or the executables, on your `PATH`.
4. Run `pqty --version`.

The archive includes `pqty` and `pqty-fls`, the adapter for TeX `.fls`
recorder files.

On Linux, run this in the directory containing the downloaded files:

```sh
sha256sum -c pqty-*.tar.gz.sha256
```

On macOS:

```sh
shasum -a 256 -c pqty-*.tar.gz.sha256
```

On Windows, compare the output of:

```powershell
$archive = Get-ChildItem .\pqty-*.zip | Select-Object -First 1
Get-FileHash $archive.FullName -Algorithm SHA256
Get-Content "$($archive.FullName).sha256"
```

To build from source instead, install Git and Rust, then run:

```sh
git clone https://github.com/backmatter/pqty.git
cd pqty
cargo install --path .
```

Install the optional recorder adapter with:

```sh
cargo install --path adapters/fls
```

The repository pins its minimum supported toolchain in
`rust-toolchain.toml`.

## Quick start

Save this as `main.tex`:

```tex
\documentclass{article}
\usepackage{amsmath}
\begin{document}
\[
  E = mc^2
\]
\end{document}
```

Create an exact lock using an immutable TeX Live snapshot:

```sh
pqty lock main.tex --remote 2026-07-26 -o pqty.lock
```

Install the locked packages and emit the environment description:

```sh
pqty install --lock pqty.lock -d .pqty/texmf
pqty env --lock pqty.lock --output pqty.env.json
```

Commit `pqty.lock`, which records the snapshot, packages, files, and integrity
data. The generated `.pqty/texmf` tree contains the installed package files;
`pqty.env.json` describes the environment and its cache fingerprint. Configure
your renderer to search this tree before fallback package roots.

Inspect what pqty discovered without changing the project:

```sh
pqty scan main.tex
pqty explain main.tex
pqty tree main.tex --remote 2026-07-26
pqty why main.tex amsmath --remote 2026-07-26
```

When the entry file is nested below the project root, declare the root:

```sh
pqty --project-root /work/project lock paper/main.tex --remote 2026-07-26 -o pqty.lock
```

Repeatable `--input-root` values add confined, project-owned search
directories. Files resolved through them enter the source digest set.

See the [complete CLI reference](docs/cli.md) or run
`pqty <command> --help`.

## Snapshots and offline use

Remote resolution accepts:

- `--remote latest` for the rolling TeX Live registry;
- `--remote YYYY-MM-DD` for an immutable dated snapshot;
- `--tlpdb-url <URL>` for an expert custom source.

Use a dated snapshot for reproducible environments. The lock records the
registry origin, metadata digest, container integrity, provider manifests, and
runtime-file index.

`--offline` is strict. It accepts cached dated metadata and content or an
already complete exact lock. `--remote latest --offline` is rejected. A local
TeX Live database remains available through `--tlpdb <path>`.

Project defaults may be recorded in `pqty.toml`; see the
[configuration reference](docs/cli.md#configuration-file). Process integrations
should pass `--no-config` and explicit project, registry, and store choices so
ambient configuration cannot alter a build.

## Renderer integration

pqty manages packages; the caller owns the engine, build passes, and PDF output.
A renderer or build system:

1. negotiates schemas with `pqty --no-config capabilities`;
2. creates or updates an exact lock;
3. installs its TEXMF tree;
4. reads `pqty.env/v1` and includes its fingerprint in cache keys;
5. mounts the tree ahead of package fallback roots.

Static scanning cannot see every filename assembled by macros or selected at
runtime. An integration can emit `pqty.trace/v1` and use
[`check-trace` or `converge`](docs/cli.md#runtime-observation-commands) to
reconcile or extend the lock. Convergence returns `stable`, `changed`, or
`unresolved`; the caller must bound the render-and-converge loop.

Integrations can request the negotiated
[JSON Lines progress stream](docs/progress-schema.md) on stderr without
changing artifact JSON on stdout or the environment fingerprint.
[`pqty-fls`](https://github.com/backmatter/pqty/blob/main/adapters/fls/README.md)
converts `.fls` recorder output from kpathsea-based engines into the common
trace format.

## Safety and limits

Registry data, archives, locks, and traces are treated as untrusted input.
pqty validates paths, identifiers, ownership, digests, sizes, redirects, and
archive expansion before publishing content. HTTPS and hashes provide
transport and content consistency, not publisher authentication.

Store objects are immutable and corrupt content is quarantined. TEXMF
replacement accepts a new or empty destination or a tree carrying pqty's
ownership marker. The caller must coordinate one writer and a quiescent
destination. Copy is the supported installation mode.

Current limits:

- scanning is conservative rather than a TeX interpreter;
- TeX Live `tlpdb` is the only built-in registry backend;
- environment fingerprints cover packages, not engine binaries, formats,
  external tools, or system fonts;
- lock and trace JSON inputs are limited to 64 MiB;
- symlink and hardlink installations are experimental.

## Reference

- [Architecture](docs/architecture.md)
- [Complete CLI and configuration reference](docs/cli.md)
- [Rust API overview](docs/rust-api.md)
- [Lock format](docs/lockfile-schema.md)
- [Environment format](docs/environment-schema.md)
- [Trace format](docs/trace-schema.md)
- [Runtime convergence](docs/convergence.md)
- [Progress stream](docs/progress-schema.md)
- [Machine-readable schemas](schemas/)

Artifact Protocol v1 covers `pqty.lock/v1`, `pqty.env/v1`, `pqty.trace/v1`,
`pqty.trace-report/v1`, `pqty.convergence-report/v1`, and
`pqty.progress/v1`. A binary that advertises these v1 schemas retains read
compatibility for their valid artifacts; integrations should still negotiate
capabilities.

## Help and contributing

For a bug or unclear behavior, open a
[GitHub issue](https://github.com/backmatter/pqty/issues) with the pqty
version, platform, command, complete error, and smallest source file that
reproduces it. See [CONTRIBUTING.md](CONTRIBUTING.md) before sending a change.

Report vulnerabilities privately as described in
[SECURITY.md](SECURITY.md).

## License

pqty and pqty-fls are available under the [MIT License](LICENSE).
