#!/usr/bin/env python3
"""Clapham's own release steps, which `release.json` lists for `tools/release.py` to run."""

import argparse
import json
import re
import subprocess
import sys
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


class Refusal(Exception):
    pass


def run(*command: str, capture: bool = False) -> str:
    print("$", " ".join(command), file=sys.stderr)
    result = subprocess.run(command, cwd=ROOT, check=False, text=True, stdout=subprocess.PIPE if capture else None)
    if result.returncode != 0:
        raise Refusal(f"command failed ({result.returncode}): {' '.join(command)}")
    return result.stdout.strip() if capture else ""


def pages_url(repository: str) -> str:
    owner, name = repository.removeprefix("https://github.com/").split("/")
    return f"https://{owner}.github.io/{name}/"


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
            if previous in line.replace(f"/blob/{tag}/", ""):
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


def link_api_reference(record: dict, version: str) -> None:
    """Close the version's changelog entry with a link to its API reference."""
    changelog = ROOT / "CHANGELOG.md"
    lines = changelog.read_text(encoding="utf-8").splitlines(keepends=True)
    start = lines.index(f"## {version}\n")
    end = next((i for i in range(start + 1, len(lines)) if lines[i].startswith("## ")), len(lines))
    link = (
        f"[API reference for {version}]({pages_url(record['repository'])}v{version}/) covers all five library\n"
        "crates with all features enabled.\n"
    )
    body = lines[start + 1 : end]
    while body and not body[-1].strip():
        body.pop()
    if "### Documentation\n" not in body:
        body += ["\n", "### Documentation\n"]
    body += ["\n", link, "\n"]
    changelog.write_text("".join(lines[: start + 1] + body + lines[end:]), encoding="utf-8")


def prepare(arguments: argparse.Namespace) -> None:
    """Pin README guide links to the new tag, refresh package inventories and link the API reference."""
    path = ROOT / "release.json"
    record = json.loads(path.read_text(encoding="utf-8"))
    changes = pin_readme_links(record, f"v{arguments.version}", arguments.previous) + update_inventories(record)
    path.write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    link_api_reference(record, arguments.version)
    for change in changes:
        print(f"  ⚠️ {change}")


def msrv(arguments: argparse.Namespace) -> None:
    """Run the CI commands that use a toolchain other than the gate's."""
    del arguments
    gate = tomllib.loads((ROOT / "rust-toolchain.toml").read_text(encoding="utf-8"))["toolchain"]["channel"]
    workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
    commands = [
        command.split()
        for command, toolchain in re.findall(r"^\s*- run: (cargo \+(\S+) .*)$", workflow, flags=re.M)
        if toolchain != gate
    ]
    for toolchain in sorted({command[1][1:] for command in commands}):
        run("rustup", "toolchain", "install", toolchain, "--profile", "minimal")
    for command in commands:
        run(*command)


def fetch(url: str) -> str:
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            return response.read().decode("utf-8")
    except urllib.error.URLError as error:
        raise Refusal(f"cannot fetch {url}: {error}") from error


def pages(arguments: argparse.Namespace) -> None:
    """Check that GitHub Pages serves the release's API reference."""
    record = json.loads((ROOT / "release.json").read_text(encoding="utf-8"))
    site = pages_url(record["repository"])
    if f'href="{arguments.tag}/"' not in fetch(site):
        raise Refusal(f"{site} does not list {arguments.tag}; check the API documentation workflow")
    for crate in record["crates"]:
        fetch(f"{site}{arguments.tag}/{crate.replace('-', '_')}/index.html")
    print(f"API reference: {site}{arguments.tag}/")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(required=True, metavar="COMMAND")
    preparing = commands.add_parser("prepare", help=prepare.__doc__)
    preparing.add_argument("previous")
    preparing.add_argument("version")
    preparing.set_defaults(run=prepare)
    commands.add_parser("msrv", help=msrv.__doc__).set_defaults(run=msrv)
    checking = commands.add_parser("pages", help=pages.__doc__)
    checking.add_argument("tag")
    checking.set_defaults(run=pages)
    arguments = parser.parse_args()
    try:
        arguments.run(arguments)
    except Refusal as refusal:
        sys.exit(f"release steps: {refusal}")


if __name__ == "__main__":
    main()
