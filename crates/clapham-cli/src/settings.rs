use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use clap::{Arg, ArgAction, Command};
use clapham::{Category, Effect, Operation, Refusal};

use crate::{Cli, usage};

pub(crate) const YES: &str = "cli:yes";
pub(crate) const PREVIEW: &str = "cli:preview";
pub(crate) const TIMEOUT: &str = "cli:timeout";
pub(crate) const FORMAT: &str = "cli:format";
pub(crate) const RAW: &str = "cli:raw";
pub(crate) const SHELL: &str = "cli:shell";

/// How successful records and results are rendered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Format {
    /// Human output on a terminal, otherwise compact NDJSON.
    #[default]
    Auto,
    /// JSON, pretty on a terminal; streaming operations always use one line per frame.
    Json,
    /// One compact JSON frame per line.
    Ndjson,
    /// Schema-derived tables and fields, or the operation's registered renderer.
    Human,
}

/// Output settings independent of the invocation's effects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputOptions {
    /// The requested representation; automatic selection uses stdout's terminal status.
    pub format: Format,
    /// Omit the versioned envelope for JSON records and results.
    pub raw: bool,
}

/// Supported completion script targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// Bash.
    Bash,
    /// Zsh.
    Zsh,
    /// Fish.
    Fish,
    /// PowerShell.
    PowerShell,
}

pub(crate) fn shell(value: &str) -> Result<Shell, Refusal> {
    match value {
        "bash" => Ok(Shell::Bash),
        "zsh" => Ok(Shell::Zsh),
        "fish" => Ok(Shell::Fish),
        "powershell" => Ok(Shell::PowerShell),
        _ => Err(usage("unsupported shell")),
    }
}

pub(crate) fn format(value: Option<&str>) -> Format {
    match value {
        Some("json") => Format::Json,
        Some("ndjson") => Format::Ndjson,
        Some("human") => Format::Human,
        _ => Format::Auto,
    }
}

pub(crate) fn command(command: Command) -> Command {
    command
        .arg(
            Arg::new(YES)
                .long("yes")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Confirm administrative or destructive effects"),
        )
        .arg(
            Arg::new(PREVIEW)
                .long("dry-run")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Preview an operation that supports dry runs"),
        )
        .arg(
            Arg::new(TIMEOUT)
                .long("timeout")
                .global(true)
                .value_name("DURATION")
                .help("Cancel the call after a duration, such as 500ms or 2m"),
        )
        .arg(
            Arg::new(FORMAT)
                .long("format")
                .global(true)
                .value_parser(["json", "ndjson", "human"])
                .help("Select result and record formatting"),
        )
        .arg(
            Arg::new(RAW)
                .long("raw")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Omit the JSON output envelope"),
        )
        .subcommand(
            Command::new("completions")
                .about("Generate a shell completion script")
                .arg(Arg::new(SHELL).required(true).value_parser([
                    "bash",
                    "zsh",
                    "fish",
                    "powershell",
                ])),
        )
}

pub(crate) fn confirmation(operation: &Operation, yes: bool) -> Result<(), Refusal> {
    if matches!(
        operation.effect,
        Effect::Administrative | Effect::Destructive
    ) && !yes
    {
        return Err(usage(
            "administrative and destructive operations require --yes",
        ));
    }
    Ok(())
}

pub(crate) fn preview(operation: &Operation, preview: bool) -> Result<(), Refusal> {
    if preview && !operation.preview {
        return Err(Refusal::not_dispatched(
            "preview_unsupported",
            Category::Usage,
            "the operation does not support a dry run",
        ));
    }
    Ok(())
}

type Wait = Pin<Box<dyn Future<Output = ()> + Send>>;

#[derive(Clone)]
pub(crate) struct Deadline(Arc<dyn Fn(Duration) -> Wait + Send + Sync>);

impl Default for Deadline {
    fn default() -> Self {
        Self(Arc::new(|duration| {
            Box::pin(futures_timer::Delay::new(duration))
        }))
    }
}

impl fmt::Debug for Deadline {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Deadline { .. }")
    }
}

impl Deadline {
    pub(crate) fn wait(&self, duration: Duration) -> Wait {
        (self.0)(duration)
    }
}

impl Cli {
    /// Use a host's deadline future, for runtime integration or explicitly scripted tests.
    /// The factory is called only for a non-zero timeout, once per call, and its
    /// future completes when the deadline expires. Dropping it must stop the wait.
    pub fn with_deadline<F, Fut>(mut self, wait: F) -> Self
    where
        F: Fn(Duration) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.deadline = Deadline(Arc::new(move |duration| Box::pin(wait(duration))));
        self
    }

    /// Generate completions from the same tree used to parse calls, without dispatch.
    pub fn completions(&self, shell: Shell) -> String {
        let shell = match shell {
            Shell::Bash => clap_complete::Shell::Bash,
            Shell::Zsh => clap_complete::Shell::Zsh,
            Shell::Fish => clap_complete::Shell::Fish,
            Shell::PowerShell => clap_complete::Shell::PowerShell,
        };
        let mut command = self.command();
        let name = command.get_name().to_owned();
        let mut output = Vec::new();
        clap_complete::generate(shell, &mut command, name, &mut output);
        String::from_utf8(output).expect("completion generators write UTF-8")
    }
}
