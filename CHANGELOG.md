# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.6](https://github.com/backmatter/pqty/compare/v0.1.5...v0.1.6) - 2026-11-04

### Fixed

- *(fetch)* retry transient container download failures ([#9](https://github.com/backmatter/pqty/pull/9))
- *(discovery)* resolve package graphics and preserve locked languages ([#8](https://github.com/backmatter/pqty/pull/8))

## [0.1.5](https://github.com/backmatter/pqty/compare/v0.1.4...v0.1.5) - 2026-11-04

### Fixed

- *(browser)* remove OPFS entries for failed downloads

### Other

- drop the release dispatch to texe
- run the checks on every PR without a scope job
- restore release-plz and its version-only CI reuse
- move browser checks to Vite+ and pnpm, drop release-plz
- *(bundle)* reject crafted bundles before publication
- correct the 0.1.3 changelog and document bundle import trust
- move to Rust 1.99.1 and the newest crates
- *(pqty)* verify browser integrity and persistent offline storage

## [0.1.4](https://github.com/backmatter/pqty/compare/v0.1.3...v0.1.4) - 2026-11-02

### Changed

- Exclude reserved TeX Live distribution metadata from package-file ownership;
  source-image and generated-file records have no installable containers.
- Consolidate all capabilities into `pqty.capabilities/v1`; remove intermediate
  v2/v3 selectors. Consumers must accept the documented v1 capability fields.

### Added

- Add standalone serial `mirror` publication of all runtime providers, with
  verified resumable public objects and a completion catalog published last.
- Prepare and cache standalone `pqty.package/v1` manifests without project
  sources or TEXMF installations. Resolve unique filenames and export verified
  individual files for immutable hosting.
- Add a standalone browser-worker adapter using streaming verification and
  native OPFS persistence, with direct reads into WASM memory and no whole-file
  JavaScript byte buffers or compatibility storage branches.
- Export snapshot-bound runtime filename ownership for stateless host lookups.

## [0.1.3](https://github.com/backmatter/pqty/compare/v0.1.2...v0.1.3) - 2026-11-02

### Fixed

- Reuse the HTTP agent's connection pool across package and metadata fetches.
- Retain verified runtime indexes and compact parsed requests only within a
  hydration operation, rechecking resolution as new providers are added.
- Own temporary files and bundle staging with `tempfile` so ordinary errors and
  unwinding clean them up automatically. Keep atomic artifact replacement,
  competing-writer checks, existing permissions, and persistent recovery data.
- Delegate transfer buffering to standard I/O for package ingestion, download,
  verification, and export. Callers and pqty implementation code no longer tune
  transfer buffer sizes; artifact limits remain enforced.
- Preserve declared TEXMF data dependencies across `Package`, `TLCore`, and
  `ConTeXt` categories, including script and configuration data. Keep platform
  binaries and distribution-root installer helpers outside file ownership lookup.
- Resolve unique literal `\input` requests after project lookup, follow bare
  literal inputs in package files, and refresh closures when input targets or
  declared data dependencies change. Preserve existing v1 artifact formats.
- Scan legacy text bytes without changing source digests or rewriting files;
  dependency arguments still require valid UTF-8.

### Added

- Export and import portable package bundles.
- Stream provider ingestion, locked recovery, and bundle object publication
  through bounded buffers instead of retaining complete provider contents.
- Export one verified locked file offline through `file`, `copy_verified_file`,
  and atomic `export_file`, without a TEXMF tree or private store paths.
- Preserve typed Map/MixedMap/KanjiMap declarations and profile templates in
  `pqty.font-maps/v1`, exposed by `font-maps` and `font_map_manifest`.
  The Rust API adds the required `IndexPackage.font_map_declarations` field.
- Resolve explicit language names and aliases from TeX Live `AddHyphen`
  declarations, retaining loaders, patterns, exceptions, minima, and attributes.
  `lock --require-language`, `require --language`, and `languages` use exact
  snapshot-bound package ownership and preserve the existing v1 lock format.
- Export verified per-file hashes and sizes with `files` and
  `export_file_manifest`, without materializing another TEXMF tree or exposing
  private store paths. Add closed `pqty.files/v1` and `pqty.languages/v1` schemas.
  The Rust API adds the required `IndexPackage.hyphenation` field.
- Advertise these interfaces through capabilities; 0.1.4 folded the interim
  capability selectors into `pqty.capabilities/v1`.

### Other

- Reuse checked source for version-only releases.
- Streaming and reuse changes are functionally tested; this release makes no
  quantified speed or memory-reduction claims.

## [0.1.2](https://github.com/backmatter/pqty/compare/v0.1.1...v0.1.2) - 2026-10-28

### Fixed

- flush transaction ownership through its writable handle
- pass Windows transaction lint and update TLS dependencies
- reuse and coordinate package installations safely

### Other

- verify native handoff access before publishing releases
- hand published pqty releases to texe suite validation

## [0.1.1](https://github.com/backmatter/pqty/compare/v0.1.0...v0.1.1) - 2026-09-08

### Fixed

- qualify package reuse and command suite contracts ([#2](https://github.com/backmatter/pqty/pull/2))

### Added

- `pqty env --output <PATH>` atomically exports an environment without shell
  redirection and preserves the previous output when export fails.

### Fixed

- Reuse local package closures after validating registry identity, source
  requirements, store manifests, and installed package contents.
- Inspect each installed directory once per registry load, with bounded memory
  and file-lookup fallback for directories that cannot be listed.
- Bound pqty-fls recorder and environment reads to 64 MiB, rejecting oversized
  or non-UTF-8 inputs before replacing a trace.
