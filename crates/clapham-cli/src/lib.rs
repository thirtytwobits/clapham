#![doc = include_str!("../README.md")]

mod backend;
pub use backend::{Backend, BackendFuture, McpBackend};
mod input;
mod output;
mod process;
mod settings;
mod tree;

use std::ffi::OsString;
use std::fmt;
use std::io::Read;
use std::time::Duration;

use clap::{Command, error::ErrorKind};
use clapham::{
    CallResult, Cancellation, Category, ComposedSet, Locality, OAuthContext, RecordSink, Refusal,
    Selection,
};
use serde_json::Value;

pub use output::{envelope_schema, write_refusal};
pub use settings::{Format, OutputOptions, Shell};

/// A parsed invocation, ready for the ordinary composed-set call runner.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    /// The complete operation name, with command-line aliases resolved.
    pub name: String,
    /// The input object, validated against the operation's schema.
    pub input: Value,
    /// Whether administrative or destructive effects were explicitly confirmed.
    pub yes: bool,
    /// Whether to run a preview rather than commit a change.
    pub preview: bool,
    /// The call budget, starting after confirmation; zero cancels before dispatch.
    pub timeout: Option<Duration>,
    /// Output selection for the process-facing runner.
    pub output: OutputOptions,
}

impl Invocation {
    /// An invocation without confirmation, preview or timeout, with automatic output.
    /// The call runner still validates input and enforces every safeguard.
    pub fn new(name: impl Into<String>, input: Value) -> Self {
        Self {
            name: name.into(),
            input,
            yes: false,
            preview: false,
            timeout: None,
            output: OutputOptions::default(),
        }
    }
}

/// Help is a successful early exit; every other error retains a typed refusal.
#[derive(Debug, Clone, PartialEq)]
pub enum CliError {
    /// Help or a generated completion script, for the host to write to stdout.
    Help(String),
    /// A parsing, configuration or call refusal, for the host to report.
    Refusal(Refusal),
}

impl CliError {
    /// Zero for help, otherwise the refusal category's command-line exit code.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Help(_) => 0,
            Self::Refusal(refusal) => refusal.category.exit_code(),
        }
    }
}

impl From<Refusal> for CliError {
    fn from(refusal: Refusal) -> Self {
        Self::Refusal(refusal)
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Help(text) => formatter.write_str(text),
            Self::Refusal(refusal) => refusal.fmt(formatter),
        }
    }
}

impl std::error::Error for CliError {}

/// A command tree built from one composed set's immutable descriptors.
///
/// Construction does not run handlers or acquire workspace exclusivity. The host
/// acquires that exclusivity before composing its held-workspace contributions.
#[derive(Debug, Clone)]
pub struct Cli {
    set: ComposedSet,
    selection: Selection,
    auth: Option<OAuthContext>,
    backend: Option<std::sync::Arc<dyn Backend>>,
    command: Command,
    tree: tree::Node,
    deadline: settings::Deadline,
    renderers: output::Renderers,
}

impl Cli {
    /// Build nested commands and schema-derived arguments, without dispatching.
    ///
    /// Unrepresentable names and colliding flags refuse as `cli_configuration`.
    /// The host supplies authority established by trusted OAuth middleware.
    pub fn new(
        name: impl Into<String>,
        set: ComposedSet,
        auth: OAuthContext,
    ) -> Result<Self, Refusal> {
        Self::build(name.into(), set, Some(auth), None)
    }

    fn build(
        name: String,
        set: ComposedSet,
        auth: Option<OAuthContext>,
        names: Option<Vec<String>>,
    ) -> Result<Self, Refusal> {
        let selection = if let Some(names) = names {
            set.select(names)?
        } else {
            set.all()
        };
        let tree = tree::Node::from_selection(&selection)?;
        let command = settings::command(tree.command(&name, &set)?);
        Ok(Self {
            selection,
            set,
            auth,
            backend: None,
            command,
            tree,
            deadline: settings::Deadline::default(),
            renderers: output::Renderers::default(),
        })
    }

    /// A detached clap tree for help, inspection or shell completion generation.
    pub fn command(&self) -> Command {
        self.command.clone()
    }

    /// Parse arguments including `argv[0]`, reading `stdin` only for `--json -`.
    ///
    /// Whole-object JSON and property arguments are mutually exclusive. Syntax
    /// and flag type-conversion errors refuse as `usage` (exit 2). Complete input
    /// validation retains the core's `invalid_input` refusal (exit 65).
    /// Help returns [`CliError::Help`] without reading stdin or calling a handler.
    pub fn parse_from<I, T>(&self, args: I, stdin: impl Read) -> Result<Invocation, CliError>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let matches = self.command().try_get_matches_from(args).map_err(|error| {
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                CliError::Help(error.to_string())
            } else {
                CliError::Refusal(usage(error.to_string()))
            }
        })?;
        if let Some(("completions", matches)) = matches.subcommand() {
            let shell = settings::shell(
                matches
                    .get_one::<String>(settings::SHELL)
                    .expect("clap requires a shell"),
            )?;
            return Err(CliError::Help(self.completions(shell)));
        }
        let (node, matches) = self.tree.selected(&matches);
        let name = node
            .operation
            .as_ref()
            .ok_or_else(|| usage("select an operation"))?;
        let operation = self
            .set
            .get(name)
            .expect("the tree uses composed operations");
        let yes = matches.get_flag(settings::YES);
        let preview = matches.get_flag(settings::PREVIEW);
        settings::confirmation(operation.descriptor(), yes)?;
        settings::preview(operation.descriptor(), preview)?;
        let timeout = matches
            .get_one::<String>(settings::TIMEOUT)
            .map(|value| humantime::parse_duration(value).map_err(|error| usage(error.to_string())))
            .transpose()?;
        let output = OutputOptions {
            format: settings::format(
                matches
                    .get_one::<String>(settings::FORMAT)
                    .map(String::as_str),
            ),
            raw: matches.get_flag(settings::RAW),
        };
        let input = input::bind(node, matches, stdin, self.set.bounds().message_bytes)?;
        // Validate before returning a prepared invocation. The call runner
        // validates again because callers may construct or edit Invocation.
        operation.validate_input(&input)?;
        Ok(Invocation {
            name: name.clone(),
            input,
            yes,
            preview,
            timeout,
            output,
        })
    }

    /// Run a prepared invocation through selection, admission and output validation.
    ///
    /// Daemon-local operations always refuse as `no_daemon` on this in-process
    /// adapter, including when the supplied set was composed for a daemon.
    /// Refusal codes and evidence from the call runner are retained unchanged.
    /// Host cancellation is forwarded to a call-local signal; a timeout does not
    /// cancel the host's signal or other calls that share it.
    pub async fn call(
        &self,
        invocation: Invocation,
        records: RecordSink,
        cancel: Cancellation,
    ) -> CallResult {
        if let Some(backend) = &self.backend {
            let operation = self
                .set
                .get(&invocation.name)
                .filter(|_| self.selection.contains(&invocation.name))
                .ok_or_else(|| {
                    Refusal::not_dispatched(
                        "unknown_operation",
                        Category::NotFound,
                        "the operation is not in this CLI selection",
                    )
                })?;
            settings::confirmation(operation.descriptor(), invocation.yes)?;
            settings::preview(operation.descriptor(), invocation.preview)?;
            operation.validate_input(&invocation.input)?;
            if cancel.is_cancelled() {
                return Err(Refusal::not_dispatched(
                    "cancelled",
                    Category::Interrupted,
                    "cancelled before dispatch",
                ));
            }
            if invocation.timeout.is_some_and(|timeout| timeout.is_zero()) {
                return Err(Refusal::not_dispatched(
                    "cancelled",
                    Category::Interrupted,
                    "the call deadline expired before dispatch",
                ));
            }
            let timeout = invocation.timeout;
            let run = backend.call(invocation, records, cancel.clone());
            if let Some(timeout) = timeout {
                use futures_util::future::{Either, select};
                if let Either::Right((result, _)) = select(self.deadline.wait(timeout), run).await {
                    return result;
                }
                return Err(Refusal::new(
                    "cancelled",
                    Category::Interrupted,
                    "the call deadline expired",
                )
                .with_evidence(clapham::Evidence::Uncertain));
            }
            return run.await;
        }
        let call_cancel = Cancellation::new();
        if cancel.is_cancelled() {
            call_cancel.cancel();
        }
        let call = self.set.call(
            &self.selection,
            self.auth.clone().ok_or_else(|| {
                Refusal::not_dispatched(
                    "not_authenticated",
                    Category::Refused,
                    "verified authority is required",
                )
            })?,
            &invocation.name,
            invocation.input,
            records,
            call_cancel.clone(),
            invocation.preview,
        );
        if call_cancel.is_cancelled() {
            return call.await;
        }
        if let Some(operation) = self.set.get(&invocation.name) {
            settings::confirmation(operation.descriptor(), invocation.yes)?;
        }
        if self
            .set
            .get(&invocation.name)
            .is_some_and(|operation| operation.descriptor().locality == Locality::Daemon)
        {
            return Err(Refusal::not_dispatched(
                "no_daemon",
                Category::Unavailable,
                "the operation needs the running daemon",
            ));
        }
        use futures_util::future::{Either, select};
        let stopped = async {
            let Some(duration) = invocation.timeout else {
                cancel.cancelled().await;
                return;
            };
            if !duration.is_zero() {
                let _ = select(Box::pin(cancel.cancelled()), self.deadline.wait(duration)).await;
            }
        };
        match select(Box::pin(stopped), Box::pin(call)).await {
            Either::Left(((), call)) => {
                call_cancel.cancel();
                call.await
            }
            Either::Right((result, _)) => result,
        }
    }

    /// Parse and run one invocation, with records and cancellation supplied by the host.
    ///
    /// This performs no output rendering or process exit. The host retains control
    /// of its executor, output streams and lifetime of the workspace lock.
    pub async fn run_from<I, T>(
        &self,
        args: I,
        stdin: impl Read,
        records: RecordSink,
        cancel: Cancellation,
    ) -> Result<clapham::Outcome, CliError>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let invocation = self.parse_from(args, stdin)?;
        self.call(invocation, records, cancel)
            .await
            .map_err(Into::into)
    }
}

fn usage(message: impl Into<String>) -> Refusal {
    Refusal::not_dispatched("usage", Category::Usage, message)
}

fn configuration(message: impl Into<String>) -> Refusal {
    Refusal::not_dispatched("cli_configuration", Category::Configuration, message)
}
