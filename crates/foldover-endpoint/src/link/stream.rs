//! One request's stream between paired Macs. Each frame is one
//! length-prefixed message whose first byte says what it holds:
//!
//! ```text
//! asker                                   answerer
//! [1 request JSON] ─────────────────────▶
//!                  ◀───────────────────── [1 reply JSON] (one or more)
//! [2 data] [2 data] [3 end] ────────────▶  file contents, either way
//! ```
//!
//! Endpoints and agents call these from blocking threads; each call runs
//! the stream on the link's runtime. Dropping a stream resets it, which
//! is how either side gives up.

use foldover_net::peer::{PeerRecvStream, PeerSendStream, read_message, write_message};
use foldover_proto::peer::PeerReply;
use serde::Serialize;
use tokio::runtime::Handle;

use super::LinkError;

const MESSAGE: u8 = 1;
const DATA: u8 = 2;
const END: u8 = 3;

/// What arrives next on a stream.
#[derive(Debug, Eq, PartialEq)]
pub enum Incoming {
    Reply(PeerReply),
    Data(Vec<u8>),
    /// No more data follows.
    End,
}

/// Both halves of a request's stream, driven from a blocking thread.
struct Stream {
    runtime: Handle,
    send: PeerSendStream,
    recv: PeerRecvStream,
}

impl Stream {
    fn write(&mut self, tag: u8, payload: &[u8]) -> Result<(), LinkError> {
        let send = &mut self.send;
        Ok(self
            .runtime
            .block_on(write_message(send, &frame(tag, payload)))?)
    }

    fn read(&mut self) -> Result<(u8, Vec<u8>), LinkError> {
        let recv = &mut self.recv;
        let mut frame = self.runtime.block_on(read_message(recv))?;
        if frame.is_empty() {
            return Err(LinkError::Protocol);
        }
        let tag = frame.remove(0);
        Ok((tag, frame))
    }

    fn next(&mut self) -> Result<Incoming, LinkError> {
        match self.read()? {
            (MESSAGE, json) => Ok(Incoming::Reply(
                serde_json::from_slice(&json).map_err(|_| LinkError::Protocol)?,
            )),
            (DATA, data) => Ok(Incoming::Data(data)),
            (END, rest) if rest.is_empty() => Ok(Incoming::End),
            _ => Err(LinkError::Protocol),
        }
    }
}

/// A request this Mac sent, for reading replies and data and sending data.
pub struct Exchange(Stream);

impl Exchange {
    pub(super) fn new(runtime: Handle, send: PeerSendStream, recv: PeerRecvStream) -> Self {
        Self(Stream {
            runtime,
            send,
            recv,
        })
    }

    pub fn next(&mut self) -> Result<Incoming, LinkError> {
        self.0.next()
    }

    /// The next frame, which must be a reply.
    pub fn reply(&mut self) -> Result<PeerReply, LinkError> {
        match self.0.next()? {
            Incoming::Reply(reply) => Ok(reply),
            _ => Err(LinkError::Protocol),
        }
    }

    pub fn send_data(&mut self, chunk: &[u8]) -> Result<(), LinkError> {
        self.0.write(DATA, chunk)
    }

    pub fn end_data(&mut self) -> Result<(), LinkError> {
        self.0.write(END, &[])
    }
}

/// A request this Mac answers, for sending replies and data and reading
/// data.
pub struct Answer(Stream);

impl Answer {
    pub(super) fn new(runtime: Handle, send: PeerSendStream, recv: PeerRecvStream) -> Self {
        Self(Stream {
            runtime,
            send,
            recv,
        })
    }

    pub fn reply(&mut self, reply: &PeerReply) -> Result<(), LinkError> {
        let json = serde_json::to_vec(reply).map_err(|_| LinkError::Protocol)?;
        self.0.write(MESSAGE, &json)
    }

    pub fn send_data(&mut self, chunk: &[u8]) -> Result<(), LinkError> {
        self.0.write(DATA, chunk)
    }

    pub fn end_data(&mut self) -> Result<(), LinkError> {
        self.0.write(END, &[])
    }

    /// The next data chunk the asker sent; None after its end.
    pub fn next_data(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
        match self.0.next()? {
            Incoming::Data(data) => Ok(Some(data)),
            Incoming::End => Ok(None),
            Incoming::Reply(_) => Err(LinkError::Protocol),
        }
    }

    pub(super) fn into_send(self) -> PeerSendStream {
        self.0.send
    }
}

/// Write one JSON frame, a request or a reply, from async code.
pub(super) async fn send_json(
    send: &mut PeerSendStream,
    value: &impl Serialize,
) -> Result<(), LinkError> {
    let json = serde_json::to_vec(value).map_err(|_| LinkError::Protocol)?;
    Ok(write_message(send, &frame(MESSAGE, &json)).await?)
}

/// Read one JSON frame's bytes from async code.
pub(super) async fn receive_json(recv: &mut PeerRecvStream) -> Result<Vec<u8>, LinkError> {
    let mut frame = read_message(recv).await?;
    if frame.first() != Some(&MESSAGE) {
        return Err(LinkError::Protocol);
    }
    frame.remove(0);
    Ok(frame)
}

fn frame(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 1);
    frame.push(tag);
    frame.extend_from_slice(payload);
    frame
}
