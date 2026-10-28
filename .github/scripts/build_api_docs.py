#!/usr/bin/env python3
"""Build a release's API reference archive, or assemble the site from published releases' archives.

`archive` documents the checkout, or a tag with `--tag`, as `clapham-api-TAG.tar.gz`; the push to
main keeps it as a release file. `site` builds nothing: it extracts the archive attached to each
published release.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from pathlib import Path

CRATES = ("clapham", "clapham-cli", "clapham-mcp-server", "clapham-mcp-client", "clapham-conformance")
TOOLCHAIN = "+1.99.0"
REPOSITORY = "https://github.com/thirtytwobits/clapham"
# These characters are also safe in directory names, HTML text and URL paths.
TAG = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?")

STYLE = """\
:root { color-scheme: light dark; font-family: system-ui, sans-serif; line-height: 1.6; }
body { max-width: 52rem; margin: 4rem auto; padding: 0 1.5rem; }
h1 { line-height: 1.2; }
a { color: light-dark(#0757a6, #88c3ff); text-underline-offset: .2em; }
ul { padding-left: 1.5rem; }
li { margin: .65rem 0; }
footer { margin-top: 3rem; border-top: 1px solid GrayText; padding-top: 1rem; }
"""


def page(title: str, body: str) -> str:
    return (
        "<!doctype html>\n"
        '<html lang="en-GB">\n'
        "<head>\n"
        '<meta charset="utf-8">\n'
        '<meta name="viewport" content="width=device-width, initial-scale=1">\n'
        f"<title>{title}</title>\n"
        f"<style>\n{STYLE}</style>\n"
        "</head>\n"
        "<body><main>\n"
        f"<h1>{title}</h1>\n"
        f"{body}"
        f'<footer><a href="{REPOSITORY}">Clapham on GitHub</a></footer>\n'
        "</main></body></html>\n"
    )


def git(*args: str) -> str:
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout.strip()


def extract(tag: str, destination: Path) -> None:
    archive = subprocess.Popen(["git", "archive", "--format=tar", f"refs/tags/{tag}"], stdout=subprocess.PIPE)
    with tarfile.open(fileobj=archive.stdout, mode="r|") as tar:
        tar.extractall(destination, filter="data")
    if archive.wait() != 0:
        raise subprocess.CalledProcessError(archive.returncode, archive.args)


def replace_in(path: Path, old: str, new: str) -> None:
    text = path.read_text(encoding="utf-8")
    if old in text:
        path.write_text(text.replace(old, new), encoding="utf-8")


def tracing_version(lock: Path) -> str | None:
    packages = tomllib.loads(lock.read_text(encoding="utf-8")).get("package", [])
    return next((package["version"] for package in packages if package["name"] == "tracing"), None)


def document(tag: str, source: Path, commit: str, site: Path, work: Path) -> None:
    manifest = str(source / "Cargo.toml")
    target = work / "target"
    # Share compiled dependencies, but never mix rustdoc files from different versions.
    subprocess.run(
        ["cargo", TOOLCHAIN, "clean", "--doc", "--manifest-path", manifest, "--target-dir", str(target)], check=True
    )
    packages = [argument for crate in CRATES for argument in ("-p", crate)]
    subprocess.run(
        ["cargo", TOOLCHAIN, "doc", "--locked", "--all-features", "--no-deps", "--manifest-path", manifest, *packages],
        check=True,
        env={**os.environ, "CARGO_TARGET_DIR": str(target), "RUSTDOCFLAGS": "-D warnings"},
    )
    output = site / tag
    shutil.copytree(target / "doc", output)

    # Inlined tracing blanket-impl docs retain one link relative to tracing's own crate.
    version = tracing_version(source / "Cargo.lock")
    if version is not None:
        for html in output.rglob("*.html"):
            replace_in(
                html,
                'href="dispatcher#setting-the-default-subscriber"',
                f'href="https://docs.rs/tracing/{version}/tracing/dispatcher/index.html#setting-the-default-subscriber"',
            )

    items = []
    for crate in CRATES:
        directory = crate.replace("-", "_")
        index = output / directory / "index.html"
        if not index.is_file() or index.stat().st_size == 0:
            sys.exit(f"Missing rustdoc index for {crate} at {tag}.")
        # README-relative schema links remain useful in the generated crate documentation.
        schemas = source / "crates" / crate / "schemas"
        if schemas.is_dir():
            shutil.copytree(schemas, output / directory / "schemas")
        # Repository-relative guide links in released READMEs need their tagged source URL.
        replace_in(index, 'href="../../docs/', f'href="{REPOSITORY}/blob/{tag}/docs/')
        items.append(f'<li><a href="{directory}/">{crate}</a></li>\n')

    body = (
        "<p>Library documentation with all features enabled.</p>\n<ul>\n"
        + "".join(items)
        + "</ul>\n"
        f'<p><a href="{REPOSITORY}/tree/{commit}">Source commit</a> · '
        f'<a href="{REPOSITORY}/blob/{tag}/docs/README.md">Guides</a></p>\n'
    )
    (output / "index.html").write_text(page(f"Clapham {tag} API reference", body), encoding="utf-8")


def archive_name(tag: str) -> str:
    return f"clapham-api-{tag}.tar.gz"


def archive(arguments: argparse.Namespace) -> None:
    """Document the checkout, or a tag, into OUTPUT_DIRECTORY/clapham-api-TAG.tar.gz."""
    root = Path(git("rev-parse", "--show-toplevel"))
    with tempfile.TemporaryDirectory() as work:
        work_path = Path(work)
        if arguments.tag:
            tag = arguments.tag
            source = work_path / "source"
            source.mkdir()
            extract(tag, source)
            commit = git("rev-parse", f"refs/tags/{tag}^{{commit}}")
        else:
            record = json.loads((root / "release.json").read_text(encoding="utf-8"))
            tag = f"v{record['version']}"
            source = root
            commit = git("rev-parse", "HEAD")
        if not TAG.fullmatch(tag):
            sys.exit(f"Invalid release tag: {tag}")
        site = work_path / "site"
        site.mkdir()
        document(tag, source, commit, site, work_path)
        arguments.output.mkdir(parents=True, exist_ok=True)
        with tarfile.open(arguments.output / archive_name(tag), "w:gz") as tar:
            tar.add(site / tag, arcname=tag)
    print(arguments.output / archive_name(tag))


def published_tags() -> list[str]:
    listing = subprocess.run(
        ["gh", "api", "--paginate", "repos/{owner}/{repo}/releases?per_page=100",
         "--jq", ".[] | select(.draft == false) | .tag_name"],
        check=True, capture_output=True, text=True,
    ).stdout
    return listing.split()


def assemble(arguments: argparse.Namespace) -> None:
    """Assemble OUTPUT_DIRECTORY from the API reference archive of every published release."""
    tags = published_tags()
    if not tags:
        sys.exit("No published releases to document.")
    site = arguments.output
    site.mkdir(parents=True, exist_ok=True)
    if any(site.iterdir()):
        sys.exit("Output directory must be empty.")
    with tempfile.TemporaryDirectory() as work:
        for tag in tags:
            if not TAG.fullmatch(tag):
                sys.exit(f"Invalid release tag: {tag}")
            downloads = Path(work) / tag
            name = archive_name(tag)
            fetched = subprocess.run(
                ["gh", "release", "download", tag, "--pattern", name, "--pattern", "SHA256SUMS", "--dir", str(downloads)],
                check=False,
            )
            if fetched.returncode != 0 or not (downloads / name).is_file():
                sys.exit(f"{tag} has no {name}; attach one built by `build_api_docs.py archive --tag {tag}`.")
            sums = downloads / "SHA256SUMS"
            if sums.is_file():
                digest = hashlib.sha256((downloads / name).read_bytes()).hexdigest()
                if f"{digest}  {name}" not in sums.read_text(encoding="utf-8").splitlines():
                    sys.exit(f"{name} does not match the SHA256SUMS of {tag}.")
            with tarfile.open(downloads / name) as tar:
                if any(member.name != tag and not member.name.startswith(f"{tag}/") for member in tar.getmembers()):
                    sys.exit(f"{name} holds files outside {tag}/.")
                tar.extractall(site, filter="data")

    body = (
        "<p>Choose a published release. Each reference includes all five library crates with all features enabled.</p>\n"
        "<ul>\n" + "".join(f'<li><a href="{tag}/">{tag}</a></li>\n' for tag in tags) + "</ul>\n"
    )
    (site / "index.html").write_text(page("Clapham API reference", body), encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(required=True, metavar="COMMAND")
    archiving = commands.add_parser("archive", help=archive.__doc__)
    archiving.add_argument("output", type=Path, metavar="OUTPUT_DIRECTORY")
    archiving.add_argument("--tag", help="document this release tag instead of the checkout")
    archiving.set_defaults(run=archive)
    assembling = commands.add_parser("site", help=assemble.__doc__)
    assembling.add_argument("output", type=Path, metavar="OUTPUT_DIRECTORY")
    assembling.set_defaults(run=assemble)
    arguments = parser.parse_args()
    arguments.run(arguments)


if __name__ == "__main__":
    main()
