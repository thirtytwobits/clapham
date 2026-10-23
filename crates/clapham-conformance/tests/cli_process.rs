//! Process streams and state evidence, without elapsed-time synchronisation.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use clapham::{Evidence, Refusal};
use serde_json::{Value, json};

struct StateFile(PathBuf);
impl StateFile {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        Self(std::env::temp_dir().join(format!(
            "clapham-cli-state-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for StateFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn run(
    args: &[&str],
    terminal: bool,
    custom: bool,
    deadline: bool,
    stdin: &[u8],
) -> (Output, Value) {
    let state = StateFile::new();
    let mut command = Command::new(env!("CARGO_BIN_EXE_clapham-cli-fixture"));
    command
        .args(args)
        .env("CLAPHAM_FIXTURE_STATE", &state.0)
        .env_remove("CLAPHAM_FIXTURE_TERMINAL")
        .env_remove("CLAPHAM_FIXTURE_CUSTOM")
        .env_remove("CLAPHAM_FIXTURE_DEADLINE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, enabled) in [
        ("CLAPHAM_FIXTURE_TERMINAL", terminal),
        ("CLAPHAM_FIXTURE_CUSTOM", custom),
        ("CLAPHAM_FIXTURE_DEADLINE", deadline),
    ] {
        if enabled {
            command.env(name, "1");
        }
    }
    let mut child = command.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let output = child.wait_with_output().unwrap();
    let state: Value = serde_json::from_slice(&std::fs::read(&state.0).unwrap()).unwrap();
    (output, state)
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
}

fn frames(output: &Output) -> Vec<Value> {
    let schema: Value = serde_json::from_str(include_str!(
        "../../clapham-cli/schemas/envelope.schema.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| {
            let frame: Value = serde_json::from_str(line).unwrap();
            validator.validate(&frame).unwrap();
            frame
        })
        .collect()
}

#[test]
fn piped_export_flushes_ordered_frames_then_one_result() {
    let (output, _) = run(
        &["lists", "export", "--id", "tasks"],
        false,
        false,
        false,
        &[],
    );
    success(&output);
    let frames = frames(&output);
    assert_eq!(
        frames,
        vec![
            json!({"v":1,"kind":"lists.export","data":{"id":"a","text":"Read","ticked":false}}),
            json!({"v":1,"kind":"lists.export","data":{"id":"b","text":"Write","ticked":false}}),
            json!({"v":1,"kind":"lists.export","data":{"count":2}}),
        ]
    );
    for format in ["json", "ndjson"] {
        let (output, _) = run(
            &["lists", "export", "--id", "tasks", "--format", format],
            true,
            false,
            false,
            &[],
        );
        success(&output);
        assert_eq!(self::frames(&output), frames);
    }
}

#[test]
fn raw_output_drops_only_the_envelope_and_json_stdin_runs() {
    let (output, _) = run(
        &["lists", "export", "--id", "tasks", "--raw"],
        false,
        false,
        false,
        &[],
    );
    success(&output);
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        lines,
        vec![
            json!({"id":"a","text":"Read","ticked":false}),
            json!({"id":"b","text":"Write","ticked":false}),
            json!({"count":2})
        ]
    );
    let (output, _) = run(
        &["lists", "get", "--json", "-"],
        false,
        false,
        false,
        br#"{"id":"tasks"}"#,
    );
    success(&output);
    assert_eq!(frames(&output)[0]["data"]["name"], "Tasks");
}

#[test]
fn terminal_defaults_to_human_and_explicit_json_is_pretty() {
    let (output, _) = run(&["lists", "get", "tasks"], true, false, false, &[]);
    success(&output);
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("name: Tasks\n")
    );
    let (output, _) = run(&["lists", "list"], true, false, false, &[]);
    success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.lines().next().unwrap().contains("revision"));
    assert!(text.contains("Tasks"));
    assert!(text.lines().nth(1).unwrap().contains("---"));
    let (output, _) = run(
        &["lists", "get", "tasks", "--format", "json"],
        true,
        false,
        false,
        &[],
    );
    success(&output);
    assert!(output.stdout.starts_with(b"{\n"));
    let frame: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(frame["kind"], "lists.get");
}

#[test]
fn human_stream_records_and_custom_renderers_work_as_process_output() {
    let (output, _) = run(
        &["lists", "export", "--id", "tasks"],
        true,
        false,
        false,
        &[],
    );
    success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("text: Read"));
    assert!(text.contains("text: Write"));
    assert!(text.ends_with("count: 2\n"));
    let (output, _) = run(
        &["lists", "export", "--id", "tasks", "--format", "human"],
        false,
        true,
        false,
        &[],
    );
    success(&output);
    assert_eq!(output.stdout, b"Item: Read\nItem: Write\nCount: 2\n");
    let (output, _) = run(
        &["lists", "get", "tasks", "--format", "human"],
        false,
        true,
        false,
        &[],
    );
    success(&output);
    assert_eq!(output.stdout, b"List: Tasks\n");
    let (output, _) = run(&["lists", "get", "tasks"], false, true, false, &[]);
    success(&output);
    assert_eq!(frames(&output)[0]["data"]["name"], "Tasks");
}

#[test]
fn every_refusal_category_uses_one_json_stderr_line_and_its_exit_code() {
    for (args, code, exit) in [
        (vec!["lists", "delete", "--id", "tasks"], "usage", 2),
        (vec!["--as", "agent:bot", "lists", "compact"], "usage", 2),
        (vec!["lists", "create", "--json", "{}"], "invalid_input", 65),
        (vec!["lists", "get", "absent"], "list_not_found", 66),
        (
            vec![
                "lists",
                "rename",
                "--id",
                "tasks",
                "--name",
                "New",
                "--revision",
                "0",
            ],
            "list_changed",
            67,
        ),
        (
            vec!["--as", "agent:bot", "lists", "compact", "--yes"],
            "not_admitted",
            77,
        ),
        (vec!["probe", "daemon"], "no_daemon", 69),
        (
            vec!["lists", "get", "tasks", "--timeout", "0s"],
            "cancelled",
            75,
        ),
        (vec!["probe", "configuration"], "fixture_configuration", 78),
        (vec!["lists", "fault"], "undeclared_refusal", 70),
    ] {
        let (output, state) = run(&args, false, false, false, &[]);
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(stderr.lines().count(), 1);
        let refusal: Refusal = serde_json::from_str(&stderr).unwrap();
        assert_eq!(refusal.code, code);
        assert_eq!(i32::from(refusal.category.exit_code()), exit);
        assert_eq!(state[0]["name"], "Tasks");
    }
}

#[test]
fn published_envelope_schema_keeps_the_root_dialect_and_exact_name_constraints() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../clapham-cli/schemas/envelope.schema.json"
    ))
    .unwrap();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert!(schema["properties"]["kind"].get("$schema").is_none());
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&json!({"v":1,"kind":"lists.get","data":null})));
    for frame in [
        json!({"v":2,"kind":"lists.get","data":null}),
        json!({"v":1,"kind":"lists.get\n","data":null}),
        json!({"v":1,"kind":"","data":null}),
    ] {
        assert!(!validator.is_valid(&frame));
    }
}

#[test]
fn confirmation_and_preview_are_enforced_before_effects() {
    let (output, state) = run(
        &["lists", "delete", "--id", "tasks"],
        false,
        false,
        false,
        &[],
    );
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(state.as_array().unwrap().len(), 1);
    let (output, state) = run(
        &["lists", "delete", "--id", "tasks", "--yes"],
        false,
        false,
        false,
        &[],
    );
    success(&output);
    assert_eq!(state, json!([]));
    let (output, state) = run(
        &[
            "lists",
            "create",
            "--list-json",
            r#"{"id":"preview","name":"Preview"}"#,
            "--dry-run",
        ],
        false,
        false,
        false,
        &[],
    );
    success(&output);
    assert_eq!(frames(&output)[0]["data"]["id"], "preview");
    assert_eq!(state.as_array().unwrap().len(), 1);
    let (output, state) = run(
        &["lists", "delete", "--id", "tasks", "--yes", "--dry-run"],
        false,
        false,
        false,
        &[],
    );
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(state.as_array().unwrap().len(), 1);
    let (output, _) = run(&["lists", "compact"], false, false, false, &[]);
    assert_eq!(output.status.code(), Some(2));
    let (output, _) = run(&["lists", "compact", "--yes"], false, false, false, &[]);
    success(&output);
}

#[test]
fn scripted_deadline_cancels_an_entered_write_without_a_timer() {
    let (output, state) = run(
        &[
            "lists",
            "rename",
            "--id",
            "tasks",
            "--name",
            "New",
            "--revision",
            "3",
            "--timeout",
            "1h",
        ],
        false,
        false,
        true,
        &[],
    );
    assert_eq!(output.status.code(), Some(75));
    assert!(output.stdout.is_empty());
    let refusal: Refusal = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(refusal.code, "cancelled");
    assert_eq!(refusal.evidence, Evidence::Uncertain);
    assert!(refusal.retryable);
    assert_eq!(state[0]["name"], "Tasks");
}

#[test]
fn completion_scripts_include_the_live_tree_and_controls_without_dispatch() {
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let (output, state) = run(&["completions", shell], false, false, false, &[]);
        success(&output);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("lists-app"), "{shell}");
        for word in ["lists", "export", "dry-run", "timeout", "format"] {
            assert!(text.contains(word), "{shell}: {word}");
        }
        assert_eq!(state[0]["name"], "Tasks");
    }
}

#[test]
fn nonordinary_outcome_content_error_and_evidence_survive_rendering() {
    let (output, _) = run(&["probe", "outcome"], false, false, false, &[]);
    success(&output);
    let data = &frames(&output)[0]["data"];
    assert_eq!(data["result"], json!({"answer":42}));
    assert_eq!(
        data["content"][0],
        json!({"type":"text","text":"Explanation"})
    );
    assert_eq!(data["is_error"], true);
    assert_eq!(data["evidence"], "uncertain");
}
