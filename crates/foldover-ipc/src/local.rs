use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use foldover_net::local::SocketLocation;
use foldover_net::local::{Connection as Channel, Listener};
use foldover_proto::local::{
    DAEMON_STATUS, Hello, INTERNAL_ERROR, INVALID_REQUEST, PARSE_ERROR, PERMISSION_DENIED,
    PROTOCOL_VERSION, Request, Response, RpcError, SESSION_HELLO, VERSION_MISMATCH,
};
use serde_json::Value;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
const READ_CHUNK_BYTES: usize = 4096;
const MAX_BATCH_REQUESTS: usize = 128;
const MAX_CONNECTIONS: usize = 32;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Calls that reach a remote host or move files inline get the agent's
/// own SFTP bounds (60s connect, 60s I/O) plus margin, not the 5s a
/// local answer needs.
const SLOW_REQUEST_TIMEOUT: Duration = Duration::from_secs(150);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Agent I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("Agent JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Agent protocol: {0}")]
    Protocol(&'static str),
    #[error("Agent request timed out")]
    Timeout,
    #[error("Agent frame exceeds the {MAX_FRAME_BYTES}-byte limit")]
    FrameTooLarge,
    #[error("Agent error {code}: {message}")]
    Remote { code: i32, message: String },
}

pub struct Connection {
    channel: Channel,
    pending: Vec<u8>,
    /// A call that timed out or failed mid-frame may still have its
    /// answer in flight; reading it later would hand it to another
    /// call, so the connection is retired instead.
    poisoned: bool,
}

pub struct Server {
    listener: Listener,
}

pub fn bind(location: &SocketLocation) -> Result<Server, Error> {
    Ok(Server {
        listener: Listener::bind(location)?,
    })
}

pub async fn connect(location: &SocketLocation) -> Result<Connection, Error> {
    let channel = tokio::time::timeout(REQUEST_TIMEOUT, Channel::connect(location))
        .await
        .map_err(|_| Error::Timeout)??;
    Ok(Connection {
        channel,
        pending: Vec::new(),
        poisoned: false,
    })
}

impl Connection {
    /// Authenticate before even a public handshake when a private call follows.
    pub async fn authenticate(&self) -> Result<(), Error> {
        Ok(self.channel.require_installed_peer().await?)
    }

    pub async fn call(&mut self, request: Request) -> Result<Value, Error> {
        self.call_within(request, REQUEST_TIMEOUT).await
    }

    /// A call the agent answers slowly: reaching a remote host (SFTP
    /// browsing, pair checks, peer handshakes) or moving files inline
    /// (put-back, keep-both) outlasts the normal five seconds.
    pub async fn call_slow(&mut self, request: Request) -> Result<Value, Error> {
        self.call_within(request, SLOW_REQUEST_TIMEOUT).await
    }

    async fn call_within(&mut self, request: Request, timeout: Duration) -> Result<Value, Error> {
        if self.poisoned {
            return Err(Error::Protocol(
                "The connection is unusable after a call timed out",
            ));
        }
        let expected = request
            .id()
            .ok_or(Error::Protocol("A call needs an ID"))?
            .clone();
        if privileged(request.method()) {
            self.channel.require_installed_peer().await?;
        }
        self.write_value(&request.to_value()).await?;
        let frame = match self.read_frame(timeout).await {
            // A read failure can leave this call's answer in flight;
            // a later read would hand it to the wrong call.
            Err(error) => {
                self.poisoned = true;
                return Err(error);
            }
            Ok(frame) => frame.ok_or(Error::Protocol("Agent closed before its response"))?,
        };
        let response = Response::parse(serde_json::from_slice(&frame)?).map_err(Error::Protocol)?;
        if response.id() != &expected {
            return Err(Error::Protocol("Response ID does not match the request"));
        }
        response.into_result().map_err(|error| Error::Remote {
            code: error.code(),
            message: error.message().to_owned(),
        })
    }

    async fn read_frame(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, Error> {
        tokio::time::timeout(timeout, self.read_inner())
            .await
            .map_err(|_| Error::Timeout)?
    }

    async fn read_inner(&mut self) -> Result<Option<Vec<u8>>, Error> {
        loop {
            if let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
                if newline > MAX_FRAME_BYTES {
                    return Err(Error::FrameTooLarge);
                }
                let mut frame = self.pending.drain(..=newline).collect::<Vec<_>>();
                frame.pop();
                return Ok(Some(frame));
            }
            if self.pending.len() > MAX_FRAME_BYTES {
                return Err(Error::FrameTooLarge);
            }
            let mut chunk = [0; READ_CHUNK_BYTES];
            let count = self.channel.read(&mut chunk).await?;
            if count == 0 {
                return if self.pending.is_empty() {
                    Ok(None)
                } else {
                    Err(Error::Protocol("Truncated agent frame"))
                };
            }
            self.pending.extend_from_slice(&chunk[..count]);
        }
    }

    async fn write_value(&mut self, value: &Value) -> Result<(), Error> {
        let mut bytes = serde_json::to_vec(value)?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(Error::FrameTooLarge);
        }
        bytes.push(b'\n');
        tokio::time::timeout(REQUEST_TIMEOUT, self.channel.write_all(&bytes))
            .await
            .map_err(|_| Error::Timeout)??;
        Ok(())
    }
}

pub async fn serve<F, Fut>(server: Server, handler: F) -> Result<(), Error>
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, RpcError>> + Send,
{
    let listener = server.listener;
    let handler = Arc::new(handler);
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut tasks = JoinSet::new();
    loop {
        while let Some(result) = tasks.try_join_next() {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!("ipc: {error}"),
                Err(error) => eprintln!("ipc task: {error}"),
            }
        }
        let permit = Arc::clone(&permits)
            .acquire_owned()
            .await
            .map_err(|_| Error::Protocol("Connection limit closed"))?;
        let channel = match listener.accept().await {
            Ok(channel) => channel,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => continue,
            Err(error) => return Err(error.into()),
        };
        let handler = Arc::clone(&handler);
        tasks.spawn(async move {
            let _permit = permit;
            let mut connection = Connection {
                channel,
                pending: Vec::new(),
                poisoned: false,
            };
            let session = AtomicBool::new(false);
            while let Some(frame) = connection.read_frame(REQUEST_TIMEOUT).await? {
                let output = match serde_json::from_slice(&frame) {
                    Ok(value) => {
                        dispatch(value, &|request| {
                            authorized(request, &connection.channel, &session, &*handler)
                        })
                        .await
                    }
                    Err(_) => Some(
                        Response::error(Value::Null, RpcError::new(PARSE_ERROR, "Parse error"))
                            .to_value(),
                    ),
                };
                if let Some(output) = output {
                    connection.write_value(&output).await?;
                }
            }
            Ok::<(), Error>(())
        });
    }
}

async fn authorized<F, Fut>(
    request: Request,
    channel: &Channel,
    session: &AtomicBool,
    handler: &F,
) -> Result<Value, RpcError>
where
    F: Fn(Request) -> Fut,
    Fut: Future<Output = Result<Value, RpcError>>,
{
    if privileged(request.method()) {
        channel.require_installed_peer().await.map_err(|_| {
            RpcError::new(PERMISSION_DENIED, "Installed peer authentication required")
        })?;
    }
    negotiated(request, session, handler).await
}

async fn negotiated<F, Fut>(
    request: Request,
    session: &AtomicBool,
    handler: &F,
) -> Result<Value, RpcError>
where
    F: Fn(Request) -> Fut,
    Fut: Future<Output = Result<Value, RpcError>>,
{
    let hello = request.method() == SESSION_HELLO;
    if hello {
        session.store(false, Ordering::Relaxed);
    } else if privileged(request.method()) && !session.load(Ordering::Relaxed) {
        return Err(RpcError::new(
            VERSION_MISMATCH,
            "Negotiate session.hello before private requests",
        ));
    }
    let response = handler(request).await?;
    if hello {
        let response: Hello = serde_json::from_value(response.clone())
            .map_err(|_| RpcError::new(VERSION_MISMATCH, "Invalid protocol negotiation"))?;
        if response.protocol_version != PROTOCOL_VERSION {
            return Err(RpcError::new(
                VERSION_MISMATCH,
                "Unsupported agent protocol version",
            ));
        }
        session.store(true, Ordering::Relaxed);
    }
    Ok(response)
}

fn privileged(method: &str) -> bool {
    !matches!(method, SESSION_HELLO | DAEMON_STATUS)
}

async fn dispatch<F, Fut>(value: Value, handler: &F) -> Option<Value>
where
    F: Fn(Request) -> Fut,
    Fut: Future<Output = Result<Value, RpcError>>,
{
    let Value::Array(batch) = value else {
        return dispatch_one(value, handler).await;
    };
    if batch.is_empty() || batch.len() > MAX_BATCH_REQUESTS {
        return Some(
            Response::error(
                Value::Null,
                RpcError::new(INVALID_REQUEST, "Invalid batch size"),
            )
            .to_value(),
        );
    }
    let mut output = Vec::new();
    let mut response_bytes: usize = 2; // JSON array brackets.
    for value in batch {
        if let Some(response) = dispatch_one(value, handler).await {
            let bytes = serde_json::to_vec(&response).map_or(MAX_FRAME_BYTES, |bytes| bytes.len());
            response_bytes = response_bytes.saturating_add(bytes + usize::from(!output.is_empty()));
            if response_bytes > MAX_FRAME_BYTES {
                // A batch is not a transaction; stop allocating and dispatching further work.
                return Some(Response::error(Value::Null, RpcError::new(INTERNAL_ERROR,
                    "Batch response exceeds frame limit; refresh state and request fewer results")).to_value());
            }
            output.push(response);
        }
    }
    if output.is_empty() {
        None
    } else {
        Some(Value::Array(output))
    }
}

async fn dispatch_one<F, Fut>(value: Value, handler: &F) -> Option<Value>
where
    F: Fn(Request) -> Fut,
    Fut: Future<Output = Result<Value, RpcError>>,
{
    let request = match Request::parse(value) {
        Ok(request) => request,
        Err(response) => return Some(response.to_value()),
    };
    let id = request.id().cloned();
    let result = handler(request).await;
    id.map(|id| match result {
        Ok(value) => Response::result(id, value).to_value(),
        Err(error) => Response::error(id, error).to_value(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn private_dispatch_requires_a_successful_current_session() {
        let session = AtomicBool::new(false);
        let calls = std::sync::Mutex::new(Vec::new());
        let handler = |request: Request| {
            calls.lock().unwrap().push(request.method().to_owned());
            async move {
                if request.method() == SESSION_HELLO {
                    return Ok(
                        json!({"protocol_version": request.params().unwrap()["protocol_version"], "core_version":"test"}),
                    );
                }
                Ok(json!("private"))
            }
        };
        assert_eq!(
            negotiated(Request::call(1, "pair.list", None), &session, &handler)
                .await
                .unwrap_err()
                .code(),
            VERSION_MISMATCH
        );
        assert!(calls.lock().unwrap().is_empty());
        negotiated(
            Request::call(
                2,
                SESSION_HELLO,
                Some(json!({"protocol_version":PROTOCOL_VERSION})),
            ),
            &session,
            &handler,
        )
        .await
        .unwrap();
        assert_eq!(
            negotiated(Request::call(3, "pair.list", None), &session, &handler)
                .await
                .unwrap(),
            "private"
        );
        assert!(
            negotiated(
                Request::call(
                    4,
                    SESSION_HELLO,
                    Some(json!({"protocol_version":PROTOCOL_VERSION + 1}))
                ),
                &session,
                &handler
            )
            .await
            .is_err()
        );
        assert_eq!(
            negotiated(Request::call(5, "pair.list", None), &session, &handler)
                .await
                .unwrap_err()
                .code(),
            VERSION_MISMATCH
        );
        assert_eq!(calls.lock().unwrap().len(), 3);
    }

    #[test]
    fn only_exact_health_method_names_bypass_code_authentication() {
        assert!(!privileged(SESSION_HELLO));
        assert!(!privileged(DAEMON_STATUS));
        for method in [
            "pair.list",
            "preview.list",
            "events.subscribe",
            "daemon.status ",
            "SESSION.HELLO",
            "",
        ] {
            assert!(privileged(method), "{method}");
        }
    }

    #[tokio::test]
    async fn batches_preserve_ids_and_suppress_notifications() {
        let handler = |_request| async { Ok(json!("ok")) };
        let response = dispatch(
            json!([
                {"jsonrpc":"2.0","method":"status","id":"text"},
                {"jsonrpc":"2.0","method":"status"},
                {"jsonrpc":"2.0","method":"status","id":null}, 4
            ]),
            &handler,
        )
        .await
        .unwrap();
        let responses = response.as_array().unwrap();
        assert_eq!(responses.len(), 3);
        assert_eq!(responses[0]["id"], "text");
        assert!(responses[1]["id"].is_null());
        assert_eq!(responses[2]["error"]["code"], INVALID_REQUEST);
        assert!(
            dispatch(json!([{ "jsonrpc":"2.0","method":"status" }]), &handler)
                .await
                .is_none()
        );
        assert_eq!(
            dispatch(json!([]), &handler).await.unwrap()["error"]["code"],
            INVALID_REQUEST
        );
    }

    #[tokio::test]
    async fn batch_results_stop_before_accumulating_oversized_private_pages() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let handler = |_request| {
            calls.fetch_add(1, Ordering::Relaxed);
            async { Ok(json!("x".repeat(MAX_FRAME_BYTES / 2))) }
        };
        let response = dispatch(
            json!([
                {"jsonrpc":"2.0", "method":"pair.list", "id":1},
                {"jsonrpc":"2.0", "method":"pair.list", "id":2},
                {"jsonrpc":"2.0", "method":"pair.list", "id":3}
            ]),
            &handler,
        )
        .await
        .unwrap();
        assert_eq!(
            response["error"]["code"],
            foldover_proto::local::INTERNAL_ERROR
        );
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert!(serde_json::to_vec(&response).unwrap().len() < MAX_FRAME_BYTES);
    }

    #[tokio::test]
    async fn reads_fragmented_and_coalesced_frames_and_detects_truncation() {
        let directory = tempfile::tempdir().unwrap();
        let location = SocketLocation::Path(directory.path().join("runtime/s"));
        let listener = Listener::bind(&location).unwrap();
        let mut client = Channel::connect(&location).await.unwrap();
        let mut server = Connection {
            channel: listener.accept().await.unwrap(),
            pending: Vec::new(),
            poisoned: false,
        };
        client.write_all(b"one\ntwo\npart").await.unwrap();
        assert_eq!(
            server.read_frame(REQUEST_TIMEOUT).await.unwrap().unwrap(),
            b"one"
        );
        assert_eq!(
            server.read_frame(REQUEST_TIMEOUT).await.unwrap().unwrap(),
            b"two"
        );
        client.write_all(b"ial\n").await.unwrap();
        assert_eq!(
            server.read_frame(REQUEST_TIMEOUT).await.unwrap().unwrap(),
            b"partial"
        );
        client.write_all(b"broken").await.unwrap();
        drop(client);
        assert!(matches!(
            server.read_frame(REQUEST_TIMEOUT).await,
            Err(Error::Protocol("Truncated agent frame"))
        ));
    }

    #[tokio::test]
    async fn a_timed_out_call_retires_the_connection() {
        // An agent that answers after the timeout must not let a later
        // call pick the late response up as its own.
        let directory = tempfile::tempdir().unwrap();
        let location = SocketLocation::Path(directory.path().join("runtime/s"));
        let server = bind(&location).unwrap();
        let handled = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = std::sync::Arc::clone(&handled);
        let serving = tokio::spawn(serve(server, move |_request| {
            let count = std::sync::Arc::clone(&count);
            async move {
                count.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok(Value::Null)
            }
        }));
        let mut connection = connect(&location).await.unwrap();
        let slow = Request::call(1, DAEMON_STATUS, None);
        assert!(matches!(
            connection
                .call_within(slow, Duration::from_millis(50))
                .await,
            Err(Error::Timeout)
        ));
        // The same connection refuses to risk answering with the late
        // response; a fresh one is unaffected.
        let next = Request::call(1, DAEMON_STATUS, None);
        assert!(matches!(
            connection
                .call_within(next, Duration::from_millis(50))
                .await,
            Err(Error::Protocol(_))
        ));
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(handled.load(Ordering::Relaxed), 1);
        serving.abort();
    }

    #[tokio::test]
    async fn rejects_oversized_frames_before_json_parsing() {
        let directory = tempfile::tempdir().unwrap();
        let location = SocketLocation::Path(directory.path().join("runtime/s"));
        let listener = Listener::bind(&location).unwrap();
        let mut client = Channel::connect(&location).await.unwrap();
        let mut server = Connection {
            channel: listener.accept().await.unwrap(),
            pending: vec![b'x'; MAX_FRAME_BYTES + 1],
            poisoned: false,
        };
        assert!(matches!(
            server.read_frame(REQUEST_TIMEOUT).await,
            Err(Error::FrameTooLarge)
        ));
        client.write_all(b"\n").await.unwrap();
    }
}
