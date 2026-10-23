//! Process fixture for the real CLI runner, seeded afresh for each invocation.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use clapham::{
    Bounds, Call, Cancellation, Category, ComposeOptions, ContentBlock, Contribution, Evidence,
    Locality, Operation, Outcome, Refusal, compose_with_options,
};
use clapham_cli::{Cli, process_principal};
use clapham_conformance::{ListsGate, ListsHandle};
use futures_channel::oneshot;
use futures_executor::block_on;
use futures_util::future::join;
use serde_json::json;

fn probe(name: &str, locality: Locality) -> Operation {
    let mut descriptor: Operation = serde_json::from_value(json!({
        "name":name,"description":"Exercise a process output boundary.",
        "input":{"type":"object","additionalProperties":false},
        "effect":"query","locality":locality,"idempotent":true,"open_world":false
    }))
    .unwrap();
    descriptor
        .refusals
        .insert("fixture_configuration".into(), Category::Configuration);
    descriptor
}

fn main() -> ExitCode {
    let handle = ListsHandle::new();
    handle.create_list("tasks", "Tasks").unwrap();
    handle.add_item("tasks", "a", "Read").unwrap();
    handle.add_item("tasks", "b", "Write").unwrap();
    let mut contributions: Vec<_> = handle.contributions().into_iter().collect();
    contributions.push(
        Contribution::new("probe")
            .operation(
                probe("probe.configuration", Locality::Workspace),
                |_: Call| async {
                    Err(Refusal::new(
                        "fixture_configuration",
                        Category::Configuration,
                        "The fixture configuration is invalid.",
                    ))
                },
            )
            .operation(probe("probe.daemon", Locality::Daemon), |_: Call| async {
                unreachable!()
            })
            .operation(
                probe("probe.outcome", Locality::Workspace),
                |_: Call| async {
                    Ok(Outcome {
                        result: Some(json!({"answer":42})),
                        content: Some(vec![ContentBlock::Text {
                            text: "Explanation".into(),
                        }]),
                        is_error: true,
                        evidence: Evidence::Uncertain,
                    })
                },
            ),
    );
    let set = compose_with_options(
        contributions,
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(ListsGate),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    let mut cli = Cli::new("lists-app", set, process_principal().unwrap()).unwrap();
    if std::env::var_os("CLAPHAM_FIXTURE_CUSTOM").is_some() {
        cli = cli
            .with_renderer("lists.get", |_, data, _| {
                format!("List: {}", data["name"].as_str().unwrap())
            })
            .unwrap();
        cli = cli
            .with_renderer("lists.export", |_, data, record| {
                if record {
                    format!("Item: {}", data["text"].as_str().unwrap())
                } else {
                    format!("Count: {}", data["count"])
                }
            })
            .unwrap();
    }
    let terminal = std::env::var_os("CLAPHAM_FIXTURE_TERMINAL").is_some();
    let code = if std::env::var_os("CLAPHAM_FIXTURE_DEADLINE").is_some() {
        let mut held = handle.hold("lists.rename").unwrap();
        let (expire, expired) = oneshot::channel();
        let expired = Arc::new(Mutex::new(Some(expired)));
        cli = cli.with_deadline(move |_| {
            let expired = expired.lock().unwrap().take().unwrap();
            async move {
                let _ = expired.await;
            }
        });
        let control = async move {
            held.entered().await;
            expire.send(()).unwrap();
            held
        };
        let (held, code) = block_on(join(
            control,
            cli.execute_from(
                std::env::args_os(),
                std::io::stdin(),
                std::io::stdout(),
                std::io::stderr(),
                terminal,
                Cancellation::new(),
            ),
        ));
        drop(held);
        code
    } else if terminal {
        block_on(cli.execute_from(
            std::env::args_os(),
            std::io::stdin(),
            std::io::stdout(),
            std::io::stderr(),
            true,
            Cancellation::new(),
        ))
    } else {
        block_on(cli.run())
    };
    if let Some(path) = std::env::var_os("CLAPHAM_FIXTURE_STATE") {
        std::fs::write(path, serde_json::to_vec(&handle.snapshot()).unwrap()).unwrap();
    }
    ExitCode::from(code)
}
