# `pqty.env/v1`

`pqty.env/v1` is the renderer-neutral projection of an exact package
environment:

```sh
pqty env --lock pqty.lock
```

The lock contains source declarations and resolution explanations. The
environment contains only what a renderer needs to identify and mount the
package layer.

## Fields

| Field | Meaning |
| --- | --- |
| `schema` | Always `pqty.env/v1`. |
| `fingerprint` | Opaque SHA-256 identity of the complete manifest. |
| `lock_schema` | Schema of the producing lock. |
| `registries` | Pinned package origins and metadata digests. |
| `requirements.engines` | Known engine constraints; empty means unspecified. |
| `requirements.external_tools` | Explicit non-engine runtime requirements. |
| `font_maps` | Registry-declared map fragments required by the closure. |
| `packages` | Provider, source, integrity, and dependency metadata. |
| `files` | Exact package-owned runtime-file index. |

Every `files[].tds_path` is relative to a TEXMF root, for example
`tex/latex/graphics/graphicx.sty`. It cannot begin with `texmf-dist/`, `/`, or
`..`. `request_name` is the basename normally requested by TeX, and `owner`
identifies its provider.

The fingerprint is deterministic for this schema, but consumers should treat
it as opaque.

## Integration rules

- Include `fingerprint` in compilation cache keys.
- Mount the materialized TEXMF root through the renderer's normal input-root
  mechanism.
- Combine the manifest with the renderer's engine, format, configuration,
  font, and sandbox identity.
- Do not depend on the order of `packages` or `files`.

A complete generated example is
[`tests/golden/v1/environment.json`](../tests/golden/v1/environment.json).
The machine-readable schema is
[`schemas/pqty.env.schema.json`](../schemas/pqty.env.schema.json).
