# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Preserve declared TEXMF data dependencies across `Package`, `TLCore`, and
  `ConTeXt` categories, including script and configuration data. Keep platform
  binaries and distribution-root installer helpers outside file ownership lookup.
- Resolve unique literal `\input` requests after project lookup, follow bare
  literal inputs in package files, and refresh closures when input targets or
  declared data dependencies change. Preserve existing v1 artifact formats.
- Scan legacy text bytes without changing source digests or rewriting files;
  dependency arguments still require valid UTF-8.

### Added

- Stream provider ingestion, locked recovery, and bundle object publication
  through bounded buffers instead of retaining complete provider contents.
- Export one verified locked file offline through `file`, `copy_verified_file`,
  and atomic `export_file`, without a TEXMF tree or private store paths.
- Preserve typed Map/MixedMap/KanjiMap declarations and profile templates in
  `pqty.font-maps/v1`, exposed by `font-maps` and `font_map_manifest`.
  Opt-in capabilities v3 advertises the new interfaces; v1 and v2 are unchanged.
  The Rust API adds `IndexPackage.font_map_declarations`.

- Resolve explicit language names and aliases from TeX Live `AddHyphen`
  declarations, retaining loaders, patterns, exceptions, minima, and attributes.
  `lock --require-language`, `require --language`, and `languages` use exact
  snapshot-bound package ownership and preserve the existing v1 lock format.
- Export verified per-file hashes and sizes with `files` and
  `export_file_manifest`, without materializing another TEXMF tree or exposing
  private store paths. Add closed `pqty.files/v1` and `pqty.languages/v1` schemas.
- Opt-in `capabilities --schema pqty.capabilities/v2` negotiation for these
  contracts; the default capabilities output and existing v1 artifacts remain
  compatible. The Rust API adds `IndexPackage.hyphenation`.

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
