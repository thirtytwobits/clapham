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
    Bounds, Call, Cancellation, Contribution, Effect, Evidence, Operation, Outcome, RecordSink,
    Refusal, compose,
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
            oauth_context(
                "operator",
                &["lists:read", "lists:write", "lists:admin", "items:tick"][..],
            ),
        )
        .unwrap(),
        calls,
        after,
    )
}

fn invocation() -> Invocation {
    Invocation::new("lists.action", json!({}))
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
    assert_eq!(refusal.code, "preview_unsupported");
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
fn deadlines_cancel_only_the_call_and_leave_a_shared_host_signal_usable() {
    let (cli, calls, _) = fixture(Effect::Query, false, false);
    let cli = cli.with_deadline(|_| std::future::ready(()));
    let host = Cancellation::new();
    for timeout in [Duration::ZERO, Duration::from_secs(60)] {
        let mut invocation = invocation();
        invocation.timeout = Some(timeout);
        let refusal =
            block_on(cli.call(invocation, RecordSink::default(), host.clone())).unwrap_err();
        assert_eq!(refusal.code, "cancelled");
        assert_eq!(refusal.evidence, Evidence::NotDispatched);
        assert!(refusal.retryable);
        assert!(!host.is_cancelled());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    block_on(cli.call(invocation(), RecordSink::default(), host)).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
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
    let host = Cancellation::new();
    let code = block_on(cli.execute_from(
        ["lists-app", "lists", "action"],
        io::empty(),
        Broken,
        stderr.clone(),
        false,
        host.clone(),
    ));
    assert_eq!(code, 69);
    let refusal = stderr.refusal();
    assert_eq!(refusal.code, "output_failed");
    assert_eq!(refusal.evidence, Evidence::Uncertain);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(after_record.load(Ordering::SeqCst), 0);
    assert!(!host.is_cancelled());
    block_on(cli.call(invocation(), RecordSink::default(), host)).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
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
    for (args, code) in [
        (vec!["lists-app", "lists", "action", "--json", "-"], "usage"),
        (
            vec![
                "lists-app",
                "lists",
                "action",
                "--yes",
                "--dry-run",
                "--json",
                "-",
            ],
            "preview_unsupported",
        ),
    ] {
        let CliError::Refusal(refusal) = cli.parse_from(args, Unread).unwrap_err() else {
            panic!("expected refusal")
        };
        assert_eq!(refusal.code, code);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn prepared_preview_refusals_match_the_core_and_keep_admission_precedence() {
    let operation: Operation = serde_json::from_value(json!({
        "name":"lists.action","description":"Exercise preview refusal precedence.",
        "input":{"type":"object"},"scopes":["lists:admin"],"effect":"administrative","locality":"workspace",
        "idempotent":false,"open_world":false
    }))
    .unwrap();
    let set = compose(
        [Contribution::new("lists").operation(operation, |_: Call| async { unreachable!() })],
        Bounds::default(),
    )
    .unwrap();
    let cli = Cli::new(
        "lists-app",
        set.clone(),
        oauth_context(
            "operator",
            &["lists:read", "lists:write", "lists:admin", "items:tick"][..],
        ),
    )
    .unwrap();
    for (kind, code) in [
        (
            &["lists:read", "lists:write", "lists:admin", "items:tick"][..],
            "preview_unsupported",
        ),
        (&["lists:read", "lists:write"][..], "insufficient_scope"),
    ] {
        let auth = oauth_context("operator", kind);
        let expected = block_on(set.call(
            &set.all(),
            auth.clone(),
            "lists.action",
            json!({}),
            RecordSink::default(),
            Cancellation::new(),
            true,
        ))
        .unwrap_err();
        let cli = Cli::new("lists-app", set.clone(), auth).unwrap();
        let mut invocation = Invocation::new("lists.action", json!({}));
        invocation.yes = true;
        invocation.preview = true;
        let actual =
            block_on(cli.call(invocation, RecordSink::default(), Cancellation::new())).unwrap_err();
        assert_eq!(actual, expected);
        assert_eq!(actual.code, code);
    }
    // Parsing refuses unsupported preview before reading stdin or checking admission.
    let error = cli
        .parse_from(
            ["lists-app", "lists", "action", "--yes", "--dry-run"],
            io::empty(),
        )
        .unwrap_err();
    let expected = block_on(set.call(
        &set.all(),
        oauth_context(
            "operator",
            &["lists:read", "lists:write", "lists:admin", "items:tick"][..],
        ),
        "lists.action",
        json!({}),
        RecordSink::default(),
        Cancellation::new(),
        true,
    ))
    .unwrap_err();
    assert_eq!(error, CliError::Refusal(expected));
}

#[test]
fn host_cancellation_is_forwarded_to_entered_calls_and_process_invocations() {
    for process in [false, true] {
        let operation: Operation = serde_json::from_value(json!({
            "name":"lists.action","description":"Wait for explicit cancellation.",
            "input":{"type":"object"},"effect":"query","locality":"workspace",
            "idempotent":true,"open_world":false
        }))
        .unwrap();
        let entered = Arc::new(AtomicUsize::new(0));
        let observed = entered.clone();
        let set = compose(
            [
                Contribution::new("lists").operation(operation, move |_: Call| {
                    observed.fetch_add(1, Ordering::SeqCst);
                    std::future::pending::<Result<Outcome, Refusal>>()
                }),
            ],
            Bounds::default(),
        )
        .unwrap();
        let cli = Cli::new(
            "lists-app",
            set,
            oauth_context(
                "operator",
                &["lists:read", "lists:write", "lists:admin", "items:tick"][..],
            ),
        )
        .unwrap();
        let host = Cancellation::new();
        let signal = host.clone();
        let mut call: Pin<Box<dyn Future<Output = Refusal>>> = if process {
            Box::pin(async move {
                let stderr = Capture::default();
                let code = cli
                    .execute_from(
                        ["lists-app", "lists", "action"],
                        io::empty(),
                        Capture::default(),
                        stderr.clone(),
                        false,
                        signal,
                    )
                    .await;
                assert_eq!(code, 75);
                stderr.refusal()
            })
        } else {
            Box::pin(async move {
                cli.call(invocation(), RecordSink::default(), signal)
                    .await
                    .unwrap_err()
            })
        };
        let waker = futures_util::task::noop_waker();
        assert!(
            call.as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        assert_eq!(entered.load(Ordering::SeqCst), 1);
        host.cancel();
        let refusal = block_on(call);
        assert_eq!(refusal.code, "cancelled");
        assert_eq!(refusal.evidence, Evidence::Uncertain);
    }
}

fn oauth_context(subject: impl Into<String>, scopes: &[&str]) -> clapham::OAuthContext {
    clapham::OAuthContext::from_verified(
        "https://identity.example.com",
        "https://lists.example.com",
        subject,
        scopes.iter().copied(),
    )
}
