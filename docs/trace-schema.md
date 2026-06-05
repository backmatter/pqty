# `pqty.trace/v1`

A trace lets a renderer report observed files without making pqty understand
that renderer's log format.

Reconcile a trace with an exact lock:

```sh
pqty check-trace --lock pqty.lock --trace engine-inputs.json
```

The command emits `pqty.trace-report/v1` and fails when a package input cannot
be matched. Project, engine, and output inputs appear under `ignored`.

To add missing package providers instead:

```sh
pqty converge --lock pqty.lock --trace engine-inputs.json
```

See [runtime convergence](convergence.md) for the fixed-point workflow.

## Fields

| Field | Meaning |
| --- | --- |
| `schema` | Always `pqty.trace/v1`. |
| `producer` | Optional adapter name and version. |
| `environment_fingerprint` | Optional fingerprint of the mounted environment. |
| `inputs[].requested` | Name requested by the engine. |
| `inputs[].resolved` | Optional package-relative TDS path. |
| `inputs[].scope` | `package`, `project`, `engine`, or `output`. |
| `inputs[].kind` | Optional resource category. |

For package inputs, adapters should emit a path such as
`tex/latex/foo/foo.sty`, not a host-absolute path. A leading `texmf-dist/` is
accepted for recorder formats that expose TeX Live's distribution root.

pqty first matches an exact `resolved` path. If it is absent, pqty matches
`requested` only when that basename has exactly one owner. A mismatched path,
ambiguous basename, or unknown basename remains missing.

Scope is explicit because engine recorders also see project files, generated
outputs, formats, configuration, fonts, and engine internals. Only the adapter
knows which root a path came from.

A complete generated example is
[`tests/golden/v1/trace.json`](../tests/golden/v1/trace.json). The schemas are:

- [`schemas/pqty.trace.schema.json`](../schemas/pqty.trace.schema.json)
- [`schemas/pqty.trace-report.schema.json`](../schemas/pqty.trace-report.schema.json)

[`pqty-fls`](https://github.com/backmatter/pqty/blob/main/adapters/fls/README.md)
is the reference adapter for kpathsea `.fls` recorder files.
