# Releasing Clapham

A release is a `v<VERSION>` tag on `main`. Consumers take crates from that tag; the workflow
does not publish to crates.io. All workspace members and internal dependency requirements
share the version in the root manifest. `release.json` records the version, repository,
licence, security contact and exact Cargo package inventories for the five library crates.
Tools and the example keep the same version but are not distributed library crates.

1. Prepare a pull request updating `[workspace.package].version`, every workspace dependency
   version in `Cargo.toml`, `Cargo.lock` and `release.json`. Keep prerelease suffixes identical;
   `v0.1.0-rc.1` requires version `0.1.0-rc.1`, not `0.1.0`.
2. Add the version's `## [VERSION]` section to `CHANGELOG.md`. Put contract changes under
   `### Breaking`, or explicitly record that there are none. The draft notes are the section
   body alone, without adjacent versions or the changelog preamble.
3. Review each crate's shipped files using
   `cargo package --locked --list --allow-dirty -p CRATE`. Update its exact `files` inventory
   in `release.json` for intended additions or removals, including Cargo's generated files.
   `--allow-dirty` lets the read-only check run while preparing the pull request. It neither
   packages an archive nor permits a dirty release checkout.
4. Preserve `LICENSE`, `NOTICE` and the extraction records in `provenance.json`. The latter
   already records the MCP client extraction for issue #13. Confirm private vulnerability
   reporting is enabled in the repository's security settings and the contact is correct.
5. Run the gates below and the minimum-version commands in [CONTRIBUTING.md](CONTRIBUTING.md).
   Review generated schema, wire and API changes; merge only after CI passes.

```console
cargo xtask check
cargo xtask boundaries
cargo xtask api
cargo xtask release-check v0.1.0-rc.1 --notes /tmp/clapham-release-notes.md
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
cargo xtask release-check v0.1.0-rc.1
git tag -a v0.1.0-rc.1 -m 'Clapham 0.1.0-rc.1'
git push origin v0.1.0-rc.1
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

## Temporary release-candidate smoke check

Run this acceptance check once the release-machinery pull request is on `main`. Its workflow
and RC manifests must be in the tagged commit; an unmerged branch cannot satisfy the `main`
guard. Do not run this cleanup procedure against a published release.

1. Confirm `v0.1.0-rc.1` and its release do not already exist. Create and push the annotated
   tag using the commands above. Watch the **Draft release** workflow through completion.
2. Generate the expected notes locally using `release-check ... --notes`, then compare them
   with the draft's body. Confirm its tag, target commit, `isDraft: true` and
   `isPrerelease: true`. Do not publish the smoke-test draft.
3. Delete the temporary draft, then its remote and local tag:

```console
gh release view v0.1.0-rc.1 --json tagName,isDraft,isPrerelease,body,targetCommitish
gh release delete v0.1.0-rc.1 --yes
git push origin --delete v0.1.0-rc.1
git tag -d v0.1.0-rc.1
```

Confirm the release and remote tag are absent afterwards. Record the workflow URL and result
in the release-machinery pull request. Prepare the eventual stable version through a new
version/changelog pull request; this smoke check does not publish a release.
