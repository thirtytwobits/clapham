#!/usr/bin/env bash
# Build versioned rustdoc from a list of published release tags, without changing the checkout.
set -euo pipefail

site=${1:?usage: build-api-docs.sh OUTPUT_DIRECTORY TAGS_FILE}
tags=${2:?usage: build-api-docs.sh OUTPUT_DIRECTORY TAGS_FILE}
[[ -s "$tags" ]] || { printf 'No published releases to document.\n' >&2; exit 1; }
mkdir -p "$site"
[[ -z "$(ls -A "$site")" ]] || { printf 'Output directory must be empty.\n' >&2; exit 1; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

header() {
    cat <<'HTML'
<!doctype html>
<html lang="en-GB">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
HTML
    printf '<title>%s</title>\n' "$1"
    cat <<'HTML'
<style>
:root { color-scheme: light dark; font-family: system-ui, sans-serif; line-height: 1.6; }
body { max-width: 52rem; margin: 4rem auto; padding: 0 1.5rem; }
h1 { line-height: 1.2; }
a { color: light-dark(#0757a6, #88c3ff); text-underline-offset: .2em; }
ul { padding-left: 1.5rem; }
li { margin: .65rem 0; }
footer { margin-top: 3rem; border-top: 1px solid GrayText; padding-top: 1rem; }
</style>
</head>
<body><main>
HTML
    printf '<h1>%s</h1>\n' "$1"
}

footer() {
    printf '<footer><a href="https://github.com/thirtytwobits/clapham">Clapham on GitHub</a></footer>\n</main></body></html>\n'
}

header 'Clapham API reference' > "$site/index.html"
printf '<p>Choose a published release. Each reference includes all five library crates with all features enabled.</p>\n<ul>\n' >> "$site/index.html"
while IFS= read -r tag || [[ -n "$tag" ]]; do
    # These characters are also safe in directory names, HTML text and URL paths.
    [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] || {
        printf 'Invalid release tag: %s\n' "$tag" >&2
        exit 1
    }
    source_dir="$work/$tag"
    mkdir "$source_dir"
    git archive --format=tar "refs/tags/$tag" | tar -xf - -C "$source_dir"
    # Share compiled dependencies, but never mix rustdoc files from different versions.
    cargo +1.99.0 clean --doc --manifest-path "$source_dir/Cargo.toml" --target-dir "$work/target"
    CARGO_TARGET_DIR="$work/target" RUSTDOCFLAGS='-D warnings' cargo +1.99.0 doc \
        --locked --all-features --no-deps --manifest-path "$source_dir/Cargo.toml" \
        -p clapham -p clapham-cli -p clapham-mcp-server -p clapham-mcp-client -p clapham-conformance
    mkdir "$site/$tag"
    cp -R "$work/target/doc/." "$site/$tag/"
    # Inlined tracing blanket-impl docs retain one link relative to tracing's own crate.
    tracing_version=$(awk -F '"' '/^name = "tracing"$/ { found=1; next } found && /^version = / { print $2; exit }' "$source_dir/Cargo.lock")
    if [[ -n "$tracing_version" ]]; then
        while IFS= read -r -d '' html; do
            sed "s@href=\"dispatcher#setting-the-default-subscriber\"@href=\"https://docs.rs/tracing/$tracing_version/tracing/dispatcher/index.html#setting-the-default-subscriber\"@g" \
                "$html" > "$work/tracing-links.html"
            mv "$work/tracing-links.html" "$html"
        done < <(find "$site/$tag" -name '*.html' -print0)
    fi
    header "Clapham $tag API reference" > "$site/$tag/index.html"
    printf '<p>Library documentation with all features enabled.</p>\n<ul>\n' >> "$site/$tag/index.html"
    for crate in clapham clapham-cli clapham-mcp-server clapham-mcp-client clapham-conformance; do
        directory=${crate//-/_}
        test -s "$site/$tag/$directory/index.html"
        # README-relative schema links remain useful in the generated crate documentation.
        if [[ -d "$source_dir/crates/$crate/schemas" ]]; then
            cp -R "$source_dir/crates/$crate/schemas" "$site/$tag/$directory/"
        fi
        # Repository-relative guide links in released READMEs need their tagged source URL.
        sed "s@href=\"[.][.]/[.][.]/docs/@href=\"https://github.com/thirtytwobits/clapham/blob/$tag/docs/@g" \
            "$site/$tag/$directory/index.html" > "$work/crate-index.html"
        mv "$work/crate-index.html" "$site/$tag/$directory/index.html"
        printf '<li><a href="%s/">%s</a></li>\n' "$directory" "$crate" >> "$site/$tag/index.html"
    done
    commit=$(git rev-parse "refs/tags/$tag^{commit}")
    printf '</ul>\n<p><a href="https://github.com/thirtytwobits/clapham/tree/%s">Source commit</a> · <a href="https://github.com/thirtytwobits/clapham/blob/%s/docs/README.md">Guides</a></p>\n' "$commit" "$tag" >> "$site/$tag/index.html"
    footer >> "$site/$tag/index.html"
    printf '<li><a href="%s/">%s</a></li>\n' "$tag" "$tag" >> "$site/index.html"
done < "$tags"
printf '</ul>\n' >> "$site/index.html"
footer >> "$site/index.html"
