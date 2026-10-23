use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::{Result, capture, cargo};

const LIBRARIES: &[&str] = &[
    "clapham",
    "clapham-cli",
    "clapham-mcp-server",
    "clapham-mcp-client",
    "clapham-conformance",
];

#[derive(Deserialize)]
struct Toolchain {
    rust: String,
    #[serde(rename = "cargo-public-api")]
    cargo_public_api: String,
}

pub(crate) fn check(root: &Path, update: bool) -> Result<()> {
    let toolchain: Toolchain = serde_json::from_str(include_str!("../../../api/toolchain.json"))?;
    let selected = format!("+{}", toolchain.rust);
    let version = capture(&mut cargo(root, &["public-api", "--version"]))?;
    require_version(&version, &toolchain.cargo_public_api)?;
    let mut stale = Vec::new();
    for library in LIBRARIES {
        let output = capture(&mut cargo(
            root,
            &[
                &selected,
                "public-api",
                "--color",
                "never",
                "--all-features",
                "-p",
                library,
            ],
        ))?;
        let output = normalise(&output);
        let path = root.join("api").join(format!("{library}.txt"));
        let previous = match fs::read_to_string(&path) {
            Ok(previous) => Some(previous),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if is_stale(previous.as_deref(), &output) {
            if update {
                fs::write(&path, output)?;
                println!("Updated api/{library}.txt");
            } else {
                stale.push(format!("api/{library}.txt"));
            }
        }
    }
    for entry in fs::read_dir(root.join("api"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "txt")
            && !LIBRARIES
                .iter()
                .any(|name| path.file_stem().is_some_and(|stem| stem == *name))
        {
            if update {
                fs::remove_file(&path)?;
            } else {
                stale.push(path.display().to_string());
            }
        }
    }
    if !stale.is_empty() {
        return Err(format!("missing, stale or obsolete API snapshots: {}\nReview the API change, then run cargo xtask api --update.", stale.join(", ")).into());
    }
    println!("Public API snapshots are current.");
    Ok(())
}

fn require_version(actual: &str, expected: &str) -> Result<()> {
    if actual.trim() != format!("cargo-public-api {expected}") {
        return Err(format!("expected cargo-public-api {expected}, got {}; install with: cargo +1.99.0 install cargo-public-api --version {expected} --locked", actual.trim()).into());
    }
    Ok(())
}

fn normalise(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn is_stale(previous: Option<&str>, generated: &str) -> bool {
    previous.is_none_or(|previous| normalise(previous) != generated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_missing_snapshots_even_for_an_empty_api() {
        assert!(is_stale(None, ""));
        assert!(!is_stale(Some(""), ""));
    }

    #[test]
    fn detects_additions_changes_and_removals_but_accepts_windows_line_endings() {
        assert!(is_stale(Some("pub fn old()\n"), "pub fn new()\n"));
        assert!(is_stale(Some("pub fn old()\n"), ""));
        assert!(is_stale(Some(""), "pub fn new()\n"));
        assert!(!is_stale(Some("pub fn same()\r\n"), "pub fn same()\n"));
    }

    #[test]
    fn refuses_an_unexpected_generator_version() {
        assert!(require_version("cargo-public-api 0.50.2\n", "0.50.2").is_ok());
        assert!(require_version("cargo-public-api 0.50.3\n", "0.50.2").is_err());
    }

    #[test]
    fn records_the_same_nightly_for_the_api_directory_and_gate() {
        let toolchain: Toolchain =
            serde_json::from_str(include_str!("../../../api/toolchain.json")).unwrap();
        assert!(
            include_str!("../../../api/rust-toolchain.toml")
                .contains(&format!("channel = {:?}", toolchain.rust))
        );
    }
}
