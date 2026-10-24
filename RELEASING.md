# Releasing Clapham

A release is an annotated `v<VERSION>` tag on `main`. Consumers take crates from that tag;
nothing is published to crates.io. `release.json` records the version, repository, licence,
security contact and exact Cargo package inventories for the five library crates, and
`cargo xtask release-check` validates them.

`tools/release.py` automates the procedure; `python3 tools/release.py --help` describes each step.

1. On a release branch, run `tools/release.py prepare major|minor|patch`.
2. Write the version's `CHANGELOG.md` section. Record contract changes under `### Breaking`, or
   state that there are none. Lines beginning with `## ` are reserved for release sections.
3. Review the diff, including each reported package inventory change, and open a pull request.
4. Run `tools/release.py gates`. Review generated schema, wire and API changes; merge only after
   CI passes.
5. On `main`, run `tools/release.py tag`. The tag workflow reruns the CI gate and
   creates a **draft** release. If the workflow fails, rerun it for the existing tag; do not
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
