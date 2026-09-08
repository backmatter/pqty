# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
