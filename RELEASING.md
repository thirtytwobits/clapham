# Releasing Clapham

A release is an annotated `v<VERSION>` tag on `main`. Consumers take crates from that tag;
nothing is published to crates.io. `release.json` records the version, repository, licence,
security contact and exact Cargo package inventories for the five library crates, and
`cargo xtask release-check` validates them.

`tools/release.py` automates the procedure; `python3 tools/release.py --help` describes each step.

1. On a release branch, run `tools/release.py prepare major|minor|patch`.
2. Add the version's `CHANGELOG.md` section with non-empty subsections. Lines beginning with
   `## ` are reserved for release sections.
3. Review the diff, including each reported package inventory change, and open a pull request.
4. Run `tools/release.py gates`. Review generated schema, wire and API changes; merge only after
   CI passes.
5. On `main`, run `tools/release.py tag`. The tag workflow requires CI to have passed on
   `main` for the tagged commit and creates a **draft** release. If the workflow fails, rerun it for the existing tag; do not
   move a pushed tag.
6. Review the draft's tag, commit and notes in GitHub, then publish it by hand. Publication
   deploys the version's API reference to [GitHub Pages](https://thirtytwobits.github.io/clapham/).
7. Run `tools/release.py verify --issue <NUMBER>` to record the evidence on the release
   issue, then close the issue.

Pages uses the GitHub Actions publishing source. Its `github-pages` environment permits
deployments from `main` and `v*` tags. Dispatch **API documentation** on `main` to rebuild the site.

Actions are pinned to the releases verified on 2026-10-24: `actions/checkout` v7.0.1
(`3d3c42e5aac5ba805825da76410c181273ba90b1`) and `softprops/action-gh-release` v3.0.3
(`efb35369e0ad2afab669f228072c1b0d510eae64`). Each tagged `action.yml` declares Node 24.
Dependabot proposes later action updates; recheck their release and runtime when updating.

<!-- newsletter:begin releasing · maintained in thirtytwobits/london -->

## Rules

- `CHANGELOG.md` holds the next release under `## Unreleased` and each release under
  `## VERSION`, with no date. An entry's subsections are `### Breaking`, `### Added`, `### Fixed`
  and `### Documentation`, in that order, each present only when it has text. A pull request that
  changes what a consumer sees adds its line under `## Unreleased`.
- A release is an annotated `vVERSION` tag on `main`. Never move or delete a pushed tag; rerun its
  workflow instead.
- The push to `main` builds the release files once. The tag workflow and publication build
  nothing.
- `tools/release.py` and `.github/workflows/release.yml` are sent from thirtytwobits/london;
  change them there.

## Procedure

`python3 tools/release.py --help` describes each step. `release.json` lists the steps
Clapham adds to `prepare`, `gates` and `verify`, and the release files its main
build keeps.

1. On a release branch, run `tools/release.py prepare major|minor|patch`. It renames the
   `## Unreleased` changelog entry `## VERSION`, sets the version and runs the prepare steps.
2. Review the diff, open a pull request and run `tools/release.py gates`. Merge once CI passes.
3. The push to `main` runs CI, which keeps the release files as its `release-files` artifact for
   90 days.
4. On `main`, run `tools/release.py tag`. It requires CI to have passed on `main` for the commit,
   then pushes the annotated tag `vVERSION`.
5. The Draft release workflow creates a **draft** release with the version's changelog entry as
   its notes, and attaches that main build's files and their `SHA256SUMS`. It builds nothing. If
   it fails, rerun it for the existing tag; never move a pushed tag. If the main build's files have
   expired, rerun that build, then the Draft release workflow.
6. Review the draft's tag, commit, notes and files in GitHub, then publish it by hand.
7. Run `tools/release.py verify --issue NUMBER` to record the evidence on the release issue, then
   close the issue.

CI runs on Rust 1.99.0; the libraries support Rust 1.88 and later.

<!-- newsletter:end releasing -->
