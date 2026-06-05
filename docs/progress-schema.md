# Progress stream

`pqty.progress/v1` is a closed JSON Lines protocol written to stderr when a
process integration passes `--progress json`. Artifact Protocol JSON on stdout
is unchanged.

Integrations discover support through the initial capability document:

```sh
pqty --no-config capabilities
```

The stream contains four events:

- `download-plan`: compressed bytes and item count known before body transfer,
  including how much is already in the container cache;
- `download-start`: the item, source URL, declared size, and mirror attempt;
- `download-progress`: throttled received bytes and elapsed transfer time;
- `download-complete`: received bytes and total transfer time. Package
  containers emit it only after size and checksum verification; Registry
  snapshot metadata validation continues after decompression.

Package container sizes come from the selected registry snapshot and are known
before their requests start. The registry metadata body size is known only
after its HTTP response headers arrive and may be absent when the server does
not send `Content-Length`.

Runtime convergence can add providers after a renderer trace. Each newly known
batch emits another plan instead of retroactively pretending it belonged to
the initial closure. Mirror retries emit another `download-start` with a higher
attempt number; only a checksum- and size-verified attempt emits
`download-complete`.

The JSON Schema is
[`schemas/pqty.progress.schema.json`](../schemas/pqty.progress.schema.json).
