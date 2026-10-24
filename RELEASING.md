# Releasing Clapham

A release is a `v<VERSION>` tag on `main`. Consumers take crates from that tag; the workflow
does not publish to crates.io. All workspace members and internal dependency requirements
share the version in the root manifest. `release.json` records the version, repository,
licence, security contact and exact Cargo package inventories for the five library crates.
Tools and the example keep the same version but are not distributed library crates.
Preparing or merging a release pull request does not create a remote tag. The tag becomes
fetchable when pushed; the workflow then creates a draft for separate manual publication.

1. Prepare a pull request updating `[workspace.package].version`, every workspace dependency
   version in `Cargo.toml` and `release.json`. Regenerate `Cargo.lock` with `cargo update -w`.
   Keep prerelease suffixes identical: `v0.1.0-rc.1` requires version `0.1.0-rc.1`, not `0.1.0`.
   Pin each crate README's absolute repository documentation links to that release tag
   (`/blob/v<VERSION>/docs/...`) so packaged API docs retain the matching contract.
   Development links may use `/blob/main/` until a release is prepared.
2. Add the version's `## [VERSION]` section to `CHANGELOG.md`. Put contract changes under
   `### Breaking`, or explicitly record that there are none. The draft notes are the section
   body alone, without adjacent versions or the changelog preamble.
   Lines beginning with `## ` are reserved for release sections; do not use those lines
   inside fenced examples, since the extractor is line-oriented.
3. Review each crate's shipped files using
   `cargo package --locked --list --allow-dirty -p CRATE`. Update its exact `files` inventory
   in `release.json` for intended additions or removals, including Cargo's generated files.
   `--allow-dirty` lets the read-only check run while preparing the pull request. It neither
   packages an archive nor permits a dirty release checkout.
   Cargo's generated `.cargo_vcs_info.json` is optional when checking a source archive.
   Tests and support fixtures remain in the inventory so tagged consumers can run them.
4. Preserve `LICENSE`, `NOTICE` and the extraction records in `provenance.json`. The latter
   already records the MCP client extraction for issue #13. Confirm private vulnerability
   reporting is enabled in the repository's security settings and the contact is correct.
5. Run the gates below and the minimum-version commands in [CONTRIBUTING.md](CONTRIBUTING.md).
   Review generated schema, wire and API changes; merge only after CI passes.

```console
cargo xtask check
cargo xtask boundaries
cargo xtask api
cargo xtask release-check v0.1.0 --notes /tmp/clapham-release-notes.md
```

Use the version being prepared in place of the example tag. With no tag argument,
`release-check` validates the current metadata and manifests; it runs within `xtask check`.
It refuses mismatched tags, member versions, internal dependency versions, repository or
licence drift, package inventory drift, missing documentation, malformed provenance, and a
missing or duplicate changelog section. It writes notes only after every check succeeds.

From a clean checkout after merging:

```console
git switch main
git pull --ff-only origin main
git status --short
cargo xtask release-check v0.1.0
git tag -a v0.1.0 -m 'Clapham 0.1.0'
git push origin v0.1.0
```

Stop if `git status --short` reports changes. The tag workflow runs the same Ubuntu/Windows
gate, minimum-version lanes and API checks as pull requests. Its write-enabled job then
requires the commit to belong to `main`, validates the tag and extracts the notes. It refuses
to change a published release and creates or updates a **draft**, marked as a prerelease
when the version contains a suffix. Review the tag, commit and exact notes in GitHub before
publishing by hand. Rerun a failed workflow for the existing tag; do not move published tags.

Actions are pinned to the current releases verified on 2026-10-24: `actions/checkout` v7.0.1
(`3d3c42e5aac5ba805825da76410c181273ba90b1`) and `softprops/action-gh-release` v3.0.3
(`efb35369e0ad2afab669f228072c1b0d510eae64`). Each tagged `action.yml` declares Node 24.
Dependabot proposes later action updates; recheck their release and runtime when updating.

## Verify and record a published release

Create a fresh consumer workspace outside the repository and check the published Git tag:

```console
cargo new --lib /tmp/clapham-release-consumer
cd /tmp/clapham-release-consumer
cargo add clapham --git https://github.com/thirtytwobits/clapham --tag v0.1.0
cargo check
```

Use the published version in place of the example tag. Confirm Cargo resolves the dependency
to the released commit and retain the consumer's `Cargo.lock` as evidence. Record the release
URL, tag, commit, successful workflow and consumer result in the release issue and any
coordinating project issue. Close the release issue after publication and verification.
