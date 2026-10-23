#![doc = include_str!("../README.md")]

mod input;
mod tree;

use std::ffi::OsString;
use std::fmt;
use std::io::Read;

use clap::{Command, error::ErrorKind};
use clapham::{
    CallResult, Cancellation, Category, ComposedSet, Locality, Principal, PrincipalKind,
    RecordSink, Refusal, Selection,
};
use serde_json::Value;

/// A parsed invocation, ready for the ordinary composed-set call runner.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    /// The complete operation name, with command-line aliases resolved.
    pub name: String,
    /// The supplied principal, or the identity selected by `--as`.
    pub principal: Principal,
    /// The input object, validated against the operation's schema.
    pub input: Value,
}

/// Help is a successful early exit; every other error retains a typed refusal.
#[derive(Debug, Clone, PartialEq)]
pub enum CliError {
    /// Descriptor-derived help, for the host to write to stdout.
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
    principal: Principal,
    command: Command,
    tree: tree::Node,
}

impl Cli {
    /// Build nested commands and schema-derived arguments, without dispatching.
    ///
    /// Unrepresentable names and colliding flags refuse as `cli_configuration`.
    /// Supply [`process_principal`] for a normal process's default operator.
    pub fn new(
        name: impl Into<String>,
        set: ComposedSet,
        principal: Principal,
    ) -> Result<Self, Refusal> {
        let name = name.into();
        let tree = tree::Node::from_set(&set)?;
        let command = tree.command(&name, &set)?.arg(
            clap::Arg::new(tree::AS)
                .long("as")
                .global(true)
                .value_name("KIND:ID")
                .help("Run as human:<id>, agent:<id>, service:<id> or system:<id>"),
        );
        Ok(Self {
            selection: set.all(),
            set,
            principal,
            command,
            tree,
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
        let principal = matches
            .get_one::<String>(tree::AS)
            .map(|value| input::principal(value))
            .transpose()?
            .unwrap_or_else(|| self.principal.clone());
        let (node, matches) = self.tree.selected(&matches);
        let name = node
            .operation
            .as_ref()
            .ok_or_else(|| usage("select an operation"))?;
        let operation = self
            .set
            .get(name)
            .expect("the tree uses composed operations");
        let input = input::bind(node, matches, stdin, self.set.bounds().message_bytes)?;
        // Validate before returning a prepared invocation. The call runner
        // validates again because callers may construct or edit Invocation.
        operation.validate_input(&input)?;
        Ok(Invocation {
            name: name.clone(),
            principal,
            input,
        })
    }

    /// Run a prepared invocation through selection, admission and output validation.
    ///
    /// Daemon-local operations always refuse as `no_daemon` on this in-process
    /// adapter, including when the supplied set was composed for a daemon.
    /// Refusal codes and evidence from the call runner are retained unchanged.
    pub async fn call(
        &self,
        invocation: Invocation,
        records: RecordSink,
        cancel: Cancellation,
    ) -> CallResult {
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
        self.set
            .call(
                &self.selection,
                invocation.principal,
                &invocation.name,
                invocation.input,
                records,
                cancel,
                false,
            )
            .await
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

/// The process account as a human principal, obtained from the operating system.
///
/// Failure to resolve a non-empty account refuses as `cli_configuration`; no
/// anonymous or environment-variable fallback is silently substituted.
pub fn process_principal() -> Result<Principal, Refusal> {
    let account = whoami::username().map_err(|error| configuration(error.to_string()))?;
    if account.is_empty() {
        return Err(configuration("the process account is empty"));
    }
    Ok(Principal::new(account, PrincipalKind::Human))
}

fn usage(message: impl Into<String>) -> Refusal {
    Refusal::not_dispatched("usage", Category::Usage, message)
}

fn configuration(message: impl Into<String>) -> Refusal {
    Refusal::not_dispatched("cli_configuration", Category::Configuration, message)
}
