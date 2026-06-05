# Runtime convergence

Static LaTeX scanning cannot discover every filename assembled by macros or
selected conditionally. pqty therefore accepts an engine-neutral
`pqty.trace/v1` after a renderer run and can extend an existing exact lock:

```sh
pqty converge --lock pqty.lock --trace engine-inputs.json
```

The command emits `pqty.convergence-report/v1` with one of three outcomes:

- `stable`: every package input is already locked; the lock is unchanged.
- `changed`: missing provider closures were verified and added atomically.
- `unresolved`: an input has no unambiguous provider; the lock is unchanged and
  the command exits unsuccessfully.

Only `package` inputs participate. Project, engine, and output inputs remain
the renderer adapter's responsibility and appear under `ignored`.
Adapters should stamp `environment_fingerprint` with the environment mounted
for the run. pqty then rejects stale traces from an earlier or concurrent
environment before attempting convergence.

An unresolved item with reason `ambiguous-provider` includes the sorted
`candidates` list. Adapters should provide the exact package-relative TDS path
in `resolved` whenever possible; exact paths disambiguate files whose basenames
are shared by multiple TeX Live providers.

Font loaders may report a family or style name without a filename extension.
Adapters represent that as `kind = "font-family"` with no `resolved` path.
pqty case-folds only this explicitly typed name, finds concrete OTF, TTF, or TTC
basenames with that stem, and converges only when the provider is unambiguous.
Ordinary TeX filenames never receive this fallback.

## Fixed-point workflow

The caller owns the renderer loop:

```text
create static lock
→ install immutable TEXMF tree
→ render and emit trace
→ converge lock
→ if changed, install the new tree and render again
→ stop when stable
```

Normal frozen builds start from the stable lock and do not resolve packages or
access registry metadata. A caller should bound its loop and surface a useful
diagnostic if the environment keeps changing.

## Determinism and transactionality

Convergence reloads the registry recorded by the lock unless `--tlpdb` or
`--tlpdb-url` supplies its location explicitly. The exact SHA-256 of the
registry metadata must match the digest in the lock. This prevents adding a
provider from a later state of a rolling mirror or an updated local TeX Live
installation.

All missing inputs are planned before the lock changes. pqty then adds provider
closures in sorted order, hydrates every new runtime file into the
content-addressed store, constructs the new environment, and reconciles the
complete trace again. The caller's in-memory lock and the on-disk lock are
changed only if every step succeeds.

If the report is `changed`, `environment_fingerprint` identifies the new
environment and `previous_environment_fingerprint` identifies the tree that
produced the trace. The caller must materialize and rerun with the new
environment before declaring convergence.

The machine-readable report schema is
[`schemas/pqty.convergence.schema.json`](../schemas/pqty.convergence.schema.json).

For kpathsea-based engines, the separate
[`pqty-fls`](https://github.com/backmatter/pqty/blob/main/adapters/fls/README.md)
crate is the reference `.fls` adapter.
