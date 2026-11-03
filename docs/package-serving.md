# Server preparation and persistent browser files

The first integration uses the application server to resolve and prepare
packages, then serves immutable individual files. Compilation stays in the host's
browser worker. There is no compilation RPC or source upload in this protocol.
No browser-side TeX Live resolver, cross-origin mirror, or new service is needed.

## Package preparation

Load one pinned TeX Live registry on the server. Its metadata digest and snapshot
must agree with the browser compiler's selected runtime. A filename request goes
through `provider_for_file`; unsafe, absent or ambiguous requests fail explicitly.
The server calls `prepare_package` for the unique provider. It streams the
checksummed container into the existing shared content store and publishes
`pqty.package/v1` only after the full declared file set is verified. It does not
prepare the rest of TeX Live or speculatively fetch all dependencies.

A prepared manifest includes registry provenance, one exact provider with its
dependencies, and the provider's file paths, kinds, hashes and sizes. Its
fingerprint is SHA-256 of compact JSON
`[schema, registry, provider, version, container_checksum, container_size,
dependencies, font_maps, integrity, files]`, with absent container fields encoded
as null in that tuple. Object fields follow the Rust model order. Provider
ownership appears once, and file paths are not repeated in a project-lock index.
It is a content/cache identity, not a publisher signature.
`PackageManifest::validate_against` also binds it to registry metadata.
The closed [schema](../schemas/pqty.package.schema.json) is normative for fields.

Checksummed remote providers reuse prepared metadata and content on disk across
process restarts. Preparation uses the existing store lock to coordinate writers.
Mutable local trees are read again. Export checks object bytes, including cached
objects; callers never publish partial output. Missing objects trigger recovery
on preparation. Corrupt content fails verification rather than being served.

## Complete publication before serving

Hosts that own enough storage can publish the complete selected runtime snapshot
before accepting requests. `mirror_packages` and `pqty mirror` load the registry
once and prepare providers serially, reusing the native HTTP connection pool and
verified content store. There are no project scans or per-file subprocesses.

```sh
pqty --no-config mirror --remote YYYY-MM-DD --tlpdb-sha256 sha256:HEX \
  --store /cache/store --output /cache/mirror
```

The output contains `public/<full-sha256>` objects and
`providers/<metadata-sha256>/<provider>.json` manifests. Only after every provider
and public object has been verified does pqty atomically publish
`catalogs/<metadata-sha256>.json` in the existing `pqty.registry/v1` format.
The catalog retains ambiguous ownership rather than selecting a provider.
An interrupted first run leaves no completion catalog. A resumed run reuses
verified public objects and cached providers; damaged public bytes are replaced
atomically from verified store objects. Old snapshot manifests are retained.
Public output and metadata must be outside the private store.

The host deploys only completed catalogs and mounts publication data read-only.
Its request handler needs no package downloader, store, extractor or pqty process.
Missing publication data is an operational error, rather than a trigger for
request-time package preparation. Storage capacity and snapshot update timing
remain host policy. Documentation, source archives and platform executables that
are outside pqty's runtime package index are not part of this publication.
Reserved `00texlive.*` distribution metadata may describe generated or
source-image files without package containers; it does not claim runtime
package ownership.

## HTTP publication owned by the host

The host owns routes and its immutable public asset directory; pqty starts no server.
Cache filename-to-provider results by the pinned registry metadata digest, never
by an unversioned rolling snapshot. A manifest lookup takes a filename, not
project source. A missing filename must not cause repeated network resolution
for every speculative TeX probe; retain misses for that same snapshot and use
actual package requests or engine missing-input observations.

Use `export_package_file` / `package-file` to publish a requested file under its
full SHA-256 digest. Keep publication outside the private pqty store; do not make
private store paths an HTTP contract. Serve the immutable result directly on
subsequent requests, with:

```text
Cache-Control: public, max-age=31536000, immutable
Content-Type: application/octet-stream
```

Only metadata for selected providers and individual files the worker opens go
to the browser. The distribution registry and package containers stay on the
server. Multiple users share the same immutable server artifacts. A CDN can
serve the same objects later without changing pqty's v1 contracts.

The browser compiler's engine/kernel files retain priority over package trees;
a package mirror cannot silently replace the selected format/kernel. Keep the
pinned package snapshot in the compiler's cache identity and retain package
namespace metadata with the host's cached runtime.

## Browser persistence

`adapters/browser` is the standalone `@pqty/browser` package. It exports
`openPackageFile(url, identity, {signal})` for dedicated workers. It uses native
OPFS, Web Locks, Fetch streams and the pinned `@noble/hashes` incremental SHA-256
implementation. There is one filesystem path and no memory-cache substitute,
keep-alive loop, polling, or caller-selected transfer buffer.

Cached files are verified by streaming their bytes. A miss streams Fetch chunks
into an OPFS writable; closing publishes the file atomically only after its
declared size and full checksum match. Failed writes abort. Native read-only
access handles then support random reads directly into caller-owned WASM views,
without constructing a full-file JavaScript byte array. Files survive workers
and browser restarts; handles close explicitly or when the worker terminates.

```js
import { openPackageFile } from "@pqty/browser";

// The host has validated this manifest against the selected runtime snapshot.
const file = manifest.files.find(file => file.tds_path === requestedPath);
const url = new URL(`objects/${file.digest.slice("sha256:".length)}`, packageBase);
const reader = await openPackageFile(url, file, { signal });
try {
  reader.readInto(new Uint8Array(memory.buffer, pointer, length), fileOffset);
} finally {
  reader.close();
}
```

Fetch and OPFS manage chunking and persistence. The 128 MiB file limit matches
pqty's existing artifact trust limit. Storage denial, an offline miss, or
unrecoverable corruption fails explicitly; there is no unverified or alternate
storage path. A failed or invalid download removes its OPFS entry. The adapter's
optional `text()` method lets hosts scan dependency hints from verified files
without a JavaScript byte buffer, but it returns the whole file as one string;
use it only for small text files and `readInto` for everything else.

Readers use `createSyncAccessHandle({ mode: "read-only" })`. Chromium shares
such handles between workers and tabs. Engines that do not implement `mode`
ignore it and return an exclusive handle, so concurrent readers of the same
file must wait for `close()`.

BYOB stream reads transfer their input ArrayBuffer, which excludes WASM linear
memory. Native OPFS reads accept the WASM view directly. Hosts can suspend their
existing synchronous WASI imports with native WebAssembly Promise Integration
while asynchronous preparation finishes, then resume the same engine call.
See the [File System standard](https://fs.spec.whatwg.org/),
[Streams standard](https://streams.spec.whatwg.org/), and
[JSPI documentation](https://v8.dev/blog/jspi).

This is a reusable package adapter. A host still needs to connect prepared package
metadata to its worker namespace and missing-input path. The adapter alone is
not evidence that arbitrary projects compile or that a deployed editor is
already connected.

## Local verification

Native package tests cover server cache reuse without network/container cache,
filename ambiguity, snapshot mismatch, corruption, and atomic file publication.
The browser fixture script uses temporary inputs and isolated persistent
Chromium profiles; it tests first fetch, cached reads, restart/offline reuse,
corruption handling, and transfer of only the requested file.
These are functional and request-count checks, not performance benchmarks.

Build the native CLI, then run the fixture with an installed Playwright module
and, optionally, an explicit Chromium executable. No application checkout or
account is required:

```sh
cargo build --locked --bin pqty
node scripts/check-browser-files.mjs /path/to/playwright/index.mjs /path/to/chromium
```

The fixture disables Chromium's separate HTTP cache so restart/offline checks
specifically verify OPFS. Install the dependencies with `vp install` first, or pass
the installed `@noble/hashes/sha2.js` path as a final argument. It deletes its temporary provider, store,
public files, and browser profile after the check.
