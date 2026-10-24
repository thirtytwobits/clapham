//! Drift checks for the contract's evidence and the packaged Quickstart.

use crate::Result;
use std::fs;
use std::path::Path;

pub(crate) fn check(root: &Path) -> Result<()> {
    let contract = fs::read_to_string(root.join("docs/contract.md"))?;
    let references = evidence(&contract, |link| {
        Ok(fs::read_to_string(root.join("docs").join(link))?)
    })?;
    quickstart(
        &fs::read_to_string(root.join("docs/composing.md"))?,
        &fs::read_to_string(root.join("crates/clapham/README.md"))?,
    )?;
    eprintln!("documentation: {references} evidence references and Quickstart agree");
    Ok(())
}

// This deliberately checks the contract's narrow format: inline .rs links followed by
// backticked function names in Evidence paragraphs. It does not prove test semantics.
fn evidence(contract: &str, mut read: impl FnMut(&str) -> Result<String>) -> Result<usize> {
    let mut guarantee = None;
    for line in contract.lines().chain(std::iter::once("## End")) {
        if let Some(heading) = line.strip_prefix("## ") {
            if let Some(missing) = guarantee.take() {
                return Err(format!("contract section lacks Evidence: {missing}").into());
            }
            if heading
                .strip_prefix('C')
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
            {
                guarantee = Some(heading);
            }
        } else if line.starts_with("Evidence:") {
            guarantee = None;
        }
    }
    let mut references = 0;
    let mut lines = contract.lines();
    while let Some(line) = lines.next() {
        if !line.starts_with("Evidence:") && !line.starts_with("Dependency evidence:") {
            continue;
        }
        let mut paragraph = line.to_owned();
        for line in lines.by_ref() {
            if line.trim().is_empty() {
                break;
            }
            paragraph.push('\n');
            paragraph.push_str(line);
        }
        let mut rest = paragraph.as_str();
        let mut source = None;
        let mut count = 0;
        while let Some(index) = rest.find(['[', '`']) {
            let token = rest.as_bytes()[index];
            rest = &rest[index + 1..];
            if token == b'[' {
                let Some((_, target)) = rest.split_once("](") else {
                    return Err("malformed evidence link".into());
                };
                let Some((link, remaining)) = target.split_once(')') else {
                    return Err("unterminated evidence link".into());
                };
                if link.ends_with(".rs") {
                    source = Some((link, read(link)?));
                } else {
                    source = None;
                }
                rest = remaining;
            } else {
                let Some((name, remaining)) = rest.split_once('`') else {
                    return Err("unterminated evidence function name".into());
                };
                let Some((link, code)) = &source else {
                    return Err(format!("evidence `{name}` needs a preceding .rs link").into());
                };
                if !function_exists(code, name) {
                    return Err(format!("contract evidence `{name}` is missing from {link}").into());
                }
                references += 1;
                count += 1;
                rest = remaining;
            }
        }
        if count == 0 {
            return Err("Evidence paragraph has no function references".into());
        }
    }
    if references == 0 {
        return Err("contract has no evidence references".into());
    }
    Ok(references)
}

fn function_exists(source: &str, name: &str) -> bool {
    // Evidence names point to ordinary private test functions, including async tests.
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && source.lines().any(|line| {
            let line = line.trim_start();
            let line = line.strip_prefix("async ").unwrap_or(line);
            line.strip_prefix("fn ").is_some_and(|declaration| {
                declaration
                    .strip_prefix(name)
                    .is_some_and(|tail| tail.trim_start().starts_with('('))
            })
        })
}

fn quickstart(guide: &str, readme: &str) -> Result<()> {
    let guide = guide.replace("\r\n", "\n");
    let readme = readme.replace("\r\n", "\n");
    let section = readme
        .split_once("## Quickstart\n")
        .ok_or("core README lacks its Quickstart section")?
        .1;
    let section = section.split("\n## ").next().unwrap_or_default();
    if rust_block(&guide)? != rust_block(section)? {
        return Err(
            "docs/composing.md's first Rust example differs from the core README Quickstart".into(),
        );
    }
    Ok(())
}

fn rust_block(markdown: &str) -> Result<String> {
    let mut lines = markdown.split_inclusive('\n');
    if !lines
        .by_ref()
        .any(|line| line.trim_end_matches('\n') == "```rust")
    {
        return Err("missing ```rust example".into());
    }
    let mut code = String::new();
    for line in lines {
        if line.trim_end_matches('\n') == "```" {
            if code.is_empty() {
                return Err("empty Rust example".into());
            }
            return Ok(code);
        }
        code.push_str(line);
    }
    Err("unterminated Rust example".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_resolves_each_name_against_its_preceding_file() {
        let contract = "Dependency evidence: [one](one.rs), `first`.\n\n## C01: Example\n\nEvidence: [one](one.rs), `first`;\n[two](two.rs), `second`. [Guide](guide.md) explains.\n";
        let read = |link: &str| -> Result<String> {
            Ok(match link {
                "one.rs" => "#[test]\nfn first() {}",
                "two.rs" => "#[tokio::test]\nasync fn second() {}",
                _ => return Err("missing file".into()),
            }
            .into())
        };
        assert_eq!(evidence(contract, read).unwrap(), 3);
        assert!(evidence(&contract.replace("`second`", "`first`"), read).is_err());
        assert!(evidence(&contract.replace("two.rs", "missing.rs"), read).is_err());
        assert!(evidence(&contract.replace("`first`", "`fir`"), read).is_err());
    }

    #[test]
    fn evidence_requires_sections_links_and_names() {
        for contract in [
            "## C01: Example\n\nNo evidence.",
            "## C01: Missing\n\n## C02: Present\n\nEvidence: [tests](tests.rs), `unlinked`.",
            "Evidence: `unlinked`.",
            "Evidence: [tests](tests.rs).",
            "Evidence: [tests](tests.rs), `unterminated",
            "Evidence: [broken](tests.rs",
            "No evidence.",
        ] {
            assert!(
                evidence(contract, |_| Ok("fn unlinked() {}".into())).is_err(),
                "{contract}"
            );
        }
    }

    #[test]
    fn quickstart_refuses_drift_or_missing_examples() {
        let guide = "# Guide\n\n```rust\nfn main() {}\n```\n";
        let readme = format!("# Core\n\n## Quickstart\n\n{guide}\n## Next\n");
        quickstart(guide, &readme).unwrap();
        quickstart(&guide.replace('\n', "\r\n"), &readme).unwrap();
        assert!(quickstart(&guide.replace("main", "changed"), &readme).is_err());
        assert!(quickstart(&guide.replace("{}", "{ }"), &readme).is_err());
        assert!(quickstart("", &readme).is_err());
        assert!(quickstart(guide, &readme.replace("## Quickstart", "## Other")).is_err());
        assert!(quickstart(guide, "## Quickstart\n```rust\n").is_err());
        assert!(quickstart(guide, "## Quickstart\n```rust\n```\n").is_err());
    }
}
