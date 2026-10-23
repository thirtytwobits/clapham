//! Copyright (c) Scott A Dixon
//! Host-supplied byte streams with explicit framing failures.
use crate::{McpConfig, McpConnection, McpError, connection::State, wire};
use rmcp::{
    RoleClient,
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::Transport,
};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

struct Stdio<R, W> {
    reader: BufReader<R>,
    writer: Arc<tokio::sync::Mutex<W>>,
    state: Arc<State>,
    limit: usize,
    notifications: usize,
    frame: Vec<u8>,
    shutdown: CancellationToken,
}
impl<R, W> Drop for Stdio<R, W> {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}
impl<R, W> Transport<RoleClient> for Stdio<R, W>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    type Error = McpError;
    fn send(
        &mut self,
        item: ClientJsonRpcMessage,
    ) -> impl std::future::Future<Output = Result<(), McpError>> + Send + 'static {
        let sending = self.state.requests.begin(&item).inspect_err(|fault| {
            self.state.fail(fault.clone());
        });
        let bytes = wire::encode(&item, self.limit.saturating_sub(1));
        let writer = self.writer.clone();
        let state = self.state.clone();
        async move {
            let sending = sending?;
            let result = async {
                let bytes = bytes?;
                let mut writer = writer.lock().await;
                writer
                    .write_all(&bytes)
                    .await
                    .map_err(|_| McpError::Transport)?;
                writer
                    .write_all(b"\n")
                    .await
                    .map_err(|_| McpError::Transport)?;
                writer.flush().await.map_err(|_| McpError::Transport)
            };
            let result = tokio::select! { result = result => result, _ = state.stop.cancelled() => Err(McpError::Transport) };
            if result.is_ok() {
                sending.sent();
            }
            result
        }
    }
    async fn receive(&mut self) -> Option<ServerJsonRpcMessage> {
        loop {
            let data = tokio::select! {
                data = self.reader.fill_buf() => match data { Ok(data) if !data.is_empty() => data, _ => { self.state.fail(McpError::Transport); return None; } },
                _ = self.state.stop.cancelled() => return None,
            };
            let end = data.iter().position(|b| *b == b'\n');
            let count = end.map_or(data.len(), |end| end + 1);
            if count > self.limit.saturating_sub(self.frame.len()) {
                self.state.fail(McpError::Limit);
                return None;
            }
            self.frame.extend_from_slice(&data[..count]);
            self.reader.consume(count);
            if end.is_some() {
                let message = wire::decode(&self.frame, &self.state.requests).and_then(|message| {
                    self.state.observe_bytes(&message, self.frame.len())?;
                    self.state.observe(&message, self.notifications)?;
                    Ok(message)
                });
                self.frame.clear();
                match message {
                    Ok(message) => return Some(message),
                    Err(error) => {
                        self.state.fail(error);
                        return None;
                    }
                }
            }
        }
    }
    async fn close(&mut self) -> Result<(), McpError> {
        self.state.stop.cancel();
        self.shutdown.cancel();
        Ok(())
    }
}
impl McpConnection {
    /// Connect host-created streams. Cancellation of `shutdown` signals the host to reap its process.
    pub async fn stdio<R, W>(
        reader: R,
        writer: W,
        shutdown: CancellationToken,
        config: McpConfig,
    ) -> Result<Self, McpError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        config.validate()?;
        let state = State::new();
        let transport = Stdio {
            reader: BufReader::new(reader),
            writer: Arc::new(tokio::sync::Mutex::new(writer)),
            limit: config.bounds.message_bytes,
            notifications: config.bounds.notifications,
            state: state.clone(),
            frame: Vec::new(),
            shutdown,
        };
        Self::start(transport, config, state).await
    }
}
