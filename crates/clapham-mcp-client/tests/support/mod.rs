#![allow(dead_code)]
use clapham::*;
use clapham_mcp_client::McpToolBinding;
use serde_json::Value;
use std::sync::atomic::Ordering;

pub struct Counter(tokio::sync::watch::Sender<usize>);
impl Counter {
    pub fn new(value: usize) -> Self {
        Self(tokio::sync::watch::channel(value).0)
    }
    pub fn fetch_add(&self, value: usize, _: Ordering) {
        self.0.send_modify(|count| *count += value);
    }
    pub fn load(&self, _: Ordering) -> usize {
        *self.0.borrow()
    }
    pub async fn wait(&self, expected: usize) {
        self.0
            .subscribe()
            .wait_for(|count| *count >= expected)
            .await
            .unwrap();
    }
}
#[derive(Clone)]
pub struct Executor(ComposedSet);
pub trait BindingExt {
    fn executor(&self) -> Executor;
}
impl BindingExt for McpToolBinding {
    fn executor(&self) -> Executor {
        Executor(
            compose_with_options(
                [self.contribution()],
                Bounds::default(),
                ComposeOptions {
                    execution: ExecutionMode::Daemon,
                    ..Default::default()
                },
            )
            .unwrap(),
        )
    }
}
impl Executor {
    pub async fn call(&self, call: Call) -> Result<Outcome, Refusal> {
        self.0
            .call(
                &self.0.all(),
                call.principal,
                call.name.as_str(),
                call.input,
                call.records,
                call.cancel,
                call.preview,
            )
            .await
    }
}
pub fn selected(name: &str, kinds: Vec<PrincipalKind>) -> clapham_mcp_client::McpSelection {
    let mut selection = clapham_mcp_client::McpSelection::new(name);
    selection.principals = Some(kinds);
    selection
}
pub fn call(name: &str, input: Value) -> Call {
    Call::new(
        OperationName::new(name).unwrap(),
        Principal::new("host", PrincipalKind::Human),
        input,
    )
}
