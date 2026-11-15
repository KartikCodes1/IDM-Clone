//! ForgeDM agent library: the background process that owns the queue, the database and
//! all disk writes. The `forgedm-agent` binary is a thin wrapper around [`app`].
//!
//! This slice (S0.8) serves Handshake, Ping, GetAgentStatus, GetSnapshot, Subscribe,
//! Unsubscribe, ListDownloads and ShutdownAgentSafely; every other action returns
//! `not_implemented` until its slice lands.
//!
//! - [`config`]: flags and the pipe name derivation.
//! - [`data_dir`]: data directory layout and the reparse-point check.
//! - [`logging`]: daily rolling JSON logs and the panic hook.
//! - [`sealer`]: DPAPI implementation of the storage `SecretSealer` port.
//! - [`reconcile`]: startup and shutdown settling of persisted states.
//! - [`events_hub`]: the StateHub actor (sequencing, replay, subscribers).
//! - [`handlers`]: the request handler.
//! - [`app`]: startup order, serving and shutdown.
//! - [`error`]: [`error::StartupError`] (exit codes) and [`error::AgentError`] (wire codes).

#![forbid(unsafe_code)]

pub mod app;
pub mod config;
pub mod data_dir;
pub mod error;
pub mod events_hub;
pub mod handlers;
pub mod logging;
pub mod reconcile;
pub mod sealer;
pub mod summary;
