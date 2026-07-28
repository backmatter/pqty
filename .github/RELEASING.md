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
5. combines the archive digests into one `SHA256SUMS` manifest;
6. creates build-provenance attestations for the archives and checksum manifest;
7. uploads the five archives and `SHA256SUMS`; and
8. publishes the draft as an immutable GitHub release.

## Repository configuration

- Store the release GitHub App client ID in the repository variable
  `RELEASE_APP_CLIENT_ID`.
- Store its private key in the repository secret
  `RELEASE_APP_PRIVATE_KEY`.
- Create the GitHub environment `crates.io` and use that exact environment in
  both crates.io trusted-publisher records.
- Enable release immutability in the repository settings. GitHub applies the
  setting only to releases published after it is enabled.

The `main` CI workflow owns formatting, Clippy, tests, documentation,
dependency policy, and the TeX acceptance corpus. `workflow_dispatch` runs
packaging verification without publishing.

Artifact Protocol changes require explicit compatibility review before the
release pull request is merged.

After publication, verify the release and a downloaded archive with:

```console
gh release verify vX.Y.Z
gh attestation verify PATH/TO/ARCHIVE -R backmatter/pqty
```
