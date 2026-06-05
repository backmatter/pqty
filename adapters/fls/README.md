# pqty-fls

`pqty-fls` converts the `.fls` file produced by kpathsea-based TeX engines
running with `-recorder` into `pqty.trace/v1`.

Most LaTeX authors do not run this adapter directly.
[texe](https://github.com/backmatter/texe) manages it as part of its
source-to-PDF workflow. Direct use is for renderer and build-system
integrations that need to classify their own recorder output.

The adapter does not guess which files belong to the project, package layer,
engine bundle, or build output. Callers declare those roots explicitly:

```sh
pqty-fls \
  --fls build/main.fls \
  --environment pqty.env.json \
  --project-root . \
  --output-root build \
  --package-root .pqty/texmf \
  --engine-root /var/lib/texmf \
  --engine-root /etc/texmf \
  --output pqty.trace.json
```

Roots may overlap. The most specific matching root wins, so an engine-owned
subtree can be declared inside a broader discovery package tree:

```sh
--package-root /usr/share/texmf-dist \
--engine-root /usr/share/texmf-dist/fonts \
--engine-root /usr/share/texmf-dist/web2c
```

Every recorder input must match exactly one most-specific root. An unmapped
input or two equally specific roots with different scopes is an error. This
fail-closed behavior prevents a package fallback from being silently treated
as part of the engine.

CLI roots are native paths, including Windows drive-letter roots. Resolved
paths written to `pqty.trace/v1` are portable UTF-8 paths with `/` separators;
the adapter fails when a native path cannot be converted losslessly.

`--environment` is optional but recommended. It reads only the schema and
fingerprint from a `pqty.env/v1` JSON document, keeping this crate independent
of pqty's Rust implementation while allowing stale traces to be rejected.
Generate one with `pqty env --lock pqty.lock > pqty.env.json`.
`--environment-fingerprint` is available when a caller already has the value.

The adapter reads only `INPUT` records. `OUTPUT` records are not dependencies;
if an auxiliary output is subsequently read, its `INPUT` record is classified
under the declared output root.

Without `--output`, the JSON trace is written to stdout. Diagnostics go to
stderr, and invalid arguments or ambiguous, unmapped, or unrepresentable paths
fail the command.
