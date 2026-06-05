# `pqty.lock/v1`

The lock records project declarations and the exact TeX Live package
environment selected for them. It does not contain engine arguments,
compilation passes, auxiliary-file rules, or PDF output.

## Distribution snapshot

TeX Live is a coordinated distribution. pqty selects one registry snapshot and
a subset of providers from it; it does not solve independent version ranges or
mix arbitrary package releases.

Use `--remote YYYY-MM-DD` for long-lived reproducibility. `--remote latest`
tracks the rolling registry and revalidates when resolution is required.

## Stages and sections

| Stage | Meaning |
| --- | --- |
| `scanned` | Project discoveries and unresolved resources. |
| `resolved` | Registry provenance and a provider closure. |
| `exact` | Verified provider manifests, store identities, and file indexes. |

Runtime convergence preserves the `exact` stage.

| Section | Purpose |
| --- | --- |
| `schema`, `stage`, `generated_with` | Format, completeness, and producer. |
| `root`, `sources` | Project paths and source digests. |
| `document_class`, `loaded_classes`, `packages` | Source declarations. |
| `inputs`, `bibliographies`, `graphics` | Project resources. |
| `unresolved` | Declarations static scanning could not resolve. |
| `environment` | Known compatibility constraints. |
| `registries` | Registry origin and exact metadata identity. |
| `consumer_requirements` | Explicit provider and TDS-file requirements. |
| `closure` | Complete direct and transitive provider set. |

An existing exact lock may reuse its hydrated closure when source scanning
finds the same resolution requirements and pinned registry. Changing an
explicit consumer requirement forces fresh resolution.

## Provider records

Each `closure` entry records:

- provider name, distribution revision, and source;
- direct requests and dependency edges;
- the requests that selected it;
- SHA-256 integrity over its sorted TDS path/content manifest;
- its content-addressed store key;
- engine constraints and font-map fragments;
- exact runtime files.

Every runtime file has a relative TDS path such as
`tex/latex/graphics/graphicx.sty`. The `texmf-dist/` installation directory is
not part of that portable path. File digests live in the provider manifest;
the lock carries one integrity digest for the complete manifest.

## Exact-lock invariants

Before use, pqty verifies that:

- the schema and stage are known;
- package requests trace to providers in the closure;
- no package request remains unresolved;
- registry ids and providers are unique;
- registry metadata has an exact SHA-256 identity;
- dependency edges and their inverse relationships are consistent;
- every provider has integrity, a store key, and a non-empty file index;
- project and TDS paths are portable;
- no two providers own the same TDS path.

`install` treats the lock as authoritative. It rehashes cached objects and
verifies provider manifests. Missing content may be fetched only from the
source recorded in the lock and is accepted only when it reproduces the locked
integrity. Corrupt evidence is quarantined; offline installation stops without
recovery.

Installation publishes a new TEXMF tree transactionally. It accepts a new or
empty destination or a tree with a valid pqty ownership marker. The caller
coordinates one writer and a quiescent destination.

Local `.cls` and `.sty` files remain project-owned. pqty includes their digests,
scans them recursively, and resolves only their non-local loads.

Generated examples for all three stages live under
[`tests/golden/v1/`](../tests/golden/v1/). The machine-readable schema is
[`schemas/pqty.lock.schema.json`](../schemas/pqty.lock.schema.json).
