use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};
use std::sync::{Arc, Mutex};

use clapham::{Cancellation, Evidence, RecordSink, Refusal};
use futures_util::future::{Either, select};

use crate::{
    Cli, CliError,
    output::{self, Frame, Presentation},
};

struct Output<W> {
    writer: W,
    failure: Option<Refusal>,
}

impl Cli {
    /// Run process arguments using stdin/stdout/stderr and stdout's terminal status.
    /// Return the exit code to the host; this never terminates the process itself.
    pub async fn run(&self) -> u8 {
        self.execute_from(
            std::env::args_os(),
            io::stdin(),
            io::stdout(),
            io::stderr(),
            io::stdout().is_terminal(),
            Cancellation::new(),
        )
        .await
    }

    /// Parse, enforce safeguards, dispatch and write one process-facing invocation.
    /// Help/completions use stdout, refusals use one unchanged JSON line on stderr.
    /// Record writes are flushed before acknowledgement. A failed record write
    /// cancels dispatch and reports `output_failed` with uncertain evidence.
    /// Delivery failure and timeout cancel only this invocation, not the host signal.
    /// `terminal` describes stdout and can be supplied by an embedding host.
    pub async fn execute_from<I, T, W>(
        &self,
        args: I,
        stdin: impl Read,
        stdout: W,
        stderr: impl Write,
        terminal: bool,
        cancel: Cancellation,
    ) -> u8
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
        W: Write + Send + 'static,
    {
        let output = Arc::new(Mutex::new(Output {
            writer: stdout,
            failure: None,
        }));
        let invocation = match self.parse_from(args, stdin) {
            Ok(invocation) => invocation,
            Err(CliError::Help(text)) => {
                let bytes = if text.ends_with('\n') {
                    text.into_bytes()
                } else {
                    format!("{text}\n").into_bytes()
                };
                return match output::write_frame(
                    &mut output.lock().expect("output poisoned").writer,
                    &bytes,
                ) {
                    Ok(()) => 0,
                    Err(error) => output::write_refusal(
                        &output::output_failed(error, Evidence::NotDispatched),
                        stderr,
                    ),
                };
            }
            Err(CliError::Refusal(refusal)) => return output::write_refusal(&refusal, stderr),
        };
        let presentation = Arc::new(Presentation {
            operation: self
                .set
                .get(&invocation.name)
                .expect("the parser selected a composed operation")
                .descriptor()
                .clone(),
            options: invocation.output,
            terminal,
            renderers: self.renderers.clone(),
        });
        let call_cancel = Cancellation::new();
        if cancel.is_cancelled() {
            call_cancel.cancel();
        }
        let records = {
            let output = output.clone();
            let presentation = presentation.clone();
            let cancel = call_cancel.clone();
            RecordSink::new(move |record| {
                let output = output.clone();
                let presentation = presentation.clone();
                let cancel = cancel.clone();
                async move {
                    let bytes = presentation.render(Frame::Record(record));
                    let failed = {
                        let mut output = output.lock().expect("output poisoned");
                        match output::write_frame(&mut output.writer, &bytes) {
                            Ok(()) => false,
                            Err(error) => {
                                output.failure =
                                    Some(output::output_failed(error, Evidence::Uncertain));
                                true
                            }
                        }
                    };
                    if failed {
                        cancel.cancel();
                        // Yield back to the call runner so cancellation stops the
                        // producer before another record or effect can be attempted.
                        std::future::pending::<()>().await;
                    }
                    Ok(())
                }
            })
        };
        let call = Box::pin(self.call(invocation, records, call_cancel.clone()));
        let result = match select(Box::pin(cancel.cancelled()), call).await {
            Either::Left(((), call)) => {
                call_cancel.cancel();
                call.await
            }
            Either::Right((result, _)) => result,
        };
        let mut output = output.lock().expect("output poisoned");
        if let Some(refusal) = output.failure.take() {
            return output::write_refusal(&refusal, stderr);
        }
        match result {
            Err(refusal) => output::write_refusal(&refusal, stderr),
            Ok(outcome) => {
                let evidence = outcome.evidence;
                let bytes = presentation.render(output::result_data(outcome));
                match output::write_frame(&mut output.writer, &bytes) {
                    Ok(()) => 0,
                    Err(error) => {
                        output::write_refusal(&output::output_failed(error, evidence), stderr)
                    }
                }
            }
        }
    }
}
