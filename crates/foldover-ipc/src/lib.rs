//! `foldover-ipc` — transport layer on the drivers. Framing, JSON-RPC,
//! `serve` (taking a handler closure) and `connect` over the Unix
//! socket. Depends on foldover-net. See PLAN.md §4.4.

#[cfg(unix)]
mod local;

#[cfg(unix)]
pub use local::{Connection, Error, MAX_FRAME_BYTES, Server, SocketLocation, bind, connect, serve};
