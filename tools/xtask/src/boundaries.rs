use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{Result, capture, cargo};

// A member may depend only on the workspace members named here. External
// dependencies remain available except for the other London pieces below.
const POLICIES: &[(&str, &str, &[&str])] = &[
    ("clapham", "crates/clapham", &[]),
    // The CLI's daemon mode is Clapham's MCP client.
    (
        "clapham-cli",
        "crates/clapham-cli",
        &["clapham", "clapham-mcp-client"],
    ),
    (
        "clapham-mcp-server",
        "crates/clapham-mcp-server",
        &["clapham"],
    ),
    (
        "clapham-mcp-client",
        "crates/clapham-mcp-client",
        &["clapham"],
    ),
    (
        "clapham-conformance",
        "crates/clapham-conformance",
        &[
            "clapham",
            "clapham-cli",
            "clapham-mcp-server",
            "clapham-mcp-client",
        ],
    ),
    (
        "clapham-lists",
        "examples/lists",
        &[
            "clapham",
            "clapham-cli",
            "clapham-mcp-server",
            "clapham-conformance",
        ],
    ),
    (
        "clapham-typed",
        "examples/typed",
        &["clapham", "clapham-cli", "clapham-mcp-server"],
    ),
    ("xtask", "tools/xtask", &["clapham", "clapham-cli"]),
];

#[derive(Deserialize)]
struct Metadata {
    workspace_root: PathBuf,
    workspace_members: Vec<String>,
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<Dependency>,
    #[serde(default)]
    features: BTreeMap<String, Vec<String>>,
}

#[derive(Default, Deserialize)]
struct Dependency {
    // Cargo reports the package name here, even when the dependency is renamed.
    name: String,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    uses_default_features: bool,
    rename: Option<String>,
}

// Audited against rmcp 3.6.0's Cargo.toml feature graph. Only `default` forwards
// `server`; `tower` and `auth` do not enable any server feature. Re-audit on a bump.
// These enable server code, either directly or through a workspace feature.
const RMCP_SERVER_FEATURES: &[&str] = &[
    "default",
    "server",
    "server-side-http",
    "transport-streamable-http-server",
    "transport-streamable-http-server-session",
];

pub(crate) fn check(root: &Path) -> Result<()> {
    let metadata = capture(&mut cargo(
        root,
        &[
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--no-deps",
            "--all-features",
        ],
    ))?;
    let metadata: Metadata = serde_json::from_str(&metadata)?;
    validate(&metadata).map_err(|errors| errors.join("\n"))?;
    for package in &metadata.packages {
        if !metadata.workspace_members.contains(&package.id) {
            continue;
        }
        let directory = package
            .manifest_path
            .parent()
            .ok_or("manifest has no parent")?;
        if directory.starts_with(metadata.workspace_root.join("crates")) {
            for name in ["README.md", "LICENSE", "NOTICE"] {
                let path = directory.join(name);
                if fs::read_to_string(&path)?.trim().is_empty() {
                    return Err(format!("{} must not be empty", path.display()).into());
                }
            }
        }
    }
    println!("Dependency boundaries and crate files are current.");
    Ok(())
}

fn validate(metadata: &Metadata) -> std::result::Result<(), Vec<String>> {
    let members: HashSet<_> = metadata
        .packages
        .iter()
        .filter(|package| metadata.workspace_members.contains(&package.id))
        .map(|package| package.name.as_str())
        .collect();
    let mut errors = Vec::new();
    for package in &metadata.packages {
        if !metadata.workspace_members.contains(&package.id) {
            continue;
        }
        let Some((_, directory, allowed)) =
            POLICIES.iter().find(|(name, _, _)| *name == package.name)
        else {
            errors.push(format!(
                "{}: no dependency policy; declare it in tools/xtask",
                package.name
            ));
            continue;
        };
        if package.manifest_path != metadata.workspace_root.join(directory).join("Cargo.toml") {
            errors.push(format!(
                "{}: expected its manifest under {directory}",
                package.name
            ));
        }
        for dependency in &package.dependencies {
            if dependency.name == "rmcp" && package.name != "clapham-mcp-server" {
                let alias = dependency.rename.as_deref().unwrap_or(&dependency.name);
                let forwarded = package.features.values().flatten().any(|feature| {
                    feature.split_once('/').is_some_and(|(name, feature)| {
                        name.trim_end_matches('?') == alias
                            && RMCP_SERVER_FEATURES.contains(&feature)
                    })
                });
                if dependency.uses_default_features
                    || dependency
                        .features
                        .iter()
                        .any(|feature| RMCP_SERVER_FEATURES.contains(&feature.as_str()))
                    || forwarded
                {
                    errors.push(format!(
                        "{}: rmcp server features belong to clapham-mcp-server alone",
                        package.name
                    ));
                }
            }
            if ["battersea-", "clerkenwell-", "walthamstow-"]
                .iter()
                .any(|prefix| dependency.name.starts_with(prefix))
            {
                errors.push(format!(
                    "{}: forbidden dependency {} (another London piece)",
                    package.name, dependency.name
                ));
            } else if members.contains(dependency.name.as_str())
                && !allowed.contains(&dependency.name.as_str())
            {
                errors.push(format!(
                    "{}: forbidden workspace dependency {}",
                    package.name, dependency.name
                ));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "clapham-boundaries-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn crate_at(&self, directory: &str, manifest: &str) {
            let path = self.0.join(directory);
            fs::create_dir_all(path.join("src")).unwrap();
            fs::write(path.join("Cargo.toml"), manifest).unwrap();
            fs::write(path.join("src/lib.rs"), "").unwrap();
        }

        fn metadata(&self) -> Metadata {
            let output = Command::new("cargo")
                .current_dir(&self.0)
                .args([
                    "metadata",
                    "--offline",
                    "--no-deps",
                    "--format-version",
                    "1",
                ])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            serde_json::from_slice(&output.stdout).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    // Exercise Cargo's real metadata, rather than a hand-written approximation.
    // Every declaration must fail, including an optional edge for another OS.
    #[test]
    fn refuses_other_pieces_in_every_dependency_kind_even_when_renamed() {
        for name in [
            "battersea-core",
            "clerkenwell-document",
            "walthamstow-workspace",
        ] {
            for section in [
                "dependencies",
                "dev-dependencies",
                "build-dependencies",
                "target.'cfg(windows)'.dependencies",
            ] {
                let fixture = Fixture::new();
                fs::write(fixture.0.join("Cargo.toml"), "[workspace]\nmembers = [\"crates/clapham\"]\nexclude = [\"external\"]\nresolver = \"2\"\n").unwrap();
                fixture.crate_at(
                    "external",
                    &format!("[package]\nname = {name:?}\nversion = \"0.1.0\"\n"),
                );
                let optional = if section.ends_with("dev-dependencies") {
                    ""
                } else {
                    ", optional = true"
                };
                fixture.crate_at("crates/clapham", &format!("[package]\nname = \"clapham\"\nversion = \"0.1.0\"\n[{section}]\nhidden = {{ package = {name:?}, path = \"../../external\"{optional} }}\n"));
                let errors = validate(&fixture.metadata()).unwrap_err().join("\n");
                assert!(
                    errors.contains(&format!("forbidden dependency {name}")),
                    "{errors}"
                );
            }
        }
    }

    fn policy_metadata() -> Metadata {
        let root = std::env::temp_dir().join("clapham-policy");
        Metadata {
            workspace_root: root.clone(),
            workspace_members: POLICIES
                .iter()
                .map(|(name, _, _)| (*name).to_owned())
                .collect(),
            packages: POLICIES
                .iter()
                .map(|(name, directory, allowed)| Package {
                    id: (*name).to_owned(),
                    name: (*name).to_owned(),
                    manifest_path: root.join(directory).join("Cargo.toml"),
                    dependencies: allowed
                        .iter()
                        .map(|name| Dependency {
                            name: (*name).to_owned(),
                            ..Dependency::default()
                        })
                        .collect(),
                    features: BTreeMap::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn permits_the_declared_graph_and_independent_external_dependencies() {
        let mut metadata = policy_metadata();
        metadata.packages[0].dependencies.push(Dependency {
            name: "serde".into(),
            ..Dependency::default()
        });
        assert!(validate(&metadata).is_ok());
    }

    #[test]
    fn refuses_every_undeclared_workspace_edge() {
        for (name, _, allowed) in POLICIES {
            for (dependency, _, _) in POLICIES {
                if allowed.contains(dependency) {
                    continue;
                }
                let mut metadata = policy_metadata();
                metadata
                    .packages
                    .iter_mut()
                    .find(|package| package.name == *name)
                    .unwrap()
                    .dependencies
                    .push(Dependency {
                        name: (*dependency).to_owned(),
                        ..Dependency::default()
                    });
                assert!(
                    validate(&metadata).is_err(),
                    "accepted {name} -> {dependency}"
                );
            }
        }
    }

    #[test]
    fn refuses_members_without_a_policy_and_misplaced_members() {
        let mut metadata = policy_metadata();
        metadata.packages[0].name = "surprise".into();
        assert!(validate(&metadata).unwrap_err()[0].contains("no dependency policy"));
        let mut metadata = policy_metadata();
        metadata.packages[0].manifest_path =
            metadata.workspace_root.join("tools/clapham/Cargo.toml");
        assert!(validate(&metadata).unwrap_err()[0].contains("expected its manifest"));
    }
    #[test]
    fn rmcp_server_features_belong_only_to_the_server_member() {
        for feature in RMCP_SERVER_FEATURES {
            for (name, _, _) in POLICIES {
                let mut metadata = policy_metadata();
                metadata
                    .packages
                    .iter_mut()
                    .find(|package| package.name == *name)
                    .unwrap()
                    .dependencies
                    .push(Dependency {
                        name: "rmcp".into(),
                        features: vec![(*feature).into()],
                        ..Dependency::default()
                    });
                assert_eq!(
                    validate(&metadata).is_ok(),
                    *name == "clapham-mcp-server",
                    "{name}: {feature}"
                );
            }
        }
    }

    #[test]
    fn rmcp_server_features_cannot_hide_in_aliases_targets_or_feature_forwarding() {
        for section in [
            "dependencies",
            "dev-dependencies",
            "build-dependencies",
            "target.'cfg(windows)'.dependencies",
        ] {
            for (dependency, feature) in [
                ("default-features = false, features = [\"server\"]", ""),
                ("", ""),
                (
                    "default-features = false",
                    "expose = [\"wire/server-side-http\"]",
                ),
            ] {
                let fixture = Fixture::new();
                fs::write(fixture.0.join("Cargo.toml"), "[workspace]\nmembers = [\"crates/clapham-mcp-client\"]\nexclude = [\"external\"]\nresolver = \"3\"\n").unwrap();
                fixture.crate_at("external", "[package]\nname = \"rmcp\"\nversion = \"3.6.0\"\n[features]\ndefault = [\"server\"]\nserver = []\nserver-side-http = []\n");
                fixture.crate_at("crates/clapham-mcp-client", &format!("[package]\nname = \"clapham-mcp-client\"\nversion = \"0.1.0\"\n[{section}]\nwire = {{ package = \"rmcp\", path = \"../../external\", {dependency} }}\n[features]\n{feature}\n"));
                let errors = validate(&fixture.metadata()).unwrap_err().join("\n");
                assert!(
                    errors.contains("rmcp server features belong to clapham-mcp-server alone"),
                    "{errors}"
                );
            }
        }
        let mut metadata = policy_metadata();
        let client = metadata
            .packages
            .iter_mut()
            .find(|package| package.name == "clapham-mcp-client")
            .unwrap();
        client.dependencies.push(Dependency {
            name: "rmcp".into(),
            rename: Some("wire".into()),
            ..Dependency::default()
        });
        client
            .features
            .insert("optional".into(), vec!["wire?/server".into()]);
        assert!(validate(&metadata).is_err());
    }
}
