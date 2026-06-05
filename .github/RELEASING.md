# Releasing pqty

Release-plz maintains a release pull request containing the shared
`pqty`/`pqty-fls` version, `Cargo.lock`, and changelog. Review and merge that
pull request with a merge commit; do not squash or rebase it.
Release entries are generated in that pull request rather than written
manually.

The merge authorizes release-plz to create the matching `vX.Y.Z` tag and a
draft GitHub release whose notes come from the changelog. The tag workflow
then:

1. checks tag and crate version parity;
2. dry-runs both crate packages;
3. builds and smoke-tests native archives;
4. publishes `pqty-fls` and then `pqty` with crates.io trusted publishing;
5. uploads the archives and publishes the draft GitHub release.

## Repository configuration

- Store the release GitHub App client ID in the repository variable
  `RELEASE_APP_CLIENT_ID`.
- Store its private key in the repository secret
  `RELEASE_APP_PRIVATE_KEY`.
- Create the GitHub environment `crates.io` and use that exact environment in
  both crates.io trusted-publisher records.

The `main` CI workflow owns formatting, Clippy, tests, documentation,
dependency policy, and the TeX acceptance corpus. `workflow_dispatch` runs
packaging verification without publishing.

Artifact Protocol changes require explicit compatibility review before the
release pull request is merged.
