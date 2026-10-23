use std::{
    collections::HashSet,
    io,
    sync::{Arc, Mutex},
};

use clapham::{Bounds, Category, Evidence, Refusal};
use rmcp::{
    ServiceExt,
    model::*,
    service::{RoleServer, RunningService, ServerInitializeError},
    transport::{Transport, async_rw::AsyncRwTransport},
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{Server, mapping};

impl Server {
    /// Start a connection on process stdin/stdout. The host supplies a Tokio runtime
    /// and retains the returned service until shutdown. Stdout is protocol-only.
    pub async fn serve_stdio(
        self,
    ) -> Result<RunningService<RoleServer, Self>, Box<ServerInitializeError>> {
        self.serve_io(tokio::io::stdin(), tokio::io::stdout()).await
    }

    /// Start the same bounded newline-delimited transport on supplied byte streams.
    /// Useful for pipes, embedding and deterministic duplex tests.
    ///
    /// Oversized input lines are discarded by rmcp without buffering their tail.
    /// Exhausted lifetime/queue limits close the connection. Oversized tool results
    /// become `result_too_large` refusals; an error reply or envelope that cannot
    /// fit closes the connection rather than changing a refusal code.
    pub async fn serve_io<R, W>(
        self,
        read: R,
        write: W,
    ) -> Result<RunningService<RoleServer, Self>, Box<ServerInitializeError>>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let bounds = self.set.bounds().clone();
        let transport = BoundedTransport {
            inner: AsyncRwTransport::new(read, write).with_max_line_length(bounds.message_bytes),
            active: Arc::new(Mutex::new(HashSet::new())),
            failed: clapham::Cancellation::new(),
            received: 0,
            bounds,
        };
        self.serve(transport).await.map_err(Box::new)
    }
}

struct BoundedTransport<R: AsyncRead, W: AsyncWrite> {
    inner: AsyncRwTransport<RoleServer, R, W>,
    bounds: Bounds,
    active: Arc<Mutex<HashSet<RequestId>>>,
    failed: clapham::Cancellation,
    received: usize,
}

impl<R, W> Transport<RoleServer> for BoundedTransport<R, W>
where
    R: AsyncRead + Send + Unpin,
    W: AsyncWrite + Send + Unpin + 'static,
{
    type Error = io::Error;

    fn send(
        &mut self,
        mut item: ServerJsonRpcMessage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let response_id = match &item {
            JsonRpcMessage::Response(response) => Some(response.id.clone()),
            JsonRpcMessage::Error(error) => error.id.clone(),
            _ => None,
        };
        let limit = self.bounds.message_bytes.saturating_sub(1); // newline is part of the frame
        if mapping::size(&item, limit).is_none()
            && let JsonRpcMessage::Response(response) = &mut item
            && matches!(&response.result, ServerResult::CallToolResult(result) if result.is_error != Some(true))
        {
            let refusal = Refusal::new(
                "result_too_large",
                Category::Usage,
                "the encoded tool result exceeds bounds.message_bytes",
            )
            .with_evidence(Evidence::Uncertain);
            let mut result = mapping::refusal(refusal);
            result.meta = Some(serde_json::json!({"io.modelcontextprotocol/serverInfo": {"name":"clapham","version":env!("CARGO_PKG_VERSION")}}).as_object().unwrap().clone().into());
            response.result = ServerResult::CallToolResult(result);
        }
        let fits = mapping::size(&item, limit).is_some();
        let sent = self.inner.send(item);
        let active = self.active.clone();
        let failed = self.failed.clone();
        async move {
            let result = if fits {
                sent.await.map_err(io::Error::other)
            } else {
                Err(io::Error::other(
                    "encoded response exceeds bounds.message_bytes",
                ))
            };
            if let Some(id) = response_id {
                active.lock().expect("request lock poisoned").remove(&id);
            }
            if result.is_err() {
                failed.cancel();
            }
            result
        }
    }

    async fn receive(&mut self) -> Option<ClientJsonRpcMessage> {
        let item = tokio::select! {
            biased;
            () = self.failed.cancelled() => return None,
            item = self.inner.receive() => item?,
        };
        if matches!(
            item,
            JsonRpcMessage::Request(_) | JsonRpcMessage::Notification(_)
        ) {
            if self.received == self.bounds.notifications {
                return None;
            }
            self.received += 1;
        }
        match &item {
            JsonRpcMessage::Request(request) => {
                let mut active = self.active.lock().expect("request lock poisoned");
                if active.len() >= self.bounds.in_flight + self.bounds.waiting
                    || !active.insert(request.id.clone())
                {
                    return None;
                }
            }
            JsonRpcMessage::Notification(notification) => {
                if let ClientNotification::CancelledNotification(cancelled) =
                    &notification.notification
                    && let Some(id) = &cancelled.params.request_id
                {
                    // rmcp deliberately suppresses replies to cancelled requests.
                    self.active
                        .lock()
                        .expect("request lock poisoned")
                        .remove(id);
                }
            }
            _ => {}
        }
        Some(item)
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.inner.close().await.map_err(io::Error::other)
    }
}
