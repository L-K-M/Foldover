//! One request's stream between paired Macs. Each frame is one
//! length-prefixed message whose first byte says what it holds:
//!
//! ```text
//! asker                                   answerer
//! [1 request JSON] ─────────────────────▶
//!                  ◀───────────────────── [1 reply JSON] (one or more)
//! [2 data] [2 data] [3 end] ────────────▶  file contents, either way
//!        [4 heartbeat] ◀────────────────▶  while local work is quiet
//! ```
//!
//! Endpoints and agents call these from blocking threads; each call runs
//! the stream on the link's runtime. Dropping a stream resets it, which
//! is how either side gives up. An answer also gives up as its link goes
//! away: the runtime may stop before the other Mac's next frame, and a
//! call still waiting then would keep the runtime from stopping.

use std::future::Future;
use std::task::Poll;
use std::time::{Duration, Instant};

use foldover_net::peer::{PeerRecvStream, PeerSendStream, read_message, write_message};
use foldover_proto::peer::PeerReply;
use foldover_types::ScanControl;
use serde::Serialize;
use tokio::runtime::Handle;
use tokio::sync::watch;

use super::LinkError;

const MESSAGE: u8 = 1;
const DATA: u8 = 2;
const END: u8 = 3;
const HEARTBEAT: u8 = 4;

/// A completed frame is expected within this much time. Job control is
/// checked much more often, so Stop remains prompt; the generous frame
/// bound also permits a slow disk flush or network link.
pub(super) const STREAM_INACTIVITY: Duration = Duration::from_secs(2 * 60);
const CONTROL_TICK: Duration = Duration::from_millis(200);

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
    /// Both halves are retired together after any interrupted frame. A
    /// partially polled framing future must never be recreated on either.
    send: Option<PeerSendStream>,
    recv: Option<PeerRecvStream>,
    /// Whether its link is going away; an answer and a call both give
    /// up then.
    closing: Option<watch::Receiver<bool>>,
    inactivity: Duration,
}

impl Stream {
    fn write_unless(
        &mut self,
        tag: u8,
        payload: &[u8],
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), LinkError> {
        let frame = frame(tag, payload);
        let result = {
            let Self {
                runtime,
                send,
                closing,
                inactivity,
                ..
            } = self;
            let send = send.as_mut().ok_or(LinkError::Closed)?;
            until_unless(runtime, closing.as_ref(), *inactivity, control, async {
                Ok(write_message(send, &frame).await?)
            })
        };
        if result.is_err() {
            self.retire();
        }
        result
    }

    fn next_unless(
        &mut self,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<Incoming, LinkError> {
        loop {
            let result = {
                let Self {
                    runtime,
                    recv,
                    closing,
                    inactivity,
                    ..
                } = self;
                let recv = recv.as_mut().ok_or(LinkError::Closed)?;
                until_unless(runtime, closing.as_ref(), *inactivity, control, async {
                    Ok(read_message(recv).await?)
                })
            };
            let decoded = result.and_then(decode);
            match decoded {
                Ok(Some(incoming)) => return Ok(incoming),
                // A heartbeat is a complete frame, so the next frame gets
                // a fresh inactivity budget.
                Ok(None) => {}
                Err(error) => {
                    self.retire();
                    return Err(error);
                }
            }
        }
    }

    fn retire(&mut self) {
        self.send.take();
        self.recv.take();
    }
}

enum Slice<T> {
    Ready(Result<T, LinkError>),
    Tick,
    Closed,
}

/// Poll one framing future in bounded slices. The future remains pinned
/// between slices: it may already have consumed a length or payload bytes.
/// Control runs outside the Tokio runtime so it may itself make a blocking
/// peer request or renew a lease without nesting `Handle::block_on`.
pub(super) fn until_unless<T>(
    runtime: &Handle,
    closing: Option<&watch::Receiver<bool>>,
    inactivity: Duration,
    control: &mut impl FnMut() -> ScanControl,
    call: impl Future<Output = Result<T, LinkError>>,
) -> Result<T, LinkError> {
    let mut call = std::pin::pin!(call);
    let mut polled_for = Duration::ZERO;
    loop {
        if control() == ScanControl::Cancel {
            return Err(LinkError::Cancelled);
        }
        if is_closed(closing) {
            return Err(LinkError::Closed);
        }
        if polled_for >= inactivity {
            // The callback above may have taken time while a frame became
            // ready. Poll once more before declaring the peer inactive.
            let ready = runtime.block_on(std::future::poll_fn(|context| {
                Poll::Ready(match call.as_mut().poll(context) {
                    Poll::Ready(result) => Some(result),
                    Poll::Pending => None,
                })
            }));
            return ready.unwrap_or(Err(LinkError::TimedOut));
        }

        let slice = CONTROL_TICK.min(inactivity.saturating_sub(polled_for));
        let mut closing_for_slice = closing.cloned();
        let started = Instant::now();
        let outcome = runtime.block_on(async {
            let sleep = tokio::time::sleep(slice);
            tokio::pin!(sleep);
            match closing_for_slice.as_mut() {
                Some(closing) => tokio::select! {
                    result = call.as_mut() => Slice::Ready(result),
                    _ = &mut sleep => Slice::Tick,
                    _ = closing.wait_for(|closing| *closing) => Slice::Closed,
                },
                None => tokio::select! {
                    result = call.as_mut() => Slice::Ready(result),
                    _ = &mut sleep => Slice::Tick,
                },
            }
        });
        polled_for = polled_for.saturating_add(started.elapsed());
        match outcome {
            Slice::Ready(result) => return result,
            Slice::Tick => {}
            Slice::Closed => return Err(LinkError::Closed),
        }
    }
}

fn is_closed(closing: Option<&watch::Receiver<bool>>) -> bool {
    closing.is_some_and(|closing| *closing.borrow() || closing.has_changed().is_err())
}

/// A request this Mac sent, for reading replies and data and sending data.
pub struct Exchange(Stream);

impl Exchange {
    pub(super) fn with_inactivity(
        runtime: Handle,
        send: PeerSendStream,
        recv: PeerRecvStream,
        closing: watch::Receiver<bool>,
        inactivity: Duration,
    ) -> Self {
        Self(Stream {
            runtime,
            send: Some(send),
            recv: Some(recv),
            closing: Some(closing),
            inactivity,
        })
    }

    pub fn next(&mut self) -> Result<Incoming, LinkError> {
        self.next_unless(&mut proceed)
    }

    pub fn next_unless(
        &mut self,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<Incoming, LinkError> {
        self.0.next_unless(control)
    }

    /// The next frame, which must be a reply.
    pub fn reply(&mut self) -> Result<PeerReply, LinkError> {
        self.reply_unless(&mut proceed)
    }

    pub fn reply_unless(
        &mut self,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<PeerReply, LinkError> {
        match self.0.next_unless(control)? {
            Incoming::Reply(reply) => Ok(reply),
            _ => {
                self.0.retire();
                Err(LinkError::Protocol)
            }
        }
    }

    pub fn send_data(&mut self, chunk: &[u8]) -> Result<(), LinkError> {
        self.send_data_unless(chunk, &mut proceed)
    }

    pub fn send_data_unless(
        &mut self,
        chunk: &[u8],
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), LinkError> {
        self.0.write_unless(DATA, chunk, control)
    }

    pub fn end_data(&mut self) -> Result<(), LinkError> {
        self.end_data_unless(&mut proceed)
    }

    pub fn end_data_unless(
        &mut self,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), LinkError> {
        self.0.write_unless(END, &[], control)
    }

    pub fn heartbeat_unless(
        &mut self,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), LinkError> {
        self.0.write_unless(HEARTBEAT, &[], control)
    }

    pub fn heartbeat(&mut self) -> Result<(), LinkError> {
        self.heartbeat_unless(&mut proceed)
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
                send: Some(send),
                recv: Some(recv),
                closing: Some(closing),
                inactivity: STREAM_INACTIVITY,
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
        self.stream.write_unless(MESSAGE, &json, &mut proceed)
    }

    pub fn send_data(&mut self, chunk: &[u8]) -> Result<(), LinkError> {
        self.stream.write_unless(DATA, chunk, &mut proceed)
    }

    pub fn end_data(&mut self) -> Result<(), LinkError> {
        self.stream.write_unless(END, &[], &mut proceed)
    }

    pub fn heartbeat(&mut self) -> Result<(), LinkError> {
        self.stream.write_unless(HEARTBEAT, &[], &mut proceed)
    }

    /// The next data chunk the asker sent; None after its end.
    pub fn next_data(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
        match self.stream.next_unless(&mut proceed)? {
            Incoming::Data(data) => Ok(Some(data)),
            Incoming::End => Ok(None),
            Incoming::Reply(_) => {
                self.stream.retire();
                Err(LinkError::Protocol)
            }
        }
    }

    pub(super) fn into_send(mut self) -> Option<PeerSendStream> {
        self.stream.send.take()
    }
}

/// What a frame holds; anything else is a protocol error.
fn decode(mut frame: Vec<u8>) -> Result<Option<Incoming>, LinkError> {
    if frame.is_empty() {
        return Err(LinkError::Protocol);
    }
    match frame.remove(0) {
        MESSAGE => Ok(Some(Incoming::Reply(
            serde_json::from_slice(&frame).map_err(|_| LinkError::Protocol)?,
        ))),
        DATA => Ok(Some(Incoming::Data(frame))),
        END if frame.is_empty() => Ok(Some(Incoming::End)),
        HEARTBEAT if frame.is_empty() => Ok(None),
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
    loop {
        if let Some(json) = json_frame(read_message(recv).await?)? {
            return Ok(json);
        }
    }
}

/// Read one JSON frame while transport heartbeats reset its inactivity
/// deadline. The caller drops the whole stream after any error.
pub(super) async fn receive_json_until(
    recv: &mut PeerRecvStream,
    inactivity: Duration,
) -> Result<Vec<u8>, LinkError> {
    loop {
        let frame = tokio::time::timeout(inactivity, read_message(recv))
            .await
            .map_err(|_| LinkError::TimedOut)??;
        if let Some(json) = json_frame(frame)? {
            return Ok(json);
        }
    }
}

fn json_frame(mut frame: Vec<u8>) -> Result<Option<Vec<u8>>, LinkError> {
    match frame.first().copied() {
        Some(MESSAGE) => {
            frame.remove(0);
            Ok(Some(frame))
        }
        Some(HEARTBEAT) if frame.len() == 1 => Ok(None),
        _ => Err(LinkError::Protocol),
    }
}

fn frame(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 1);
    frame.push(tag);
    frame.extend_from_slice(payload);
    frame
}

fn proceed() -> ScanControl {
    ScanControl::Continue
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
                Some((&DATA, data)) => prop_assert_eq!(decoded.unwrap(), Some(Incoming::Data(data.to_vec()))),
                Some((&END, [])) => prop_assert_eq!(decoded.unwrap(), Some(Incoming::End)),
                Some((&HEARTBEAT, [])) => prop_assert_eq!(decoded.unwrap(), None),
                Some((&MESSAGE, _)) => {}
                _ => prop_assert!(decoded.is_err()),
            }
        }
    }
}
