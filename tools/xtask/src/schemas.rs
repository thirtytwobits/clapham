use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::Result;

pub(crate) fn run(root: &Path, check: bool) -> Result<()> {
    for (name, schema) in [
        ("operation", clapham::schema::operation_schema()),
        ("refusal", clapham::schema::refusal_schema()),
        ("catalogue", clapham::schema::catalogue_schema()),
    ] {
        let path = root
            .join("crates/clapham/schemas")
            .join(format!("{name}.schema.json"));
        let generated = serde_json::to_string_pretty(&schema)? + "\n";
        sync(&path, &generated, check)?;
    }
    println!("Published schemas are current.");
    Ok(())
}

fn sync(path: &Path, generated: &str, check: bool) -> Result<()> {
    if check {
        let current = match fs::read_to_string(path) {
            Ok(current) => Some(current),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if current.is_none_or(|current| current.replace("\r\n", "\n") != generated) {
            return Err(format!("missing or stale schema: {}\nRegenerate with cargo xtask schemas and review the diff.", path.display()).into());
        }
    } else {
        fs::create_dir_all(path.parent().ok_or("schema has no parent directory")?)?;
        fs::write(path, generated)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_refuses_missing_and_stale_files_without_writing_them() {
        let directory =
            std::env::temp_dir().join(format!("clapham-schemas-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("contract.json");
        let generated = "{\n  \"type\": \"object\"\n}\n";
        assert!(sync(&path, generated, true).is_err());
        assert!(!path.exists());
        sync(&path, generated, false).unwrap();
        assert!(sync(&path, generated, true).is_ok());
        fs::write(&path, "stale\n").unwrap();
        assert!(sync(&path, generated, true).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "stale\n");
        fs::write(&path, generated.replace('\n', "\r\n")).unwrap();
        assert!(sync(&path, generated, true).is_ok());
        fs::remove_dir_all(directory).unwrap();
    }
}
