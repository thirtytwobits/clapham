use std::{
    collections::HashSet,
    io,
    sync::{Arc, Mutex},
};

use clapham::Bounds;
use rmcp::{
    ServiceExt,
    model::*,
    service::{RoleServer, RunningService, ServerInitializeError},
    transport::{Transport, async_rw::AsyncRwTransport},
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{Server, mapping, records};

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
    /// Exhausted lifetime/queue limits and reused cancelled IDs close the connection.
    /// Oversized successful tool results
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
        let transport = BoundedTransport::new(read, write, self.set.bounds().clone());
        self.serve(transport).await.map_err(Box::new)
    }
}

pub(crate) struct BoundedTransport<R: AsyncRead, W: AsyncWrite> {
    inner: AsyncRwTransport<RoleServer, R, W>,
    bounds: Bounds,
    active: Arc<Mutex<HashSet<RequestId>>>,
    // Fixed-size digests keep lifetime bookkeeping bounded even for long IDs.
    cancelled: HashSet<[u8; 32]>,
    failed: clapham::Cancellation,
    received: usize,
}

impl<R, W> BoundedTransport<R, W>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    pub(crate) fn new(read: R, write: W, bounds: Bounds) -> Self {
        Self {
            inner: AsyncRwTransport::new(read, write).with_max_line_length(bounds.message_bytes),
            active: Arc::new(Mutex::new(HashSet::new())),
            cancelled: HashSet::new(),
            failed: clapham::Cancellation::new(),
            received: 0,
            bounds,
        }
    }
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
            && let ServerResult::CallToolResult(result) = &mut response.result
            && result.is_error != Some(true)
        {
            let meta = result.meta.take();
            *result = mapping::refusal(mapping::too_large());
            result.meta = meta;
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
        // A streaming call holding every slot waits for its acknowledgements, so they
        // are admitted outside the lifetime and active-request limits.
        let acknowledgement = records::is_acknowledgement(&item);
        if matches!(
            item,
            JsonRpcMessage::Request(_) | JsonRpcMessage::Notification(_)
        ) && !acknowledgement
        {
            if self.received == self.bounds.notifications {
                return None;
            }
            self.received += 1;
        }
        match &item {
            JsonRpcMessage::Request(request) => {
                let mut active = self.active.lock().expect("request lock poisoned");
                if self.cancelled.contains(&request_key(&request.id))
                    || active.contains(&request.id)
                {
                    return None;
                }
                if !acknowledgement {
                    if active.len() >= self.bounds.in_flight + self.bounds.waiting {
                        return None;
                    }
                    active.insert(request.id.clone());
                }
            }
            JsonRpcMessage::Notification(notification) => {
                if let ClientNotification::CancelledNotification(cancelled) =
                    &notification.notification
                    && let Some(id) = &cancelled.params.request_id
                {
                    // rmcp suppresses cancelled replies. Retire the ID for this
                    // connection so a late completion cannot clear a newer call.
                    self.cancelled.insert(request_key(id));
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

fn request_key(id: &RequestId) -> [u8; 32] {
    let mut digest = Sha256::new();
    match id {
        NumberOrString::Number(number) => {
            digest.update([0]);
            digest.update(number.to_le_bytes());
        }
        NumberOrString::String(text) => {
            digest.update([1]);
            digest.update(text.as_bytes());
        }
    }
    digest.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[tokio::test]
    async fn oversized_tool_replacement_preserves_all_response_metadata() {
        let (write, read) = tokio::io::duplex(16384);
        let mut transport = BoundedTransport {
            inner: AsyncRwTransport::new(tokio::io::empty(), write),
            bounds: Bounds {
                message_bytes: 8192,
                ..Bounds::default()
            },
            active: Arc::new(Mutex::new(HashSet::new())),
            cancelled: HashSet::new(),
            failed: clapham::Cancellation::new(),
            received: 0,
        };
        let meta = json!({"io.modelcontextprotocol/serverInfo":{"name":"host","version":"2"},"example.test/correlation":"retained"});
        let mut result = CallToolResult::structured(json!({"text":"x".repeat(8192)}));
        result.meta = Some(meta.as_object().unwrap().clone().into());
        transport
            .send(ServerJsonRpcMessage::response(
                ServerResult::CallToolResult(result),
                NumberOrString::Number(1),
            ))
            .await
            .unwrap();
        let mut line = String::new();
        BufReader::new(read).read_line(&mut line).await.unwrap();
        assert!(line.len() <= 8192);
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            response["result"]["structuredContent"]["code"],
            "result_too_large"
        );
        assert_eq!(response["result"]["_meta"], meta);
        transport.close().await.unwrap();
    }
}
