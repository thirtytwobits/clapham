//! Repository gates, invoked with `cargo xtask`.

mod api;
mod boundaries;
mod schemas;

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() {
    if let Err(error) = dispatch() {
        eprintln!("xtask: {error}");
        std::process::exit(1);
    }
}

fn dispatch() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = root.canonicalize()?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["check"] => check(&root),
        ["boundaries"] => boundaries::check(&root),
        ["api"] => api::check(&root, false),
        ["api", "--update"] => api::check(&root, true),
        ["schemas"] => schemas::run(&root, false),
        ["schemas", "--check"] => schemas::run(&root, true),
        ["help"] | ["--help"] | ["-h"] | [] => {
            println!("cargo xtask <check|boundaries|api [--update]|schemas [--check]>");
            Ok(())
        }
        _ => Err("usage: cargo xtask <check|boundaries|api [--update]|schemas [--check]>".into()),
    }
}

fn cargo(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("cargo");
    // Gates must never inherit the wire test's explicit record-writing mode.
    command
        .current_dir(root)
        .args(args)
        .env_remove("CLAPHAM_WRITE_RECORDS");
    command
}

fn run(command: &mut Command) -> Result<()> {
    eprintln!("{command:?}");
    let status = command.status()?;
    if !status.success() {
        return Err(format!("command failed ({status}): {command:?}").into());
    }
    Ok(())
}

fn capture(command: &mut Command) -> Result<String> {
    eprintln!("{command:?}");
    let output = command.stderr(Stdio::inherit()).output()?;
    if !output.status.success() {
        return Err(format!("command failed ({}): {command:?}", output.status).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn check(root: &Path) -> Result<()> {
    boundaries::check(root)?;
    schemas::run(root, true)?;
    run(&mut cargo(root, &["fmt", "--all", "--", "--check"]))?;
    for features in [None, Some("--all-features")] {
        let mut clippy = cargo(
            root,
            &["clippy", "--locked", "--workspace", "--all-targets"],
        );
        if let Some(features) = features {
            clippy.arg(features);
        }
        run(clippy.args(["--", "-D", "warnings"]))?;

        let mut tests = cargo(root, &["test", "--locked", "--workspace"]);
        if let Some(features) = features {
            tests.arg(features);
        }
        run(&mut tests)?;

        let mut docs = cargo(root, &["doc", "--locked", "--workspace", "--no-deps"]);
        if let Some(features) = features {
            docs.arg(features);
        }
        // Preserve caller flags, but never allow them to weaken the warning gate.
        let flags = format!(
            "{} -D warnings",
            std::env::var("RUSTDOCFLAGS").unwrap_or_default()
        );
        run(docs.env("RUSTDOCFLAGS", flags))?;
    }
    Ok(())
}
