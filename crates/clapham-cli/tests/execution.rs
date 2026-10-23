//! Safeguards, output failures and explicit deadline futures without timers.

use std::future::Future;
use std::io::{self, Write};
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::task::{Context, Poll};
use std::time::Duration;

use clapham::{
    Bounds, Call, Cancellation, Contribution, Effect, Evidence, Operation, Outcome, Principal,
    PrincipalKind, RecordSink, Refusal, compose,
};
use clapham_cli::{Cli, CliError, Format, Invocation};
use futures_executor::block_on;
use serde_json::json;

fn fixture(
    effect: Effect,
    preview: bool,
    streams: bool,
) -> (Cli, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let operation:Operation = serde_json::from_value(json!({
        "name":"lists.action","description":"Exercise the CLI call boundary.",
        "input":{"type":"object","additionalProperties":false}, "effect":effect,
        "locality":"workspace","preview":preview,"streams":streams,"idempotent":false,"open_world":false
    })).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let after = Arc::new(AtomicUsize::new(0));
    let entered = calls.clone();
    let after_record = after.clone();
    let set = compose(
        [
            Contribution::new("lists").operation(operation, move |call: Call| {
                let entered = entered.clone();
                let after_record = after_record.clone();
                async move {
                    entered.fetch_add(1, Ordering::SeqCst);
                    if streams {
                        call.records.send(json!({"record":1})).await?;
                        after_record.fetch_add(1, Ordering::SeqCst);
                        call.records.send(json!({"record":2})).await?;
                    }
                    Ok(Outcome::new(json!({"preview":call.preview})))
                }
            }),
        ],
        Bounds::default(),
    )
    .unwrap();
    (
        Cli::new(
            "lists-app",
            set,
            Principal::new("operator", PrincipalKind::Human),
        )
        .unwrap(),
        calls,
        after,
    )
}

fn invocation() -> Invocation {
    Invocation::new(
        "lists.action",
        Principal::new("operator", PrincipalKind::Human),
        json!({}),
    )
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Capture {
    fn refusal(&self) -> Refusal {
        serde_json::from_slice(&self.0.lock().unwrap()).unwrap()
    }
}

struct Broken;
impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed output"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn hand_built_invocations_cannot_bypass_confirmation_or_preview_guards() {
    for effect in [Effect::Administrative, Effect::Destructive] {
        let (cli, calls, _) = fixture(effect, false, false);
        let refusal = block_on(cli.call(invocation(), RecordSink::default(), Cancellation::new()))
            .unwrap_err();
        assert_eq!(refusal.code, "usage");
        assert_eq!(refusal.evidence, Evidence::NotDispatched);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let mut confirmed = invocation();
        confirmed.yes = true;
        block_on(cli.call(confirmed, RecordSink::default(), Cancellation::new())).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    let (cli, calls, _) = fixture(Effect::Mutation, false, false);
    let mut preview = invocation();
    preview.preview = true;
    let refusal =
        block_on(cli.call(preview, RecordSink::default(), Cancellation::new())).unwrap_err();
    assert_eq!(refusal.code, "usage");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (cli, _, _) = fixture(Effect::Mutation, true, false);
    let mut preview = invocation();
    preview.preview = true;
    assert_eq!(
        block_on(cli.call(preview, RecordSink::default(), Cancellation::new()))
            .unwrap()
            .result,
        Some(json!({"preview":true}))
    );
}

#[test]
fn controls_bind_without_reading_a_clock_or_starting_a_timer() {
    let (cli, _, _) = fixture(Effect::Mutation, true, false);
    let parsed = cli
        .parse_from(
            [
                "lists-app",
                "--format",
                "ndjson",
                "lists",
                "action",
                "--raw",
                "--dry-run",
                "--timeout",
                "1.25s",
            ],
            io::empty(),
        )
        .unwrap();
    assert_eq!(parsed.timeout, Some(Duration::from_millis(1250)));
    assert_eq!(parsed.output.format, Format::Ndjson);
    assert!(parsed.output.raw);
    assert!(parsed.preview);
    for args in [
        vec!["lists-app", "lists", "action", "--timeout", "later"],
        vec!["lists-app", "lists", "action", "--format", "xml"],
        vec!["lists-app", "completions", "unknown"],
    ] {
        assert_eq!(
            cli.parse_from(args, io::empty()).unwrap_err().exit_code(),
            2
        );
    }
}

#[test]
fn zero_and_explicitly_ready_deadlines_cancel_before_dispatch() {
    let (cli, calls, _) = fixture(Effect::Query, false, false);
    let cli = cli.with_deadline(|_| std::future::ready(()));
    for timeout in [Duration::ZERO, Duration::from_secs(60)] {
        let mut invocation = invocation();
        invocation.timeout = Some(timeout);
        let refusal =
            block_on(cli.call(invocation, RecordSink::default(), Cancellation::new())).unwrap_err();
        assert_eq!(refusal.code, "cancelled");
        assert_eq!(refusal.evidence, Evidence::NotDispatched);
        assert!(refusal.retryable);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

struct HeldDeadline(Arc<AtomicBool>);
impl Future for HeldDeadline {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }
}
impl Drop for HeldDeadline {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn completing_a_call_drops_its_deadline_future() {
    let (cli, calls, _) = fixture(Effect::Query, false, false);
    let dropped = Arc::new(AtomicBool::new(false));
    let observed = dropped.clone();
    let cli = cli.with_deadline(move |_| HeldDeadline(observed.clone()));
    let mut invocation = invocation();
    invocation.timeout = Some(Duration::from_secs(60));
    block_on(cli.call(invocation, RecordSink::default(), Cancellation::new())).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(dropped.load(Ordering::SeqCst));
}

#[test]
fn a_failed_record_write_cancels_the_producer_and_keeps_uncertain_evidence() {
    let (cli, calls, after_record) = fixture(Effect::Mutation, false, true);
    let stderr = Capture::default();
    let code = block_on(cli.execute_from(
        ["lists-app", "lists", "action"],
        io::empty(),
        Broken,
        stderr.clone(),
        false,
        Cancellation::new(),
    ));
    assert_eq!(code, 69);
    let refusal = stderr.refusal();
    assert_eq!(refusal.code, "output_failed");
    assert_eq!(refusal.evidence, Evidence::Uncertain);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(after_record.load(Ordering::SeqCst), 0);
}

#[test]
fn failed_result_and_help_delivery_distinguish_completion_from_no_dispatch() {
    let (cli, calls, _) = fixture(Effect::Mutation, false, false);
    for (args, evidence, entered) in [
        (vec!["lists-app", "lists", "action"], Evidence::Completed, 1),
        (
            vec!["lists-app", "lists", "action", "--help"],
            Evidence::NotDispatched,
            1,
        ),
    ] {
        let stderr = Capture::default();
        let code = block_on(cli.execute_from(
            args,
            io::empty(),
            Broken,
            stderr.clone(),
            false,
            Cancellation::new(),
        ));
        assert_eq!(code, 69);
        assert_eq!(stderr.refusal().evidence, evidence);
        assert_eq!(calls.load(Ordering::SeqCst), entered);
    }
    let refusal = Refusal::not_dispatched("unchanged", clapham::Category::Conflict, "conflict");
    assert_eq!(clapham_cli::write_refusal(&refusal, Broken), 67);
}

#[test]
fn confirmations_and_dry_runs_are_checked_before_stdin_is_read() {
    struct Unread;
    impl io::Read for Unread {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("stdin must not be read")
        }
    }
    let (cli, calls, _) = fixture(Effect::Destructive, false, false);
    for args in [
        vec!["lists-app", "lists", "action", "--json", "-"],
        vec![
            "lists-app",
            "lists",
            "action",
            "--yes",
            "--dry-run",
            "--json",
            "-",
        ],
    ] {
        let CliError::Refusal(refusal) = cli.parse_from(args, Unread).unwrap_err() else {
            panic!("expected refusal")
        };
        assert_eq!(refusal.code, "usage");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
