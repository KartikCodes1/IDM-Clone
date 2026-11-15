//! ForgeDM IPC transport: the named-pipe server and client.
//!
//! - [`server`]: accept loop with the capacity invariant (ADR-005), caller check, timeouts,
//!   and an injected [`RequestHandler`]. The real handler lives in the agent.
//! - [`client`]: connect with busy-retry, verify the server's SID and full image path
//!   before the first write, correlate responses by `request_id`, 10 s request timeout.
//! - [`identity`]: the pure identity decision shared by both sides.
//! - [`subscribe`]: the bounded per-connection event queue (the hub is in the agent).
//!
//! All Win32 FFI goes through `forgedm-platform`; this crate contains no `unsafe`.

#![forbid(unsafe_code)]

use std::time::Duration;

pub mod client;
pub mod error;
pub mod identity;
pub mod server;
pub mod subscribe;

pub use client::{Client, ClientConfig};
pub use error::IpcError;
pub use identity::{decide_identity, verify_pid};
pub use server::{
    CloseReason, Connection, HandlerFuture, RequestHandler, Server, ServerOptions, Timeouts,
    serve_connection,
};
pub use subscribe::{EventReceiver, EventSink, SinkError, SubscriptionItem, event_channel};

/// Connections served at once; one more pipe instance listens (`PIPE_MAX_INSTANCES = 17`).
pub const MAX_ACTIVE_CONNECTIONS: usize = 16;
const _: () = assert!(
    MAX_ACTIVE_CONNECTIONS + 1 == forgedm_platform::pipe::PIPE_MAX_INSTANCES as usize,
    "one listening instance beyond the active connections"
);
/// Frames in a connection's outbound event queue (PHASE_0_1_PLAN A.9).
pub const OUTBOUND_QUEUE: usize = 256;
/// How long a client waits for the response to one request (PHASE_0_1_PLAN A.9).
pub const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Pause between `ERROR_PIPE_BUSY` retries (PHASE_0_1_PLAN A.9).
pub const PIPE_BUSY_RETRY_INTERVAL: Duration = Duration::from_millis(50);
/// Total time a client keeps retrying `ERROR_PIPE_BUSY` (PHASE_0_1_PLAN A.9).
pub const PIPE_BUSY_RETRY_TOTAL: Duration = Duration::from_secs(5);
