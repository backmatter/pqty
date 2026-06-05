# Contributing to pqty

Bug reports and focused pull requests are welcome. Open an issue before work
that changes Artifact Protocol v1, resolution semantics, registry behavior,
store ownership, or supported platforms.

Security reports belong in the private channel described in
[SECURITY.md](SECURITY.md), not the public issue tracker.

## Development setup

The repository pins its minimum supported Rust toolchain. Install
[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) for the dependency
policy check:

```sh
cargo install cargo-deny --locked
```

Run before opening a pull request:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo deny check
```

When TeX Live is available, also run:

```sh
cargo xtask tex
```

Use `cargo xtask tex common`, `cargo xtask tex convergence`, or
`cargo xtask tex corpus [CASE...]` for a focused acceptance run. The
[corpus notes](corpus/README.md) describe the required TeX tools. The harness
uses a locally discoverable `texlive.tlpdb` by default. Set
`PQTY_XTASK_REMOTE=YYYY-MM-DD` to exercise an immutable remote snapshot instead;
CI pins the snapshot used for the release acceptance suite. State any checks
you could not run in the pull request.

## Pull requests

- Keep changes focused and add regression tests for behavior changes.
- Keep Rust models, JSON Schemas, golden artifacts, and protocol documentation
  aligned.
- Do not weaken path, ownership, digest, download, expansion, or resource
  limits to make a fixture pass.
- Explain user-visible behavior and trust-boundary changes.
- Use a Conventional Commit title. Accepted types are `feat`, `fix`, `docs`,
  `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `style`, and `revert`;
  for example, `fix(store): reject a corrupt object`.

By contributing, you agree that your contribution is licensed under the
repository's MIT license.
