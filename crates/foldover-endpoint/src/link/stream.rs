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
//! is how either side gives up. An answer also gives up as its link goes
//! away: the runtime may stop before the other Mac's next frame, and a
//! call still waiting then would keep the runtime from stopping.

use std::future::Future;

use foldover_net::peer::{PeerRecvStream, PeerSendStream, read_message, write_message};
use foldover_proto::peer::PeerReply;
use serde::Serialize;
use tokio::runtime::Handle;
use tokio::sync::watch;

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
    /// Whether its link is going away; an answer and a call both give
    /// up then.
    closing: Option<watch::Receiver<bool>>,
}

impl Stream {
    fn write(&mut self, tag: u8, payload: &[u8]) -> Result<(), LinkError> {
        let Self {
            runtime,
            send,
            closing,
            ..
        } = self;
        let frame = frame(tag, payload);
        until_closing(runtime, closing, async {
            Ok(write_message(send, &frame).await?)
        })
    }

    fn next(&mut self) -> Result<Incoming, LinkError> {
        let Self {
            runtime,
            recv,
            closing,
            ..
        } = self;
        decode(until_closing(runtime, closing, async {
            Ok(read_message(recv).await?)
        })?)
    }
}

/// Run `call` on `runtime` until it ends, or until `closing` says the
/// link is going away.
fn until_closing<T>(
    runtime: &Handle,
    closing: &mut Option<watch::Receiver<bool>>,
    call: impl Future<Output = Result<T, LinkError>>,
) -> Result<T, LinkError> {
    runtime.block_on(async {
        let Some(closing) = closing else {
            return call.await;
        };
        tokio::select! {
            result = call => result,
            // A link dropped before saying so counts as going away too.
            _ = closing.wait_for(|closing| *closing) => Err(LinkError::Closed),
        }
    })
}

/// A request this Mac sent, for reading replies and data and sending data.
pub struct Exchange(Stream);

impl Exchange {
    pub(super) fn new(
        runtime: Handle,
        send: PeerSendStream,
        recv: PeerRecvStream,
        closing: watch::Receiver<bool>,
    ) -> Self {
        Self(Stream {
            runtime,
            send,
            recv,
            closing: Some(closing),
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
pub struct Answer {
    stream: Stream,
}

impl Answer {
    pub(super) fn new(
        runtime: Handle,
        send: PeerSendStream,
        recv: PeerRecvStream,
        closing: watch::Receiver<bool>,
    ) -> Self {
        Self {
            stream: Stream {
                runtime,
                send,
                recv,
                closing: Some(closing),
            },
        }
    }

    /// Whether this Mac's link is going away; an answer that waits on its
    /// own, such as a folder watch, stops then.
    pub fn closing(&self) -> bool {
        self.stream
            .closing
            .as_ref()
            .is_some_and(|closing| *closing.borrow() || closing.has_changed().is_err())
    }

    pub fn reply(&mut self, reply: &PeerReply) -> Result<(), LinkError> {
        let json = serde_json::to_vec(reply).map_err(|_| LinkError::Protocol)?;
        self.stream.write(MESSAGE, &json)
    }

    pub fn send_data(&mut self, chunk: &[u8]) -> Result<(), LinkError> {
        self.stream.write(DATA, chunk)
    }

    pub fn end_data(&mut self) -> Result<(), LinkError> {
        self.stream.write(END, &[])
    }

    /// The next data chunk the asker sent; None after its end.
    pub fn next_data(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
        match self.stream.next()? {
            Incoming::Data(data) => Ok(Some(data)),
            Incoming::End => Ok(None),
            Incoming::Reply(_) => Err(LinkError::Protocol),
        }
    }

    pub(super) fn into_send(self) -> PeerSendStream {
        self.stream.send
    }
}

/// What a frame holds; anything else is a protocol error.
fn decode(mut frame: Vec<u8>) -> Result<Incoming, LinkError> {
    if frame.is_empty() {
        return Err(LinkError::Protocol);
    }
    match frame.remove(0) {
        MESSAGE => Ok(Incoming::Reply(
            serde_json::from_slice(&frame).map_err(|_| LinkError::Protocol)?,
        )),
        DATA => Ok(Incoming::Data(frame)),
        END if frame.is_empty() => Ok(Incoming::End),
        _ => Err(LinkError::Protocol),
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

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        /// Frames from another Mac never panic the decoder; data and the
        /// end come back as sent.
        #[test]
        fn fuzz_frames(frame in prop::collection::vec(any::<u8>(), 0..512)) {
            let decoded = decode(frame.clone());
            match frame.split_first() {
                Some((&DATA, data)) => prop_assert_eq!(decoded.unwrap(), Incoming::Data(data.to_vec())),
                Some((&END, [])) => prop_assert_eq!(decoded.unwrap(), Incoming::End),
                Some((&MESSAGE, _)) => {}
                _ => prop_assert!(decoded.is_err()),
            }
        }
    }
}
