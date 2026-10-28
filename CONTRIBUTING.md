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

## Release handoff to texe

After publishing a stable GitHub release, the release workflow dispatches its tag
to texe's **Update pqty suite** workflow. texe validates the source, records an
exact suite pin, runs installed-suite contracts and prepares a protected dependency
PR. Its normal platform checks and release process produce the next texe suite.
Citet and tex-ls do not need a release for every pqty update.

Install texe's receiver workflow before enabling this dispatcher. The existing
pqty release app must have access to texe with Contents write permission. The
organization having separate release apps for both repositories does not verify
that cross-repository scope. Confirm it during rollout. No new token secret is
required when the existing app has the required access.

Before publishing a new pqty version, run the reusable handoff workflow against
an existing stable tag. Its default mode verifies App access without dispatching:

```sh
gh workflow run texe-handoff.yml --repo backmatter/pqty --ref main -f tag=v0.1.1
```

After texe's receiver is merged, add `-F dispatch=true` to verify the full handoff
with the already pinned tag. The receiver leaves an unchanged pin alone.
The release workflow calls this same handoff with dispatch enabled.

A failed dispatch leaves pqty published. Retry the receiver directly:

```sh
gh workflow run pqty-update.yml --repo backmatter/texe --ref main -f tag=v0.1.2
```

See [texe's release procedure](https://github.com/backmatter/texe/blob/main/docs/releasing.md)
for idempotency and recovery details. Use a new release version for changed
binaries rather than replacing an existing tag.

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

## Reuse CI for release metadata

Code changes run the full platform checks on the pull request. Merging does not
repeat those checks on `main`. A version-only release PR can reuse the successful
source check when its base has exactly the same tree as the checked, merged PR.
The scope job checks parsed manifests and lockfiles. Source changes, dependency
changes, incomplete checks, or unavailable proof require the full matrix.
The reused run is linked in the workflow summary. Ordinary manual CI runs always run the full checks. The synchronized tex-ls
release request can reuse a verified source check. Run `python3 scripts/test_ci_scope.py` when changing this guard.
