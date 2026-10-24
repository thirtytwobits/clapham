# Releasing Clapham

A release is a `v<VERSION>` tag on `main`. Consumers take crates from that tag; the workflow
does not publish to crates.io. All workspace members and internal dependency requirements
share the version in the root manifest. `release.json` records the version, repository,
licence, security contact and exact Cargo package inventories for the five library crates.
Tools and the example keep the same version but are not distributed library crates.

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

## Temporary release-candidate smoke check

Run this acceptance check once the release-machinery pull request is on `main`. Its workflow
and RC manifests must be in the tagged commit; an unmerged branch cannot satisfy the `main`
guard. Complete it before merging the stable-version pull request, while `main` still names
`0.1.0-rc.1`. Do not run this cleanup procedure against a published release.

1. Confirm `v0.1.0-rc.1` and its release do not already exist. Create and push the annotated
   tag from a clean, updated `main`:

```console
cargo xtask release-check v0.1.0-rc.1
git tag -a v0.1.0-rc.1 -m 'Clapham 0.1.0-rc.1'
git push origin v0.1.0-rc.1
```

   Watch the **Draft release** workflow through completion.
2. Generate the expected notes locally using `release-check ... --notes`, then compare them
   with the draft's body. Confirm its tag, target commit, `isDraft: true` and
   `isPrerelease: true`. Do not publish the smoke-test draft.
3. Rerun the successful **Draft release** workflow once. List releases through the API
   (including drafts), and confirm the same release ID is updated with the same notes and
   there is still exactly one release for the tag. This checks draft lookup and rerun behaviour:

```console
gh api --paginate repos/thirtytwobits/clapham/releases \
  --jq '.[] | select(.tag_name == "v0.1.0-rc.1") | {id, tag_name, draft, prerelease, body}'
```

4. Delete the temporary draft, then its remote and local tag:

```console
gh release view v0.1.0-rc.1 --json tagName,isDraft,isPrerelease,body,targetCommitish
gh release delete v0.1.0-rc.1 --yes
git push origin --delete v0.1.0-rc.1
git tag -d v0.1.0-rc.1
```

Confirm the release and remote tag are absent afterwards. Record the workflow URL and result
in the release-machinery pull request. Prepare the eventual stable version through a new
version/changelog pull request; this smoke check does not publish a release.

## First stable release follow-through

The 0.1.0 preparation pull request does not publish a release or finish issue #17. Before
merging it, finish the RC smoke check above and confirm implementation issues #3–#16 are
closed. After merging, require green `main` CI on both runners, including API and
minimum-version lanes. Review the five `api/*.txt` snapshots, `crates/*/schemas/*.json` and
`crates/clapham-conformance/records/mcp-protocol.json` as the surface 0.1.0 commits to.

Create `v0.1.0` using the clean-checkout commands above. When its workflow succeeds, confirm
the draft's commit, stable version (`isPrerelease: false`) and notes against the output of
`cargo xtask release-check v0.1.0 --notes /tmp/clapham-release-notes.md`, then publish it.
Check the real published tag from a fresh consumer workspace:

```console
cargo new --lib /tmp/clapham-0.1.0-consumer
cd /tmp/clapham-0.1.0-consumer
cargo add clapham --git https://github.com/thirtytwobits/clapham --tag v0.1.0
cargo check
```

Confirm the dependency resolves to the published commit. Only then record completion of
step 1 in [London issue #2](https://github.com/thirtytwobits/london/issues/2), with the release
and consumer-check evidence. Record the two settled questions: Clapham owns `Principal`
(`id`, `kind`), converted at a consuming piece's seam; `DefaultGate` admits the descriptor's
principal kinds, whose defaults exclude agents from administrative and destructive operations
until the application admits them. Close Clapham issues #17 and #2 after this follow-through.
