//! Validate release metadata against Cargo and extract exactly one changelog section.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use serde_json::Value;

use crate::{Result, capture, cargo};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Release {
    schema_version: u32,
    version: String,
    repository: String,
    license: String,
    security_contact: String,
    crates: BTreeMap<String, Crate>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Crate {
    path: String,
    files: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    workspace_root: PathBuf,
    workspace_members: Vec<String>,
    packages: Vec<Package>,
}

#[derive(Debug, Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
    repository: Option<String>,
    license: Option<String>,
    manifest_path: PathBuf,
    dependencies: Vec<Dependency>,
}

#[derive(Debug, Deserialize)]
struct Dependency {
    name: String,
    req: String,
}

pub(crate) fn check(root: &Path, tag: Option<&str>, notes_path: Option<&Path>) -> Result<()> {
    let release: Release = serde_json::from_str(&fs::read_to_string(root.join("release.json"))?)?;
    let metadata: Metadata = serde_json::from_str(&capture(&mut cargo(
        root,
        &[
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--no-deps",
            "--all-features",
        ],
    ))?)?;
    validate(&release, &metadata, tag)?;
    for name in release.crates.keys() {
        let inventory = capture(&mut cargo(
            root,
            &["package", "--locked", "--list", "--allow-dirty", "-p", name],
        ))?;
        check_files(name, &release.crates[name].files, &inventory)?;
    }
    for file in [
        "LICENSE",
        "NOTICE",
        "RELEASING.md",
        "SECURITY.md",
        "docs/using-a-release.md",
    ] {
        if fs::read_to_string(root.join(file))?.trim().is_empty() {
            return Err(format!("{file} must not be empty").into());
        }
    }
    let security = fs::read_to_string(root.join("SECURITY.md"))?;
    if !security.contains(&release.security_contact) {
        return Err("SECURITY.md does not contain release.json's security contact".into());
    }
    check_provenance(
        root,
        &serde_json::from_str(&fs::read_to_string(root.join("provenance.json"))?)?,
    )?;
    let notes = release_notes(
        &fs::read_to_string(root.join("CHANGELOG.md"))?,
        &release.version,
    )?;
    // Write only after every check succeeds. Relative paths use the invocation's directory.
    if let Some(path) = notes_path {
        fs::write(path, notes)?;
    }
    println!(
        "Release v{} metadata, package files and changelog are current.",
        release.version
    );
    Ok(())
}

fn validate(release: &Release, metadata: &Metadata, tag: Option<&str>) -> Result<()> {
    if release.schema_version != 1 {
        return Err("unsupported release.json schema_version".into());
    }
    if release.version.is_empty() || release.repository.is_empty() || release.license.is_empty() {
        return Err("release version, repository and licence must not be empty".into());
    }
    let expected_tag = format!("v{}", release.version);
    if let Some(tag) = tag.filter(|tag| *tag != expected_tag) {
        return Err(format!("tag must be {expected_tag}, got {tag}").into());
    }
    if !release.repository.starts_with("https://github.com/")
        || release.security_contact != format!("{}/security/advisories/new", release.repository)
    {
        return Err("security contact must be this repository's private advisory form".into());
    }
    let members: Vec<_> = metadata
        .packages
        .iter()
        .filter(|package| metadata.workspace_members.contains(&package.id))
        .collect();
    if members.is_empty() || members.len() != metadata.workspace_members.len() {
        return Err("Cargo metadata does not contain every workspace member".into());
    }
    let names: BTreeSet<_> = members
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    let mut libraries = BTreeSet::new();
    let root = &metadata.workspace_root;
    for package in members {
        if package.version != release.version
            || package.repository.as_deref() != Some(&release.repository)
            || package.license.as_deref() != Some(&release.license)
        {
            return Err(format!(
                "{}: version, repository or licence differs from release.json",
                package.name
            )
            .into());
        }
        for dependency in &package.dependencies {
            if names.contains(dependency.name.as_str())
                && dependency.req != format!("^{}", release.version)
            {
                return Err(format!(
                    "{}: workspace dependency {} must name version {}",
                    package.name, dependency.name, release.version
                )
                .into());
            }
        }
        // Keep Cargo's paths in one representation. canonicalize() adds the Windows
        // verbatim prefix (\\?\), while cargo metadata uses ordinary drive paths.
        if package.manifest_path.starts_with(root.join("crates")) {
            libraries.insert(package.name.clone());
            let record = release
                .crates
                .get(&package.name)
                .ok_or_else(|| format!("{} is missing from release.json", package.name))?;
            if package.manifest_path != root.join(&record.path).join("Cargo.toml") {
                return Err(format!(
                    "{}: crate path differs from its Cargo manifest",
                    package.name
                )
                .into());
            }
        }
    }
    if libraries.is_empty() || libraries != release.crates.keys().cloned().collect() {
        return Err("release.json must inventory exactly the library crates under crates/".into());
    }
    Ok(())
}

fn check_files(name: &str, recorded: &[String], inventory: &str) -> Result<()> {
    let mut recorded_set: BTreeSet<_> = recorded.iter().cloned().collect();
    if recorded_set.len() != recorded.len() {
        return Err(format!("{name}: release.json contains duplicate package paths").into());
    }
    let actual: BTreeSet<_> = inventory
        .lines()
        .map(|line| line.replace('\\', "/"))
        .collect();
    // Cargo generates this only in a Git work tree. Source archives contain the
    // same crate files but cannot supply this generated VCS record.
    if !actual.contains(".cargo_vcs_info.json") {
        recorded_set.remove(".cargo_vcs_info.json");
    }
    if recorded_set != actual {
        let added: Vec<_> = actual.difference(&recorded_set).collect();
        let removed: Vec<_> = recorded_set.difference(&actual).collect();
        return Err(format!("{name}: package inventory differs from release.json; added {added:?}, removed {removed:?}; review cargo package --locked --list --allow-dirty -p {name}").into());
    }
    for required in ["Cargo.toml", "LICENSE", "NOTICE", "README.md", "src/lib.rs"] {
        if !actual.contains(required) {
            return Err(format!("{name}: package must ship {required}").into());
        }
    }
    Ok(())
}

fn check_provenance(root: &Path, provenance: &Value) -> Result<()> {
    let extractions = provenance["extractions"]
        .as_array()
        .filter(|records| !records.is_empty())
        .ok_or("provenance.json must record the extractions")?;
    if provenance["version"] != 1
        || provenance["source_repository"] != "https://github.com/thirtytwobits/battersea"
        || !extractions.iter().any(|record| record["issue"] == 13)
    {
        return Err("provenance.json must retain the MCP client extraction for issue #13".into());
    }
    for record in extractions {
        if !record["source_commit"]
            .as_str()
            .is_some_and(|sha| sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
            || !record["source_tag"]
                .as_str()
                .is_some_and(|tag| tag.starts_with('v'))
        {
            return Err("extraction provenance needs a source tag and full commit SHA".into());
        }
        for field in ["source_paths", "destination_paths"] {
            let paths = record[field]
                .as_array()
                .filter(|paths| !paths.is_empty())
                .ok_or("extraction provenance needs source and destination paths")?;
            for path in paths {
                let path = path.as_str().ok_or("provenance paths must be strings")?;
                if path.is_empty()
                    || Path::new(path)
                        .components()
                        .any(|part| !matches!(part, std::path::Component::Normal(_)))
                    || (field == "destination_paths" && !root.join(path).exists())
                {
                    return Err(format!("invalid extraction {field} path: {path}").into());
                }
            }
        }
    }
    Ok(())
}

/// Changelog lines beginning with `## ` are reserved for release sections, including
/// inside fenced examples. This is a section extractor, not a Markdown parser.
fn release_notes(changelog: &str, version: &str) -> Result<String> {
    let heading = format!("## [{version}]");
    let mut found = 0;
    let mut selected = false;
    let mut lines = Vec::new();
    for line in changelog.lines() {
        if line.starts_with("## ") {
            selected = line == heading
                || line
                    .strip_prefix(&heading)
                    .is_some_and(|tail| tail.starts_with(" - "));
            if selected {
                found += 1;
            }
        } else if selected {
            lines.push(line);
        }
    }
    if found != 1 {
        return Err(
            format!("CHANGELOG.md needs exactly one {heading} section, found {found}").into(),
        );
    }
    let subsections: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.starts_with("### ").then_some(index))
        .collect();
    if subsections.is_empty() {
        return Err(format!("CHANGELOG.md {heading} needs a subsection").into());
    }
    for (position, index) in subsections.iter().copied().enumerate() {
        let end = subsections
            .get(position + 1)
            .copied()
            .unwrap_or(lines.len());
        if !lines[index + 1..end]
            .iter()
            .any(|line| !line.trim().is_empty())
        {
            return Err(format!("CHANGELOG.md {heading} {} is empty", lines[index]).into());
        }
    }
    Ok(format!("{}\n", lines.join("\n").trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (Release, Metadata) {
        let root = PathBuf::from("/release-fixture");
        let release = serde_json::from_value(json!({
            "schema_version":1,"version":"0.1.0-rc.1","repository":"https://github.com/example/library",
            "license":"MIT","security_contact":"https://github.com/example/library/security/advisories/new",
            "crates":{"clapham":{"path":"crates/clapham","files":[]}}
        })).unwrap();
        let metadata = serde_json::from_value(json!({
            "workspace_root":root,"workspace_members":["core","tool"],"packages":[
                {"id":"core","name":"clapham","version":"0.1.0-rc.1","repository":"https://github.com/example/library",
                 "license":"MIT","manifest_path":root.join("crates/clapham/Cargo.toml"),"dependencies":[]},
                {"id":"tool","name":"xtask","version":"0.1.0-rc.1","repository":"https://github.com/example/library",
                 "license":"MIT","manifest_path":root.join("tools/xtask/Cargo.toml"),
                 "dependencies":[{"name":"clapham","req":"^0.1.0-rc.1"}]}
            ]
        })).unwrap();
        (release, metadata)
    }

    #[test]
    fn tag_must_exactly_name_the_workspace_version_including_prerelease() {
        let (release, metadata) = fixture();
        assert!(validate(&release, &metadata, Some("v0.1.0-rc.1")).is_ok());
        assert!(validate(&release, &metadata, None).is_ok());
        for tag in ["v0.1.0", "v0.1.0-rc.2", "0.1.0-rc.1", "v0.2.0", ""] {
            let error = validate(&release, &metadata, Some(tag)).unwrap_err();
            assert!(
                error.to_string().contains("tag must be v0.1.0-rc.1"),
                "{error}"
            );
        }
    }

    #[test]
    fn all_members_and_internal_dependencies_must_match_release_metadata() {
        for field in [
            "version",
            "repository",
            "license",
            "dependency",
            "path",
            "missing",
        ] {
            let (mut release, mut metadata) = fixture();
            match field {
                "version" => metadata.packages[1].version = "0.1.0".into(),
                "repository" => metadata.packages[0].repository = None,
                "license" => metadata.packages[0].license = Some("other".into()),
                "dependency" => metadata.packages[1].dependencies[0].req = "*".into(),
                "path" => release.crates.get_mut("clapham").unwrap().path = "other".into(),
                "missing" => {
                    release.crates.clear();
                }
                _ => unreachable!(),
            }
            assert!(validate(&release, &metadata, None).is_err(), "{field}");
        }
    }

    #[test]
    fn cargo_paths_are_compared_with_cargos_workspace_root() {
        // Exercise real Cargo metadata, including Windows drive paths. The gate's
        // canonical filesystem root can have a different prefix from these paths.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let release: Release =
            serde_json::from_str(&fs::read_to_string(root.join("release.json")).unwrap()).unwrap();
        let metadata: Metadata = serde_json::from_str(
            &capture(&mut cargo(
                &root,
                &[
                    "metadata",
                    "--locked",
                    "--format-version",
                    "1",
                    "--no-deps",
                    "--all-features",
                ],
            ))
            .unwrap(),
        )
        .unwrap();
        validate(&release, &metadata, None).unwrap();
    }

    #[test]
    fn shipped_file_additions_removals_and_duplicates_require_review() {
        let inventory = "Cargo.toml\nLICENSE\nNOTICE\nREADME.md\nsrc/lib.rs\n";
        let files: Vec<_> = inventory.lines().map(str::to_owned).collect();
        assert!(check_files("clapham", &files, inventory).is_ok());
        assert!(check_files("clapham", &files, &inventory.replace('/', "\\")).is_ok());
        assert!(check_files("clapham", &files, &(inventory.to_owned() + "secret.txt\n")).is_err());
        assert!(check_files("clapham", &files, &inventory.replace("NOTICE\n", "")).is_err());
        let mut duplicate = files.clone();
        duplicate.push(files[0].clone());
        assert!(check_files("clapham", &duplicate, inventory).is_err());
        let missing_notice: Vec<_> = files.into_iter().filter(|file| file != "NOTICE").collect();
        assert!(
            check_files(
                "clapham",
                &missing_notice,
                &inventory.replace("NOTICE\n", "")
            )
            .is_err()
        );
    }

    #[test]
    fn source_archives_may_omit_only_cargos_generated_vcs_record() {
        let archive = "Cargo.lock\nCargo.toml\nLICENSE\nNOTICE\nREADME.md\nsrc/lib.rs\n";
        let checkout = format!(".cargo_vcs_info.json\n{archive}");
        let recorded: Vec<_> = checkout.lines().map(str::to_owned).collect();
        assert!(check_files("clapham", &recorded, &checkout).is_ok());
        assert!(check_files("clapham", &recorded, archive).is_ok());
        for missing in ["Cargo.lock\n", "NOTICE\n", "src/lib.rs\n"] {
            assert!(check_files("clapham", &recorded, &archive.replace(missing, "")).is_err());
        }
        let unrecorded: Vec<_> = archive.lines().map(str::to_owned).collect();
        assert!(check_files("clapham", &unrecorded, &checkout).is_err());
    }

    #[test]
    fn notes_are_only_the_selected_body_and_accept_windows_line_endings() {
        let changelog = "# Changelog\n\n## [Unreleased]\nFuture\n\n## [0.1.0-rc.1]\n\n### Changes\n\nInitial contract.\n\n### Added\n\nFirst release.\n\n## [0.0.1] - 2026-01-01\nOlder\n";
        let expected = "### Changes\n\nInitial contract.\n\n### Added\n\nFirst release.\n";
        assert_eq!(release_notes(changelog, "0.1.0-rc.1").unwrap(), expected);
        assert_eq!(
            release_notes(&changelog.replace('\n', "\r\n"), "0.1.0-rc.1").unwrap(),
            expected
        );
        assert!(
            release_notes(
                &changelog.replace("### Changes", "### Breaking"),
                "0.1.0-rc.1"
            )
            .is_ok()
        );
        for (invalid, reason) in [
            (
                changelog
                    .replace("### Changes", "Changes")
                    .replace("### Added", "Added"),
                "needs a subsection",
            ),
            (
                "## [0.1.0-rc.1]\n### Changes\n\n### Added\nNew.\n".into(),
                "### Changes is empty",
            ),
            (
                changelog.to_owned() + "## [0.1.0-rc.1]\nDuplicate\n",
                "found 2",
            ),
            ("## [0.1.0-rc.2]\nOther\n".into(), "found 0"),
        ] {
            let error = release_notes(&invalid, "0.1.0-rc.1").unwrap_err();
            assert!(error.to_string().contains(reason), "{error}");
        }
    }

    #[test]
    fn extraction_records_require_source_identity_and_existing_destinations() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let provenance: Value =
            serde_json::from_str(include_str!("../../../provenance.json")).unwrap();
        assert!(check_provenance(&root, &provenance).is_ok());
        for (field, value) in [
            ("source_commit", json!("short")),
            ("source_tag", Value::Null),
            ("source_paths", json!([])),
            ("destination_paths", json!(["../outside"])),
            ("destination_paths", json!(["absent-destination"])),
        ] {
            let mut broken = provenance.clone();
            broken["extractions"][0][field] = value;
            assert!(check_provenance(&root, &broken).is_err(), "{field}");
        }
    }
}
