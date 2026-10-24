#!/usr/bin/env python3
"""Prepare, gate, tag and verify a Clapham release. Run the commands in the order listed.

A release is an annotated v<VERSION> tag on main. `prepare` bumps the version recorded in
release.json; `tag` and `verify` act on that recorded version. `cargo xtask release-check`
remains the authority on release metadata; this tool edits the files that command checks and
runs it.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VERSION = re.compile(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)")
BUMPS = ("major", "minor", "patch")


class Refusal(Exception):
    pass


def run(*command: str, cwd: Path = ROOT, capture: bool = False) -> str:
    print("$", " ".join(command), file=sys.stderr)
    result = subprocess.run(command, cwd=cwd, check=False, text=True, stdout=subprocess.PIPE if capture else None)
    if result.returncode != 0:
        raise Refusal(f"command failed ({result.returncode}): {' '.join(command)}")
    return result.stdout.strip() if capture else ""


def bumped(current: str, bump: str) -> str:
    match = VERSION.fullmatch(current)
    if match is None:
        raise Refusal(f"release.json version {current} is not MAJOR.MINOR.PATCH")
    major, minor, patch = map(int, match.groups())
    if bump == "major":
        return f"{major + 1}.0.0"
    if bump == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def release_record() -> dict:
    return json.loads((ROOT / "release.json").read_text(encoding="utf-8"))


def repository_slug(repository: str) -> str:
    return repository.removeprefix("https://github.com/")


def pages_url(repository: str) -> str:
    owner, name = repository_slug(repository).split("/")
    return f"https://{owner}.github.io/{name}/"


def require_clean() -> None:
    if run("git", "status", "--porcelain", capture=True):
        raise Refusal("the work tree has changes; commit or remove them first")


# prepare


def set_versions(record: dict, new: str) -> None:
    manifest = ROOT / "Cargo.toml"
    text = manifest.read_text(encoding="utf-8")
    text, count = re.subn(
        r'(\[workspace\.package\][^\[]*?^version = ")[^"]*"', rf'\g<1>{new}"', text, count=1, flags=re.M | re.S
    )
    if count != 1:
        raise Refusal("Cargo.toml has no [workspace.package] version")
    for crate in record["crates"]:
        text, count = re.subn(rf'^({re.escape(crate)} = \{{ version = ")[^"]*"', rf'\g<1>{new}"', text, flags=re.M)
        if count != 1:
            raise Refusal(f"Cargo.toml has no versioned workspace dependency for {crate}")
    manifest.write_text(text, encoding="utf-8")
    record["version"] = new


def pin_readme_links(record: dict, tag: str, previous: str) -> list[str]:
    blob = re.compile(rf"{re.escape(record['repository'])}/blob/[^/\s)]+/docs/")
    stale = []
    for crate in record["crates"].values():
        readme = ROOT / crate["path"] / "README.md"
        text = readme.read_text(encoding="utf-8")
        pinned = blob.sub(f"{record['repository']}/blob/{tag}/docs/", text)
        if pinned != text:
            readme.write_text(pinned, encoding="utf-8")
        for number, line in enumerate(pinned.splitlines(), 1):
            if previous != tag[1:] and previous in line.replace(f"/blob/{tag}/", ""):
                stale.append(f"{readme.relative_to(ROOT)}:{number} names {previous}")
    return stale


def update_inventories(record: dict) -> list[str]:
    changes = []
    for name, crate in record["crates"].items():
        listed = run("cargo", "package", "--locked", "--list", "--allow-dirty", "-p", name, capture=True)
        files = sorted(line.replace("\\", "/") for line in listed.splitlines())
        added = sorted(set(files) - set(crate["files"]))
        removed = sorted(set(crate["files"]) - set(files))
        changes += [f"{name}: ships {path}" for path in added] + [f"{name}: no longer ships {path}" for path in removed]
        crate["files"] = files
    return changes


def add_changelog_section(record: dict, new: str) -> bool:
    changelog = ROOT / "CHANGELOG.md"
    text = changelog.read_text(encoding="utf-8")
    if re.search(rf"^## \[{re.escape(new)}\]", text, flags=re.M):
        return False
    section = (
        f"## [{new}]\n\n### Breaking\n\n### Documentation\n\n"
        f"[API reference for {new}]({pages_url(record['repository'])}v{new}/) covers all five library\n"
        "crates with all features enabled.\n\n"
    )
    index = text.find("\n## ") + 1 or len(text)
    changelog.write_text(text[:index] + section + text[index:], encoding="utf-8")
    return True


def prepare(arguments: argparse.Namespace) -> None:
    """On a release branch: bump the version, pin README guide links and refresh package inventories."""
    record = release_record()
    previous = record["version"]
    new = bumped(previous, arguments.bump)
    set_versions(record, new)
    run("cargo", "update", "-w")
    changes = pin_readme_links(record, f"v{new}", previous) + update_inventories(record)
    (ROOT / "release.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    added = add_changelog_section(record, new)

    print(f"\nPrepared {new} from {previous}. Review `git diff`.")
    for change in changes:
        print(f"  ⚠️ {change}")
    if added:
        print(f"Write the CHANGELOG.md [{new}] section: record contract changes under ### Breaking, or state there are none.")
    print("Then commit, open a pull request and run `tools/release.py gates`.")


# gates


def msrv_commands() -> list[list[str]]:
    gate = tomllib.loads((ROOT / "rust-toolchain.toml").read_text(encoding="utf-8"))["toolchain"]["channel"]
    workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
    commands = re.findall(r"^\s*- run: (cargo \+(\S+) .*)$", workflow, flags=re.M)
    return [command.split() for command, toolchain in commands if toolchain != gate]


def gates(arguments: argparse.Namespace) -> None:
    """Before merging the release pull request: run the gates, release check and minimum-version lanes."""
    for gate in (["check"], ["boundaries"], ["api"], ["release-check"]):
        run("cargo", "xtask", *gate)
    commands = msrv_commands()
    for toolchain in sorted({command[1][1:] for command in commands}):
        run("rustup", "toolchain", "install", toolchain, "--profile", "minimal")
    for command in commands:
        run(*command)
    print("\nAll release gates passed. Review generated schema, wire and API changes before merging.")


# tag


def remote_tag_commit(tag: str) -> str | None:
    listing = run("git", "ls-remote", "origin", f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}", capture=True)
    refs = dict(reversed(line.split("\t")) for line in listing.splitlines())
    return refs.get(f"refs/tags/{tag}^{{}}", refs.get(f"refs/tags/{tag}"))


def tag(arguments: argparse.Namespace) -> None:
    """On main after merging: validate the release and push its annotated tag, which drafts a release."""
    record = release_record()
    new = record["version"]
    name = f"v{new}"
    if run("git", "branch", "--show-current", capture=True) != "main":
        raise Refusal("switch to main first")
    require_clean()
    run("git", "pull", "--ff-only", "origin", "main")
    require_clean()
    if run("git", "tag", "--list", name, capture=True) or remote_tag_commit(name):
        raise Refusal(f"{name} already exists; rerun its workflow instead of moving the tag")
    slug = repository_slug(record["repository"])
    reporting = json.loads(run("gh", "api", f"repos/{slug}/private-vulnerability-reporting", capture=True))
    if not reporting.get("enabled"):
        raise Refusal("enable private vulnerability reporting in the repository's security settings")
    with tempfile.TemporaryDirectory() as work:
        notes = Path(work) / "notes.md"
        run("cargo", "xtask", "release-check", name, "--notes", str(notes))
        print(f"\nDraft release notes:\n\n{notes.read_text(encoding='utf-8')}")
    commit = run("git", "rev-parse", "HEAD", capture=True)
    if not arguments.yes and input(f"Tag {commit} as {name} and push it to origin? [y/N] ").strip().lower() != "y":
        raise Refusal("not tagged")
    run("git", "tag", "-a", name, "-m", f"Clapham {new}")
    run("git", "push", "origin", name)
    print(
        f"\nPushed {name}. The Draft release workflow creates a draft: {record['repository']}/actions/workflows/release.yml\n"
        "Review its tag, commit and notes, publish it in GitHub, then run `tools/release.py verify`."
    )


# verify


def fetch(url: str) -> str:
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            return response.read().decode("utf-8")
    except urllib.error.URLError as error:
        raise Refusal(f"cannot fetch {url}: {error}") from error


def release_run(tag: str) -> str:
    runs = json.loads(
        run(
            "gh", "run", "list", "--workflow", "release.yml", "--branch", tag,
            "--json", "status,conclusion,url", "--limit", "1", capture=True,
        )
    )
    if not runs or runs[0]["conclusion"] != "success":
        raise Refusal(f"the Draft release workflow for {tag} has not succeeded")
    return runs[0]["url"]


def check_pages(record: dict, tag: str) -> str:
    site = pages_url(record["repository"])
    if f'href="{tag}/"' not in fetch(site):
        raise Refusal(f"{site} does not list {tag}; check the API documentation workflow")
    for crate in record["crates"]:
        fetch(f"{site}{tag}/{crate.replace('-', '_')}/index.html")
    return f"{site}{tag}/"


def consumer_lockfile(record: dict, tag: str, commit: str) -> str:
    with tempfile.TemporaryDirectory() as work:
        consumer = Path(work) / "clapham-release-consumer"
        run("cargo", "new", "--lib", "--vcs", "none", str(consumer), cwd=Path(work))
        run("cargo", "add", "clapham", "--git", record["repository"], "--tag", tag, cwd=consumer)
        run("cargo", "check", cwd=consumer)
        lockfile = (consumer / "Cargo.lock").read_text(encoding="utf-8")
    sources = {package.get("source", "") for package in tomllib.loads(lockfile)["package"] if package["name"] == "clapham"}
    if sources != {f"git+{record['repository']}?tag={tag}#{commit}"}:
        raise Refusal(f"the consumer resolved clapham to {sources}, not {commit}")
    return lockfile


def verify(arguments: argparse.Namespace) -> None:
    """After publishing the draft by hand: check the release, its API reference and a Git consumer."""
    record = release_record()
    name = f"v{record['version']}"
    slug = repository_slug(record["repository"])
    release = json.loads(run("gh", "release", "view", name, "--repo", slug, "--json", "isDraft,url", capture=True))
    if release["isDraft"]:
        raise Refusal(f"{name} is still a draft; publish it first: {release['url']}")
    commit = remote_tag_commit(name)
    if commit is None:
        raise Refusal(f"origin has no {name} tag")
    workflow = release_run(name)
    reference = check_pages(record, name)
    lockfile = consumer_lockfile(record, name, commit)

    evidence = (
        f"Release {name} is published and verified.\n\n"
        f"- Release: {release['url']}\n"
        f"- Tag: `{name}`\n"
        f"- Commit: `{commit}`\n"
        f"- Draft release workflow: {workflow}\n"
        f"- API reference: {reference}\n"
        f"- Consumer: `cargo add clapham --git {record['repository']} --tag {name}` and `cargo check` "
        f"succeeded and resolved `{commit}`.\n\n"
        f"<details><summary>Consumer Cargo.lock</summary>\n\n```toml\n{lockfile}```\n\n</details>\n"
    )
    print(f"\n{evidence}")
    for issue in arguments.issue:
        repo, _, number = issue.rpartition("#")
        run("gh", "issue", "comment", number, "--repo", repo or slug, "--body", evidence)
    if arguments.issue:
        print("Recorded. Close the release issue.")
    else:
        print("Record this in the release issue and any coordinating issue, then close the release issue.")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(required=True, metavar="COMMAND")

    def command(function, **options) -> argparse.ArgumentParser:
        sub = commands.add_parser(function.__name__, help=function.__doc__, description=function.__doc__, **options)
        sub.set_defaults(run=function)
        return sub

    command(prepare).add_argument("bump", choices=BUMPS, help="the semantic version component to increment")
    command(gates)
    command(tag).add_argument("--yes", action="store_true", help="tag and push without asking")
    verifying = command(verify)
    verifying.add_argument(
        "--issue", action="append", default=[], metavar="[OWNER/REPO#]NUMBER",
        help="comment the evidence on this issue; repeat for coordinating issues",
    )
    arguments = parser.parse_args()
    try:
        arguments.run(arguments)
    except Refusal as refusal:
        sys.exit(f"release: {refusal}")


if __name__ == "__main__":
    os.chdir(ROOT)
    main()
