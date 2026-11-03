# Releasing pqty

Versions are not bumped automatically, and there are no release pull requests.
The version stays at its current value in `Cargo.toml` unless the maintainer
changes it deliberately. To publish, push a matching `vX.Y.Z` tag for a commit
on `main` manually. The tag workflow then:

1. checks tag and crate version parity;
2. dry-runs both crate packages;
3. builds and smoke-tests native archives;
4. publishes `pqty-fls` and then `pqty` with crates.io trusted publishing;
5. combines the archive digests into one `SHA256SUMS` manifest;
6. creates build-provenance attestations for the archives and checksum manifest;
7. uploads the five archives and `SHA256SUMS`; and
8. creates the GitHub release with generated notes and these assets.

## Repository configuration

- Store the release GitHub App client ID in the repository variable
  `RELEASE_APP_CLIENT_ID` and its private key in the repository secret
  `RELEASE_APP_PRIVATE_KEY`; the texe handoff uses them.
- Create the GitHub environment `crates.io` and use that exact environment in
  both crates.io trusted-publisher records.
- Enable release immutability in the repository settings. GitHub applies the
  setting only to releases published after it is enabled.

The CI workflow owns formatting, Clippy, tests, documentation, dependency policy,
browser integrity and persistence, and the TeX acceptance corpus. The Release
workflow's `workflow_dispatch` runs packaging verification without publishing.

Artifact Protocol changes require explicit compatibility review before a
release tag is pushed. Add a `CHANGELOG.md` entry by hand when it matters.

After publication, verify the release and a downloaded archive with:

```console
gh release verify vX.Y.Z
gh attestation verify PATH/TO/ARCHIVE -R backmatter/pqty
```
